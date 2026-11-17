/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a display list is painted in, bottom to top.
//!
//! Content is rastered in the space of the scroll node or held sticky box it moves with, and its raster is composited
//! within the clips it leaves to apply then: content of one space and one set of such clips makes a placement node.
//! Within a node, a canvas, video or nested context that is a layer of its own, a backdrop filter that filters what is
//! composited below it, and each band of content of a node inside it, sits between the node's records as a barrier. A
//! barrier cuts the tiles of the node it can show in, or for a backdrop filter that it reads, at its paint position:
//! there, the node's records painted before it go into the node's layer below it and those painted after it into the
//! node's layer above it. Elsewhere it cuts nothing. A band of a node inside ends where content outside it that can
//! show where the band can is painted, so the layers stacked in paint order of their barriers paint every pixel in the
//! order the list does, as long as nothing within a layer reads what is below the layer.

use crate::display_list::builder::read_command;
use crate::display_list::commands::{DeclareMaskContent, DisplayListCommandType, EffectNodeIndex, SpatialNodeIndex};
use crate::display_list::effect_geometry::{EffectGeometry, compute_effect_geometry};
pub use crate::display_list::placement::{
    CompositeClip, RasterSpace, RasterSpaceAnchorKind, VIEWPORT_RASTER_SPACE, raster_space_anchors,
};
use crate::display_list::placement::{
    PlacementTables, RasterSpaces, RunGroups, RunsReachingOutsideTheirContext, SpaceGeometry, chain_of_space,
    is_finite_rect, lowest_common_space, raster_scroll_offsets, rect_swept_over_translations,
    space_is_ancestor_or_self, spaces_with_geometries, spatial_nodes_that_move_on_their_own,
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
// Each one is a backdrop layer and a painted layer above it, so four take a third of the most layers a stack keeps.
pub const MAXIMUM_BACKDROP_LAYER_COUNT: usize = 4;
// Beyond this, the innermost space that scrolls is rastered in the space around it instead.
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
    InsideGroupEffect,
    UnderTransformThatIsNot2DAffine,
    DrawsNothingVisible,
    BetweenRunsOfOneGroupEffect,
    UnderContentReadingTheRootGroup,
    MoreMediaThanLayersFor,
}

impl std::fmt::Display for MediaLayerRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            MediaLayerRejection::DrawnInsideAnotherCommand => "drawn inside another command",
            MediaLayerRejection::InsideGroupEffect => "inside a group effect",
            MediaLayerRejection::UnderTransformThatIsNot2DAffine => "under a transform that is not 2D affine",
            MediaLayerRejection::DrawsNothingVisible => "draws nothing visible",
            MediaLayerRejection::BetweenRunsOfOneGroupEffect => "between the runs of one group effect",
            MediaLayerRejection::UnderContentReadingTheRootGroup => "under content that reads the root group below it",
            MediaLayerRejection::MoreMediaThanLayersFor => "past the most media that get layers",
        })
    }
}

/// Why a backdrop filter that reads the root group is applied within the layer of what it reads rather than by a layer
/// of its own, which filters what is composited below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropLayerRejection {
    OpacityGroupsTheBackdropWithTheContent,
    BlendModeGroupsTheBackdropWithTheContent,
    FilterGroupsTheBackdropWithTheContent,
    MasksApplyToTheBackdrop,
    ContentReadsTheFilteredBackdrop,
    InsideGroupEffect,
    ContentIsNotOneStretchStartingWithTheBackdrop,
    CompositedWithinClips,
    RegionIsNotAnAxisAlignedRect,
    ReachOfFilterIsNotKnown,
    UnderContentReadingTheRootGroup,
    MoreBackdropFiltersThanLayersFor,
}

impl std::fmt::Display for BackdropLayerRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            BackdropLayerRejection::OpacityGroupsTheBackdropWithTheContent => {
                "its opacity composites the filtered backdrop and the content as one"
            }
            BackdropLayerRejection::BlendModeGroupsTheBackdropWithTheContent => {
                "its blend mode composites the filtered backdrop and the content as one"
            }
            BackdropLayerRejection::FilterGroupsTheBackdropWithTheContent => {
                "its filter applies to the filtered backdrop and the content as one"
            }
            BackdropLayerRejection::MasksApplyToTheBackdrop => "its masks apply to the filtered backdrop",
            BackdropLayerRejection::ContentReadsTheFilteredBackdrop => "content in it reads the filtered backdrop",
            BackdropLayerRejection::InsideGroupEffect => "inside a group effect",
            BackdropLayerRejection::ContentIsNotOneStretchStartingWithTheBackdrop => {
                "its content is not one stretch of the list that starts where it reads the backdrop"
            }
            BackdropLayerRejection::CompositedWithinClips => "composited within clips of another space",
            BackdropLayerRejection::RegionIsNotAnAxisAlignedRect => {
                "its region is not an axis-aligned rect in its raster space"
            }
            BackdropLayerRejection::ReachOfFilterIsNotKnown => "how far its filter reads is not known",
            BackdropLayerRejection::UnderContentReadingTheRootGroup => {
                "under content that reads the root group below it"
            }
            BackdropLayerRejection::MoreBackdropFiltersThanLayersFor => {
                "past the most backdrop filters that get layers"
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
        })
    }
}

/// Names a placement node by the identity of its space and the clips its rasters are composited within, which outlive
/// the tree and the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlacementKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarrierKey {
    /// The `ordinal`th drawing of a canvas, video or nested context in paint order.
    Media { content: LiveContent, ordinal: u32 },
    /// The `band`th band of content of a node, counted over the whole list.
    Band { node: PlacementKey, band: u32 },
    /// The `ordinal`th backdrop filter of a node that has a layer of its own, counted over the whole list.
    Backdrop { node: PlacementKey, ordinal: u32 },
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
    Backdrop {
        node: PlacementKey,
        ordinal: u32,
    },
}

