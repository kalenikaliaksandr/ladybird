/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Where the content of a display list can be rastered: the raster spaces a visual context tree offers, anchored at
//! the scroll nodes the compositor scrolls and at the sticky boxes within them, the space each visual
//! context is placed in and the clips left to apply when its rasters are composited, and the runs that are placed
//! together because they hold whole group effects.

use crate::display_list::animated_transform_spaces::{animated_transform_anchors, raster_scale_of};
use crate::display_list::chunks::ReachOutsideRunContext;
use crate::display_list::commands::{
    ClipMode, ClipNodeIndex, ContextRef, DisplayListCommandRun, EffectNodeIndex, SpatialNodeIndex,
    VISUAL_VIEWPORT_NODE_INDEX,
};
use crate::display_list::effect_clip_plan::EffectClipPlan;
use crate::display_list::effect_geometry::{
    Extent, changed_extent_through_filters, changed_extent_through_filters_inside, filter_may_spread_at_some_sample,
    spreading_filter_of,
};
use crate::display_list::reach::Reach;
use crate::display_list::storage::{ChunkIndexInList, DisplayListStorage};
use crate::host::FfiVisualAnimationTargetKind;
use crate::visual_context::queries::TreeCullingScratch;
use crate::visual_context::world_transforms::translation_on_whole_device_pixels;
use crate::visual_context::{
    ClipNodeData, CompositorScrollNodeRecord, EffectNodeData, RasterRoot, SpatialData, StickyData, VisualContextTree,
    WorldTransforms,
};
use libgfx_rust::{CornerRadii, FloatMatrix4x4, FloatPoint, FloatRect, IntPoint, IntRect, enclosing_int_rect};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;

/// The index of a raster space among the spaces of a placement, each after the space it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RasterSpaceIndex(pub usize);

impl RasterSpaceIndex {
    pub const VIEWPORT: Self = Self(0);
}

const MAXIMUM_SCROLLER_SPACE_COUNT: usize = 7;
// Enough for twenty section headers and a row of sticky table headers.
const MAXIMUM_STICKY_BOX_SPACE_COUNT: usize = 32;
/// How far past its bounds antialiasing can show content.
pub(crate) const ANTIALIASING_MARGIN_IN_DEVICE_PIXELS: i32 = 3;
const LARGEST_DEVICE_PIXEL_RATIO_EXTENTS_COVER_PAGES_AT: f32 = 8.0;
// Small enough that a rect twice as wide still fits in an `IntRect`.
pub(crate) const EXTENT_LIMIT_IN_DEVICE_PIXELS: f32 =
    (crate::css_pixels::MAX_INTEGER_VALUE as f32 + 1.0) * LARGEST_DEVICE_PIXEL_RATIO_EXTENTS_COVER_PAGES_AT;
const COMPOSITE_CLIP_EDGE_TOLERANCE_IN_DEVICE_PIXELS: f32 = 1.0 / 64.0;
const ANCHOR_TRANSLATION_ROUNDING_OVERSHOOT_IN_DEVICE_PIXELS: i32 = 1;

fn extent_limit_rect() -> FloatRect {
    FloatRect::new(
        -EXTENT_LIMIT_IN_DEVICE_PIXELS,
        -EXTENT_LIMIT_IN_DEVICE_PIXELS,
        2.0 * EXTENT_LIMIT_IN_DEVICE_PIXELS,
        2.0 * EXTENT_LIMIT_IN_DEVICE_PIXELS,
    )
}

/// What the anchor of a space is, which decides how its translation is rounded and what the dump calls the space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RasterSpaceAnchorKind {
    Viewport,
    /// The scroll node that scrolls the viewport, whose content is the document.
    ViewportScroller,
    Scroller,
    /// A sticky box, laid out at `border_box_origin`, which its translation is rounded relative to as world transforms
    /// round it.
    StickyBox {
        border_box_origin: FloatPoint,
    },
    /// A spatial node whose transform the compositor animates, whose content is rastered in its own coordinates times
    /// `raster_scale` and composited through the transform each frame.
    AnimatedTransform {
        raster_scale: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterSpace {
    pub anchor: SpatialNodeIndex,
    /// Names the space by its anchor's scroll node or sticky box as the page knows it, which outlives the tree and its
    /// indices.
    pub identity: u64,
    pub parent_index: RasterSpaceIndex,
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
    pub(crate) fn of_viewport(viewport_rect: IntRect) -> Self {
        Self {
            anchor: VISUAL_VIEWPORT_NODE_INDEX,
            identity: 0,
            parent_index: RasterSpaceIndex::VIEWPORT,
            anchor_kind: RasterSpaceAnchorKind::Viewport,
            lowest_anchor_translation: FloatPoint::default(),
            highest_anchor_translation: FloatPoint::default(),
            extent: viewport_rect,
        }
    }

    /// For a space whose anchor the compositor animates, the root its rasters map its anchor's content from.
    pub fn raster_root(&self) -> Option<RasterRoot> {
        match self.anchor_kind {
            RasterSpaceAnchorKind::AnimatedTransform { raster_scale } => Some(RasterRoot {
                anchor: self.anchor,
                raster_scale,
            }),
            _ => None,
        }
    }

    pub fn anchor_translation_on_whole_device_pixels(&self, offset: FloatPoint) -> IntPoint {
        let sticky_box_origin = match self.anchor_kind {
            RasterSpaceAnchorKind::StickyBox { border_box_origin } => Some(border_box_origin),
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
        return enclosing_int_rect_within_extent_limit(extent_limit_rect());
    }
    let margin = ANCHOR_TRANSLATION_ROUNDING_OVERSHOOT_IN_DEVICE_PIXELS;
    enclosing_int_rect_within_extent_limit(swept).inflated_edges(margin, margin, margin, margin)
}

/// A rect, with rounded corners or not, that rasters of content are clipped to as they are composited, in the raster
/// coordinates of a space whose anchor the content does not move with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompositeClip {
    pub raster_space_index: RasterSpaceIndex,
    pub rect: IntRect,
    pub corner_radii: CornerRadii,
}

impl CompositeClip {
    pub fn is_rounded(&self) -> bool {
        self.corner_radii.has_any_radius()
    }

    /// Whether the clip leaves all of `rect` unclipped: it lies within the clip's rect and outside its corner boxes.
    pub fn leaves_all_of(&self, rect: IntRect) -> bool {
        if !self.rect.contains_rect(rect) {
            return false;
        }
        let radii = self.corner_radii;
        let (left, top, right, bottom) = (self.rect.x, self.rect.y, self.rect.right(), self.rect.bottom());
        [
            IntRect::new(
                left,
                top,
                radii.top_left.horizontal_radius,
                radii.top_left.vertical_radius,
            ),
            IntRect::new(
                right - radii.top_right.horizontal_radius,
                top,
                radii.top_right.horizontal_radius,
                radii.top_right.vertical_radius,
            ),
            IntRect::new(
                right - radii.bottom_right.horizontal_radius,
                bottom - radii.bottom_right.vertical_radius,
                radii.bottom_right.horizontal_radius,
                radii.bottom_right.vertical_radius,
            ),
            IntRect::new(
                left,
                bottom - radii.bottom_left.vertical_radius,
                radii.bottom_left.horizontal_radius,
                radii.bottom_left.vertical_radius,
            ),
        ]
        .iter()
        .all(|corner_box| corner_box.is_empty() || corner_box.intersected(rect).is_empty())
    }
}

pub(crate) fn space_and_ancestors_below_viewport(
    spaces: &[RasterSpace],
    mut space: RasterSpaceIndex,
) -> impl Iterator<Item = RasterSpaceIndex> + '_ {
    std::iter::from_fn(move || {
        if space == RasterSpaceIndex::VIEWPORT {
            return None;
        }
        let current = space;
        space = spaces[space.0].parent_index;
        Some(current)
    })
}

pub(crate) fn space_is_ancestor_or_self(
    spaces: &[RasterSpace],
    ancestor: RasterSpaceIndex,
    mut space: RasterSpaceIndex,
) -> bool {
    loop {
        if space == ancestor {
            return true;
        }
        if space == RasterSpaceIndex::VIEWPORT {
            return false;
        }
        space = spaces[space.0].parent_index;
    }
}

pub(crate) fn lowest_common_ancestor_space(
    spaces: &[RasterSpace],
    first: RasterSpaceIndex,
    second: RasterSpaceIndex,
) -> RasterSpaceIndex {
    let mut candidate = first;
    while !space_is_ancestor_or_self(spaces, candidate, second) {
        candidate = spaces[candidate.0].parent_index;
    }
    candidate
}

/// The scroll offsets content of `space` is rastered under: those of the frame, with the anchors of the space and of
/// the spaces it is in at their origin.
pub fn raster_scroll_offsets(
    spaces: &[RasterSpace],
    space: RasterSpaceIndex,
    scroll_offsets: &[FloatPoint],
) -> Vec<FloatPoint> {
    let mut offsets = scroll_offsets.to_vec();
    for space in space_and_ancestors_below_viewport(spaces, space) {
        if let Some(offset) = offsets.get_mut(spaces[space.0].anchor.0 as usize) {
            *offset = FloatPoint::default();
        }
    }
    offsets
}

pub(crate) fn translation_if_only_translates(matrix: &FloatMatrix4x4) -> Option<FloatPoint> {
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
    spatial_nodes_that_move_on_their_own_relative_to_spaces_at(tree, None)
}

