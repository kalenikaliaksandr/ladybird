/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a display list is painted in, bottom to top.
//!
//! Content is rastered in the space of the scroll node or held sticky box it moves with, and its raster is composited
//! within the clips it leaves to apply then, and inside the effects the compositor applies to it: content of one space,
//! one set of such clips and one such effect makes a placement node. Within a node, a canvas, video or nested context
//! that is a layer of its own, and each band of content of a node inside it, sits between the node's records as a
//! barrier. A barrier cuts the tiles of the node it can show in, or for an effect that reads what is composited below
//! it, that it reads, at its paint position: there, the node's records painted before it go into the node's layer below
//! it and those painted after it into the node's layer above it. Elsewhere it cuts nothing. A band of a node inside ends
//! where content outside it that can show where the band can is painted, so the layers stacked in paint order of their
//! barriers paint every pixel in the order the list does, as long as nothing within a layer reads what is below the
//! layer. So a backdrop filter that finds all it reads in the layer it is in, where it is painted, reads it there, and
//! one that does not gets a node of its own, whose layers the compositor composites inside the filter's effect as a
//! replay of the list composites the effect, over what is composited below them.

use crate::display_list::builder::read_command;
use crate::display_list::commands::{DeclareMaskContent, DisplayListCommandType, EffectNodeIndex, SpatialNodeIndex};
use crate::display_list::effect_geometry::{EffectGeometry, compute_effect_geometry};
pub use crate::display_list::placement::{
    CompositeClip, RasterSpace, RasterSpaceAnchorKind, VIEWPORT_RASTER_SPACE, raster_space_anchors,
};
use crate::display_list::placement::{
    PlacementTables, RasterSpaces, RunsReachingOutsideTheirContext, SpaceGeometry, chain_of_space, is_finite_rect,
    lowest_common_space, raster_scroll_offsets, rect_swept_over_translations, spaces_with_geometries,
    spatial_nodes_that_move_on_their_own,
};
use crate::display_list::reach::Reach;
use crate::display_list::storage::{DisplayListStorage, ListPosition, ListSpan};
use crate::display_list::summary::{IndexedRecord, LiveContent};
use crate::display_list::tiles::{
    TILE_SIZE_IN_DEVICE_PIXELS, reaches_too_many_cells_to_follow_one_by_one, tile_coords_touching,
};
use crate::display_list::window_server_placement::{WindowServerPlacement, WindowServerRejection};
use crate::visual_context::{EffectNodeData, SpatialData, VisualContextTree, WorldTransforms, device_offset_for_index};
use libgfx_rust::{FloatMatrix4x4, FloatPoint, IntPoint, IntRect};
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;

pub const MAXIMUM_MEDIA_LAYER_COUNT: usize = 8;
// Beyond this, the list is placed again with the spaces of nested scrollers and held sticky boxes rastered in the spaces
// around them, once.
const MAXIMUM_LAYER_COUNT: usize = 24;

pub(crate) fn media_name(content: LiveContent) -> &'static str {
    match content {
        LiveContent::Canvas(_) => "canvas",
        LiveContent::VideoFrame(_) => "video",
        LiveContent::ImageFrame(_) => "image",
        LiveContent::CompositedContext(_) => "composited context",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaLayerRejection {
    DrawnInsideAnotherCommand,
    UnderTransformThatIsNot2DAffine,
    DrawsNothingVisible,
    MoreMediaThanLayersFor,
}

impl std::fmt::Display for MediaLayerRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            MediaLayerRejection::DrawnInsideAnotherCommand => "drawn inside another command",
            MediaLayerRejection::UnderTransformThatIsNot2DAffine => "under a transform that is not 2D affine",
            MediaLayerRejection::DrawsNothingVisible => "draws nothing visible",
            MediaLayerRejection::MoreMediaThanLayersFor => "past the most media that get layers",
        })
    }
}

/// Why the compositor composites the layers of an effect's content inside the effect, as a replay of the list
/// composites the effect, rather than tiles holding the effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyEffectIsComposited {
    ItReadsWhatItsOwnLayerDoesNotHold,
    ItsBackdropFilterReadsPastTheEdgeOfTheFrame,
    ItHoldsAnEffectTheCompositorComposites,
    ItHoldsMedia(LiveContent),
}

impl std::fmt::Display for WhyEffectIsComposited {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyEffectIsComposited::ItReadsWhatItsOwnLayerDoesNotHold => "it reads what its own layer does not hold",
            WhyEffectIsComposited::ItsBackdropFilterReadsPastTheEdgeOfTheFrame => {
                "its backdrop filter reads past the edge of the frame"
            }
            WhyEffectIsComposited::ItHoldsAnEffectTheCompositorComposites => {
                "it holds an effect the compositor composites"
            }
            WhyEffectIsComposited::ItHoldsMedia(content) => {
                return write!(formatter, "it holds a {} with a layer of its own", media_name(*content));
            }
        })
    }
}

/// Why a list is painted in a single layer that replays all of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlayeredReason {
    EmptyViewport,
    EmptyDisplayList,
    SortingContext3D,
    RootIsolationEffectIsNotPlainGroup,
    MaskDeclaredUnderEffectsItsMaskIsNotUnder,
    RecordsBlendWithWhatOtherLayersHoldBelowThem,
}

impl std::fmt::Display for UnlayeredReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            UnlayeredReason::EmptyViewport => "empty viewport",
            UnlayeredReason::EmptyDisplayList => "empty display list",
            UnlayeredReason::SortingContext3D => "3D sorting context",
            UnlayeredReason::RootIsolationEffectIsNotPlainGroup => "root isolation effect is not a plain group",
            UnlayeredReason::MaskDeclaredUnderEffectsItsMaskIsNotUnder => {
                "mask content declared under effects its mask is not under"
            }
            UnlayeredReason::RecordsBlendWithWhatOtherLayersHoldBelowThem => {
                "records blend with what other layers hold below them"
            }
        })
    }
}

/// Names a placement node by the identity of its space, the clips its rasters are composited within, and the effects
/// they are composited inside, which outlive the tree and the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlacementKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarrierKey {
    /// The `ordinal`th drawing of a canvas, video or nested context in paint order.
    Media { content: LiveContent, ordinal: u32 },
    /// The `band`th band of content of a node, counted over the whole list.
    Band { node: PlacementKey, band: u32 },
}

/// Names a layer from one list to the next, so that its tiles outlive both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerKey {
    Painted {
        node: PlacementKey,
        band: u32,
        above: Option<BarrierKey>,
    },
    Media {
        content: LiveContent,
        ordinal: u32,
    },
}

/// Content rastered in one space, composited within one set of clips, and inside at most one effect that the
/// compositor applies, which a replay of the list would apply around it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementNode {
    pub key: PlacementKey,
    pub space: usize,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    /// The effect the compositor composites the rasters of the node inside, within those it composites the parent
    /// node inside. The rasters leave it out, with the effects around it and the clips it is entered within.
    pub composited_effect: Option<EffectNodeIndex>,
    pub parent: Option<usize>,
    /// Where content of the node and of the nodes in it can show, or a composited effect read, in the raster
    /// coordinates of the parent node.
    pub region_in_parent: Reach<IntRect>,
}

/// Where a barrier cuts the tiles of a group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub barrier: BarrierKey,
    /// Where the barrier's first record is.
    pub position: ListPosition,
    /// The barrier cuts the tiles of the group's space this reaches.
    pub region: Reach<IntRect>,
    pub layer_above: usize,
}

impl Cut {
    pub fn cuts_cell(&self, cell: IntRect) -> bool {
        self.region.touches(cell)
    }
}

/// One band of a placement node: the records placed in the node while the band lasts, and the barriers that cut them.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerGroup {
    pub node: usize,
    pub band: u32,
    /// The group of the band of the parent node that this band is a barrier in.
    pub group_it_is_a_barrier_in: Option<usize>,
    /// In paint order.
    pub record_spans: Vec<ListSpan>,
    /// In paint order.
    pub cuts: Vec<Cut>,
    pub layer_below_every_cut: usize,
}

impl LayerGroup {
    /// The layer the group's records at `position` go into within the tile at `cell`: the one above the last barrier
    /// painted before them that cuts the tile.
    pub fn layer_at(&self, position: ListPosition, cell: IntRect) -> usize {
        self.cuts
            .iter()
            .take_while(|cut| cut.position <= position)
            .filter(|cut| cut.cuts_cell(cell))
            .last()
            .map_or(self.layer_below_every_cut, |cut| cut.layer_above)
    }

    /// Which barriers cut the tile at `cell`, one bit each, up to as many as a word holds and every further one alike:
    /// tiles that answer alike place every record alike.
    pub fn cuts_of_cell(&self, cell: IntRect) -> u64 {
        self.cuts
            .iter()
            .enumerate()
            .filter(|(_, cut)| cut.cuts_cell(cell))
            .fold(0, |cuts, (index, _)| cuts | (1u64 << index.min(63)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    /// The records of a group that the layer holds in some of its tiles.
    Painted { group: usize },
    /// A canvas, video or nested context, replayed or shown by the window server.
    Media {
        content: LiveContent,
        record_span: ListSpan,
        /// In the raster coordinates of the node's space.
        region: IntRect,
        /// For a canvas or a video, where the window server can show its content by itself, or why it cannot.
        window_server_placement: Option<Result<WindowServerPlacement, WindowServerRejection>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackLayer {
    pub key: LayerKey,
    pub node: usize,
    /// The group of the band the layer is in: the band it holds the records of, or the one it is a barrier in.
    pub group_of_band: usize,
    pub content: LayerContent,
    /// Whether any record can go into the layer anywhere; a layer that holds nothing has no tiles.
    pub may_hold_content: bool,
}

/// An effect that the compositor composites the layers of its content inside, and why.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompositedEffect {
    pub effect: EffectNodeIndex,
    pub node: usize,
    pub why: WhyEffectIsComposited,
    /// What the effect reads of what is composited below it, in the raster coordinates of its node's space.
    pub read_area: Reach<IntRect>,
    /// Whether its filter moves what it composites past where it is drawn, so that it reads what is drawn inside it
    /// past each of its pixels.
    pub filter_spreads_what_it_composites: bool,
}

/// The layers of a list bottom to top, with the base layer, which is rastered over the surface's clear color and
/// replaces what is below it. Layers below the base layer hold nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerStack {
    pub spaces: Vec<RasterSpace>,
    pub nodes: Vec<PlacementNode>,
    pub groups: Vec<LayerGroup>,
    pub layers: Vec<StackLayer>,
    /// In paint order.
    pub composited_effects: Vec<CompositedEffect>,
    pub base_layer: usize,
    /// Why the stack is one layer that replays the whole list, if it is.
    pub unlayered_reason: Option<UnlayeredReason>,
    pub layering_obstacles: LayeringObstacles,
    /// The canvases, videos and nested contexts that were not given layers, and why.
    pub rejected_media: Vec<(LiveContent, MediaLayerRejection)>,
    /// The backdrop filters and blend modes that find all they read in the layer they go into, and read it there
    /// rather than the compositor compositing them, in paint order.
    pub effects_reading_in_their_tiles: Vec<EffectNodeIndex>,
    /// Where the parts of the list are placed among the spaces of the stack.
    pub placement: ListPlacement,
    /// What the layer budget did to the stack.
    pub layer_budget: LayerBudget,
    /// Where they are placed at every anchor the tree offers, which the stack was worked out from: a later frame
    /// whose placement there compares equal keeps the stack, even where the layer budget had the list placed again
    /// without some of those spaces.
    pub placement_at_every_anchor: ListPlacement,
}

/// How many layers a stack holds content in, from the base layer up, and whether spaces gave way to keep them within
/// the most a stack holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerBudget {
    pub layers_with_content: usize,
    pub spaces_gave_way: bool,
}

fn translated(rect: IntRect, offset: IntPoint) -> IntRect {
    IntRect::new(rect.x + offset.x, rect.y + offset.y, rect.width, rect.height)
}

fn key_of_node(spaces: &[RasterSpace], space: usize, composite_clips: &[CompositeClip]) -> PlacementKey {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x706c_6163_656d_656e).build_hasher();
    hasher.write_u64(spaces[space].identity);
    for clip in composite_clips {
        hasher.write_u64(spaces[clip.space].identity);
        for value in [clip.rect.x, clip.rect.y, clip.rect.width, clip.rect.height] {
            hasher.write_i32(value);
        }
    }
    PlacementKey(hasher.finish())
}

// The effect index names the effect for as long as the tree keeps the node, which outlives lists and scrolls.
fn key_of_composited_node(parent: PlacementKey, effect: EffectNodeIndex) -> PlacementKey {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x636f_6d70_6f73_6974).build_hasher();
    hasher.write_u64(parent.0);
    hasher.write_u32(effect.0);
    PlacementKey(hasher.finish())
}