/// Content rastered in one space and composited within one set of clips.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementNode {
    pub key: PlacementKey,
    pub space: usize,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    pub parent: Option<usize>,
    /// Where content of the node and of the nodes in it can show, in the raster coordinates of the parent node.
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
    /// The backdrop filter of an effect, which filters what is composited below the layer when the record that marks
    /// where the effect reads its backdrop is replayed over it. The effect's content goes into the layers above it.
    Backdrop {
        effect: EffectNodeIndex,
        marker_span: ListSpan,
        /// What the filter reads and draws, in the raster coordinates of the node's space.
        read_area: IntRect,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackLayer {
    pub key: LayerKey,
    pub node: usize,
    pub content: LayerContent,
    /// Whether any record can go into the layer anywhere; a layer that holds nothing has no tiles.
    pub may_hold_content: bool,
    /// Whether a record painted within the root group, which what reads the root group below it reads, can go into
    /// the layer anywhere.
    pub may_hold_root_group_content: bool,
}

/// The layers of a list bottom to top, with the base layer, which is rastered over the surface's clear color and
/// replaces what is below it. Layers below the base layer hold nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerStack {
    pub spaces: Vec<RasterSpace>,
    pub nodes: Vec<PlacementNode>,
    pub groups: Vec<LayerGroup>,
    pub layers: Vec<StackLayer>,
    pub base_layer: usize,
    /// Why the stack is one layer that replays the whole list, if it is.
    pub unlayered_reason: Option<UnlayeredReason>,
    pub layering_obstacles: LayeringObstacles,
    /// The canvases, videos and nested contexts that were not given layers, and why.
    pub rejected_media: Vec<(LiveContent, MediaLayerRejection)>,
    /// The effects whose backdrop filter reads the root group and is applied within the layer of what it reads, and
    /// why.
    pub rejected_backdrop_filters: Vec<(EffectNodeIndex, BackdropLayerRejection)>,
    /// Where the parts of the list are placed among the spaces of the stack.
    pub placement: ListPlacement,
    /// Where they are placed at every anchor the tree offers, which the stack was worked out from: a later frame
    /// whose placement there compares equal keeps the stack.
    pub placement_at_every_anchor: ListPlacement,
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

impl LayerStack {
    /// One layer that replays the whole list, which is always correct.
    pub fn single_layer(viewport_rect: IntRect, whole_list: ListSpan, reason: Option<UnlayeredReason>) -> Self {
        let spaces = vec![RasterSpace::of_the_viewport(viewport_rect)];
        let root = PlacementNode {
            key: key_of_node(&spaces, VIEWPORT_RASTER_SPACE, &[]),
            space: VIEWPORT_RASTER_SPACE,
            composite_clips: Vec::new(),
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
                content: LayerContent::Painted { group: 0 },
                may_hold_content: true,
                may_hold_root_group_content: true,
            }],
            groups: vec![LayerGroup {
                node: 0,
                band: 0,
                record_spans: vec![whole_list],
                cuts: Vec::new(),
                layer_below_every_cut: 0,
            }],
            nodes: vec![root],
            spaces,
            base_layer: 0,
            unlayered_reason: reason,
            layering_obstacles: LayeringObstacles::default(),
            rejected_media: Vec::new(),
            rejected_backdrop_filters: Vec::new(),
            placement: ListPlacement::default(),
            placement_at_every_anchor: ListPlacement::default(),
        }
    }

    pub fn replays_whole_list(&self) -> bool {
        self.unlayered_reason.is_some()
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
                && layer.may_hold_content == other.may_hold_content
                && layer.may_hold_root_group_content == other.may_hold_root_group_content
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
                    (
                        LayerContent::Backdrop { effect, read_area, .. },
                        LayerContent::Backdrop {
                            effect: other_effect,
                            read_area: other_read_area,
                            ..
                        },
                    ) => effect == other_effect && read_area == other_read_area,
                    _ => false,
                }
        };
        let group_is_alike = |group: &LayerGroup, other: &LayerGroup| {
            group.node == other.node
                && group.band == other.band
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
        self.nodes[self.layers[layer].node]
            .composite_clips
            .iter()
            .fold(None, |clip_rect, clip| {
                let rect = translated(clip.rect, composite_offsets[clip.space]);
                Some(clip_rect.map_or(rect, |clip_rect: IntRect| clip_rect.intersected(rect)))
            })
    }

    pub fn has_media_layers(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| matches!(layer.content, LayerContent::Media { .. }))
    }

    /// Whether a layer of the stack filters what is composited below it, which then has to be composited by the
    /// compositor rather than shown above the frame by the window server.
    pub fn has_backdrop_layers(&self) -> bool {
        self.layers[self.base_layer..]
            .iter()
            .any(|layer| matches!(layer.content, LayerContent::Backdrop { .. }))
    }

    /// The effects whose backdrop filter a layer of the stack applies, which rasters of the effects' content leave out.
    pub fn backdrop_filters_applied_by_layers(&self) -> Vec<EffectNodeIndex> {
        self.layers
            .iter()
            .filter_map(|layer| match layer.content {
                LayerContent::Backdrop { effect, .. } => Some(effect),
                _ => None,
            })
            .collect()
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

    fn name_of_barrier(&self, barrier: &BarrierKey) -> String {
        let name_of_barrier_layer = |key: LayerKey| {
            let layer = self
                .layers
                .iter()
                .position(|layer| layer.key == key)
                .expect("a media or backdrop barrier is a layer of the stack");
            format!("L{layer}")
        };
        match barrier {
            BarrierKey::Media { content, ordinal } => name_of_barrier_layer(LayerKey::Media {
                content: *content,
                ordinal: *ordinal,
            }),
            BarrierKey::Backdrop { node, ordinal } => name_of_barrier_layer(LayerKey::Backdrop {
                node: *node,
                ordinal: *ordinal,
            }),
            BarrierKey::Band { node, band } => {
                let node = self
                    .nodes
                    .iter()
                    .find(|candidate| candidate.key == *node)
                    .expect("a band barrier names a node of the stack");
                format!("band {band} of {}", self.name_of_space(node.space))
            }
        }
    }

    /// What the dump calls a layer: its index, what it holds, the space it is rastered in, and what it sits above.
    pub fn name_of_layer(&self, index: usize) -> String {
        if self.unlayered_reason.is_some() {
            return "[L0] single layer replaying the whole list".into();
        }
        let layer = &self.layers[index];
        let space = self.name_of_space(self.nodes[layer.node].space);
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
            (LayerKey::Backdrop { ordinal, .. }, LayerContent::Backdrop { effect, read_area, .. }) => format!(
                "[L{index}] backdrop #{ordinal} of e{} space={space} reads=[{},{} {}x{}]",
                effect.0, read_area.x, read_area.y, read_area.width, read_area.height
            ),
            (LayerKey::Media { .. } | LayerKey::Backdrop { .. }, _) => {
                unreachable!("a media or backdrop key names a layer of its kind")
            }
        }
    }

    /// What the dump says of a backdrop filter that reads the root group and has no layer of its own, and why.
    pub fn names_of_backdrop_filters_without_layers(&self) -> impl Iterator<Item = String> + '_ {
        self.rejected_backdrop_filters
            .iter()
            .map(|(effect, rejection)| format!("no backdrop layer for e{}: {rejection}", effect.0))
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
                } if self.has_backdrop_layers() => writeln!(
                    output,
                    " replayed: a backdrop layer filters what is composited below it"
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
            .names_of_media_without_layers()
            .chain(self.names_of_backdrop_filters_without_layers())
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
            let (LayerContent::Media {
                record_span: barrier_span,
                ..
            }
            | LayerContent::Backdrop {
                marker_span: barrier_span,
                ..
            }) = &layer.content
            else {
                continue;
            };
            if barrier_span.begin == position {
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

// Per effect node, whether an effect on its chain blends with the root group, which it reads below it to do so.
fn blend_reading_root_group_on_chain(tree: &VisualContextTree) -> Vec<bool> {
    let mut on_chain = vec![false; tree.effect_nodes.len()];
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let own = matches!(&node.data, EffectNodeData::Effects(effects)
            if effects.blend_mode != libgfx_rust::CompositingAndBlendingOperator::Normal)
            && tree.effect_backdrop_is_root_group(EffectNodeIndex(index as u32));
        on_chain[index] = own || (!node.parent.is_none() && on_chain[node.parent.0 as usize]);
    });
    on_chain
}

