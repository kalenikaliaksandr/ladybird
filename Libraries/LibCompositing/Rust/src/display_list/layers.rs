/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a display list is painted in, bottom to top.
//!
//! Content is rastered in the space of the scroll node it moves with, and its raster is composited within the clips it
//! leaves to apply then: content of one space and one set of such clips makes a placement node. Within a node, a
//! canvas, video or nested context that is a layer of its own, and each band of content of a node inside it, sits
//! between the node's records as a barrier. A barrier cuts the tiles of the node it can show in at its paint position:
//! there, the node's records painted before it go into the node's layer below it and those painted after it into the
//! node's layer above it. Elsewhere it cuts nothing. A band of a node inside ends where content outside it that can
//! show where the band can is painted, so the layers stacked in paint order of their barriers paint every pixel in the
//! order the list does, as long as nothing between them reads what is below it.

use crate::display_list::builder::read_command;
use crate::display_list::commands::{DeclareMaskContent, DisplayListCommandType, EffectNodeIndex, SpatialNodeIndex};
use crate::display_list::effect_geometry::{EffectGeometry, compute_effect_geometry};
pub use crate::display_list::placement::{CompositeClip, RasterSpace, VIEWPORT_RASTER_SPACE, raster_space_anchors};
use crate::display_list::placement::{
    PlacementTables, RasterSpaces, RunsReachingOutsideTheirContext, SpaceGeometry, chain_of_space, lowest_common_space,
    raster_scroll_offsets, space_is_ancestor_or_self, spaces_with_geometries,
};
use crate::display_list::storage::{DisplayListStorage, ListPosition, ListSpan};
use crate::display_list::summary::{IndexedRecord, LiveContent};
use crate::display_list::tiles::{TILE_SIZE_IN_DEVICE_PIXELS, tile_coords_touching, tile_count_touching};
use crate::display_list::window_server_placement::{WindowServerPlacement, WindowServerRejection};
use crate::visual_context::{EffectNodeData, SpatialData, VisualContextTree, WorldTransforms, device_offset_for_index};
use libgfx_rust::{FloatMatrix4x4, FloatPoint, IntPoint, IntRect};
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;

pub const MAXIMUM_MEDIA_LAYER_COUNT: usize = 8;
// Beyond this, the innermost space that scrolls is rastered in the space around it instead.
const MAXIMUM_LAYER_COUNT: usize = 24;
// Beyond this, a unit is taken to reach every layer of its band painted before it, rather than followed tile by tile.
const MAXIMUM_TILES_A_UNIT_IS_FOLLOWED_INTO: u64 = 4096;

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

/// Content rastered in one space and composited within one set of clips.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementNode {
    pub key: PlacementKey,
    pub space: usize,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    pub parent: Option<usize>,
    /// Where content of the node and of the nodes in it can show, in the raster coordinates of the parent node, or
    /// none where that is not bounded.
    pub region_in_parent: Option<IntRect>,
}

/// Where a barrier cuts the tiles of a group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub barrier: BarrierKey,
    /// Where the barrier's first record is.
    pub position: ListPosition,
    /// The barrier cuts the tiles of the group's space this reaches, every tile where it is none.
    pub region: Option<IntRect>,
    pub layer_above: usize,
}

