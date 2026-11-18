/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Where the content of a display list can be rastered: the raster spaces a visual context tree offers, anchored at
//! the scroll nodes the compositor scrolls and at the sticky boxes held at their sticky edge, the space each visual
//! context is placed in and the clips left to apply when its rasters are composited, and the runs that are placed
//! together because they hold whole group effects.

use crate::display_list::chunks::ReachOutsideChunk;
use crate::display_list::commands::{
    ClipMode, ContextRef, DisplayListCommandRun, EffectNodeIndex, SpatialNodeIndex, VISUAL_VIEWPORT_NODE_INDEX,
};
use crate::display_list::effect_clip_plan::EffectClipPlan;
use crate::display_list::effect_geometry::Extent;
use crate::display_list::storage::DisplayListStorage;
use crate::host::FfiVisualAnimationTargetKind;
use crate::visual_context::queries::TreeCullingScratch;
use crate::visual_context::world_transforms::translation_on_whole_device_pixels;
use crate::visual_context::{
    ClipNodeData, CompositorScrollNodeRecord, EffectNodeData, SpatialData, StickyData, VisualContextTree,
    WorldTransforms, device_offset_for_index,
};
use libgfx_rust::{FloatMatrix4x4, FloatPoint, FloatRect, IntPoint, IntRect, enclosing_int_rect};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;

pub const VIEWPORT_RASTER_SPACE: usize = 0;
// Besides the viewport's, scrollers have spaces of their own up to the first of these, and the sticky boxes held at the
// time up to the second, so that which scrollers have one stays put while boxes start and stop being held.
const MAXIMUM_SCROLLER_SPACE_COUNT: usize = 7;
const MAXIMUM_STICKY_BOX_SPACE_COUNT: usize = 4;
const BOUNDS_MARGIN_IN_DEVICE_PIXELS: i32 = 3;
const EXTENT_LIMIT: f32 = 16777216.0;
const LARGEST_FRACTION_OF_A_DEVICE_PIXEL_A_COMPOSITE_CLIP_EDGE_MAY_BE_OFF_BY: f32 = 1.0 / 64.0;
// How far rounding the translation of an anchor to whole device pixels can take it past its range.
const ROUNDING_OF_AN_ANCHOR_TRANSLATION_IN_DEVICE_PIXELS: i32 = 1;

/// What the anchor of a space is, which decides how its translation is rounded and what the dump calls the space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RasterSpaceAnchorKind {
    Viewport,
    /// The scroll node that scrolls the viewport, whose content is the document.
    ViewportScroller,
    Scroller,
    /// A sticky box held at its sticky edge, laid out at `border_box_origin`, which its translation is rounded relative
    /// to as world transforms round it.
    HeldStickyBox {
        border_box_origin: FloatPoint,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterSpace {
    pub anchor: SpatialNodeIndex,
    /// Names the space by its anchor's scroll node or sticky box as the page knows it, which outlives the tree and its
    /// indices.
    pub identity: u64,
    pub parent: usize,
    pub anchor_kind: RasterSpaceAnchorKind,
    /// How far the anchor can translate its content: the negated scroll range of a scroller, the offset range of a
    /// sticky box.
    pub lowest_anchor_translation: FloatPoint,
    pub highest_anchor_translation: FloatPoint,
    /// Where content that moves with the anchor can show, in the space's raster coordinates: a scroller's scrollport
    /// over the whole scroll range, and for a sticky box the extent of the space around it wherever the box is held.
    /// For the viewport's space, the viewport.
    pub extent: IntRect,
}

impl RasterSpace {
    pub(crate) fn of_the_viewport(viewport_rect: IntRect) -> Self {
        Self {
            anchor: VISUAL_VIEWPORT_NODE_INDEX,
            identity: 0,
            parent: VIEWPORT_RASTER_SPACE,
            anchor_kind: RasterSpaceAnchorKind::Viewport,
            lowest_anchor_translation: FloatPoint::default(),
            highest_anchor_translation: FloatPoint::default(),
            extent: viewport_rect,
        }
    }

    /// The translation of the anchor at `offset`, on the whole device pixels a direct raster translates it by.
    pub fn anchor_translation_on_whole_device_pixels(&self, offset: FloatPoint) -> IntPoint {
        let sticky_box_origin = match self.anchor_kind {
            RasterSpaceAnchorKind::HeldStickyBox { border_box_origin } => Some(border_box_origin),
            _ => None,
        };
        let translation = translation_on_whole_device_pixels(offset, sticky_box_origin);
        IntPoint {
            x: translation.x as i32,
            y: translation.y as i32,
        }
    }
}

/// The rect that content within `rect` covers wherever a translation between `lowest` and `highest` takes it, with the
/// rounding of that translation to whole device pixels.
pub(crate) fn rect_swept_over_translations(rect: IntRect, lowest: FloatPoint, highest: FloatPoint) -> IntRect {
    if rect.is_empty() {
        return rect;
    }
    let swept = FloatRect::new(
        rect.x as f32 + lowest.x,
        rect.y as f32 + lowest.y,
        rect.width as f32 + highest.x - lowest.x,
        rect.height as f32 + highest.y - lowest.y,
    );
    if !is_finite_rect(swept) {
        return limited_to_extent_limit(FloatRect::new(
            -EXTENT_LIMIT,
            -EXTENT_LIMIT,
            2.0 * EXTENT_LIMIT,
            2.0 * EXTENT_LIMIT,
        ));
    }
    let margin = ROUNDING_OF_AN_ANCHOR_TRANSLATION_IN_DEVICE_PIXELS;
    limited_to_extent_limit(swept).inflated_edges(margin, margin, margin, margin)
}

/// A rect that rasters of content are clipped to as they are composited, in the raster coordinates of a space whose
/// anchor the content does not move with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompositeClip {
    pub space: usize,
    pub rect: IntRect,
}

pub(crate) fn chain_of_space(spaces: &[RasterSpace], mut space: usize) -> impl Iterator<Item = usize> + '_ {
    std::iter::from_fn(move || {
        if space == VIEWPORT_RASTER_SPACE {
            return None;
        }
        let current = space;
        space = spaces[space].parent;
        Some(current)
    })
}

pub(crate) fn space_is_ancestor_or_self(spaces: &[RasterSpace], ancestor: usize, mut space: usize) -> bool {
    loop {
        if space == ancestor {
            return true;
        }
        if space == VIEWPORT_RASTER_SPACE {
            return false;
        }
        space = spaces[space].parent;
    }
}

