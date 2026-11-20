/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Plans the layers of a display list. WebContent's Paint thread plans them and sends the plan to the compositor. The
//! compositor takes a plan as it is, once the plan passes the checks of its wire form, and never plans.
//!
//! A band of a nested node ends where content outside it that can show where the band can is painted, so the layers
//! stacked in paint order of their barriers paint every pixel in the order the list does, as long as nothing within a
//! layer reads what is below the layer. So a backdrop filter that finds all it reads in its own layer, where it is
//! painted, reads it there, and one that does not gets a node of its own, whose layers the compositor composites inside
//! the filter's effect as a replay of the list composites the effect, over what is composited below them. So does an
//! effect whose opacity or filter the compositor animates, so that its samples change how its layers are composited and
//! not their tiles.

use super::{
    BandOfPlan, BarrierCut, BarrierOfPlan, CutOfPlan, LayerContent, LayerIndex, LayerStack, LayeredPlan,
    LayeringObstacles, MAXIMUM_MEDIA_LAYER_COUNT, NestingOfLiveEffects, NodeNamesFromIndices, NodeOfPlan,
    PaintedLayerOfPlan, PlacementNodeIndex, VisualContextNodeNamer, WhyCompositorCannotCompositeEffect,
    WhyEffectIsComposited, WhyListIsOneLayer, WhyMediaHasNoLayer, WindowServerPresentationOfMedia,
    backdrop_filter_read_reach, backdrop_reader_by_reading_effect_index, composited_effects_nest_too_deep, media_name,
    wire,
};
use crate::display_list::commands::{DisplayListCommandType, EffectNodeIndex, SpatialNodeIndex};
use crate::display_list::effect_geometry::{
    KeyframeFiltersOfEffects, filter_may_spread_at_some_sample, nearest_effects_spreading_at_some_sample,
    outermost_effects_with_filters,
};
use crate::display_list::placement::{
    CompositeClip, FiltersAndClipsOfList, RasterSpace, RasterSpaceAnchorKind, RasterSpaceGeometry, RasterSpaceIndex,
    RunGroupIndex, enclosing_int_rect_within_extent_limit, is_finite_rect,
};
use crate::display_list::reach::Reach;
use crate::display_list::storage::{DisplayListStorage, ListPosition, ListSpan, RunIndexInList};
use crate::display_list::summary::{IndexedRecord, LiveContent};
use crate::display_list::tiles::{
    TILE_SIZE_IN_DEVICE_PIXELS, reaches_too_many_cells_to_follow_one_by_one, tile_coords_touching,
};
use crate::visual_context::{EffectNodeData, VisualContextTree};
use libgfx_rust::{CornerRadii, FloatPoint, IntRect};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

mod placement_tables;
mod raster_spaces_for_placing;

use placement_tables::{PlacementTables, RunsReachingOutsideTheirContext, lowest_common_ancestor_space};
use raster_spaces_for_placing::{
    RasterSpaceAnchorsOfTree, RasterSpaceGeometryForPlacing, RasterSpacesForPlacing, rect_swept_over_translations,
    spaces_with_geometries,
};

/// How many effects and subtrees the compositor animates get layers of their own, first in paint order.
pub const MAXIMUM_ANIMATED_NODES_WITH_LAYERS: usize = 32;

/// A node whose values the compositor animates: a spatial node it transforms, or an effect node whose opacity or
/// filter it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatedNode {
    Transform(SpatialNodeIndex),
    Effect(EffectNodeIndex),
}

/// Why what the compositor animates stays in the tiles of the layers around it, which it keys anew as it changes,
/// rather than getting layers of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyAnimationHasNoLayer {
    InsideAnEffectTheCompositorCannotComposite(WhyCompositorCannotCompositeEffect),
    ItsFilterSpreadsWhatItFilters,
    ItHoldsAnEffectThatReadsWhatIsBelowIt,
    ItHoldsRecordsThatBlendWithWhatIsBelowThem,
    SortedByDepthInA3DSortingContext,
    NothingBoundsWhereItMoves,
    ItCanShowOverAnEarlierCanvasOrVideo,
    MoreAnimationsThanLayersFor,
}

impl std::fmt::Display for WhyAnimationHasNoLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyAnimationHasNoLayer::InsideAnEffectTheCompositorCannotComposite(why) => {
                return write!(formatter, "inside a {why}");
            }
            WhyAnimationHasNoLayer::ItsFilterSpreadsWhatItFilters => "its filter spreads what it filters",
            WhyAnimationHasNoLayer::ItHoldsAnEffectThatReadsWhatIsBelowIt => {
                "it holds an effect that reads what is below it"
            }
            WhyAnimationHasNoLayer::ItHoldsRecordsThatBlendWithWhatIsBelowThem => {
                "it holds records that blend with what is below them"
            }
            WhyAnimationHasNoLayer::SortedByDepthInA3DSortingContext => "sorted by depth in a 3D sorting context",
            WhyAnimationHasNoLayer::NothingBoundsWhereItMoves => "nothing bounds where it moves",
            WhyAnimationHasNoLayer::ItCanShowOverAnEarlierCanvasOrVideo => {
                "it can show over an earlier canvas or video"
            }
            WhyAnimationHasNoLayer::MoreAnimationsThanLayersFor => "past the most animations that get layers",
        })
    }
}

/// Why the compositor composites an effect, and what the effect reads of what is composited below it, in the raster
/// coordinates of its node's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteOfCompositedEffect {
    pub why: WhyEffectIsComposited,
    pub backdrop_read_reach: Reach<IntRect>,
}

/// What the dump and the overlay of layers tell of a stack past what a frame reads of it. A stack of one layer that
/// replays the whole list has no notes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotesOfStack {
    /// One per effect of `LayerStack::composited_effects`, in that order.
    pub composited_effects: Vec<NoteOfCompositedEffect>,
    pub media_without_layers: Vec<(LiveContent, WhyMediaHasNoLayer)>,
    /// The backdrop filters and blend modes that find all they read in the layer they go into, and read it there
    /// rather than the compositor compositing them, in paint order.
    pub effects_reading_only_own_layer: Vec<EffectNodeIndex>,
}

/// A plan of the layers of a list: the stack the compositor composites frames from, the notes the dump and the overlay
/// of layers tell of it, and where the parts of the list are placed among the spaces of the stack, which only the dump
/// and the tests read.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedLayers {
    pub stack: LayerStack,
    pub notes: NotesOfStack,
    pub placement: ListPlacement,
}

impl PlannedLayers {
    /// What the dump says of each effect the compositor composites: the space of its node, what it reads of what is
    /// composited below it, and why it is composited.
    pub fn lines_of_composited_effects<'a>(
        &'a self,
        node_namer: &'a dyn VisualContextNodeNamer,
    ) -> impl Iterator<Item = String> + 'a {
        let stack = &self.stack;
        stack
            .composited_effects
            .iter()
            .zip(&self.notes.composited_effects)
            .map(move |(composited, note)| {
                let parent_node_index = stack.nodes[composited.placement_node_index.0]
                    .parent_index
                    .expect("a composited node is inside another");
                let backdrop_read_text = match note.backdrop_read_reach {
                    Reach::Nothing => String::new(),
                    Reach::Within(rect) => format!(" reads=[{},{} {}x{}]", rect.x, rect.y, rect.width, rect.height),
                    Reach::Anywhere => " reads=anywhere".into(),
                };
                format!(
                    "composited {} space={}{backdrop_read_text}: {}",
                    node_namer.name_of_effect_node(composited.effect),
                    stack.name_of_node(parent_node_index, node_namer),
                    note.why
                )
            })
    }

    /// What the dump says of each backdrop filter or blend mode that reads only the layer it goes into.
    pub fn lines_of_effects_reading_only_own_layer<'a>(
        &'a self,
        node_namer: &'a dyn VisualContextNodeNamer,
    ) -> impl Iterator<Item = String> + 'a {
        self.notes.effects_reading_only_own_layer.iter().map(|effect| {
            format!(
                "{} is not composited: it reads only its own layer",
                node_namer.name_of_effect_node(*effect)
            )
        })
    }

    /// What the dump says of each canvas, video or nested context that was not given a layer, and why.
    pub fn lines_of_media_without_layers(&self) -> impl Iterator<Item = String> + '_ {
        self.notes
            .media_without_layers
            .iter()
            .map(|(content, why_no_layer)| format!("no layer for {}: {why_no_layer}", media_name(*content)))
    }

    /// The lines the overlay of layers shows of the stack past its layers, which WebContent sends with the plan: those
    /// of the effects the compositor composites, of the canvases, videos and nested contexts without layers, and of the
    /// effects that read only their own layer, with nodes named by their indices, as many and as long as a plan holds.
    pub fn note_lines_for_layers_overlay(&self) -> Vec<String> {
        wire::note_lines_a_plan_holds(
            self.lines_of_composited_effects(&NodeNamesFromIndices)
                .chain(self.lines_of_media_without_layers())
                .chain(self.lines_of_effects_reading_only_own_layer(&NodeNamesFromIndices)),
        )
    }

    /// What the dump says of each node the compositor animates whose content was not given layers of its own, and why.
    pub fn lines_of_animations_without_layers<'a>(
        &'a self,
        node_namer: &'a dyn VisualContextNodeNamer,
    ) -> impl Iterator<Item = String> + 'a {
        self.placement
            .animations_without_layers
            .iter()
            .map(|(animated_node, why_no_layer)| {
                let node_name = match animated_node {
                    AnimatedNode::Transform(spatial) => node_namer.name_of_spatial_node(*spatial),
                    AnimatedNode::Effect(effect) => node_namer.name_of_effect_node(*effect),
                };
                format!("no layer for animated {node_name}: {why_no_layer}")
            })
    }

    /// The lines of the Layers section of the dump, each with the layer it is about, if it is about one. There is one
    /// line per layer, bottom to top: its index, what it holds, the space it is rastered in, what it sits above, the
    /// records `describe_records` names for it, what it is clipped to as it is composited, and for a canvas or video
    /// whether the window server shows it, as `presentation` tells. A layer below the base layer, or one that can hold
    /// nothing, is left out. Then a line for each effect the compositor composites, each canvas, video or nested context
    /// without a layer, each animation without a layer, and each effect that reads only its own layer.
    pub fn layer_lines(
        &self,
        presentation: &WindowServerPresentationOfMedia,
        node_namer: &dyn VisualContextNodeNamer,
        describe_records: impl Fn(LayerIndex) -> Option<String>,
    ) -> Vec<(Option<LayerIndex>, String)> {
        let stack = &self.stack;
        let records_suffix_of = |layer_index: LayerIndex| {
            describe_records(layer_index).map_or_else(String::new, |records| format!(" records={records}"))
        };
        if let Some(why_list_is_one_layer) = stack.why_list_is_one_layer {
            let only_layer = LayerIndex(0);
            return vec![(
                Some(only_layer),
                format!(
                    "{}{}: {why_list_is_one_layer}",
                    stack.name_of_layer(only_layer, node_namer),
                    records_suffix_of(only_layer)
                ),
            )];
        }
        let mut lines = Vec::new();
        for (index, layer) in stack.layers.iter().enumerate() {
            let layer_index = LayerIndex(index);
            if layer_index < stack.base_layer || (layer_index != stack.base_layer && !layer.may_hold_content) {
                continue;
            }
            let node = &stack.nodes[layer.placement_node_index.0];
            let mut line = format!(
                "{}{}",
                stack.name_of_layer(layer_index, node_namer),
                records_suffix_of(layer_index)
            );
            for clip in &node.composite_clips {
                let clip_rect = clip.rect;
                line.push_str(&format!(
                    " composited within [{},{} {}x{}]{} of {}",
                    clip_rect.x,
                    clip_rect.y,
                    clip_rect.width,
                    clip_rect.height,
                    text_of_corner_radii_if_any(clip.corner_radii),
                    stack.name_of_space(clip.raster_space_index, node_namer)
                ));
            }
            if matches!(layer.content, LayerContent::Media { .. }) {
                match presentation.placements.of_layer(layer_index) {
                    Some(Ok(placement)) if presentation.layer_is_shown_by_window_server(layer_index) => {
                        line.push_str(&format!(" shown by the window server: {placement}"));
                    }
                    Some(Ok(_)) if !stack.composited_bands_around_layer(layer_index).is_empty() => {
                        line.push_str(" replayed: inside an effect the compositor composites");
                    }
                    Some(Ok(_)) => line.push_str(" replayed: under a layer the frame holds"),
                    Some(Err(why_window_server_cannot_show)) => {
                        line.push_str(&format!(" replayed: {why_window_server_cannot_show}"));
                    }
                    None => {}
                }
            }
            lines.push((Some(layer_index), line));
        }
        lines.extend(
            self.lines_of_composited_effects(node_namer)
                .chain(self.lines_of_media_without_layers())
                .chain(self.lines_of_animations_without_layers(node_namer))
                .chain(self.lines_of_effects_reading_only_own_layer(node_namer))
                .map(|line| (None, line)),
        );
        lines
    }

    /// The lines of `layer_lines`, each after `indent`.
    pub fn write_layer_lines(
        &self,
        output: &mut impl std::fmt::Write,
        indent: &str,
        presentation: &WindowServerPresentationOfMedia,
        node_namer: &dyn VisualContextNodeNamer,
        describe_records: impl Fn(LayerIndex) -> Option<String>,
    ) -> std::fmt::Result {
        for (_, line) in self.layer_lines(presentation, node_namer, describe_records) {
            writeln!(output, "{indent}{line}")?;
        }
        Ok(())
    }

    /// The layers that the record at `position` can be drawn into, in the tiles it reaches.
    pub fn layers_of_record(&self, position: ListPosition) -> Vec<LayerIndex> {
        let stack = &self.stack;
        if stack.replays_whole_list() {
            return vec![LayerIndex(0)];
        }
        for (index, layer) in stack.layers.iter().enumerate() {
            let LayerContent::Media { record_span, .. } = &layer.content else {
                continue;
            };
            if record_span.contains(position) {
                return vec![LayerIndex(index)];
            }
        }
        let Some(stretch) = self
            .placement
            .record_stretches
            .iter()
            .find(|stretch| stretch.record_span.contains(position))
        else {
            return Vec::new();
        };
        let Some(band_index) = stack
            .bands
            .iter()
            .position(|band| band.record_spans.iter().any(|span| span.contains(position)))
        else {
            return Vec::new();
        };
        let band = &stack.bands[band_index];
        let mut layers: Vec<LayerIndex> = if reaches_too_many_cells_to_follow_one_by_one(stretch.reach) {
            std::iter::once(band.layer_below_all_cuts)
                .chain(
                    band.cuts
                        .iter()
                        .take_while(|cut| cut.barrier_first_record_position <= stretch.record_span.begin)
                        .map(|cut| cut.layer_above_cut),
                )
                .collect()
        } else {
            tile_coords_touching(stretch.reach)
                .map(|coord| band.layer_of_record_in_cell(stretch.record_span.begin, coord.cell_rect()))
                .collect()
        };
        layers.sort_unstable();
        layers.dedup();
        layers
    }
}

/// What a plan of the layers of a list reads, all of which WebContent publishes.
pub struct LayerStackInputs<'a> {
    pub list: &'a DisplayListStorage,
    /// The tree as WebContent published it, without the samples of the compositor's animations and without a visual
    /// viewport transform of the compositor's.
    pub tree: &'a VisualContextTree,
    /// The scroll offsets WebContent published with the tree. A plan reads only those that
    /// `RasterSpaceAnchorsOfTree::leaves_out_scroll_offset_of_node` does not name.
    pub scroll_offsets: &'a [FloatPoint],
    /// The viewport in device pixels.
    pub viewport_rect: IntRect,
}

// An animation of the opacity or filter of the root isolation effect makes it more than a plain group at some sample.
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
                && !tree.effect_opacity_or_filter_is_animated(root)
        }
        _ => false,
    }
}

// Empty where no transform animation of the tree leaves the plane.
fn spatial_nodes_under_transform_animations_leaving_plane(tree: &VisualContextTree) -> Vec<bool> {
    let roots: Vec<SpatialNodeIndex> = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == crate::host::FfiVisualAnimationTargetKind::Transform)
        .flat_map(|animation| animation.node_indices.iter().map(|&index| SpatialNodeIndex(index)))
        .filter(|&node| tree.transform_animation_of_node_leaves_plane(node))
        .collect();
    if roots.is_empty() {
        return Vec::new();
    }
    tree.spatial_nodes_in_subtrees_of(&roots)
}

/// Why a list cannot be layered at all.
pub fn why_list_cannot_be_layered(
    inputs: &LayerStackInputs<'_>,
    layering_obstacles: &LayeringObstacles,
) -> Option<WhyListIsOneLayer> {
    if inputs.viewport_rect.is_empty() {
        return Some(WhyListIsOneLayer::EmptyViewport);
    }
    if inputs.list.run_count() == 0 {
        return Some(WhyListIsOneLayer::EmptyDisplayList);
    }
    if !root_isolation_effect_is_plain_group(inputs.tree) {
        return Some(WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup);
    }
    if layering_obstacles.mask_declared_under_effects_its_mask_is_not_under {
        return Some(WhyListIsOneLayer::MaskDeclaredUnderEffectsItsMaskIsNotUnder);
    }
    None
}

/// A group of runs, which is placed whole, with where it is placed.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedRunGroup {
    pub first_run_index: RunIndexInList,
    pub last_run_index: RunIndexInList,
    pub raster_space_index: RasterSpaceIndex,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    /// Where it can show in the space's raster coordinates, under the values the frame shows, except that content
    /// that moves on its own is taken to be anywhere its fixed clips let it show.
    pub reach: IntRect,
    pub moves_within_space: bool,
    /// Whether the group paints within the root group, rather than below it as the canvas background is.
    pub paints_within_root_group: bool,
    /// In the space's raster coordinates.
    pub reach_read_by_records_blending_with_root_group: Reach<IntRect>,
}

/// The index of a placed effect in `ListPlacement::placed_effects`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlacedEffectIndex(pub usize);

/// The index of a content in `ListPlacement::placed_media`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlacedMediaIndex(pub usize);

/// A stretch of records of one run group that is placed as one: the records of the run group's runs that are inside
/// the same effects the compositor can composite, or the runs of a content that is a layer of its own, which hold it
/// alone and may span run groups.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedRecordStretch {
    pub record_span: ListSpan,
    /// The run group of its first run.
    pub run_group_index: RunGroupIndex,
    /// Where the records can show, in the raster coordinates of the run group's space: where the innermost placed
    /// effect around them can draw, if there is one.
    pub reach: IntRect,
    pub placed_media_index: Option<PlacedMediaIndex>,
    /// The innermost effect the compositor can composite the stretch's records inside.
    pub placed_effect_index: Option<PlacedEffectIndex>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RecordStretchIndex(usize);

/// An effect whose content the compositor composites inside it, or may where the content does not find all it reads
/// in the layer it goes into, with where it is placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedEffect {
    pub effect: EffectNodeIndex,
    /// The placed effect around it, inside which the compositor composites it.
    pub placed_effect_around_index: Option<PlacedEffectIndex>,
    pub run_group_index: RunGroupIndex,
    /// Where compositing the effect can draw, in the raster coordinates of the run group's space: wherever the runs
    /// under it in the run group of its first run can show, and if it moves within its space, wherever it can show.
    pub reach: IntRect,
    /// What it reads of what is composited below it there.
    pub backdrop_read_reach: Reach<IntRect>,
    /// Whether what it reads below it is the content of the root group, which the bands it can read are only known of
    /// as the stack is cut into them.
    pub reads_root_group: bool,
    pub why_composited_regardless_of_layer_content: Option<WhyEffectIsComposited>,
}