impl LayerStack {
    /// One layer that replays the whole list, which is always correct.
    pub fn single_layer(viewport_rect: IntRect, whole_list: ListSpan, reason: Option<UnlayeredReason>) -> Self {
        let spaces = vec![RasterSpace::of_the_viewport(viewport_rect)];
        let root = PlacementNode {
            key: key_of_node(&spaces, VIEWPORT_RASTER_SPACE, &[]),
            space: VIEWPORT_RASTER_SPACE,
            composite_clips: Vec::new(),
            composited_effect: None,
            parent: None,
            region_in_parent: Reach::Anywhere,
        };
        Self {
            layers: vec![StackLayer {
                key: LayerKey::Painted {
                    node: root.key,
                    band: 0,
                    above: None,
                },
                node: 0,
                group_of_band: 0,
                content: LayerContent::Painted { group: 0 },
                may_hold_content: true,
            }],
            groups: vec![LayerGroup {
                node: 0,
                band: 0,
                group_it_is_a_barrier_in: None,
                record_spans: vec![whole_list],
                cuts: Vec::new(),
                layer_below_every_cut: 0,
            }],
            nodes: vec![root],
            spaces,
            composited_effects: Vec::new(),
            base_layer: 0,
            unlayered_reason: reason,
            layering_obstacles: LayeringObstacles::default(),
            rejected_media: Vec::new(),
            effects_reading_in_their_tiles: Vec::new(),
            layer_budget: LayerBudget::default(),
            placement: ListPlacement::default(),
            placement_at_every_anchor: ListPlacement::default(),
        }
    }

    pub fn replays_whole_list(&self) -> bool {
        self.unlayered_reason.is_some()
    }

    fn layers_with_content_from_the_base_layer(&self) -> usize {
        self.layers
            .iter()
            .skip(self.base_layer)
            .filter(|layer| layer.may_hold_content)
            .count()
    }

    /// What the dump says of the layer budget, where the stack holds more layers than it or spaces gave way to it.
    pub fn names_of_what_the_layer_budget_did(&self) -> impl Iterator<Item = String> + '_ {
        let spaces_gave_way = self.layer_budget.spaces_gave_way.then(|| {
            format!("nested scrollers and held sticky boxes have no spaces: past {MAXIMUM_LAYER_COUNT} layers")
        });
        let layers_past_the_budget = (self.layer_budget.layers_with_content > MAXIMUM_LAYER_COUNT).then(|| {
            format!(
                "{} layers past the most a stack holds, {MAXIMUM_LAYER_COUNT}: composited effects keep theirs",
                self.layer_budget.layers_with_content
            )
        });
        spaces_gave_way.into_iter().chain(layers_past_the_budget)
    }

    pub fn keys(&self) -> impl Iterator<Item = LayerKey> + '_ {
        self.layers.iter().map(|layer| layer.key)
    }

    /// Whether the stack has the shape of `other`: the same spaces, placement nodes and layers, which can hold content
    /// where those of `other` can, and the same barriers cutting the same regions, wherever the records of the two
    /// lists are. What is keyed against a stack holds for any stack of its shape.
    pub fn has_the_shape_of(&self, other: &LayerStack) -> bool {
        let space_is_alike = |space: &RasterSpace, other: &RasterSpace| {
            space.anchor == other.anchor
                && space.identity == other.identity
                && space.parent == other.parent
                && space.anchor_kind == other.anchor_kind
        };
        let layer_is_alike = |layer: &StackLayer, other: &StackLayer| {
            layer.key == other.key
                && layer.node == other.node
                && layer.group_of_band == other.group_of_band
                && layer.may_hold_content == other.may_hold_content
                && match (&layer.content, &other.content) {
                    (LayerContent::Painted { group }, LayerContent::Painted { group: other_group }) => {
                        group == other_group
                    }
                    (
                        LayerContent::Media { content, region, .. },
                        LayerContent::Media {
                            content: other_content,
                            region: other_region,
                            ..
                        },
                    ) => content == other_content && region == other_region,
                    _ => false,
                }
        };
        let group_is_alike = |group: &LayerGroup, other: &LayerGroup| {
            group.node == other.node
                && group.band == other.band
                && group.group_it_is_a_barrier_in == other.group_it_is_a_barrier_in
                && group.layer_below_every_cut == other.layer_below_every_cut
                && group.cuts.len() == other.cuts.len()
                && group.cuts.iter().zip(&other.cuts).all(|(cut, other)| {
                    cut.barrier == other.barrier && cut.region == other.region && cut.layer_above == other.layer_above
                })
        };
        self.unlayered_reason == other.unlayered_reason
            && self.layering_obstacles == other.layering_obstacles
            && self.base_layer == other.base_layer
            && self.nodes == other.nodes
            && self.composited_effects == other.composited_effects
            && self.spaces.len() == other.spaces.len()
            && self
                .spaces
                .iter()
                .zip(&other.spaces)
                .all(|(space, other)| space_is_alike(space, other))
            && self.layers.len() == other.layers.len()
            && self
                .layers
                .iter()
                .zip(&other.layers)
                .all(|(layer, other)| layer_is_alike(layer, other))
            && self.groups.len() == other.groups.len()
            && self
                .groups
                .iter()
                .zip(&other.groups)
                .all(|(group, other)| group_is_alike(group, other))
    }

    pub fn space_of_layer(&self, layer: usize) -> usize {
        self.nodes[self.layers[layer].node].space
    }

    pub fn raster_scroll_offsets(&self, space: usize, scroll_offsets: &[FloatPoint]) -> Vec<FloatPoint> {
        raster_scroll_offsets(&self.spaces, space, scroll_offsets)
    }

    /// Where the frame composites the rasters of a space: translated by the offsets of the anchors of its chain, on the
    /// whole device pixels a direct raster translates their content by.
    pub fn composite_offset(&self, space: usize, scroll_offsets: &[FloatPoint]) -> IntPoint {
        chain_of_space(&self.spaces, space).fold(IntPoint::default(), |sum, space| {
            let raster_space = &self.spaces[space];
            let translation = raster_space.anchor_translation_on_whole_device_pixels(device_offset_for_index(
                scroll_offsets,
                raster_space.anchor,
            ));
            IntPoint {
                x: sum.x + translation.x,
                y: sum.y + translation.y,
            }
        })
    }

    pub fn composite_offsets(&self, scroll_offsets: &[FloatPoint]) -> Vec<IntPoint> {
        (0..self.spaces.len())
            .map(|space| self.composite_offset(space, scroll_offsets))
            .collect()
    }

    /// The clip a layer's raster is composited within, in the viewport, or none for no clip.
    pub fn composite_clip_in_viewport(&self, layer: usize, composite_offsets: &[IntPoint]) -> Option<IntRect> {
        self.composite_clip_of_node_in_viewport(self.layers[layer].node, composite_offsets)
    }

    /// The clip the rasters of a node are composited within, in the viewport, or none for no clip.
    pub fn composite_clip_of_node_in_viewport(&self, node: usize, composite_offsets: &[IntPoint]) -> Option<IntRect> {
        self.nodes[node].composite_clips.iter().fold(None, |clip_rect, clip| {
            let rect = translated(clip.rect, composite_offsets[clip.space]);
            Some(clip_rect.map_or(rect, |clip_rect: IntRect| clip_rect.intersected(rect)))
        })
    }

    pub fn has_media_layers(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| matches!(layer.content, LayerContent::Media { .. }))
    }

    /// Whether the compositor composites an effect of the stack over what is composited below it, which the frame then
    /// has to hold rather than the window server showing it above the frame.
    pub fn has_composited_effects(&self) -> bool {
        !self.composited_effects.is_empty()
    }

    /// The effects the compositor composites, which rasters of their content leave out.
    pub fn effects_the_compositor_composites(&self) -> Vec<EffectNodeIndex> {
        self.composited_effects
            .iter()
            .map(|composited| composited.effect)
            .collect()
    }

    /// The groups of the bands the layer is in, outermost first, whose nodes the compositor composites inside their
    /// effects.
    pub fn composited_groups_around_layer(&self, layer: usize) -> Vec<usize> {
        let mut groups = Vec::new();
        let mut group = Some(self.layers[layer].group_of_band);
        while let Some(index) = group {
            if self.nodes[self.groups[index].node].composited_effect.is_some() {
                groups.push(index);
            }
            group = self.groups[index].group_it_is_a_barrier_in;
        }
        groups.reverse();
        groups
    }

    /// Works out again where the window server can show the content of each canvas and video layer, under
    /// `scroll_offsets`, as the stack is kept while its spaces scroll and its media move with them.
    pub fn update_window_server_placements(
        &mut self,
        list: &DisplayListStorage,
        tree: &VisualContextTree,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
    ) {
        let window_server_may_show = |content: &LiveContent| !matches!(content, LiveContent::CompositedContext(_));
        let has_media_the_window_server_may_show = self.layers.iter().any(
            |layer| matches!(&layer.content, LayerContent::Media { content, .. } if window_server_may_show(content)),
        );
        if !has_media_the_window_server_may_show {
            return;
        }
        let world = WorldTransforms::new(tree, scroll_offsets, Some(FloatMatrix4x4::identity()));
        for layer in &mut self.layers {
            let LayerContent::Media {
                content,
                record_span,
                window_server_placement,
                ..
            } = &mut layer.content
            else {
                continue;
            };
            if !window_server_may_show(content) {
                continue;
            }
            let position = record_span.begin;
            let runs = list.chunk(position.chunk).runs();
            let run = &runs[runs.partition_point(|run| run.offset + run.size <= position.offset)];
            let (header, payload) = list.record_at(position);
            *window_server_placement = Some(crate::display_list::window_server_placement::window_server_placement(
                tree,
                &world,
                viewport_rect,
                run,
                &header,
                payload,
            ));
        }
    }

    fn name_of_space(&self, space: usize) -> String {
        let raster_space = &self.spaces[space];
        let kind = match raster_space.anchor_kind {
            RasterSpaceAnchorKind::Viewport => return "viewport".into(),
            RasterSpaceAnchorKind::ViewportScroller => return "document".into(),
            RasterSpaceAnchorKind::Scroller => "scroller",
            RasterSpaceAnchorKind::HeldStickyBox { .. } => "sticky",
        };
        format!(
            "{kind}@{} in {}",
            raster_space.anchor.0,
            self.name_of_space(raster_space.parent)
        )
    }

    // The space of the node, and the effects the compositor composites it inside, innermost first.
    fn name_of_node(&self, node: usize) -> String {
        let mut name = self.name_of_space(self.nodes[node].space);
        let mut node = Some(node);
        while let Some(index) = node {
            if let Some(effect) = self.nodes[index].composited_effect {
                name.push_str(&format!(" inside e{}", effect.0));
            }
            node = self.nodes[index].parent;
        }
        name
    }

    fn name_of_barrier(&self, barrier: &BarrierKey) -> String {
        match barrier {
            BarrierKey::Media { content, ordinal } => {
                let key = LayerKey::Media {
                    content: *content,
                    ordinal: *ordinal,
                };
                let layer = self
                    .layers
                    .iter()
                    .position(|layer| layer.key == key)
                    .expect("a media barrier is a layer of the stack");
                format!("L{layer}")
            }
            BarrierKey::Band { node, band } => {
                let node = self
                    .nodes
                    .iter()
                    .position(|candidate| candidate.key == *node)
                    .expect("a band barrier names a node of the stack");
                format!("band {band} of {}", self.name_of_node(node))
            }
        }
    }

    /// What the dump calls a layer: its index, what it holds, the space it is rastered in and the effects the
    /// compositor composites it inside, and what it sits above.
    pub fn name_of_layer(&self, index: usize) -> String {
        if self.unlayered_reason.is_some() {
            return "[L0] single layer replaying the whole list".into();
        }
        let layer = &self.layers[index];
        let space = self.name_of_node(layer.node);
        match (&layer.key, &layer.content) {
            (LayerKey::Painted { band, above, .. }, _) => {
                let kind = if index == self.base_layer { "base" } else { "painted" };
                let mut name = format!("[L{index}] {kind} space={space}");
                if *band != 0 {
                    name.push_str(&format!(" band={band}"));
                }
                if let Some(above) = above {
                    name.push_str(&format!(" above {}", self.name_of_barrier(above)));
                }
                name
            }
            (LayerKey::Media { content, ordinal }, LayerContent::Media { region, .. }) => format!(
                "[L{index}] {} #{ordinal} space={space} region=[{},{} {}x{}]",
                media_name(*content),
                region.x,
                region.y,
                region.width,
                region.height
            ),
            (LayerKey::Media { .. }, _) => unreachable!("a media key names a media layer"),
        }
    }

    /// What the dump says of an effect the compositor composites: the space of its node, what it reads of what is
    /// composited below it, and why it is composited.
    pub fn names_of_composited_effects(&self) -> impl Iterator<Item = String> + '_ {
        self.composited_effects.iter().map(|composited| {
            let parent = self.nodes[composited.node]
                .parent
                .expect("a composited node is inside another");
            let reads = match composited.read_area {
                Reach::Nothing => String::new(),
                Reach::Within(rect) => format!(" reads=[{},{} {}x{}]", rect.x, rect.y, rect.width, rect.height),
                Reach::Anywhere => " reads=anywhere".into(),
            };
            format!(
                "composited e{} space={}{reads}: {}",
                composited.effect.0,
                self.name_of_node(parent),
                composited.why
            )
        })
    }

    /// What the dump says of a backdrop filter or blend mode that reads in the layer it goes into.
    pub fn names_of_effects_reading_in_their_tiles(&self) -> impl Iterator<Item = String> + '_ {
        self.effects_reading_in_their_tiles
            .iter()
            .map(|effect| format!("e{} is not composited: it reads only its own layer", effect.0))
    }

    /// What the dump says of a canvas, video or nested context that was not given a layer, and why.
    pub fn names_of_media_without_layers(&self) -> impl Iterator<Item = String> + '_ {
        self.rejected_media
            .iter()
            .map(|(content, rejection)| format!("no layer for {}: {rejection}", media_name(*content)))
    }

    /// One line per layer, bottom to top, after `indent`: its index, what it holds, the space it is rastered in, what it
    /// sits above, what it is clipped to as it is composited, and for a canvas or video whether the window server
    /// shows it. A layer below the base layer, or one that can hold nothing, is left out.
    pub fn write_layer_lines(&self, output: &mut impl std::fmt::Write, indent: &str) -> std::fmt::Result {
        if let Some(reason) = self.unlayered_reason {
            return writeln!(output, "{indent}{}: {reason}", self.name_of_layer(0));
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if index < self.base_layer || (index != self.base_layer && !layer.may_hold_content) {
                continue;
            }
            let node = &self.nodes[layer.node];
            write!(output, "{indent}{}", self.name_of_layer(index))?;
            for clip in &node.composite_clips {
                let clip_rect = clip.rect;
                write!(
                    output,
                    " composited within [{},{} {}x{}] of {}",
                    clip_rect.x,
                    clip_rect.y,
                    clip_rect.width,
                    clip_rect.height,
                    self.name_of_space(clip.space)
                )?;
            }
            match &layer.content {
                LayerContent::Media {
                    window_server_placement: Some(Ok(_)),
                    ..
                } if self.has_composited_effects() => writeln!(
                    output,
                    " replayed: the compositor composites an effect over what is below it"
                )?,
                LayerContent::Media {
                    window_server_placement: Some(Ok(placement)),
                    ..
                } => writeln!(output, " shown by the window server: {placement}")?,
                LayerContent::Media {
                    window_server_placement: Some(Err(rejection)),
                    ..
                } => writeln!(output, " replayed: {rejection}")?,
                _ => writeln!(output)?,
            }
        }
        for name in self
            .names_of_composited_effects()
            .chain(self.names_of_media_without_layers())
            .chain(self.names_of_effects_reading_in_their_tiles())
            .chain(self.names_of_what_the_layer_budget_did())
        {
            writeln!(output, "{indent}{name}")?;
        }
        Ok(())
    }

    /// The layers that the record at `position` can be drawn into, in the tiles it reaches.
    pub fn layers_of_record(&self, position: ListPosition) -> Vec<usize> {
        if self.replays_whole_list() {
            return vec![0];
        }
        for (index, layer) in self.layers.iter().enumerate() {
            let LayerContent::Media { record_span, .. } = &layer.content else {
                continue;
            };
            if record_span.begin == position {
                return vec![index];
            }
        }
        let Some(unit) = self.placement.units.iter().find(|unit| unit.span.contains(position)) else {
            return Vec::new();
        };
        let Some(group) = self
            .groups
            .iter()
            .position(|group| group.record_spans.iter().any(|span| span.contains(position)))
        else {
            return Vec::new();
        };
        let group = &self.groups[group];
        let mut layers: Vec<usize> = if reaches_too_many_cells_to_follow_one_by_one(unit.reach) {
            std::iter::once(group.layer_below_every_cut)
                .chain(
                    group
                        .cuts
                        .iter()
                        .take_while(|cut| cut.position <= unit.span.begin)
                        .map(|cut| cut.layer_above),
                )
                .collect()
        } else {
            tile_coords_touching(unit.reach)
                .map(|coord| group.layer_at(unit.span.begin, coord.cell_rect()))
                .collect()
        };
        layers.sort_unstable();
        layers.dedup();
        layers
    }
}