pub(crate) fn lowest_common_space(spaces: &[RasterSpace], first: usize, second: usize) -> usize {
    let mut candidate = first;
    while !space_is_ancestor_or_self(spaces, candidate, second) {
        candidate = spaces[candidate].parent;
    }
    candidate
}

/// The scroll offsets content of `space` is rastered under: those of the frame, with the anchors of the space and of
/// the spaces it is in at their origin.
pub fn raster_scroll_offsets(spaces: &[RasterSpace], space: usize, scroll_offsets: &[FloatPoint]) -> Vec<FloatPoint> {
    let mut offsets = scroll_offsets.to_vec();
    for space in chain_of_space(spaces, space) {
        if let Some(offset) = offsets.get_mut(spaces[space].anchor.0 as usize) {
            *offset = FloatPoint::default();
        }
    }
    offsets
}

pub(crate) fn translation_of(matrix: &FloatMatrix4x4) -> Option<FloatPoint> {
    if !matrix.is_2d_affine() {
        return None;
    }
    let affine = matrix.extract_2d_affine();
    affine.is_identity_or_translation().then(|| FloatPoint {
        x: affine.e(),
        y: affine.f(),
    })
}

fn node_keeps_whole_pixel_translations_below_it(data: &SpatialData) -> bool {
    match data {
        SpatialData::Scroll(_) | SpatialData::BackfaceVisibility(_) => true,
        SpatialData::Transform(transform) => transform.matrix_including_origin() == FloatMatrix4x4::identity(),
        _ => false,
    }
}

/// Per spatial node, whether where it lands can change while the list and the tree's structure stay: it is under a
/// sticky box, an anchor scroll shift or a transform the compositor animates.
pub fn spatial_nodes_that_move_on_their_own(tree: &VisualContextTree) -> Vec<bool> {
    spatial_nodes_that_move_on_their_own_at_anchors(tree, None)
}

/// As `spatial_nodes_that_move_on_their_own`, where nodes under a scroll node the compositor scrolls that is not one of
/// `anchors` move on their own too, as rasters of the space they are placed in do not follow it, and a sticky box that
/// is one of `anchors` does not, as rasters of its space follow it.
fn spatial_nodes_that_move_on_their_own_at_anchors(
    tree: &VisualContextTree,
    anchors: Option<&[SpatialNodeIndex]>,
) -> Vec<bool> {
    let animated: Vec<SpatialNodeIndex> = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Transform)
        .flat_map(|animation| animation.node_indices.iter().map(|&index| SpatialNodeIndex(index)))
        .collect();
    let scrolled_by_the_compositor_without_a_space: Vec<SpatialNodeIndex> = match anchors {
        Some(anchors) => tree
            .compositor_scroll_table()
            .nodes
            .iter()
            .map(|record| record.scroll_node_index)
            .filter(|node| *node != VISUAL_VIEWPORT_NODE_INDEX && !anchors.contains(node))
            .collect(),
        None => Vec::new(),
    };
    let is_anchor = |node: SpatialNodeIndex| anchors.is_some_and(|anchors| anchors.contains(&node));
    let mut moves = vec![false; tree.spatial_nodes.len()];
    tree.visit_spatial_nodes_parents_first(|index| {
        let node = &tree.spatial_nodes[index];
        let node_index = SpatialNodeIndex(index as u32);
        let own = match node.data {
            SpatialData::Sticky(_) => !is_anchor(node_index),
            SpatialData::AnchorScrollShift(_) => true,
            _ => false,
        } || animated.contains(&node_index)
            || scrolled_by_the_compositor_without_a_space.contains(&node_index);
        let from_parent = index != VISUAL_VIEWPORT_NODE_INDEX.0 as usize && moves[node.parent.0 as usize];
        moves[index] = own || from_parent;
    });
    moves
}

/// Per effect node, whether how far its filters spread its content can change while the list and the tree's structure
/// stay, as the compositor animates the filter of it or of an effect above it.
pub fn effects_whose_spread_moves_on_its_own(tree: &VisualContextTree) -> Vec<bool> {
    let animated: Vec<EffectNodeIndex> = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Filter)
        .flat_map(|animation| animation.node_indices.iter().map(|&index| EffectNodeIndex(index)))
        .collect();
    let mut moves = vec![false; tree.effect_nodes.len()];
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let from_parent = !node.parent.is_none() && moves[node.parent.0 as usize];
        moves[index] = from_parent || animated.contains(&EffectNodeIndex(index as u32));
    });
    moves
}

fn visual_viewport_only_translates(tree: &VisualContextTree) -> bool {
    let Some(SpatialData::Transform(transform)) = tree.spatial_nodes.first().map(|node| &node.data) else {
        return false;
    };
    translation_of(&transform.matrix_including_origin()).is_some()
}

// How deep `node` is below the viewport, where its content lands on whole device pixels of the viewport wherever the
// node itself translates it, since nothing between it and the viewport transforms, moves on its own or can be animated
// to.
fn depth_on_whole_device_pixels(
    tree: &VisualContextTree,
    node: SpatialNodeIndex,
    moves_on_its_own: &[bool],
) -> Option<usize> {
    let mut depth = 1;
    let mut ancestor = tree.spatial_nodes.get(node.0 as usize)?.parent;
    while ancestor != VISUAL_VIEWPORT_NODE_INDEX {
        let ancestor_node = tree.spatial_nodes.get(ancestor.0 as usize)?;
        if depth > tree.spatial_nodes.len()
            || !node_keeps_whole_pixel_translations_below_it(&ancestor_node.data)
            || moves_on_its_own[ancestor.0 as usize]
        {
            return None;
        }
        depth += 1;
        ancestor = ancestor_node.parent;
    }
    Some(depth)
}

fn shallowest_first(mut nodes_with_depths: Vec<(usize, SpatialNodeIndex)>, most: usize) -> Vec<SpatialNodeIndex> {
    nodes_with_depths.sort_by_key(|&(depth, node)| (depth, node.0));
    nodes_with_depths.truncate(most);
    nodes_with_depths.into_iter().map(|(_, node)| node).collect()
}

// Whether a sticky box at `offset` is held at its sticky edge on some axis: pushed off where it is laid out, and short
// of where its containing block stops it, so that it stays put in its scrollport as its scroller scrolls. Elsewhere it
// moves with the content around it.
fn sticky_box_is_held(sticky: &StickyData, offset: FloatPoint) -> bool {
    let (lowest, highest) = sticky.offset_range_without_sticky_ancestors();
    let is_held_along = |offset: f32, lowest: f32, highest: f32| {
        (lowest < offset && offset < 0.0) || (0.0 < offset && offset < highest)
    };
    is_held_along(offset.x, lowest.x, highest.x) || is_held_along(offset.y, lowest.y, highest.y)
}