fn spatial_nodes_that_move_on_their_own_relative_to_spaces_at(
    tree: &VisualContextTree,
    anchors: Option<&[SpatialNodeIndex]>,
) -> Vec<bool> {
    let is_anchor = |node: SpatialNodeIndex| anchors.is_some_and(|anchors| anchors.contains(&node));
    let mut roots_of_subtrees_moving_on_their_own: Vec<SpatialNodeIndex> = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Transform)
        .flat_map(|animation| animation.node_indices.iter().map(|&index| SpatialNodeIndex(index)))
        .filter(|&node| !is_anchor(node))
        .collect();
    if let Some(anchors) = anchors {
        roots_of_subtrees_moving_on_their_own.extend(compositor_scrolled_nodes_without_own_space(tree, anchors));
    }
    roots_of_subtrees_moving_on_their_own.extend(tree.spatial_nodes.iter().enumerate().filter_map(|(index, node)| {
        let node_index = SpatialNodeIndex(index as u32);
        let moves = match node.data {
            SpatialData::Sticky(_) => !is_anchor(node_index),
            SpatialData::AnchorScrollShift(_) => true,
            _ => false,
        };
        moves.then_some(node_index)
    }));
    tree.spatial_nodes_in_subtrees_of(&roots_of_subtrees_moving_on_their_own)
}

/// Per spatial node, whether placing a list among the spaces at `anchors` leaves out the node's scroll offset: the
/// compositor scrolls the node, or the node moves on its own in those spaces. What moves with such a node is placed in
/// a space that follows it, or wherever the node can take it.
pub fn spatial_nodes_whose_scroll_offset_placement_leaves_out(
    tree: &VisualContextTree,
    anchors: &[SpatialNodeIndex],
) -> Vec<bool> {
    let mut leaves_out_by_node = spatial_nodes_that_move_on_their_own_relative_to_spaces_at(tree, Some(anchors));
    for record in tree.compositor_scroll_table().nodes.iter() {
        if let Some(leaves_out) = leaves_out_by_node.get_mut(record.scroll_node_index.0 as usize) {
            *leaves_out = true;
        }
    }
    leaves_out_by_node
}

// Empty where the tree has no transform animations.
fn spatial_nodes_that_animation_samples_move_within_their_space(
    tree: &VisualContextTree,
    spaces: &[RasterSpace],
) -> Vec<bool> {
    let roots: Vec<SpatialNodeIndex> = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Transform)
        .flat_map(|animation| animation.node_indices.iter().map(|&index| SpatialNodeIndex(index)))
        .filter(|&node| spaces.iter().skip(1).all(|space| space.anchor != node))
        .collect();
    if roots.is_empty() {
        return Vec::new();
    }
    tree.spatial_nodes_in_subtrees_of(&roots)
}

fn compositor_scrolled_nodes_without_own_space<'a>(
    tree: &'a VisualContextTree,
    scrollers_with_spaces: &'a [SpatialNodeIndex],
) -> impl Iterator<Item = SpatialNodeIndex> + 'a {
    tree.compositor_scroll_table()
        .nodes
        .iter()
        .map(|record| record.scroll_node_index)
        .filter(move |node| *node != VISUAL_VIEWPORT_NODE_INDEX && !scrollers_with_spaces.contains(node))
}

/// Per effect node, whether how far its filters spread its content can change while the list and the tree's structure
/// stay, as the compositor animates the filter of it or of an effect above it, and the filter spreads at some sample.
pub fn effects_whose_spread_moves_on_its_own(tree: &VisualContextTree) -> Vec<bool> {
    let mut spread_moves = vec![false; tree.effect_nodes.len()];
    if !tree
        .visual_animations()
        .iter()
        .any(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Filter)
    {
        return spread_moves;
    }
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let effect = EffectNodeIndex(index as u32);
        let spread_of_parent_moves = !node.parent.is_none() && spread_moves[node.parent.0 as usize];
        spread_moves[index] = spread_of_parent_moves
            || (tree.effect_filter_is_animated(effect) && filter_may_spread_at_some_sample(tree, effect));
    });
    spread_moves
}

fn visual_viewport_only_translates(tree: &VisualContextTree) -> bool {
    let Some(SpatialData::Transform(transform)) = tree.spatial_nodes.first().map(|node| &node.data) else {
        return false;
    };
    translation_if_only_translates(&transform.matrix_including_origin()).is_some()
}

fn depth_below_viewport_if_on_whole_device_pixels(
    tree: &VisualContextTree,
    node: SpatialNodeIndex,
    spatial_node_moves_on_its_own: &[bool],
) -> Option<usize> {
    let mut depth = 1;
    let mut ancestor = tree.spatial_nodes.get(node.0 as usize)?.parent;
    while ancestor != VISUAL_VIEWPORT_NODE_INDEX {
        let ancestor_node = tree.spatial_nodes.get(ancestor.0 as usize)?;
        if depth > tree.spatial_nodes.len()
            || !node_keeps_whole_pixel_translations_below_it(&ancestor_node.data)
            || spatial_node_moves_on_its_own[ancestor.0 as usize]
        {
            return None;
        }
        depth += 1;
        ancestor = ancestor_node.parent;
    }
    Some(depth)
}

fn shallowest_first_up_to(
    mut depths_and_nodes: Vec<(usize, SpatialNodeIndex)>,
    maximum_count: usize,
) -> Vec<SpatialNodeIndex> {
    depths_and_nodes.sort_by_key(|&(depth, node)| (depth, node.0));
    depths_and_nodes.truncate(maximum_count);
    depths_and_nodes.into_iter().map(|(_, node)| node).collect()
}

fn spatial_nodes_moving_on_their_own_or_under_scrollers_without_space(
    tree: &VisualContextTree,
    spatial_nodes_that_move_on_their_own: &[bool],
    scrollers_with_spaces: &[SpatialNodeIndex],
) -> Vec<bool> {
    let scrolled_nodes_without_own_space: Vec<SpatialNodeIndex> =
        compositor_scrolled_nodes_without_own_space(tree, scrollers_with_spaces).collect();
    if scrolled_nodes_without_own_space.is_empty() {
        return spatial_nodes_that_move_on_their_own.to_vec();
    }
    spatial_nodes_that_move_on_their_own
        .iter()
        .zip(tree.spatial_nodes_in_subtrees_of(&scrolled_nodes_without_own_space))
        .map(|(&moves_on_its_own, under_scroller_without_own_space)| {
            moves_on_its_own || under_scroller_without_own_space
        })
        .collect()
}

fn sticky_boxes_that_can_be_held_with_depths(
    tree: &VisualContextTree,
    spatial_nodes_that_move_on_their_own: &[bool],
    scrollers_with_spaces: &[SpatialNodeIndex],
) -> Vec<(usize, SpatialNodeIndex)> {
    if !tree
        .spatial_nodes
        .iter()
        .any(|node| matches!(node.data, SpatialData::Sticky(_)))
    {
        return Vec::new();
    }
    let spatial_node_moves_on_its_own = spatial_nodes_moving_on_their_own_or_under_scrollers_without_space(
        tree,
        spatial_nodes_that_move_on_their_own,
        scrollers_with_spaces,
    );
    tree.spatial_nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            let SpatialData::Sticky(sticky) = &node.data else {
                return None;
            };
            let node_index = SpatialNodeIndex(index as u32);
            if !sticky.non_scrolling_scroll_container.is_invalid() || !scrollers_with_spaces.contains(&sticky.scroller)
            {
                return None;
            }
            let depth =
                depth_below_viewport_if_on_whole_device_pixels(tree, node_index, &spatial_node_moves_on_its_own)?;
            let moves_with_its_scroller = tree.spatial_is_ancestor_or_self(sticky.scroller, node.parent);
            let (lowest, highest) = sticky.offset_range_without_sticky_ancestors();
            let can_be_held = lowest.x < 0.0 || lowest.y < 0.0 || highest.x > 0.0 || highest.y > 0.0;
            (moves_with_its_scroller && can_be_held).then_some((depth, node_index))
        })
        .collect()
}

struct ScrollerThatCanHaveSpace {
    depth: usize,
    node: SpatialNodeIndex,
    can_be_wheel_scrolled: bool,
}

fn scrollers_to_give_spaces_parents_first(
    tree: &VisualContextTree,
    mut candidates: Vec<ScrollerThatCanHaveSpace>,
    maximum_count: usize,
) -> Vec<SpatialNodeIndex> {
    let mut is_candidate = vec![false; tree.spatial_nodes.len()];
    for candidate in &candidates {
        is_candidate[candidate.node.0 as usize] = true;
    }
    candidates.sort_by_key(|candidate| (!candidate.can_be_wheel_scrolled, candidate.depth, candidate.node.0));
    let mut is_chosen = vec![false; tree.spatial_nodes.len()];
    let mut chosen_depths_and_nodes: Vec<(usize, SpatialNodeIndex)> = Vec::new();
    let mut candidate_and_unchosen_candidate_ancestors = Vec::new();
    for candidate in &candidates {
        if chosen_depths_and_nodes.len() == maximum_count {
            break;
        }
        if is_chosen[candidate.node.0 as usize] {
            continue;
        }
        candidate_and_unchosen_candidate_ancestors.clear();
        let mut node = candidate.node;
        let mut depth = candidate.depth;
        while node != VISUAL_VIEWPORT_NODE_INDEX {
            let index = node.0 as usize;
            if is_candidate[index] && !is_chosen[index] {
                candidate_and_unchosen_candidate_ancestors.push((depth, node));
            }
            depth -= 1;
            node = tree.spatial_nodes[index].parent;
        }
        if chosen_depths_and_nodes.len() + candidate_and_unchosen_candidate_ancestors.len() > maximum_count {
            continue;
        }
        for &(_, node) in &candidate_and_unchosen_candidate_ancestors {
            is_chosen[node.0 as usize] = true;
        }
        chosen_depths_and_nodes.extend_from_slice(&candidate_and_unchosen_candidate_ancestors);
    }
    chosen_depths_and_nodes.sort_by_key(|&(depth, node)| (depth, node.0));
    chosen_depths_and_nodes.into_iter().map(|(_, node)| node).collect()
}