pub struct LayerStackInputs<'a> {
    pub list: &'a DisplayListStorage,
    pub tree: &'a VisualContextTree,
    pub scroll_offsets: &'a [FloatPoint],
    /// The viewport in the device pixels of the raster, into which `raster_transform` maps those of the viewport.
    pub viewport_rect: IntRect,
    pub raster_transform: FloatMatrix4x4,
}

fn tree_has_sorting_contexts(tree: &VisualContextTree) -> bool {
    tree.spatial_nodes.iter().any(|node| {
        matches!(&node.data, SpatialData::Transform(transform) if transform.sorting_context_root_index.is_some())
    })
}

fn root_isolation_effect_is_plain_group(tree: &VisualContextTree) -> bool {
    let Some(root) = tree.root_isolation_effect else {
        return true;
    };
    match &tree.effect_nodes[root.0 as usize].data {
        EffectNodeData::Effects(effects) => {
            effects.opacity == 1.0
                && effects.blend_mode == libgfx_rust::CompositingAndBlendingOperator::Normal
                && effects.filter.is_none()
                && effects.backdrop_filter.is_none()
        }
        _ => false,
    }
}

fn effect_chain_contains(tree: &VisualContextTree, mut effect: EffectNodeIndex, wanted: EffectNodeIndex) -> bool {
    while !effect.is_none() {
        if effect == wanted {
            return true;
        }
        effect = tree.effect_nodes[effect.0 as usize].parent;
    }
    false
}

// A tile replays the declaration of a mask's content along with what the mask holds, and so opens the effects the
// declaration is under there, which draw nothing only when they are the mask's own or those around it.
fn mask_declarations_sit_on_the_chains_of_their_masks(list: &DisplayListStorage, tree: &VisualContextTree) -> bool {
    list.summary().mask_declarations.iter().all(|record| {
        let (_, payload) = list.record_at(record.position());
        let mask = read_command::<DeclareMaskContent>(payload).effect;
        let run_effect = list.run(record.run_index as usize).context.effect;
        run_effect.is_none()
            || effect_chain_contains(tree, run_effect, mask)
            || effect_chain_contains(tree, mask, run_effect)
    })
}

/// What of a tree and its list keeps the list from being layered, or the tiles of its layers from replaying only the
/// records that reach them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayeringObstacles {
    pub has_3d_sorting_contexts: bool,
    pub root_isolation_effect_is_not_a_plain_group: bool,
    pub mask_declared_under_effects_its_mask_is_not_under: bool,
}

impl LayeringObstacles {
    pub fn of(list: &DisplayListStorage, tree: &VisualContextTree) -> Self {
        Self {
            has_3d_sorting_contexts: tree_has_sorting_contexts(tree),
            root_isolation_effect_is_not_a_plain_group: !root_isolation_effect_is_plain_group(tree),
            mask_declared_under_effects_its_mask_is_not_under: !mask_declarations_sit_on_the_chains_of_their_masks(
                list, tree,
            ),
        }
    }

    /// Whether every tile replays every record of its group, as which records can show in a tile cannot be told.
    pub fn every_tile_replays_every_record(&self) -> bool {
        self.has_3d_sorting_contexts || self.mask_declared_under_effects_its_mask_is_not_under
    }
}

/// Why a list cannot be layered at all.
pub fn why_list_is_unlayered(
    inputs: &LayerStackInputs<'_>,
    layering_obstacles: &LayeringObstacles,
) -> Option<UnlayeredReason> {
    if inputs.viewport_rect.is_empty() {
        return Some(UnlayeredReason::EmptyViewport);
    }
    if inputs.list.run_count() == 0 {
        return Some(UnlayeredReason::EmptyDisplayList);
    }
    if layering_obstacles.has_3d_sorting_contexts {
        return Some(UnlayeredReason::SortingContext3D);
    }
    if layering_obstacles.root_isolation_effect_is_not_a_plain_group {
        return Some(UnlayeredReason::RootIsolationEffectIsNotPlainGroup);
    }
    if layering_obstacles.mask_declared_under_effects_its_mask_is_not_under {
        return Some(UnlayeredReason::MaskDeclaredUnderEffectsItsMaskIsNotUnder);
    }
    None
}

/// A group of runs, which is placed whole, with where it is placed.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedRunGroup {
    pub first_run: u32,
    pub last_run: u32,
    pub space: usize,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    /// Where it can show in the space's raster coordinates, under the values the frame shows, except that content
    /// that moves on its own is taken to be anywhere its fixed clips let it show.
    pub device_rect: IntRect,
    pub moves_within_space: bool,
    /// Whether the group paints within the root group, rather than below it as the canvas background is.
    pub paints_within_the_root_group: bool,
    /// Where records of the group that blend with what the root group holds below them read it, in the space's raster
    /// coordinates.
    pub read_by_records_blending_with_the_root_group: Reach<IntRect>,
}