// As `spatial_nodes_that_move_on_their_own` gave it, where the nodes under a scroll node the compositor scrolls that is
// not one of `scrollers` move on their own too, as the rasters of the space they are placed in do not follow it.
fn moving_on_their_own_under_scrollers_without_spaces(
    tree: &VisualContextTree,
    spatial_nodes_that_move_on_their_own: &[bool],
    scrollers: &[SpatialNodeIndex],
) -> Vec<bool> {
    let mut moves = spatial_nodes_that_move_on_their_own.to_vec();
    let scrolled_by_the_compositor_without_a_space: Vec<SpatialNodeIndex> = tree
        .compositor_scroll_table()
        .nodes
        .iter()
        .map(|record| record.scroll_node_index)
        .filter(|node| *node != VISUAL_VIEWPORT_NODE_INDEX && !scrollers.contains(node))
        .collect();
    if scrolled_by_the_compositor_without_a_space.is_empty() {
        return moves;
    }
    tree.visit_spatial_nodes_parents_first(|index| {
        let from_parent =
            index != VISUAL_VIEWPORT_NODE_INDEX.0 as usize && moves[tree.spatial_nodes[index].parent.0 as usize];
        moves[index] |=
            from_parent || scrolled_by_the_compositor_without_a_space.contains(&SpatialNodeIndex(index as u32));
    });
    moves
}

// The sticky boxes held at their sticky edge under `scroll_offsets` whose content lands on whole device pixels, and
// whose scroller is one of `scrollers`, so that the box translates the content of its scroller's space alone.
fn held_sticky_boxes(
    tree: &VisualContextTree,
    scroll_offsets: &[FloatPoint],
    spatial_nodes_that_move_on_their_own: &[bool],
    scrollers: &[SpatialNodeIndex],
) -> Vec<(usize, SpatialNodeIndex)> {
    if !tree
        .spatial_nodes
        .iter()
        .any(|node| matches!(node.data, SpatialData::Sticky(_)))
    {
        return Vec::new();
    }
    let moves_on_its_own =
        moving_on_their_own_under_scrollers_without_spaces(tree, spatial_nodes_that_move_on_their_own, scrollers);
    tree.spatial_nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            let SpatialData::Sticky(sticky) = &node.data else {
                return None;
            };
            let node_index = SpatialNodeIndex(index as u32);
            if !sticky.non_scrolling_scroll_container.is_invalid() || !scrollers.contains(&sticky.scroller) {
                return None;
            }
            let depth = depth_on_whole_device_pixels(tree, node_index, &moves_on_its_own)?;
            let mut ancestor = node.parent;
            while ancestor != sticky.scroller && ancestor != VISUAL_VIEWPORT_NODE_INDEX {
                ancestor = tree.spatial_nodes[ancestor.0 as usize].parent;
            }
            let moves_with_its_scroller = ancestor == sticky.scroller;
            (moves_with_its_scroller && sticky_box_is_held(sticky, device_offset_for_index(scroll_offsets, node_index)))
                .then_some((depth, node_index))
        })
        .collect()
}

/// The nodes that layers can be rastered at, parents first. First the scroll nodes the compositor scrolls, over a
/// scroll range, whose content lands on whole device pixels of the viewport wherever they are scrolled to, since
/// nothing between them and the viewport transforms, or can be animated to. Then the sticky boxes held at their sticky
/// edge under `scroll_offsets` within those scrollers, which change only as a box starts or stops being held.
/// `spatial_nodes_that_move_on_their_own` is what the function of that name gives for the tree.
pub fn raster_space_anchors(
    tree: &VisualContextTree,
    scroll_offsets: &[FloatPoint],
    spatial_nodes_that_move_on_their_own: &[bool],
) -> Vec<SpatialNodeIndex> {
    if !visual_viewport_only_translates(tree) {
        return Vec::new();
    }
    let moves_on_its_own = spatial_nodes_that_move_on_their_own;
    let scrollers_with_depths: Vec<(usize, SpatialNodeIndex)> = tree
        .compositor_scroll_table()
        .nodes
        .iter()
        .filter_map(|record| {
            let node = record.scroll_node_index;
            if node == VISUAL_VIEWPORT_NODE_INDEX || record.min_scroll_offset == record.max_scroll_offset {
                return None;
            }
            if !matches!(tree.spatial_nodes.get(node.0 as usize)?.data, SpatialData::Scroll(_)) {
                return None;
            }
            Some((depth_on_whole_device_pixels(tree, node, moves_on_its_own)?, node))
        })
        .collect();
    let mut anchors = shallowest_first(scrollers_with_depths, MAXIMUM_SCROLLER_SPACE_COUNT);
    let held_sticky_boxes = held_sticky_boxes(tree, scroll_offsets, moves_on_its_own, &anchors);
    anchors.extend(shallowest_first(held_sticky_boxes, MAXIMUM_STICKY_BOX_SPACE_COUNT));
    anchors
}

fn scroll_record_of(tree: &VisualContextTree, node: SpatialNodeIndex) -> Option<&CompositorScrollNodeRecord> {
    tree.compositor_scroll_table()
        .nodes
        .iter()
        .find(|record| record.scroll_node_index == node)
}

fn identity_of_anchor(record: &CompositorScrollNodeRecord) -> u64 {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x7261_7374_6572_5f73).build_hasher();
    hasher.write_i64(record.scrollable_node_id.0);
    hasher.write_u8(record.scroll_node_kind as u8);
    hasher.write_u8(record.pseudo_element_type);
    // Nodes the page does not name are told apart by where they are.
    if record.scrollable_node_id.0 == 0 {
        hasher.write_u32(record.scroll_node_index.0);
    }
    hasher.finish() | 1
}

// Seeded apart from the identities of scroll nodes, as a box can be both sticky and a scroller.
fn identity_of_sticky_box_anchor(sticky: &StickyData, node: SpatialNodeIndex) -> u64 {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x7374_6963_6b79_5f62).build_hasher();
    hasher.write_i64(sticky.box_node_id.0);
    hasher.write_u8(sticky.box_pseudo_element_type);
    if sticky.box_node_id.0 == 0 {
        hasher.write_u32(node.0);
    }
    hasher.finish() | 1
}

fn limited_to_extent_limit(rect: FloatRect) -> IntRect {
    enclosing_int_rect(rect.intersected(FloatRect::new(
        -EXTENT_LIMIT,
        -EXTENT_LIMIT,
        2.0 * EXTENT_LIMIT,
        2.0 * EXTENT_LIMIT,
    )))
}