// A chain holds at most one such effect, since the layer of its backdrop filter isolates every effect under it.
fn backdrop_filter_reading_root_group_on_chain(tree: &VisualContextTree) -> Vec<EffectNodeIndex> {
    let mut on_chain = vec![EffectNodeIndex::NONE; tree.effect_nodes.len()];
    tree.visit_effect_nodes_parents_first(|index| {
        let node = &tree.effect_nodes[index];
        let effect = EffectNodeIndex(index as u32);
        let own = matches!(&node.data, EffectNodeData::Effects(effects) if effects.backdrop_filter.is_some())
            && tree.effect_backdrop_is_root_group(effect);
        on_chain[index] = if own {
            effect
        } else if node.parent.is_none() {
            EffectNodeIndex::NONE
        } else {
            on_chain[node.parent.0 as usize]
        };
    });
    on_chain
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
    /// Where the backdrop filters of the group that read the root group read, in the space's raster coordinates.
    pub root_group_backdrop_read: Reach<IntRect>,
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

/// A stretch of records of one group that is placed as one: the whole group, or the records of a run between the
/// canvases, videos and nested contexts that are layers of their own, or one of those, or the record that marks where
/// an effect with a backdrop layer reads its backdrop, or the rest of that effect's records.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedUnit {
    pub span: ListSpan,
    pub group: usize,
    pub reach: IntRect,
    pub media: Option<usize>,
    pub backdrop: Option<usize>,
}

/// A backdrop filter that reads the root group and gets a layer of its own, placed with the group of its effect, which
/// holds the effect's records and nothing else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedBackdrop {
    pub effect: EffectNodeIndex,
    pub group: usize,
    /// The record that marks where the effect reads its backdrop, the first of its group.
    pub marker_span: ListSpan,
    /// What the filter reads and draws, in the raster coordinates of the group's space, wherever the effect can show if
    /// it moves within its space.
    pub read_area: IntRect,
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
    /// In paint order.
    pub backdrops: Vec<PlacedBackdrop>,
    pub rejected_backdrop_filters: Vec<(EffectNodeIndex, BackdropLayerRejection)>,
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
    let run_groups = inputs.list.run_groups(inputs.tree);
    if !run_groups.grouping_effect_of(run.context.effect).is_none() {
        return Some(MediaLayerRejection::InsideGroupEffect);
    }
    if !geometry.world.draw_matrix(run.context.spatial).is_2d_affine() {
        return Some(MediaLayerRejection::UnderTransformThatIsNot2DAffine);
    }
    // What moves on its own may show wherever its fixed clips let it, which its group's rect already says.
    if !moves_within_space && geometry.ink_device_rect(run, record.bounding_rect).is_empty() {
        return Some(MediaLayerRejection::DrawsNothingVisible);
    }
    if !run_groups.run_is_a_group_of_its_own(run_index as usize) {
        return Some(MediaLayerRejection::BetweenRunsOfOneGroupEffect);
    }
    None
}