/// Where a backdrop filter reads, in the raster coordinates of the space: its region, and how far its filter reaches
/// past it.
pub fn backdrop_filter_read_area(
    tree: &VisualContextTree,
    geometry: &SpaceGeometry,
    effect: EffectNodeIndex,
) -> Reach<IntRect> {
    let node = &tree.effect_nodes[effect.0 as usize];
    let EffectNodeData::Effects(effects) = &node.data else {
        return Reach::Anywhere;
    };
    let Some(backdrop) = &effects.backdrop_filter else {
        return Reach::Anywhere;
    };
    let effect_to_device = geometry.world.draw_matrix(node.spatial);
    let Some(reach) = crate::display_list::tile_keys::backdrop_filter_read_reach_in_device_pixels(
        &backdrop.filter,
        &effect_to_device,
    ) else {
        return Reach::Anywhere;
    };
    let region = geometry
        .world
        .map_rect_to_root(node.spatial, backdrop.region.to_float());
    if !is_finite_rect(region) {
        return Reach::Anywhere;
    }
    Reach::Within(libgfx_rust::enclosing_int_rect(region).inflated_edges(reach, reach, reach, reach))
}

/// A stretch of records of one group that is placed as one: the records of the group's runs that are inside the same
/// effects the compositor can composite, or a record of a run that draws a canvas, video or nested context that is a
/// layer of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedUnit {
    pub span: ListSpan,
    pub group: usize,
    pub reach: IntRect,
    pub media: Option<usize>,
    /// The innermost effect the compositor can composite the unit's records inside.
    pub effect: Option<usize>,
}

/// An effect whose content the compositor composites inside it, or may where the content does not find all it reads
/// in the layer it goes into, with where it is placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedEffect {
    pub effect: EffectNodeIndex,
    /// The placed effect around it, inside which the compositor composites it.
    pub around: Option<usize>,
    pub group: usize,
    /// Where compositing the effect can draw, in the raster coordinates of the group's space, wherever the effect can
    /// show if it moves within its space.
    pub region: IntRect,
    /// What it reads of what is composited below it there.
    pub read_area: Reach<IntRect>,
    /// Whether what it reads below it is the content of the root group, which the bands it can read are only known of
    /// as the stack is cut into them.
    pub reads_the_root_group: bool,
    /// Why the compositor composites it, whatever the layer it goes into holds, if it does.
    pub composited_whatever_its_layer_holds: Option<WhyEffectIsComposited>,
    pub filter_spreads_what_it_composites: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedMedia {
    pub content: LiveContent,
    pub ordinal: u32,
    pub position: ListPosition,
    pub run_index: u32,
}

/// Where each part of a list is placed at the spaces a tree offers, under the values a frame shows. A frame whose
/// placement compares equal to the one a stack was worked out from keeps the stack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListPlacement {
    pub layering_obstacles: LayeringObstacles,
    pub spaces: Vec<RasterSpace>,
    pub groups: Vec<PlacedRunGroup>,
    pub units: Vec<PlacedUnit>,
    pub media: Vec<PlacedMedia>,
    pub rejected_media: Vec<(LiveContent, MediaLayerRejection)>,
    /// In paint order, each after the one around it.
    pub effects: Vec<PlacedEffect>,
    /// The nodes that read the backdrop of a backdrop filter or a blend mode, in paint order.
    pub readers_in_paint_order: Vec<EffectNodeIndex>,
}

fn canonical_composite_clips(clips: &[CompositeClip]) -> Vec<CompositeClip> {
    let mut by_space: Vec<CompositeClip> = Vec::with_capacity(clips.len());
    for clip in clips {
        match by_space.iter_mut().find(|known| known.space == clip.space) {
            Some(known) => known.rect = known.rect.intersected(clip.rect),
            None => by_space.push(*clip),
        }
    }
    by_space.sort_by_key(|clip| clip.space);
    by_space
}

fn media_layer_rejection(
    inputs: &LayerStackInputs<'_>,
    geometry: &SpaceGeometry,
    run_index: u32,
    position: ListPosition,
    moves_within_space: bool,
) -> Option<MediaLayerRejection> {
    let table = inputs.list.chunk(position.chunk).record_table();
    let record = &table.records()[table.records_within(position.offset, position.offset + 1).start as usize];
    if !matches!(
        record.command_type,
        DisplayListCommandType::DrawCanvas
            | DisplayListCommandType::DrawVideoFrame
            | DisplayListCommandType::DrawCompositedContext
    ) {
        return Some(MediaLayerRejection::DrawnInsideAnotherCommand);
    }
    let run = inputs.list.run(run_index as usize);
    if !geometry.world.draw_matrix(run.context.spatial).is_2d_affine() {
        return Some(MediaLayerRejection::UnderTransformThatIsNot2DAffine);
    }
    // What moves on its own may show wherever its fixed clips let it, which its group's rect already says.
    if !moves_within_space && geometry.ink_device_rect(run, record.bounding_rect).is_empty() {
        return Some(MediaLayerRejection::DrawsNothingVisible);
    }
    None
}

// A backdrop filter or a blend mode, which reads what is composited below the layer of the node that reads it: the
// outermost of the masks of a box that draw its filtered backdrop, or the effect itself.
#[derive(Clone, Copy)]
struct Reader {
    backdrop_filter_effect: Option<EffectNodeIndex>,
}

// Per effect node, the reader it is the node that reads the backdrop of, if any.
fn readers_of_tree(tree: &VisualContextTree) -> Vec<Option<Reader>> {
    let mut readers = vec![None; tree.effect_nodes.len()];
    for (index, node) in tree.effect_nodes.iter().enumerate() {
        let EffectNodeData::Effects(effects) = &node.data else {
            continue;
        };
        let effect = EffectNodeIndex(index as u32);
        if effects.backdrop_filter.is_some() {
            let reading_effect = tree.node_reading_the_backdrop_of(effect);
            readers[reading_effect.0 as usize] = Some(Reader {
                backdrop_filter_effect: Some(effect),
            });
        } else if effects.blend_mode != libgfx_rust::CompositingAndBlendingOperator::Normal {
            readers[index] = Some(Reader {
                backdrop_filter_effect: None,
            });
        }
    }
    readers
}

fn effect_is_inside(tree: &VisualContextTree, effect: EffectNodeIndex, around: EffectNodeIndex) -> bool {
    effect != around && effect_chain_contains(tree, effect, around)
}