/// The nodes that layers can be rastered at, parents first, which the tree alone decides. First the scroll nodes the
/// compositor scrolls, over a scroll range, whose content lands on whole device pixels of the viewport wherever they
/// are scrolled to, since nothing between them and the viewport transforms, or can be animated to. Those the user can
/// scroll with a wheel go ahead of the rest, with the scrollers they are nested in. Then the sticky boxes within those
/// scrollers that can be held at a sticky edge, held or not, so that a box that starts or stops being held moves its
/// space rather than changing the spaces. Each kind goes shallowest first and then in tree order up to as many as it
/// may have.
pub fn raster_space_anchors(
    tree: &VisualContextTree,
    spatial_nodes_that_move_on_their_own: &[bool],
) -> Vec<SpatialNodeIndex> {
    if !visual_viewport_only_translates(tree) {
        return Vec::new();
    }
    let candidate_scrollers: Vec<ScrollerThatCanHaveSpace> = tree
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
            Some(ScrollerThatCanHaveSpace {
                depth: depth_below_viewport_if_on_whole_device_pixels(
                    tree,
                    node,
                    spatial_nodes_that_move_on_their_own,
                )?,
                node,
                can_be_wheel_scrolled: record.can_be_wheel_scrolled_horizontally
                    || record.can_be_wheel_scrolled_vertically,
            })
        })
        .collect();
    let mut anchors = scrollers_to_give_spaces_parents_first(tree, candidate_scrollers, MAXIMUM_SCROLLER_SPACE_COUNT);
    let holdable_sticky_boxes =
        sticky_boxes_that_can_be_held_with_depths(tree, spatial_nodes_that_move_on_their_own, &anchors);
    anchors.extend(shallowest_first_up_to(
        holdable_sticky_boxes,
        MAXIMUM_STICKY_BOX_SPACE_COUNT,
    ));
    anchors.extend(animated_transform_anchors(tree));
    anchors
}

fn compositor_scroll_record_of(
    tree: &VisualContextTree,
    node: SpatialNodeIndex,
) -> Option<&CompositorScrollNodeRecord> {
    tree.compositor_scroll_table()
        .nodes
        .iter()
        .find(|record| record.scroll_node_index == node)
}

const SCROLLER_ANCHOR_IDENTITY_SEED: u64 = 0x7261_7374_6572_5f73;
const ANIMATED_TRANSFORM_ANCHOR_IDENTITY_SEED: u64 = 0x616e_696d_6174_6564;

// A node index names the node for as long as the tree keeps it, which outlives lists and animation samples.
fn identity_of_animated_transform_anchor(anchor: SpatialNodeIndex, raster_scale: f32) -> u64 {
    let mut hasher = foldhash::fast::FixedState::with_seed(ANIMATED_TRANSFORM_ANCHOR_IDENTITY_SEED).build_hasher();
    hasher.write_u32(anchor.0);
    hasher.write_u32(raster_scale.to_bits());
    hasher.finish()
}
const STICKY_BOX_ANCHOR_IDENTITY_SEED: u64 = 0x7374_6963_6b79_5f62;

fn identity_of_scroller_anchor(record: &CompositorScrollNodeRecord) -> u64 {
    let mut hasher = foldhash::fast::FixedState::with_seed(SCROLLER_ANCHOR_IDENTITY_SEED).build_hasher();
    hasher.write_i64(record.scrollable_node_id.0);
    hasher.write_u8(record.scroll_node_kind as u8);
    hasher.write_u8(record.pseudo_element_type);
    let page_leaves_node_unnamed = record.scrollable_node_id.0 == 0;
    if page_leaves_node_unnamed {
        hasher.write_u32(record.scroll_node_index.0);
    }
    hasher.finish() | 1
}

fn identity_of_sticky_box_anchor(sticky: &StickyData, node: SpatialNodeIndex) -> u64 {
    let mut hasher = foldhash::fast::FixedState::with_seed(STICKY_BOX_ANCHOR_IDENTITY_SEED).build_hasher();
    hasher.write_i64(sticky.box_node_id.0);
    hasher.write_u8(sticky.box_pseudo_element_type);
    let page_leaves_box_unnamed = sticky.box_node_id.0 == 0;
    if page_leaves_box_unnamed {
        hasher.write_u32(node.0);
    }
    hasher.finish() | 1
}

pub(crate) fn enclosing_int_rect_within_extent_limit(rect: FloatRect) -> IntRect {
    enclosing_int_rect(rect.intersected(extent_limit_rect()))
}

fn extent_of_space(
    tree: &VisualContextTree,
    spaces: &[RasterSpace],
    space: RasterSpaceIndex,
    world_with_anchors_of_space_at_origin: &WorldTransforms,
    viewport_rect: IntRect,
) -> IntRect {
    let world = world_with_anchors_of_space_at_origin;
    let unbounded = enclosing_int_rect_within_extent_limit(extent_limit_rect());
    let space = &spaces[space.0];
    if let RasterSpaceAnchorKind::AnimatedTransform { .. } = space.anchor_kind {
        return unbounded;
    }
    if let RasterSpaceAnchorKind::StickyBox { .. } = space.anchor_kind {
        let extent_of_parent = if space.parent_index == RasterSpaceIndex::VIEWPORT {
            viewport_rect
        } else {
            extent_of_space(tree, spaces, space.parent_index, world, viewport_rect)
        };
        let negated = |point: FloatPoint| FloatPoint {
            x: -point.x,
            y: -point.y,
        };
        let extent_of_parent_translated_back_by_every_box_offset = rect_swept_over_translations(
            extent_of_parent,
            negated(space.highest_anchor_translation),
            negated(space.lowest_anchor_translation),
        );
        return extent_of_parent_translated_back_by_every_box_offset;
    }
    let Some(record) = compositor_scroll_record_of(tree, space.anchor) else {
        return unbounded;
    };
    let Some(scrollport_offset) =
        translation_if_only_translates(&world.draw_matrix(tree.spatial_nodes[space.anchor.0 as usize].parent))
    else {
        return unbounded;
    };
    let scrollport = record
        .scrollport_rect
        .to_float()
        .translated(scrollport_offset.x, scrollport_offset.y);
    let (minimum, maximum) = (record.min_scroll_offset, record.max_scroll_offset);
    let extent = FloatRect::new(
        scrollport.x + minimum.x.min(0.0),
        scrollport.y + minimum.y.min(0.0),
        scrollport.width + (maximum.x.max(0.0) - minimum.x.min(0.0)),
        scrollport.height + (maximum.y.max(0.0) - minimum.y.min(0.0)),
    );
    if !is_finite_rect(extent) {
        return unbounded;
    }
    enclosing_int_rect_within_extent_limit(extent)
}

/// The raster spaces of a tree at a set of anchors, and the deepest of them that each spatial node moves with.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterSpaces {
    spaces: Vec<RasterSpace>,
    deepest_space_of_spatial_node: Vec<RasterSpaceIndex>,
    spatial_node_moves_on_its_own: Vec<bool>,
    effect_node_spread_moves_on_its_own: Vec<bool>,
}

impl RasterSpaces {
    /// The spaces at `anchors`, which `raster_space_anchors` gave or a prefix of them, with the extent of each but the
    /// viewport's left to be filled in from its geometry.
    pub fn at_anchors(tree: &VisualContextTree, anchors: &[SpatialNodeIndex], viewport_rect: IntRect) -> Self {
        Self::of_spaces(tree, Self::spaces_at_anchors(tree, anchors, viewport_rect))
    }