// Why the backdrop filter of `reader`, which reads the root group, cannot be applied by a layer of its own between what
// is composited below it and the content of its effect, or where that layer goes. Applying the filter to what is below
// and then compositing the content over it gives the pixels of a replay only where nothing composites the filtered
// backdrop and the content as one, and where the effect's records are one group that starts with the record that marks
// where the replay reads the backdrop.
#[allow(clippy::too_many_arguments)]
fn backdrop_layer_of_reader(
    inputs: &LayerStackInputs<'_>,
    run_groups: &RunGroups,
    groups: &[PlacedRunGroup],
    geometries: &[Rc<SpaceGeometry>],
    runs_with_destination_blending: &[bool],
    reader: EffectNodeIndex,
    runs_under_reader: &[usize],
) -> Result<PlacedBackdrop, BackdropLayerRejection> {
    let tree = inputs.tree;
    let list = inputs.list;
    let node = &tree.effect_nodes[reader.0 as usize];
    let EffectNodeData::Effects(effects) = &node.data else {
        unreachable!("a backdrop filter belongs to an effects node");
    };
    let backdrop_filter = effects
        .backdrop_filter
        .as_ref()
        .expect("a reader of the root group's backdrop has a backdrop filter");
    if effects.opacity < 1.0 {
        return Err(BackdropLayerRejection::OpacityGroupsTheBackdropWithTheContent);
    }
    if effects.blend_mode != libgfx_rust::CompositingAndBlendingOperator::Normal {
        return Err(BackdropLayerRejection::BlendModeGroupsTheBackdropWithTheContent);
    }
    if effects.filter.is_some() {
        return Err(BackdropLayerRejection::FilterGroupsTheBackdropWithTheContent);
    }
    if !node.parent.is_none()
        && matches!(&tree.effect_nodes[node.parent.0 as usize].data, EffectNodeData::Mask(mask) if mask.masks_backdrop)
    {
        return Err(BackdropLayerRejection::MasksApplyToTheBackdrop);
    }
    if run_groups.grouping_effect_of(reader) != reader {
        return Err(BackdropLayerRejection::InsideGroupEffect);
    }
    // A replay of the effect draws its content over the filtered backdrop without a layer between them, so whatever in
    // it reads what is below it reads the filtered backdrop.
    let reads_the_filtered_backdrop = runs_under_reader.iter().any(|&run_index| {
        if runs_with_destination_blending[run_index] {
            return true;
        }
        let mut effect = list.run(run_index).context.effect;
        while effect != reader {
            let node = &tree.effect_nodes[effect.0 as usize];
            if matches!(&node.data, EffectNodeData::Effects(effects)
                if effects.blend_mode != libgfx_rust::CompositingAndBlendingOperator::Normal
                    || effects.backdrop_filter.is_some())
            {
                return true;
            }
            effect = node.parent;
        }
        false
    });
    if reads_the_filtered_backdrop {
        return Err(BackdropLayerRejection::ContentReadsTheFilteredBackdrop);
    }
    let (Some(&first_run), Some(&last_run)) = (runs_under_reader.first(), runs_under_reader.last()) else {
        unreachable!("a reader of the root group's backdrop has runs");
    };
    let group_index = run_groups.group_of_run(first_run);
    let (chunk, run_in_chunk) = list.chunk_of_run(first_run);
    let table = list.chunk(chunk).record_table();
    // Compositor metadata, which draws nothing, may come before the marker: a box records where it takes the wheel
    // ahead of what it paints.
    let marker = table
        .records_of_run(run_in_chunk)
        .map(|record| &table.records()[record as usize])
        .find(|record| !record.command_type.is_compositor_metadata());
    let is_one_stretch_starting_with_the_marker = last_run - first_run + 1 == runs_under_reader.len()
        && run_groups.ranges()[group_index] == (first_run as u32, last_run as u32)
        && marker.is_some_and(|marker| marker.command_type == DisplayListCommandType::BackdropFilterRegion);
    if !is_one_stretch_starting_with_the_marker {
        return Err(BackdropLayerRejection::ContentIsNotOneStretchStartingWithTheBackdrop);
    }
    let marker = marker.expect("the stretch starts with the marker");
    let group = &groups[group_index];
    if !group.composite_clips.is_empty() {
        return Err(BackdropLayerRejection::CompositedWithinClips);
    }
    let geometry = &geometries[group.space];
    let effect_to_device = geometry.world.draw_matrix(node.spatial);
    let maps_to_an_axis_aligned_rect = effect_to_device.is_2d_affine() && {
        let affine = effect_to_device.extract_2d_affine();
        affine.b() == 0.0 && affine.c() == 0.0
    };
    if !maps_to_an_axis_aligned_rect {
        return Err(BackdropLayerRejection::RegionIsNotAnAxisAlignedRect);
    }
    // What moves within its space can read wherever its fixed clips let it show.
    let read_area = if group.moves_within_space {
        crate::display_list::tile_keys::backdrop_filter_read_reach_in_device_pixels(
            &backdrop_filter.filter,
            &effect_to_device,
        )
        .map(|reach| group.device_rect.inflated_edges(reach, reach, reach, reach))
    } else {
        match backdrop_filter_read_area(tree, geometry, reader) {
            Reach::Within(read_area) => Some(read_area),
            Reach::Nothing | Reach::Anywhere => None,
        }
    };
    let Some(read_area) = read_area else {
        return Err(BackdropLayerRejection::ReachOfFilterIsNotKnown);
    };
    Ok(PlacedBackdrop {
        effect: reader,
        group: group_index,
        marker_span: ListSpan {
            begin: ListPosition::new(chunk, marker.offset),
            end: ListPosition::new(chunk, marker.end_offset),
        },
        read_area,
    })
}