/// Where each part of the list is placed among `spaces`, given the geometry of each of them.
pub fn place_list(
    inputs: &LayerStackInputs<'_>,
    spaces: RasterSpaces,
    geometries: &[Rc<SpaceGeometry>],
) -> ListPlacement {
    let tree = inputs.tree;
    let list = inputs.list;
    let effect_clips = list.effect_clip_plan(tree);
    let run_groups = list.run_groups(tree);
    let tables = PlacementTables::new(tree, spaces, geometries);
    let spaces = tables.spaces();
    let runs_reaching_outside = RunsReachingOutsideTheirContext::new(list);
    let mut runs_blending_with_root_group = vec![false; list.run_count()];
    for record in &list.summary().destination_blending_records {
        let effect = list.run(record.run_index as usize).context.effect;
        if tree.content_under_effect_composites_into_root_group(effect) {
            runs_blending_with_root_group[record.run_index as usize] = true;
        }
    }
    let mut effect_geometry_of_spaces: Vec<Option<Vec<EffectGeometry>>> = vec![None; spaces.len()];
    let mut output_rects_of_effects: std::collections::HashMap<(usize, u32), IntRect> =
        std::collections::HashMap::new();
    // Where the output of an effect can show in the raster coordinates of a space, within the clips fixed in it.
    let mut output_rect_of_effect = |space: usize, effect: EffectNodeIndex| {
        *output_rects_of_effects.entry((space, effect.0)).or_insert_with(|| {
            let geometry = &geometries[space];
            let effect_geometry = effect_geometry_of_spaces[space].get_or_insert_with(|| {
                let mut effect_geometry = Vec::new();
                compute_effect_geometry(tree, list.runs(), &geometry.world, &mut effect_geometry);
                effect_geometry
            });
            geometry.group_device_rect(tree, effect, effect_geometry[effect.0 as usize].output, effect_clips)
        })
    };
    let mut rect_of_run = vec![IntRect::default(); list.run_count()];

    let groups: Vec<PlacedRunGroup> = run_groups
        .ranges()
        .iter()
        .map(|&(first_run, last_run)| {
            let runs = first_run as usize..=last_run as usize;
            let mut space = runs
                .clone()
                .map(|run_index| {
                    runs_reaching_outside.deepest_space_of_run(tree, &tables, run_index, list.run(run_index))
                })
                .reduce(|first, second| lowest_common_space(spaces, first, second))
                .unwrap_or(VIEWPORT_RASTER_SPACE);
            let composite_clips = loop {
                let mut clips_of_runs = runs.clone().map(|run_index| {
                    runs_reaching_outside.composite_clips_of_run_in(
                        tree,
                        &tables,
                        run_index,
                        list.run(run_index),
                        space,
                    )
                });
                let first_clips = clips_of_runs.next().flatten();
                if let Some(first_clips) = first_clips
                    && clips_of_runs.all(|clips| clips.as_deref() == Some(&*first_clips))
                {
                    break canonical_composite_clips(&first_clips);
                }
                if space == VIEWPORT_RASTER_SPACE {
                    break Vec::new();
                }
                space = spaces[space].parent;
            };
            let geometry = &geometries[space];
            let moves_within_space = runs
                .clone()
                .any(|run_index| tables.context_moves_within_space(tree, list.run(run_index).context, space));
            let mut device_rect = IntRect::default();
            let mut read_by_records_blending_with_the_root_group = Reach::Nothing;
            for run_index in runs {
                let run = list.run(run_index);
                let group = run_groups.grouping_effect_of_run(run_index);
                let run_rect = if moves_within_space {
                    geometry.device_rect(None, run.context.clip)
                } else if group.is_none() {
                    geometry.ink_device_rect(run, run.ink_bounds)
                } else {
                    output_rect_of_effect(space, group)
                };
                rect_of_run[run_index] = run_rect;
                device_rect = device_rect.united(run_rect);
                // A blend reads below what it draws.
                if runs_blending_with_root_group[run_index] {
                    read_by_records_blending_with_the_root_group =
                        read_by_records_blending_with_the_root_group.united(Reach::Within(run_rect));
                }
            }
            let paints_within_the_root_group = match tree.root_isolation_effect {
                None => true,
                Some(root) => (first_run..=last_run)
                    .any(|run_index| effect_chain_contains(tree, list.run(run_index as usize).context.effect, root)),
            };
            PlacedRunGroup {
                first_run,
                last_run,
                space,
                composite_clips,
                device_rect,
                moves_within_space,
                paints_within_the_root_group,
                read_by_records_blending_with_the_root_group,
            }
        })
        .collect();

    // The effects of the list in paint order of their first run, each after those around it.
    let mut first_run_of_effect = vec![usize::MAX; tree.effect_nodes.len()];
    let mut effects_in_paint_order = Vec::new();
    for run_index in 0..list.run_count() {
        let mut effect = list.run(run_index).context.effect;
        let mut chain = Vec::new();
        while !effect.is_none() && first_run_of_effect[effect.0 as usize] == usize::MAX {
            first_run_of_effect[effect.0 as usize] = run_index;
            chain.push(effect);
            effect = tree.effect_nodes[effect.0 as usize].parent;
        }
        effects_in_paint_order.extend(chain.into_iter().rev());
    }
    let group_of_effect = |effect: EffectNodeIndex| run_groups.group_of_run(first_run_of_effect[effect.0 as usize]);
    // Per effect of the list, where compositing it can draw, which is anywhere its group can show where that moves.
    let mut regions_of_effects = vec![IntRect::default(); tree.effect_nodes.len()];
    for &effect in &effects_in_paint_order {
        let group = &groups[group_of_effect(effect)];
        regions_of_effects[effect.0 as usize] = if group.moves_within_space {
            group.device_rect
        } else {
            output_rect_of_effect(group.space, effect)
        };
    }

    // The canvases, videos and nested contexts are layers of their own where they can be, with
    // canvases and videos ahead of nested contexts once there are more than layers for them.
    let mut media = Vec::new();
    let mut rejected_media = Vec::new();
    for live_content_record in &list.summary().live_content_records {
        let content = live_content_record.content;
        if matches!(content, LiveContent::ImageFrame(_)) {
            continue;
        }
        let record: IndexedRecord = live_content_record.record;
        let (run_index, position) = (record.run_index, record.position());
        let group = &groups[run_groups.group_of_run(run_index as usize)];
        let rejection = if group.moves_within_space && group.device_rect.is_empty() {
            Some(MediaLayerRejection::DrawsNothingVisible)
        } else {
            media_layer_rejection(
                inputs,
                &geometries[group.space],
                run_index,
                position,
                group.moves_within_space,
            )
        };
        match rejection {
            None => media.push(PlacedMedia {
                content,
                ordinal: 0,
                position,
                run_index,
            }),
            Some(rejection) => rejected_media.push((content, rejection)),
        }
    }
    if media.len() > MAXIMUM_MEDIA_LAYER_COUNT {
        media.sort_by_key(|candidate| matches!(candidate.content, LiveContent::CompositedContext(_)));
        for candidate in media.drain(MAXIMUM_MEDIA_LAYER_COUNT..) {
            rejected_media.push((candidate.content, MediaLayerRejection::MoreMediaThanLayersFor));
        }
        media.sort_by_key(|candidate| candidate.position);
    }
    for index in 0..media.len() {
        media[index].ordinal = media[..index]
            .iter()
            .filter(|earlier| earlier.content == media[index].content)
            .count() as u32;
    }

    // The effects around a canvas, video or nested context with a layer of its own hold its barrier, so the compositor
    // composites them, with the media inside.
    let mut composited_whatever_its_layer_holds: Vec<Option<WhyEffectIsComposited>> =
        vec![None; tree.effect_nodes.len()];
    let mark_effects_around_as_composited = |composited: &mut Vec<Option<WhyEffectIsComposited>>,
                                             mut around: EffectNodeIndex,
                                             why: WhyEffectIsComposited| {
        while !around.is_none() && tree.root_isolation_effect != Some(around) {
            if tree.effect_pushes_layer_at_replay(around) && composited[around.0 as usize].is_none() {
                composited[around.0 as usize] = Some(why);
            }
            around = tree.effect_nodes[around.0 as usize].parent;
        }
    };
    // Per placed canvas, video or nested context, where its record can show in the space of its group.
    let reaches_of_media: Vec<IntRect> = media
        .iter()
        .map(|placed| {
            let group = &groups[run_groups.group_of_run(placed.run_index as usize)];
            if group.moves_within_space {
                return group.device_rect;
            }
            let table = list.chunk(placed.position.chunk).record_table();
            let record = &table.records()[table
                .records_within(placed.position.offset, placed.position.offset + 1)
                .start as usize];
            geometries[group.space].ink_device_rect(list.run(placed.run_index as usize), record.bounding_rect)
        })
        .collect();
    for placed in &media {
        mark_effects_around_as_composited(
            &mut composited_whatever_its_layer_holds,
            list.run(placed.run_index as usize).context.effect,
            WhyEffectIsComposited::ItHoldsMedia(placed.content),
        );
    }

    // What each reader reads, and whether the compositor composites it whatever the layer it goes into holds: where its
    // backdrop filter reads past each pixel without mirroring at the edge of the frame, or where an effect around it
    // holds what it reads and a barrier painted before it cuts the tiles it reads there. The effects around one that is
    // composited are composited too, as they hold its barrier.
    let readers = readers_of_tree(tree);
    let mut read_areas = vec![Reach::Nothing; tree.effect_nodes.len()];
    let mut readers_in_paint_order = Vec::new();
    let mut readers_of_the_root_group = Vec::new();
    for &effect in &effects_in_paint_order {
        let Some(reader) = readers[effect.0 as usize] else {
            continue;
        };
        let group = &groups[group_of_effect(effect)];
        let geometry = &geometries[group.space];
        let region = regions_of_effects[effect.0 as usize];
        let (read_area, reads_nothing_past_the_edge_it_mirrors_at) = match reader.backdrop_filter_effect {
            Some(filter_effect) => {
                let node = &tree.effect_nodes[filter_effect.0 as usize];
                let EffectNodeData::Effects(effects) = &node.data else {
                    unreachable!("a backdrop filter belongs to an effects node");
                };
                let backdrop_filter = effects
                    .backdrop_filter
                    .as_ref()
                    .expect("a reader with a backdrop filter has one");
                // What moves within its space can read wherever its fixed clips let it show.
                let read_area = if group.moves_within_space {
                    crate::display_list::tile_keys::backdrop_filter_read_reach_in_device_pixels(
                        &backdrop_filter.filter,
                        &geometry.world.draw_matrix(node.spatial),
                    )
                    .map_or(Reach::Anywhere, |reach| {
                        Reach::Within(group.device_rect.inflated_edges(reach, reach, reach, reach))
                    })
                } else {
                    backdrop_filter_read_area(tree, geometry, filter_effect)
                };
                (
                    read_area,
                    crate::display_list::tile_keys::backdrop_filter_reads_nothing_past_the_edge_it_mirrors_at(
                        &backdrop_filter.filter,
                    ),
                )
            }
            None => (Reach::Within(region), true),
        };
        read_areas[effect.0 as usize] = read_area;
        let reading_node = &tree.effect_nodes[effect.0 as usize];
        let backdrop_root = if reading_node.parent.is_none() {
            None
        } else {
            tree.nearest_layer_at_or_above(reading_node.parent)
                .filter(|root| tree.root_isolation_effect != Some(*root))
        };
        let why = if !reads_nothing_past_the_edge_it_mirrors_at {
            Some(WhyEffectIsComposited::ItsBackdropFilterReadsPastTheEdgeOfTheFrame)
        } else if let Some(backdrop_root) = backdrop_root {
            let read_tiles = match read_area {
                Reach::Within(rect) => Reach::Within(expanded_to_tile_grid(rect)),
                read_area => read_area,
            };
            let cut_by_a_composited_effect = effects_in_paint_order.iter().any(|&other| {
                other != effect
                    && composited_whatever_its_layer_holds[other.0 as usize].is_some()
                    && first_run_of_effect[other.0 as usize] < first_run_of_effect[effect.0 as usize]
                    && effect_is_inside(tree, other, backdrop_root)
                    && !effect_is_inside(tree, effect, other)
                    && read_tiles.meets(Reach::Within(regions_of_effects[other.0 as usize].united(
                        match read_areas[other.0 as usize] {
                            Reach::Within(rect) => rect,
                            _ => IntRect::default(),
                        },
                    )))
            });
            let cut_by_media = media.iter().zip(&reaches_of_media).any(|(placed, reach)| {
                let effect_of_media = list.run(placed.run_index as usize).context.effect;
                (placed.run_index as usize) < first_run_of_effect[effect.0 as usize]
                    && effect_chain_contains(tree, effect_of_media, backdrop_root)
                    && read_tiles.meets(Reach::Within(*reach))
            });
            (cut_by_a_composited_effect || cut_by_media)
                .then_some(WhyEffectIsComposited::ItReadsWhatItsOwnLayerDoesNotHold)
        } else {
            None
        };
        readers_in_paint_order.push(effect);
        if backdrop_root.is_none() {
            readers_of_the_root_group.push(effect);
        }
        let Some(why) = why else {
            continue;
        };
        composited_whatever_its_layer_holds[effect.0 as usize] = Some(why);
        mark_effects_around_as_composited(
            &mut composited_whatever_its_layer_holds,
            reading_node.parent,
            WhyEffectIsComposited::ItHoldsAnEffectTheCompositorComposites,
        );
    }

    // The effects the compositor composites or may, in paint order, each after the one around it.
    let mut placed_effect_of_effect: Vec<Option<usize>> = vec![None; tree.effect_nodes.len()];
    let mut effects: Vec<PlacedEffect> = Vec::new();
    for &effect in &effects_in_paint_order {
        let composited = composited_whatever_its_layer_holds[effect.0 as usize];
        let reads_the_root_group = readers_of_the_root_group.contains(&effect);
        if composited.is_none() && !reads_the_root_group {
            continue;
        }
        let mut around = tree.effect_nodes[effect.0 as usize].parent;
        while !around.is_none() && placed_effect_of_effect[around.0 as usize].is_none() {
            around = tree.effect_nodes[around.0 as usize].parent;
        }
        placed_effect_of_effect[effect.0 as usize] = Some(effects.len());
        effects.push(PlacedEffect {
            effect,
            around: if around.is_none() {
                None
            } else {
                placed_effect_of_effect[around.0 as usize]
            },
            group: group_of_effect(effect),
            region: regions_of_effects[effect.0 as usize],
            read_area: read_areas[effect.0 as usize],
            reads_the_root_group,
            composited_whatever_its_layer_holds: composited,
            filter_spreads_what_it_composites: matches!(&tree.effect_nodes[effect.0 as usize].data,
                EffectNodeData::Effects(effects) if effects.filter.as_deref().is_some_and(|filter| crate::filter_bytes::may_affect_output_bounds(filter))),
        });
    }
    // Per run, the innermost placed effect its records are inside.
    let innermost_placed_effect_of_run = |run_index: usize| {
        let mut effect = list.run(run_index).context.effect;
        while !effect.is_none() {
            if let Some(placed) = placed_effect_of_effect[effect.0 as usize] {
                return Some(placed);
            }
            effect = tree.effect_nodes[effect.0 as usize].parent;
        }
        None
    };

    let mut units: Vec<PlacedUnit> = Vec::with_capacity(groups.len());
    let mut media_of_records = media.iter().enumerate().peekable();
    for (group_index, group) in groups.iter().enumerate() {
        let runs_of_group = group.first_run as usize..=group.last_run as usize;
        for (run_index, &rect_of_run) in rect_of_run
            .iter()
            .enumerate()
            .take(*runs_of_group.end() + 1)
            .skip(*runs_of_group.start())
        {
            let effect = innermost_placed_effect_of_run(run_index);
            let has_media = media_of_records
                .peek()
                .is_some_and(|(_, candidate)| candidate.run_index as usize == run_index);
            let span_of_run = ListSpan {
                begin: list.start_of_run(run_index),
                end: list.end_of_run(run_index),
            };
            if !has_media {
                // Runs inside the same placed effects make one unit, which reaches where that effect can draw.
                let reach = match effect {
                    Some(placed) => effects[placed].region,
                    None => rect_of_run,
                };
                match units.last_mut() {
                    Some(last) if last.group == group_index && last.effect == effect && last.media.is_none() => {
                        last.span.end = span_of_run.end;
                        if effect.is_none() {
                            last.reach = last.reach.united(reach);
                        }
                    }
                    _ => units.push(PlacedUnit {
                        span: span_of_run,
                        group: group_index,
                        reach,
                        media: None,
                        effect,
                    }),
                }
                continue;
            }
            // In a run that draws media, each record is a unit of its own, so content over the media goes above it only
            // where it reaches.
            let geometry = &geometries[group.space];
            let (chunk_index, run_in_chunk) = list.chunk_of_run(run_index);
            let chunk = list.chunk(chunk_index);
            let run = &chunk.runs()[run_in_chunk];
            let record_reach = |bounding_rect: IntRect| {
                if group.moves_within_space {
                    group.device_rect
                } else {
                    geometry.ink_device_rect(run, bounding_rect)
                }
            };
            let table = chunk.record_table();
            let records_of_run = table.records_of_run(run_in_chunk);
            for record in &table.records()[records_of_run.start as usize..records_of_run.end as usize] {
                let span = ListSpan {
                    begin: ListPosition::new(chunk_index, record.offset),
                    end: ListPosition::new(chunk_index, record.end_offset),
                };
                let reach = if record.has_ink {
                    record_reach(record.bounding_rect)
                } else {
                    IntRect::default()
                };
                let media = media_of_records
                    .next_if(|(_, candidate)| candidate.position == span.begin)
                    .map(|(media_index, _)| media_index);
                units.push(PlacedUnit {
                    span,
                    group: group_index,
                    reach,
                    media,
                    effect,
                });
            }
        }
    }

    ListPlacement {
        layering_obstacles: LayeringObstacles::of(list, tree),
        spaces: spaces.to_vec(),
        groups,
        units,
        media,
        rejected_media,
        effects,
        readers_in_paint_order,
    }
}