/// A canvas, video or nested context that is a layer of its own, alone in its run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedMedia {
    pub content: LiveContent,
    /// How many drawings of the same content are painted before it.
    pub ordinal: u32,
    pub record_span: ListSpan,
    pub run_index: RunIndexInList,
    /// Where it can show, in the raster coordinates of the space of its run's group.
    pub reach: IntRect,
}

/// Where each part of a list is placed at the spaces a tree offers, under the values a frame shows. What it decides
/// holds for every sample of the animations of the tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListPlacement {
    pub layering_obstacles: LayeringObstacles,
    pub spaces: Vec<RasterSpace>,
    pub run_groups: Vec<PlacedRunGroup>,
    pub record_stretches: Vec<PlacedRecordStretch>,
    /// In paint order.
    pub placed_media: Vec<PlacedMedia>,
    pub media_without_layers: Vec<(LiveContent, WhyMediaHasNoLayer)>,
    pub animations_without_layers: Vec<(AnimatedNode, WhyAnimationHasNoLayer)>,
    /// Per raster space whose anchor the compositor animates and that holds content, where that content can show in
    /// the raster coordinates of the space it is in wherever the animation takes it.
    pub swept_reach_in_parent_space_by_space_index: Vec<Option<IntRect>>,
    /// In paint order, each after the one around it.
    pub placed_effects: Vec<PlacedEffect>,
    /// The nodes that read the backdrop of a backdrop filter or a blend mode, in paint order.
    pub backdrop_readers_in_paint_order: Vec<EffectNodeIndex>,
    pub why_compositor_cannot_composite_by_effect_index: Vec<Option<WhyCompositorCannotCompositeEffect>>,
    /// Per raster space, whether records its tiles hold move within it as nodes that move on their own, or that the
    /// compositor scrolls without the space following them, do. Canvases, videos and nested contexts with layers of
    /// their own hold none of those records.
    pub tiles_hold_content_moving_within_space_by_space_index: Vec<bool>,
}

fn text_of_corner_radii_if_any(radii: CornerRadii) -> String {
    if !radii.has_any_radius() {
        return String::new();
    }
    let corners = [radii.top_left, radii.top_right, radii.bottom_right, radii.bottom_left];
    let every_corner_is_circular = corners
        .iter()
        .all(|corner| corner.horizontal_radius == corner.vertical_radius);
    let corner_texts: Vec<String> = corners
        .iter()
        .map(|corner| {
            if every_corner_is_circular {
                corner.horizontal_radius.to_string()
            } else {
                format!("{}x{}", corner.horizontal_radius, corner.vertical_radius)
            }
        })
        .collect();
    format!(" radii=({})", corner_texts.join(","))
}

fn one_composite_clip_per_space_sorted_by_space(clips: &[CompositeClip]) -> Vec<CompositeClip> {
    let mut clip_per_space: Vec<CompositeClip> = Vec::with_capacity(clips.len());
    for clip in clips {
        match clip_per_space
            .iter_mut()
            .find(|clip_of_same_space| clip_of_same_space.raster_space_index == clip.raster_space_index)
        {
            Some(clip_of_same_space) => clip_of_same_space.rect = clip_of_same_space.rect.intersected(clip.rect),
            None => clip_per_space.push(*clip),
        }
    }
    clip_per_space.sort_by_key(|clip| clip.raster_space_index);
    clip_per_space
}

// Under a transform animation that leaves the plane, a sample can draw the media through a matrix that is not 2D
// affine, whatever the tree holds.
fn why_media_has_no_layer(
    inputs: &LayerStackInputs<'_>,
    geometry: &RasterSpaceGeometry,
    why_compositor_cannot_composite_effects_around: impl Fn(EffectNodeIndex) -> Option<WhyCompositorCannotCompositeEffect>,
    run_index: u32,
    media_record_position: ListPosition,
    reach_of_run_holding_only_media: IntRect,
    is_under_transform_animation_leaving_plane: bool,
) -> Option<WhyMediaHasNoLayer> {
    let (header, _) = inputs.list.record_at(media_record_position);
    if !matches!(
        header.command_type,
        DisplayListCommandType::DrawCanvas
            | DisplayListCommandType::DrawVideoFrame
            | DisplayListCommandType::DrawCompositedContext
    ) {
        return Some(WhyMediaHasNoLayer::DrawnInsideAnotherCommand);
    }
    let run = inputs.list.run(run_index as usize);
    if is_under_transform_animation_leaving_plane || !geometry.world.draw_matrix(run.context.spatial).is_2d_affine() {
        return Some(WhyMediaHasNoLayer::UnderTransformThatIsNot2DAffine);
    }
    if let Some(why) = why_compositor_cannot_composite_effects_around(run.context.effect) {
        return Some(WhyMediaHasNoLayer::InsideAnEffectTheCompositorCannotComposite(why));
    }
    if reach_of_run_holding_only_media.is_empty() {
        return Some(WhyMediaHasNoLayer::DrawsNothingVisible);
    }
    None
}

// Marks in each of `marks_by_effect_index` every effect node that holds a node marked there, in one walk of the tree.
fn mark_effects_around_marked_effects<const MARK_KIND_COUNT: usize>(
    tree: &VisualContextTree,
    marks_by_effect_index: &mut [Vec<bool>; MARK_KIND_COUNT],
) {
    let mut live_effects_parents_first = Vec::with_capacity(tree.effect_nodes.len());
    tree.visit_effect_nodes_parents_first(|index| live_effects_parents_first.push(index));
    for &index in live_effects_parents_first.iter().rev() {
        let parent = tree.effect_nodes[index].parent;
        if parent.is_none() {
            continue;
        }
        for marked_by_effect_index in marks_by_effect_index.iter_mut() {
            if marked_by_effect_index[index] {
                marked_by_effect_index[parent.0 as usize] = true;
            }
        }
    }
}

// Marks the effect of each run that `marks_run` marks.
fn effects_of_marked_runs(
    list: &DisplayListStorage,
    tree: &VisualContextTree,
    marks_run: impl Fn(usize) -> bool,
) -> Vec<bool> {
    let mut marked_by_effect_index = vec![false; tree.effect_nodes.len()];
    for (run_index, run) in list.runs().enumerate() {
        if !run.context.effect.is_none() && marks_run(run_index) {
            marked_by_effect_index[run.context.effect.0 as usize] = true;
        }
    }
    marked_by_effect_index
}

// Per effect node, whether it is or holds the effect of a run sorted by depth, of a record that blends with what is
// below it, or an effect that reads what is below it.
struct WhatEffectsHold {
    run_sorted_by_depth_by_effect_index: Vec<bool>,
    records_blending_with_what_is_below_them_by_effect_index: Vec<bool>,
    effect_reading_what_is_below_it_by_effect_index: Vec<bool>,
}

// Why each effect is composited whatever its layers hold, and the effects that are, in the order they became so.
struct WhyEffectsAreCompositedRegardlessOfLayerContent {
    why_by_effect_index: Vec<Option<WhyEffectIsComposited>>,
    composited_effects: Vec<EffectNodeIndex>,
}

impl WhyEffectsAreCompositedRegardlessOfLayerContent {
    fn set(&mut self, effect: EffectNodeIndex, why: WhyEffectIsComposited) {
        let why_composited = &mut self.why_by_effect_index[effect.0 as usize];
        if why_composited.is_none() {
            self.composited_effects.push(effect);
        }
        *why_composited = Some(why);
    }

    fn set_unless_composited(&mut self, effect: EffectNodeIndex, why: WhyEffectIsComposited) {
        if self.why_by_effect_index[effect.0 as usize].is_none() {
            self.set(effect, why);
        }
    }

    fn mark_effects_around_as_composited(
        &mut self,
        tree: &VisualContextTree,
        mut effect_around: EffectNodeIndex,
        why: WhyEffectIsComposited,
    ) {
        while !effect_around.is_none() && tree.root_isolation_effect != Some(effect_around) {
            if tree.effect_pushes_layer_at_replay(effect_around) {
                self.set_unless_composited(effect_around, why);
            }
            effect_around = tree.effect_nodes[effect_around.0 as usize].parent;
        }
    }
}