/// Where each part of the list is placed among `spaces`, given the geometry of each of them, leaving out of the media
/// layers the drawings at `positions_refused_media_layers`, and out of the backdrop layers the backdrop filters of
/// `effects_refused_backdrop_layers`.
pub fn place_list(
    inputs: &LayerStackInputs<'_>,
    spaces: RasterSpaces,
    geometries: &[Rc<SpaceGeometry>],
    positions_refused_media_layers: &[ListPosition],
    effects_refused_backdrop_layers: &[EffectNodeIndex],
) -> ListPlacement {
    let tree = inputs.tree;
    let list = inputs.list;
    let effect_clips = list.effect_clip_plan(tree);
    let run_groups = list.run_groups(tree);
    let tables = PlacementTables::new(tree, spaces, geometries);
    let spaces = tables.spaces();
    let runs_reaching_outside = RunsReachingOutsideTheirContext::new(list);
    let backdrop_filter_reading_root_group_on_chain = backdrop_filter_reading_root_group_on_chain(tree);
    let blend_reading_root_group_on_chain = blend_reading_root_group_on_chain(tree);
    let mut runs_blending_with_root_group = vec![false; list.run_count()];
    for record in &list.summary().destination_blending_records {
        let effect = list.run(record.run_index as usize).context.effect;
        if tree.content_under_effect_composites_into_root_group(effect) {
            runs_blending_with_root_group[record.run_index as usize] = true;
        }
    }
    let mut runs_with_destination_blending = vec![false; list.run_count()];
    for record in &list.summary().destination_blending_records {
        runs_with_destination_blending[record.run_index as usize] = true;
    }
    let mut effect_geometry_of_spaces: Vec<Option<Vec<EffectGeometry>>> = vec![None; spaces.len()];
    let mut group_rects: std::collections::HashMap<(usize, u32), IntRect> = std::collections::HashMap::new();

    // Per group, the backdrop filters on the chains of its runs that read the root group, which are read in the group's
    // layer unless they get layers of their own.
    let (mut groups, backdrop_readers_of_groups): (Vec<PlacedRunGroup>, Vec<Vec<EffectNodeIndex>>) = run_groups
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
            let mut root_group_backdrop_read = Reach::Nothing;
            let mut backdrop_readers = Vec::new();
            for run_index in runs {
                let run = list.run(run_index);
                let group = run_groups.grouping_effect_of_run(run_index);
                let run_rect = if moves_within_space {
                    geometry.device_rect(None, run.context.clip)
                } else if group.is_none() {
                    geometry.ink_device_rect(run, run.ink_bounds)
                } else {
                    *group_rects.entry((space, group.0)).or_insert_with(|| {
                        let effect_geometry = effect_geometry_of_spaces[space].get_or_insert_with(|| {
                            let mut effect_geometry = Vec::new();
                            compute_effect_geometry(tree, list.runs(), &geometry.world, &mut effect_geometry);
                            effect_geometry
                        });
                        geometry.group_device_rect(tree, group, effect_geometry[group.0 as usize].output, effect_clips)
                    })
                };
                device_rect = device_rect.united(run_rect);
                // A blend reads below what it draws.
                let blends_with_root_group = runs_blending_with_root_group[run_index]
                    || (!run.context.effect.is_none()
                        && blend_reading_root_group_on_chain[run.context.effect.0 as usize]);
                if blends_with_root_group {
                    root_group_backdrop_read = root_group_backdrop_read.united(Reach::Within(run_rect));
                }
                if !run.context.effect.is_none() {
                    let backdrop_filter = backdrop_filter_reading_root_group_on_chain[run.context.effect.0 as usize];
                    if !backdrop_filter.is_none() && !backdrop_readers.contains(&backdrop_filter) {
                        backdrop_readers.push(backdrop_filter);
                    }
                }
            }
            let paints_within_the_root_group = match tree.root_isolation_effect {
                None => true,
                Some(root) => (first_run..=last_run).any(|run_index| {
                    let mut effect = list.run(run_index as usize).context.effect;
                    while !effect.is_none() {
                        if effect == root {
                            return true;
                        }
                        effect = tree.effect_nodes[effect.0 as usize].parent;
                    }
                    false
                }),
            };
            (
                PlacedRunGroup {
                    first_run,
                    last_run,
                    space,
                    composite_clips,
                    device_rect,
                    moves_within_space,
                    paints_within_the_root_group,
                    root_group_backdrop_read,
                },
                backdrop_readers,
            )
        })
        .unzip();

    // The backdrop filters that read the root group get layers of their own where they can, the last ones in paint order
    // once there are more than layers for them: one that reads in the layer of what it reads needs no backdrop layer
    // below it where it reads, so those left out go first.
    let mut runs_under_readers: Vec<(EffectNodeIndex, Vec<usize>)> = Vec::new();
    for run_index in 0..list.run_count() {
        let effect = list.run(run_index).context.effect;
        if effect.is_none() {
            continue;
        }
        let reader = backdrop_filter_reading_root_group_on_chain[effect.0 as usize];
        if reader.is_none() {
            continue;
        }
        match runs_under_readers.iter_mut().find(|(known, _)| *known == reader) {
            Some((_, runs)) => runs.push(run_index),
            None => runs_under_readers.push((reader, vec![run_index])),
        }
    }
    let mut backdrop_layers_of_readers: Vec<(EffectNodeIndex, Result<PlacedBackdrop, BackdropLayerRejection>)> =
        runs_under_readers
            .iter()
            .map(|(reader, runs_under_reader)| {
                let placed = backdrop_layer_of_reader(
                    inputs,
                    run_groups,
                    &groups,
                    geometries,
                    &runs_with_destination_blending,
                    *reader,
                    runs_under_reader,
                )
                .and_then(|placed| {
                    if effects_refused_backdrop_layers.contains(reader) {
                        Err(BackdropLayerRejection::UnderContentReadingTheRootGroup)
                    } else {
                        Ok(placed)
                    }
                });
                (*reader, placed)
            })
            .collect();
    let mut backdrop_layers_past_the_most = backdrop_layers_of_readers
        .iter()
        .filter(|(_, placed)| placed.is_ok())
        .count()
        .saturating_sub(MAXIMUM_BACKDROP_LAYER_COUNT);
    for (_, placed) in &mut backdrop_layers_of_readers {
        if backdrop_layers_past_the_most > 0 && placed.is_ok() {
            *placed = Err(BackdropLayerRejection::MoreBackdropFiltersThanLayersFor);
            backdrop_layers_past_the_most -= 1;
        }
    }
    let mut backdrops: Vec<PlacedBackdrop> = Vec::new();
    let mut rejected_backdrop_filters = Vec::new();
    for (reader, placed) in backdrop_layers_of_readers {
        match placed {
            Ok(placed) => backdrops.push(placed),
            Err(rejection) => rejected_backdrop_filters.push((reader, rejection)),
        }
    }
    for (group, backdrop_readers) in groups.iter_mut().zip(&backdrop_readers_of_groups) {
        let geometry = &geometries[group.space];
        for &reader in backdrop_readers {
            if backdrops.iter().all(|backdrop| backdrop.effect != reader) {
                group.root_group_backdrop_read = group
                    .root_group_backdrop_read
                    .united(backdrop_filter_read_area(tree, geometry, reader));
            }
        }
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
        let rejection = if positions_refused_media_layers.contains(&position) {
            Some(MediaLayerRejection::UnderContentReadingTheRootGroup)
        } else if group.moves_within_space && group.device_rect.is_empty() {
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

    let mut units = Vec::with_capacity(groups.len() + backdrops.len());
    let mut media_of_records = media.iter().enumerate().peekable();
    let mut backdrops_of_groups = backdrops.iter().enumerate().peekable();
    for (group_index, group) in groups.iter().enumerate() {
        let span_of_group = ListSpan {
            begin: list.start_of_run(group.first_run as usize),
            end: list.end_of_run(group.last_run as usize),
        };
        // The marker of a backdrop layer is a barrier, and the rest of its effect's records go above it where it reads.
        if let Some((backdrop_index, backdrop)) =
            backdrops_of_groups.next_if(|(_, backdrop)| backdrop.group == group_index)
        {
            units.push(PlacedUnit {
                span: backdrop.marker_span,
                group: group_index,
                reach: backdrop.read_area,
                media: None,
                backdrop: Some(backdrop_index),
            });
            if backdrop.marker_span.end < span_of_group.end {
                units.push(PlacedUnit {
                    span: ListSpan {
                        begin: backdrop.marker_span.end,
                        end: span_of_group.end,
                    },
                    group: group_index,
                    reach: group.device_rect,
                    media: None,
                    backdrop: None,
                });
            }
            continue;
        }
        let has_media = media_of_records
            .peek()
            .is_some_and(|(_, candidate)| candidate.run_index == group.first_run);
        if !has_media {
            units.push(PlacedUnit {
                span: span_of_group,
                group: group_index,
                reach: group.device_rect,
                media: None,
                backdrop: None,
            });
            continue;
        }
        // In a run that draws media, each record is a unit of its own, so content over the media goes above it only
        // where it reaches.
        let geometry = &geometries[group.space];
        let (chunk_index, run_in_chunk) = list.chunk_of_run(group.first_run as usize);
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
                backdrop: None,
            });
        }
    }

    ListPlacement {
        layering_obstacles: LayeringObstacles::of(list, tree),
        spaces: spaces.to_vec(),
        groups,
        units,
        media,
        rejected_media,
        backdrops,
        rejected_backdrop_filters,
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
    Backdrop { unit: usize },
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
    // Per backdrop of the placement, how many backdrops of its node are painted before it.
    ordinals_of_backdrops: Vec<u32>,
}