// A band of a node being worked out: the barriers in it and the units placed in the node, in paint order.
struct BandBeingBuilt {
    node: usize,
    ordinal: u32,
    barriers: Vec<BarrierBeingBuilt>,
    own_units: Vec<usize>,
    first_unit: usize,
}

#[derive(Clone, Copy)]
enum BarrierBeingBuilt {
    Media { unit: usize },
    Band { band: usize },
}

fn expanded_to_tile_grid(rect: IntRect) -> IntRect {
    if rect.is_empty() {
        return rect;
    }
    let tile = TILE_SIZE_IN_DEVICE_PIXELS;
    let left = rect.x.div_euclid(tile) * tile;
    let top = rect.y.div_euclid(tile) * tile;
    let right = (rect.right() - 1).div_euclid(tile) * tile + tile;
    let bottom = (rect.bottom() - 1).div_euclid(tile) * tile + tile;
    IntRect::new(left, top, right - left, bottom - top)
}

struct StackBeingBuilt<'a> {
    placement: &'a ListPlacement,
    nodes: Vec<PlacementNode>,
    node_of_group: Vec<usize>,
    children_of_node: Vec<Vec<usize>>,
    bands: Vec<BandBeingBuilt>,
    layers: Vec<StackLayer>,
    groups: Vec<LayerGroup>,
    // Per band, where content of the root group in it can show so far, in the raster coordinates of its node.
    root_group_reach_of_bands: Vec<Reach<IntRect>>,
    // Per placed effect, once its first unit is placed: why the compositor composites it, if it does, and its node then.
    decisions_of_effects: Vec<Option<Option<WhyEffectIsComposited>>>,
    composited_nodes_of_effects: Vec<Option<usize>>,
    // In paint order.
    composited_effects: Vec<CompositedEffect>,
    // Whether records that blend with what the root group holds below them would read it in other layers.
    records_blend_with_what_other_layers_hold_below_them: bool,
}