/// Where each part of the list is placed among `spaces`, given the geometry of each of them.
fn place_list(
    inputs: &LayerStackInputs<'_>,
    spaces: RasterSpacesForPlacing,
    geometries: &[RasterSpaceGeometryForPlacing],
    nesting_of_live_effects: &NestingOfLiveEffects<'_>,
) -> ListPlacement {
    let tree = inputs.tree;
    let list = inputs.list;
    let effect_clips = list.effect_clip_plan(tree);
    let run_groups = list.run_groups(tree);
    let mut tables = PlacementTables::new(tree, spaces, geometries);
    tables.note_animated_spaces_holding_every_run_of_effects(tree, list);
    let tables = tables;
    let spaces = tables.spaces();
    let runs_reaching_outside = RunsReachingOutsideTheirContext::new(list);
    let mut run_blends_with_root_group_by_run_index = vec![false; list.run_count()];
    for record in &list.summary().destination_blending_records {
        let effect = list.run(record.run_index as usize).context.effect;
        if tree.content_under_effect_composites_into_root_group(effect) {
            run_blends_with_root_group_by_run_index[record.run_index as usize] = true;
        }
    }
    // Placing decides for the filter that the tree holds and for each filter that an animation can give the effect.
    let nearest_spreading_effects = nearest_effects_spreading_at_some_sample(tree);
    let keyframe_filters = KeyframeFiltersOfEffects::of(tree);
    // A space whose anchor the compositor animates rasters at a scale of its own, so how far a filter reads in the
    // pixels of tiles is told by the deepest space of the filter. Where a transform the compositor animates within that
    // space scales the filter, it is not known.
    let why_compositor_cannot_composite_by_effect_index: Vec<Option<WhyCompositorCannotCompositeEffect>> = tree
        .effect_nodes
        .iter()
        .enumerate()
        .map(|(index, effect_node)| {
            let effect = EffectNodeIndex(index as u32);
            if keyframe_filters.filter_may_move_what_it_filters(tree, effect) {
                return Some(WhyCompositorCannotCompositeEffect::FilterMovesWhatItFilters);
            }
            if !filter_may_spread_at_some_sample(tree, effect) {
                return None;
            }
            let deepest_space = tables.deepest_space_of(effect_node.spatial);
            let read_reach_in_device_pixels = (!tables
                .animation_samples_move_spatial_within_space(effect_node.spatial, deepest_space))
            .then(|| {
                keyframe_filters.filter_read_reach_in_device_pixels(
                    tree,
                    effect,
                    &geometries[deepest_space.0]
                        .raster
                        .world
                        .draw_matrix(effect_node.spatial),
                )
            })
            .flatten();
            read_reach_in_device_pixels
                .is_none()
                .then_some(WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed)
        })
        .collect();
    // The walk up from an effect stops at the root isolation effect, which holds no reason.
    let why_compositor_cannot_composite_effect_or_effect_around_by_effect_index = std::cell::OnceCell::new();
    let why_compositor_cannot_composite_effect_or_effects_around = |effect: EffectNodeIndex| {
        why_compositor_cannot_composite_effect_or_effect_around_by_effect_index
            .get_or_init(|| {
                let mut why_by_effect_index: Vec<Option<WhyCompositorCannotCompositeEffect>> =
                    vec![None; tree.effect_nodes.len()];
                tree.visit_effect_nodes_parents_first(|index| {
                    if tree.root_isolation_effect == Some(EffectNodeIndex(index as u32)) {
                        return;
                    }
                    let parent = tree.effect_nodes[index].parent;
                    why_by_effect_index[index] = why_compositor_cannot_composite_by_effect_index[index]
                        .or_else(|| why_by_effect_index.get(parent.0 as usize).copied().flatten());
                });
                why_by_effect_index
            })
            .get(effect.0 as usize)
            .copied()
            .flatten()
    };
    let content_is_spread_by_filters = nearest_spreading_effects.iter().any(|effect| !effect.is_none());
    let outermost_effects_with_filters_at_some_sample = if content_is_spread_by_filters {
        outermost_effects_with_filters(tree, true)
    } else {
        Vec::new()
    };
    let filters_and_clips = FiltersAndClipsOfList {
        tree,
        nearest_spreading_effects: &nearest_spreading_effects,
        effect_clips,
        outermost_effects_with_filters: &outermost_effects_with_filters_at_some_sample,
    };
    let mut reach_of_run_by_run_index = vec![IntRect::default(); list.run_count()];
    let mut run_moves_within_space_by_run_index = vec![false; list.run_count()];

    let is_animated_space = |space: RasterSpaceIndex| {
        matches!(
            spaces[space.0].anchor_kind,
            RasterSpaceAnchorKind::AnimatedTransform { .. }
        )
    };
    let mut runs_by_animated_space: HashMap<RasterSpaceIndex, Vec<usize>> = HashMap::new();
    for run_index in 0..list.run_count() {
        let space = tables.deepest_space_of(list.run(run_index).context.spatial);
        if is_animated_space(space) {
            runs_by_animated_space.entry(space).or_default().push(run_index);
        }
    }
    let mut animated_space_is_decided = vec![false; spaces.len()];
    let mut swept_reach_in_parent_space_by_space_index: Vec<Option<IntRect>> = vec![None; spaces.len()];
    let mut animations_without_layers: Vec<(AnimatedNode, WhyAnimationHasNoLayer)> = Vec::new();

    let mut placed_run_groups: Vec<PlacedRunGroup> =
        Vec::with_capacity(run_groups.first_and_last_run_of_groups().len());
    for &(first_run_index, last_run_index) in run_groups.first_and_last_run_of_groups() {
        for run_index in first_run_index as usize..=last_run_index as usize {
            let space = tables.deepest_space_of(list.run(run_index).context.spatial);
            if !is_animated_space(space) || animated_space_is_decided[space.0] {
                continue;
            }
            animated_space_is_decided[space.0] = true;
            let canvases_and_videos_placed_before = list
                .summary()
                .live_content_records
                .iter()
                .filter(|media| matches!(media.content, LiveContent::Canvas(_) | LiveContent::VideoFrame(_)))
                .filter_map(|media| {
                    let media_run_index = media.record.run_index as usize;
                    let placed_group = placed_run_groups.get(run_groups.group_of_run(media_run_index).0)?;
                    Some((
                        placed_group.raster_space_index,
                        reach_of_run_by_run_index[media_run_index],
                    ))
                });
            match swept_reach_of_animated_space_in_parent_space(
                inputs,
                &tables,
                geometries,
                &filters_and_clips,
                &why_compositor_cannot_composite_by_effect_index,
                space,
                &runs_by_animated_space[&space],
                canvases_and_videos_placed_before,
            ) {
                Ok(swept_reach) => swept_reach_in_parent_space_by_space_index[space.0] = Some(swept_reach),
                Err(why_no_layer) => {
                    tables.refuse_animated_space(space);
                    animations_without_layers.push((AnimatedNode::Transform(spaces[space.0].anchor), why_no_layer));
                }
            }
        }
        placed_run_groups.push({
            let run_indices = first_run_index as usize..=last_run_index as usize;
            let mut raster_space_index = run_indices
                .clone()
                .map(|run_index| {
                    runs_reaching_outside.deepest_space_of_run(tree, &tables, run_index, list.run(run_index))
                })
                .reduce(|first, second| lowest_common_ancestor_space(spaces, first, second))
                .unwrap_or(RasterSpaceIndex::VIEWPORT);
            let composite_clips = loop {
                let mut clips_of_runs = run_indices.clone().map(|run_index| {
                    runs_reaching_outside.composite_clips_of_run_in(
                        tree,
                        &tables,
                        run_index,
                        list.run(run_index),
                        raster_space_index,
                    )
                });
                let first_clips = clips_of_runs.next().flatten();
                if let Some(first_clips) = first_clips
                    && clips_of_runs.all(|clips| clips.as_deref() == Some(&*first_clips))
                {
                    break one_composite_clip_per_space_sorted_by_space(&first_clips);
                }
                if raster_space_index == RasterSpaceIndex::VIEWPORT {
                    break Vec::new();
                }
                raster_space_index = spaces[raster_space_index.0].parent_index;
            };
            let geometry = &geometries[raster_space_index.0];
            let mut moves_within_space = false;
            for run_index in run_indices.clone() {
                let run_moves_within_space =
                    tables.context_moves_within_space(tree, list.run(run_index).context, raster_space_index)
                        || runs_reaching_outside.inline_clips_of_run_move_within_space(
                            tree,
                            &tables,
                            run_index,
                            raster_space_index,
                        );
                run_moves_within_space_by_run_index[run_index] = run_moves_within_space;
                moves_within_space |= run_moves_within_space;
            }
            let mut reach_of_run_group = IntRect::default();
            let mut reach_read_by_records_blending_with_root_group = Reach::Nothing;
            for run_index in run_indices {
                let run = list.run(run_index);
                let ink_bounds_unless_moving_within_space = (!moves_within_space).then(|| run.ink_bounds.to_float());
                let reach_of_run = geometry.part_of_extent_reached_by(geometry.reach_of_content(
                    &filters_and_clips,
                    ink_bounds_unless_moving_within_space,
                    run.context,
                ));
                reach_of_run_by_run_index[run_index] = reach_of_run;
                reach_of_run_group = reach_of_run_group.united(reach_of_run);
                if run_blends_with_root_group_by_run_index[run_index] {
                    let blend_reads_below_where_it_draws = Reach::Within(reach_of_run);
                    reach_read_by_records_blending_with_root_group =
                        reach_read_by_records_blending_with_root_group.united(blend_reads_below_where_it_draws);
                }
            }
            let paints_within_root_group = match tree.root_isolation_effect {
                None => true,
                Some(root_isolation_effect) => (first_run_index..=last_run_index).any(|run_index| {
                    let mut effect = list.run(run_index as usize).context.effect;
                    while !effect.is_none() && effect != root_isolation_effect {
                        effect = tree.effect_nodes[effect.0 as usize].parent;
                    }
                    effect == root_isolation_effect
                }),
            };
            PlacedRunGroup {
                first_run_index: RunIndexInList(first_run_index),
                last_run_index: RunIndexInList(last_run_index),
                raster_space_index,
                composite_clips,
                reach: reach_of_run_group,
                moves_within_space,
                paints_within_root_group,
                reach_read_by_records_blending_with_root_group,
            }
        });
    }

    let mut first_run_index_by_effect_index = vec![usize::MAX; tree.effect_nodes.len()];
    let mut effects_in_paint_order_with_outer_before_inner = Vec::new();
    for run_index in 0..list.run_count() {
        let mut effect = list.run(run_index).context.effect;
        let mut newly_reached_effects_inner_to_outer = Vec::new();
        while !effect.is_none() && first_run_index_by_effect_index[effect.0 as usize] == usize::MAX {
            first_run_index_by_effect_index[effect.0 as usize] = run_index;
            newly_reached_effects_inner_to_outer.push(effect);
            effect = tree.effect_nodes[effect.0 as usize].parent;
        }
        effects_in_paint_order_with_outer_before_inner.extend(newly_reached_effects_inner_to_outer.into_iter().rev());
    }
    let mut last_run_index_by_effect_index: Vec<Option<usize>> = vec![None; tree.effect_nodes.len()];
    for run_index in (0..list.run_count()).rev() {
        let mut effect = list.run(run_index).context.effect;
        while !effect.is_none() && last_run_index_by_effect_index[effect.0 as usize].is_none() {
            last_run_index_by_effect_index[effect.0 as usize] = Some(run_index);
            effect = tree.effect_nodes[effect.0 as usize].parent;
        }
    }
    let run_group_of_effect =
        |effect: EffectNodeIndex| run_groups.group_of_run(first_run_index_by_effect_index[effect.0 as usize]);
    let mut reach_by_effect_index = vec![IntRect::default(); tree.effect_nodes.len()];
    for (run_index, &reach_of_run) in reach_of_run_by_run_index.iter().enumerate() {
        let mut effect = list.run(run_index).context.effect;
        while !effect.is_none() {
            if run_group_of_effect(effect) == run_groups.group_of_run(run_index) {
                let reach_of_effect = &mut reach_by_effect_index[effect.0 as usize];
                *reach_of_effect = reach_of_effect.united(reach_of_run);
            }
            effect = tree.effect_nodes[effect.0 as usize].parent;
        }
    }
    let space_of_run = |run_index: usize| placed_run_groups[run_groups.group_of_run(run_index).0].raster_space_index;

    let mut placed_media: Vec<PlacedMedia> = Vec::new();
    let mut media_without_layers = Vec::new();
    let spatial_node_is_under_transform_animation_leaving_plane =
        spatial_nodes_under_transform_animations_leaving_plane(tree);
    for live_content_record in &list.summary().live_content_records {
        let content = live_content_record.content;
        if matches!(content, LiveContent::ImageFrame(_)) {
            continue;
        }
        let record: IndexedRecord = live_content_record.record;
        let (run_index, record_position) = (record.run_index, record.position());
        let why_no_layer = if run_groups.run_is_sorted_by_depth(run_index as usize) {
            Some(WhyMediaHasNoLayer::SortedByDepthInA3DSortingContext)
        } else {
            why_media_has_no_layer(
                inputs,
                &geometries[space_of_run(run_index as usize).0].raster,
                why_compositor_cannot_composite_effect_or_effects_around,
                run_index,
                record_position,
                reach_of_run_by_run_index[run_index as usize],
                spatial_node_is_under_transform_animation_leaving_plane
                    .get(list.run(run_index as usize).context.spatial.0 as usize)
                    .copied()
                    .unwrap_or(false),
            )
        };
        match why_no_layer {
            None => {
                let record_span = ListSpan {
                    begin: list.start_of_run(run_index as usize),
                    end: list.end_of_run(run_index as usize),
                };
                debug_assert_eq!(
                    record_position, record_span.begin,
                    "a canvas, video or nested context is alone in its run"
                );
                placed_media.push(PlacedMedia {
                    content,
                    ordinal: 0,
                    record_span,
                    run_index: RunIndexInList(run_index),
                    reach: reach_of_run_by_run_index[run_index as usize],
                });
            }
            Some(why_no_layer) => media_without_layers.push((content, why_no_layer)),
        }
    }
    if placed_media.len() > MAXIMUM_MEDIA_LAYER_COUNT {
        let nested_contexts_after_canvases_and_videos =
            |media: &PlacedMedia| matches!(media.content, LiveContent::CompositedContext(_));
        placed_media.sort_by_key(nested_contexts_after_canvases_and_videos);
        for media_past_limit in placed_media.drain(MAXIMUM_MEDIA_LAYER_COUNT..) {
            media_without_layers.push((media_past_limit.content, WhyMediaHasNoLayer::MoreMediaThanLayersFor));
        }
        placed_media.sort_by_key(|media| media.record_span.begin);
    }

    let mut why_composited_regardless_of_layer_content = WhyEffectsAreCompositedRegardlessOfLayerContent {
        why_by_effect_index: vec![None; tree.effect_nodes.len()],
        composited_effects: Vec::new(),
    };
    for media in &placed_media {
        why_composited_regardless_of_layer_content.mark_effects_around_as_composited(
            tree,
            list.run(media.run_index.0 as usize).context.effect,
            WhyEffectIsComposited::ItHoldsMedia(media.content),
        );
    }

    let backdrop_reader_by_effect_index = backdrop_reader_by_reading_effect_index(tree);
    let effect_reads_what_is_below_it = |effect: EffectNodeIndex| {
        backdrop_reader_by_effect_index[effect.0 as usize].is_some()
            || match &tree.effect_nodes[effect.0 as usize].data {
                EffectNodeData::Effects(effects) => effects.backdrop_filter.is_some(),
                EffectNodeData::Mask(mask) => mask.masks_backdrop,
                _ => false,
            }
    };
    let canvas_or_video_painted_before_can_show_under =
        |first_run_index: usize, raster_space_index: RasterSpaceIndex, reach: IntRect| {
            placed_media.iter().any(|media| {
                matches!(media.content, LiveContent::Canvas(_) | LiveContent::VideoFrame(_))
                    && (media.run_index.0 as usize) < first_run_index
                    && (space_of_run(media.run_index.0 as usize) != raster_space_index
                        || !media.reach.intersected(reach).is_empty())
            })
        };
    let mut animated_nodes_with_layers_count = 0;

    let space_holds_placed_content_by_space_index = std::cell::OnceCell::new();
    let space_holds_placed_content = |space: RasterSpaceIndex| {
        space_holds_placed_content_by_space_index.get_or_init(|| {
            let mut holds_by_space_index = vec![false; spaces.len()];
            for run_group in &placed_run_groups {
                holds_by_space_index[run_group.raster_space_index.0] = true;
            }
            holds_by_space_index
        })[space.0]
    };
    for (index, effect_node) in tree.effect_nodes.iter().enumerate() {
        let effect = EffectNodeIndex(index as u32);
        let Some(space) = tables.animated_space_holding_every_run_of_effect(effect) else {
            continue;
        };
        let applies_over_animated_transform = swept_reach_in_parent_space_by_space_index[space.0].is_some()
            && !tables.moves_with(effect_node.spatial, space)
            && space_holds_placed_content(space);
        if !applies_over_animated_transform || !tree.effect_pushes_layer_at_replay(effect) {
            continue;
        }
        why_composited_regardless_of_layer_content.set_unless_composited(
            effect,
            WhyEffectIsComposited::ItAppliesOverATransformTheCompositorAnimates,
        );
        why_composited_regardless_of_layer_content.mark_effects_around_as_composited(
            tree,
            effect_node.parent,
            WhyEffectIsComposited::ItHoldsAnEffectTheCompositorComposites,
        );
    }

    let what_effects_hold_once_worked_out = std::cell::OnceCell::new();
    let what_effects_hold = || {
        what_effects_hold_once_worked_out.get_or_init(|| {
            let mut runs_holding_records_blending_with_what_is_below_them = vec![false; list.run_count()];
            for record in &list.summary().destination_blending_records {
                runs_holding_records_blending_with_what_is_below_them[record.run_index as usize] = true;
            }
            let mut effect_reads_what_is_below_it_by_effect_index = vec![false; tree.effect_nodes.len()];
            for &effect in &effects_in_paint_order_with_outer_before_inner {
                effect_reads_what_is_below_it_by_effect_index[effect.0 as usize] =
                    effect_reads_what_is_below_it(effect);
            }
            let mut marks = [
                effects_of_marked_runs(list, tree, |run_index| run_groups.run_is_sorted_by_depth(run_index)),
                effects_of_marked_runs(list, tree, |run_index| {
                    runs_holding_records_blending_with_what_is_below_them[run_index]
                }),
                effect_reads_what_is_below_it_by_effect_index,
            ];
            mark_effects_around_marked_effects(tree, &mut marks);
            let [
                run_sorted_by_depth_by_effect_index,
                records_blending_with_what_is_below_them_by_effect_index,
                effect_reading_what_is_below_it_by_effect_index,
            ] = marks;
            WhatEffectsHold {
                run_sorted_by_depth_by_effect_index,
                records_blending_with_what_is_below_them_by_effect_index,
                effect_reading_what_is_below_it_by_effect_index,
            }
        })
    };
    let mut animated_effects_in_paint_order = effects_whose_opacity_or_filter_the_compositor_animates(tree);
    animated_effects_in_paint_order.sort_by_key(|(effect, _)| first_run_index_by_effect_index[effect.0 as usize]);
    for (effect, filter_is_animated) in animated_effects_in_paint_order {
        let first_run_index = first_run_index_by_effect_index[effect.0 as usize];
        if last_run_index_by_effect_index[effect.0 as usize].is_none() {
            continue;
        }
        if tree.root_isolation_effect == Some(effect)
            || !matches!(tree.effect_nodes[effect.0 as usize].data, EffectNodeData::Effects(_))
        {
            continue;
        }
        let why_no_layer = (|| {
            if filter_is_animated && filter_may_spread_at_some_sample(tree, effect) {
                return Some(WhyAnimationHasNoLayer::ItsFilterSpreadsWhatItFilters);
            }
            if let Some(why) = why_compositor_cannot_composite_effect_or_effects_around(effect) {
                return Some(WhyAnimationHasNoLayer::InsideAnEffectTheCompositorCannotComposite(why));
            }
            let what_effects_hold = what_effects_hold();
            if what_effects_hold.run_sorted_by_depth_by_effect_index[effect.0 as usize] {
                return Some(WhyAnimationHasNoLayer::SortedByDepthInA3DSortingContext);
            }
            if what_effects_hold.records_blending_with_what_is_below_them_by_effect_index[effect.0 as usize] {
                return Some(WhyAnimationHasNoLayer::ItHoldsRecordsThatBlendWithWhatIsBelowThem);
            }
            if what_effects_hold.effect_reading_what_is_below_it_by_effect_index[effect.0 as usize] {
                return Some(WhyAnimationHasNoLayer::ItHoldsAnEffectThatReadsWhatIsBelowIt);
            }
            if canvas_or_video_painted_before_can_show_under(
                first_run_index,
                space_of_run(first_run_index),
                reach_by_effect_index[effect.0 as usize],
            ) {
                return Some(WhyAnimationHasNoLayer::ItCanShowOverAnEarlierCanvasOrVideo);
            }
            if animated_nodes_with_layers_count == MAXIMUM_ANIMATED_NODES_WITH_LAYERS {
                return Some(WhyAnimationHasNoLayer::MoreAnimationsThanLayersFor);
            }
            None
        })();
        if let Some(why_no_layer) = why_no_layer {
            animations_without_layers.push((AnimatedNode::Effect(effect), why_no_layer));
            continue;
        }
        animated_nodes_with_layers_count += 1;
        why_composited_regardless_of_layer_content
            .set_unless_composited(effect, WhyEffectIsComposited::TheCompositorAnimatesIt);
        why_composited_regardless_of_layer_content.mark_effects_around_as_composited(
            tree,
            tree.effect_nodes[effect.0 as usize].parent,
            WhyEffectIsComposited::ItHoldsAnEffectTheCompositorComposites,
        );
    }

    for index in 0..placed_media.len() {
        placed_media[index].ordinal = placed_media[..index]
            .iter()
            .filter(|earlier| earlier.content == placed_media[index].content)
            .count() as u32;
    }

    let mut backdrop_read_reach_by_effect_index = vec![Reach::Nothing; tree.effect_nodes.len()];
    let mut backdrop_readers_in_paint_order = Vec::new();
    // Empty until an effect reads the root group.
    let mut reads_root_group_by_effect_index: Vec<bool> = Vec::new();
    for &effect in &effects_in_paint_order_with_outer_before_inner {
        let Some(backdrop_reader) = backdrop_reader_by_effect_index[effect.0 as usize] else {
            continue;
        };
        let run_group = &placed_run_groups[run_group_of_effect(effect).0];
        let geometry = &geometries[run_group.raster_space_index.0];
        let reach_of_effect = reach_by_effect_index[effect.0 as usize];
        let (backdrop_read_reach, reads_nothing_past_mirrored_edge) = match backdrop_reader.effect_with_backdrop_filter
        {
            Some(effect_with_backdrop_filter) => {
                let effect_node = &tree.effect_nodes[effect_with_backdrop_filter.0 as usize];
                let EffectNodeData::Effects(effects) = &effect_node.data else {
                    unreachable!("a backdrop filter belongs to an effects node");
                };
                let backdrop_filter = effects
                    .backdrop_filter
                    .as_ref()
                    .expect("a reader with a backdrop filter has one");
                // Where a transform the compositor animates moves or scales the backdrop filter within the space, a
                // sample can read anywhere.
                let backdrop_read_reach = if tables
                    .animation_samples_move_spatial_within_space(effect_node.spatial, run_group.raster_space_index)
                {
                    Reach::Anywhere
                } else if run_group.moves_within_space {
                    crate::display_list::tile_keys::filter_read_reach_in_device_pixels(
                        &backdrop_filter.filter,
                        &geometry.raster.world.draw_matrix(effect_node.spatial),
                    )
                    .map_or(Reach::Anywhere, |read_reach_in_device_pixels| {
                        Reach::Within(run_group.reach.inflated_edges(
                            read_reach_in_device_pixels,
                            read_reach_in_device_pixels,
                            read_reach_in_device_pixels,
                            read_reach_in_device_pixels,
                        ))
                    })
                } else {
                    backdrop_filter_read_reach(tree, &geometry.raster, effect_with_backdrop_filter)
                };
                (
                    backdrop_read_reach,
                    crate::display_list::tile_keys::backdrop_filter_reads_nothing_past_mirrored_edge(
                        &backdrop_filter.filter,
                    ),
                )
            }
            None => (Reach::Within(reach_of_effect), true),
        };
        backdrop_read_reach_by_effect_index[effect.0 as usize] = backdrop_read_reach;
        let reading_effect_node = &tree.effect_nodes[effect.0 as usize];
        let backdrop_root = if reading_effect_node.parent.is_none() {
            None
        } else {
            tree.nearest_layer_at_or_above(reading_effect_node.parent)
                .filter(|root| tree.root_isolation_effect != Some(*root))
        };
        let why_composited = if !reads_nothing_past_mirrored_edge {
            Some(WhyEffectIsComposited::ItsBackdropFilterReadsPastTheEdgeOfTheFrame)
        } else if let Some(backdrop_root) = backdrop_root {
            let backdrop_read_tiles = match backdrop_read_reach {
                Reach::Within(rect) => Reach::Within(expanded_to_tile_grid(rect)),
                backdrop_read_reach => backdrop_read_reach,
            };
            let read_tiles_cut_by_earlier_composited_effect = why_composited_regardless_of_layer_content
                .composited_effects
                .iter()
                .any(|&other_effect| {
                    other_effect != effect
                        && first_run_index_by_effect_index[other_effect.0 as usize]
                            < first_run_index_by_effect_index[effect.0 as usize]
                        && nesting_of_live_effects.is_strictly_inside(other_effect, backdrop_root)
                        && !nesting_of_live_effects.is_strictly_inside(effect, other_effect)
                        && backdrop_read_tiles.meets(Reach::Within(
                            reach_by_effect_index[other_effect.0 as usize].united(
                                match backdrop_read_reach_by_effect_index[other_effect.0 as usize] {
                                    Reach::Within(rect) => rect,
                                    _ => IntRect::default(),
                                },
                            ),
                        ))
                });
            let read_tiles_cut_by_earlier_media = placed_media.iter().any(|media| {
                let effect_of_media = list.run(media.run_index.0 as usize).context.effect;
                (media.run_index.0 as usize) < first_run_index_by_effect_index[effect.0 as usize]
                    && nesting_of_live_effects.is_or_is_inside(effect_of_media, backdrop_root)
                    && backdrop_read_tiles.meets(Reach::Within(media.reach))
            });
            (read_tiles_cut_by_earlier_composited_effect || read_tiles_cut_by_earlier_media)
                .then_some(WhyEffectIsComposited::ItReadsWhatItsOwnLayerDoesNotHold)
        } else {
            None
        };
        backdrop_readers_in_paint_order.push(effect);
        if backdrop_root.is_none() {
            reads_root_group_by_effect_index.resize(tree.effect_nodes.len(), false);
            reads_root_group_by_effect_index[effect.0 as usize] = true;
        }
        let Some(why_composited) = why_composited else {
            continue;
        };
        why_composited_regardless_of_layer_content.set(effect, why_composited);
        why_composited_regardless_of_layer_content.mark_effects_around_as_composited(
            tree,
            reading_effect_node.parent,
            WhyEffectIsComposited::ItHoldsAnEffectTheCompositorComposites,
        );
    }

    // Effects come after the effects around them, so the innermost placed effect at or around each is known by then.
    let mut innermost_placed_effect_at_or_around_by_effect_index: Vec<Option<PlacedEffectIndex>> =
        vec![None; tree.effect_nodes.len()];
    let mut placed_effects: Vec<PlacedEffect> = Vec::new();
    for &effect in &effects_in_paint_order_with_outer_before_inner {
        let parent = tree.effect_nodes[effect.0 as usize].parent;
        let innermost_placed_effect_around = innermost_placed_effect_at_or_around_by_effect_index
            .get(parent.0 as usize)
            .copied()
            .flatten();
        innermost_placed_effect_at_or_around_by_effect_index[effect.0 as usize] = innermost_placed_effect_around;
        let why_composited_regardless_of_layer_content =
            why_composited_regardless_of_layer_content.why_by_effect_index[effect.0 as usize];
        let reads_root_group = reads_root_group_by_effect_index
            .get(effect.0 as usize)
            .copied()
            .unwrap_or(false);
        if why_composited_regardless_of_layer_content.is_none() && !reads_root_group {
            continue;
        }
        innermost_placed_effect_at_or_around_by_effect_index[effect.0 as usize] =
            Some(PlacedEffectIndex(placed_effects.len()));
        placed_effects.push(PlacedEffect {
            effect,
            placed_effect_around_index: innermost_placed_effect_around,
            run_group_index: run_group_of_effect(effect),
            reach: reach_by_effect_index[effect.0 as usize],
            backdrop_read_reach: backdrop_read_reach_by_effect_index[effect.0 as usize],
            reads_root_group,
            why_composited_regardless_of_layer_content,
        });
    }
    let innermost_placed_effect_of_run = |run_index: usize| {
        innermost_placed_effect_at_or_around_by_effect_index
            .get(list.run(run_index).context.effect.0 as usize)
            .copied()
            .flatten()
    };

    let mut record_stretches: Vec<PlacedRecordStretch> = Vec::with_capacity(placed_run_groups.len());
    let mut remaining_placed_media = placed_media.iter().enumerate().peekable();
    let mut tiles_hold_content_moving_within_space_by_space_index = vec![false; spaces.len()];
    for run_index in 0..list.run_count() {
        let run_group_index = run_groups.group_of_run(run_index);
        let innermost_placed_effect = innermost_placed_effect_of_run(run_index);
        if let Some((placed_media_index, media)) =
            remaining_placed_media.next_if(|(_, media)| media.run_index.0 as usize == run_index)
        {
            record_stretches.push(PlacedRecordStretch {
                record_span: media.record_span,
                run_group_index,
                reach: media.reach,
                placed_media_index: Some(PlacedMediaIndex(placed_media_index)),
                placed_effect_index: innermost_placed_effect,
            });
            continue;
        }
        if run_moves_within_space_by_run_index[run_index] {
            tiles_hold_content_moving_within_space_by_space_index[space_of_run(run_index).0] = true;
        }
        let run_span = ListSpan {
            begin: list.start_of_run(run_index),
            end: list.end_of_run(run_index),
        };
        let run_draws_nothing = list.run(run_index).ink_bounds.is_empty();
        let reach_of_run_in_stretch = match innermost_placed_effect {
            _ if run_draws_nothing => IntRect::default(),
            Some(placed_effect_index) => placed_effects[placed_effect_index.0].reach,
            None => reach_of_run_by_run_index[run_index],
        };
        match record_stretches.last_mut() {
            Some(last)
                if last.run_group_index == run_group_index
                    && last.placed_effect_index == innermost_placed_effect
                    && last.placed_media_index.is_none() =>
            {
                last.record_span.end = run_span.end;
                last.reach = last.reach.united(reach_of_run_in_stretch);
            }
            _ => record_stretches.push(PlacedRecordStretch {
                record_span: run_span,
                run_group_index,
                reach: reach_of_run_in_stretch,
                placed_media_index: None,
                placed_effect_index: innermost_placed_effect,
            }),
        }
    }

    ListPlacement {
        layering_obstacles: LayeringObstacles::of_with_nesting_of_live_effects(list, tree, nesting_of_live_effects),
        spaces: spaces.to_vec(),
        run_groups: placed_run_groups,
        record_stretches,
        placed_media,
        media_without_layers,
        animations_without_layers,
        swept_reach_in_parent_space_by_space_index,
        placed_effects,
        backdrop_readers_in_paint_order,
        why_compositor_cannot_composite_by_effect_index,
        tiles_hold_content_moving_within_space_by_space_index,
    }
}