impl StackBeingBuilt<'_> {
    fn node_of(&mut self, space: usize, composite_clips: Vec<CompositeClip>) -> usize {
        if let Some(node) = self
            .nodes
            .iter()
            .position(|node| node.space == space && node.composite_clips == composite_clips)
        {
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
        self.nodes.push(PlacementNode {
            key: key_of_node(spaces, space, &composite_clips),
            space,
            composite_clips,
            parent,
            region_in_parent,
        });
        self.children_of_node.push(Vec::new());
        let node = self.nodes.len() - 1;
        if let Some(parent) = parent {
            self.children_of_node[parent].push(node);
        }
        node
    }

    // The content of a held sticky box shows in the space around it wherever the box's offset takes it, so the region of
    // its node there is what the node's units reach swept over the box's offset range, within the clips the node is
    // composited within there.
    fn bound_the_regions_of_held_sticky_boxes(&mut self) {
        let placement = self.placement;
        let mut reach_of_nodes = vec![IntRect::default(); self.nodes.len()];
        for unit in &placement.units {
            let node = self.node_of_group[unit.group];
            reach_of_nodes[node] = reach_of_nodes[node].united(unit.reach);
        }
        for (node, reach) in self.nodes.iter_mut().zip(reach_of_nodes) {
            let space = &placement.spaces[node.space];
            if !matches!(space.anchor_kind, RasterSpaceAnchorKind::HeldStickyBox { .. }) {
                continue;
            }
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

    fn close_band_of(&self, node: usize, open_band_of_node: &mut [Option<usize>]) {
        open_band_of_node[node] = None;
        for &child in &self.children_of_node[node] {
            self.close_band_of(child, open_band_of_node);
        }
    }

    // Walks the units in paint order, giving each node bands within the bands of its parent, and each band its
    // barriers and its own units.
    fn cut_into_bands(&mut self) {
        let placement = self.placement;
        let mut open_band_of_node: Vec<Option<usize>> = vec![None; self.nodes.len()];
        let mut next_band_of_node = vec![0u32; self.nodes.len()];
        self.bands.push(BandBeingBuilt {
            node: 0,
            ordinal: 0,
            barriers: Vec::new(),
            own_units: Vec::new(),
            first_unit: 0,
        });
        open_band_of_node[0] = Some(0);
        next_band_of_node[0] = 1;
        for (unit_index, unit) in placement.units.iter().enumerate() {
            if unit.reach.is_empty() {
                continue;
            }
            let path = self.path_to(self.node_of_group[unit.group]);
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
                        self.bands.push(BandBeingBuilt {
                            node: child,
                            ordinal: next_band_of_node[child],
                            barriers: Vec::new(),
                            own_units: Vec::new(),
                            first_unit: unit_index,
                        });
                        next_band_of_node[child] += 1;
                        let child_band = self.bands.len() - 1;
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
                    None if unit.backdrop.is_some() => {
                        self.bands[band]
                            .barriers
                            .push(BarrierBeingBuilt::Backdrop { unit: unit_index });
                    }
                    None => self.bands[band].own_units.push(unit_index),
                }
            }
        }
    }

    // Pushes the layers of a band and of the bands in it, bottom to top, and the group of its records.
    fn emit_band(&mut self, band_index: usize) -> usize {
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
            content: LayerContent::Painted { group },
            may_hold_content: false,
            may_hold_root_group_content: false,
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
                        content: LayerContent::Media {
                            content: media.content,
                            record_span: placed_unit.span,
                            region: placed_unit.reach,
                            window_server_placement: None,
                        },
                        may_hold_content: true,
                        may_hold_root_group_content: true,
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
                    self.emit_band(band);
                    (key, position, self.nodes[child_node].region_in_parent)
                }
                BarrierBeingBuilt::Backdrop { unit } => {
                    let placed_unit = &placement.units[unit];
                    let backdrop_index = placed_unit.backdrop.expect("a backdrop barrier is a backdrop unit");
                    let backdrop = &placement.backdrops[backdrop_index];
                    let ordinal = self.ordinals_of_backdrops[backdrop_index];
                    self.layers.push(StackLayer {
                        key: LayerKey::Backdrop {
                            node: node_key,
                            ordinal,
                        },
                        node,
                        content: LayerContent::Backdrop {
                            effect: backdrop.effect,
                            marker_span: backdrop.marker_span,
                            read_area: backdrop.read_area,
                        },
                        may_hold_content: true,
                        may_hold_root_group_content: true,
                    });
                    (
                        BarrierKey::Backdrop {
                            node: node_key,
                            ordinal,
                        },
                        unit_position(unit),
                        Reach::Within(backdrop.read_area),
                    )
                }
            };
            self.layers.push(StackLayer {
                key: LayerKey::Painted {
                    node: node_key,
                    band: ordinal,
                    above: Some(key),
                },
                node,
                content: LayerContent::Painted { group },
                may_hold_content: false,
                may_hold_root_group_content: false,
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
            record_spans: Vec::new(),
            cuts,
            layer_below_every_cut,
        };
        for &unit in &own_units {
            let placed_unit = &placement.units[unit];
            let span = placed_unit.span;
            let position = span.begin;
            let in_root_group = placement.groups[placed_unit.group].paints_within_the_root_group;
            let mut mark = |layer: usize| {
                self.layers[layer].may_hold_content = true;
                self.layers[layer].may_hold_root_group_content |= in_root_group;
            };
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
    if !placement_node.composite_clips.is_empty() || placement_node.region_in_parent != Reach::Anywhere {
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

enum StackConflict {
    // Media drawn, and backdrop filters applied by layers of their own, before content that reads the root group's
    // backdrop, where the content can show.
    BarriersUnderBackdrop {
        media: Vec<ListPosition>,
        backdrop_filters: Vec<EffectNodeIndex>,
    },
    // Content that reads the root group's backdrop would read through the layers of these spaces, which are to be
    // rastered in the spaces around them instead.
    BackdropReadThroughSpaces(Vec<usize>),
}

// The spaces on the chain from `space` up to the lowest space it shares with `other`, that one left out.
fn spaces_up_to_common_space(spaces: &[RasterSpace], space: usize, other: usize) -> Vec<usize> {
    let common = lowest_common_space(spaces, space, other);
    chain_of_space(spaces, space)
        .take_while(|&in_chain| in_chain != common)
        .collect()
}

fn assemble_stack(placement: ListPlacement) -> (LayerStack, Option<StackConflict>) {
    let mut built = StackBeingBuilt {
        placement: &placement,
        nodes: Vec::new(),
        node_of_group: Vec::new(),
        children_of_node: Vec::new(),
        bands: Vec::new(),
        layers: Vec::new(),
        groups: Vec::new(),
        ordinals_of_backdrops: Vec::new(),
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
    let mut backdrops_of_nodes = vec![0u32; built.nodes.len()];
    built.ordinals_of_backdrops = placement
        .backdrops
        .iter()
        .map(|backdrop| {
            let backdrops_of_node = &mut backdrops_of_nodes[built.node_of_group[backdrop.group]];
            *backdrops_of_node += 1;
            *backdrops_of_node - 1
        })
        .collect();
    built.cut_into_bands();
    built.emit_band(0);

    let StackBeingBuilt {
        nodes,
        node_of_group,
        layers,
        groups,
        ..
    } = built;

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

    // Content that reads the root group's backdrop reads what is below it in its own layer only, so no layer below its
    // own may hold content of the root group, and no barrier painted before it may cut the tiles it reaches.
    let mut conflict = None;
    for unit in &placement.units {
        let group_of_unit = &placement.groups[unit.group];
        let read_area = match group_of_unit.root_group_backdrop_read {
            Reach::Nothing => continue,
            Reach::Within(rect) => Some(rect),
            Reach::Anywhere => None,
        };
        if unit.reach.is_empty() {
            continue;
        }
        let node = node_of_group[unit.group];
        let Some(layer_group) = groups
            .iter()
            .find(|group| group.node == node && group.record_spans.iter().any(|span| span.contains(unit.span.begin)))
        else {
            continue;
        };
        // The content and the layers it would read through go into one space, the lowest one that holds both. Layers
        // of its own space alone are in other bands or behind other clips, which the space around it merges.
        let space = nodes[node].space;
        let layers_read_through = base_layer..layer_group.layer_below_every_cut;
        // A backdrop layer read through goes first, as the spaces around it may then hold what it reads.
        let backdrop_filters_read_through: Vec<EffectNodeIndex> = layers_read_through
            .clone()
            .filter_map(|layer| match layers[layer].content {
                LayerContent::Backdrop { effect, .. } => Some(effect),
                _ => None,
            })
            .collect();
        if !backdrop_filters_read_through.is_empty() {
            conflict = Some(StackConflict::BarriersUnderBackdrop {
                media: Vec::new(),
                backdrop_filters: backdrop_filters_read_through,
            });
            break;
        }
        let spaces_read_through: Vec<usize> = layers_read_through
            .filter(|&layer| layers[layer].may_hold_root_group_content)
            .map(|layer| nodes[layers[layer].node].space)
            .collect();
        if !spaces_read_through.is_empty() {
            let mut spaces_to_fold = Vec::new();
            for &other_space in &spaces_read_through {
                spaces_to_fold.extend(spaces_up_to_common_space(&placement.spaces, space, other_space));
                spaces_to_fold.extend(spaces_up_to_common_space(&placement.spaces, other_space, space));
            }
            if spaces_to_fold.is_empty() && space != VIEWPORT_RASTER_SPACE {
                spaces_to_fold.push(space);
            }
            conflict = Some(StackConflict::BackdropReadThroughSpaces(spaces_to_fold));
            break;
        }
        // What it reads below it has to be in its layer wherever it reads, as must it be wherever it draws.
        let reached_tiles = read_area.map(|read_area| expanded_to_tile_grid(unit.reach.united(read_area)));
        let mut refused_media = Vec::new();
        let mut refused_backdrop_filters = Vec::new();
        let mut spaces_to_fold = Vec::new();
        for cut in &layer_group.cuts {
            if cut.position > unit.span.begin || reached_tiles.is_some_and(|tiles| !cut.cuts_cell(tiles)) {
                continue;
            }
            match cut.barrier {
                BarrierKey::Media { .. } => refused_media.push(cut.position),
                BarrierKey::Backdrop { node, ordinal } => {
                    let backdrop_layer = layers
                        .iter()
                        .find(|layer| layer.key == LayerKey::Backdrop { node, ordinal })
                        .expect("a backdrop barrier is a layer of the stack");
                    let LayerContent::Backdrop { effect, .. } = backdrop_layer.content else {
                        unreachable!("a backdrop key names a backdrop layer");
                    };
                    refused_backdrop_filters.push(effect);
                }
                BarrierKey::Band { node: band_node, .. } => {
                    let band_space = nodes
                        .iter()
                        .find(|candidate| candidate.key == band_node)
                        .expect("a band barrier names a node of the stack")
                        .space;
                    spaces_to_fold.extend(spaces_up_to_common_space(
                        &placement.spaces,
                        band_space,
                        nodes[node].space,
                    ));
                }
            }
        }
        if !spaces_to_fold.is_empty() {
            conflict = Some(StackConflict::BackdropReadThroughSpaces(spaces_to_fold));
            break;
        }
        if !refused_media.is_empty() || !refused_backdrop_filters.is_empty() {
            conflict = Some(StackConflict::BarriersUnderBackdrop {
                media: refused_media,
                backdrop_filters: refused_backdrop_filters,
            });
            break;
        }
    }

    let stack = LayerStack {
        spaces: placement.spaces.clone(),
        nodes,
        groups,
        layers,
        base_layer,
        unlayered_reason: None,
        layering_obstacles: placement.layering_obstacles,
        rejected_media: placement.rejected_media.clone(),
        rejected_backdrop_filters: placement.rejected_backdrop_filters.clone(),
        placement,
        placement_at_every_anchor: ListPlacement::default(),
    };
    (stack, conflict)
}

/// Works out the layers of a list from where its parts are placed among the spaces at every anchor the tree offers,
/// which `place_list` gave for the spaces at `anchors`. Where the window server can show the media of the stack is left
/// to be worked out for each frame.
pub fn build_layer_stack(
    inputs: &LayerStackInputs<'_>,
    anchors: &[SpatialNodeIndex],
    placement_at_every_anchor: ListPlacement,
) -> LayerStack {
    let single_layer = |reason: Option<UnlayeredReason>| {
        let mut stack = LayerStack::single_layer(inputs.viewport_rect, inputs.list.whole_span(), reason);
        stack.layering_obstacles = placement_at_every_anchor.layering_obstacles;
        stack
    };
    if let Some(reason) = why_list_is_unlayered(inputs, &placement_at_every_anchor.layering_obstacles) {
        let mut stack = single_layer(Some(reason));
        stack.placement_at_every_anchor = placement_at_every_anchor;
        return stack;
    }
    let mut anchors = anchors.to_vec();
    let mut placement = placement_at_every_anchor.clone();
    let mut positions_refused_media_layers: Vec<ListPosition> = Vec::new();
    let mut effects_refused_backdrop_layers: Vec<EffectNodeIndex> = Vec::new();
    // Each try leaves out at least one more canvas, video or backdrop layer, or the innermost space that scrolls, so
    // this ends.
    loop {
        let (mut stack, conflict) = assemble_stack(placement);
        match conflict {
            Some(StackConflict::BarriersUnderBackdrop {
                media,
                backdrop_filters,
            }) => {
                positions_refused_media_layers.extend(media);
                effects_refused_backdrop_layers.extend(backdrop_filters);
            }
            Some(StackConflict::BackdropReadThroughSpaces(spaces_to_fold))
                if spaces_to_fold.iter().any(|&space| space != VIEWPORT_RASTER_SPACE) =>
            {
                // A space folded into its parent takes the spaces inside it along.
                let stack_spaces = &stack.spaces;
                let folded_anchors: Vec<SpatialNodeIndex> = (1..stack_spaces.len())
                    .filter(|&space| {
                        spaces_to_fold
                            .iter()
                            .any(|&folded| space_is_ancestor_or_self(stack_spaces, folded, space))
                    })
                    .map(|space| stack_spaces[space].anchor)
                    .collect();
                anchors.retain(|anchor| !folded_anchors.contains(anchor));
            }
            Some(StackConflict::BackdropReadThroughSpaces(_)) => {
                let rejected_backdrop_filters = std::mem::take(&mut stack.rejected_backdrop_filters);
                let mut stack = single_layer(None);
                stack.rejected_backdrop_filters = rejected_backdrop_filters;
                stack.placement_at_every_anchor = placement_at_every_anchor;
                return stack;
            }
            None => {
                let layers_with_content = stack
                    .layers
                    .iter()
                    .skip(stack.base_layer)
                    .filter(|layer| layer.may_hold_content)
                    .count();
                if layers_with_content <= MAXIMUM_LAYER_COUNT || anchors.is_empty() {
                    stack.placement_at_every_anchor = placement_at_every_anchor;
                    return stack;
                }
                anchors.pop();
            }
        }
        let (spaces, geometries) = spaces_with_geometries(
            inputs.tree,
            &anchors,
            inputs.scroll_offsets,
            inputs.viewport_rect,
            inputs.raster_transform,
        );
        placement = place_list(
            inputs,
            spaces,
            &geometries,
            &positions_refused_media_layers,
            &effects_refused_backdrop_layers,
        );
    }
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
    let placement = place_list(inputs, spaces, &geometries, &[], &[]);
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
        BackdropFilterData, CompositorScrollNodeRecord, CompositorScrollTable, EffectsData, ScrollData, TransformData,
        TransformDataRole,
    };
    use libgfx_rust::filter::Filter;
    use libgfx_rust::{Color, CompositingAndBlendingOperator, CornerRadii, FloatRect, ScalingMode};

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
                    LayerKey::Backdrop { ordinal, .. } => format!("backdrop #{ordinal} {space}"),
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

    fn frosted_effect(region: IntRect) -> EffectNodeData {
        EffectNodeData::Effects(EffectsData {
            isolation: false,
            opacity: 1.0,
            blend_mode: CompositingAndBlendingOperator::Normal,
            filter: None,
            backdrop_filter: Some(BackdropFilterData {
                filter: std::sync::Arc::new(Filter::blur(4.0, 4.0, None).serialize()),
                region,
                corner_radii: CornerRadii::default(),
            }),
        })
    }

    // The recorder starts what a box with a backdrop filter paints with the record that marks where it reads its
    // backdrop, so no page has records that do not.
    #[test]
    fn a_backdrop_filter_whose_records_do_not_start_with_its_marker_reads_in_the_layer_of_what_it_reads() {
        let region = IntRect::new(120, 120, 60, 40);
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let frosted = tree.append_effect_node_under(root, frosted_effect(region));
        let list = TestList::new().with(fill(VIEWPORT), root).with(fill(region), frosted);
        let stack = list.stack(&tree, &[]);
        assert!(stack.backdrop_filters_applied_by_layers().is_empty());
        assert_eq!(
            stack.rejected_backdrop_filters,
            [(
                frosted.effect,
                BackdropLayerRejection::ContentIsNotOneStretchStartingWithTheBackdrop
            )]
        );
    }
}