// The extent of `space` under `world`, in which the anchors of its chain are at their origin.
fn extent_of_space(
    tree: &VisualContextTree,
    spaces: &[RasterSpace],
    space: usize,
    world: &WorldTransforms,
    viewport_rect: IntRect,
) -> IntRect {
    let unbounded = limited_to_extent_limit(FloatRect::new(
        -EXTENT_LIMIT,
        -EXTENT_LIMIT,
        2.0 * EXTENT_LIMIT,
        2.0 * EXTENT_LIMIT,
    ));
    let space = &spaces[space];
    if let RasterSpaceAnchorKind::HeldStickyBox { .. } = space.anchor_kind {
        // Content of a held box shows where it is in the space around it, translated back by the box's offset.
        let extent_of_parent = if space.parent == VIEWPORT_RASTER_SPACE {
            viewport_rect
        } else {
            extent_of_space(tree, spaces, space.parent, world, viewport_rect)
        };
        let negated = |point: FloatPoint| FloatPoint {
            x: -point.x,
            y: -point.y,
        };
        return rect_swept_over_translations(
            extent_of_parent,
            negated(space.highest_anchor_translation),
            negated(space.lowest_anchor_translation),
        );
    }
    let Some(record) = scroll_record_of(tree, space.anchor) else {
        return unbounded;
    };
    let Some(port_offset) = translation_of(&world.draw_matrix(tree.spatial_nodes[space.anchor.0 as usize].parent))
    else {
        return unbounded;
    };
    let port = record
        .scrollport_rect
        .to_float()
        .translated(port_offset.x, port_offset.y);
    let (minimum, maximum) = (record.min_scroll_offset, record.max_scroll_offset);
    let extent = FloatRect::new(
        port.x + minimum.x.min(0.0),
        port.y + minimum.y.min(0.0),
        port.width + (maximum.x.max(0.0) - minimum.x.min(0.0)),
        port.height + (maximum.y.max(0.0) - minimum.y.min(0.0)),
    );
    if !is_finite_rect(extent) {
        return unbounded;
    }
    limited_to_extent_limit(extent)
}

/// The raster spaces of a tree at a set of anchors, and the deepest of them that each spatial node moves with.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterSpaces {
    spaces: Vec<RasterSpace>,
    deepest_space_of_spatial: Vec<usize>,
    spatial_moves_on_its_own: Vec<bool>,
    effect_spread_moves_on_its_own: Vec<bool>,
}

impl RasterSpaces {
    /// The spaces at `anchors`, which `raster_space_anchors` gave or a prefix of them, with the extent of each but the
    /// viewport's left to be filled in from its geometry.
    pub fn at_anchors(tree: &VisualContextTree, anchors: &[SpatialNodeIndex], viewport_rect: IntRect) -> Self {
        let mut spaces = vec![RasterSpace::of_the_viewport(viewport_rect)];
        for &anchor in anchors {
            let mut parent = VIEWPORT_RASTER_SPACE;
            let mut ancestor = tree.spatial_nodes[anchor.0 as usize].parent;
            while ancestor != VISUAL_VIEWPORT_NODE_INDEX {
                if let Some(space) = spaces.iter().position(|space| space.anchor == ancestor) {
                    parent = space;
                    break;
                }
                ancestor = tree.spatial_nodes[ancestor.0 as usize].parent;
            }
            if let SpatialData::Sticky(sticky) = &tree.spatial_nodes[anchor.0 as usize].data {
                let (lowest_anchor_translation, highest_anchor_translation) =
                    sticky.offset_range_without_sticky_ancestors();
                spaces.push(RasterSpace {
                    anchor,
                    identity: identity_of_sticky_box_anchor(sticky, anchor),
                    parent,
                    anchor_kind: RasterSpaceAnchorKind::HeldStickyBox {
                        border_box_origin: sticky.border_box_origin,
                    },
                    lowest_anchor_translation,
                    highest_anchor_translation,
                    extent: IntRect::default(),
                });
                continue;
            }
            let Some(record) = scroll_record_of(tree, anchor) else {
                continue;
            };
            let mut lowest_anchor_translation = FloatPoint {
                x: -record.max_scroll_offset.x,
                y: -record.max_scroll_offset.y,
            };
            let mut highest_anchor_translation = FloatPoint {
                x: -record.min_scroll_offset.x,
                y: -record.min_scroll_offset.y,
            };
            if record.shows_overscroll_effect_horizontally {
                lowest_anchor_translation.x -= record.scrollport_rect.width as f32;
                highest_anchor_translation.x += record.scrollport_rect.width as f32;
            }
            if record.shows_overscroll_effect_vertically {
                lowest_anchor_translation.y -= record.scrollport_rect.height as f32;
                highest_anchor_translation.y += record.scrollport_rect.height as f32;
            }
            spaces.push(RasterSpace {
                anchor,
                identity: identity_of_anchor(record),
                parent,
                anchor_kind: if record.is_viewport {
                    RasterSpaceAnchorKind::ViewportScroller
                } else {
                    RasterSpaceAnchorKind::Scroller
                },
                lowest_anchor_translation,
                highest_anchor_translation,
                extent: IntRect::default(),
            });
        }
        Self::of_spaces(tree, spaces)
    }

    /// The given spaces, which a tree offers, and the deepest of them each spatial node of the tree moves with.
    pub fn of_spaces(tree: &VisualContextTree, spaces: Vec<RasterSpace>) -> Self {
        let mut deepest = vec![VIEWPORT_RASTER_SPACE; tree.spatial_nodes.len()];
        tree.visit_spatial_nodes_parents_first(|index| {
            if index == VISUAL_VIEWPORT_NODE_INDEX.0 as usize {
                return;
            }
            let node = &tree.spatial_nodes[index];
            deepest[index] =
                if let Some(space) = spaces.iter().skip(1).position(|space| space.anchor.0 as usize == index) {
                    space + 1
                } else if matches!(node.data, SpatialData::AnchorScrollShift(_)) {
                    VIEWPORT_RASTER_SPACE
                } else {
                    deepest[node.parent.0 as usize]
                };
        });
        let anchors: Vec<SpatialNodeIndex> = spaces.iter().skip(1).map(|space| space.anchor).collect();
        Self {
            spaces,
            deepest_space_of_spatial: deepest,
            spatial_moves_on_its_own: spatial_nodes_that_move_on_their_own_at_anchors(tree, Some(&anchors)),
            effect_spread_moves_on_its_own: effects_whose_spread_moves_on_its_own(tree),
        }
    }