impl Cut {
    pub fn cuts_cell(&self, cell: IntRect) -> bool {
        self.region.is_none_or(|region| !region.intersected(cell).is_empty())
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
    /// The canvases, videos and nested contexts that were not given layers, and why.
    pub rejected_media: Vec<(LiveContent, MediaLayerRejection)>,
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
            region_in_parent: None,
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
            rejected_media: Vec::new(),
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
                && space.anchor_scrolls_the_viewport == other.anchor_scrolls_the_viewport
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

    /// Where the frame composites the rasters of a space: translated by the scroll offsets of the anchors of its chain,
    /// rounded to whole device pixels.
    pub fn composite_offset(&self, space: usize, scroll_offsets: &[FloatPoint]) -> IntPoint {
        chain_of_space(&self.spaces, space).fold(IntPoint::default(), |sum, space| {
            let offset = device_offset_for_index(scroll_offsets, self.spaces[space].anchor);
            IntPoint {
                x: sum.x + offset.x.round() as i32,
                y: sum.y + offset.y.round() as i32,
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
        if space == VIEWPORT_RASTER_SPACE {
            "viewport".into()
        } else if raster_space.anchor_scrolls_the_viewport {
            "document".into()
        } else {
            format!(
                "scroller@{} in {}",
                raster_space.anchor.0,
                self.name_of_space(raster_space.parent)
            )
        }
    }

    fn name_of_barrier(&self, barrier: &BarrierKey) -> String {
        match barrier {
            BarrierKey::Media { content, ordinal } => {
                let layer = self
                    .layers
                    .iter()
                    .position(|layer| {
                        layer.key
                            == LayerKey::Media {
                                content: *content,
                                ordinal: *ordinal,
                            }
                    })
                    .expect("a media barrier is a layer of the stack");
                format!("L{layer}")
            }
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
            (LayerKey::Media { .. }, LayerContent::Painted { .. }) => unreachable!("a media key names media"),
        }
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
        for name in self.names_of_media_without_layers() {
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
            if let LayerContent::Media { record_span, .. } = &layer.content
                && record_span.begin == position
            {
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
        let mut layers: Vec<usize> = if tile_count_touching(unit.reach) > MAXIMUM_TILES_A_UNIT_IS_FOLLOWED_INTO {
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
pub(crate) fn mask_declarations_sit_on_the_chains_of_their_masks(
    list: &DisplayListStorage,
    tree: &VisualContextTree,
) -> bool {
    list.summary().mask_declarations.iter().all(|record| {
        let (_, payload) = list.record_at(record.position());
        let mask = read_command::<DeclareMaskContent>(payload).effect;
        let run_effect = list.run(record.run_index as usize).context.effect;
        run_effect.is_none()
            || effect_chain_contains(tree, run_effect, mask)
            || effect_chain_contains(tree, mask, run_effect)
    })
}

/// Why a list cannot be layered at all.
pub fn why_list_is_unlayered(inputs: &LayerStackInputs<'_>) -> Option<UnlayeredReason> {
    let tree = inputs.tree;
    if inputs.viewport_rect.is_empty() {
        return Some(UnlayeredReason::EmptyViewport);
    }
    if inputs.list.run_count() == 0 {
        return Some(UnlayeredReason::EmptyDisplayList);
    }
    if tree_has_sorting_contexts(tree) {
        return Some(UnlayeredReason::SortingContext3D);
    }
    if !root_isolation_effect_is_plain_group(tree) {
        return Some(UnlayeredReason::RootIsolationEffectIsNotPlainGroup);
    }
    if !mask_declarations_sit_on_the_chains_of_their_masks(inputs.list, tree) {
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
    pub root_group_backdrop_read: RootGroupBackdropRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootGroupBackdropRead {
    Nothing,
    Within(IntRect),
    Anywhere,
}

impl RootGroupBackdropRead {
    fn united(self, other: Self) -> Self {
        match (self, other) {
            (Self::Nothing, read) | (read, Self::Nothing) => read,
            (Self::Within(rect), Self::Within(other)) => Self::Within(rect.united(other)),
            _ => Self::Anywhere,
        }
    }
}

// Where a backdrop filter reads, in the raster coordinates of the space: its region, and how far its filter reaches
// past it.
fn backdrop_filter_read_area(
    tree: &VisualContextTree,
    geometry: &SpaceGeometry,
    effect: EffectNodeIndex,
) -> RootGroupBackdropRead {
    let node = &tree.effect_nodes[effect.0 as usize];
    let EffectNodeData::Effects(effects) = &node.data else {
        return RootGroupBackdropRead::Anywhere;
    };
    let Some(backdrop) = &effects.backdrop_filter else {
        return RootGroupBackdropRead::Anywhere;
    };
    let effect_to_device = geometry.world.draw_matrix(node.spatial);
    let Some(reach) = crate::display_list::tile_keys::backdrop_filter_read_reach_in_device_pixels(
        &backdrop.filter,
        &effect_to_device,
    ) else {
        return RootGroupBackdropRead::Anywhere;
    };
    let region = geometry
        .world
        .map_rect_to_root(node.spatial, backdrop.region.to_float());
    if !(region.x.is_finite() && region.y.is_finite() && region.width.is_finite() && region.height.is_finite()) {
        return RootGroupBackdropRead::Anywhere;
    }
    RootGroupBackdropRead::Within(libgfx_rust::enclosing_int_rect(region).inflated_edges(reach, reach, reach, reach))
}

/// A stretch of records of one group that is placed as one: the whole group, or the records of a run between the
/// canvases, videos and nested contexts that are layers of their own, or one of those.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedUnit {
    pub span: ListSpan,
    pub group: usize,
    pub reach: IntRect,
    pub media: Option<usize>,
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
    pub spaces: Vec<RasterSpace>,
    pub groups: Vec<PlacedRunGroup>,
    pub units: Vec<PlacedUnit>,
    pub media: Vec<PlacedMedia>,
    pub rejected_media: Vec<(LiveContent, MediaLayerRejection)>,
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

/// Where each part of the list is placed among the spaces at `anchors`, given the geometry of each of those spaces,
/// leaving out of the media layers the drawings at `positions_refused_media_layers`.
pub fn place_list(
    inputs: &LayerStackInputs<'_>,
    anchors: &[SpatialNodeIndex],
    geometries: &[Rc<SpaceGeometry>],
    positions_refused_media_layers: &[ListPosition],
) -> ListPlacement {
    let tree = inputs.tree;
    let list = inputs.list;
    let effect_clips = list.effect_clip_plan(tree);
    let run_groups = list.run_groups(tree);
    let spaces = RasterSpaces::at_anchors(tree, anchors, inputs.viewport_rect);
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
    let mut effect_geometry_of_spaces: Vec<Option<Vec<EffectGeometry>>> = vec![None; spaces.len()];
    let mut group_rects: std::collections::HashMap<(usize, u32), IntRect> = std::collections::HashMap::new();

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
            let mut root_group_backdrop_read = RootGroupBackdropRead::Nothing;
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
                    root_group_backdrop_read = root_group_backdrop_read.united(RootGroupBackdropRead::Within(run_rect));
                }
                if !run.context.effect.is_none() {
                    let backdrop_filter = backdrop_filter_reading_root_group_on_chain[run.context.effect.0 as usize];
                    if !backdrop_filter.is_none() {
                        root_group_backdrop_read =
                            root_group_backdrop_read.united(backdrop_filter_read_area(tree, geometry, backdrop_filter));
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
            PlacedRunGroup {
                first_run,
                last_run,
                space,
                composite_clips,
                device_rect,
                moves_within_space,
                paints_within_the_root_group,
                root_group_backdrop_read,
            }
        })
        .collect();

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

    let mut units = Vec::with_capacity(groups.len());
    let mut media_of_records = media.iter().enumerate().peekable();
    for (group_index, group) in groups.iter().enumerate() {
        let has_media = media_of_records
            .peek()
            .is_some_and(|(_, candidate)| candidate.run_index == group.first_run);
        if !has_media {
            units.push(PlacedUnit {
                span: ListSpan {
                    begin: list.start_of_run(group.first_run as usize),
                    end: list.end_of_run(group.last_run as usize),
                },
                group: group_index,
                reach: group.device_rect,
                media: None,
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
            });
        }
    }

    ListPlacement {
        spaces: spaces.to_vec(),
        groups,
        units,
        media,
        rejected_media,
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

fn regions_meet(first: Option<IntRect>, second: Option<IntRect>) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => !first.intersected(second).is_empty(),
        (Some(rect), None) | (None, Some(rect)) => !rect.is_empty(),
        (None, None) => true,
    }
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
            (None, None)
        } else {
            let parent_space = spaces[space].parent;
            let region = composite_clips
                .iter()
                .find(|clip| clip.space == parent_space)
                .map(|clip| clip.rect);
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
                    None => Some(unit.reach),
                };
                for &child in &self.children_of_node[node] {
                    if Some(child) == child_on_path || open_band_of_node[child].is_none() {
                        continue;
                    }
                    if regions_meet(reach, self.nodes[child].region_in_parent) {
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
                        Some(placed_unit.reach),
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
            } else if tile_count_touching(placed_unit.reach) > MAXIMUM_TILES_A_UNIT_IS_FOLLOWED_INTO {
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
    if !placement_node.composite_clips.is_empty() || placement_node.region_in_parent.is_some() {
        return false;
    }
    match placement_node.parent {
        None => true,
        Some(parent) => {
            spaces[placement_node.space].anchor_scrolls_the_viewport && node_covers_the_viewport(nodes, spaces, parent)
        }
    }
}

enum StackConflict {
    // Media drawn before content that reads the root group's backdrop, where the content can show.
    MediaUnderBackdrop(Vec<ListPosition>),
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
            RootGroupBackdropRead::Nothing => continue,
            RootGroupBackdropRead::Within(rect) => Some(rect),
            RootGroupBackdropRead::Anywhere => None,
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
        let spaces_read_through: Vec<usize> = (base_layer..layer_group.layer_below_every_cut)
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
        let mut spaces_to_fold = Vec::new();
        for cut in &layer_group.cuts {
            if cut.position > unit.span.begin || reached_tiles.is_some_and(|tiles| !cut.cuts_cell(tiles)) {
                continue;
            }
            match cut.barrier {
                BarrierKey::Media { .. } => refused_media.push(cut.position),
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
        if !refused_media.is_empty() {
            conflict = Some(StackConflict::MediaUnderBackdrop(refused_media));
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
        rejected_media: placement.rejected_media.clone(),
        placement,
        placement_at_every_anchor: ListPlacement::default(),
    };
    (stack, conflict)
}

/// Works out the layers of a list from where its parts are placed among the spaces at every anchor the tree offers,
/// which `place_list` gave for `anchors`.
pub fn build_layer_stack(
    inputs: &LayerStackInputs<'_>,
    anchors: &[SpatialNodeIndex],
    placement_at_every_anchor: ListPlacement,
) -> LayerStack {
    if let Some(reason) = why_list_is_unlayered(inputs) {
        let mut stack = LayerStack::single_layer(inputs.viewport_rect, inputs.list.whole_span(), Some(reason));
        stack.placement_at_every_anchor = placement_at_every_anchor;
        return stack;
    }
    let mut anchors = anchors.to_vec();
    let mut placement = placement_at_every_anchor.clone();
    let mut positions_refused_media_layers: Vec<ListPosition> = Vec::new();
    // Each try leaves out at least one more canvas or video, or the innermost space that scrolls, so this ends.
    loop {
        let (mut stack, conflict) = assemble_stack(placement);
        match conflict {
            Some(StackConflict::MediaUnderBackdrop(positions)) => {
                positions_refused_media_layers.extend(positions);
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
                let mut stack = LayerStack::single_layer(inputs.viewport_rect, inputs.list.whole_span(), None);
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
                    stack.update_window_server_placements(
                        inputs.list,
                        inputs.tree,
                        inputs.scroll_offsets,
                        inputs.viewport_rect,
                    );
                    return stack;
                }
                anchors.pop();
            }
        }
        let (_, geometries) = spaces_with_geometries(
            inputs.tree,
            &anchors,
            inputs.scroll_offsets,
            inputs.viewport_rect,
            inputs.raster_transform,
        );
        placement = place_list(inputs, &anchors, &geometries, &positions_refused_media_layers);
    }
}

/// Works out the layers of a list from scratch, as a dump or a test does.
pub fn layer_stack_of(inputs: &LayerStackInputs<'_>) -> LayerStack {
    let anchors = raster_space_anchors(inputs.tree);
    let (_, geometries) = spaces_with_geometries(
        inputs.tree,
        &anchors,
        inputs.scroll_offsets,
        inputs.viewport_rect,
        inputs.raster_transform,
    );
    let placement = place_list(inputs, &anchors, &geometries, &[]);
    build_layer_stack(inputs, &anchors, placement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{
        CanvasId, ClipNodeIndex, CompositorContextId, ContextRef, DisplayListCommand, DrawCanvas,
        DrawCompositedContext, FillRect, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::display_list::tiles::TileCoord;
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_context::{
        BackdropFilterData, ClipNodeData, CompositorScrollNodeRecord, CompositorScrollTable, EffectsData, ScrollData,
        StickyData, TransformData, TransformDataRole,
    };
    use libgfx_rust::filter::Filter;
    use libgfx_rust::{Color, CompositingAndBlendingOperator, CornerRadii, FloatRect, ScalingMode};

    const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);
    const CANVAS: LiveContent = LiveContent::Canvas(CanvasId(1));

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

    fn nested_context_with_id(rect: IntRect, child_context_id: CompositorContextId) -> DrawCompositedContext {
        DrawCompositedContext {
            dst_rect: rect.to_float(),
            child_context_id,
            scaling_mode: ScalingMode::NearestNeighbor,
            spatial_node_index: VISUAL_VIEWPORT_NODE_INDEX,
            snap_origin_if_untransformed: true,
        }
    }

    struct TestList {
        builder: DisplayListBuilder,
        positions: Vec<ListPosition>,
    }

    impl TestList {
        fn new() -> Self {
            Self {
                builder: DisplayListBuilder::new(),
                positions: Vec::new(),
            }
        }

        fn with<C: DisplayListCommand>(mut self, command: C, context: ContextRef) -> Self {
            self.positions
                .push(ListPosition::new(0, self.builder.byte_size() as u32));
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

        fn layers_of(&self, stack: &LayerStack, record: usize) -> Vec<usize> {
            stack.layers_of_record(self.positions[record])
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

    #[test]
    fn content_over_a_canvas_goes_above_it_only_in_the_tiles_the_canvas_can_show_in() {
        let tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let list = TestList::new()
            .with(fill(VIEWPORT), root)
            .with(canvas(IntRect::new(100, 100, 200, 100)), root)
            .with(fill(IntRect::new(150, 150, 20, 20)), root)
            .with(fill(IntRect::new(500, 400, 20, 20)), root)
            .with(fill(IntRect::new(250, 150, 20, 200)), root);
        let stack = list.stack(&tree, &[]);
        assert_eq!(
            shape(&stack),
            [
                "base viewport band 0",
                "canvas #0 viewport",
                "painted viewport band 0 above L1"
            ]
        );
        let (base, canvas_layer, above_canvas) = (0, 1, 2);
        assert_eq!(list.layers_of(&stack, 0), [base]);
        assert_eq!(list.layers_of(&stack, 1), [canvas_layer]);
        assert_eq!(list.layers_of(&stack, 2), [above_canvas]);
        assert_eq!(list.layers_of(&stack, 3), [base]);
        // A box that reaches tiles the canvas can show in and tiles it cannot is in both layers.
        assert_eq!(list.layers_of(&stack, 4), [base, above_canvas]);
        let group = &stack.groups[0];
        let box_position = list.positions[4];
        assert_eq!(
            group.layer_at(box_position, TileCoord::new(1, 0).cell_rect()),
            above_canvas
        );
        assert_eq!(group.layer_at(box_position, TileCoord::new(1, 1).cell_rect()), base);
    }

    #[test]
    fn a_canvas_under_a_group_effect_is_not_a_layer() {
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let group = tree.append_effect_node_under(root, effects(0.5, CompositingAndBlendingOperator::Normal));
        let list = TestList::new().with(canvas(IntRect::new(100, 100, 200, 100)), group);
        let stack = list.stack(&tree, &[]);
        assert_eq!(shape(&stack), ["base viewport band 0"]);
        assert_eq!(stack.rejected_media, [(CANVAS, MediaLayerRejection::InsideGroupEffect)]);
    }

    #[test]
    fn a_blend_mode_reading_the_root_group_refuses_only_the_media_it_would_read_through() {
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let multiply = tree.append_effect_node_under(root, effects(1.0, CompositingAndBlendingOperator::Multiply));
        let list_with_blend_at = |rect| {
            TestList::new()
                .with(fill(VIEWPORT), root)
                .with(canvas(IntRect::new(100, 100, 200, 100)), root)
                .with(fill(rect), multiply)
        };
        let beside = list_with_blend_at(IntRect::new(500, 400, 20, 20)).stack(&tree, &[]);
        assert_eq!(beside.unlayered_reason, None);
        assert_eq!(shape(&beside), ["base viewport band 0", "canvas #0 viewport"]);
        let over = list_with_blend_at(IntRect::new(150, 150, 20, 20)).stack(&tree, &[]);
        assert_eq!(shape(&over), ["base viewport band 0"]);
        assert_eq!(
            over.rejected_media,
            [(CANVAS, MediaLayerRejection::UnderContentReadingTheRootGroup)]
        );
    }

    #[test]
    fn a_backdrop_filter_reading_through_a_nested_scroller_folds_only_that_scroller() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let scrolled = add_nested_scroller(&mut tree, document);
        let frosted = tree.append_effect_node_under(document, backdrop_filter_effect());
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(20, 300, 200, 2000)), scrolled)
            .with(fill(IntRect::new(120, 400, 60, 40)), frosted);
        let stack = list.stack(&tree, &[]);
        assert_eq!(stack.spaces.len(), 2);
        assert_eq!(shape(&stack), ["base document band 0"]);
    }

    #[test]
    fn a_blend_mode_keeps_its_space_over_the_canvas_background_painted_below_the_root_group() {
        let (mut tree, viewport, document) = tree_with_document_scroll();
        let multiply = tree.append_effect_node_under(document, effects(1.0, CompositingAndBlendingOperator::Multiply));
        let canvas_background = ContextRef {
            effect: EffectNodeIndex::NONE,
            ..viewport
        };
        let list_over = |background| {
            TestList::new()
                .with(fill(VIEWPORT), background)
                .with(fill(IntRect::new(0, 0, 800, 2600)), document)
                .with(fill(IntRect::new(40, 120, 120, 60)), multiply)
        };
        let stack = list_over(canvas_background).stack(&tree, &[]);
        assert_eq!(stack.spaces.len(), 2);
        assert_eq!(shape(&stack), ["base viewport band 0", "painted document band 0"]);

        let stack = list_over(viewport).stack(&tree, &[]);
        assert_eq!(stack.spaces.len(), 1);
        assert_eq!(shape(&stack), ["base viewport band 0"]);
    }

    #[test]
    fn nested_contexts_give_way_to_canvases_past_the_most_media_that_get_layers() {
        let tree = identity_tree();
        let mut list = TestList::new();
        for index in 0..=MAXIMUM_MEDIA_LAYER_COUNT {
            let child_context_id = CompositorContextId(10 + index as u64);
            let rect = IntRect::new(index as i32 * 60, 0, 50, 50);
            list = list.with(nested_context_with_id(rect, child_context_id), ContextRef::default());
        }
        list = list.with(canvas(IntRect::new(0, 300, 200, 100)), ContextRef::default());
        let stack = list.stack(&tree, &[]);
        let media: Vec<LiveContent> = stack
            .layers
            .iter()
            .filter_map(|layer| match layer.key {
                LayerKey::Media { content, .. } => Some(content),
                LayerKey::Painted { .. } => None,
            })
            .collect();
        assert_eq!(media.len(), MAXIMUM_MEDIA_LAYER_COUNT);
        assert_eq!(media.last(), Some(&CANVAS));
        assert_eq!(
            stack.rejected_media,
            [
                (
                    LiveContent::CompositedContext(CompositorContextId(10 + MAXIMUM_MEDIA_LAYER_COUNT as u64 - 1)),
                    MediaLayerRejection::MoreMediaThanLayersFor
                ),
                (
                    LiveContent::CompositedContext(CompositorContextId(10 + MAXIMUM_MEDIA_LAYER_COUNT as u64)),
                    MediaLayerRejection::MoreMediaThanLayersFor
                )
            ]
        );
    }

    #[test]
    fn the_window_server_is_asked_about_canvases_and_videos_only() {
        let tree = identity_tree();
        let list = TestList::new()
            .with(
                nested_context_with_id(IntRect::new(100, 100, 200, 100), CompositorContextId(7)),
                ContextRef::default(),
            )
            .with(canvas(IntRect::new(400, 100, 200, 100)), ContextRef::default());
        let stack = list.stack(&tree, &[]);
        let placements: Vec<bool> = stack
            .layers
            .iter()
            .filter_map(|layer| match &layer.content {
                LayerContent::Media {
                    window_server_placement,
                    ..
                } => Some(window_server_placement.is_some()),
                LayerContent::Painted { .. } => None,
            })
            .collect();
        assert_eq!(placements, [false, true]);
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

    fn add_nested_scroller(tree: &mut VisualContextTree, document: ContextRef) -> ContextRef {
        let scrollport = tree.append_clip(
            ClipNodeData::rect_clip(FloatRect::new(20.0, 300.0, 200.0, 200.0)),
            document.clip,
            document.spatial,
        );
        let scroll_node = tree.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            document.spatial,
        );
        let mut table = tree.compositor_scroll_table().clone();
        let mut nodes = table.nodes.to_vec();
        nodes.push(CompositorScrollNodeRecord {
            scroll_node_index: scroll_node,
            parent_scroll_node_index: document.spatial,
            scrollport_rect: IntRect::new(20, 300, 200, 200),
            max_scroll_offset: FloatPoint { x: 0.0, y: 1800.0 },
            ..CompositorScrollNodeRecord::default()
        });
        nodes.sort_by_key(|node| node.scroll_node_index);
        table.nodes = nodes.into();
        assert!(tree.set_compositor_scroll_table(table));
        ContextRef {
            spatial: scroll_node,
            clip: scrollport,
            ..document
        }
    }

    fn document_offsets(tree: &VisualContextTree, document: ContextRef, y: f32) -> Vec<FloatPoint> {
        let mut offsets = vec![FloatPoint::default(); tree.spatial_nodes.len()];
        offsets[document.spatial.0 as usize] = FloatPoint { x: 0.0, y };
        offsets
    }

    #[test]
    fn content_that_scrolls_with_the_document_is_rastered_in_its_space_as_the_base_layer() {
        let (tree, viewport, document) = tree_with_document_scroll();
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(100, 1500, 20, 20)), document)
            .with(fill(IntRect::new(10, 10, 100, 30)), viewport);
        let stack = list.stack(&tree, &[]);
        assert_eq!(
            shape(&stack),
            [
                "base document band 0",
                "painted viewport band 0 above band 0 of document"
            ]
        );
        assert_eq!(
            stack.spaces[stack.space_of_layer(stack.base_layer)].anchor,
            document.spatial
        );
    }

    #[test]
    fn document_content_painted_after_a_fixed_box_starts_a_band_above_the_box() {
        let (tree, viewport, document) = tree_with_document_scroll();
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(10, 10, 100, 30)), viewport)
            .with(fill(IntRect::new(50, 1500, 20, 20)), document);
        let stack = list.stack(&tree, &[]);
        assert_eq!(
            shape(&stack),
            [
                "base document band 0",
                "painted viewport band 0 above band 0 of document",
                "painted document band 1",
            ]
        );
    }

    #[test]
    fn layers_keep_their_keys_and_the_stack_its_placement_as_the_document_scrolls() {
        let (tree, viewport, document) = tree_with_document_scroll();
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(100, 900, 200, 100)), document)
            .with(fill(IntRect::new(10, 10, 100, 30)), viewport);
        let unscrolled = list.stack(&tree, &document_offsets(&tree, document, 0.0));
        let scrolled = list.stack(&tree, &document_offsets(&tree, document, -700.0));
        assert_eq!(
            unscrolled.keys().collect::<Vec<_>>(),
            scrolled.keys().collect::<Vec<_>>()
        );
        assert_eq!(unscrolled.placement_at_every_anchor, scrolled.placement_at_every_anchor);
    }

    #[test]
    fn a_sticky_box_leaves_the_placement_alike_as_it_sticks() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let sticky_node = tree.append_spatial(
            SpatialData::Sticky(StickyData::unconstrained(
                document.spatial,
                None,
                1,
                NodeSlotId::INVALID,
            )),
            document.spatial,
        );
        let sticky = ContextRef {
            spatial: sticky_node,
            ..document
        };
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(100, 900, 200, 100)), document)
            .with(fill(IntRect::new(0, 0, 800, 50)), sticky);
        let mut offsets = document_offsets(&tree, document, -700.0);
        let unstuck = list.stack(&tree, &offsets);
        offsets[sticky_node.0 as usize] = FloatPoint { x: 0.0, y: 700.0 };
        let stuck = list.stack(&tree, &offsets);
        assert_eq!(unstuck.placement_at_every_anchor, stuck.placement_at_every_anchor);
        assert_eq!(
            shape(&stuck),
            [
                "base document band 0",
                "canvas #0 document",
                "painted document band 0 above L2"
            ]
        );
    }

    #[test]
    fn a_nested_scroller_is_one_band_while_document_content_painted_after_it_misses_its_scrollport() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let scrolled = add_nested_scroller(&mut tree, document);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(20, 300, 200, 2000)), scrolled)
            .with(fill(IntRect::new(400, 300, 100, 100)), document)
            .with(fill(IntRect::new(30, 400, 50, 50)), scrolled);
        let stack = list.stack(&tree, &[]);
        // The box painted after the scroller's content misses the tiles of its scrollport, so the layer above the
        // band holds nothing.
        assert_eq!(
            shape(&stack),
            ["base document band 0", "painted scroller@2 in document band 0"]
        );
        let scroller_layer = 2;
        assert_eq!(
            stack.nodes[stack.layers[scroller_layer].node].composite_clips,
            [CompositeClip {
                space: stack.space_of_layer(stack.base_layer),
                rect: IntRect::new(20, 300, 200, 200),
            }]
        );
        assert_eq!(list.layers_of(&stack, 3), [scroller_layer]);
    }

    #[test]
    fn a_band_ends_where_document_content_painted_after_it_reaches_its_scrollport() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let scrolled = add_nested_scroller(&mut tree, document);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(20, 300, 200, 2000)), scrolled)
            .with(fill(IntRect::new(100, 350, 300, 50)), document)
            .with(fill(IntRect::new(30, 400, 50, 50)), scrolled);
        let stack = list.stack(&tree, &[]);
        assert_eq!(
            shape(&stack),
            [
                "base document band 0",
                "painted scroller@2 in document band 0",
                "painted document band 0 above band 0 of scroller@2 in document",
                "painted scroller@2 in document band 1",
            ]
        );
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

    fn backdrop_filter_effect() -> EffectNodeData {
        EffectNodeData::Effects(EffectsData {
            isolation: false,
            opacity: 1.0,
            blend_mode: CompositingAndBlendingOperator::Normal,
            filter: None,
            backdrop_filter: Some(BackdropFilterData {
                filter: std::sync::Arc::new(Filter::blur(4.0, 4.0, None).serialize()),
                region: IntRect::new(120, 120, 60, 40),
                corner_radii: CornerRadii::default(),
            }),
        })
    }

    #[test]
    fn a_canvas_under_a_backdrop_filter_reading_the_root_group_gets_no_layer() {
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let frosted = tree.append_effect_node_under(root, backdrop_filter_effect());
        let list = TestList::new()
            .with(fill(VIEWPORT), root)
            .with(canvas(IntRect::new(100, 100, 200, 100)), root)
            .with(canvas(IntRect::new(500, 400, 200, 100)), root)
            .with(fill(IntRect::new(120, 120, 60, 40)), frosted);
        let stack = list.stack(&tree, &[]);
        assert_eq!(shape(&stack), ["base viewport band 0", "canvas #0 viewport"]);
        assert_eq!(
            stack.rejected_media,
            [(CANVAS, MediaLayerRejection::UnderContentReadingTheRootGroup)]
        );
        let LayerContent::Media { record_span, .. } = &stack.layers[1].content else {
            panic!("the second layer is the canvas");
        };
        assert_eq!(record_span.begin, list.positions[2]);
    }

    #[test]
    fn content_of_a_scroller_that_is_not_a_raster_space_is_placed_wherever_it_can_scroll_to() {
        let (mut tree, viewport, document) = tree_with_document_scroll();
        let frosted = tree.append_effect_node_under(viewport, backdrop_filter_effect());
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(100, 300, 200, 100)), document)
            .with(fill(IntRect::new(120, 320, 20, 20)), document)
            .with(fill(IntRect::new(600, 10, 60, 40)), frosted);
        let unscrolled = list.stack(&tree, &document_offsets(&tree, document, 0.0));
        let scrolled = list.stack(&tree, &document_offsets(&tree, document, -250.0));
        // The backdrop filter keeps the document in the viewport's space, where it scrolls without a new stack, so
        // nothing of the stack may hold only at one scroll offset.
        assert_eq!(unscrolled.spaces.len(), 1);
        assert_eq!(unscrolled.placement_at_every_anchor, scrolled.placement_at_every_anchor);
        assert_eq!(unscrolled.groups, scrolled.groups);
        assert_eq!(unscrolled.layers, scrolled.layers);
    }

    #[test]
    fn a_fixed_backdrop_filter_over_the_document_rasters_everything_in_the_viewport() {
        let (mut tree, viewport, document) = tree_with_document_scroll();
        let frosted = tree.append_effect_node_under(viewport, backdrop_filter_effect());
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(120, 120, 60, 40)), frosted);
        let stack = list.stack(&tree, &[]);
        assert_eq!(shape(&stack), ["base viewport band 0"]);
        assert_eq!(stack.spaces.len(), 1);
    }
}