    /// The spaces at `anchors`, without what `of_spaces` works out of every node of the tree for them.
    pub fn spaces_at_anchors(
        tree: &VisualContextTree,
        anchors: &[SpatialNodeIndex],
        viewport_rect: IntRect,
    ) -> Vec<RasterSpace> {
        let mut spaces = vec![RasterSpace::of_viewport(viewport_rect)];
        for &anchor in anchors {
            let mut parent = RasterSpaceIndex::VIEWPORT;
            let mut ancestor = tree.spatial_nodes[anchor.0 as usize].parent;
            while ancestor != VISUAL_VIEWPORT_NODE_INDEX {
                if let Some(space) = spaces.iter().position(|space| space.anchor == ancestor) {
                    parent = RasterSpaceIndex(space);
                    break;
                }
                ancestor = tree.spatial_nodes[ancestor.0 as usize].parent;
            }
            if let SpatialData::Transform(_) = &tree.spatial_nodes[anchor.0 as usize].data {
                let raster_scale = raster_scale_of(tree, anchor);
                spaces.push(RasterSpace {
                    anchor,
                    identity: identity_of_animated_transform_anchor(anchor, raster_scale),
                    parent_index: parent,
                    anchor_kind: RasterSpaceAnchorKind::AnimatedTransform { raster_scale },
                    lowest_anchor_translation: FloatPoint::default(),
                    highest_anchor_translation: FloatPoint::default(),
                    extent: IntRect::default(),
                });
                continue;
            }
            if let SpatialData::Sticky(sticky) = &tree.spatial_nodes[anchor.0 as usize].data {
                let (lowest_anchor_translation, highest_anchor_translation) =
                    sticky.offset_range_without_sticky_ancestors();
                spaces.push(RasterSpace {
                    anchor,
                    identity: identity_of_sticky_box_anchor(sticky, anchor),
                    parent_index: parent,
                    anchor_kind: RasterSpaceAnchorKind::StickyBox {
                        border_box_origin: sticky.border_box_origin,
                    },
                    lowest_anchor_translation,
                    highest_anchor_translation,
                    extent: IntRect::default(),
                });
                continue;
            }
            let Some(record) = compositor_scroll_record_of(tree, anchor) else {
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
                identity: identity_of_scroller_anchor(record),
                parent_index: parent,
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
        spaces
    }

    pub fn with_other_extents(mut self, spaces_at_same_anchors: Vec<RasterSpace>) -> Self {
        debug_assert!(
            self.spaces
                .iter()
                .map(|space| space.anchor)
                .eq(spaces_at_same_anchors.iter().map(|space| space.anchor))
        );
        self.spaces = spaces_at_same_anchors;
        self
    }

    /// The given spaces, which a tree offers, and the deepest of them each spatial node of the tree moves with.
    pub fn of_spaces(tree: &VisualContextTree, spaces: Vec<RasterSpace>) -> Self {
        let mut deepest_space_of_spatial_node = vec![RasterSpaceIndex::VIEWPORT; tree.spatial_nodes.len()];
        tree.visit_spatial_nodes_parents_first(|index| {
            if index == VISUAL_VIEWPORT_NODE_INDEX.0 as usize {
                return;
            }
            let node = &tree.spatial_nodes[index];
            deepest_space_of_spatial_node[index] = if let Some(index_after_viewport) =
                spaces.iter().skip(1).position(|space| space.anchor.0 as usize == index)
            {
                RasterSpaceIndex(index_after_viewport + 1)
            } else if matches!(node.data, SpatialData::AnchorScrollShift(_)) {
                RasterSpaceIndex::VIEWPORT
            } else {
                deepest_space_of_spatial_node[node.parent.0 as usize]
            };
        });
        let anchors: Vec<SpatialNodeIndex> = spaces.iter().skip(1).map(|space| space.anchor).collect();
        Self {
            spaces,
            deepest_space_of_spatial_node,
            spatial_node_moves_on_its_own: spatial_nodes_that_move_on_their_own_relative_to_spaces_at(
                tree,
                Some(&anchors),
            ),
            effect_node_spread_moves_on_its_own: effects_whose_spread_moves_on_its_own(tree),
        }
    }

    pub fn spatial_moves_on_its_own(&self, spatial: SpatialNodeIndex) -> bool {
        self.spatial_node_moves_on_its_own
            .get(spatial.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    pub fn effect_spread_moves_on_its_own(&self, effect: EffectNodeIndex) -> bool {
        !effect.is_none()
            && self
                .effect_node_spread_moves_on_its_own
                .get(effect.0 as usize)
                .copied()
                .unwrap_or(false)
    }

    pub fn spaces(&self) -> &[RasterSpace] {
        &self.spaces
    }

    pub fn len(&self) -> usize {
        self.spaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spaces.is_empty()
    }

    pub fn deepest_space_of(&self, spatial: SpatialNodeIndex) -> RasterSpaceIndex {
        self.deepest_space_of_spatial_node
            .get(spatial.0 as usize)
            .copied()
            .unwrap_or(RasterSpaceIndex::VIEWPORT)
    }

    pub fn moves_with(&self, spatial: SpatialNodeIndex, space: RasterSpaceIndex) -> bool {
        space_is_ancestor_or_self(&self.spaces, space, self.deepest_space_of(spatial))
    }

    /// Per clip node, whether rasters of `space` leave it to be applied as they are composited, since the clip does
    /// not move with the space.
    pub fn clips_left_to_composite(&self, space: RasterSpaceIndex, tree: &VisualContextTree) -> Vec<bool> {
        tree.clip_nodes
            .iter()
            .map(|node| {
                self.deepest_space_of_spatial_node
                    .get(node.spatial.0 as usize)
                    .is_none_or(|&deepest| !space_is_ancestor_or_self(&self.spaces, space, deepest))
            })
            .collect()
    }
}

fn bounds_of_projected_rect_if_corners_in_front_of_eye(matrix: FloatMatrix4x4, rect: FloatRect) -> Option<FloatRect> {
    const SMALLEST_W_IN_FRONT_OF_EYE: f32 = 0.00001;
    let corners = [
        (rect.x, rect.y),
        (rect.right(), rect.y),
        (rect.x, rect.bottom()),
        (rect.right(), rect.bottom()),
    ]
    .map(|(x, y)| matrix.map_vector4([x, y, 0.0, 1.0]));
    if corners.iter().any(|corner| corner[3] <= SMALLEST_W_IN_FRONT_OF_EYE) {
        return None;
    }
    let xs = corners.map(|corner| corner[0] / corner[3]);
    let ys = corners.map(|corner| corner[1] / corner[3]);
    let (left, right) = (
        xs.into_iter().fold(f32::INFINITY, f32::min),
        xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
    );
    let (top, bottom) = (
        ys.into_iter().fold(f32::INFINITY, f32::min),
        ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
    );
    Some(FloatRect::new(left, top, right - left, bottom - top))
}

pub(crate) fn is_finite_rect(rect: FloatRect) -> bool {
    rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()
}

/// What the filters and clips of a list drawn under a tree do to where its content can show.
#[derive(Clone, Copy)]
pub struct FiltersAndClipsOfList<'a> {
    pub tree: &'a VisualContextTree,
    /// What `nearest_spreading_effects` gives for the tree, or where `follows_filters_of_every_sample` holds, what
    /// `nearest_effects_spreading_at_some_sample` gives.
    pub nearest_spreading_effects: &'a [EffectNodeIndex],
    pub effect_clips: &'a EffectClipPlan,
    /// Whether an effect counts as filtered where an animation of its filter gives it a filter at some sample, as
    /// placing follows, rather than only where the tree holds a filter, as keying the tiles of a frame does.
    pub follows_filters_of_every_sample: bool,
}

fn antialiased_reach_in_device_pixels(bounds: Option<FloatRect>) -> Reach<IntRect> {
    let Some(bounds) = bounds.filter(|bounds| is_finite_rect(*bounds)) else {
        return Reach::Anywhere;
    };
    let bounds = bounds.intersected(extent_limit_rect());
    if bounds.is_empty() {
        return Reach::Nothing;
    }
    let margin = ANTIALIASING_MARGIN_IN_DEVICE_PIXELS;
    Reach::Within(enclosing_int_rect(bounds).inflated_edges(margin, margin, margin, margin))
}

/// Where the content of one raster space lands in the space's raster coordinates under the scroll offsets of a frame:
/// the transforms of the tree's spatial nodes with the anchors of the space's chain at their origin, what the tree
/// culls, and the bounds of its clip chains.
pub struct RasterSpaceGeometry {
    pub raster_scroll_offsets: Vec<FloatPoint>,
    pub world: WorldTransforms,
    pub culling: TreeCullingScratch,
    /// Empty for the viewport's space, which applies every clip.
    pub clips_left_to_composite: Vec<bool>,
    rastered_affine_clip_chain_bounds_by_clip: Vec<Option<FloatRect>>,
    rastered_affine_own_clip_bounds_by_clip: Vec<Option<FloatRect>>,
    clip_chain_bounds_fixed_in_space_by_clip: Vec<Option<FloatRect>>,
    pub extent: IntRect,
}

impl RasterSpaceGeometry {
    /// `viewport_rect` is in the raster's device pixels, which `viewport_to_raster_transform` maps the viewport's into.
    pub fn new(
        tree: &VisualContextTree,
        spaces: &RasterSpaces,
        space: RasterSpaceIndex,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
        viewport_to_raster_transform: FloatMatrix4x4,
    ) -> Self {
        let raster_scroll_offsets = raster_scroll_offsets(spaces.spaces(), space, scroll_offsets);
        let world = WorldTransforms::with_raster_root(
            tree,
            &raster_scroll_offsets,
            Some(viewport_to_raster_transform),
            spaces.spaces()[space.0].raster_root(),
        );
        let mut culling = TreeCullingScratch::default();
        tree.fill_culling_scratch(&mut culling);
        let clips_left_to_composite = if space == RasterSpaceIndex::VIEWPORT {
            Vec::new()
        } else {
            spaces.clips_left_to_composite(space, tree)
        };
        let mut rastered_affine_clip_chain_bounds_by_clip: Vec<Option<FloatRect>> = vec![None; tree.clip_nodes.len()];
        let mut rastered_affine_own_clip_bounds_by_clip: Vec<Option<FloatRect>> = vec![None; tree.clip_nodes.len()];
        let mut clip_chain_bounds_fixed_in_space_by_clip: Vec<Option<FloatRect>> = vec![None; tree.clip_nodes.len()];
        tree.visit_clip_nodes_parents_first(|index| {
            let node = &tree.clip_nodes[index];
            let own_clip_rect = match &node.data {
                ClipNodeData::Rect(data) if data.mode == ClipMode::Intersect => Some(data.rect),
                ClipNodeData::Path(path) => Some(path.bounding_rect.to_float()),
                _ => None,
            };
            let bounds_of_parent = |bounds_by_clip: &[Option<FloatRect>]| {
                if node.parent.is_none() {
                    None
                } else {
                    bounds_by_clip[node.parent.0 as usize]
                }
            };
            let left_to_composite = clips_left_to_composite.get(index).copied().unwrap_or(false);
            let own_bounds_if_rastered_affinely = own_clip_rect
                .filter(|_| !left_to_composite && world.draw_matrix(node.spatial).is_2d_affine())
                .map(|rect| world.map_rect_to_root(node.spatial, rect))
                .filter(|rect| is_finite_rect(*rect));
            rastered_affine_own_clip_bounds_by_clip[index] = own_bounds_if_rastered_affinely;
            rastered_affine_clip_chain_bounds_by_clip[index] = intersect_bounds(
                own_bounds_if_rastered_affinely,
                bounds_of_parent(&rastered_affine_clip_chain_bounds_by_clip),
            );
            let own_bounds_if_fixed_in_space = own_bounds_if_rastered_affinely.filter(|_| {
                spaces.deepest_space_of(node.spatial) == space && !spaces.spatial_moves_on_its_own(node.spatial)
            });
            clip_chain_bounds_fixed_in_space_by_clip[index] = intersect_bounds(
                own_bounds_if_fixed_in_space,
                bounds_of_parent(&clip_chain_bounds_fixed_in_space_by_clip),
            );
        });
        let extent = if space == RasterSpaceIndex::VIEWPORT {
            viewport_rect
        } else {
            extent_of_space(tree, spaces.spaces(), space, &world, viewport_rect)
        };
        Self {
            raster_scroll_offsets,
            world,
            culling,
            clips_left_to_composite,
            rastered_affine_clip_chain_bounds_by_clip,
            rastered_affine_own_clip_bounds_by_clip,
            clip_chain_bounds_fixed_in_space_by_clip,
            extent,
        }
    }

    pub fn clip_is_left_to_composite(&self, clip: ClipNodeIndex) -> bool {
        self.clips_left_to_composite
            .get(clip.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Where content of `context` can show in the space's raster coordinates, antialiased: drawn within `drawn_within`
    /// as the frame places it, spread by the filters it is drawn through, within the clips that rasters of the space
    /// apply; or, without `drawn_within`, anywhere it can move, within the clips fixed in the space, which are some of
    /// those. Placing a list asks this of the ink of its runs, and keying tiles of the records in them, so what a
    /// record reaches in a frame is within what its run reaches, as far as the space's extent shows either.
    pub fn reach_of_content(
        &self,
        list: &FiltersAndClipsOfList<'_>,
        drawn_within: Option<FloatRect>,
        context: ContextRef,
    ) -> Reach<IntRect> {
        let is_spread_by_filters =
            !context.effect.is_none() && !list.nearest_spreading_effects[context.effect.0 as usize].is_none();
        let clip_bounding_filter_spread = if is_spread_by_filters {
            list.effect_clips
                .clip_outside_every_filter(list.tree, context, list.follows_filters_of_every_sample)
        } else {
            context.clip
        };
        let chain_bounds_of_clip_bounding_filter_spread = |bounds_by_clip: &[Option<FloatRect>]| {
            if clip_bounding_filter_spread.is_none() {
                None
            } else {
                bounds_by_clip[clip_bounding_filter_spread.0 as usize]
            }
        };
        let Some(drawn_within) = drawn_within else {
            return antialiased_reach_in_device_pixels(chain_bounds_of_clip_bounding_filter_spread(
                &self.clip_chain_bounds_fixed_in_space_by_clip,
            ));
        };
        if drawn_within.is_empty()
            || self.culling.context_culls_everything(context)
            || self.world.backface_culled[context.spatial.0 as usize]
        {
            return Reach::Nothing;
        }
        let (space_of_extent, extent) = changed_extent_through_filters(
            list.tree,
            list.nearest_spreading_effects,
            context.spatial,
            drawn_within,
            context.effect,
            &self.world,
        );
        let Some(bounds) = self.bounds_where_extent_lands(space_of_extent, extent, context.spatial) else {
            return Reach::Nothing;
        };
        antialiased_reach_in_device_pixels(intersect_bounds(
            bounds,
            chain_bounds_of_clip_bounding_filter_spread(&self.rastered_affine_clip_chain_bounds_by_clip),
        ))
    }

    /// As `reach_of_content` with `drawn_within`, for content a raster draws inside `composited_effect`, which it
    /// leaves out with the effects around it and the clips the effect is entered within: through the filters inside the
    /// effect, within the clips inside it, and within as far past the clips it is entered within as the filters left
    /// out read, by `read_reach_of_filters_left_out` where that is known.
    pub fn reach_of_content_inside_composited_effect(
        &self,
        list: &FiltersAndClipsOfList<'_>,
        drawn_within: FloatRect,
        context: ContextRef,
        composited_effect: EffectNodeIndex,
        read_reach_of_filters_left_out: Option<i32>,
    ) -> Reach<IntRect> {
        if drawn_within.is_empty()
            || self.culling.context_culls_everything(context)
            || self.world.backface_culled[context.spatial.0 as usize]
        {
            return Reach::Nothing;
        }
        let tree = list.tree;
        let (space_of_extent, extent) = changed_extent_through_filters_inside(
            tree,
            context.spatial,
            drawn_within,
            context.effect,
            composited_effect,
            &self.world,
        );
        let Some(bounds) = self.bounds_where_extent_lands(space_of_extent, extent, context.spatial) else {
            return Reach::Nothing;
        };
        let mut clip_bounding_filter_spread = context.clip;
        let mut effect = context.effect;
        let mut is_spread_by_filters = false;
        while !effect.is_none() && effect != composited_effect {
            let node = &tree.effect_nodes[effect.0 as usize];
            if let EffectNodeData::Effects(effects) = &node.data
                && (effects.filter.is_some() || effects.backdrop_filter.is_some())
            {
                is_spread_by_filters |= spreading_filter_of(node).is_some();
                if is_spread_by_filters {
                    clip_bounding_filter_spread = list.effect_clips.output_clip(effect);
                }
            }
            effect = node.parent;
        }
        let clip_composited_effect_is_entered_within = list
            .effect_clips
            .output_clip_if_participating(composited_effect)
            .unwrap_or(ClipNodeIndex::NONE);
        let mut bounds_of_clips_inside_composited_effect = None;
        let mut clip = clip_bounding_filter_spread;
        while !clip.is_none() && clip != clip_composited_effect_is_entered_within {
            bounds_of_clips_inside_composited_effect = intersect_bounds(
                self.rastered_affine_own_clip_bounds_by_clip[clip.0 as usize],
                bounds_of_clips_inside_composited_effect,
            );
            clip = tree.clip_nodes[clip.0 as usize].parent;
        }
        let clips_entered_within_widened_by_what_filters_left_out_read =
            read_reach_of_filters_left_out.and_then(|read_reach| {
                let bounds = if clip_composited_effect_is_entered_within.is_none() {
                    None
                } else {
                    self.rastered_affine_clip_chain_bounds_by_clip[clip_composited_effect_is_entered_within.0 as usize]
                }?;
                let twice_read_reach = 2.0 * read_reach as f32;
                Some(bounds.inflated(twice_read_reach, twice_read_reach))
            });
        antialiased_reach_in_device_pixels(intersect_bounds(
            intersect_bounds(bounds, bounds_of_clips_inside_composited_effect),
            clips_entered_within_widened_by_what_filters_left_out_read,
        ))
    }

    // None where the extent lands nowhere, Some(None) where nothing bounds where it lands.
    fn bounds_where_extent_lands(
        &self,
        space_of_extent: SpatialNodeIndex,
        extent: Extent,
        context_spatial: SpatialNodeIndex,
    ) -> Option<Option<FloatRect>> {
        let maps_affinely = self.world.draw_matrix(space_of_extent).is_2d_affine()
            && self.world.draw_matrix(context_spatial).is_2d_affine();
        Some(match extent {
            Extent::Nothing => return None,
            Extent::Within(rect) if maps_affinely => {
                Some(self.world.map_rect_to_root(space_of_extent, rect)).filter(|rect| is_finite_rect(*rect))
            }
            Extent::Within(rect) if space_of_extent == context_spatial => {
                bounds_of_projected_rect_if_corners_in_front_of_eye(self.world.draw_matrix(space_of_extent), rect)
                    .filter(|rect| is_finite_rect(*rect))
            }
            _ => None,
        })
    }

    pub fn part_of_extent_reached_by(&self, reach: Reach<IntRect>) -> IntRect {
        reach.part_of(self.extent)
    }
}

pub(crate) fn intersect_bounds(bounds: Option<FloatRect>, limit: Option<FloatRect>) -> Option<FloatRect> {
    match (bounds, limit) {
        (Some(bounds), Some(limit)) => Some(bounds.intersected(limit)),
        (bounds, limit) => bounds.or(limit),
    }
}

type CompositeClipsOfContextInSpace = Option<Rc<[CompositeClip]>>;

/// Where content is placed among the spaces at a set of anchors: the spaces, the clips that can be left to apply when
/// rasters are composited, and the effects whose content reads its backdrop. Two tables that compare equal place every
/// visual context alike, whatever tree copies they were worked out from. Where each context is placed is worked out as
/// it is first asked for.
pub struct PlacementTables {
    spaces: RasterSpaces,
    /// Per spatial node, whether a sample of the compositor's animations can move it within the deepest space it is
    /// in, as it is under a node whose transform the compositor animates that anchors no space. Empty where the tree
    /// has no transform animations.
    spatial_node_moves_with_animation_samples: Vec<bool>,
    composite_form_of_clips: Vec<Option<CompositeClip>>,
    placement_constraint_of_effects: Vec<EffectPlacementConstraint>,
    deepest_space_and_backdrop_filter_read_rect_by_effect: Vec<Option<(RasterSpaceIndex, IntRect)>>,
    animated_space_holding_every_run_of_effect: Vec<Option<RasterSpaceIndex>>,
    refused_animated_spaces: RefCell<Vec<bool>>,
    cached_composite_clips_of_contexts:
        RefCell<HashMap<(ContextRef, RasterSpaceIndex), CompositeClipsOfContextInSpace>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectPlacementConstraint {
    ConstrainsNothing,
    MustMoveWithSpace,
    MustMoveWithSpaceAndKeepBackdropReadUncut,
}

impl PartialEq for PlacementTables {
    fn eq(&self, other: &Self) -> bool {
        self.spaces == other.spaces
            && self.composite_form_of_clips == other.composite_form_of_clips
            && self.placement_constraint_of_effects == other.placement_constraint_of_effects
            && self.deepest_space_and_backdrop_filter_read_rect_by_effect
                == other.deepest_space_and_backdrop_filter_read_rect_by_effect
    }
}

/// The spaces at `anchors` and the geometry of each, in the device pixels of the viewport.
pub fn spaces_with_geometries(
    tree: &VisualContextTree,
    anchors: &[SpatialNodeIndex],
    scroll_offsets: &[FloatPoint],
    viewport_rect: IntRect,
) -> (RasterSpaces, Vec<Rc<RasterSpaceGeometry>>) {
    let spaces = RasterSpaces::at_anchors(tree, anchors, viewport_rect);
    let geometries: Vec<Rc<RasterSpaceGeometry>> = (0..spaces.len())
        .map(|space| {
            Rc::new(RasterSpaceGeometry::new(
                tree,
                &spaces,
                RasterSpaceIndex(space),
                scroll_offsets,
                viewport_rect,
                FloatMatrix4x4::identity(),
            ))
        })
        .collect();
    (spaces, geometries)
}

impl PlacementTables {
    /// Also fills in the extent of each of `spaces` from its geometry in `geometries`.
    pub fn new(tree: &VisualContextTree, spaces: RasterSpaces, geometries: &[Rc<RasterSpaceGeometry>]) -> Self {
        let spaces_len = spaces.len();
        let RasterSpaces {
            spaces: mut spaces_with_extents,
            deepest_space_of_spatial_node,
            spatial_node_moves_on_its_own,
            effect_node_spread_moves_on_its_own,
        } = spaces;
        for (space, geometry) in spaces_with_extents.iter_mut().zip(geometries) {
            space.extent = geometry.extent;
        }
        let spaces = RasterSpaces {
            spaces: spaces_with_extents,
            deepest_space_of_spatial_node,
            spatial_node_moves_on_its_own,
            effect_node_spread_moves_on_its_own,
        };
        let spatial_node_moves_with_animation_samples =
            spatial_nodes_that_animation_samples_move_within_their_space(tree, spaces.spaces());
        let moves_with_animation_samples = |spatial: SpatialNodeIndex| {
            spatial_node_moves_with_animation_samples
                .get(spatial.0 as usize)
                .copied()
                .unwrap_or(false)
        };
        let composite_form_of_clips = tree
            .clip_nodes
            .iter()
            .map(|node| {
                let ClipNodeData::Rect(data) = &node.data else {
                    return None;
                };
                if data.mode != ClipMode::Intersect || moves_with_animation_samples(node.spatial) {
                    return None;
                }
                let clip_space = spaces.deepest_space_of(node.spatial);
                let world = &geometries[clip_space.0].world;
                translation_if_only_translates(&world.draw_matrix(node.spatial))?;
                let rect = world.map_rect_to_root(node.spatial, data.rect);
                // A clip of one axis alone clips as much at the extent limit as at its far edges on the other axis.
                let [left, top, right, bottom] = [rect.x, rect.y, rect.right(), rect.bottom()]
                    .map(|edge| edge.clamp(-EXTENT_LIMIT_IN_DEVICE_PIXELS, EXTENT_LIMIT_IN_DEVICE_PIXELS));
                let edges_are_on_whole_device_pixels = [left, top, right, bottom]
                    .iter()
                    .all(|edge| (edge - edge.round()).abs() <= COMPOSITE_CLIP_EDGE_TOLERANCE_IN_DEVICE_PIXELS);
                if !edges_are_on_whole_device_pixels {
                    return None;
                }
                Some(CompositeClip {
                    raster_space_index: clip_space,
                    rect: IntRect::new(
                        left.round() as i32,
                        top.round() as i32,
                        right.round() as i32 - left.round() as i32,
                        bottom.round() as i32 - top.round() as i32,
                    ),
                    corner_radii: data.corner_radii,
                })
            })
            .collect();
        let placement_constraint_of_effects = tree
            .effect_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                if tree.root_isolation_effect == Some(EffectNodeIndex(index as u32))
                    || matches!(node.data, EffectNodeData::BackgroundColorAnimation)
                {
                    return EffectPlacementConstraint::ConstrainsNothing;
                }
                let reads_backdrop = match &node.data {
                    EffectNodeData::Effects(effects) => effects.backdrop_filter.is_some(),
                    EffectNodeData::Mask(mask) => mask.masks_backdrop,
                    _ => false,
                };
                if reads_backdrop {
                    EffectPlacementConstraint::MustMoveWithSpaceAndKeepBackdropReadUncut
                } else {
                    EffectPlacementConstraint::MustMoveWithSpace
                }
            })
            .collect();
        let deepest_space_and_backdrop_filter_read_rect_by_effect = tree
            .effect_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                if moves_with_animation_samples(node.spatial) {
                    return None;
                }
                let space = spaces.deepest_space_of(node.spatial);
                match super::layers::backdrop_filter_read_reach(
                    tree,
                    &geometries[space.0],
                    EffectNodeIndex(index as u32),
                ) {
                    Reach::Within(read_rect) => Some((space, read_rect)),
                    _ => None,
                }
            })
            .collect();
        Self {
            spaces,
            spatial_node_moves_with_animation_samples,
            composite_form_of_clips,
            placement_constraint_of_effects,
            deepest_space_and_backdrop_filter_read_rect_by_effect,
            animated_space_holding_every_run_of_effect: Vec::new(),
            refused_animated_spaces: RefCell::new(vec![false; spaces_len]),
            cached_composite_clips_of_contexts: RefCell::new(HashMap::new()),
        }
    }

    /// Notes, per effect node, the space whose anchor the compositor animates that holds every run of the list under
    /// the effect, if one does: content of that space can be placed in it under such an effect although the effect
    /// does not move with the space, as the compositor composites the effect around the space's rasters.
    pub fn note_animated_spaces_holding_every_run_of_effects(
        &mut self,
        tree: &VisualContextTree,
        list: &DisplayListStorage,
    ) {
        #[derive(Clone, Copy, PartialEq)]
        enum SpacesOfRuns {
            NoRun,
            All(RasterSpaceIndex),
            Several,
        }
        let mut spaces_of_runs_by_effect = vec![SpacesOfRuns::NoRun; tree.effect_nodes.len()];
        for run_index in 0..list.run_count() {
            let context = list.run(run_index).context;
            let space_of_run = self.deepest_space_of(context.spatial);
            let mut effect = context.effect;
            while !effect.is_none() {
                let spaces_of_runs = &mut spaces_of_runs_by_effect[effect.0 as usize];
                *spaces_of_runs = match *spaces_of_runs {
                    SpacesOfRuns::NoRun => SpacesOfRuns::All(space_of_run),
                    SpacesOfRuns::All(space) if space == space_of_run => SpacesOfRuns::All(space),
                    _ => SpacesOfRuns::Several,
                };
                effect = tree.effect_nodes[effect.0 as usize].parent;
            }
        }
        let spaces = self.spaces.spaces();
        self.animated_space_holding_every_run_of_effect = spaces_of_runs_by_effect
            .into_iter()
            .map(|spaces_of_runs| match spaces_of_runs {
                SpacesOfRuns::All(space)
                    if matches!(
                        spaces[space.0].anchor_kind,
                        RasterSpaceAnchorKind::AnimatedTransform { .. }
                    ) =>
                {
                    Some(space)
                }
                _ => None,
            })
            .collect();
    }

    pub fn animated_space_holding_every_run_of_effect(&self, effect: EffectNodeIndex) -> Option<RasterSpaceIndex> {
        self.animated_space_holding_every_run_of_effect
            .get(effect.0 as usize)
            .copied()
            .flatten()
    }

    /// Keeps content out of a space whose anchor the compositor animates, which then goes into the space around it.
    /// Called before anything is placed in the space.
    pub fn refuse_animated_space(&self, space: RasterSpaceIndex) {
        self.refused_animated_spaces.borrow_mut()[space.0] = true;
    }

    pub fn spaces(&self) -> &[RasterSpace] {
        self.spaces.spaces()
    }

    pub fn raster_spaces(&self) -> &RasterSpaces {
        &self.spaces
    }

    pub fn deepest_space_of(&self, spatial: SpatialNodeIndex) -> RasterSpaceIndex {
        self.spaces.deepest_space_of(spatial)
    }

    pub fn moves_with(&self, spatial: SpatialNodeIndex, space: RasterSpaceIndex) -> bool {
        self.spaces.moves_with(spatial, space)
    }

    /// The clips that rasters of `context` in `space` leave to be applied as they are composited, in the order its
    /// clip chain names them, or none where the context cannot be rastered in the space: it does not move with the
    /// space, an effect of it does not, or a clip that does not cannot be applied as a rect of whole device pixels.
    pub fn composite_clips_in(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        space: RasterSpaceIndex,
    ) -> Option<Rc<[CompositeClip]>> {
        if let Some(cached) = self.cached_composite_clips_of_contexts.borrow().get(&(context, space)) {
            return cached.clone();
        }
        let clips = self.compute_composite_clips_in(tree, context, space).map(Rc::from);
        self.cached_composite_clips_of_contexts
            .borrow_mut()
            .insert((context, space), clips.clone());
        clips
    }

    fn compute_composite_clips_in(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        space: RasterSpaceIndex,
    ) -> Option<Vec<CompositeClip>> {
        if !self.moves_with(context.spatial, space) || self.refused_animated_spaces.borrow()[space.0] {
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
            if composite_clips.contains(&composite_clip) {
                continue;
            }
            let rounded_clip_shares_space_with_another = composite_clips.iter().any(|other: &CompositeClip| {
                other.raster_space_index == composite_clip.raster_space_index
                    && (other.is_rounded() || composite_clip.is_rounded())
            });
            if rounded_clip_shares_space_with_another {
                return None;
            }
            composite_clips.push(composite_clip);
        }
        let mut effect = context.effect;
        while !effect.is_none() {
            let node = &tree.effect_nodes[effect.0 as usize];
            match self.placement_constraint_of_effects[effect.0 as usize] {
                EffectPlacementConstraint::ConstrainsNothing => {}
                constraint => {
                    let composited_around_animated_space = constraint == EffectPlacementConstraint::MustMoveWithSpace
                        && self.animated_space_holding_every_run_of_effect(effect) == Some(space);
                    if !self.moves_with(node.spatial, space) && !composited_around_animated_space {
                        return None;
                    }
                    if constraint == EffectPlacementConstraint::MustMoveWithSpaceAndKeepBackdropReadUncut
                        && !self.composite_clips_cut_nothing_backdrop_filter_reads(effect, space, &composite_clips)
                    {
                        return None;
                    }
                }
            }
            effect = node.parent;
        }
        Some(composite_clips)
    }

    fn composite_clips_cut_nothing_backdrop_filter_reads(
        &self,
        effect: EffectNodeIndex,
        space: RasterSpaceIndex,
        composite_clips: &[CompositeClip],
    ) -> bool {
        if composite_clips.is_empty() {
            return true;
        }
        let Some((space_of_read_rect, read_rect)) =
            self.deepest_space_and_backdrop_filter_read_rect_by_effect[effect.0 as usize]
        else {
            return false;
        };
        if space_of_read_rect != space {
            return false;
        }
        let spaces = self.spaces.spaces();
        composite_clips.iter().all(|clip| {
            let mut read_rect_swept_over_anchor_translations = read_rect;
            let mut space_swept_through = space;
            while space_swept_through != clip.raster_space_index {
                if space_swept_through == RasterSpaceIndex::VIEWPORT {
                    return false;
                }
                read_rect_swept_over_anchor_translations = rect_swept_over_translations(
                    read_rect_swept_over_anchor_translations,
                    spaces[space_swept_through.0].lowest_anchor_translation,
                    spaces[space_swept_through.0].highest_anchor_translation,
                );
                space_swept_through = spaces[space_swept_through.0].parent_index;
            }
            clip.leaves_all_of(read_rect_swept_over_anchor_translations)
        })
    }

    /// Whether a sample of the compositor's animations can change how rasters of `space`, which `spatial` moves with,
    /// draw what is drawn in `spatial`, as a transform the compositor animates is between them.
    pub fn animation_samples_move_spatial_within_space(
        &self,
        spatial: SpatialNodeIndex,
        space: RasterSpaceIndex,
    ) -> bool {
        if self
            .spatial_node_moves_with_animation_samples
            .get(spatial.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            return true;
        }
        let spaces = self.spaces.spaces();
        let mut space_of_spatial = self.deepest_space_of(spatial);
        while space_of_spatial != space && space_of_spatial != RasterSpaceIndex::VIEWPORT {
            if matches!(
                spaces[space_of_spatial.0].anchor_kind,
                RasterSpaceAnchorKind::AnimatedTransform { .. }
            ) {
                return true;
            }
            space_of_spatial = spaces[space_of_spatial.0].parent_index;
        }
        false
    }

    /// Whether what is drawn in `spatial` moves within rasters of `space`: it is in a space inside it, or moves on its
    /// own.
    pub fn spatial_moves_within_space(&self, spatial: SpatialNodeIndex, space: RasterSpaceIndex) -> bool {
        let deepest = self.deepest_space_of(spatial);
        (deepest != space && space_is_ancestor_or_self(self.spaces.spaces(), space, deepest))
            || self.spaces.spatial_moves_on_its_own(spatial)
    }

    /// Whether content of `context` rastered in `space` moves within it as an anchor of a space inside it scrolls, a
    /// sticky box sticks or the compositor animates a transform or a filter, so that where it lands holds at the
    /// current values only.
    pub fn context_moves_within_space(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        space: RasterSpaceIndex,
    ) -> bool {
        let moves_within_space = |spatial: SpatialNodeIndex| self.spatial_moves_within_space(spatial, space);
        if self.spaces.effect_spread_moves_on_its_own(context.effect) {
            return true;
        }
        if moves_within_space(context.spatial) {
            return true;
        }
        let mut clip = context.clip;
        while !clip.is_none() {
            let node = &tree.clip_nodes[clip.0 as usize];
            if moves_within_space(node.spatial) {
                return true;
            }
            clip = node.parent;
        }
        let mut effect = context.effect;
        while !effect.is_none() {
            let node = &tree.effect_nodes[effect.0 as usize];
            if tree.root_isolation_effect != Some(effect) && moves_within_space(node.spatial) {
                return true;
            }
            effect = node.parent;
        }
        false
    }
}

/// The records of runs that a space whose anchor what they draw does not move with cannot raster: records with inline
/// clips in spaces of their own, and scroll bars, alone in their runs, whose thumbs follow a scroll offset, as the
/// chunks they are in know.
pub struct RunsReachingOutsideTheirContext {
    inline_clip_spaces_of_runs: HashMap<usize, Vec<SpatialNodeIndex>>,
    scroll_nodes_moving_scroll_bar_thumbs_of_runs: HashMap<usize, Vec<SpatialNodeIndex>>,
}

impl RunsReachingOutsideTheirContext {
    pub fn new(list: &DisplayListStorage) -> Self {
        let mut inline_clip_spaces_of_runs: HashMap<usize, Vec<SpatialNodeIndex>> = HashMap::new();
        let mut scroll_nodes_moving_scroll_bar_thumbs_of_runs: HashMap<usize, Vec<SpatialNodeIndex>> = HashMap::new();
        for (chunk_index, chunk) in list.chunks().iter().enumerate() {
            let first_run_index_of_chunk = list.first_run_of_chunk(ChunkIndexInList(chunk_index as u32));
            for record in chunk.records_reaching_outside_run_context() {
                let run_index = first_run_index_of_chunk + record.run_index as usize;
                match record.reach_outside_run_context {
                    ReachOutsideRunContext::InlineClipInSpaceOf(space) => {
                        inline_clip_spaces_of_runs.entry(run_index).or_default().push(space);
                    }
                    ReachOutsideRunContext::ScrollbarThumbFollowsScrollOffsetOf(scroll_node_index) => {
                        scroll_nodes_moving_scroll_bar_thumbs_of_runs
                            .entry(run_index)
                            .or_default()
                            .push(scroll_node_index);
                    }
                }
            }
        }
        Self {
            inline_clip_spaces_of_runs,
            scroll_nodes_moving_scroll_bar_thumbs_of_runs,
        }
    }

    /// The clips rasters of the run in `space` leave to be composited, or none where the run cannot be rastered there.
    pub fn composite_clips_of_run_in(
        &self,
        tree: &VisualContextTree,
        tables: &PlacementTables,
        run_index: usize,
        run: &DisplayListCommandRun,
        space: RasterSpaceIndex,
    ) -> Option<Rc<[CompositeClip]>> {
        let composite_clips = tables.composite_clips_in(tree, run.context, space)?;
        let has_inline_clip_not_moving_with_space =
            self.inline_clip_spaces_of_runs
                .get(&run_index)
                .is_some_and(|clip_spaces| {
                    clip_spaces.iter().any(|&clip_space| {
                        (clip_space.0 as usize) < tree.spatial_nodes.len() && !tables.moves_with(clip_space, space)
                    })
                });
        if has_inline_clip_not_moving_with_space {
            return None;
        }
        let has_scroll_bar_thumb_following_anchor_of_space = self
            .scroll_nodes_moving_scroll_bar_thumbs_of_runs
            .get(&run_index)
            .is_some_and(|scroll_nodes| {
                space_and_ancestors_below_viewport(tables.spaces(), space)
                    .any(|space_or_ancestor| scroll_nodes.contains(&tables.spaces()[space_or_ancestor.0].anchor))
            });
        if has_scroll_bar_thumb_following_anchor_of_space {
            return None;
        }
        Some(composite_clips)
    }

    /// Whether an inline clip of the run in a space of its own moves within rasters of `space`, which then read where
    /// the clip is.
    pub fn inline_clips_of_run_move_within_space(
        &self,
        tree: &VisualContextTree,
        tables: &PlacementTables,
        run_index: usize,
        space: RasterSpaceIndex,
    ) -> bool {
        self.inline_clip_spaces_of_runs
            .get(&run_index)
            .is_some_and(|clip_spaces| {
                clip_spaces.iter().any(|&clip_space| {
                    (clip_space.0 as usize) < tree.spatial_nodes.len()
                        && tables.spatial_moves_within_space(clip_space, space)
                })
            })
    }

    /// The deepest space the run can be rastered in.
    pub fn deepest_space_of_run(
        &self,
        tree: &VisualContextTree,
        tables: &PlacementTables,
        run_index: usize,
        run: &DisplayListCommandRun,
    ) -> RasterSpaceIndex {
        let mut space = tables.deepest_space_of(run.context.spatial);
        while space != RasterSpaceIndex::VIEWPORT
            && self
                .composite_clips_of_run_in(tree, tables, run_index, run, space)
                .is_none()
        {
            space = tables.spaces()[space.0].parent_index;
        }
        space
    }
}

/// The index of a run group among the `RunGroups` of a list, and in `ListPlacement::run_groups`, which places them in
/// the same order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunGroupIndex(pub usize);

/// Runs that are placed together, as they hold every run of the group effects and of the 3D sorting contexts that sort
/// their planes by depth among them, in paint order.
pub struct RunGroups {
    first_and_last_run_of_groups: Vec<(u32, u32)>,
    group_of_run: Vec<u32>,
    group_is_under_mask: Vec<bool>,
    roots_of_contexts_sorting_planes_by_depth: Vec<Option<SpatialNodeIndex>>,
    run_is_sorted_by_depth: Vec<bool>,
    group_is_sorted_by_depth: Vec<bool>,
}

// By the tree's structure rather than effect values, so that groups stay put while an opacity animates.
fn outermost_grouping_effects(tree: &VisualContextTree) -> Vec<EffectNodeIndex> {
    let mut outermost_grouping_effect_of_effect = vec![EffectNodeIndex::NONE; tree.effect_nodes.len()];
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let outermost_of_parent = if node.parent.is_none() {
            EffectNodeIndex::NONE
        } else {
            outermost_grouping_effect_of_effect[node.parent.0 as usize]
        };
        let is_grouping = node.data.pushes_layer() && tree.root_isolation_effect != Some(EffectNodeIndex(index as u32));
        outermost_grouping_effect_of_effect[index] = if !outermost_of_parent.is_none() {
            outermost_of_parent
        } else if is_grouping {
            EffectNodeIndex(index as u32)
        } else {
            EffectNodeIndex::NONE
        };
    });
    outermost_grouping_effect_of_effect
}