    pub fn spatial_moves_on_its_own(&self, spatial: SpatialNodeIndex) -> bool {
        self.spatial_moves_on_its_own
            .get(spatial.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    pub fn effect_spread_moves_on_its_own(&self, effect: EffectNodeIndex) -> bool {
        !effect.is_none()
            && self
                .effect_spread_moves_on_its_own
                .get(effect.0 as usize)
                .copied()
                .unwrap_or(false)
    }

    pub fn spaces(&self) -> &[RasterSpace] {
        &self.spaces
    }

    pub fn into_spaces(self) -> Vec<RasterSpace> {
        self.spaces
    }

    pub fn len(&self) -> usize {
        self.spaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spaces.is_empty()
    }

    pub fn deepest_space_of(&self, spatial: SpatialNodeIndex) -> usize {
        self.deepest_space_of_spatial
            .get(spatial.0 as usize)
            .copied()
            .unwrap_or(VIEWPORT_RASTER_SPACE)
    }

    pub fn moves_with(&self, spatial: SpatialNodeIndex, space: usize) -> bool {
        space_is_ancestor_or_self(&self.spaces, space, self.deepest_space_of(spatial))
    }

    /// Per clip node, whether rasters of `space` leave it to be applied as they are composited, since the clip does
    /// not move with the space.
    pub fn clips_left_to_composite(&self, space: usize, tree: &VisualContextTree) -> Vec<bool> {
        tree.clip_nodes
            .iter()
            .map(|node| {
                self.deepest_space_of_spatial
                    .get(node.spatial.0 as usize)
                    .is_none_or(|&deepest| !space_is_ancestor_or_self(&self.spaces, space, deepest))
            })
            .collect()
    }
}

pub(crate) fn is_finite_rect(rect: FloatRect) -> bool {
    rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()
}

/// Where the content of one raster space lands in the space's raster coordinates under the scroll offsets of a frame:
/// the transforms of the tree's spatial nodes with the anchors of the space's chain at their origin, what the tree
/// culls, and the bounds of its clip chains.
pub struct SpaceGeometry {
    pub raster_scroll_offsets: Vec<FloatPoint>,
    pub world: WorldTransforms,
    pub culling: TreeCullingScratch,
    /// Empty for the viewport's space, which applies every clip.
    pub clips_left_to_composite: Vec<bool>,
    /// Per clip node, the bounds of the clips in its chain that rasters of the space apply and that map affinely.
    pub clip_bounds_of_clips_rastered_in_space: Vec<Option<FloatRect>>,
    /// Per clip node, the bounds of the clips in its chain whose node moves with the space and nothing inside it.
    clip_bounds_of_clips_fixed_in_space: Vec<Option<FloatRect>>,
    pub extent: IntRect,
}

impl SpaceGeometry {
    /// The geometry of `space`, with the device pixels of the viewport mapped through `raster_transform` into those of
    /// the raster, which `viewport_rect` is in.
    pub fn new(
        tree: &VisualContextTree,
        spaces: &RasterSpaces,
        space: usize,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
        raster_transform: FloatMatrix4x4,
    ) -> Self {
        let raster_scroll_offsets = raster_scroll_offsets(spaces.spaces(), space, scroll_offsets);
        let world = WorldTransforms::new(tree, &raster_scroll_offsets, Some(raster_transform));
        let mut culling = TreeCullingScratch::default();
        tree.fill_culling_scratch(&mut culling);
        let clips_left_to_composite = if space == VIEWPORT_RASTER_SPACE {
            Vec::new()
        } else {
            spaces.clips_left_to_composite(space, tree)
        };
        let mut clip_bounds_of_clips_rastered_in_space: Vec<Option<FloatRect>> = vec![None; tree.clip_nodes.len()];
        let mut clip_bounds_of_clips_fixed_in_space: Vec<Option<FloatRect>> = vec![None; tree.clip_nodes.len()];
        tree.visit_clip_nodes_parents_first(|index| {
            let node = &tree.clip_nodes[index];
            let own = match &node.data {
                ClipNodeData::Rect(data) if data.mode == ClipMode::Intersect => Some(data.rect),
                ClipNodeData::Path(path) => Some(path.bounding_rect.to_float()),
                _ => None,
            };
            let parent_bounds = |bounds: &[Option<FloatRect>]| {
                if node.parent.is_none() {
                    None
                } else {
                    bounds[node.parent.0 as usize]
                }
            };
            let left_to_composite = clips_left_to_composite.get(index).copied().unwrap_or(false);
            let rastered = own
                .filter(|_| !left_to_composite && world.draw_matrix(node.spatial).is_2d_affine())
                .map(|rect| world.map_rect_to_root(node.spatial, rect))
                .filter(|rect| is_finite_rect(*rect));
            clip_bounds_of_clips_rastered_in_space[index] =
                intersect_bounds(rastered, parent_bounds(&clip_bounds_of_clips_rastered_in_space));
            let fixed = own
                .filter(|_| {
                    spaces.deepest_space_of(node.spatial) == space && !spaces.spatial_moves_on_its_own(node.spatial)
                })
                .map(|rect| world.map_rect_to_root(node.spatial, rect))
                .filter(|rect| is_finite_rect(*rect));
            clip_bounds_of_clips_fixed_in_space[index] =
                intersect_bounds(fixed, parent_bounds(&clip_bounds_of_clips_fixed_in_space));
        });
        let extent = if space == VIEWPORT_RASTER_SPACE {
            viewport_rect
        } else {
            extent_of_space(tree, spaces.spaces(), space, &world, viewport_rect)
        };
        Self {
            raster_scroll_offsets,
            world,
            culling,
            clips_left_to_composite,
            clip_bounds_of_clips_rastered_in_space,
            clip_bounds_of_clips_fixed_in_space,
            extent,
        }
    }

    /// The device pixels of the space that content within `rect` under `clip` can reach, as far as the clips fixed in
    /// the space bound it, within the space's extent.
    pub fn device_rect(&self, rect: Option<FloatRect>, clip: crate::display_list::commands::ClipNodeIndex) -> IntRect {
        let clip_bounds = if clip.is_none() {
            None
        } else {
            self.clip_bounds_of_clips_fixed_in_space[clip.0 as usize]
        };
        let rect = match (rect, clip_bounds) {
            (Some(rect), Some(clip_bounds)) => rect.intersected(clip_bounds),
            (Some(rect), None) => rect,
            (None, Some(clip_bounds)) => clip_bounds,
            (None, None) => return self.extent,
        };
        if !is_finite_rect(rect) {
            return self.extent;
        }
        if rect.is_empty() {
            return IntRect::default();
        }
        let extent = self.extent.to_float();
        let rect = rect.intersected(extent.inflated(
            2.0 * BOUNDS_MARGIN_IN_DEVICE_PIXELS as f32,
            2.0 * BOUNDS_MARGIN_IN_DEVICE_PIXELS as f32,
        ));
        if rect.is_empty() {
            return IntRect::default();
        }
        enclosing_int_rect(rect)
            .inflated_edges(
                BOUNDS_MARGIN_IN_DEVICE_PIXELS,
                BOUNDS_MARGIN_IN_DEVICE_PIXELS,
                BOUNDS_MARGIN_IN_DEVICE_PIXELS,
                BOUNDS_MARGIN_IN_DEVICE_PIXELS,
            )
            .intersected(self.extent)
    }

    pub fn ink_device_rect(&self, run: &DisplayListCommandRun, ink: IntRect) -> IntRect {
        if ink.is_empty()
            || self.world.backface_culled[run.context.spatial.0 as usize]
            || self.culling.context_culls_everything(run.context)
        {
            return IntRect::default();
        }
        let rect = self.world.map_rect_to_root(run.context.spatial, ink.to_float());
        self.device_rect(Some(rect), run.context.clip)
    }

    pub fn group_device_rect(
        &self,
        tree: &VisualContextTree,
        effect: EffectNodeIndex,
        output: Extent,
        effect_clips: &EffectClipPlan,
    ) -> IntRect {
        let node = &tree.effect_nodes[effect.0 as usize];
        let output_clip = effect_clips.output_clip(effect);
        match output {
            Extent::Nothing => IntRect::default(),
            Extent::Within(rect) => {
                self.device_rect(Some(self.world.map_rect_to_root(node.spatial, rect)), output_clip)
            }
            Extent::Anywhere => self.device_rect(None, output_clip),
        }
    }
}

pub(crate) fn intersect_bounds(bounds: Option<FloatRect>, limit: Option<FloatRect>) -> Option<FloatRect> {
    match (bounds, limit) {
        (Some(bounds), Some(limit)) => Some(bounds.intersected(limit)),
        (bounds, limit) => bounds.or(limit),
    }
}

// None where the context cannot be rastered in the space.
type CompositeClipsInSpace = Option<Rc<[CompositeClip]>>;

/// Where content is placed among the spaces at a set of anchors: the spaces, the clips that can be left to apply when
/// rasters are composited, and the effects whose content reads its backdrop. Two tables that compare equal place every
/// visual context alike, whatever tree copies they were worked out from. Where each context is placed is worked out as
/// it is first asked for.
pub struct PlacementTables {
    spaces: RasterSpaces,
    // Per clip node, the rect it clips to in the raster coordinates of the deepest space its node moves with, where
    // that is a rect of whole device pixels that rasters of spaces inside can leave to be applied as they are composited.
    composite_form_of_clips: Vec<Option<CompositeClip>>,
    // Per effect node: whether rasters of content under it in another space than its own cannot leave clips to
    // composite, as its content reads the backdrop, or whether it does not keep content out of other spaces at all.
    kind_of_effects: Vec<EffectKindForPlacement>,
    composite_clips_of_contexts: RefCell<HashMap<(ContextRef, usize), CompositeClipsInSpace>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectKindForPlacement {
    MovesNoContent,
    Groups,
    ReadsBackdrop,
}

impl PartialEq for PlacementTables {
    fn eq(&self, other: &Self) -> bool {
        self.spaces == other.spaces
            && self.composite_form_of_clips == other.composite_form_of_clips
            && self.kind_of_effects == other.kind_of_effects
    }
}

/// The spaces at `anchors` with the geometry of each under `scroll_offsets`.
pub fn spaces_with_geometries(
    tree: &VisualContextTree,
    anchors: &[SpatialNodeIndex],
    scroll_offsets: &[FloatPoint],
    viewport_rect: IntRect,
    raster_transform: FloatMatrix4x4,
) -> (RasterSpaces, Vec<Rc<SpaceGeometry>>) {
    let spaces = RasterSpaces::at_anchors(tree, anchors, viewport_rect);
    let geometries: Vec<Rc<SpaceGeometry>> = (0..spaces.len())
        .map(|space| {
            Rc::new(SpaceGeometry::new(
                tree,
                &spaces,
                space,
                scroll_offsets,
                viewport_rect,
                raster_transform,
            ))
        })
        .collect();
    (spaces, geometries)
}

impl PlacementTables {
    /// The tables of `spaces`, whose geometry each of `geometries` holds, which also fills in the extent of each space.
    pub fn new(tree: &VisualContextTree, spaces: RasterSpaces, geometries: &[Rc<SpaceGeometry>]) -> Self {
        let RasterSpaces {
            spaces: mut spaces_with_extents,
            deepest_space_of_spatial,
            spatial_moves_on_its_own,
            effect_spread_moves_on_its_own,
        } = spaces;
        for (space, geometry) in spaces_with_extents.iter_mut().zip(geometries) {
            space.extent = geometry.extent;
        }
        let spaces = RasterSpaces {
            spaces: spaces_with_extents,
            deepest_space_of_spatial,
            spatial_moves_on_its_own,
            effect_spread_moves_on_its_own,
        };
        let composite_form_of_clips = tree
            .clip_nodes
            .iter()
            .map(|node| {
                let ClipNodeData::Rect(data) = &node.data else {
                    return None;
                };
                if data.mode != ClipMode::Intersect || data.corner_radii.has_any_radius() {
                    return None;
                }
                let clip_space = spaces.deepest_space_of(node.spatial);
                let world = &geometries[clip_space].world;
                translation_of(&world.draw_matrix(node.spatial))?;
                let rect = world.map_rect_to_root(node.spatial, data.rect);
                // A clip of one axis alone has edges far past any content on the other, which clip as much at the
                // limit of extents as where they are.
                let [left, top, right, bottom] =
                    [rect.x, rect.y, rect.right(), rect.bottom()].map(|edge| edge.clamp(-EXTENT_LIMIT, EXTENT_LIMIT));
                if ![left, top, right, bottom].iter().all(|edge| {
                    (edge - edge.round()).abs()
                        <= LARGEST_FRACTION_OF_A_DEVICE_PIXEL_A_COMPOSITE_CLIP_EDGE_MAY_BE_OFF_BY
                }) {
                    return None;
                }
                Some(CompositeClip {
                    space: clip_space,
                    rect: IntRect::new(
                        left.round() as i32,
                        top.round() as i32,
                        right.round() as i32 - left.round() as i32,
                        bottom.round() as i32 - top.round() as i32,
                    ),
                })
            })
            .collect();
        let kind_of_effects = tree
            .effect_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                if tree.root_isolation_effect == Some(EffectNodeIndex(index as u32))
                    || matches!(node.data, EffectNodeData::BackgroundColorAnimation)
                {
                    return EffectKindForPlacement::MovesNoContent;
                }
                let reads_backdrop = match &node.data {
                    EffectNodeData::Effects(effects) => effects.backdrop_filter.is_some(),
                    EffectNodeData::Mask(mask) => mask.masks_backdrop,
                    _ => false,
                };
                if reads_backdrop {
                    EffectKindForPlacement::ReadsBackdrop
                } else {
                    EffectKindForPlacement::Groups
                }
            })
            .collect();
        Self {
            spaces,
            composite_form_of_clips,
            kind_of_effects,
            composite_clips_of_contexts: RefCell::new(HashMap::new()),
        }
    }

    pub fn spaces(&self) -> &[RasterSpace] {
        self.spaces.spaces()
    }

    pub fn raster_spaces(&self) -> &RasterSpaces {
        &self.spaces
    }

    pub fn deepest_space_of(&self, spatial: SpatialNodeIndex) -> usize {
        self.spaces.deepest_space_of(spatial)
    }

    pub fn moves_with(&self, spatial: SpatialNodeIndex, space: usize) -> bool {
        self.spaces.moves_with(spatial, space)
    }

    /// The clips that rasters of `context` in `space` leave to be applied as they are composited, in the order its
    /// clip chain names them, or none where the context cannot be rastered in the space: it does not move with the
    /// space, an effect of it does not, or a clip that does not cannot be applied as a rect of whole device pixels.
    pub fn composite_clips_in(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        space: usize,
    ) -> Option<Rc<[CompositeClip]>> {
        if let Some(known) = self.composite_clips_of_contexts.borrow().get(&(context, space)) {
            return known.clone();
        }
        let clips = self.work_out_composite_clips_in(tree, context, space).map(Rc::from);
        self.composite_clips_of_contexts
            .borrow_mut()
            .insert((context, space), clips.clone());
        clips
    }

    fn work_out_composite_clips_in(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        space: usize,
    ) -> Option<Vec<CompositeClip>> {
        if !self.moves_with(context.spatial, space) {
            return None;
        }
        let mut composite_clips = Vec::new();
        let mut clip = context.clip;
        while !clip.is_none() {
            let node = &tree.clip_nodes[clip.0 as usize];
            let composite_form = self.composite_form_of_clips[clip.0 as usize];
            clip = node.parent;
            if self.moves_with(node.spatial, space) {
                continue;
            }
            let composite_clip = composite_form?;
            if !composite_clips.contains(&composite_clip) {
                composite_clips.push(composite_clip);
            }
        }
        let mut reads_backdrop = false;
        let mut effect = context.effect;
        while !effect.is_none() {
            let node = &tree.effect_nodes[effect.0 as usize];
            match self.kind_of_effects[effect.0 as usize] {
                EffectKindForPlacement::MovesNoContent => {}
                kind => {
                    if !self.moves_with(node.spatial, space) {
                        return None;
                    }
                    reads_backdrop |= kind == EffectKindForPlacement::ReadsBackdrop;
                }
            }
            effect = node.parent;
        }
        if reads_backdrop && !composite_clips.is_empty() {
            return None;
        }
        Some(composite_clips)
    }

    /// Whether content of `context` rastered in `space` moves within it as an anchor of a space inside it scrolls, a
    /// sticky box sticks or the compositor animates a transform or a filter, so that where it lands holds at the
    /// current values only.
    pub fn context_moves_within_space(&self, tree: &VisualContextTree, context: ContextRef, space: usize) -> bool {
        let spaces = self.spaces.spaces();
        let moves_with_a_space_inside = |spatial: SpatialNodeIndex| {
            let deepest = self.deepest_space_of(spatial);
            (deepest != space && space_is_ancestor_or_self(spaces, space, deepest))
                || self.spaces.spatial_moves_on_its_own(spatial)
        };
        if self.spaces.effect_spread_moves_on_its_own(context.effect) {
            return true;
        }
        if moves_with_a_space_inside(context.spatial) {
            return true;
        }
        let mut clip = context.clip;
        while !clip.is_none() {
            let node = &tree.clip_nodes[clip.0 as usize];
            if moves_with_a_space_inside(node.spatial) {
                return true;
            }
            clip = node.parent;
        }
        let mut effect = context.effect;
        while !effect.is_none() {
            let node = &tree.effect_nodes[effect.0 as usize];
            if tree.root_isolation_effect != Some(effect) && moves_with_a_space_inside(node.spatial) {
                return true;
            }
            effect = node.parent;
        }
        false
    }
}

/// The records of runs that a space whose anchor what they draw does not move with cannot raster: records with inline
/// clips in spaces of their own, and scroll bars whose thumbs follow a scroll offset, as the chunks they are in know.
pub struct RunsReachingOutsideTheirContext {
    inline_clip_spaces_of_runs: HashMap<usize, Vec<SpatialNodeIndex>>,
    scroll_bar_nodes_of_runs: HashMap<usize, Vec<SpatialNodeIndex>>,
}

impl RunsReachingOutsideTheirContext {
    pub fn new(list: &DisplayListStorage) -> Self {
        let mut inline_clip_spaces_of_runs: HashMap<usize, Vec<SpatialNodeIndex>> = HashMap::new();
        let mut scroll_bar_nodes_of_runs: HashMap<usize, Vec<SpatialNodeIndex>> = HashMap::new();
        for (chunk_index, chunk) in list.chunks().iter().enumerate() {
            let first_run = list.first_run_of_chunk(chunk_index as u32);
            for record in chunk.records_reaching_outside() {
                let run_index = first_run + record.run_index as usize;
                match record.reach {
                    ReachOutsideChunk::InlineClipInSpaceOf(space) => {
                        inline_clip_spaces_of_runs.entry(run_index).or_default().push(space);
                    }
                    ReachOutsideChunk::ScrollbarThumbFollowsScrollOffsetOf(scroll_node_index) => {
                        scroll_bar_nodes_of_runs
                            .entry(run_index)
                            .or_default()
                            .push(scroll_node_index);
                    }
                }
            }
        }
        Self {
            inline_clip_spaces_of_runs,
            scroll_bar_nodes_of_runs,
        }
    }