impl StackBeingBuilt<'_> {
    fn add_node(&mut self, node: PlacementNode) -> usize {
        let parent = node.parent;
        self.nodes.push(node);
        self.children_of_node.push(Vec::new());
        let index = self.nodes.len() - 1;
        if let Some(parent) = parent {
            self.children_of_node[parent].push(index);
        }
        index
    }

    fn node_of(&mut self, space: usize, composite_clips: Vec<CompositeClip>) -> usize {
        if let Some(node) = self.nodes.iter().position(|node| {
            node.space == space && node.composite_clips == composite_clips && node.composited_effect.is_none()
        }) {
            return node;
        }
        let spaces = &self.placement.spaces;
        let (parent, region_in_parent) = if space == VIEWPORT_RASTER_SPACE {
            (None, Reach::Anywhere)
        } else {
            let parent_space = spaces[space].parent;
            let region = composite_clips
                .iter()
                .find(|clip| clip.space == parent_space)
                .map_or(Reach::Anywhere, |clip| Reach::Within(clip.rect));
            let clips_of_parent = composite_clips
                .iter()
                .filter(|clip| clip.space != parent_space)
                .copied()
                .collect();
            (Some(self.node_of(parent_space, clips_of_parent)), region)
        };
        self.add_node(PlacementNode {
            key: key_of_node(spaces, space, &composite_clips),
            space,
            composite_clips,
            composited_effect: None,
            parent,
            region_in_parent,
        })
    }

    // A node of the space and the clips of `parent`, whose rasters the compositor composites inside `effect`, which
    // shows and reads within `region` of the raster coordinates of the space.
    fn composited_node_inside(&mut self, parent: usize, effect: EffectNodeIndex, region: Reach<IntRect>) -> usize {
        let parent_node = &self.nodes[parent];
        let node = PlacementNode {
            key: key_of_composited_node(parent_node.key, effect),
            space: parent_node.space,
            composite_clips: parent_node.composite_clips.clone(),
            composited_effect: Some(effect),
            parent: Some(parent),
            region_in_parent: region,
        };
        self.add_node(node)
    }

    // The placed effects a unit is inside, outermost first.
    fn chain_of_effects(&self, unit: &PlacedUnit) -> Vec<usize> {
        let mut chain: Vec<usize> =
            std::iter::successors(unit.effect, |&effect| self.placement.effects[effect].around).collect();
        chain.reverse();
        chain
    }

    // Where a unit can show, and where the effects it is inside that the compositor can composite read, as nodes of
    // their own then show there.
    fn reach_of_unit_with_what_it_can_read(&self, unit: &PlacedUnit) -> Reach<IntRect> {
        self.chain_of_effects(unit)
            .into_iter()
            .fold(Reach::Within(unit.reach), |reach, effect| {
                reach.united(self.placement.effects[effect].read_area)
            })
    }

    // Where compositing a placed effect can draw or read, in the raster coordinates of its space.
    fn region_of_composited_effect(&self, effect: usize) -> Reach<IntRect> {
        let placed = &self.placement.effects[effect];
        Reach::Within(placed.region).united(placed.read_area)
    }

    // The content of a held sticky box shows in the space around it wherever the box's offset takes it, so the region of
    // its node there is what the node's units reach swept over the box's offset range, within the clips the node is
    // composited within there.
    fn bound_the_regions_of_held_sticky_boxes(&mut self) {
        let placement = self.placement;
        let mut reach_of_nodes = vec![Reach::Nothing; self.nodes.len()];
        for unit in &placement.units {
            let node = self.node_of_group[unit.group];
            reach_of_nodes[node] = reach_of_nodes[node].united(self.reach_of_unit_with_what_it_can_read(unit));
        }
        for (node, reach) in self.nodes.iter_mut().zip(reach_of_nodes) {
            let space = &placement.spaces[node.space];
            if !matches!(space.anchor_kind, RasterSpaceAnchorKind::HeldStickyBox { .. }) {
                continue;
            }
            let Reach::Within(reach) = reach else {
                continue;
            };
            let swept =
                rect_swept_over_translations(reach, space.lowest_anchor_translation, space.highest_anchor_translation);
            node.region_in_parent = node.region_in_parent.limited_to(swept);
        }
    }

    fn path_to(&self, mut node: usize) -> Vec<usize> {
        let mut path = vec![node];
        while let Some(parent) = self.nodes[node].parent {
            path.push(parent);
            node = parent;
        }
        path.reverse();
        path
    }

    fn open_band(&mut self, node: usize, ordinal: u32, first_unit: usize) -> usize {
        self.bands.push(BandBeingBuilt {
            node,
            ordinal,
            barriers: Vec::new(),
            own_units: Vec::new(),
            first_unit,
        });
        self.root_group_reach_of_bands.push(Reach::Nothing);
        self.bands.len() - 1
    }

    // Whether `reach`, in the raster coordinates of `node`, can meet `tiles`, in those of the last node of
    // `path_of_tiles`, as far as the regions of the nodes on the way to the lowest node the two share tell.
    fn reach_meets_tiles(&self, node: usize, reach: Reach<IntRect>, path_of_tiles: &[usize], tiles: IntRect) -> bool {
        if reach == Reach::Nothing {
            return false;
        }
        let path_of_reach = self.path_to(node);
        let length_of_shared_path = path_of_reach
            .iter()
            .zip(path_of_tiles)
            .take_while(|(first, second)| first == second)
            .count();
        let reach_in_lowest_shared_node = match path_of_reach.get(length_of_shared_path) {
            Some(&child) => self.nodes[child].region_in_parent,
            None => reach,
        };
        let tiles_in_lowest_shared_node = match path_of_tiles.get(length_of_shared_path) {
            Some(&child) => self.nodes[child].region_in_parent,
            None => Reach::Within(tiles),
        };
        reach_in_lowest_shared_node.meets(tiles_in_lowest_shared_node)
    }

    // Whether a backdrop filter of content of `node`, painted next and reading the root group within `read_tiles`, finds
    // all it reads in the layer it goes into, `band_of_reader` being the band of the node open then: no other band holds
    // content of the root group that can show there, and no barrier of its own band cuts those tiles.
    fn reads_only_its_own_layer(&self, node: usize, band_of_reader: Option<usize>, read_tiles: IntRect) -> bool {
        let path_of_reader = self.path_to(node);
        let another_band_reaches_the_tiles = self.bands.iter().enumerate().any(|(band_index, band)| {
            Some(band_index) != band_of_reader
                && self.reach_meets_tiles(
                    band.node,
                    self.root_group_reach_of_bands[band_index],
                    &path_of_reader,
                    read_tiles,
                )
        });
        if another_band_reaches_the_tiles {
            return false;
        }
        let Some(band_of_reader) = band_of_reader else {
            return true;
        };
        self.bands[band_of_reader].barriers.iter().all(|barrier| {
            let region = match *barrier {
                BarrierBeingBuilt::Media { unit } => Reach::Within(self.placement.units[unit].reach),
                BarrierBeingBuilt::Band { band } => self.nodes[self.bands[band].node].region_in_parent,
            };
            !region.touches(read_tiles)
        })
    }

    fn close_band_of(&self, node: usize, open_band_of_node: &mut [Option<usize>]) {
        open_band_of_node[node] = None;
        for &child in &self.children_of_node[node] {
            self.close_band_of(child, open_band_of_node);
        }
    }

    // Why the compositor composites a placed effect, whose first unit is painted next in `node`, if it does: it does
    // where it is composited whatever its layer holds, and where it reads the root group and does not find all it reads
    // in the layer it goes into.
    fn why_effect_is_composited(
        &self,
        effect: &PlacedEffect,
        node: usize,
        band_of_node: Option<usize>,
    ) -> Option<WhyEffectIsComposited> {
        if let Some(why) = effect.composited_whatever_its_layer_holds {
            return Some(why);
        }
        if !effect.reads_the_root_group {
            return None;
        }
        let reads_only_its_own_layer = match effect.read_area {
            Reach::Nothing => true,
            Reach::Within(read_area) => {
                self.reads_only_its_own_layer(node, band_of_node, expanded_to_tile_grid(read_area))
            }
            Reach::Anywhere => false,
        };
        (!reads_only_its_own_layer).then_some(WhyEffectIsComposited::ItReadsWhatItsOwnLayerDoesNotHold)
    }

    // The node a unit goes into: inside the composited nodes of the placed effects around it that the compositor
    // composites, each worked out as its first unit is placed.
    fn node_of_unit(
        &mut self,
        unit: &PlacedUnit,
        open_band_of_node: &mut Vec<Option<usize>>,
        next_band_of_node: &mut Vec<u32>,
    ) -> usize {
        let mut node = self.node_of_group[unit.group];
        for effect in self.chain_of_effects(unit) {
            let decision = match self.decisions_of_effects[effect] {
                Some(decision) => decision,
                None => {
                    let decision =
                        self.why_effect_is_composited(&self.placement.effects[effect], node, open_band_of_node[node]);
                    self.decisions_of_effects[effect] = Some(decision);
                    decision
                }
            };
            let Some(why) = decision else {
                break;
            };
            node = match self.composited_nodes_of_effects[effect] {
                Some(composited_node) => composited_node,
                None => {
                    let placed = self.placement.effects[effect];
                    let region = self.region_of_composited_effect(effect);
                    let composited_node = self.composited_node_inside(node, placed.effect, region);
                    open_band_of_node.push(None);
                    next_band_of_node.push(0);
                    self.composited_nodes_of_effects[effect] = Some(composited_node);
                    self.composited_effects.push(CompositedEffect {
                        effect: placed.effect,
                        node: composited_node,
                        why,
                        read_area: placed.read_area,
                        filter_spreads_what_it_composites: placed.filter_spreads_what_it_composites,
                    });
                    composited_node
                }
            };
        }
        node
    }

    // Walks the units in paint order, giving each node bands within the bands of its parent, and each band its
    // barriers and its own units. The content of an effect that the compositor composites goes into a node of its own,
    // whose band ends wherever content outside the effect is painted, as a replay of the list composites the effect
    // anew there.
    fn cut_into_bands(&mut self) {
        let placement = self.placement;
        let mut open_band_of_node: Vec<Option<usize>> = vec![None; self.nodes.len()];
        let mut next_band_of_node = vec![0u32; self.nodes.len()];
        open_band_of_node[0] = Some(self.open_band(0, 0, 0));
        next_band_of_node[0] = 1;
        for (unit_index, unit) in placement.units.iter().enumerate() {
            if unit.reach.is_empty() {
                continue;
            }
            let node_of_unit = self.node_of_unit(unit, &mut open_band_of_node, &mut next_band_of_node);
            let read_by_records_blending_with_the_root_group =
                placement.groups[unit.group].read_by_records_blending_with_the_root_group;
            if read_by_records_blending_with_the_root_group != Reach::Nothing {
                // What they read below them has to be in their layer wherever they read, as must they be wherever they
                // draw.
                let reads_only_its_own_layer = match read_by_records_blending_with_the_root_group {
                    Reach::Within(read_area) => self.reads_only_its_own_layer(
                        node_of_unit,
                        open_band_of_node[node_of_unit],
                        expanded_to_tile_grid(unit.reach.united(read_area)),
                    ),
                    _ => false,
                };
                self.records_blend_with_what_other_layers_hold_below_them |= !reads_only_its_own_layer;
            }
            let path = self.path_to(node_of_unit);
            for &composited_node in self.composited_nodes_of_effects.iter().flatten() {
                if open_band_of_node[composited_node].is_some() && !path.contains(&composited_node) {
                    self.close_band_of(composited_node, &mut open_band_of_node);
                }
            }
            for level in 0..path.len() {
                let node = path[level];
                let band = open_band_of_node[node].expect("the bands along the path of a unit are open");
                let child_on_path = path.get(level + 1).copied();
                let reach = match child_on_path {
                    Some(child) => self.nodes[child].region_in_parent,
                    None => Reach::Within(unit.reach),
                };
                for &child in &self.children_of_node[node] {
                    if Some(child) == child_on_path || open_band_of_node[child].is_none() {
                        continue;
                    }
                    if reach.meets(self.nodes[child].region_in_parent) {
                        self.close_band_of(child, &mut open_band_of_node);
                    }
                }
                match child_on_path {
                    Some(child) if open_band_of_node[child].is_none() => {
                        let child_band = self.open_band(child, next_band_of_node[child], unit_index);
                        next_band_of_node[child] += 1;
                        open_band_of_node[child] = Some(child_band);
                        self.bands[band]
                            .barriers
                            .push(BarrierBeingBuilt::Band { band: child_band });
                    }
                    Some(_) => {}
                    None if unit.media.is_some() => {
                        self.bands[band]
                            .barriers
                            .push(BarrierBeingBuilt::Media { unit: unit_index });
                    }
                    None => self.bands[band].own_units.push(unit_index),
                }
            }
            if placement.groups[unit.group].paints_within_the_root_group {
                let band = open_band_of_node[node_of_unit].expect("the band of a unit is open once it is placed");
                self.root_group_reach_of_bands[band] =
                    self.root_group_reach_of_bands[band].united(Reach::Within(unit.reach));
            }
        }
    }

    // Pushes the layers of a band and of the bands in it, bottom to top, and the group of its records.
    fn emit_band(&mut self, band_index: usize, group_it_is_a_barrier_in: Option<usize>) -> usize {
        let placement = self.placement;
        let node = self.bands[band_index].node;
        let ordinal = self.bands[band_index].ordinal;
        let node_key = self.nodes[node].key;
        let own_units = std::mem::take(&mut self.bands[band_index].own_units);
        let barriers = std::mem::take(&mut self.bands[band_index].barriers);
        let unit_position = |unit: usize| placement.units[unit].span.begin;

        let group = self.groups.len();
        self.groups.push(LayerGroup {
            node,
            band: ordinal,
            group_it_is_a_barrier_in,
            record_spans: Vec::new(),
            cuts: Vec::new(),
            layer_below_every_cut: self.layers.len(),
        });
        self.layers.push(StackLayer {
            key: LayerKey::Painted {
                node: node_key,
                band: ordinal,
                above: None,
            },
            node,
            group_of_band: group,
            content: LayerContent::Painted { group },
            may_hold_content: false,
        });
        let layer_below_every_cut = self.layers.len() - 1;
        let mut cuts = Vec::with_capacity(barriers.len());
        for barrier in barriers {
            let (key, position, region) = match barrier {
                BarrierBeingBuilt::Media { unit } => {
                    let placed_unit = &placement.units[unit];
                    let media = &placement.media[placed_unit.media.expect("a media barrier is a media unit")];
                    let key = LayerKey::Media {
                        content: media.content,
                        ordinal: media.ordinal,
                    };
                    self.layers.push(StackLayer {
                        key,
                        node,
                        group_of_band: group,
                        content: LayerContent::Media {
                            content: media.content,
                            record_span: placed_unit.span,
                            region: placed_unit.reach,
                            window_server_placement: None,
                        },
                        may_hold_content: true,
                    });
                    (
                        BarrierKey::Media {
                            content: media.content,
                            ordinal: media.ordinal,
                        },
                        unit_position(unit),
                        Reach::Within(placed_unit.reach),
                    )
                }
                BarrierBeingBuilt::Band { band } => {
                    let child_node = self.bands[band].node;
                    let key = BarrierKey::Band {
                        node: self.nodes[child_node].key,
                        band: self.bands[band].ordinal,
                    };
                    let position = unit_position(self.bands[band].first_unit);
                    self.emit_band(band, Some(group));
                    (key, position, self.nodes[child_node].region_in_parent)
                }
            };
            self.layers.push(StackLayer {
                key: LayerKey::Painted {
                    node: node_key,
                    band: ordinal,
                    above: Some(key),
                },
                node,
                group_of_band: group,
                content: LayerContent::Painted { group },
                may_hold_content: false,
            });
            cuts.push(Cut {
                barrier: key,
                position,
                region,
                layer_above: self.layers.len() - 1,
            });
        }

        // Which layers of the band can hold records: in each tile a unit reaches, the one above the last barrier painted
        // before it that cuts the tile. A record draws nothing past the reach of its unit, so what it is replayed into
        // anywhere else stays empty.
        let mut record_spans: Vec<ListSpan> = Vec::with_capacity(own_units.len());
        let group_so_far = LayerGroup {
            node,
            band: ordinal,
            group_it_is_a_barrier_in,
            record_spans: Vec::new(),
            cuts,
            layer_below_every_cut,
        };
        for &unit in &own_units {
            let placed_unit = &placement.units[unit];
            let span = placed_unit.span;
            let position = span.begin;
            let mut mark = |layer: usize| self.layers[layer].may_hold_content = true;
            let cuts_before = group_so_far.cuts.iter().take_while(|cut| cut.position <= position);
            if cuts_before.clone().next().is_none() {
                mark(layer_below_every_cut);
            } else if reaches_too_many_cells_to_follow_one_by_one(placed_unit.reach) {
                mark(layer_below_every_cut);
                for cut in cuts_before {
                    mark(cut.layer_above);
                }
            } else {
                for coord in tile_coords_touching(placed_unit.reach) {
                    mark(group_so_far.layer_at(position, coord.cell_rect()));
                }
            }
            match record_spans.last_mut() {
                Some(last) if last.end == span.begin => last.end = span.end,
                _ => record_spans.push(span),
            }
        }
        let cuts = group_so_far.cuts;
        self.groups[group].record_spans = record_spans;
        self.groups[group].cuts = cuts;
        group
    }
}

// Whether the content of a node covers every pixel its space shows in the viewport, so that its bottom layer can
// replace what is below it.
fn node_covers_the_viewport(nodes: &[PlacementNode], spaces: &[RasterSpace], node: usize) -> bool {
    let placement_node = &nodes[node];
    if !placement_node.composite_clips.is_empty()
        || placement_node.composited_effect.is_some()
        || placement_node.region_in_parent != Reach::Anywhere
    {
        return false;
    }
    match placement_node.parent {
        None => true,
        Some(parent) => {
            spaces[placement_node.space].anchor_kind == RasterSpaceAnchorKind::ViewportScroller
                && node_covers_the_viewport(nodes, spaces, parent)
        }
    }
}

// The stack of a placement, or none where records that blend with what the root group holds below them would read
// what other layers hold, which only a single layer replaying the whole list draws as the list does.
fn assemble_stack(placement: ListPlacement) -> Option<LayerStack> {
    let mut built = StackBeingBuilt {
        placement: &placement,
        nodes: Vec::new(),
        node_of_group: Vec::new(),
        children_of_node: Vec::new(),
        bands: Vec::new(),
        layers: Vec::new(),
        groups: Vec::new(),
        root_group_reach_of_bands: Vec::new(),
        decisions_of_effects: vec![None; placement.effects.len()],
        composited_nodes_of_effects: vec![None; placement.effects.len()],
        composited_effects: Vec::new(),
        records_blend_with_what_other_layers_hold_below_them: false,
    };
    built.node_of(VIEWPORT_RASTER_SPACE, Vec::new());
    built.node_of_group = placement
        .groups
        .iter()
        .map(|group| group.composite_clips.clone())
        .zip(placement.groups.iter().map(|group| group.space))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(clips, space)| built.node_of(space, clips))
        .collect();
    built.bound_the_regions_of_held_sticky_boxes();
    built.cut_into_bands();
    if built.records_blend_with_what_other_layers_hold_below_them {
        return None;
    }
    built.emit_band(0, None);

    let StackBeingBuilt {
        nodes,
        layers,
        groups,
        composited_effects,
        ..
    } = built;
    let effects_reading_in_their_tiles = placement
        .readers_in_paint_order
        .iter()
        .copied()
        .filter(|reader| composited_effects.iter().all(|composited| composited.effect != *reader))
        .collect();

    let first_layer_with_content = layers.iter().position(|layer| layer.may_hold_content);
    let base_layer = match first_layer_with_content {
        Some(index)
            if matches!(layers[index].key, LayerKey::Painted { above: None, .. })
                && node_covers_the_viewport(&nodes, &placement.spaces, layers[index].node) =>
        {
            index
        }
        _ => 0,
    };

    Some(LayerStack {
        spaces: placement.spaces.clone(),
        nodes,
        groups,
        layers,
        composited_effects,
        base_layer,
        unlayered_reason: None,
        layering_obstacles: placement.layering_obstacles,
        rejected_media: placement.rejected_media.clone(),
        effects_reading_in_their_tiles,
        layer_budget: LayerBudget::default(),
        placement,
        placement_at_every_anchor: ListPlacement::default(),
    })
}