fn first_and_last_run_of_groups_holding_whole_grouping_effects_and_sorting_contexts(
    grouping_effect_of_run: &[EffectNodeIndex],
    sorting_context_of_run: &[Option<SpatialNodeIndex>],
) -> Vec<(u32, u32)> {
    let mut last_run_of_grouping_effect = HashMap::new();
    let mut last_run_of_sorting_context = HashMap::new();
    for (run_index, (grouping_effect, sorting_context)) in
        grouping_effect_of_run.iter().zip(sorting_context_of_run).enumerate()
    {
        if !grouping_effect.is_none() {
            last_run_of_grouping_effect.insert(grouping_effect.0, run_index);
        }
        if let Some(sorting_context) = sorting_context {
            last_run_of_sorting_context.insert(sorting_context.0, run_index);
        }
    }
    let mut first_and_last_run_of_groups = Vec::new();
    let mut run_index = 0;
    while run_index < grouping_effect_of_run.len() {
        let first = run_index;
        let mut last = run_index;
        let mut run_that_may_extend_group = run_index;
        while run_that_may_extend_group <= last {
            let grouping_effect = grouping_effect_of_run[run_that_may_extend_group];
            if !grouping_effect.is_none() {
                last = last.max(last_run_of_grouping_effect[&grouping_effect.0]);
            }
            if let Some(sorting_context) = sorting_context_of_run[run_that_may_extend_group] {
                last = last.max(last_run_of_sorting_context[&sorting_context.0]);
            }
            run_that_may_extend_group += 1;
        }
        first_and_last_run_of_groups.push((first as u32, last as u32));
        run_index = last + 1;
    }
    first_and_last_run_of_groups
}