    /// The clips rasters of the run in `space` leave to be composited, or none where the run cannot be rastered there.
    pub fn composite_clips_of_run_in(
        &self,
        tree: &VisualContextTree,
        tables: &PlacementTables,
        run_index: usize,
        run: &DisplayListCommandRun,
        space: usize,
    ) -> Option<Rc<[CompositeClip]>> {
        let composite_clips = tables.composite_clips_in(tree, run.context, space)?;
        if self
            .inline_clip_spaces_of_runs
            .get(&run_index)
            .is_some_and(|clip_spaces| {
                clip_spaces.iter().any(|&clip_space| {
                    (clip_space.0 as usize) < tree.spatial_nodes.len() && !tables.moves_with(clip_space, space)
                })
            })
        {
            return None;
        }
        if self.scroll_bar_nodes_of_runs.get(&run_index).is_some_and(|nodes| {
            chain_of_space(tables.spaces(), space).any(|space| nodes.contains(&tables.spaces()[space].anchor))
        }) {
            return None;
        }
        Some(composite_clips)
    }

    /// The deepest space the run can be rastered in.
    pub fn deepest_space_of_run(
        &self,
        tree: &VisualContextTree,
        tables: &PlacementTables,
        run_index: usize,
        run: &DisplayListCommandRun,
    ) -> usize {
        let mut space = tables.deepest_space_of(run.context.spatial);
        while space != VIEWPORT_RASTER_SPACE
            && self
                .composite_clips_of_run_in(tree, tables, run_index, run, space)
                .is_none()
        {
            space = tables.spaces()[space].parent;
        }
        space
    }
}

/// Runs that are placed together, as they hold every run of the group effects among them, in paint order.
pub struct RunGroups {
    /// The first and the last run of each group.
    ranges: Vec<(u32, u32)>,
    group_of_run: Vec<u32>,
    /// Per run, the outermost effect of its chain that composites its content as a group, other than the root's.
    grouping_effect_of_run: Vec<EffectNodeIndex>,
    /// Per effect node, the outermost effect of its chain that composites its content as a group.
    outermost_grouping_effects: Vec<EffectNodeIndex>,
    group_is_under_mask: Vec<bool>,
}

// The structure decides the grouping effect rather than the effect values, so groups stay put while an opacity animates.
fn outermost_grouping_effects(tree: &VisualContextTree) -> Vec<EffectNodeIndex> {
    let mut outermost = vec![EffectNodeIndex::NONE; tree.effect_nodes.len()];
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let from_parent = if node.parent.is_none() {
            EffectNodeIndex::NONE
        } else {
            outermost[node.parent.0 as usize]
        };
        let is_grouping = node.data.pushes_layer() && tree.root_isolation_effect != Some(EffectNodeIndex(index as u32));
        outermost[index] = if !from_parent.is_none() {
            from_parent
        } else if is_grouping {
            EffectNodeIndex(index as u32)
        } else {
            EffectNodeIndex::NONE
        };
    });
    outermost
}