fn is_the_anchor_of_the_document(tree: &VisualContextTree, anchor: SpatialNodeIndex) -> bool {
    tree.compositor_scroll_table()
        .nodes
        .iter()
        .any(|record| record.scroll_node_index == anchor && record.is_viewport)
}

/// Works out the layers of a list from where its parts are placed among the spaces at every anchor the tree offers,
/// which `place_list` gave for the spaces at `anchors`. Where the window server can show the media of the stack is left
/// to be worked out for each frame.
///
/// A stack holds at most `MAXIMUM_LAYER_COUNT` layers with content where it can: past that, the list is placed once
/// more without the spaces of nested scrollers and held sticky boxes, which are rastered in the spaces around them,
/// and that stack is kept whatever it holds. The document keeps its space, and an effect the compositor composites keeps
/// its layers, as reading in a layer that does not hold what it reads would draw it wrong.
pub fn build_layer_stack(
    inputs: &LayerStackInputs<'_>,
    anchors: &[SpatialNodeIndex],
    placement_at_every_anchor: ListPlacement,
) -> LayerStack {
    let single_layer = |reason: UnlayeredReason| {
        let mut stack = LayerStack::single_layer(inputs.viewport_rect, inputs.list.whole_span(), Some(reason));
        stack.layering_obstacles = placement_at_every_anchor.layering_obstacles;
        stack.placement_at_every_anchor = placement_at_every_anchor.clone();
        stack
    };
    if let Some(reason) = why_list_is_unlayered(inputs, &placement_at_every_anchor.layering_obstacles) {
        return single_layer(reason);
    }
    let Some(mut stack) = assemble_stack(placement_at_every_anchor.clone()) else {
        return single_layer(UnlayeredReason::RecordsBlendWithWhatOtherLayersHoldBelowThem);
    };
    let layers_with_content = stack.layers_with_content_from_the_base_layer();
    let anchors_of_the_document: Vec<SpatialNodeIndex> = anchors
        .iter()
        .copied()
        .filter(|&anchor| is_the_anchor_of_the_document(inputs.tree, anchor))
        .collect();
    if layers_with_content > MAXIMUM_LAYER_COUNT && anchors_of_the_document.len() < anchors.len() {
        let (spaces, geometries) = spaces_with_geometries(
            inputs.tree,
            &anchors_of_the_document,
            inputs.scroll_offsets,
            inputs.viewport_rect,
            inputs.raster_transform,
        );
        let Some(placed_again) = assemble_stack(place_list(inputs, spaces, &geometries)) else {
            return single_layer(UnlayeredReason::RecordsBlendWithWhatOtherLayersHoldBelowThem);
        };
        stack = placed_again;
        stack.layer_budget.spaces_gave_way = true;
    }
    stack.layer_budget.layers_with_content = stack.layers_with_content_from_the_base_layer();
    stack.placement_at_every_anchor = placement_at_every_anchor;
    stack
}

/// Works out the layers of a list from scratch, and where the window server can show their media, as a dump or a test
/// does.
pub fn layer_stack_of(inputs: &LayerStackInputs<'_>) -> LayerStack {
    let anchors = raster_space_anchors(
        inputs.tree,
        inputs.scroll_offsets,
        &spatial_nodes_that_move_on_their_own(inputs.tree),
    );
    let (spaces, geometries) = spaces_with_geometries(
        inputs.tree,
        &anchors,
        inputs.scroll_offsets,
        inputs.viewport_rect,
        inputs.raster_transform,
    );
    let placement = place_list(inputs, spaces, &geometries);
    let mut stack = build_layer_stack(inputs, &anchors, placement);
    stack.update_window_server_placements(inputs.list, inputs.tree, inputs.scroll_offsets, inputs.viewport_rect);
    stack
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{
        CanvasId, ClipNodeIndex, ContextRef, DisplayListCommand, DrawCanvas, FillRect, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_context::{
        CompositorScrollNodeRecord, CompositorScrollTable, EffectsData, ScrollData, TransformData, TransformDataRole,
    };
    use libgfx_rust::{Color, CompositingAndBlendingOperator, FloatRect, ScalingMode};

    const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);

    fn identity_tree() -> VisualContextTree {
        VisualContextTree::create(TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
            bounds: libgfx_rust::FloatRect::default(),
        })
    }

    fn effects(opacity: f32, blend_mode: CompositingAndBlendingOperator) -> EffectNodeData {
        EffectNodeData::Effects(EffectsData {
            isolation: false,
            opacity,
            blend_mode,
            filter: None,
            backdrop_filter: None,
        })
    }

    fn tree_with_root_isolation_effect() -> VisualContextTree {
        let mut tree = identity_tree();
        let root = tree.append_effect(
            effects(1.0, CompositingAndBlendingOperator::Normal),
            EffectNodeIndex::NONE,
            VISUAL_VIEWPORT_NODE_INDEX,
            ClipNodeIndex::NONE,
        );
        tree.root_isolation_effect = Some(root);
        tree
    }

    fn root_context(tree: &VisualContextTree) -> ContextRef {
        ContextRef {
            effect: tree.root_isolation_effect.unwrap(),
            ..ContextRef::default()
        }
    }

    fn fill(rect: IntRect) -> FillRect {
        FillRect {
            rect,
            color: Color::from_rgb(0, 128, 0),
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    fn canvas(rect: IntRect) -> DrawCanvas {
        DrawCanvas {
            dst_rect: rect,
            canvas_id: CanvasId(1),
            scaling_mode: ScalingMode::NearestNeighbor,
        }
    }

    struct TestList {
        builder: DisplayListBuilder,
    }

    impl TestList {
        fn new() -> Self {
            Self {
                builder: DisplayListBuilder::new(),
            }
        }

        fn with<C: DisplayListCommand>(mut self, command: C, context: ContextRef) -> Self {
            self.builder.append(&command, &[], context);
            self
        }

        fn stack(&self, tree: &VisualContextTree, scroll_offsets: &[FloatPoint]) -> LayerStack {
            layer_stack_of(&LayerStackInputs {
                list: &DisplayListStorage::of_builder(&self.builder),
                tree,
                scroll_offsets,
                viewport_rect: VIEWPORT,
                raster_transform: FloatMatrix4x4::identity(),
            })
        }
    }

    // The layers that can hold something, from the base layer up, each as its kind and what it sits above.
    fn shape(stack: &LayerStack) -> Vec<String> {
        stack
            .layers
            .iter()
            .enumerate()
            .filter(|(index, layer)| {
                *index == stack.base_layer || (*index > stack.base_layer && layer.may_hold_content)
            })
            .map(|(index, layer)| {
                let space = stack.name_of_space(stack.nodes[layer.node].space);
                match layer.key {
                    LayerKey::Painted { band, above, .. } => {
                        let kind = if index == stack.base_layer { "base" } else { "painted" };
                        let above = above.map_or(String::new(), |above| {
                            format!(" above {}", stack.name_of_barrier(&above))
                        });
                        format!("{kind} {space} band {band}{above}")
                    }
                    LayerKey::Media { content, ordinal } => format!("{} #{ordinal} {space}", media_name(content)),
                }
            })
            .collect()
    }

    fn tree_with_document_scroll() -> (VisualContextTree, ContextRef, ContextRef) {
        let mut tree = tree_with_root_isolation_effect();
        let document_scroll_node = tree.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            VISUAL_VIEWPORT_NODE_INDEX,
        );
        assert!(
            tree.set_compositor_scroll_table(CompositorScrollTable {
                nodes: vec![CompositorScrollNodeRecord {
                    scroll_node_index: document_scroll_node,
                    parent_scroll_node_index: VISUAL_VIEWPORT_NODE_INDEX,
                    scrollport_rect: VIEWPORT,
                    max_scroll_offset: FloatPoint { x: 0.0, y: 2000.0 },
                    is_viewport: true,
                    ..CompositorScrollNodeRecord::default()
                }]
                .into(),
                ..CompositorScrollTable::default()
            })
        );
        let viewport = root_context(&tree);
        let document = ContextRef {
            spatial: document_scroll_node,
            ..viewport
        };
        (tree, viewport, document)
    }

    #[test]
    fn samples_of_a_transform_animation_leave_the_placement_alike() {
        use crate::animation_timing::{AnimationEffectTiming, TimeValue};
        use crate::host::{FfiVisualAnimationTargetKind, FfiVisualAnimationTransformOperationKind};
        use crate::visual_animation::{
            VisualAnimation, VisualAnimationKeyframe, VisualAnimationTransformOperation, VisualAnimationValue,
        };
        let (mut tree, _, document) = tree_with_document_scroll();
        let moving_node = tree.append_spatial(
            SpatialData::Transform(TransformData {
                matrix: FloatMatrix4x4::identity(),
                origin: FloatPoint::default(),
                sorting_context_root_index: None,
                flattens_inherited_transform: false,
                role: TransformDataRole::CssTransform,
                synthetic_plane: false,
                establishes_sorting_context: false,
                bounds: FloatRect::new(0.0, 0.0, 50.0, 50.0),
            }),
            document.spatial,
        );
        let translate_x = |pixels: f32| VisualAnimationKeyframe {
            offset: f64::from(pixels) / 400.0,
            easing: crate::easing::Easing::default(),
            value: VisualAnimationValue::Transform(vec![
                VisualAnimationTransformOperation::new(FfiVisualAnimationTransformOperationKind::TranslateX, &[pixels])
                    .unwrap(),
            ]),
        };
        tree.set_visual_animations(vec![VisualAnimation {
            target_kind: FfiVisualAnimationTargetKind::Transform,
            node_indices: vec![moving_node.0],
            timing: AnimationEffectTiming {
                iteration_duration: TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            keyframes: vec![translate_x(0.0), translate_x(400.0)],
            ..VisualAnimation::default()
        }]);
        let moving = ContextRef {
            spatial: moving_node,
            ..document
        };
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(500, 300, 200, 100)), document)
            .with(fill(IntRect::new(100, 300, 50, 50)), moving);
        let at_start = list.stack(&tree.with_visual_animation_samples(0), &[]);
        let halfway = list.stack(&tree.with_visual_animation_samples(500_000_000), &[]);
        assert_eq!(at_start.placement_at_every_anchor, halfway.placement_at_every_anchor);
        // Wherever the animation takes the box, it can go over the canvas, so it sits above it.
        assert_eq!(
            shape(&halfway),
            [
                "base document band 0",
                "canvas #0 document",
                "painted document band 0 above L2"
            ]
        );
    }
}