/// Where content of `space`, whose anchor the compositor animates, can show in the raster coordinates of the space it
/// is in wherever the animation takes it, or why the space holds none of it: content that a frame cannot composite
/// through the anchor's transform, or whose reach nothing bounds, which would cut every tile around it, or that can
/// show over a canvas or video painted before it, which would take those from the window server for the whole
/// animation.
#[allow(clippy::too_many_arguments)]
fn swept_reach_of_animated_space_in_parent_space(
    inputs: &LayerStackInputs<'_>,
    tables: &PlacementTables,
    geometries: &[RasterSpaceGeometryForPlacing],
    filters_and_clips: &FiltersAndClipsOfList<'_>,
    why_compositor_cannot_composite_by_effect_index: &[Option<WhyCompositorCannotCompositeEffect>],
    space: RasterSpaceIndex,
    run_indices_of_space: &[usize],
    canvases_and_videos_placed_before: impl Iterator<Item = (RasterSpaceIndex, IntRect)>,
) -> Result<IntRect, WhyAnimationHasNoLayer> {
    let (tree, list) = (inputs.tree, inputs.list);
    let raster_space = tables.spaces()[space.0];
    let RasterSpaceAnchorKind::AnimatedTransform { raster_scale } = raster_space.anchor_kind else {
        unreachable!("only spaces whose anchor the compositor animates are swept");
    };
    if list
        .summary()
        .destination_blending_records
        .iter()
        .any(|record| run_indices_of_space.binary_search(&(record.run_index as usize)).is_ok())
    {
        return Err(WhyAnimationHasNoLayer::ItHoldsRecordsThatBlendWithWhatIsBelowThem);
    }
    for &run_index in run_indices_of_space {
        let mut effect = list.run(run_index).context.effect;
        while !effect.is_none() {
            let effect_node = &tree.effect_nodes[effect.0 as usize];
            let composited_around_space = tables.animated_space_holding_every_run_of_effect(effect) == Some(space)
                && !tables.moves_with(effect_node.spatial, space);
            if composited_around_space {
                if crate::display_list::animated_transform_spaces::effect_reads_below_it(tree, effect) {
                    return Err(WhyAnimationHasNoLayer::ItHoldsAnEffectThatReadsWhatIsBelowIt);
                }
                if let Some(why) = why_compositor_cannot_composite_by_effect_index[effect.0 as usize] {
                    return Err(WhyAnimationHasNoLayer::InsideAnEffectTheCompositorCannotComposite(why));
                }
            }
            effect = effect_node.parent;
        }
    }

    let geometry = &geometries[space.0];
    // Content that moves within the space, as a sample of an animation inside it moves it or spreads it, reaches
    // wherever its clips fixed in the space let it.
    let content_reach_in_space = run_indices_of_space.iter().fold(Reach::Nothing, |reach, &run_index| {
        let run = list.run(run_index);
        if run.ink_bounds.is_empty() {
            return reach;
        }
        let drawn_within =
            (!tables.context_moves_within_space(tree, run.context, space)).then(|| run.ink_bounds.to_float());
        reach.united(geometry.reach_of_content(filters_and_clips, drawn_within, run.context))
    });
    let content_rect_in_anchor_space = match content_reach_in_space {
        Reach::Nothing => return Ok(IntRect::default()),
        Reach::Anywhere => return Err(WhyAnimationHasNoLayer::NothingBoundsWhereItMoves),
        Reach::Within(rect) => libgfx_rust::FloatRect::new(
            rect.x as f32 / raster_scale,
            rect.y as f32 / raster_scale,
            rect.width as f32 / raster_scale,
            rect.height as f32 / raster_scale,
        ),
    };

    let parent_space = raster_space.parent_index;
    let parent_geometry = &geometries[parent_space.0];
    let parent_of_anchor = tree.spatial_nodes[raster_space.anchor.0 as usize].parent;
    let reach_within_clips_fixed_in_parent_space =
        run_indices_of_space.iter().fold(Reach::Nothing, |reach, &run_index| {
            reach.united(parent_geometry.reach_of_content(filters_and_clips, None, list.run(run_index).context))
        });
    let swept_bounds_in_parent_space = (!tables.spatial_moves_within_space(parent_of_anchor, parent_space))
        .then(|| {
            crate::display_list::animated_transform_spaces::swept_bounds_in_parent(
                tree,
                raster_space.anchor,
                content_rect_in_anchor_space,
            )
        })
        .flatten()
        .map(|bounds| parent_geometry.raster.world.map_rect_to_root(parent_of_anchor, bounds))
        .filter(|bounds| is_finite_rect(*bounds));
    let swept_reach = match swept_bounds_in_parent_space {
        Some(bounds) => {
            let margin = crate::display_list::placement::ANTIALIASING_MARGIN_IN_DEVICE_PIXELS;
            let swept_rect =
                enclosing_int_rect_within_extent_limit(bounds).inflated_edges(margin, margin, margin, margin);
            reach_within_clips_fixed_in_parent_space.limited_to(swept_rect)
        }
        None => reach_within_clips_fixed_in_parent_space,
    };
    if swept_reach == Reach::Anywhere {
        return Err(WhyAnimationHasNoLayer::NothingBoundsWhereItMoves);
    }
    let swept_reach = parent_geometry.part_of_extent_reached_by(swept_reach);

    for (space_of_media, reach_of_media) in canvases_and_videos_placed_before {
        if space_of_media != parent_space || !reach_of_media.intersected(swept_reach).is_empty() {
            return Err(WhyAnimationHasNoLayer::ItCanShowOverAnEarlierCanvasOrVideo);
        }
    }
    Ok(swept_reach)
}

/// The effects whose opacity or filter the compositor animates, each with whether its filter is animated.
fn effects_whose_opacity_or_filter_the_compositor_animates(tree: &VisualContextTree) -> Vec<(EffectNodeIndex, bool)> {
    use crate::host::FfiVisualAnimationTargetKind;
    let mut animated_effects: Vec<(EffectNodeIndex, bool)> = Vec::new();
    let mut position_in_animated_effects_by_effect_index: HashMap<u32, usize> = HashMap::new();
    for animation in tree.visual_animations() {
        let filter_is_animated = match animation.target_kind {
            FfiVisualAnimationTargetKind::Opacity => false,
            FfiVisualAnimationTargetKind::Filter => true,
            _ => continue,
        };
        for &effect_index in &animation.node_indices {
            if effect_index as usize >= tree.effect_nodes.len() {
                continue;
            }
            match position_in_animated_effects_by_effect_index.entry(effect_index) {
                std::collections::hash_map::Entry::Occupied(position) => {
                    animated_effects[*position.get()].1 |= filter_is_animated;
                }
                std::collections::hash_map::Entry::Vacant(position) => {
                    position.insert(animated_effects.len());
                    animated_effects.push((EffectNodeIndex(effect_index), filter_is_animated));
                }
            }
        }
    }
    animated_effects
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BandBeingBuiltIndex(usize);

struct BandBeingBuilt {
    placement_node_index: PlacementNodeIndex,
    barriers: Vec<BarrierBeingBuilt>,
    own_record_stretch_indices: Vec<RecordStretchIndex>,
    first_record_stretch_index: RecordStretchIndex,
}

#[derive(Clone, Copy)]
enum BarrierBeingBuilt {
    Media {
        record_stretch_index: RecordStretchIndex,
    },
    NestedNodeBand {
        band_being_built_index: BandBeingBuiltIndex,
    },
}

fn expanded_to_tile_grid(rect: IntRect) -> IntRect {
    if rect.is_empty() {
        return rect;
    }
    let tile_size = TILE_SIZE_IN_DEVICE_PIXELS;
    let left = rect.x.div_euclid(tile_size) * tile_size;
    let top = rect.y.div_euclid(tile_size) * tile_size;
    let right = (rect.right() - 1).div_euclid(tile_size) * tile_size + tile_size;
    let bottom = (rect.bottom() - 1).div_euclid(tile_size) * tile_size + tile_size;
    IntRect::new(left, top, right - left, bottom - top)
}

struct StackBeingBuilt<'a> {
    placement: &'a ListPlacement,
    nodes: Vec<NodeOfPlan>,
    node_of_run_group: Vec<PlacementNodeIndex>,
    children_of_node: Vec<Vec<PlacementNodeIndex>>,
    bands_being_built: Vec<BandBeingBuilt>,
    root_group_reach_so_far_by_band_being_built: Vec<Reach<IntRect>>,
    why_composited_once_decided_by_placed_effect: Vec<Option<Option<WhyEffectIsComposited>>>,
    composited_node_by_placed_effect: Vec<Option<PlacementNodeIndex>>,
    /// One per composited node, in the order of the nodes.
    notes_of_composited_effects: Vec<NoteOfCompositedEffect>,
    records_blend_with_what_other_layers_hold_below_them: bool,
}