fn run_ranges_holding_whole_grouping_effects(grouping_of_run: &[EffectNodeIndex]) -> Vec<(u32, u32)> {
    let mut last_run_of_group = HashMap::new();
    for (run_index, group) in grouping_of_run.iter().enumerate() {
        if !group.is_none() {
            last_run_of_group.insert(group.0, run_index);
        }
    }
    let mut ranges = Vec::new();
    let mut run_index = 0;
    while run_index < grouping_of_run.len() {
        let first = run_index;
        let mut last = run_index;
        let mut next = run_index;
        while next <= last {
            let group = grouping_of_run[next];
            if !group.is_none() {
                last = last.max(last_run_of_group[&group.0]);
            }
            next += 1;
        }
        ranges.push((first as u32, last as u32));
        run_index = last + 1;
    }
    ranges
}

impl RunGroups {
    /// The groups of the runs of a list, which the structure of its tree decides.
    pub fn new<'a>(
        tree: &VisualContextTree,
        command_runs: impl IntoIterator<Item = &'a DisplayListCommandRun>,
    ) -> Self {
        let outermost_grouping_effects = outermost_grouping_effects(tree);
        let mut effect_chain_holds_mask = vec![false; tree.effect_nodes.len()];
        tree.visit_effect_nodes_parents_first(|index| {
            let node = &tree.effect_nodes[index];
            let parent_holds_mask = !node.parent.is_none() && effect_chain_holds_mask[node.parent.0 as usize];
            effect_chain_holds_mask[index] = parent_holds_mask || matches!(node.data, EffectNodeData::Mask(_));
        });
        let (grouping_effect_of_run, run_is_under_mask): (Vec<EffectNodeIndex>, Vec<bool>) = command_runs
            .into_iter()
            .map(|run| {
                if run.context.effect.is_none() {
                    (EffectNodeIndex::NONE, false)
                } else {
                    (
                        outermost_grouping_effects[run.context.effect.0 as usize],
                        effect_chain_holds_mask[run.context.effect.0 as usize],
                    )
                }
            })
            .unzip();
        let ranges = run_ranges_holding_whole_grouping_effects(&grouping_effect_of_run);
        let mut group_of_run = vec![0u32; grouping_effect_of_run.len()];
        for (group, &(first, last)) in ranges.iter().enumerate() {
            group_of_run[first as usize..=last as usize].fill(group as u32);
        }

        let group_is_under_mask = ranges
            .iter()
            .map(|&(first, last)| {
                run_is_under_mask[first as usize..=last as usize]
                    .iter()
                    .any(|&under_mask| under_mask)
            })
            .collect();
        Self {
            ranges,
            group_of_run,
            grouping_effect_of_run,
            outermost_grouping_effects,
            group_is_under_mask,
        }
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub fn group_of_run(&self, run_index: usize) -> usize {
        self.group_of_run[run_index] as usize
    }

    pub fn grouping_effect_of_run(&self, run_index: usize) -> EffectNodeIndex {
        self.grouping_effect_of_run[run_index]
    }

    pub fn grouping_effect_of(&self, effect: EffectNodeIndex) -> EffectNodeIndex {
        if effect.is_none() {
            EffectNodeIndex::NONE
        } else {
            self.outermost_grouping_effects[effect.0 as usize]
        }
    }

    pub fn run_is_a_group_of_its_own(&self, run_index: usize) -> bool {
        let (first, last) = self.ranges[self.group_of_run(run_index)];
        first == last
    }

    /// Whether the group is under a mask, whose content is replayed whole wherever any of it shows.
    pub fn group_is_under_mask(&self, group: usize) -> bool {
        self.group_is_under_mask[group]
    }
}