impl RunGroups {
    /// The groups of the runs of a list, which the structure of its tree decides, with the 3D sorting contexts of the
    /// tree that sort their planes by depth, which the transforms of their planes decide.
    pub fn new<'a>(
        tree: &VisualContextTree,
        command_runs: impl IntoIterator<Item = &'a DisplayListCommandRun>,
    ) -> Self {
        let roots_of_contexts_sorting_planes_by_depth = tree.roots_of_contexts_sorting_planes_by_depth();
        let mut sorting_context_of_run = Vec::new();
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
                sorting_context_of_run.push(
                    roots_of_contexts_sorting_planes_by_depth
                        .get(run.context.spatial.0 as usize)
                        .copied()
                        .flatten(),
                );
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
        let first_and_last_run_of_groups =
            first_and_last_run_of_groups_holding_whole_grouping_effects_and_sorting_contexts(
                &grouping_effect_of_run,
                &sorting_context_of_run,
            );
        let mut group_of_run = vec![0u32; grouping_effect_of_run.len()];
        for (group_index, &(first, last)) in first_and_last_run_of_groups.iter().enumerate() {
            group_of_run[first as usize..=last as usize].fill(group_index as u32);
        }

        let group_is_under_mask = first_and_last_run_of_groups
            .iter()
            .map(|&(first, last)| {
                run_is_under_mask[first as usize..=last as usize]
                    .iter()
                    .any(|&under_mask| under_mask)
            })
            .collect();
        let run_is_sorted_by_depth: Vec<bool> = sorting_context_of_run.iter().map(Option::is_some).collect();
        let group_is_sorted_by_depth = first_and_last_run_of_groups
            .iter()
            .map(|&(first, last)| {
                run_is_sorted_by_depth[first as usize..=last as usize]
                    .iter()
                    .any(|&sorted| sorted)
            })
            .collect();
        Self {
            first_and_last_run_of_groups,
            group_of_run,
            group_is_under_mask,
            roots_of_contexts_sorting_planes_by_depth,
            run_is_sorted_by_depth,
            group_is_sorted_by_depth,
        }
    }

    pub fn root_of_context_sorting_planes_by_depth(&self, spatial: SpatialNodeIndex) -> Option<SpatialNodeIndex> {
        self.roots_of_contexts_sorting_planes_by_depth
            .get(spatial.0 as usize)
            .copied()
            .flatten()
    }

    /// Whether the groups are those of a tree whose 3D sorting contexts sort the same content by depth.
    pub fn match_depth_sorting_roots(
        &self,
        roots_of_contexts_sorting_planes_by_depth: &[Option<SpatialNodeIndex>],
    ) -> bool {
        self.roots_of_contexts_sorting_planes_by_depth == roots_of_contexts_sorting_planes_by_depth
    }

    /// Whether the group holds a 3D sorting context that sorts its planes by depth, whose content is replayed whole
    /// wherever any of the group shows, as sorting the planes reads all of them.
    pub fn group_is_sorted_by_depth(&self, group: RunGroupIndex) -> bool {
        self.group_is_sorted_by_depth[group.0]
    }

    pub fn run_is_sorted_by_depth(&self, run_index: usize) -> bool {
        self.run_is_sorted_by_depth[run_index]
    }

    pub fn first_and_last_run_of_groups(&self) -> &[(u32, u32)] {
        &self.first_and_last_run_of_groups
    }

    pub fn group_of_run(&self, run_index: usize) -> RunGroupIndex {
        RunGroupIndex(self.group_of_run[run_index] as usize)
    }

    /// Whether the group is under a mask, whose content is replayed whole wherever any of it shows.
    pub fn group_is_under_mask(&self, group: RunGroupIndex) -> bool {
        self.group_is_under_mask[group.0]
    }
}