impl StackBeingBuilt<'_> {
    fn add_node(&mut self, node: NodeOfPlan) -> PlacementNodeIndex {
        let parent_index = node.parent_index;
        self.nodes.push(node);
        self.children_of_node.push(Vec::new());
        let node_index = PlacementNodeIndex(self.nodes.len() - 1);
        if let Some(parent_index) = parent_index {
            self.children_of_node[parent_index.0].push(node_index);
        }
        node_index
    }

    fn find_or_add_node(
        &mut self,
        raster_space_index: RasterSpaceIndex,
        composite_clips: Vec<CompositeClip>,
    ) -> PlacementNodeIndex {
        if let Some(existing_node_index) = self.nodes.iter().position(|node| {
            node.raster_space_index == raster_space_index
                && node.composite_clips == composite_clips
                && node.composited_effect.is_none()
        }) {
            return PlacementNodeIndex(existing_node_index);
        }
        let spaces = &self.placement.spaces;
        let (parent_index, reach_in_parent_node) = if raster_space_index == RasterSpaceIndex::VIEWPORT {
            (None, Reach::Anywhere)
        } else {
            let parent_space = spaces[raster_space_index.0].parent_index;
            let reach_in_parent_node = composite_clips
                .iter()
                .find(|clip| clip.raster_space_index == parent_space)
                .map_or(Reach::Anywhere, |clip| Reach::Within(clip.rect));
            let composite_clips_of_parent_node = composite_clips
                .iter()
                .filter(|clip| clip.raster_space_index != parent_space)
                .copied()
                .collect();
            (
                Some(self.find_or_add_node(parent_space, composite_clips_of_parent_node)),
                reach_in_parent_node,
            )
        };
        self.add_node(NodeOfPlan {
            raster_space_index,
            composite_clips,
            composited_effect: None,
            parent_index,
            reach_in_parent_node,
        })
    }

    fn add_composited_node_inside(
        &mut self,
        parent_index: PlacementNodeIndex,
        effect: EffectNodeIndex,
        reach_in_parent_node: Reach<IntRect>,
    ) -> PlacementNodeIndex {
        let parent_node = &self.nodes[parent_index.0];
        let composited_node = NodeOfPlan {
            raster_space_index: parent_node.raster_space_index,
            composite_clips: parent_node.composite_clips.clone(),
            composited_effect: Some(effect),
            parent_index: Some(parent_index),
            reach_in_parent_node,
        };
        self.add_node(composited_node)
    }

    fn placed_effects_around_stretch_outermost_first(&self, stretch: &PlacedRecordStretch) -> Vec<PlacedEffectIndex> {
        let mut placed_effects_around_stretch: Vec<PlacedEffectIndex> =
            std::iter::successors(stretch.placed_effect_index, |&placed_effect_index| {
                self.placement.placed_effects[placed_effect_index.0].placed_effect_around_index
            })
            .collect();
        placed_effects_around_stretch.reverse();
        placed_effects_around_stretch
    }

    fn reach_of_stretch_with_what_effects_around_it_read(&self, stretch: &PlacedRecordStretch) -> Reach<IntRect> {
        self.placed_effects_around_stretch_outermost_first(stretch)
            .into_iter()
            .fold(Reach::Within(stretch.reach), |reach, placed_effect_index| {
                reach.united(self.placement.placed_effects[placed_effect_index.0].backdrop_read_reach)
            })
    }

    fn draw_and_read_reach_of_placed_effect(&self, placed_effect_index: PlacedEffectIndex) -> Reach<IntRect> {
        let placed_effect = &self.placement.placed_effects[placed_effect_index.0];
        Reach::Within(placed_effect.reach).united(placed_effect.backdrop_read_reach)
    }

    // A node of a space whose anchor the compositor animates reaches only where its content can show wherever the
    // animation takes it, and a node of a sticky box only where its content, with the nodes of the spaces in it, can
    // show wherever the box is held. Nodes come after the nodes they are in, so a walk from the last node sees the final
    // reach of each node before the node it is in.
    fn limit_reach_of_moving_space_nodes_to_content_swept_over_how_they_move(&mut self) {
        let placement = self.placement;
        let mut content_reach_by_node_index = vec![Reach::Nothing; self.nodes.len()];
        for stretch in &placement.record_stretches {
            let node_index = self.node_of_run_group[stretch.run_group_index.0];
            content_reach_by_node_index[node_index.0] = content_reach_by_node_index[node_index.0]
                .united(self.reach_of_stretch_with_what_effects_around_it_read(stretch));
        }
        for node_index in (0..self.nodes.len()).rev() {
            let node = &mut self.nodes[node_index];
            if node.composited_effect.is_some() {
                continue;
            }
            if let Some(Some(swept_reach)) = placement
                .swept_reach_in_parent_space_by_space_index
                .get(node.raster_space_index.0)
            {
                node.reach_in_parent_node = node.reach_in_parent_node.limited_to(*swept_reach);
            }
            let raster_space = &placement.spaces[node.raster_space_index.0];
            if matches!(raster_space.anchor_kind, RasterSpaceAnchorKind::StickyBox { .. })
                && let Reach::Within(content_reach) = content_reach_by_node_index[node_index]
            {
                let content_reach_swept_over_offset_range = rect_swept_over_translations(
                    content_reach,
                    raster_space.lowest_anchor_translation,
                    raster_space.highest_anchor_translation,
                );
                node.reach_in_parent_node = node
                    .reach_in_parent_node
                    .limited_to(content_reach_swept_over_offset_range);
            }
            if let Some(parent_index) = node.parent_index {
                let reach_in_parent_node = node.reach_in_parent_node;
                content_reach_by_node_index[parent_index.0] =
                    content_reach_by_node_index[parent_index.0].united(reach_in_parent_node);
            }
        }
    }

    fn path_from_root_to(&self, mut node_index: PlacementNodeIndex) -> Vec<PlacementNodeIndex> {
        let mut path = vec![node_index];
        while let Some(parent_index) = self.nodes[node_index.0].parent_index {
            path.push(parent_index);
            node_index = parent_index;
        }
        path.reverse();
        path
    }

    fn open_band(
        &mut self,
        placement_node_index: PlacementNodeIndex,
        first_record_stretch_index: RecordStretchIndex,
    ) -> BandBeingBuiltIndex {
        self.bands_being_built.push(BandBeingBuilt {
            placement_node_index,
            barriers: Vec::new(),
            own_record_stretch_indices: Vec::new(),
            first_record_stretch_index,
        });
        self.root_group_reach_so_far_by_band_being_built.push(Reach::Nothing);
        BandBeingBuiltIndex(self.bands_being_built.len() - 1)
    }

    fn reach_can_meet_tiles(
        &self,
        node_of_reach: PlacementNodeIndex,
        reach_in_node: Reach<IntRect>,
        path_from_root_to_node_of_tiles: &[PlacementNodeIndex],
        tiles_in_node: IntRect,
    ) -> bool {
        if reach_in_node == Reach::Nothing {
            return false;
        }
        let path_from_root_to_node_of_reach = self.path_from_root_to(node_of_reach);
        let length_of_shared_path = path_from_root_to_node_of_reach
            .iter()
            .zip(path_from_root_to_node_of_tiles)
            .take_while(|(first, second)| first == second)
            .count();
        let reach_in_lowest_shared_node = match path_from_root_to_node_of_reach.get(length_of_shared_path) {
            Some(&child_of_lowest_shared_node) => self.nodes[child_of_lowest_shared_node.0].reach_in_parent_node,
            None => reach_in_node,
        };
        let tiles_in_lowest_shared_node = match path_from_root_to_node_of_tiles.get(length_of_shared_path) {
            Some(&child_of_lowest_shared_node) => self.nodes[child_of_lowest_shared_node.0].reach_in_parent_node,
            None => Reach::Within(tiles_in_node),
        };
        reach_in_lowest_shared_node.meets(tiles_in_lowest_shared_node)
    }

    fn reader_painted_next_finds_all_it_reads_in_own_layer(
        &self,
        reader_node: PlacementNodeIndex,
        open_band_of_reader_node: Option<BandBeingBuiltIndex>,
        read_tiles: IntRect,
    ) -> bool {
        let path_from_root_to_reader_node = self.path_from_root_to(reader_node);
        let another_band_shows_root_group_in_read_tiles =
            self.bands_being_built.iter().enumerate().any(|(index, band)| {
                Some(BandBeingBuiltIndex(index)) != open_band_of_reader_node
                    && self.reach_can_meet_tiles(
                        band.placement_node_index,
                        self.root_group_reach_so_far_by_band_being_built[index],
                        &path_from_root_to_reader_node,
                        read_tiles,
                    )
            });
        if another_band_shows_root_group_in_read_tiles {
            return false;
        }
        let Some(open_band_of_reader_node) = open_band_of_reader_node else {
            return true;
        };
        self.bands_being_built[open_band_of_reader_node.0]
            .barriers
            .iter()
            .all(|barrier| {
                let reach_of_barrier = match *barrier {
                    BarrierBeingBuilt::Media { record_stretch_index } => {
                        Reach::Within(self.placement.record_stretches[record_stretch_index.0].reach)
                    }
                    BarrierBeingBuilt::NestedNodeBand { band_being_built_index } => {
                        let nested_node_index = self.bands_being_built[band_being_built_index.0].placement_node_index;
                        self.nodes[nested_node_index.0].reach_in_parent_node
                    }
                };
                !reach_of_barrier.touches(read_tiles)
            })
    }

    fn close_bands_of_node_and_nodes_inside(
        &self,
        node_index: PlacementNodeIndex,
        open_band_by_node_index: &mut [Option<BandBeingBuiltIndex>],
    ) {
        open_band_by_node_index[node_index.0] = None;
        for &child_index in &self.children_of_node[node_index.0] {
            self.close_bands_of_node_and_nodes_inside(child_index, open_band_by_node_index);
        }
    }

    fn why_effect_is_composited(
        &self,
        placed_effect: &PlacedEffect,
        node_of_first_stretch_painted_next: PlacementNodeIndex,
        open_band_of_node: Option<BandBeingBuiltIndex>,
    ) -> Option<WhyEffectIsComposited> {
        if let Some(why) = placed_effect.why_composited_regardless_of_layer_content {
            return Some(why);
        }
        if !placed_effect.reads_root_group {
            return None;
        }
        let finds_all_it_reads_in_own_layer = match placed_effect.backdrop_read_reach {
            Reach::Nothing => true,
            Reach::Within(backdrop_read_rect) => self.reader_painted_next_finds_all_it_reads_in_own_layer(
                node_of_first_stretch_painted_next,
                open_band_of_node,
                expanded_to_tile_grid(backdrop_read_rect),
            ),
            Reach::Anywhere => false,
        };
        (!finds_all_it_reads_in_own_layer).then_some(WhyEffectIsComposited::ItReadsWhatItsOwnLayerDoesNotHold)
    }

    fn node_of_stretch(
        &mut self,
        stretch: &PlacedRecordStretch,
        open_band_by_node_index: &mut Vec<Option<BandBeingBuiltIndex>>,
    ) -> PlacementNodeIndex {
        let mut node_index = self.node_of_run_group[stretch.run_group_index.0];
        for placed_effect_index in self.placed_effects_around_stretch_outermost_first(stretch) {
            let why_composited = match self.why_composited_once_decided_by_placed_effect[placed_effect_index.0] {
                Some(why_composited) => why_composited,
                None => {
                    let why_composited = self.why_effect_is_composited(
                        &self.placement.placed_effects[placed_effect_index.0],
                        node_index,
                        open_band_by_node_index[node_index.0],
                    );
                    self.why_composited_once_decided_by_placed_effect[placed_effect_index.0] = Some(why_composited);
                    why_composited
                }
            };
            let Some(why) = why_composited else {
                break;
            };
            node_index = match self.composited_node_by_placed_effect[placed_effect_index.0] {
                Some(composited_node_index) => composited_node_index,
                None => {
                    let placed_effect = self.placement.placed_effects[placed_effect_index.0];
                    let draw_and_read_reach = self.draw_and_read_reach_of_placed_effect(placed_effect_index);
                    let composited_node_index =
                        self.add_composited_node_inside(node_index, placed_effect.effect, draw_and_read_reach);
                    open_band_by_node_index.push(None);
                    self.composited_node_by_placed_effect[placed_effect_index.0] = Some(composited_node_index);
                    self.notes_of_composited_effects.push(NoteOfCompositedEffect {
                        why,
                        backdrop_read_reach: placed_effect.backdrop_read_reach,
                    });
                    composited_node_index
                }
            };
        }
        node_index
    }

    fn cut_into_bands(&mut self) {
        let placement = self.placement;
        let root_node_index = PlacementNodeIndex(0);
        let mut open_band_by_node_index: Vec<Option<BandBeingBuiltIndex>> = vec![None; self.nodes.len()];
        open_band_by_node_index[root_node_index.0] = Some(self.open_band(root_node_index, RecordStretchIndex(0)));
        let mut composited_nodes_whose_bands_may_be_open: Vec<PlacementNodeIndex> = Vec::new();
        let mut node_is_on_path_of_stretch: Vec<bool> = Vec::new();
        for (index, stretch) in placement.record_stretches.iter().enumerate() {
            let stretch_index = RecordStretchIndex(index);
            if stretch.reach.is_empty() {
                continue;
            }
            let node_of_stretch = self.node_of_stretch(stretch, &mut open_band_by_node_index);
            let reach_read_by_blending_records =
                placement.run_groups[stretch.run_group_index.0].reach_read_by_records_blending_with_root_group;
            if reach_read_by_blending_records != Reach::Nothing {
                let blending_records_find_all_they_read_in_own_layer = match reach_read_by_blending_records {
                    Reach::Within(read_rect) => {
                        let tiles_blending_records_draw_or_read =
                            expanded_to_tile_grid(stretch.reach.united(read_rect));
                        self.reader_painted_next_finds_all_it_reads_in_own_layer(
                            node_of_stretch,
                            open_band_by_node_index[node_of_stretch.0],
                            tiles_blending_records_draw_or_read,
                        )
                    }
                    _ => false,
                };
                self.records_blend_with_what_other_layers_hold_below_them |=
                    !blending_records_find_all_they_read_in_own_layer;
            }
            let path_from_root_to_node_of_stretch = self.path_from_root_to(node_of_stretch);
            // A replay of the list composites an effect anew wherever content outside it is painted, so the bands of
            // its node end there.
            composited_nodes_whose_bands_may_be_open.retain(|node| open_band_by_node_index[node.0].is_some());
            if !composited_nodes_whose_bands_may_be_open.is_empty() {
                node_is_on_path_of_stretch.resize(self.nodes.len(), false);
                for node_on_path in &path_from_root_to_node_of_stretch {
                    node_is_on_path_of_stretch[node_on_path.0] = true;
                }
                for &composited_node_index in &composited_nodes_whose_bands_may_be_open {
                    if open_band_by_node_index[composited_node_index.0].is_some()
                        && !node_is_on_path_of_stretch[composited_node_index.0]
                    {
                        self.close_bands_of_node_and_nodes_inside(composited_node_index, &mut open_band_by_node_index);
                    }
                }
                for node_on_path in &path_from_root_to_node_of_stretch {
                    node_is_on_path_of_stretch[node_on_path.0] = false;
                }
            }
            for depth in 0..path_from_root_to_node_of_stretch.len() {
                let node_on_path = path_from_root_to_node_of_stretch[depth];
                let open_band_of_node_on_path =
                    open_band_by_node_index[node_on_path.0].expect("the bands along the path of a stretch are open");
                let next_node_on_path = path_from_root_to_node_of_stretch.get(depth + 1).copied();
                let reach_of_stretch_in_node_on_path = match next_node_on_path {
                    Some(next_node) => self.nodes[next_node.0].reach_in_parent_node,
                    None => Reach::Within(stretch.reach),
                };
                for &other_child in &self.children_of_node[node_on_path.0] {
                    if Some(other_child) == next_node_on_path || open_band_by_node_index[other_child.0].is_none() {
                        continue;
                    }
                    if reach_of_stretch_in_node_on_path.meets(self.nodes[other_child.0].reach_in_parent_node) {
                        self.close_bands_of_node_and_nodes_inside(other_child, &mut open_band_by_node_index);
                    }
                }
                match next_node_on_path {
                    Some(next_node) if open_band_by_node_index[next_node.0].is_none() => {
                        let new_band_of_next_node = self.open_band(next_node, stretch_index);
                        open_band_by_node_index[next_node.0] = Some(new_band_of_next_node);
                        if self.nodes[next_node.0].composited_effect.is_some() {
                            composited_nodes_whose_bands_may_be_open.push(next_node);
                        }
                        self.bands_being_built[open_band_of_node_on_path.0].barriers.push(
                            BarrierBeingBuilt::NestedNodeBand {
                                band_being_built_index: new_band_of_next_node,
                            },
                        );
                    }
                    Some(_) => {}
                    None if stretch.placed_media_index.is_some() => {
                        self.bands_being_built[open_band_of_node_on_path.0]
                            .barriers
                            .push(BarrierBeingBuilt::Media {
                                record_stretch_index: stretch_index,
                            });
                    }
                    None => self.bands_being_built[open_band_of_node_on_path.0]
                        .own_record_stretch_indices
                        .push(stretch_index),
                }
            }
            if placement.run_groups[stretch.run_group_index.0].paints_within_root_group {
                let open_band_of_stretch = open_band_by_node_index[node_of_stretch.0]
                    .expect("the band of a stretch is open once it is placed");
                self.root_group_reach_so_far_by_band_being_built[open_band_of_stretch.0] = self
                    .root_group_reach_so_far_by_band_being_built[open_band_of_stretch.0]
                    .united(Reach::Within(stretch.reach));
            }
        }
    }

    // Puts the band into `bands`, and after it the bands nested in it, in the order of a walk that enters a nested band
    // at its barrier.
    fn emit_band(&mut self, band_being_built_index: BandBeingBuiltIndex, bands: &mut Vec<BandOfPlan>) {
        let placement = self.placement;
        let band_being_built = &mut self.bands_being_built[band_being_built_index.0];
        let placement_node_index = band_being_built.placement_node_index;
        let own_record_stretch_indices = std::mem::take(&mut band_being_built.own_record_stretch_indices);
        let barriers = std::mem::take(&mut band_being_built.barriers);
        let first_record_position = placement
            .record_stretches
            .get(band_being_built.first_record_stretch_index.0)
            .map_or(ListPosition::default(), |stretch| stretch.record_span.begin);
        let band_position = bands.len();
        bands.push(BandOfPlan {
            placement_node_index,
            first_record_position,
            record_spans: Vec::new(),
            layer_below_all_cuts: PaintedLayerOfPlan::default(),
            cuts: Vec::new(),
        });
        // Here a layer index counts the band's painted layers: the one below all cuts, then the one above each cut.
        let mut cuts = Vec::with_capacity(barriers.len());
        let mut barriers_of_plan = Vec::with_capacity(barriers.len());
        for barrier in barriers {
            let (barrier_of_plan, barrier_first_record_position, cut_reach) = match barrier {
                BarrierBeingBuilt::Media { record_stretch_index } => {
                    let media_stretch = &placement.record_stretches[record_stretch_index.0];
                    let media = &placement.placed_media[media_stretch
                        .placed_media_index
                        .expect("a media barrier is a media stretch")
                        .0];
                    let barrier_of_plan = BarrierOfPlan::Media {
                        content: media.content,
                        ordinal: media.ordinal,
                        record_span: media_stretch.record_span,
                        reach: media_stretch.reach,
                    };
                    (
                        barrier_of_plan,
                        media_stretch.record_span.begin,
                        Reach::Within(media_stretch.reach),
                    )
                }
                BarrierBeingBuilt::NestedNodeBand {
                    band_being_built_index: nested_band_being_built_index,
                } => {
                    let nested_band = &self.bands_being_built[nested_band_being_built_index.0];
                    let first_record_position_of_nested_band = placement.record_stretches
                        [nested_band.first_record_stretch_index.0]
                        .record_span
                        .begin;
                    let cut_reach = self.nodes[nested_band.placement_node_index.0].reach_in_parent_node;
                    self.emit_band(nested_band_being_built_index, bands);
                    (
                        BarrierOfPlan::NestedBand,
                        first_record_position_of_nested_band,
                        cut_reach,
                    )
                }
            };
            cuts.push(BarrierCut {
                barrier_first_record_position,
                cut_reach,
                layer_above_cut: LayerIndex(cuts.len() + 1),
            });
            barriers_of_plan.push(barrier_of_plan);
        }

        // A record draws nothing past the reach of its stretch, so the layers it is replayed into anywhere else stay
        // empty.
        let mut painted_layers = vec![PaintedLayerOfPlan::default(); cuts.len() + 1];
        let mut record_spans: Vec<ListSpan> = Vec::with_capacity(own_record_stretch_indices.len());
        for &stretch_index in &own_record_stretch_indices {
            let stretch = &placement.record_stretches[stretch_index.0];
            let stretch_span = stretch.record_span;
            let mut mark_layer_may_hold_content_within = |layer: LayerIndex, reach: IntRect| {
                painted_layers[layer.0].may_hold_content = true;
                painted_layers[layer.0].reach = painted_layers[layer.0].reach.united(reach);
            };
            let cuts_painted_before_stretch = cuts
                .iter()
                .take_while(|cut| cut.barrier_first_record_position <= stretch_span.begin);
            if cuts_painted_before_stretch.clone().next().is_none() {
                mark_layer_may_hold_content_within(LayerIndex(0), stretch.reach);
            } else if reaches_too_many_cells_to_follow_one_by_one(stretch.reach) {
                mark_layer_may_hold_content_within(LayerIndex(0), stretch.reach);
                for cut in cuts_painted_before_stretch {
                    mark_layer_may_hold_content_within(cut.layer_above_cut, stretch.reach);
                }
            } else {
                for coord in tile_coords_touching(stretch.reach) {
                    let cell = coord.cell_rect();
                    let layer_of_record_in_cell = cuts_painted_before_stretch
                        .clone()
                        .filter(|cut| cut.cuts_cell(cell))
                        .last()
                        .map_or(LayerIndex(0), |cut| cut.layer_above_cut);
                    mark_layer_may_hold_content_within(layer_of_record_in_cell, stretch.reach.intersected(cell));
                }
            }
            match record_spans.last_mut() {
                Some(last) if last.end == stretch_span.begin => last.end = stretch_span.end,
                _ => record_spans.push(stretch_span),
            }
        }
        let band = &mut bands[band_position];
        band.record_spans = record_spans;
        band.layer_below_all_cuts = painted_layers[0];
        band.cuts = barriers_of_plan
            .into_iter()
            .zip(&painted_layers[1..])
            .map(|(barrier, &layer_above_cut)| CutOfPlan {
                barrier,
                layer_above_cut,
            })
            .collect();
    }
}

// The plan of the stack, and one note per composited node, in the order of the nodes.
fn assemble_plan_unless_records_blend_with_other_layers(
    placement: &ListPlacement,
) -> Option<(LayeredPlan, Vec<NoteOfCompositedEffect>)> {
    let mut stack_being_built = StackBeingBuilt {
        placement,
        nodes: Vec::new(),
        node_of_run_group: Vec::new(),
        children_of_node: Vec::new(),
        bands_being_built: Vec::new(),
        root_group_reach_so_far_by_band_being_built: Vec::new(),
        why_composited_once_decided_by_placed_effect: vec![None; placement.placed_effects.len()],
        composited_node_by_placed_effect: vec![None; placement.placed_effects.len()],
        notes_of_composited_effects: Vec::new(),
        records_blend_with_what_other_layers_hold_below_them: false,
    };
    stack_being_built.find_or_add_node(RasterSpaceIndex::VIEWPORT, Vec::new());
    stack_being_built.node_of_run_group = placement
        .run_groups
        .iter()
        .map(|run_group| run_group.composite_clips.clone())
        .zip(
            placement
                .run_groups
                .iter()
                .map(|run_group| run_group.raster_space_index),
        )
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(composite_clips, raster_space_index)| {
            stack_being_built.find_or_add_node(raster_space_index, composite_clips)
        })
        .collect();
    stack_being_built.limit_reach_of_moving_space_nodes_to_content_swept_over_how_they_move();
    stack_being_built.cut_into_bands();
    if stack_being_built.records_blend_with_what_other_layers_hold_below_them {
        return None;
    }
    let mut bands = Vec::with_capacity(stack_being_built.bands_being_built.len());
    stack_being_built.emit_band(BandBeingBuiltIndex(0), &mut bands);

    let StackBeingBuilt {
        nodes,
        notes_of_composited_effects,
        ..
    } = stack_being_built;
    let plan = LayeredPlan {
        nodes,
        bands,
        tiles_hold_content_moving_within_space_by_space_index: placement
            .tiles_hold_content_moving_within_space_by_space_index
            .clone(),
        swept_reach_in_parent_space_by_space_index: placement.swept_reach_in_parent_space_by_space_index.clone(),
    };
    Some((plan, notes_of_composited_effects))
}

fn effects_composited_in_plan(plan: &LayeredPlan) -> impl Iterator<Item = EffectNodeIndex> + '_ {
    plan.nodes.iter().filter_map(|node| node.composited_effect)
}

// A 3D sorting context that sorts its planes by depth is placed whole into the tiles of one band, which replay it
// whole, as sorting its planes reads all of them. That holds only where nothing inside it reads what is below it or is
// composited apart from it.
fn contexts_sorting_planes_by_depth_hold_effects_that_read_or_are_composited(
    inputs: &LayerStackInputs<'_>,
    placement: &ListPlacement,
    plan: &LayeredPlan,
) -> bool {
    if !placement.layering_obstacles.sorts_planes_by_depth {
        return false;
    }
    let tree = inputs.tree;
    let run_groups = inputs.list.run_groups(tree);
    let mut marks = [effects_of_marked_runs(inputs.list, tree, |run_index| {
        run_groups.run_is_sorted_by_depth(run_index)
    })];
    mark_effects_around_marked_effects(tree, &mut marks);
    let [effect_holds_run_sorted_by_depth_by_effect_index] = marks;
    placement
        .backdrop_readers_in_paint_order
        .iter()
        .copied()
        .chain(effects_composited_in_plan(plan))
        .any(|effect| effect_holds_run_sorted_by_depth_by_effect_index[effect.0 as usize])
}

// The compositor composites a filter only where its tiles hold all it reads: where the filter shows what it filters
// over and around where that is drawn, as the barriers inside it cut its tiles there, and reads no farther past each
// pixel than tiles are followed past the frame. Media and animations get no layer inside any other filter;
// an effect that reads what is below its layer can still make one composited.
fn why_compositor_cannot_composite_effect_of_plan(
    placement: &ListPlacement,
    plan: &LayeredPlan,
) -> Option<WhyListIsOneLayer> {
    effects_composited_in_plan(plan).find_map(|composited_effect| {
        placement.why_compositor_cannot_composite_by_effect_index[composited_effect.0 as usize]
            .map(WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited)
    })
}

fn why_plan_goes_as_one_layer(
    inputs: &LayerStackInputs<'_>,
    placement: &ListPlacement,
    plan: &LayeredPlan,
) -> Option<WhyListIsOneLayer> {
    if composited_effects_nest_too_deep(
        plan.nodes
            .iter()
            .map(|node| (node.parent_index, node.composited_effect.is_some())),
    ) {
        return Some(WhyListIsOneLayer::CompositedEffectsNestTooDeep);
    }
    if contexts_sorting_planes_by_depth_hold_effects_that_read_or_are_composited(inputs, placement, plan) {
        return Some(WhyListIsOneLayer::SortingContext3D);
    }
    why_compositor_cannot_composite_effect_of_plan(placement, plan)
}

fn notes_of_stack(
    placement: &ListPlacement,
    stack: &LayerStack,
    notes_of_composited_effects: Vec<NoteOfCompositedEffect>,
) -> NotesOfStack {
    debug_assert_eq!(stack.composited_effects.len(), notes_of_composited_effects.len());
    let effects_reading_only_own_layer = placement
        .backdrop_readers_in_paint_order
        .iter()
        .copied()
        .filter(|reader| {
            stack
                .composited_effects
                .iter()
                .all(|composited| composited.effect != *reader)
        })
        .collect();
    NotesOfStack {
        composited_effects: notes_of_composited_effects,
        media_without_layers: placement.media_without_layers.clone(),
        effects_reading_only_own_layer,
    }
}

/// Works out the layers of a list from where `place_list` placed its parts among `spaces`, whose extents are not filled
/// in.
///
/// Nothing limits how many layers a stack holds past the spaces a tree offers, which are limited before the list is
/// placed: the layers an effect the compositor composites or a canvas, video or nested context needs cannot be given
/// up without drawing the list wrong, and a layer holds tiles only where its content reaches, which the pool of tiles
/// gives back.
///
/// A plan of more layers that fails the checks the compositor runs on a plan it receives is a bug of the planner. A
/// build with debug assertions panics on it, and other builds give the list one layer.
fn build_layer_stack(
    inputs: &LayerStackInputs<'_>,
    placement: ListPlacement,
    spaces: Vec<RasterSpace>,
    nesting_of_live_effects: &NestingOfLiveEffects<'_>,
) -> PlannedLayers {
    let layering_obstacles = placement.layering_obstacles;
    let why_list_is_one_layer = match why_list_cannot_be_layered(inputs, &layering_obstacles) {
        Some(why) => why,
        None => match assemble_plan_unless_records_blend_with_other_layers(&placement) {
            None => WhyListIsOneLayer::RecordsBlendWithWhatOtherLayersHoldBelowThem,
            Some((plan, notes_of_composited_effects)) => match why_plan_goes_as_one_layer(inputs, &placement, &plan) {
                Some(why) => why,
                None => match LayerStack::of_plan_passing_checks(
                    plan,
                    spaces,
                    inputs.list,
                    inputs.tree,
                    inputs.viewport_rect,
                    layering_obstacles,
                    nesting_of_live_effects,
                ) {
                    Ok(stack) => {
                        return PlannedLayers {
                            notes: notes_of_stack(&placement, &stack, notes_of_composited_effects),
                            stack,
                            placement,
                        };
                    }
                    Err(why_plan_fails_checks) => {
                        debug_assert!(
                            false,
                            "the compositor refuses a plan of layers the planner made: {why_plan_fails_checks}"
                        );
                        eprintln!(
                            "WebContent: Sending one layer, as the compositor refuses the plan of layers: {why_plan_fails_checks}"
                        );
                        WhyListIsOneLayer::PlanFailsChecksOfCompositor
                    }
                },
            },
        },
    };
    let mut stack = LayerStack::single_layer(
        inputs.viewport_rect,
        inputs.list.whole_span(),
        Some(why_list_is_one_layer),
    );
    stack.layering_obstacles = layering_obstacles;
    PlannedLayers {
        stack,
        notes: NotesOfStack::default(),
        placement,
    }
}

// The scroll offsets a plan reads of `scroll_offsets`: zero for each node that `leaves_out_scroll_offset_of_node` names.
fn scroll_offsets_layer_plans_read(
    scroll_offsets: &[FloatPoint],
    leaves_out_scroll_offset_of_node: &[bool],
) -> Vec<FloatPoint> {
    scroll_offsets
        .iter()
        .enumerate()
        .map(|(node_index, &scroll_offset)| {
            if leaves_out_scroll_offset_of_node
                .get(node_index)
                .copied()
                .unwrap_or(false)
            {
                FloatPoint::default()
            } else {
                scroll_offset
            }
        })
        .collect()
}

/// Plans the layers of a list, and gives the stack with where it placed the parts of the list. The plan holds for every
/// sample of the compositor's animations and wherever the compositor scrolls the nodes it scrolls. Where the window
/// server can show the media of the stack is left to be worked out for each frame.
pub fn plan_layer_stack(inputs: &LayerStackInputs<'_>) -> PlannedLayers {
    plan_layer_stack_at_anchors(inputs, &RasterSpaceAnchorsOfTree::of(inputs.tree))
}

// `anchors_of_tree` are those of `inputs.tree`.
fn plan_layer_stack_at_anchors(
    inputs: &LayerStackInputs<'_>,
    anchors_of_tree: &RasterSpaceAnchorsOfTree,
) -> PlannedLayers {
    let scroll_offsets_read =
        scroll_offsets_layer_plans_read(inputs.scroll_offsets, &anchors_of_tree.leaves_out_scroll_offset_of_node);
    let inputs = LayerStackInputs {
        scroll_offsets: &scroll_offsets_read,
        ..*inputs
    };
    let (spaces, geometries) = spaces_with_geometries(
        inputs.tree,
        anchors_of_tree,
        inputs.scroll_offsets,
        inputs.viewport_rect,
    );
    let spaces_of_stack = spaces.spaces().to_vec();
    let nesting_of_live_effects = NestingOfLiveEffects::of(inputs.tree);
    let placement = place_list(&inputs, spaces, &geometries, &nesting_of_live_effects);
    build_layer_stack(&inputs, placement, spaces_of_stack, &nesting_of_live_effects)
}

/// What a plan of the layers of a list is made from, all of which WebContent publishes: the list, the tree, the viewport
/// rect, and of the scroll offsets, those the plan reads. A plan made from inputs holds for others that plan alike.
pub struct InputsOfLayerPlan {
    list: Arc<DisplayListStorage>,
    tree: Arc<VisualContextTree>,
    viewport_rect: IntRect,
    anchors_of_tree: Arc<RasterSpaceAnchorsOfTree>,
    scroll_offsets_plan_reads: Vec<FloatPoint>,
}

impl InputsOfLayerPlan {
    pub fn new(
        list: Arc<DisplayListStorage>,
        tree: Arc<VisualContextTree>,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
    ) -> Self {
        Self::new_after(None, list, tree, scroll_offsets, viewport_rect)
    }

    /// As `new`, but where `earlier` inputs were made for the same tree, what plans work out of the tree alone comes
    /// from them.
    pub fn new_after(
        earlier: Option<&Self>,
        list: Arc<DisplayListStorage>,
        tree: Arc<VisualContextTree>,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
    ) -> Self {
        let anchors_of_tree = match earlier {
            Some(earlier) if Arc::ptr_eq(&earlier.tree, &tree) => earlier.anchors_of_tree.clone(),
            _ => Arc::new(RasterSpaceAnchorsOfTree::of(&tree)),
        };
        Self {
            scroll_offsets_plan_reads: scroll_offsets_layer_plans_read(
                scroll_offsets,
                &anchors_of_tree.leaves_out_scroll_offset_of_node,
            ),
            list,
            tree,
            viewport_rect,
            anchors_of_tree,
        }
    }

    /// The inputs of the same list and tree under other scroll offsets and another viewport rect.
    pub fn with_scroll_offsets_and_viewport_rect(&self, scroll_offsets: &[FloatPoint], viewport_rect: IntRect) -> Self {
        Self::new_after(
            Some(self),
            self.list.clone(),
            self.tree.clone(),
            scroll_offsets,
            viewport_rect,
        )
    }

    pub fn viewport_rect(&self) -> IntRect {
        self.viewport_rect
    }

    /// Whether a plan made from these inputs is the plan of `other`: the lists hold the same chunks in the same order,
    /// the trees have the same content, the viewport rects are the same, and the plan reads the same scroll offsets.
    pub fn plan_alike_as(&self, other: &Self) -> bool {
        // A list of scroll offsets leaves out the zero offsets of the nodes past its end.
        let offset_at =
            |offsets: &[FloatPoint], node_index: usize| offsets.get(node_index).copied().unwrap_or_default();
        let node_count = self
            .scroll_offsets_plan_reads
            .len()
            .max(other.scroll_offsets_plan_reads.len());
        self.viewport_rect == other.viewport_rect
            && (0..node_count).all(|node_index| {
                offset_at(&self.scroll_offsets_plan_reads, node_index)
                    == offset_at(&other.scroll_offsets_plan_reads, node_index)
            })
            && (Arc::ptr_eq(&self.list, &other.list) || self.list.holds_same_chunks_in_same_order_as(&other.list))
            && (Arc::ptr_eq(&self.tree, &other.tree) || self.tree.has_same_content_as(&other.tree))
    }

    /// Plans the layers of the list under these inputs.
    pub fn plan(&self) -> PlannedLayers {
        plan_layer_stack_at_anchors(
            &LayerStackInputs {
                list: &self.list,
                tree: &self.tree,
                scroll_offsets: &self.scroll_offsets_plan_reads,
                viewport_rect: self.viewport_rect,
            },
            &self.anchors_of_tree,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{
        CanvasId, ClipNodeIndex, ContextRef, DisplayListCommand, DrawCanvas, FillRect, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::display_list::layers::{LayerIdentity, NodeNamesFromIndices, media_name};
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_context::{
        CompositorScrollNodeRecord, CompositorScrollTable, EffectsData, ScrollData, SpatialData, TransformData,
        TransformDataRole,
    };
    use libgfx_rust::{Color, CompositingAndBlendingOperator, FloatMatrix4x4, FloatRect, ScalingMode};

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

        fn planned(&self, tree: &VisualContextTree, scroll_offsets: &[FloatPoint]) -> PlannedLayers {
            let list = DisplayListStorage::of_builder(&self.builder);
            plan_layer_stack(&LayerStackInputs {
                list: &list,
                tree,
                scroll_offsets,
                viewport_rect: VIEWPORT,
            })
        }

        fn stack(&self, tree: &VisualContextTree, scroll_offsets: &[FloatPoint]) -> LayerStack {
            self.planned(tree, scroll_offsets).stack
        }
    }

    fn kinds_of_layers_from_base_up_that_may_hold_content(stack: &LayerStack) -> Vec<String> {
        stack
            .layers
            .iter()
            .enumerate()
            .filter(|(index, layer)| {
                *index == stack.base_layer.0 || (*index > stack.base_layer.0 && layer.may_hold_content)
            })
            .map(|(index, layer)| {
                let space = stack.name_of_space(
                    stack.nodes[layer.placement_node_index.0].raster_space_index,
                    &NodeNamesFromIndices,
                );
                match layer.identity {
                    LayerIdentity::Painted {
                        band_ordinal_in_node: band,
                        above_barrier,
                        ..
                    } => {
                        let kind = if index == stack.base_layer.0 { "base" } else { "painted" };
                        let above = above_barrier.map_or(String::new(), |barrier| {
                            format!(" above {}", stack.name_of_barrier(&barrier, &NodeNamesFromIndices))
                        });
                        format!("{kind} {space} band {band}{above}")
                    }
                    LayerIdentity::Media { content, ordinal } => format!("{} #{ordinal} {space}", media_name(content)),
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
        let at_start = list.planned(&tree.with_visual_animation_samples(0), &[]);
        let halfway = list.planned(&tree.with_visual_animation_samples(500_000_000), &[]);
        assert_eq!(at_start.placement, halfway.placement);
        // Wherever the animation takes the box, it can go over the canvas, so it sits above it.
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&halfway.stack),
            [
                "base document band 0",
                "canvas #0 document",
                "painted document band 0 above L2"
            ]
        );
    }

    #[test]
    fn samples_of_an_opacity_animation_leave_the_stack_alike() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let fading_box = tree.append_effect_node_under(document, effects(1.0, CompositingAndBlendingOperator::Normal));
        tree.set_visual_animations(vec![animation_of(
            crate::host::FfiVisualAnimationTargetKind::Opacity,
            fading_box.effect.0,
            vec![
                opacity_animation_keyframe(0.0, 1.0),
                opacity_animation_keyframe(1.0, 0.0),
            ],
        )]);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(100, 300, 50, 50)), fading_box)
            .with(fill(IntRect::new(0, 500, 800, 50)), document);
        // The effect pushes a layer at every sample, also at the start, where it is opaque.
        assert!(
            tree.with_visual_animation_samples(0)
                .effect_pushes_layer_at_replay(fading_box.effect)
        );
        let at_start = list.planned(&tree.with_visual_animation_samples(0), &[]);
        let halfway = list.planned(&tree.with_visual_animation_samples(500_000_000), &[]);
        assert_eq!(at_start.placement, halfway.placement);
        assert_eq!(at_start.stack.layers, halfway.stack.layers);
        assert_eq!(
            whys_of_composited_effects(&halfway),
            [(fading_box.effect, WhyEffectIsComposited::TheCompositorAnimatesIt)]
        );
    }

    fn transform_animation_keyframe(
        offset: f64,
        kind: crate::host::FfiVisualAnimationTransformOperationKind,
        value: f32,
    ) -> crate::visual_animation::VisualAnimationKeyframe {
        crate::visual_animation::VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: crate::visual_animation::VisualAnimationValue::Transform(vec![
                crate::visual_animation::VisualAnimationTransformOperation::new(kind, &[value]).unwrap(),
            ]),
        }
    }

    fn opacity_animation_keyframe(offset: f64, opacity: f32) -> crate::visual_animation::VisualAnimationKeyframe {
        crate::visual_animation::VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: crate::visual_animation::VisualAnimationValue::Opacity(opacity),
        }
    }

    fn animation_of(
        target_kind: crate::host::FfiVisualAnimationTargetKind,
        node_index: u32,
        keyframes: Vec<crate::visual_animation::VisualAnimationKeyframe>,
    ) -> crate::visual_animation::VisualAnimation {
        use crate::visual_animation::VisualAnimation;
        VisualAnimation {
            target_kind,
            node_indices: vec![node_index],
            timing: crate::animation_timing::AnimationEffectTiming {
                iteration_duration: crate::animation_timing::TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            keyframes,
            ..VisualAnimation::default()
        }
    }

    fn spinning(node: SpatialNodeIndex) -> crate::visual_animation::VisualAnimation {
        use crate::host::{FfiVisualAnimationTargetKind, FfiVisualAnimationTransformOperationKind};
        animation_of(
            FfiVisualAnimationTargetKind::Transform,
            node.0,
            vec![
                transform_animation_keyframe(0.0, FfiVisualAnimationTransformOperationKind::Rotate, 0.0),
                transform_animation_keyframe(1.0, FfiVisualAnimationTransformOperationKind::Rotate, 360.0),
            ],
        )
    }

    fn sliding(node: SpatialNodeIndex) -> crate::visual_animation::VisualAnimation {
        use crate::host::{FfiVisualAnimationTargetKind, FfiVisualAnimationTransformOperationKind};
        animation_of(
            FfiVisualAnimationTargetKind::Transform,
            node.0,
            vec![
                transform_animation_keyframe(0.0, FfiVisualAnimationTransformOperationKind::TranslateX, 0.0),
                transform_animation_keyframe(1.0, FfiVisualAnimationTransformOperationKind::TranslateX, 300.0),
            ],
        )
    }

    fn fading(effect: EffectNodeIndex) -> crate::visual_animation::VisualAnimation {
        animation_of(
            crate::host::FfiVisualAnimationTargetKind::Opacity,
            effect.0,
            vec![
                opacity_animation_keyframe(0.0, 0.2),
                opacity_animation_keyframe(1.0, 1.0),
            ],
        )
    }

    fn transformed_context(tree: &mut VisualContextTree, parent: ContextRef, bounds: FloatRect) -> ContextRef {
        let spatial = tree.append_spatial(
            SpatialData::Transform(TransformData {
                matrix: FloatMatrix4x4::identity(),
                origin: FloatPoint {
                    x: bounds.x + bounds.width / 2.0,
                    y: bounds.y + bounds.height / 2.0,
                },
                sorting_context_root_index: None,
                flattens_inherited_transform: false,
                role: TransformDataRole::CssTransform,
                synthetic_plane: false,
                establishes_sorting_context: false,
                bounds,
            }),
            parent.spatial,
        );
        ContextRef { spatial, ..parent }
    }

    fn clipped_context(
        tree: &mut VisualContextTree,
        parent: ContextRef,
        rect: IntRect,
        corner_radii: libgfx_rust::CornerRadii,
    ) -> ContextRef {
        let clip = tree.append_clip(
            crate::visual_context::ClipNodeData::Rect(crate::visual_context::ClipData {
                rect: rect.to_float(),
                corner_radii,
                mode: crate::display_list::commands::ClipMode::Intersect,
            }),
            parent.clip,
            parent.spatial,
        );
        ContextRef { clip, ..parent }
    }

    // LibWeb appends an element's effects node at the spatial node of its parent, above the element's transform.
    fn element_with_own_effect(
        tree: &mut VisualContextTree,
        transformed: ContextRef,
        parent: ContextRef,
        opacity: f32,
    ) -> ContextRef {
        let effect = tree.append_effect(
            effects(opacity, CompositingAndBlendingOperator::Normal),
            parent.effect,
            parent.spatial,
            parent.clip,
        );
        ContextRef { effect, ..transformed }
    }

    fn transform_animation_of(
        node: SpatialNodeIndex,
        kind: crate::host::FfiVisualAnimationTransformOperationKind,
        from: f32,
        to: f32,
    ) -> crate::visual_animation::VisualAnimation {
        animation_of(
            crate::host::FfiVisualAnimationTargetKind::Transform,
            node.0,
            vec![
                transform_animation_keyframe(0.0, kind, from),
                transform_animation_keyframe(1.0, kind, to),
            ],
        )
    }

    fn space_names(stack: &LayerStack) -> Vec<String> {
        (0..stack.spaces.len())
            .map(|index| stack.name_of_space(RasterSpaceIndex(index), &NodeNamesFromIndices))
            .collect()
    }

    #[test]
    fn a_spinner_is_rastered_in_its_own_space_and_reaches_the_circle_it_turns_in() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner)
            .with(fill(IntRect::new(0, 100, 100, 100)), viewport)
            .stack(&tree, &[]);
        assert_eq!(space_names(&stack), ["viewport", "animated@s1 in viewport"]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&stack),
            [
                "base viewport band 0",
                "painted animated@s1 in viewport band 0",
                "painted viewport band 0 above band 0 of animated@s1 in viewport"
            ]
        );
        assert_eq!(
            stack.swept_reach_in_parent_space_by_space_index[1],
            Some(IntRect::new(34, 34, 72, 72))
        );
    }

    #[test]
    fn a_spinner_under_a_scale_far_below_the_smallest_raster_scale_is_rastered_at_the_smallest() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let shrunk = transformed_context(&mut tree, viewport, FloatRect::new(0.0, 0.0, 100.0, 100.0));
        let SpatialData::Transform(shrinking) = &mut tree.spatial_nodes[shrunk.spatial.0 as usize].data else {
            unreachable!()
        };
        shrinking.matrix = libgfx_rust::scale_matrix(0.001, 0.001, 1.0);
        let spinner = transformed_context(&mut tree, shrunk, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner)
            .stack(&tree, &[]);
        let raster_scales: Vec<f32> = stack
            .spaces
            .iter()
            .filter_map(|space| match space.anchor_kind {
                RasterSpaceAnchorKind::AnimatedTransform { raster_scale } => Some(raster_scale),
                _ => None,
            })
            .collect();
        assert_eq!(raster_scales, [1.0 / 64.0]);
    }

    #[test]
    fn samples_of_a_spinner_leave_its_space_and_raster_geometry_alike() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner);
        let geometry_of_spinner_space_at = |time_ns: i64| {
            let sample = tree.with_visual_animation_samples(time_ns);
            let (_, geometries) =
                spaces_with_geometries(&sample, &RasterSpaceAnchorsOfTree::of(&sample), &[], VIEWPORT);
            geometries[1].raster.world.draw_matrix(spinner.spatial)
        };
        assert_eq!(
            geometry_of_spinner_space_at(0),
            geometry_of_spinner_space_at(250_000_000)
        );
        let at_start = list.planned(&tree.with_visual_animation_samples(0), &[]);
        let a_quarter_in = list.planned(&tree.with_visual_animation_samples(250_000_000), &[]);
        assert_eq!(at_start, a_quarter_in);
    }

    #[test]
    fn an_unclipped_slide_reaches_where_its_keyframes_take_it() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let slider = transformed_context(&mut tree, viewport, FloatRect::new(0.0, 0.0, 50.0, 50.0));
        tree.set_visual_animations(vec![sliding(slider.spatial)]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 50, 50)), slider)
            .with(fill(IntRect::new(0, 50, 100, 100)), viewport)
            .stack(&tree, &[]);
        assert_eq!(space_names(&stack), ["viewport", "animated@s1 in viewport"]);
        assert_eq!(
            stack.swept_reach_in_parent_space_by_space_index[1],
            Some(IntRect::new(0, 0, 356, 56))
        );
    }

    #[test]
    fn a_slide_within_a_rounded_clip_is_composited_within_it() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let track = clipped_context(
            &mut tree,
            viewport,
            IntRect::new(0, 0, 200, 50),
            libgfx_rust::CornerRadii::uniform(10),
        );
        let slider = transformed_context(&mut tree, track, FloatRect::new(0.0, 0.0, 50.0, 50.0));
        tree.set_visual_animations(vec![sliding(slider.spatial)]);
        let planned = TestList::new()
            .with(fill(IntRect::new(0, 0, 200, 50)), track)
            .with(fill(IntRect::new(0, 0, 50, 50)), slider)
            .planned(&tree, &[]);
        let lines: Vec<String> = planned
            .layer_lines(
                &WindowServerPresentationOfMedia::default(),
                &NodeNamesFromIndices,
                |_| None,
            )
            .into_iter()
            .map(|(_, line)| line)
            .collect();
        assert_eq!(
            lines,
            [
                "[L0] base space=viewport",
                "[L1] painted space=animated@s1 in viewport composited within [0,0 200x50] radii=(10,10,10,10) of viewport"
            ]
        );
    }

    #[test]
    fn a_skew_or_an_overshooting_slide_stays_in_the_tiles_around_it() {
        use crate::host::FfiVisualAnimationTransformOperationKind as Kind;
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let skewed = transformed_context(&mut tree, viewport, FloatRect::new(0.0, 0.0, 50.0, 50.0));
        let overshooting = transformed_context(&mut tree, viewport, FloatRect::new(0.0, 100.0, 50.0, 50.0));
        let mut overshooting_slide = sliding(overshooting.spatial);
        overshooting_slide.keyframes[0].easing = crate::easing::Easing::CubicBezier {
            x1: 0.3,
            y1: 1.6,
            x2: 0.6,
            y2: 1.0,
        };
        tree.set_visual_animations(vec![
            transform_animation_of(skewed.spatial, Kind::SkewX, 0.0, 0.3),
            overshooting_slide,
        ]);
        let planned = TestList::new()
            .with(fill(IntRect::new(0, 0, 50, 50)), skewed)
            .with(fill(IntRect::new(0, 100, 50, 50)), overshooting)
            .planned(&tree, &[]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&planned.stack),
            ["base viewport band 0"]
        );
        assert_eq!(
            planned.placement.animations_without_layers,
            [
                (
                    AnimatedNode::Transform(skewed.spatial),
                    WhyAnimationHasNoLayer::NothingBoundsWhereItMoves
                ),
                (
                    AnimatedNode::Transform(overshooting.spatial),
                    WhyAnimationHasNoLayer::NothingBoundsWhereItMoves
                )
            ]
        );
    }

    #[test]
    fn a_spinner_with_its_own_opacity_is_composited_inside_it() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        let translucent_spinner = element_with_own_effect(&mut tree, spinner, viewport, 0.5);
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let planned = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), translucent_spinner)
            .planned(&tree, &[]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&planned.stack),
            ["base viewport band 0", "painted animated@s1 in viewport band 0"]
        );
        assert_eq!(
            whys_of_composited_effects(&planned),
            [(
                translucent_spinner.effect,
                WhyEffectIsComposited::ItAppliesOverATransformTheCompositorAnimates
            )]
        );
    }

    #[test]
    fn an_element_that_fades_and_spins_is_composited_through_both() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        let fading_spinner = element_with_own_effect(&mut tree, spinner, viewport, 1.0);
        tree.set_visual_animations(vec![spinning(spinner.spatial), fading(fading_spinner.effect)]);
        let planned = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), fading_spinner)
            .planned(&tree, &[]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&planned.stack),
            ["base viewport band 0", "painted animated@s1 in viewport band 0"]
        );
        assert_eq!(
            whys_of_composited_effects(&planned),
            // The fading effect pushes a layer at every sample, so it applies over the spinner at every sample.
            [(
                fading_spinner.effect,
                WhyEffectIsComposited::ItAppliesOverATransformTheCompositorAnimates
            )]
        );
    }

    #[test]
    fn a_spinner_in_a_translucent_group_with_siblings_is_placed_with_the_group() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let group = tree.append_effect_node_under(viewport, effects(0.5, CompositingAndBlendingOperator::Normal));
        let spinner = transformed_context(&mut tree, group, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), group)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner)
            .stack(&tree, &[]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&stack),
            ["base viewport band 0"]
        );
    }

    #[test]
    fn a_spinner_over_an_earlier_canvas_stays_in_the_tiles_around_it() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let planned = TestList::new()
            .with(canvas(IntRect::new(0, 0, 200, 120)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner)
            .planned(&tree, &[]);
        assert_eq!(
            planned.placement.animations_without_layers,
            [(
                AnimatedNode::Transform(spinner.spatial),
                WhyAnimationHasNoLayer::ItCanShowOverAnEarlierCanvasOrVideo
            )]
        );
    }

    #[test]
    fn a_spinner_holding_a_canvas_gives_the_canvas_a_layer_in_its_space() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let spinner = transformed_context(&mut tree, viewport, FloatRect::new(50.0, 50.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(canvas(IntRect::new(50, 50, 40, 40)), spinner)
            .stack(&tree, &[]);
        assert_eq!(
            kinds_of_layers_from_base_up_that_may_hold_content(&stack),
            ["base viewport band 0", "canvas #0 animated@s1 in viewport"]
        );
    }

    #[test]
    fn a_fade_over_an_earlier_canvas_stays_in_the_tiles_around_it() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let controls = tree.append_effect_node_under(viewport, effects(1.0, CompositingAndBlendingOperator::Normal));
        tree.set_visual_animations(vec![fading(controls.effect)]);
        let planned = TestList::new()
            .with(canvas(IntRect::new(0, 0, 200, 120)), viewport)
            .with(fill(IntRect::new(0, 80, 200, 40)), controls)
            .planned(&tree, &[]);
        assert!(planned.stack.composited_effects.is_empty());
        assert_eq!(
            planned.placement.animations_without_layers,
            [(
                AnimatedNode::Effect(controls.effect),
                WhyAnimationHasNoLayer::ItCanShowOverAnEarlierCanvasOrVideo
            )]
        );
    }

    // Before the animation starts, at its first keyframe, between its keyframes, at its last keyframe and after its
    // end, for an animation that `played_once_after_a_delay` gives.
    const SAMPLE_TIMES_NS: [i64; 6] = [0, 200_000_000, 450_000_000, 700_000_000, 1_200_000_000, 2_000_000_000];

    fn played_once_after_a_delay(
        animation: crate::visual_animation::VisualAnimation,
    ) -> crate::visual_animation::VisualAnimation {
        crate::visual_animation::VisualAnimation {
            timing: crate::animation_timing::AnimationEffectTiming {
                start_delay: crate::animation_timing::TimeValue::milliseconds(200.0),
                iteration_count: 1.0,
                ..animation.timing
            },
            ..animation
        }
    }

    fn planned_layers(list: &DisplayListStorage, tree: &VisualContextTree) -> PlannedLayers {
        plan_layer_stack(&LayerStackInputs {
            list,
            tree,
            scroll_offsets: &[],
            viewport_rect: VIEWPORT,
        })
    }

    fn layers_planned_alike_for_every_sample(list: &TestList, tree: &VisualContextTree) -> PlannedLayers {
        let list = DisplayListStorage::of_builder(&list.builder);
        let planned = planned_layers(&list, tree);
        for time_ns in SAMPLE_TIMES_NS {
            assert!(
                planned_layers(&list, &tree.with_visual_animation_samples(time_ns)) == planned,
                "the plan of the sample at {time_ns} ns differs from the plan of the tree"
            );
        }
        planned
    }

    fn filter_function(
        kind: crate::filter_bytes::FfiFilterFunctionKind,
        amount: f32,
    ) -> crate::filter_bytes::FfiFilterFunction {
        crate::filter_bytes::FfiFilterFunction {
            kind,
            amount,
            offset_x: 0.0,
            offset_y: 0.0,
            color: Color::TRANSPARENT,
            color_operation: libgfx_rust::ColorFilterType::Grayscale,
        }
    }

    fn grayscale(amount: f32) -> crate::filter_bytes::FfiFilterFunction {
        filter_function(crate::filter_bytes::FfiFilterFunctionKind::Color, amount)
    }

    fn blur(radius: f32) -> crate::filter_bytes::FfiFilterFunction {
        filter_function(crate::filter_bytes::FfiFilterFunctionKind::Blur, radius)
    }

    fn filter_bytes_of(functions: &[crate::filter_bytes::FfiFilterFunction]) -> Option<std::sync::Arc<Vec<u8>>> {
        crate::filter_bytes::filter_functions_graph(functions.iter().copied().map(libgfx_rust::filter::Filter::from))
            .map(|graph| std::sync::Arc::new(graph.serialize()))
    }

    fn filter_animation_of(
        effect: EffectNodeIndex,
        from: Vec<crate::filter_bytes::FfiFilterFunction>,
        to: Vec<crate::filter_bytes::FfiFilterFunction>,
    ) -> crate::visual_animation::VisualAnimation {
        let keyframe = |offset: f64, functions| crate::visual_animation::VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: crate::visual_animation::VisualAnimationValue::Filter(functions),
        };
        animation_of(
            crate::host::FfiVisualAnimationTargetKind::Filter,
            effect.0,
            vec![keyframe(0.0, from), keyframe(1.0, to)],
        )
    }

    fn element_with_filter(
        tree: &mut VisualContextTree,
        parent: ContextRef,
        filter: Option<std::sync::Arc<Vec<u8>>>,
    ) -> ContextRef {
        tree.append_effect_node_under(
            parent,
            EffectNodeData::Effects(EffectsData {
                isolation: false,
                opacity: 1.0,
                blend_mode: CompositingAndBlendingOperator::Normal,
                filter,
                backdrop_filter: None,
            }),
        )
    }

    fn frosted_glass(tree: &mut VisualContextTree, parent: ContextRef, region: IntRect) -> ContextRef {
        tree.append_effect_node_under(
            parent,
            EffectNodeData::Effects(EffectsData {
                isolation: false,
                opacity: 1.0,
                blend_mode: CompositingAndBlendingOperator::Normal,
                filter: None,
                backdrop_filter: Some(crate::visual_context::BackdropFilterData {
                    filter: std::sync::Arc::new(libgfx_rust::filter::Filter::blur(4.0, 4.0, None).serialize()),
                    region,
                    corner_radii: libgfx_rust::CornerRadii::default(),
                }),
            }),
        )
    }

    #[derive(Clone, Copy, Debug)]
    enum Animated {
        OpacityFromOne,
        ColorFilter,
        BlurFilter,
        BlurFilterFromNone,
        Translation,
        Rotation,
        Scale,
        RotationOutOfPlane,
    }

    #[derive(Clone, Copy, Debug)]
    enum UnderAnimatedNode {
        Fill,
        Canvas,
        BackdropFilter,
    }

    const ELEMENT: IntRect = IntRect::new(50, 50, 40, 40);

    // An element whose own value equals that of the first keyframe of its animation, between two fills of the page.
    fn page_with_animated_element(
        animated: Animated,
        under_animated_node: UnderAnimatedNode,
    ) -> (VisualContextTree, TestList, ContextRef) {
        use crate::host::FfiVisualAnimationTransformOperationKind as Kind;
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let (element, animation) = match animated {
            Animated::OpacityFromOne => {
                let element =
                    tree.append_effect_node_under(viewport, effects(1.0, CompositingAndBlendingOperator::Normal));
                let animation = animation_of(
                    crate::host::FfiVisualAnimationTargetKind::Opacity,
                    element.effect.0,
                    vec![
                        opacity_animation_keyframe(0.0, 1.0),
                        opacity_animation_keyframe(1.0, 0.0),
                    ],
                );
                (element, animation)
            }
            Animated::ColorFilter => {
                let element = element_with_filter(&mut tree, viewport, filter_bytes_of(&[grayscale(0.0)]));
                let animation = filter_animation_of(element.effect, vec![grayscale(0.0)], vec![grayscale(1.0)]);
                (element, animation)
            }
            Animated::BlurFilter => {
                let element = element_with_filter(&mut tree, viewport, filter_bytes_of(&[blur(0.0)]));
                let animation = filter_animation_of(element.effect, vec![blur(0.0)], vec![blur(4.0)]);
                (element, animation)
            }
            Animated::BlurFilterFromNone => {
                let element = element_with_filter(&mut tree, viewport, None);
                let animation = filter_animation_of(element.effect, Vec::new(), vec![blur(4.0)]);
                (element, animation)
            }
            Animated::Translation | Animated::Rotation | Animated::Scale | Animated::RotationOutOfPlane => {
                let element = transformed_context(&mut tree, viewport, ELEMENT.to_float());
                let (kind, from, to) = match animated {
                    Animated::Translation => (Kind::TranslateX, 0.0, 100.0),
                    Animated::Rotation => (Kind::Rotate, 0.0, 360.0),
                    Animated::Scale => (Kind::Scale, 1.0, 2.0),
                    _ => (Kind::RotateX, 0.0, 180.0),
                };
                (element, transform_animation_of(element.spatial, kind, from, to))
            }
        };
        tree.set_visual_animations(vec![played_once_after_a_delay(animation)]);
        let list = TestList::new().with(fill(IntRect::new(0, 0, 200, 200)), viewport);
        let list = match under_animated_node {
            UnderAnimatedNode::Fill => list.with(fill(ELEMENT), element),
            UnderAnimatedNode::Canvas => list.with(canvas(ELEMENT), element),
            UnderAnimatedNode::BackdropFilter => {
                let glass = frosted_glass(&mut tree, element, ELEMENT);
                list.with(fill(ELEMENT), glass)
            }
        };
        let list = list.with(fill(IntRect::new(0, 150, 200, 50)), viewport);
        (tree, list, element)
    }

    #[test]
    fn every_sample_of_an_animation_starting_at_the_value_of_the_tree_is_planned_as_the_tree_is() {
        for animated in [
            Animated::OpacityFromOne,
            Animated::ColorFilter,
            Animated::BlurFilter,
            Animated::BlurFilterFromNone,
            Animated::Translation,
            Animated::Rotation,
            Animated::Scale,
            Animated::RotationOutOfPlane,
        ] {
            for under_animated_node in [
                UnderAnimatedNode::Fill,
                UnderAnimatedNode::Canvas,
                UnderAnimatedNode::BackdropFilter,
            ] {
                let (tree, list, _) = page_with_animated_element(animated, under_animated_node);
                let stack = layers_planned_alike_for_every_sample(&list, &tree).stack;
                assert!(
                    stack.why_list_is_one_layer.is_none(),
                    "{animated:?} over {under_animated_node:?} is one layer: {:?}",
                    stack.why_list_is_one_layer
                );
            }
        }
    }

    fn whys_of_composited_effects(planned: &PlannedLayers) -> Vec<(EffectNodeIndex, WhyEffectIsComposited)> {
        planned
            .stack
            .composited_effects
            .iter()
            .zip(&planned.notes.composited_effects)
            .map(|(composited, note)| (composited.effect, note.why))
            .collect()
    }

    #[test]
    fn an_opacity_animation_starting_opaque_is_composited_at_every_sample() {
        let (tree, list, element) = page_with_animated_element(Animated::OpacityFromOne, UnderAnimatedNode::Fill);
        assert!(
            tree.with_visual_animation_samples(0)
                .effect_pushes_layer_at_replay(element.effect)
        );
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert_eq!(
            whys_of_composited_effects(&planned),
            [(element.effect, WhyEffectIsComposited::TheCompositorAnimatesIt)]
        );
    }

    #[test]
    fn a_backdrop_filter_in_an_element_whose_opacity_is_animated_reads_the_layer_of_the_element() {
        let (tree, list, _) = page_with_animated_element(Animated::OpacityFromOne, UnderAnimatedNode::BackdropFilter);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert!(
            planned
                .placement
                .placed_effects
                .iter()
                .all(|placed| !placed.reads_root_group)
        );
    }

    #[test]
    fn a_color_filter_animation_is_composited_and_a_blur_animation_is_not() {
        let (tree, list, element) = page_with_animated_element(Animated::ColorFilter, UnderAnimatedNode::Fill);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert_eq!(
            whys_of_composited_effects(&planned),
            [(element.effect, WhyEffectIsComposited::TheCompositorAnimatesIt)]
        );

        let (tree, list, element) = page_with_animated_element(Animated::BlurFilter, UnderAnimatedNode::Fill);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert!(planned.stack.composited_effects.is_empty());
        assert_eq!(
            planned.placement.animations_without_layers,
            [(
                AnimatedNode::Effect(element.effect),
                WhyAnimationHasNoLayer::ItsFilterSpreadsWhatItFilters
            )]
        );
        assert_eq!(
            planned.placement.why_compositor_cannot_composite_by_effect_index[element.effect.0 as usize],
            None
        );

        // Also where the tree holds no filter, as the first keyframe gives none.
        let (tree, list, element) = page_with_animated_element(Animated::BlurFilterFromNone, UnderAnimatedNode::Fill);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert_eq!(
            planned.placement.animations_without_layers,
            [(
                AnimatedNode::Effect(element.effect),
                WhyAnimationHasNoLayer::ItsFilterSpreadsWhatItFilters
            )]
        );
    }

    #[test]
    fn an_element_with_its_own_blur_animated_between_color_filters_is_planned_for_its_blur() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let element = element_with_filter(&mut tree, viewport, filter_bytes_of(&[blur(5.0)]));
        tree.set_visual_animations(vec![filter_animation_of(
            element.effect,
            vec![grayscale(0.0)],
            vec![grayscale(1.0)],
        )]);
        let list = DisplayListStorage::of_builder(
            &TestList::new()
                .with(fill(IntRect::new(0, 0, 200, 200)), viewport)
                .with(fill(ELEMENT), element)
                .builder,
        );
        let planned = planned_layers(&list, &tree);
        assert!(planned.stack.composited_effects.is_empty());
        assert_eq!(
            planned.placement.animations_without_layers,
            [(
                AnimatedNode::Effect(element.effect),
                WhyAnimationHasNoLayer::ItsFilterSpreadsWhatItFilters
            )]
        );
    }

    #[test]
    fn a_blur_animation_whose_easing_overshoots_reads_farther_than_is_known() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let element = element_with_filter(&mut tree, viewport, filter_bytes_of(&[blur(0.0)]));
        let mut animation = filter_animation_of(element.effect, vec![blur(0.0)], vec![blur(4.0)]);
        animation.keyframes[0].easing = crate::easing::Easing::CubicBezier {
            x1: 0.3,
            y1: 1.6,
            x2: 0.6,
            y2: 1.0,
        };
        tree.set_visual_animations(vec![animation]);
        let list = DisplayListStorage::of_builder(
            &TestList::new()
                .with(fill(IntRect::new(0, 0, 200, 200)), viewport)
                .with(fill(ELEMENT), element)
                .builder,
        );
        assert_eq!(
            planned_layers(&list, &tree)
                .placement
                .why_compositor_cannot_composite_by_effect_index[element.effect.0 as usize],
            Some(WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed)
        );
    }

    #[test]
    fn transforms_that_stay_in_the_plane_get_spaces_and_one_that_leaves_it_keeps_its_canvas_from_a_layer() {
        for animated in [Animated::Translation, Animated::Rotation, Animated::Scale] {
            let (tree, list, element) = page_with_animated_element(animated, UnderAnimatedNode::Canvas);
            let planned = layers_planned_alike_for_every_sample(&list, &tree);
            assert_eq!(
                space_names(&planned.stack),
                ["viewport", &format!("animated@s{} in viewport", element.spatial.0)]
            );
            assert!(planned.notes.media_without_layers.is_empty());
        }
        let (tree, list, _) = page_with_animated_element(Animated::RotationOutOfPlane, UnderAnimatedNode::Canvas);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert_eq!(space_names(&planned.stack), ["viewport"]);
        assert_eq!(
            planned.notes.media_without_layers,
            [(
                LiveContent::Canvas(CanvasId(1)),
                WhyMediaHasNoLayer::UnderTransformThatIsNot2DAffine
            )]
        );
    }

    #[test]
    fn a_backdrop_filter_moved_by_a_transform_animation_reads_anywhere() {
        let (tree, list, _) = page_with_animated_element(Animated::Translation, UnderAnimatedNode::BackdropFilter);
        let planned = layers_planned_alike_for_every_sample(&list, &tree);
        assert_eq!(space_names(&planned.stack), ["viewport"]);
        assert!(
            planned
                .placement
                .placed_effects
                .iter()
                .any(|placed| placed.backdrop_read_reach == Reach::Anywhere)
        );
    }

    #[test]
    fn a_plane_whose_animation_turns_it_out_of_its_sorting_context_is_sorted_by_depth_at_every_sample() {
        use crate::host::FfiVisualAnimationTransformOperationKind as Kind;
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let plane_of = |tree: &mut VisualContextTree, parent: SpatialNodeIndex, root: Option<SpatialNodeIndex>| {
            tree.append_spatial(
                SpatialData::Transform(TransformData {
                    matrix: FloatMatrix4x4::identity(),
                    origin: FloatPoint { x: 70.0, y: 70.0 },
                    sorting_context_root_index: root,
                    flattens_inherited_transform: false,
                    role: TransformDataRole::CssTransform,
                    synthetic_plane: false,
                    establishes_sorting_context: root.is_none(),
                    bounds: ELEMENT.to_float(),
                }),
                parent,
            )
        };
        let root = plane_of(&mut tree, viewport.spatial, None);
        let still_plane = plane_of(&mut tree, root, Some(root));
        let turning_plane = plane_of(&mut tree, root, Some(root));
        tree.set_visual_animations(vec![played_once_after_a_delay(transform_animation_of(
            turning_plane,
            Kind::RotateY,
            0.0,
            180.0,
        ))]);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 200, 200)), viewport)
            .with(
                fill(ELEMENT),
                ContextRef {
                    spatial: still_plane,
                    ..viewport
                },
            )
            .with(
                fill(ELEMENT),
                ContextRef {
                    spatial: turning_plane,
                    ..viewport
                },
            );
        // Replay sorts the planes by depth where they leave the plane of their context in the frame.
        assert!(!tree.with_visual_animation_samples(0).sorts_planes_by_depth());
        assert!(tree.with_visual_animation_samples(700_000_000).sorts_planes_by_depth());
        let storage = DisplayListStorage::of_builder(&list.builder);
        for time_ns in SAMPLE_TIMES_NS {
            let sample = tree.with_visual_animation_samples(time_ns);
            assert!(sample.may_sort_planes_by_depth());
            assert_eq!(
                sample.roots_of_contexts_sorting_planes_by_depth()[turning_plane.0 as usize],
                Some(root)
            );
            let run_groups = storage.run_groups(&sample);
            assert!(!run_groups.run_is_sorted_by_depth(0));
            assert!(run_groups.run_is_sorted_by_depth(1) && run_groups.run_is_sorted_by_depth(2));
        }
        let stack = layers_planned_alike_for_every_sample(&list, &tree).stack;
        assert!(stack.layering_obstacles.sorts_planes_by_depth);
        assert!(stack.why_list_is_one_layer.is_none());
    }

    #[test]
    fn a_rotation_out_of_the_plane_stays_in_it_only_where_it_keeps_its_angle() {
        use crate::display_list::animated_transform_spaces::transform_animation_stays_in_plane;
        use crate::host::FfiVisualAnimationTransformOperationKind as Kind;
        let node = SpatialNodeIndex(1);
        assert!(transform_animation_stays_in_plane(&transform_animation_of(
            node,
            Kind::Rotate,
            0.0,
            360.0
        )));
        assert!(transform_animation_stays_in_plane(&transform_animation_of(
            node,
            Kind::RotateX,
            0.0,
            0.0
        )));
        // Each keyframe is in the plane within the precision of a rotation matrix, but the angle changes.
        assert!(!transform_animation_stays_in_plane(&transform_animation_of(
            node,
            Kind::RotateX,
            0.0,
            1e-8
        )));
        assert!(!transform_animation_stays_in_plane(&transform_animation_of(
            node,
            Kind::RotateY,
            0.0,
            180.0
        )));
    }

    #[test]
    fn a_plan_holds_wherever_a_sticky_box_that_anchors_a_space_is_held() {
        use crate::visual_context::StickyData;
        let (mut tree, _, document) = tree_with_document_scroll();
        let mut sticky_data = StickyData::unconstrained(
            document.spatial,
            None,
            crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
            NodeSlotId::INVALID,
        );
        sticky_data.position_relative_to_scroller = FloatPoint { x: 0.0, y: 100.0 };
        sticky_data.border_box_size = libgfx_rust::FloatSize {
            width: 800.0,
            height: 50.0,
        };
        sticky_data.scrollport_size = libgfx_rust::FloatSize {
            width: 800.0,
            height: 600.0,
        };
        sticky_data.containing_block_region = FloatRect::new(0.0, 0.0, 800.0, 2000.0);
        sticky_data.needs_parent_offset_adjustment = true;
        sticky_data.inset_top = Some(0.0);
        let (_, highest_offset) = sticky_data.offset_range_without_sticky_ancestors();
        let sticky_node = tree.append_spatial(SpatialData::Sticky(sticky_data), document.spatial);
        let sticky = ContextRef {
            spatial: sticky_node,
            ..document
        };
        // The box comes over what the document paints after it only once the document is scrolled.
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(0, 100, 800, 50)), sticky)
            .with(canvas(IntRect::new(10, 110, 30, 30)), sticky)
            .with(fill(IntRect::new(0, 900, 800, 100)), document);
        let stack_with_box_held_at = |offset: FloatPoint| {
            let mut offsets = vec![FloatPoint::default(); tree.spatial_nodes.len()];
            offsets[sticky_node.0 as usize] = offset;
            list.stack(&tree, &offsets)
        };

        let stack = stack_with_box_held_at(FloatPoint::default());
        assert!(
            stack
                .spaces
                .iter()
                .any(|space| matches!(space.anchor_kind, RasterSpaceAnchorKind::StickyBox { .. }))
        );
        assert!(highest_offset.y > 800.0);
        // The compositor holds the box at offsets of its own as it scrolls the document.
        for y in [800.0, highest_offset.y] {
            assert_eq!(stack_with_box_held_at(FloatPoint { x: 0.0, y }), stack);
        }

        // WebContent does not plan again as it publishes where the box is held.
        let storage = DisplayListStorage::of_builder(&list.builder);
        let tree = Arc::new(tree);
        let inputs_with_box_held_at = |y: f32| {
            let mut offsets = vec![FloatPoint::default(); tree.spatial_nodes.len()];
            offsets[sticky_node.0 as usize].y = y;
            InputsOfLayerPlan::new(storage.clone(), tree.clone(), &offsets, VIEWPORT)
        };
        assert!(inputs_with_box_held_at(0.0).plan_alike_as(&inputs_with_box_held_at(800.0)));
    }

    fn scroller_with_record(
        tree: &mut VisualContextTree,
        records: &mut Vec<CompositorScrollNodeRecord>,
        parent: ContextRef,
        scrollport: IntRect,
        max_scroll_offset_y: f32,
    ) -> ContextRef {
        let clipped_to_scrollport = clipped_context(tree, parent, scrollport, libgfx_rust::CornerRadii::default());
        let scroll_node = tree.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            parent.spatial,
        );
        records.push(CompositorScrollNodeRecord {
            scroll_node_index: scroll_node,
            parent_scroll_node_index: parent.spatial,
            scrollport_rect: scrollport,
            max_scroll_offset: FloatPoint {
                x: 0.0,
                y: max_scroll_offset_y,
            },
            can_be_wheel_scrolled_vertically: true,
            ..CompositorScrollNodeRecord::default()
        });
        ContextRef {
            spatial: scroll_node,
            ..clipped_to_scrollport
        }
    }

    #[test]
    fn a_backdrop_filter_that_the_compositor_scrolls_within_a_space_is_not_placed_under_its_composite_clips() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let mut records = tree.compositor_scroll_table().nodes.to_vec();
        let outer_scroller =
            scroller_with_record(&mut tree, &mut records, document, IntRect::new(0, 0, 400, 400), 10.0);
        for index in 0..5 {
            scroller_with_record(
                &mut tree,
                &mut records,
                document,
                IntRect::new(420, 80 * index, 300, 70),
                500.0,
            );
        }
        // The inner scroller is past the most scrollers that get spaces, so the compositor scrolls it within the space
        // of the outer scroller.
        let inner_scroller = scroller_with_record(
            &mut tree,
            &mut records,
            outer_scroller,
            IntRect::new(0, 100, 400, 200),
            500.0,
        );
        assert!(tree.set_compositor_scroll_table(CompositorScrollTable {
            nodes: records.into(),
            ..CompositorScrollTable::default()
        }));
        let glass = frosted_glass(&mut tree, inner_scroller, IntRect::new(50, 150, 100, 100));
        let planned = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(0, 0, 400, 1000)), outer_scroller)
            .with(fill(IntRect::new(50, 150, 100, 100)), glass)
            .planned(&tree, &[]);
        let space_of_outer_scroller = planned
            .stack
            .spaces
            .iter()
            .position(|space| space.anchor == outer_scroller.spatial)
            .expect("the outer scroller anchors a space");
        assert!(
            planned
                .stack
                .spaces
                .iter()
                .all(|space| space.anchor != inner_scroller.spatial)
        );
        // Scrolled to the end of its range, the inner scroller moves the glass far past the outer scrollport, which
        // would cut what the glass reads where it is composited in the space of the outer scroller.
        let run_group_of_glass = planned
            .placement
            .run_groups
            .iter()
            .find(|run_group| run_group.first_run_index.0 <= 2 && 2 <= run_group.last_run_index.0)
            .expect("the run of the glass is placed");
        assert_ne!(run_group_of_glass.raster_space_index.0, space_of_outer_scroller);
    }

    #[test]
    fn content_painted_after_a_sticky_toolbar_covers_a_spinner_in_it() {
        use crate::visual_context::StickyData;
        let (mut tree, _, document) = tree_with_document_scroll();
        let mut sticky_data = StickyData::unconstrained(
            document.spatial,
            None,
            crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
            NodeSlotId::INVALID,
        );
        sticky_data.position_relative_to_scroller = FloatPoint { x: 0.0, y: 100.0 };
        sticky_data.border_box_size = libgfx_rust::FloatSize {
            width: 800.0,
            height: 50.0,
        };
        sticky_data.scrollport_size = libgfx_rust::FloatSize {
            width: 800.0,
            height: 600.0,
        };
        sticky_data.containing_block_region = FloatRect::new(0.0, 0.0, 800.0, 2000.0);
        sticky_data.needs_parent_offset_adjustment = true;
        sticky_data.inset_top = Some(0.0);
        let toolbar = ContextRef {
            spatial: tree.append_spatial(SpatialData::Sticky(sticky_data), document.spatial),
            ..document
        };
        let spinner = transformed_context(&mut tree, toolbar, FloatRect::new(650.0, 105.0, 40.0, 40.0));
        tree.set_visual_animations(vec![spinning(spinner.spatial)]);
        // The label and the spinner of the toolbar are in different cells, and the card covers both of them.
        let label = IntRect::new(10, 110, 100, 30);
        assert!(TILE_SIZE_IN_DEVICE_PIXELS < 650);
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(label), toolbar)
            .with(fill(IntRect::new(650, 105, 40, 40)), spinner)
            .with(fill(IntRect::new(0, 100, 800, 50)), document);
        let planned = list.planned(&tree, &[]);
        assert_eq!(
            space_names(&planned.stack),
            [
                "viewport",
                "document",
                "sticky@s2 in document",
                "animated@s3 in sticky@s2 in document"
            ]
        );
        let storage = DisplayListStorage::of_builder(&list.builder);
        let layers_of_card = planned.layers_of_record(storage.start_of_run(3));
        assert_eq!(
            layers_of_card.len(),
            1,
            "the card goes into one layer in every cell: {layers_of_card:?}"
        );
        let layer_of_card = &planned.stack.layers[layers_of_card[0].0];
        assert!(matches!(
            layer_of_card.identity,
            LayerIdentity::Painted {
                above_barrier: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn a_slide_farther_than_extents_reach_reaches_the_whole_viewport() {
        let mut tree = tree_with_root_isolation_effect();
        let viewport = root_context(&tree);
        let slider = transformed_context(&mut tree, viewport, FloatRect::new(0.0, 0.0, 50.0, 50.0));
        tree.set_visual_animations(vec![transform_animation_of(
            slider.spatial,
            crate::host::FfiVisualAnimationTransformOperationKind::TranslateX,
            -3.0e9,
            3.0e9,
        )]);
        let stack = TestList::new()
            .with(fill(IntRect::new(0, 0, 50, 50)), slider)
            .with(fill(IntRect::new(0, 50, 100, 100)), viewport)
            .stack(&tree, &[]);
        assert_eq!(space_names(&stack), ["viewport", "animated@s1 in viewport"]);
        assert_eq!(
            stack.swept_reach_in_parent_space_by_space_index[1],
            Some(IntRect::new(0, 0, 800, 56))
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "the compositor refuses a plan of layers the planner made")]
    fn the_planner_runs_the_checks_of_the_compositor_on_a_plan_of_more_layers() {
        let (tree, _, document) = tree_with_document_scroll();
        let viewport_past_coordinate_limit = IntRect::new(0, 0, 600_000_000, 600);
        let list = TestList::new().with(fill(IntRect::new(0, 0, 800, 2600)), document);
        plan_layer_stack(&LayerStackInputs {
            list: &DisplayListStorage::of_builder(&list.builder),
            tree: &tree,
            scroll_offsets: &[],
            viewport_rect: viewport_past_coordinate_limit,
        });
    }

    #[test]
    fn a_plan_holds_for_a_tree_that_differs_only_in_what_the_main_thread_keeps_of_its_nodes() {
        let (tree, _, document) = tree_with_document_scroll();
        let list = TestList::new().with(fill(IntRect::new(0, 0, 800, 2600)), document);
        let storage = DisplayListStorage::of_builder(&list.builder);
        let mut tree_of_other_paintable = tree.clone();
        let SpatialData::Scroll(scroll) = &mut tree_of_other_paintable.spatial_nodes[document.spatial.0 as usize].data
        else {
            unreachable!("the document scrolls");
        };
        scroll.state_slot = 3;
        scroll.owner_paintable = NodeSlotId::new(5, 1);
        let inputs = InputsOfLayerPlan::new(storage.clone(), Arc::new(tree), &[], VIEWPORT);
        assert!(inputs.plan_alike_as(&InputsOfLayerPlan::new(
            storage,
            Arc::new(tree_of_other_paintable),
            &[],
            VIEWPORT
        )));
    }

    #[test]
    fn a_plan_holds_for_inputs_that_differ_only_in_what_it_does_not_read() {
        let (mut tree, _, document) = tree_with_document_scroll();
        let scroller = tree.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            document.spatial,
        );
        let list = TestList::new()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(
                fill(IntRect::new(0, 0, 100, 100)),
                ContextRef {
                    spatial: scroller,
                    ..document
                },
            );
        let storage = DisplayListStorage::of_builder(&list.builder);
        let tree = Arc::new(tree);
        let offsets = |document_y: f32, scroller_y: f32| {
            let mut offsets = vec![FloatPoint::default(); tree.spatial_nodes.len()];
            offsets[document.spatial.0 as usize].y = document_y;
            offsets[scroller.0 as usize].y = scroller_y;
            offsets
        };
        let inputs = InputsOfLayerPlan::new(storage.clone(), tree.clone(), &offsets(0.0, 0.0), VIEWPORT);

        // The compositor scrolls the document, which has a row in its scroll table, and not the scroller.
        assert!(inputs.plan_alike_as(&inputs.with_scroll_offsets_and_viewport_rect(&offsets(700.0, 0.0), VIEWPORT)));
        // Inputs made after these for the same tree take what plans work out of the tree alone from them.
        let inputs_after = InputsOfLayerPlan::new_after(
            Some(&inputs),
            storage.clone(),
            tree.clone(),
            &offsets(700.0, 0.0),
            VIEWPORT,
        );
        assert!(Arc::ptr_eq(&inputs.anchors_of_tree, &inputs_after.anchors_of_tree));
        assert!(inputs.plan_alike_as(&inputs_after));
        assert!(!inputs.plan_alike_as(&inputs.with_scroll_offsets_and_viewport_rect(&offsets(0.0, 30.0), VIEWPORT)));
        assert!(!inputs.plan_alike_as(
            &inputs.with_scroll_offsets_and_viewport_rect(&offsets(0.0, 0.0), IntRect::new(0, 0, 800, 500))
        ));
        // A list is told apart by its chunks, and a tree by its content.
        let list_of_same_chunks = DisplayListStorage::of_builder(&list.builder);
        let tree_of_same_content = Arc::new((*tree).clone());
        assert!(inputs.plan_alike_as(&InputsOfLayerPlan::new(
            list_of_same_chunks,
            tree_of_same_content,
            &[],
            VIEWPORT
        )));
        let mut tree_of_other_content = (*tree).clone();
        tree_of_other_content.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            document.spatial,
        );
        assert!(!inputs.plan_alike_as(&InputsOfLayerPlan::new(
            storage,
            Arc::new(tree_of_other_content),
            &[],
            VIEWPORT
        )));
        let other_list = TestList::new().with(fill(IntRect::new(0, 0, 800, 2600)), document);
        assert!(!inputs.plan_alike_as(&InputsOfLayerPlan::new(
            DisplayListStorage::of_builder(&other_list.builder),
            tree.clone(),
            &[],
            VIEWPORT
        )));
        assert_eq!(inputs.plan().stack, list.stack(&tree, &offsets(700.0, 0.0)));
    }
}
