/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The content hashes of the tiles of the layers of one band, which alone decide whether a tile is rastered again.
//!
//! A tile's content hash covers what it holds: the records of the band that can show in it, as the geometry of the
//! band's raster space places their visual contexts, and the frame state those records read. A chunk is placed once for
//! each placement of the contexts of its runs, and the placement is cached on the chunk for every list that names it. A
//! tile is binned once and keeps its bin, and with it its content hash, for as long as the chunks that can reach it and
//! the barriers that cut it stay as they are and in the same order, whatever happens elsewhere in the list.

use super::builder::{
    HEADER_SIZE, bounding_rect_computed_from_payload, for_each_command, has_inline_clip_in_its_own_space,
    inline_clips_of, read_command,
};
use super::commands::{
    ClipMode, ClipNodeIndex, ContextRef, DisplayListCommandRun, DisplayListCommandType, DrawCompositedContext,
    EffectNodeIndex, FillRect, FillRectWithRoundedCorners, FillRoundedRectRing, PaintCaret, PaintScrollBar,
    SpatialNodeIndex,
};
use super::effect_clip_plan::EffectClipPlan;
use super::effect_geometry::{nearest_spreading_effects, spreading_filter_of};
use super::layers::{BandIndex, BarrierCutsOfCell, LayerIndex, LayerStack, PlacementNodeBand};
use super::nested_records::{
    NestedRecordsRole, for_each_command_including_nested, for_each_nested_record_span, span_bytes,
};
use super::placement::{
    ANTIALIASING_MARGIN_IN_DEVICE_PIXELS, FiltersAndClipsOfList, RasterSpaceGeometry, RunGroupIndex, RunGroups,
    is_finite_rect,
};
use super::reach::Reach;
use super::replay::{ScrollbarShownByFrame, caret_is_visible_at_time, scrollbar_shown_by_frame};
use super::storage::{
    ChunkIndexInList, ChunkName, DisplayListStorage, ListChunk, ListPosition, ListSpan, RunIndexInChunk,
};
use super::summary::{LiveContent, command_blend, live_content_of};
use super::tiles::{TileCoord, reaches_too_many_cells_to_follow_one_by_one, tile_coords_touching};
use crate::display_list::commands::{CanvasId, CompositorContextId, ImageFrameResourceId, VideoSinkHandle};
use crate::visual_context::{
    ClipData, ClipNode, ClipNodeData, EffectNodeData, VisualContextTree, WorldTransforms, device_offset_for_index,
};
use foldhash::fast::{FixedState, FoldHasher};
use libgfx_rust::{
    Color, CompositingAndBlendingOperator, CornerRadii, FloatMatrix4x4, FloatPoint, FloatRect, IntRect,
    enclosing_int_rect,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

pub type ContentHash = u128;

const LOW_HALF_SEED: u64 = 0x243f_6a88_85a3_08d3;
const HIGH_HALF_SEED: u64 = 0x1319_8a2e_0370_7344;
const LARGEST_FILTER_READ_REACH_IN_DEVICE_PIXELS: f32 = 2048.0;

struct ContentHasher {
    low: FoldHasher<'static>,
    high: FoldHasher<'static>,
}

impl ContentHasher {
    fn new() -> Self {
        Self {
            low: FixedState::with_seed(LOW_HALF_SEED).build_hasher(),
            high: FixedState::with_seed(HIGH_HALF_SEED).build_hasher(),
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        self.low.write(bytes);
        self.high.write(bytes);
    }

    fn write_u64(&mut self, value: u64) {
        self.low.write_u64(value);
        self.high.write_u64(value);
    }

    fn write_u32(&mut self, value: u32) {
        self.low.write_u32(value);
        self.high.write_u32(value);
    }

    fn write_i32(&mut self, value: i32) {
        self.write_u32(value as u32);
    }

    fn write_bool(&mut self, value: bool) {
        self.write_u32(u32::from(value));
    }

    fn write_f32(&mut self, value: f32) {
        self.write_u32(value.to_bits());
    }

    fn write_content_hash(&mut self, content_hash: ContentHash) {
        self.write_u64(content_hash as u64);
        self.write_u64((content_hash >> 64) as u64);
    }

    fn write_int_rect(&mut self, rect: IntRect) {
        self.write_i32(rect.x);
        self.write_i32(rect.y);
        self.write_i32(rect.width);
        self.write_i32(rect.height);
    }

    fn write_float_rect(&mut self, rect: FloatRect) {
        self.write_f32(rect.x);
        self.write_f32(rect.y);
        self.write_f32(rect.width);
        self.write_f32(rect.height);
    }

    fn write_float_point(&mut self, point: FloatPoint) {
        self.write_f32(point.x);
        self.write_f32(point.y);
    }

    fn write_matrix(&mut self, matrix: &FloatMatrix4x4) {
        for row in &matrix.elements {
            for value in row {
                self.write_f32(*value);
            }
        }
    }

    fn write_corner_radii(&mut self, corner_radii: &libgfx_rust::CornerRadii) {
        for corner in [
            corner_radii.top_left,
            corner_radii.top_right,
            corner_radii.bottom_right,
            corner_radii.bottom_left,
        ] {
            self.write_i32(corner.horizontal_radius);
            self.write_i32(corner.vertical_radius);
        }
    }

    fn write_reach(&mut self, reach: Reach<IntRect>) {
        match reach {
            Reach::Nothing => self.write_u32(0),
            Reach::Within(rect) => {
                self.write_u32(1);
                self.write_int_rect(rect);
            }
            Reach::Anywhere => self.write_u32(2),
        }
    }

    fn write_chunk_name(&mut self, name: ChunkName) {
        self.write_u64(name.id.0[0]);
        self.write_u64(name.id.0[1]);
        self.write_u32(name.occurrence);
    }

    fn finish(&self) -> ContentHash {
        (u128::from(self.high.finish()) << 64) | u128::from(self.low.finish())
    }
}

const FRAME_STATE_CARET_BLINK_PHASE: u8 = 1 << 0;
const FRAME_STATE_SCROLL_BAR: u8 = 1 << 1;
const FRAME_STATE_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS: u8 = 1 << 2;
const FRAME_STATE_SPACES_OF_INLINE_CLIPS: u8 = 1 << 3;

/// A top-level record of a chunk, with what placing it and hashing tile content read of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChunkRecord {
    pub(crate) offset: u32,
    pub(crate) end_offset: u32,
    pub(crate) run_index: RunIndexInChunk,
    pub(crate) command_type: DisplayListCommandType,
    pub(crate) bounding_rect: IntRect,
    draws_something: bool,
    frame_state_read_flags: u8,
    can_lower_destination_alpha: bool,
    // Record bytes hold no offset into the chunk, so a record hashes alike wherever it is.
    content_hash_of_bytes: ContentHash,
}

/// The index of a top-level record in the `RecordTable` of its chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordIndexInChunk(pub u32);

/// The top-level records of a chunk, by run, with what hashing tile content needs of each.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordTable {
    records: Vec<ChunkRecord>,
    first_record_index_of_each_run: Vec<RecordIndexInChunk>,
}

fn frame_state_read_flags_of_record(
    header: &super::commands::DisplayListCommandHeader,
    payload: &[u8],
    record_bytes: &[u8],
) -> u8 {
    let mut flags = match header.command_type {
        DisplayListCommandType::PaintCaret => FRAME_STATE_CARET_BLINK_PHASE,
        DisplayListCommandType::PaintScrollBar => FRAME_STATE_SCROLL_BAR,
        _ => 0,
    };
    if header.inline_clip_count != 0 && has_inline_clip_in_its_own_space(header, payload) {
        flags |= FRAME_STATE_SPACES_OF_INLINE_CLIPS;
    }
    for_each_command_including_nested(record_bytes, &mut |command_type, _, payload| {
        if live_content_of(command_type, payload).is_some()
            || !background_color_animation_effect_of(command_type, payload).is_none()
        {
            flags |= FRAME_STATE_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS;
        }
    });
    flags
}

// Records nested in a group, a pattern tile or a mask draw into a layer of their own; only those of a vector image draw
// straight into the destination.
fn record_can_lower_destination_alpha(command_type: DisplayListCommandType, payload: &[u8]) -> bool {
    if command_blend(command_type, payload).is_some_and(|(operator, _)| operator.lowers_destination_alpha()) {
        return true;
    }
    let mut can_lower = false;
    for_each_nested_record_span(command_type, payload, |role, span| {
        if role == NestedRecordsRole::VectorImage {
            for_each_command(span_bytes(payload, span), |header, _, payload| {
                can_lower |= record_can_lower_destination_alpha(header.command_type, payload);
            });
        }
    });
    can_lower
}

fn background_color_animation_effect_of(command_type: DisplayListCommandType, payload: &[u8]) -> EffectNodeIndex {
    match command_type {
        DisplayListCommandType::FillRect => read_command::<FillRect>(payload).background_color_animation_effect,
        DisplayListCommandType::FillRectWithRoundedCorners => {
            read_command::<FillRectWithRoundedCorners>(payload).background_color_animation_effect
        }
        _ => EffectNodeIndex::NONE,
    }
}

impl RecordTable {
    pub fn new(records_of_chunk: &[u8], command_runs: &[DisplayListCommandRun]) -> Self {
        let mut records = Vec::new();
        let mut first_record_index_of_each_run = Vec::with_capacity(command_runs.len() + 1);
        for (run_index, run) in command_runs.iter().enumerate() {
            first_record_index_of_each_run.push(RecordIndexInChunk(records.len() as u32));
            let run_bytes = &records_of_chunk[run.offset as usize..(run.offset + run.size) as usize];
            for_each_command(run_bytes, |header, offset_in_run, payload| {
                let offset = run.offset + offset_in_run as u32;
                let end_offset = offset + (HEADER_SIZE + payload.len()) as u32;
                let record_bytes = &records_of_chunk[offset as usize..end_offset as usize];
                let mut hasher = ContentHasher::new();
                hasher.write_bytes(record_bytes);
                let bounding_rect = bounding_rect_computed_from_payload(header, payload);
                records.push(ChunkRecord {
                    offset,
                    end_offset,
                    run_index: RunIndexInChunk(run_index as u32),
                    command_type: header.command_type,
                    bounding_rect: bounding_rect.unwrap_or_default(),
                    draws_something: !header.command_type.is_compositor_metadata()
                        && bounding_rect.is_some_and(|rect| !rect.is_empty()),
                    frame_state_read_flags: frame_state_read_flags_of_record(header, payload, record_bytes),
                    can_lower_destination_alpha: record_can_lower_destination_alpha(header.command_type, payload),
                    content_hash_of_bytes: hasher.finish(),
                });
            });
        }
        first_record_index_of_each_run.push(RecordIndexInChunk(records.len() as u32));
        Self {
            records,
            first_record_index_of_each_run,
        }
    }

    pub(crate) fn record(&self, index: RecordIndexInChunk) -> &ChunkRecord {
        &self.records[index.0 as usize]
    }

    pub(crate) fn records_in(&self, indices: std::ops::Range<RecordIndexInChunk>) -> &[ChunkRecord] {
        &self.records[indices.start.0 as usize..indices.end.0 as usize]
    }

    pub(crate) fn record_indices_of_run(&self, run_index: RunIndexInChunk) -> std::ops::Range<RecordIndexInChunk> {
        let run_index = run_index.0 as usize;
        self.first_record_index_of_each_run[run_index]..self.first_record_index_of_each_run[run_index + 1]
    }

    pub(crate) fn record_indices_between_offsets(
        &self,
        begin_offset: u32,
        end_offset: u32,
    ) -> std::ops::Range<RecordIndexInChunk> {
        let first = self.records.partition_point(|record| record.offset < begin_offset);
        let end = self.records.partition_point(|record| record.offset < end_offset);
        RecordIndexInChunk(first as u32)..RecordIndexInChunk(end as u32)
    }
}

pub trait LiveContentVersions {
    fn canvas_content_generation(&self, canvas: CanvasId) -> u64;
    fn video_frame_id(&self, video_sink: VideoSinkHandle) -> Option<u64>;
    fn image_frame_pixel_generation(&self, image_frame: ImageFrameResourceId) -> Option<u64>;
    fn nested_context_frame_generation(&self, nested_context: CompositorContextId) -> Option<u64>;
}

#[derive(Clone, Copy)]
pub struct FrameStateReadByReplay<'a> {
    pub scrollbars: &'a [ScrollbarShownByFrame],
    pub time_ns: i64,
    pub live_content: &'a dyn LiveContentVersions,
}

#[derive(Clone, Copy)]
pub struct LayerRasterSettings {
    pub raster_space_identity: u64,
    pub clear_color: Color,
    pub root_group_color: Color,
    pub viewport_to_raster_transform: FloatMatrix4x4,
    pub allows_lcd_text: bool,
}

impl LayerRasterSettings {
    fn content_hash(&self) -> ContentHash {
        let mut hasher = ContentHasher::new();
        hasher.write_u64(self.raster_space_identity);
        hasher.write_u32(self.clear_color.0);
        hasher.write_u32(self.root_group_color.0);
        hasher.write_matrix(&self.viewport_to_raster_transform);
        hasher.write_bool(self.allows_lcd_text);
        hasher.finish()
    }
}

pub struct TileKeyingOfList<'a> {
    display_list: &'a DisplayListStorage,
    run_groups: Arc<RunGroups>,
    effect_clips: &'a EffectClipPlan,
    tree: &'a VisualContextTree,
    frame_state: FrameStateReadByReplay<'a>,
    nearest_spreading_effects: Vec<EffectNodeIndex>,
    replays_every_record_into_every_tile: bool,
    tree_sorts_planes_by_depth: bool,
    frame_state_hash: ContentHash,
    mask_effect_and_position_of_declarations: Vec<(EffectNodeIndex, ListPosition)>,
    innermost_composited_effect_by_effect_index: Vec<EffectNodeIndex>,
}

impl<'a> TileKeyingOfList<'a> {
    /// Prepares keying the tiles of a frame's list, of which `stack` was worked out.
    pub fn new(
        display_list: &'a DisplayListStorage,
        tree: &'a VisualContextTree,
        stack: &LayerStack,
        frame_state: FrameStateReadByReplay<'a>,
    ) -> Self {
        let layering_obstacles = stack.layering_obstacles;
        let run_groups = display_list.run_groups(tree);
        let mask_effect_and_position_of_declarations = display_list
            .summary()
            .mask_declarations
            .iter()
            .map(|declaration| (declaration.mask_effect, declaration.record.position()))
            .collect();
        let mut is_composited_by_effect_index = vec![false; tree.effect_nodes.len()];
        for effect in stack.effects_the_compositor_composites() {
            is_composited_by_effect_index[effect.0 as usize] = true;
        }
        let mut innermost_composited_effect_by_effect_index = vec![EffectNodeIndex::NONE; tree.effect_nodes.len()];
        tree.visit_effect_nodes_parents_first(|index| {
            let node = &tree.effect_nodes[index];
            innermost_composited_effect_by_effect_index[index] = if is_composited_by_effect_index[index] {
                EffectNodeIndex(index as u32)
            } else if node.parent.is_none() {
                EffectNodeIndex::NONE
            } else {
                innermost_composited_effect_by_effect_index[node.parent.0 as usize]
            };
        });
        let one_layer_list_cannot_tell_which_records_show_in_tile =
            stack.replays_whole_list() && layering_obstacles.sorts_planes_by_depth;
        Self {
            display_list,
            run_groups,
            effect_clips: display_list.effect_clip_plan(tree),
            tree,
            frame_state,
            nearest_spreading_effects: nearest_spreading_effects(tree),
            replays_every_record_into_every_tile: layering_obstacles.every_tile_replays_every_record()
                || one_layer_list_cannot_tell_which_records_show_in_tile,
            tree_sorts_planes_by_depth: layering_obstacles.sorts_planes_by_depth,
            frame_state_hash: frame_state_hash_of(display_list, &frame_state),
            mask_effect_and_position_of_declarations,
            innermost_composited_effect_by_effect_index,
        }
    }

    fn compositor_composites(&self, effect: EffectNodeIndex) -> bool {
        self.innermost_composited_effect_by_effect_index[effect.0 as usize] == effect
    }
}

fn version_of_live_content(live_content: &dyn LiveContentVersions, content: LiveContent) -> u64 {
    match content {
        LiveContent::Canvas(canvas) => live_content.canvas_content_generation(canvas),
        LiveContent::VideoFrame(video_sink) => live_content.video_frame_id(video_sink).unwrap_or(u64::MAX),
        LiveContent::ImageFrame(image_frame) => live_content
            .image_frame_pixel_generation(image_frame)
            .unwrap_or(u64::MAX),
        LiveContent::CompositedContext(nested_context) => live_content
            .nested_context_frame_generation(nested_context)
            .unwrap_or(u64::MAX),
    }
}

fn write_scrollbar_shown_by_frame(hasher: &mut ContentHasher, scrollbar: &ScrollbarShownByFrame) {
    hasher.write_float_rect(scrollbar.gutter_rect);
    hasher.write_float_rect(scrollbar.thumb_rect);
    hasher.write_f32(scrollbar.expansion);
    hasher.write_f32(scrollbar.opacity);
}

fn frame_state_hash_of(display_list: &DisplayListStorage, frame_state: &FrameStateReadByReplay<'_>) -> ContentHash {
    let summary = display_list.summary();
    let mut hasher = ContentHasher::new();
    for caret in &summary.carets {
        let (_, payload) = display_list.record_at(caret.position());
        hasher.write_bool(caret_is_visible_at_time(
            &read_command::<PaintCaret>(payload),
            frame_state.time_ns,
        ));
    }
    for scrollbar in frame_state.scrollbars {
        hasher.write_u32(scrollbar.scroll_node_index.0);
        hasher.write_bool(scrollbar.vertical);
        write_scrollbar_shown_by_frame(&mut hasher, scrollbar);
    }
    for live_content_record in &summary.live_content_records {
        hasher.write_u64(version_of_live_content(
            frame_state.live_content,
            live_content_record.content,
        ));
    }
    hasher.finish()
}

struct BackdropReadInLayer {
    first_record_position: ListPosition,
    region_reach: Reach<IntRect>,
    backdrop_read_reach: Reach<IntRect>,
    read_reach_in_device_pixels: Option<i32>,
    spread_past_region_in_device_pixels: i32,
    // A mirroring blur stops at the frame's edge where its region reaches past the frame, as a raster of the frame
    // does.
    mirroring_blur_region: Option<Reach<IntRect>>,
}

impl BackdropReadInLayer {
    fn blur_mirrors_at_edge_of_frame(&self, frame: IntRect) -> bool {
        match self.mirroring_blur_region {
            None | Some(Reach::Nothing) => false,
            Some(Reach::Within(region)) => !frame.contains_rect(region),
            Some(Reach::Anywhere) => true,
        }
    }
}

impl BandBoundToListAndGeometry {
    fn frame_edge_backdrop_blurs_of_cell_mirror_at(&self, cell: IntRect, frame: Option<IntRect>) -> Option<IntRect> {
        frame.filter(|frame| {
            self.backdrop_reads.iter().any(|backdrop_read| {
                backdrop_read.region_reach.touches(cell) && backdrop_read.blur_mirrors_at_edge_of_frame(*frame)
            })
        })
    }
}

// A tile that shows part of a backdrop filter's region must also hold what the filter reads past that part.
fn reach_extended_through_backdrop_reads(
    mut reach: Reach<IntRect>,
    paint_position: ListPosition,
    backdrop_reads: &[BackdropReadInLayer],
) -> Reach<IntRect> {
    if backdrop_reads.is_empty() {
        return reach;
    }
    for _ in 0..=backdrop_reads.len() {
        let mut reach_grew = false;
        for backdrop_read in backdrop_reads {
            if paint_position >= backdrop_read.first_record_position {
                continue;
            }
            let Reach::Within(rect) = reach else {
                continue;
            };
            if !backdrop_read.backdrop_read_reach.touches(rect) {
                continue;
            }
            let reach_through_backdrop_read =
                match (backdrop_read.region_reach, backdrop_read.read_reach_in_device_pixels) {
                    (Reach::Within(region), Some(read_reach_in_device_pixels)) => {
                        let distance = read_reach_in_device_pixels + backdrop_read.spread_past_region_in_device_pixels;
                        Reach::Within(
                            rect.inflated_edges(distance, distance, distance, distance)
                                .intersected(region),
                        )
                    }
                    (region_reach, _) => region_reach,
                };
            let extended = reach.united(reach_through_backdrop_read);
            if extended != reach {
                reach = extended;
                reach_grew = true;
            }
        }
        if !reach_grew {
            break;
        }
    }
    reach
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DeviceAlignedRoundedRect {
    rect: FloatRect,
    horizontal_and_vertical_radii_clockwise_from_top_left: [(f32, f32); 4],
}

impl DeviceAlignedRoundedRect {
    fn of(
        world: &WorldTransforms,
        spatial: SpatialNodeIndex,
        rect: FloatRect,
        corner_radii: &CornerRadii,
    ) -> Option<Self> {
        let matrix = world.draw_matrix(spatial);
        if !matrix.is_2d_affine() {
            return None;
        }
        let affine = matrix.extract_2d_affine();
        if affine.b() != 0.0 || affine.c() != 0.0 || affine.a() <= 0.0 || affine.d() <= 0.0 {
            return None;
        }
        let rect = world.map_rect_to_root(spatial, rect);
        if !is_finite_rect(rect) {
            return None;
        }
        let radius_in_device_pixels = |radius: &libgfx_rust::CornerRadius| {
            (
                radius.horizontal_radius as f32 * affine.a(),
                radius.vertical_radius as f32 * affine.d(),
            )
        };
        Some(Self {
            rect,
            horizontal_and_vertical_radii_clockwise_from_top_left: [
                radius_in_device_pixels(&corner_radii.top_left),
                radius_in_device_pixels(&corner_radii.top_right),
                radius_in_device_pixels(&corner_radii.bottom_right),
                radius_in_device_pixels(&corner_radii.bottom_left),
            ],
        })
    }

    fn boxes_rounded_corners_curve_within(&self) -> [FloatRect; 4] {
        let rect = self.rect;
        let [top_left, top_right, bottom_right, bottom_left] =
            self.horizontal_and_vertical_radii_clockwise_from_top_left;
        [
            FloatRect::new(rect.x, rect.y, top_left.0, top_left.1),
            FloatRect::new(rect.right() - top_right.0, rect.y, top_right.0, top_right.1),
            FloatRect::new(
                rect.right() - bottom_right.0,
                rect.bottom() - bottom_right.1,
                bottom_right.0,
                bottom_right.1,
            ),
            FloatRect::new(rect.x, rect.bottom() - bottom_left.1, bottom_left.0, bottom_left.1),
        ]
    }

    fn rounded_corner_reaches_into(&self, area: FloatRect) -> bool {
        self.boxes_rounded_corners_curve_within()
            .iter()
            .any(|corner| !corner.is_empty() && !corner.intersected(area).is_empty())
    }

    fn covers(&self, area: FloatRect) -> bool {
        self.rect.contains_rect(area) && !self.rounded_corner_reaches_into(area)
    }

    fn misses(&self, area: FloatRect) -> bool {
        self.rect.intersected(area).is_empty()
    }

    fn write_part_within_or_whole_if_corner_reaches_in(&self, hasher: &mut ContentHasher, area: FloatRect) {
        for (horizontal, vertical) in self.horizontal_and_vertical_radii_clockwise_from_top_left {
            hasher.write_f32(horizontal);
            hasher.write_f32(vertical);
        }
        let rect = self.rect;
        if self.rounded_corner_reaches_into(area) {
            hasher.write_u32(1);
            hasher.write_float_rect(rect);
            return;
        }
        let part = rect.intersected(area);
        if part.is_empty() {
            hasher.write_u32(2);
            return;
        }
        hasher.write_u32(3);
        hasher.write_float_rect(part);
    }
}

#[derive(Clone, Copy, Debug)]
struct DeviceAlignedFill {
    rect: DeviceAlignedRoundedRect,
    unfilled_inner_rect_of_ring: Option<DeviceAlignedRoundedRect>,
    content_hash_except_rects: ContentHash,
    color: Color,
    draws_recorded_color_with_source_over: bool,
}

/// What a tile holds over a part of its cell, as its records tell without rastering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileCoverage {
    /// Every pixel of one color, which a raster gives exactly: the clear color of its layer, or an opaque one.
    OneColor(Color),
    /// Every pixel opaque, of more than one color.
    Opaque,
    Unknown,
}

impl TileCoverage {
    pub fn is_opaque(self) -> bool {
        match self {
            TileCoverage::OneColor(color) => color.alpha() == 255,
            TileCoverage::Opaque => true,
            TileCoverage::Unknown => false,
        }
    }

    fn under_plain_fill_of(self, color: Color) -> Self {
        match color.alpha() {
            255 => TileCoverage::OneColor(color),
            0 => self,
            _ if self.is_opaque() => TileCoverage::Opaque,
            _ => TileCoverage::Unknown,
        }
    }

    fn under_content_other_than_plain_fill(self, content_can_lower_destination_alpha: bool) -> Self {
        if self.is_opaque() && !content_can_lower_destination_alpha {
            TileCoverage::Opaque
        } else {
            TileCoverage::Unknown
        }
    }
}

enum WhatOneFillDoesWithinArea {
    DrawsNothing,
    CoversItWith(Color),
}

/// The index of a range in the `record_ranges` of a `ChunkPlacedInRasterSpace`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlacedRecordRangeIndex(pub u32);

#[derive(Clone, Copy, Debug)]
struct PlacedRecordRange {
    run_index: RunIndexInChunk,
    first_record_index: RecordIndexInChunk,
    end_record_index: RecordIndexInChunk,
    begin_offset: u32,
    end_offset: u32,
    reach: Reach<IntRect>,
    content_hash_without_frame_state: ContentHash,
    reads_frame_state: bool,
    context: ContextRef,
    is_spread_by_no_filter: bool,
    device_aligned_fill: Option<DeviceAlignedFill>,
}

struct ContentHashesNearArea {
    area: FloatRect,
    content_hash_by_clip: HashMap<u32, ContentHash>,
    content_hash_by_effect: HashMap<u32, ContentHash>,
}

impl ContentHashesNearArea {
    fn new(area: FloatRect) -> Self {
        Self {
            area,
            content_hash_by_clip: HashMap::new(),
            content_hash_by_effect: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PlacedRecordRangesOfRun {
    first_and_end_binned_range_index: (PlacedRecordRangeIndex, PlacedRecordRangeIndex),
    reach_of_binned_ranges: Reach<IntRect>,
    whole_run_range_index: PlacedRecordRangeIndex,
}

/// What the records of a chunk hold in one raster space, as the geometry of the space places the visual contexts of
/// the chunk's runs it holds: the record ranges the records are binned into tiles as, the reach of each, and the
/// content hash of what each draws. A chunk caches its latest placements for every list that names it.
pub struct ChunkPlacedInRasterSpace {
    placement_inputs_hash: ContentHash,
    record_ranges: Vec<PlacedRecordRange>,
    ranges_of_each_run: Vec<PlacedRecordRangesOfRun>,
    listed_reached_cells: Vec<TileCoord>,
    reach_past_listed_cells: Reach<IntRect>,
}

impl ChunkPlacedInRasterSpace {
    pub fn placement_inputs_hash(&self) -> ContentHash {
        self.placement_inputs_hash
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TileReplayItem {
    PlacedRecordRange {
        chunk_index: ChunkIndexInList,
        range_index: PlacedRecordRangeIndex,
    },
    // A record that no placed record range of the band holds, replayed for the mask content it declares.
    MaskDeclarationRecord {
        chunk_index: ChunkIndexInList,
        record_index: RecordIndexInChunk,
        begin_offset: u32,
        end_offset: u32,
    },
    SpanReplayedInEveryTile {
        chunk_index: ChunkIndexInList,
        begin_offset: u32,
        end_offset: u32,
    },
}

impl TileReplayItem {
    fn chunk_index(&self) -> ChunkIndexInList {
        match *self {
            TileReplayItem::PlacedRecordRange { chunk_index, .. }
            | TileReplayItem::MaskDeclarationRecord { chunk_index, .. }
            | TileReplayItem::SpanReplayedInEveryTile { chunk_index, .. } => chunk_index,
        }
    }

    fn with_chunk_index(self, chunk_index: ChunkIndexInList) -> Self {
        match self {
            TileReplayItem::PlacedRecordRange { range_index, .. } => TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index,
            },
            TileReplayItem::MaskDeclarationRecord {
                record_index,
                begin_offset,
                end_offset,
                ..
            } => TileReplayItem::MaskDeclarationRecord {
                chunk_index,
                record_index,
                begin_offset,
                end_offset,
            },
            TileReplayItem::SpanReplayedInEveryTile {
                begin_offset,
                end_offset,
                ..
            } => TileReplayItem::SpanReplayedInEveryTile {
                chunk_index,
                begin_offset,
                end_offset,
            },
        }
    }
}

/// A tile of a layer of a band, with the hash of what it holds, the replay items of the band replayed into it, and how
/// far past its cell content is read to raster it.
pub struct KeyedTile {
    pub layer_index: LayerIndex,
    pub coord: TileCoord,
    pub content_hash: ContentHash,
    pub raster_margin_in_device_pixels: Option<i32>,
    /// Which barriers cut the tile: tiles of a layer that are cut alike hold every replay item that reaches them alike,
    /// so one replay can raster them together.
    pub cuts: BarrierCutsOfCell,
    /// The part of the tile's cell that what it holds can reach.
    pub content_bounds: IntRect,
    // Names the layer in any stack whose band cuts the cell alike.
    count_of_cell_cuts_below_layer: usize,
    replay_items_in_paint_order: Vec<TileReplayItem>,
    raster_settings_hash: ContentHash,
    reads_frame_state: bool,
    frame_edge_backdrop_blurs_mirror_at: Option<IntRect>,
}

struct CellOfBandTiles {
    binning_inputs_hash: ContentHash,
    tiles: Vec<KeyedTile>,
}

struct ChunkHeldByBand {
    chunk_index: ChunkIndexInList,
    placement: Arc<ChunkPlacedInRasterSpace>,
    held_offset_ranges_in_paint_order: Vec<(u32, u32)>,
    held_run_indices: Vec<RunIndexInChunk>,
    reach_of_held_records: Reach<IntRect>,
    uses_listed_reached_cells: bool,
    reach_past_listed_cells: Reach<IntRect>,
    binning_inputs_hash: ContentHash,
}

struct RunReplayedWholeWhereverRunGroupShows {
    reach: Reach<IntRect>,
    mask_declarations_replayed_with_run: Vec<TileReplayItem>,
}

struct BandBoundToListAndGeometry {
    chunk_names: Arc<[ChunkName]>,
    held_chunks: Vec<ChunkHeldByBand>,
    held_chunk_index_by_chunk_index: HashMap<ChunkIndexInList, usize>,
    runs_replayed_whole: HashMap<(ChunkIndexInList, RunIndexInChunk), RunReplayedWholeWhereverRunGroupShows>,
    backdrop_reads: Vec<BackdropReadInLayer>,
    items_replayed_in_every_tile: Option<Vec<(ListPosition, TileReplayItem)>>,
    binning_inputs_hash_beyond_held_chunks: ContentHash,
}

impl BandBoundToListAndGeometry {
    fn held_chunk(&self, chunk_index: ChunkIndexInList) -> &ChunkHeldByBand {
        &self.held_chunks[self.held_chunk_index_by_chunk_index[&chunk_index]]
    }

    fn placed_record_range(
        &self,
        chunk_index: ChunkIndexInList,
        range_index: PlacedRecordRangeIndex,
    ) -> &PlacedRecordRange {
        &self.held_chunk(chunk_index).placement.record_ranges[range_index.0 as usize]
    }

    fn reach_of_item(&self, item: TileReplayItem) -> Reach<IntRect> {
        let TileReplayItem::PlacedRecordRange {
            chunk_index,
            range_index,
        } = item
        else {
            return Reach::Nothing;
        };
        let held_chunk = self.held_chunk(chunk_index);
        let range = &held_chunk.placement.record_ranges[range_index.0 as usize];
        if let Some(run_replayed_whole) = self.runs_replayed_whole.get(&(chunk_index, range.run_index))
            && held_chunk.placement.ranges_of_each_run[range.run_index.0 as usize].whole_run_range_index == range_index
        {
            return run_replayed_whole.reach;
        }
        reach_extended_through_backdrop_reads(
            range.reach,
            ListPosition::new(chunk_index, range.begin_offset),
            &self.backdrop_reads,
        )
    }

    fn span_of(&self, item: TileReplayItem) -> ListSpan {
        let (chunk_index, begin_offset, end_offset) = match item {
            TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index,
            } => {
                let range = self.placed_record_range(chunk_index, range_index);
                (chunk_index, range.begin_offset, range.end_offset)
            }
            TileReplayItem::MaskDeclarationRecord {
                chunk_index,
                begin_offset,
                end_offset,
                ..
            }
            | TileReplayItem::SpanReplayedInEveryTile {
                chunk_index,
                begin_offset,
                end_offset,
            } => (chunk_index, begin_offset, end_offset),
        };
        ListSpan {
            begin: ListPosition::new(chunk_index, begin_offset),
            end: ListPosition::new(chunk_index, end_offset),
        }
    }

    fn held_chunks_reaching_cells_in_paint_order(
        &self,
        coords: impl IntoIterator<Item = TileCoord>,
    ) -> BTreeMap<TileCoord, Vec<usize>> {
        let mut held_chunks_by_cell: BTreeMap<TileCoord, Vec<usize>> =
            coords.into_iter().map(|coord| (coord, Vec::new())).collect();
        let cells_area = held_chunks_by_cell
            .keys()
            .fold(IntRect::default(), |area, coord| area.united(coord.cell_rect()));
        for (held_chunk_index, held_chunk) in self.held_chunks.iter().enumerate() {
            if held_chunk.reach_of_held_records.part_of(cells_area).is_empty() {
                continue;
            }
            let mut note_cell = |coord: TileCoord| {
                if let Some(held_chunks) = held_chunks_by_cell.get_mut(&coord)
                    && held_chunks.last() != Some(&held_chunk_index)
                {
                    held_chunks.push(held_chunk_index);
                }
            };
            if held_chunk.uses_listed_reached_cells {
                for coord in &held_chunk.placement.listed_reached_cells {
                    note_cell(*coord);
                }
            }
            let reach_past_listed_cells_in_area = held_chunk.reach_past_listed_cells.part_of(cells_area);
            if !reach_past_listed_cells_in_area.is_empty() {
                for coord in tile_coords_touching(reach_past_listed_cells_in_area) {
                    note_cell(coord);
                }
            }
        }
        held_chunks_by_cell
    }

    fn binning_inputs_hash_of_cell(
        &self,
        band: &PlacementNodeBand,
        cell: IntRect,
        held_chunks_reaching_cell: &[usize],
    ) -> ContentHash {
        let mut hasher = ContentHasher::new();
        let mut cuts = band.cuts.iter().filter(|cut| cut.cuts_cell(cell)).peekable();
        // The layer of a replay item depends only on which cuts are painted before it, so a cut is hashed by where it
        // is among the chunks that reach the cell, and by its offset where it is in one of those. Which barrier a cut
        // is does not matter, as tiles name their layers by how many of the cell's cuts are below them, and a cut
        // painted after every chunk that reaches the cell has nothing above it there.
        let write_cut = |hasher: &mut ContentHasher, offset_in_chunk_reaching_cell: Option<u32>| {
            hasher.write_u32(2);
            hasher.write_u32(offset_in_chunk_reaching_cell.unwrap_or(u32::MAX));
        };
        for &held_chunk_index in held_chunks_reaching_cell {
            let held_chunk = &self.held_chunks[held_chunk_index];
            while cuts
                .next_if(|cut| cut.barrier_first_record_position.chunk_index < held_chunk.chunk_index)
                .is_some()
            {
                write_cut(&mut hasher, None);
            }
            hasher.write_u32(1);
            hasher.write_content_hash(held_chunk.binning_inputs_hash);
            while let Some(cut) =
                cuts.next_if(|cut| cut.barrier_first_record_position.chunk_index == held_chunk.chunk_index)
            {
                write_cut(&mut hasher, Some(cut.barrier_first_record_position.offset));
            }
        }
        hasher.finish()
    }
}

/// What the tiles of the layers of one band hold, kept from frame to frame, from list to list and from stack to stack:
/// the tiles keyed so far, binned against the list, the geometry and the band of the stack they are bound to. A tile is
/// binned again only where a chunk that can reach it or a barrier that cuts it changed, and keyed again only where
/// the frame state it reads or the raster settings of its layer changed.
#[derive(Default)]
pub struct BandTileKeys {
    cells: BTreeMap<TileCoord, CellOfBandTiles>,
    bound_band: Option<BandBoundToListAndGeometry>,
    frame_state_hash: ContentHash,
    // Frame state such as a sampled background color or the space of an inline clip depends on the bound tree and
    // geometry, so binding again can change it.
    tiles_reading_frame_state_need_rehashing: bool,
    pub replays_whole_list: bool,
}

/// What keying the shown tiles of a band did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileKeyingCounts {
    pub tiles_keyed: usize,
    /// The cells binned anew where the band holds something.
    pub cells_binned: usize,
}

impl BandTileKeys {
    pub fn tile(&self, coord: TileCoord, layer: LayerIndex) -> Option<&KeyedTile> {
        self.cells
            .get(&coord)?
            .tiles
            .iter()
            .find(|tile| tile.layer_index == layer)
    }

    /// The keyed tiles within `rect`, by row and then by layer.
    pub fn tiles_within(&self, rect: IntRect) -> impl Iterator<Item = &KeyedTile> {
        tile_coords_touching(rect).flat_map(|coord| self.cells.get(&coord).into_iter().flat_map(|cell| &cell.tiles))
    }

    pub fn record_spans_of_tiles<'a>(&self, tiles: impl IntoIterator<Item = &'a KeyedTile>) -> Vec<ListSpan> {
        let bound_band = self.bound_band.as_ref().expect("keyed tiles are bound to a list");
        let mut spans: Vec<ListSpan> = tiles
            .into_iter()
            .flat_map(|tile| {
                tile.replay_items_in_paint_order
                    .iter()
                    .map(|item| bound_band.span_of(*item))
            })
            .collect();
        spans.sort_unstable_by_key(|span| (span.begin, span.end));
        spans.dedup();
        let mut merged: Vec<ListSpan> = Vec::with_capacity(spans.len());
        for span in spans {
            match merged.last_mut() {
                Some(last) if last.end >= span.begin => last.end = last.end.max(span.end),
                _ => merged.push(span),
            }
        }
        merged
    }
}

struct LazyContentHashesPerNode {
    content_hash_by_node_index: RefCell<Vec<Option<ContentHash>>>,
    node_count: usize,
}

impl LazyContentHashesPerNode {
    fn new(node_count: usize) -> Self {
        Self {
            content_hash_by_node_index: RefCell::new(Vec::new()),
            node_count,
        }
    }

    fn get(&self, index: u32) -> Option<ContentHash> {
        self.content_hash_by_node_index
            .borrow()
            .get(index as usize)
            .copied()
            .flatten()
    }

    fn cache(&self, index: u32, content_hash: ContentHash) -> ContentHash {
        let mut content_hash_by_node_index = self.content_hash_by_node_index.borrow_mut();
        if content_hash_by_node_index.is_empty() {
            content_hash_by_node_index.resize(self.node_count, None);
        }
        content_hash_by_node_index[index as usize] = Some(content_hash);
        content_hash
    }
}

#[derive(Clone, Copy, PartialEq)]
enum EffectHashScope {
    AllEffects,
    EffectsDrawnInsideTiles,
    /// Every effect, but of those the compositor composites only what places their content: not their opacity or
    /// blend mode, which compositing applies, so that a chunk stays placed while the compositor animates them.
    WhatPlacingReads,
}

pub struct TileKeyingOfRasterSpace<'a> {
    list: &'a TileKeyingOfList<'a>,
    geometry: &'a RasterSpaceGeometry,
    spatial_content_hashes: LazyContentHashesPerNode,
    clip_content_hashes: LazyContentHashesPerNode,
    effect_content_hashes_by_scope: [LazyContentHashesPerNode; 3],
    cached_filter_read_reach_in_device_pixels_by_composited_effect: RefCell<HashMap<u32, Option<i32>>>,
    chunks_placed: Cell<u64>,
}

const CONTENT_HASH_OF_EFFECT_LEFT_OUT_OF_TILES: ContentHash = 1;

/// Whether a backdrop filter reads nothing past each pixel, or only through blurs that mirror at the edge of what they
/// read, so that what it reads past the edge of the frame does not show.
pub(crate) fn backdrop_filter_reads_nothing_past_mirrored_edge(filter: &[u8]) -> bool {
    let probe = FloatRect::new(0.0, 0.0, 1.0, 1.0);
    let reads_past_each_pixel = crate::filter_bytes::changed_output_bounds(filter, probe) != Some(probe);
    !reads_past_each_pixel || libgfx_rust::filter::Filter::serialized_has_mirroring_blur(filter)
}

pub(crate) fn filter_read_reach_in_device_pixels(filter: &[u8], effect_to_device: &FloatMatrix4x4) -> Option<i32> {
    if !effect_to_device.is_2d_affine() {
        return None;
    }
    let probe = FloatRect::new(0.0, 0.0, 1.0, 1.0);
    let changed_output_bounds_of_probe = crate::filter_bytes::changed_output_bounds(filter, probe)?;
    let reach_in_effect_space = [
        probe.x - changed_output_bounds_of_probe.x,
        changed_output_bounds_of_probe.right() - probe.right(),
        probe.y - changed_output_bounds_of_probe.y,
        changed_output_bounds_of_probe.bottom() - probe.bottom(),
    ]
    .into_iter()
    .fold(0.0f32, f32::max);
    let affine = effect_to_device.extract_2d_affine();
    let largest_stretch = affine.a().abs() + affine.b().abs() + affine.c().abs() + affine.d().abs();
    let reach_in_device_pixels =
        (reach_in_effect_space * largest_stretch).ceil() + ANTIALIASING_MARGIN_IN_DEVICE_PIXELS as f32;
    (reach_in_device_pixels.is_finite() && reach_in_device_pixels <= LARGEST_FILTER_READ_REACH_IN_DEVICE_PIXELS)
        .then_some(reach_in_device_pixels as i32)
}

/// How far past each pixel the filter of an effect the compositor composites reads what is composited inside it, in
/// device pixels. Placing a list composites no filter that reads farther than is followed, so that limit stands in only
/// while a transform the compositor animates scales a filter past what placing saw.
pub fn composited_filter_read_reach_in_device_pixels(filter: &[u8], effect_to_device: &FloatMatrix4x4) -> i32 {
    filter_read_reach_in_device_pixels(filter, effect_to_device)
        .unwrap_or(LARGEST_FILTER_READ_REACH_IN_DEVICE_PIXELS as i32)
}

impl<'a> TileKeyingOfRasterSpace<'a> {
    pub fn new(list: &'a TileKeyingOfList<'a>, geometry: &'a RasterSpaceGeometry) -> Self {
        let tree = list.tree;
        Self {
            list,
            geometry,
            spatial_content_hashes: LazyContentHashesPerNode::new(tree.spatial_nodes.len()),
            clip_content_hashes: LazyContentHashesPerNode::new(tree.clip_nodes.len()),
            effect_content_hashes_by_scope: [
                LazyContentHashesPerNode::new(tree.effect_nodes.len()),
                LazyContentHashesPerNode::new(tree.effect_nodes.len()),
                LazyContentHashesPerNode::new(tree.effect_nodes.len()),
            ],
            cached_filter_read_reach_in_device_pixels_by_composited_effect: RefCell::new(HashMap::new()),
            chunks_placed: Cell::new(0),
        }
    }

    /// How many chunks were placed in the space since it was made, rather than found placed alike before.
    pub fn chunks_placed(&self) -> u64 {
        self.chunks_placed.get()
    }

    fn reach_of_content_within(&self, rect: FloatRect, context: ContextRef) -> Reach<IntRect> {
        let list = self.list;
        let filters_and_clips = FiltersAndClipsOfList {
            tree: list.tree,
            nearest_spreading_effects: &list.nearest_spreading_effects,
            effect_clips: list.effect_clips,
            follows_filters_of_every_sample: false,
        };
        let composited_effect = if context.effect.is_none() {
            EffectNodeIndex::NONE
        } else {
            list.innermost_composited_effect_by_effect_index[context.effect.0 as usize]
        };
        if composited_effect.is_none() || list.nearest_spreading_effects[composited_effect.0 as usize].is_none() {
            return self.geometry.reach_of_content(&filters_and_clips, rect, context);
        }
        self.geometry.reach_of_content_inside_composited_effect(
            &filters_and_clips,
            rect,
            context,
            composited_effect,
            self.read_reach_in_device_pixels_of_filters_at_and_around(composited_effect),
        )
    }

    fn read_reach_in_device_pixels_of_filters_at_and_around(&self, composited_effect: EffectNodeIndex) -> Option<i32> {
        if let Some(&cached_read_reach) = self
            .cached_filter_read_reach_in_device_pixels_by_composited_effect
            .borrow()
            .get(&composited_effect.0)
        {
            return cached_read_reach;
        }
        let tree = self.list.tree;
        let mut read_reach = Some(0i32);
        let mut effect = composited_effect;
        while !effect.is_none() && read_reach.is_some() {
            let node = &tree.effect_nodes[effect.0 as usize];
            if let Some(filter) = spreading_filter_of(node) {
                read_reach = read_reach
                    .zip(filter_read_reach_in_device_pixels(
                        filter,
                        &self.geometry.world.draw_matrix(node.spatial),
                    ))
                    .map(|(sum, filter_read_reach)| sum.saturating_add(filter_read_reach));
            }
            effect = node.parent;
        }
        self.cached_filter_read_reach_in_device_pixels_by_composited_effect
            .borrow_mut()
            .insert(composited_effect.0, read_reach);
        read_reach
    }

    fn backdrop_read_in_layer(
        &self,
        effect: EffectNodeIndex,
        first_record_position: ListPosition,
    ) -> BackdropReadInLayer {
        let tree = self.list.tree;
        let node = &tree.effect_nodes[effect.0 as usize];
        let EffectNodeData::Effects(effects) = &node.data else {
            unreachable!("only effects with a backdrop filter are read as backdrops");
        };
        let backdrop_filter = effects
            .backdrop_filter
            .as_ref()
            .expect("only effects with a backdrop filter are read as backdrops");
        let region_in_effect_space = backdrop_filter.region.to_float();
        let backdrop_context = ContextRef {
            spatial: node.spatial,
            clip: self.list.effect_clips.output_clip(effect),
            effect,
        };
        let region_reach = self.reach_of_content_within(region_in_effect_space, backdrop_context);
        let effect_to_device = self.geometry.world.draw_matrix(node.spatial);
        let read_reach_in_device_pixels =
            filter_read_reach_in_device_pixels(&backdrop_filter.filter, &effect_to_device);
        let region_in_device_pixels = read_reach_in_device_pixels
            .map(|_| {
                self.geometry
                    .world
                    .map_rect_to_root(node.spatial, region_in_effect_space)
            })
            .filter(|rect| is_finite_rect(*rect))
            .map(enclosing_int_rect);
        let backdrop_read_reach = match (region_in_device_pixels, read_reach_in_device_pixels) {
            (Some(region), Some(read_reach)) => {
                Reach::Within(region.inflated_edges(read_reach, read_reach, read_reach, read_reach))
            }
            _ => Reach::Anywhere,
        };
        let spread_past_region_in_device_pixels = match (region_reach, region_in_device_pixels) {
            (Reach::Within(spread), Some(region)) => [
                region.x - spread.x,
                spread.right() - region.right(),
                region.y - spread.y,
                spread.bottom() - region.bottom(),
            ]
            .into_iter()
            .fold(0, i32::max),
            _ => 0,
        };
        let mirroring_blur_region = libgfx_rust::filter::Filter::serialized_has_mirroring_blur(&backdrop_filter.filter)
            .then(|| {
                Some(
                    self.geometry
                        .world
                        .map_rect_to_root(node.spatial, region_in_effect_space),
                )
                .filter(|rect| is_finite_rect(*rect))
                .map_or(Reach::Anywhere, |rect| Reach::Within(enclosing_int_rect(rect)))
            });
        BackdropReadInLayer {
            first_record_position,
            region_reach,
            backdrop_read_reach,
            read_reach_in_device_pixels: read_reach_in_device_pixels.filter(|_| region_in_device_pixels.is_some()),
            spread_past_region_in_device_pixels,
            mirroring_blur_region,
        }
    }

    fn spatial_content_hash(&self, spatial: SpatialNodeIndex) -> ContentHash {
        if let Some(cached) = self.spatial_content_hashes.get(spatial.0) {
            return cached;
        }
        let mut hasher = ContentHasher::new();
        hasher.write_matrix(&self.geometry.world.draw_matrix(spatial));
        hasher.write_bool(self.geometry.world.backface_culled[spatial.0 as usize]);
        self.spatial_content_hashes.cache(spatial.0, hasher.finish())
    }

    fn clip_content_hash(&self, clip: ClipNodeIndex) -> ContentHash {
        if clip.is_none() {
            return 0;
        }
        if let Some(cached) = self.clip_content_hashes.get(clip.0) {
            return cached;
        }
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        if self.geometry.clip_is_left_to_composite(clip) {
            return self
                .clip_content_hashes
                .cache(clip.0, self.clip_content_hash(node.parent));
        }
        let mut hasher = ContentHasher::new();
        hasher.write_content_hash(self.clip_content_hash(node.parent));
        self.write_clip_node(&mut hasher, clip);
        self.clip_content_hashes.cache(clip.0, hasher.finish())
    }

    fn write_clip_node(&self, hasher: &mut ContentHasher, clip: ClipNodeIndex) {
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        hasher.write_content_hash(self.spatial_content_hash(node.spatial));
        hasher.write_bool(node.clips_everything);
        match &node.data {
            ClipNodeData::Rect(data) => {
                hasher.write_u32(1);
                hasher.write_float_rect(data.rect);
                hasher.write_corner_radii(&data.corner_radii);
                hasher.write_u32(data.mode as u32);
            }
            ClipNodeData::Path(data) => {
                hasher.write_u32(2);
                hasher.write_int_rect(data.bounding_rect);
                hasher.write_u32(data.fill_rule as u32);
                hasher.write_bytes(&data.path.serialize_to_bytes());
            }
            ClipNodeData::Dead => hasher.write_u32(3),
        }
    }

    fn clip_content_hash_near_area(&self, clip: ClipNodeIndex, near_area: &mut ContentHashesNearArea) -> ContentHash {
        if clip.is_none() {
            return 0;
        }
        if let Some(&cached) = near_area.content_hash_by_clip.get(&clip.0) {
            return cached;
        }
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        let left_to_composite = self.geometry.clip_is_left_to_composite(clip);
        let parent_hash = self.clip_content_hash_near_area(node.parent, near_area);
        let content_hash = if left_to_composite {
            parent_hash
        } else {
            let mut hasher = ContentHasher::new();
            hasher.write_content_hash(parent_hash);
            let device_aligned_rect_and_mode = match &node.data {
                ClipNodeData::Rect(ClipData {
                    rect,
                    corner_radii,
                    mode,
                }) => DeviceAlignedRoundedRect::of(&self.geometry.world, node.spatial, *rect, corner_radii)
                    .map(|rect| (rect, *mode)),
                _ => None,
            };
            match device_aligned_rect_and_mode {
                Some((rect, mode)) => {
                    hasher.write_u32(4);
                    hasher.write_bool(node.clips_everything);
                    hasher.write_u32(mode as u32);
                    rect.write_part_within_or_whole_if_corner_reaches_in(&mut hasher, near_area.area);
                }
                None => self.write_clip_node(&mut hasher, clip),
            }
            hasher.finish()
        };
        near_area.content_hash_by_clip.insert(clip.0, content_hash);
        content_hash
    }

    fn effect_content_hash(&self, effect: EffectNodeIndex, scope: EffectHashScope) -> ContentHash {
        if effect.is_none() {
            return 0;
        }
        if matches!(scope, EffectHashScope::EffectsDrawnInsideTiles) && self.list.compositor_composites(effect) {
            return CONTENT_HASH_OF_EFFECT_LEFT_OUT_OF_TILES;
        }
        let cached_hashes = &self.effect_content_hashes_by_scope[scope as usize];
        if let Some(cached) = cached_hashes.get(effect.0) {
            return cached;
        }
        let node = &self.list.tree.effect_nodes[effect.0 as usize];
        let content_hash = self.content_hash_of_effect_node(
            effect,
            self.effect_content_hash(node.parent, scope),
            self.clip_content_hash(self.list.effect_clips.output_clip(effect)),
            scope,
        );
        cached_hashes.cache(effect.0, content_hash)
    }

    fn effect_content_hash_inside_tiles_near_area(
        &self,
        effect: EffectNodeIndex,
        near_area: &mut ContentHashesNearArea,
    ) -> ContentHash {
        if effect.is_none() {
            return 0;
        }
        if self.list.compositor_composites(effect) {
            return CONTENT_HASH_OF_EFFECT_LEFT_OUT_OF_TILES;
        }
        if let Some(&cached) = near_area.content_hash_by_effect.get(&effect.0) {
            return cached;
        }
        let node = &self.list.tree.effect_nodes[effect.0 as usize];
        let parent_hash = self.effect_content_hash_inside_tiles_near_area(node.parent, near_area);
        let output_clip_hash = self.clip_content_hash_near_area(self.list.effect_clips.output_clip(effect), near_area);
        let content_hash = self.content_hash_of_effect_node(
            effect,
            parent_hash,
            output_clip_hash,
            EffectHashScope::EffectsDrawnInsideTiles,
        );
        near_area.content_hash_by_effect.insert(effect.0, content_hash);
        content_hash
    }

    fn content_hash_of_effect_node(
        &self,
        effect: EffectNodeIndex,
        parent_hash: ContentHash,
        output_clip_hash: ContentHash,
        scope: EffectHashScope,
    ) -> ContentHash {
        let tree = self.list.tree;
        let node = &tree.effect_nodes[effect.0 as usize];
        let mut hasher = ContentHasher::new();
        // Sibling nodes of equal values are composited as a group each, so the index tells them apart.
        hasher.write_u32(effect.0);
        hasher.write_content_hash(parent_hash);
        hasher.write_content_hash(self.spatial_content_hash(node.spatial));
        hasher.write_content_hash(output_clip_hash);
        hasher.write_bool(tree.root_isolation_effect == Some(effect));
        let tiles_leave_effect_out = self.list.compositor_composites(effect);
        hasher.write_bool(tiles_leave_effect_out);
        match &node.data {
            EffectNodeData::Effects(effects) => {
                hasher.write_u32(1);
                hasher.write_bool(effects.isolation);
                let hashes_what_compositing_applies =
                    !(scope == EffectHashScope::WhatPlacingReads && tiles_leave_effect_out);
                if hashes_what_compositing_applies {
                    hasher.write_f32(effects.opacity);
                    hasher.write_u32(effects.blend_mode as u32);
                }
                match &effects.filter {
                    Some(filter) => hasher.write_bytes(filter),
                    None => hasher.write_u64(u64::MAX),
                }
                match &effects.backdrop_filter {
                    Some(backdrop_filter) => {
                        hasher.write_bytes(&backdrop_filter.filter);
                        hasher.write_int_rect(backdrop_filter.region);
                        hasher.write_corner_radii(&backdrop_filter.corner_radii);
                    }
                    None => hasher.write_u64(u64::MAX),
                }
            }
            EffectNodeData::Mask(mask) => {
                hasher.write_u32(2);
                hasher.write_int_rect(mask.rect);
                hasher.write_u32(mask.kind as u32);
                hasher.write_u32(mask.origin as u32);
                hasher.write_bool(mask.masks_backdrop);
            }
            EffectNodeData::BackgroundColorAnimation => hasher.write_u32(3),
            EffectNodeData::Dead => hasher.write_u32(4),
        }
        hasher.finish()
    }

    fn context_content_hash(&self, context: ContextRef, scope: EffectHashScope) -> ContentHash {
        let mut hasher = ContentHasher::new();
        hasher.write_content_hash(self.spatial_content_hash(context.spatial));
        hasher.write_content_hash(self.clip_content_hash(context.clip));
        hasher.write_content_hash(self.effect_content_hash(context.effect, scope));
        hasher.finish()
    }

    fn content_hash_of_placed_range_near_area(
        &self,
        chunk: &ListChunk,
        range: &PlacedRecordRange,
        near_area: &mut ContentHashesNearArea,
    ) -> ContentHash {
        let mut hasher = ContentHasher::new();
        hasher.write_content_hash(self.spatial_content_hash(range.context.spatial));
        hasher.write_content_hash(self.clip_content_hash_near_area(range.context.clip, near_area));
        hasher.write_content_hash(self.effect_content_hash_inside_tiles_near_area(range.context.effect, near_area));
        match &range.device_aligned_fill {
            Some(fill) => {
                hasher.write_content_hash(fill.content_hash_except_rects);
                fill.rect
                    .write_part_within_or_whole_if_corner_reaches_in(&mut hasher, near_area.area);
                // The inner rect's corners curve only within the outer rect's, whose part tells of them.
                if let Some(unfilled_inner_rect) = &fill.unfilled_inner_rect_of_ring {
                    unfilled_inner_rect.write_part_within_or_whole_if_corner_reaches_in(&mut hasher, near_area.area);
                }
            }
            None => {
                for record in chunk
                    .record_table()
                    .records_in(range.first_record_index..range.end_record_index)
                {
                    hasher.write_content_hash(record.content_hash_of_bytes);
                }
            }
        }
        hasher.finish()
    }

    fn write_frame_state_read_by_record(&self, hasher: &mut ContentHasher, chunk: &ListChunk, record: &ChunkRecord) {
        let list = self.list;
        let frame_state = &list.frame_state;
        let record_bytes = &chunk.records()[record.offset as usize..record.end_offset as usize];
        let (header, payload) = chunk.record_at(record.offset);
        if record.frame_state_read_flags & FRAME_STATE_CARET_BLINK_PHASE != 0 {
            hasher.write_bool(caret_is_visible_at_time(
                &read_command::<PaintCaret>(payload),
                frame_state.time_ns,
            ));
        }
        if record.frame_state_read_flags & FRAME_STATE_SCROLL_BAR != 0 {
            let command = read_command::<PaintScrollBar>(payload);
            match scrollbar_shown_by_frame(frame_state.scrollbars, &command) {
                Some(scrollbar) => {
                    hasher.write_u32(1);
                    write_scrollbar_shown_by_frame(hasher, scrollbar);
                }
                None => {
                    hasher.write_u32(2);
                    hasher.write_float_point(device_offset_for_index(
                        &self.geometry.raster_scroll_offsets,
                        command.scroll_node_index,
                    ));
                }
            }
        }
        if record.frame_state_read_flags & FRAME_STATE_SPACES_OF_INLINE_CLIPS != 0 {
            for clip in inline_clips_of(&header, payload).filter(|clip| clip.has_own_space) {
                match self.geometry.world.draw_matrix_if_known(clip.space) {
                    Some(matrix) => hasher.write_matrix(&matrix),
                    None => hasher.write_u64(u64::MAX),
                }
            }
        }
        if record.frame_state_read_flags & FRAME_STATE_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS == 0 {
            return;
        }
        let tree = list.tree;
        for_each_command_including_nested(record_bytes, &mut |command_type, _, payload| {
            if let Some(content) = live_content_of(command_type, payload) {
                hasher.write_u32(match content {
                    LiveContent::Canvas(_) => 1,
                    LiveContent::VideoFrame(_) => 2,
                    LiveContent::ImageFrame(_) => 3,
                    LiveContent::CompositedContext(_) => 4,
                });
                hasher.write_u64(version_of_live_content(frame_state.live_content, content));
                if matches!(content, LiveContent::CompositedContext(_)) {
                    let command = read_command::<DrawCompositedContext>(payload);
                    hasher.write_bool(tree.translations_snap_to_device_pixels(command.spatial_node_index));
                }
            }
            let background_color_animation_effect = background_color_animation_effect_of(command_type, payload);
            if !background_color_animation_effect.is_none() {
                let sampled = tree.sampled_background_color(background_color_animation_effect);
                hasher.write_u32(sampled.map_or(u32::MAX, |color| color.0));
                hasher.write_bool(sampled.is_some());
            }
        });
    }

    fn content_hash_of_records_without_frame_state(
        &self,
        chunk: &ListChunk,
        record_indices: std::ops::Range<RecordIndexInChunk>,
    ) -> ContentHash {
        let table = chunk.record_table();
        let mut hasher = ContentHasher::new();
        let mut run_index_of_previous_record = RunIndexInChunk(u32::MAX);
        for record in table.records_in(record_indices) {
            if record.run_index != run_index_of_previous_record {
                run_index_of_previous_record = record.run_index;
                hasher.write_content_hash(self.context_content_hash(
                    chunk.runs()[record.run_index.0 as usize].context,
                    EffectHashScope::EffectsDrawnInsideTiles,
                ));
            }
            hasher.write_content_hash(record.content_hash_of_bytes);
        }
        hasher.finish()
    }

    fn frame_state_hash_of_records(
        &self,
        chunk: &ListChunk,
        record_indices: std::ops::Range<RecordIndexInChunk>,
    ) -> Option<ContentHash> {
        let table = chunk.record_table();
        let records = table.records_in(record_indices);
        if records.iter().all(|record| record.frame_state_read_flags == 0) {
            return None;
        }
        let mut hasher = ContentHasher::new();
        for record in records.iter().filter(|record| record.frame_state_read_flags != 0) {
            self.write_frame_state_read_by_record(&mut hasher, chunk, record);
        }
        Some(hasher.finish())
    }

    fn content_hash_of_records(
        &self,
        chunk: &ListChunk,
        record_indices: std::ops::Range<RecordIndexInChunk>,
    ) -> ContentHash {
        let mut hasher = ContentHasher::new();
        hasher.write_content_hash(self.content_hash_of_records_without_frame_state(chunk, record_indices.clone()));
        if let Some(frame_state_hash) = self.frame_state_hash_of_records(chunk, record_indices) {
            hasher.write_content_hash(frame_state_hash);
        }
        hasher.finish()
    }

    /// The hash of what compositing the layers of a node inside `effect` reads of the effect: its values, where it is,
    /// and the clips it is entered within, which changes wherever the composited effect draws differently.
    pub fn content_hash_of_composited_effect(&self, effect: EffectNodeIndex) -> ContentHash {
        self.effect_content_hash(effect, EffectHashScope::AllEffects)
    }

    /// The hash of what the records within `span` draw in the frame.
    pub fn content_hash_of_records_within(&self, span: ListSpan) -> ContentHash {
        let display_list = self.list.display_list;
        let mut hasher = ContentHasher::new();
        for (chunk_index, begin_offset, end_offset) in display_list.parts_of_span(span) {
            let chunk = display_list.chunk(chunk_index);
            let table = chunk.record_table();
            let record_indices = table.record_indices_between_offsets(begin_offset, end_offset);
            let mut first_record_index_of_run_part = record_indices.start;
            while first_record_index_of_run_part < record_indices.end {
                let run_index = table.record(first_record_index_of_run_part).run_index;
                let end_record_index_of_run_part = table.record_indices_of_run(run_index).end.min(record_indices.end);
                hasher.write_content_hash(
                    self.content_hash_of_records(chunk, first_record_index_of_run_part..end_record_index_of_run_part),
                );
                first_record_index_of_run_part = end_record_index_of_run_part;
            }
        }
        hasher.finish()
    }

    fn place_record_range(
        &self,
        chunk: &ListChunk,
        run_index: RunIndexInChunk,
        record_indices: std::ops::Range<RecordIndexInChunk>,
        reach: Reach<IntRect>,
    ) -> PlacedRecordRange {
        let table = chunk.record_table();
        let context = chunk.runs()[run_index.0 as usize].context;
        let is_spread_by_filters =
            !context.effect.is_none() && !self.list.nearest_spreading_effects[context.effect.0 as usize].is_none();
        let is_one_record = (record_indices.start.0..record_indices.end.0).len() == 1;
        PlacedRecordRange {
            run_index,
            first_record_index: record_indices.start,
            end_record_index: record_indices.end,
            begin_offset: table.record(record_indices.start).offset,
            end_offset: table.record(RecordIndexInChunk(record_indices.end.0 - 1)).end_offset,
            reach,
            content_hash_without_frame_state: self
                .content_hash_of_records_without_frame_state(chunk, record_indices.clone()),
            reads_frame_state: table
                .records_in(record_indices.clone())
                .iter()
                .any(|record| record.frame_state_read_flags != 0),
            context,
            is_spread_by_no_filter: !is_spread_by_filters,
            device_aligned_fill: is_one_record
                .then(|| self.device_aligned_fill_of_record(chunk, table.record(record_indices.start), context))
                .flatten(),
        }
    }

    fn device_aligned_fill_of_record(
        &self,
        chunk: &ListChunk,
        record: &ChunkRecord,
        context: ContextRef,
    ) -> Option<DeviceAlignedFill> {
        let (header, payload) = chunk.record_at(record.offset);
        if header.inline_clip_count != 0 || header.has_inline_transform {
            return None;
        }
        let mut hasher = ContentHasher::new();
        hasher.write_u32(header.command_type as u32);
        let color;
        let draws_recorded_color_with_source_over;
        let (rect, corner_radii, unfilled_inner_rect_of_ring) = match header.command_type {
            DisplayListCommandType::FillRect => {
                let fill = read_command::<FillRect>(payload);
                hasher.write_u32(fill.color.0);
                hasher.write_u32(fill.compositing_and_blending_operator as u32);
                hasher.write_u32(fill.background_color_animation_effect.0);
                color = fill.color;
                draws_recorded_color_with_source_over = fill.compositing_and_blending_operator
                    == CompositingAndBlendingOperator::Normal
                    && fill.background_color_animation_effect.is_none();
                (fill.rect, CornerRadii::default(), None)
            }
            DisplayListCommandType::FillRectWithRoundedCorners => {
                let fill = read_command::<FillRectWithRoundedCorners>(payload);
                hasher.write_u32(fill.color.0);
                hasher.write_u32(fill.background_color_animation_effect.0);
                color = fill.color;
                draws_recorded_color_with_source_over = fill.background_color_animation_effect.is_none();
                (fill.rect, fill.corner_radii, None)
            }
            DisplayListCommandType::FillRoundedRectRing => {
                let ring = read_command::<FillRoundedRectRing>(payload);
                hasher.write_u32(ring.color.0);
                color = ring.color;
                draws_recorded_color_with_source_over = true;
                let top_width = ring.top_width.clamp(0, ring.rect.height.max(0));
                let bottom_width = ring.bottom_width.clamp(0, (ring.rect.height - top_width).max(0));
                let left_width = ring.left_width.clamp(0, ring.rect.width.max(0));
                let right_width = ring.right_width.clamp(0, (ring.rect.width - left_width).max(0));
                for width in [top_width, right_width, bottom_width, left_width] {
                    hasher.write_i32(width);
                }
                let unfilled_inner_rect = IntRect::new(
                    ring.rect.x + left_width,
                    ring.rect.y + top_width,
                    ring.rect.width - left_width - right_width,
                    ring.rect.height - top_width - bottom_width,
                );
                (ring.rect, ring.corner_radii, Some(unfilled_inner_rect))
            }
            _ => return None,
        };
        let unfilled_inner_rect_of_ring = match unfilled_inner_rect_of_ring {
            Some(unfilled_inner_rect) => Some(DeviceAlignedRoundedRect::of(
                &self.geometry.world,
                context.spatial,
                unfilled_inner_rect.to_float(),
                &CornerRadii::default(),
            )?),
            None => None,
        };
        Some(DeviceAlignedFill {
            rect: DeviceAlignedRoundedRect::of(&self.geometry.world, context.spatial, rect.to_float(), &corner_radii)?,
            unfilled_inner_rect_of_ring,
            content_hash_except_rects: hasher.finish(),
            color,
            draws_recorded_color_with_source_over,
        })
    }

    fn place_runs_of_chunk(
        &self,
        chunk: &ListChunk,
        run_indices: &[RunIndexInChunk],
        placement_inputs_hash: ContentHash,
    ) -> ChunkPlacedInRasterSpace {
        let table = chunk.record_table();
        let mut record_ranges = Vec::new();
        let mut ranges_of_each_run = vec![PlacedRecordRangesOfRun::default(); chunk.runs().len()];
        for &run_index in run_indices {
            let run = &chunk.runs()[run_index.0 as usize];
            let record_indices_of_run = table.record_indices_of_run(run_index);
            let run_reach = self.reach_of_content_within(run.ink_bounds.to_float(), run.context);
            let first_binned_range_index = PlacedRecordRangeIndex(record_ranges.len() as u32);
            let mut reach_of_binned_ranges = Reach::Nothing;
            if run_reach != Reach::Nothing {
                if !self.geometry.world.draw_matrix(run.context.spatial).is_2d_affine() {
                    record_ranges.push(self.place_record_range(
                        chunk,
                        run_index,
                        record_indices_of_run.clone(),
                        run_reach,
                    ));
                    reach_of_binned_ranges = run_reach;
                } else {
                    for record_index in
                        (record_indices_of_run.start.0..record_indices_of_run.end.0).map(RecordIndexInChunk)
                    {
                        let record = table.record(record_index);
                        if !record.draws_something {
                            continue;
                        }
                        let record_reach = self.reach_of_content_within(record.bounding_rect.to_float(), run.context);
                        if record_reach == Reach::Nothing {
                            continue;
                        }
                        record_ranges.push(self.place_record_range(
                            chunk,
                            run_index,
                            record_index..RecordIndexInChunk(record_index.0 + 1),
                            record_reach,
                        ));
                        reach_of_binned_ranges = reach_of_binned_ranges.united(record_reach);
                    }
                }
            }
            let end_binned_range_index = PlacedRecordRangeIndex(record_ranges.len() as u32);
            let whole_run_range_index = end_binned_range_index;
            record_ranges.push(self.place_record_range(chunk, run_index, record_indices_of_run, run_reach));
            ranges_of_each_run[run_index.0 as usize] = PlacedRecordRangesOfRun {
                first_and_end_binned_range_index: (first_binned_range_index, end_binned_range_index),
                reach_of_binned_ranges,
                whole_run_range_index,
            };
        }
        let mut listed_reached_cells = Vec::new();
        let mut reach_past_listed_cells = Reach::Nothing;
        for ranges_of_run in &ranges_of_each_run {
            let (first_binned_range_index, end_binned_range_index) = ranges_of_run.first_and_end_binned_range_index;
            for range in &record_ranges[first_binned_range_index.0 as usize..end_binned_range_index.0 as usize] {
                match range.reach {
                    Reach::Within(rect) if !reaches_too_many_cells_to_follow_one_by_one(rect) => {
                        listed_reached_cells.extend(tile_coords_touching(rect));
                    }
                    reach => reach_past_listed_cells = reach_past_listed_cells.united(reach),
                }
            }
        }
        listed_reached_cells.sort_unstable();
        listed_reached_cells.dedup();
        ChunkPlacedInRasterSpace {
            placement_inputs_hash,
            record_ranges,
            ranges_of_each_run,
            listed_reached_cells,
            reach_past_listed_cells,
        }
    }

    fn placement_of_runs_of_chunk(
        &self,
        chunk_index: ChunkIndexInList,
        run_indices: &[RunIndexInChunk],
    ) -> Arc<ChunkPlacedInRasterSpace> {
        let chunk = self.list.display_list.chunk(chunk_index);
        let mut hasher = ContentHasher::new();
        for &run_index in run_indices {
            hasher.write_u32(run_index.0);
            hasher.write_content_hash(self.context_content_hash(
                chunk.runs()[run_index.0 as usize].context,
                EffectHashScope::WhatPlacingReads,
            ));
        }
        let placement_inputs_hash = hasher.finish();
        let (placement, newly_placed) = chunk.find_or_make_placement(placement_inputs_hash, || {
            self.place_runs_of_chunk(chunk, run_indices, placement_inputs_hash)
        });
        if newly_placed {
            self.chunks_placed.set(self.chunks_placed.get() + 1);
        }
        placement
    }

    fn content_hash_of_replay_item(
        &self,
        bound_band: &BandBoundToListAndGeometry,
        item: TileReplayItem,
        includes_frame_state: bool,
        near_area: Option<&mut ContentHashesNearArea>,
    ) -> ContentHash {
        let display_list = self.list.display_list;
        let chunk = display_list.chunk(item.chunk_index());
        let (content_hash_without_frame_state, record_indices) = match item {
            TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index,
            } => {
                let range = bound_band.placed_record_range(chunk_index, range_index);
                let content_hash = match near_area {
                    Some(near_area) if range.is_spread_by_no_filter => {
                        self.content_hash_of_placed_range_near_area(chunk, range, near_area)
                    }
                    _ => range.content_hash_without_frame_state,
                };
                (content_hash, range.first_record_index..range.end_record_index)
            }
            TileReplayItem::MaskDeclarationRecord { record_index, .. } => (
                self.content_hash_of_records_without_frame_state(
                    chunk,
                    record_index..RecordIndexInChunk(record_index.0 + 1),
                ),
                record_index..RecordIndexInChunk(record_index.0 + 1),
            ),
            TileReplayItem::SpanReplayedInEveryTile {
                begin_offset,
                end_offset,
                ..
            } => {
                let record_indices = chunk
                    .record_table()
                    .record_indices_between_offsets(begin_offset, end_offset);
                (
                    self.content_hash_of_records_without_frame_state(chunk, record_indices.clone()),
                    record_indices,
                )
            }
        };
        let frame_state_hash = if includes_frame_state {
            self.frame_state_hash_of_records(chunk, record_indices)
        } else {
            None
        };
        match frame_state_hash {
            None => content_hash_without_frame_state,
            Some(frame_state_hash) => {
                let mut hasher = ContentHasher::new();
                hasher.write_content_hash(content_hash_without_frame_state);
                hasher.write_content_hash(frame_state_hash);
                hasher.finish()
            }
        }
    }

    fn records_of_item(&self, bound_band: &BandBoundToListAndGeometry, item: TileReplayItem) -> &[ChunkRecord] {
        let table = self.list.display_list.chunk(item.chunk_index()).record_table();
        let record_indices = match item {
            TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index,
            } => {
                let range = bound_band.placed_record_range(chunk_index, range_index);
                range.first_record_index..range.end_record_index
            }
            TileReplayItem::MaskDeclarationRecord { record_index, .. } => {
                record_index..RecordIndexInChunk(record_index.0 + 1)
            }
            TileReplayItem::SpanReplayedInEveryTile {
                begin_offset,
                end_offset,
                ..
            } => table.record_indices_between_offsets(begin_offset, end_offset),
        };
        table.records_in(record_indices)
    }

    fn root_of_context_sorting_item_by_depth(
        &self,
        bound_band: &BandBoundToListAndGeometry,
        item: TileReplayItem,
    ) -> Option<Option<SpatialNodeIndex>> {
        let chunk = self.list.display_list.chunk(item.chunk_index());
        let first_run_of_chunk = self.list.display_list.first_run_of_chunk(item.chunk_index());
        self.records_of_item(bound_band, item).iter().find_map(|record| {
            self.list
                .run_groups
                .run_is_sorted_by_depth(first_run_of_chunk + record.run_index.0 as usize)
                .then(|| {
                    self.list.run_groups.root_of_context_sorting_planes_by_depth(
                        chunk.runs()[record.run_index.0 as usize].context.spatial,
                    )
                })
        })
    }

    fn item_is_sorted_by_depth(&self, bound_band: &BandBoundToListAndGeometry, item: TileReplayItem) -> bool {
        let first_run_of_chunk = self.list.display_list.first_run_of_chunk(item.chunk_index());
        self.records_of_item(bound_band, item).iter().any(|record| {
            self.list
                .run_groups
                .run_is_sorted_by_depth(first_run_of_chunk + record.run_index.0 as usize)
        })
    }

    fn item_reads_frame_state(&self, bound_band: &BandBoundToListAndGeometry, item: TileReplayItem) -> bool {
        if let TileReplayItem::PlacedRecordRange {
            chunk_index,
            range_index,
        } = item
        {
            return bound_band
                .placed_record_range(chunk_index, range_index)
                .reads_frame_state;
        }
        self.records_of_item(bound_band, item)
            .iter()
            .any(|record| record.frame_state_read_flags != 0)
    }

    fn clip_chain_applied_in_tiles(&self, clip: ClipNodeIndex) -> impl Iterator<Item = &ClipNode> {
        let tree = self.list.tree;
        std::iter::successors((!clip.is_none()).then_some(clip), move |clip| {
            Some(tree.clip_nodes[clip.0 as usize].parent).filter(|parent| !parent.is_none())
        })
        .filter(|&clip| !self.geometry.clip_is_left_to_composite(clip))
        .map(move |clip| &tree.clip_nodes[clip.0 as usize])
    }

    fn effect_chain_applied_in_tiles(&self, effect: EffectNodeIndex) -> impl Iterator<Item = EffectNodeIndex> {
        let tree = self.list.tree;
        std::iter::successors((!effect.is_none()).then_some(effect), move |effect| {
            Some(tree.effect_nodes[effect.0 as usize].parent).filter(|parent| !parent.is_none())
        })
        .take_while(|&effect| !self.list.compositor_composites(effect))
    }

    fn clip_chain_leaves_area_unclipped(&self, clip: ClipNodeIndex, area_in_device_pixels: FloatRect) -> bool {
        self.clip_chain_applied_in_tiles(clip).all(|node| {
            let ClipNodeData::Rect(ClipData {
                rect,
                corner_radii,
                mode: ClipMode::Intersect,
            }) = &node.data
            else {
                return false;
            };
            !node.clips_everything
                && DeviceAlignedRoundedRect::of(&self.geometry.world, node.spatial, *rect, corner_radii)
                    .is_some_and(|rect| rect.covers(area_in_device_pixels))
        })
    }

    fn context_applies_no_clip_or_effect_within(&self, context: ContextRef, area_in_device_pixels: FloatRect) -> bool {
        let backface_culled = self
            .geometry
            .world
            .backface_culled
            .get(context.spatial.0 as usize)
            .copied()
            .unwrap_or(false);
        !backface_culled
            && self.clip_chain_leaves_area_unclipped(context.clip, area_in_device_pixels)
            && self.effect_chain_applied_in_tiles(context.effect).all(|effect| {
                let EffectNodeData::Effects(effects) = &self.list.tree.effect_nodes[effect.0 as usize].data else {
                    return false;
                };
                effects.opacity == 1.0
                    && effects.filter.is_none()
                    && effects.backdrop_filter.is_none()
                    && effects.blend_mode == CompositingAndBlendingOperator::Normal
                    && self.clip_chain_leaves_area_unclipped(
                        self.list.effect_clips.output_clip(effect),
                        area_in_device_pixels,
                    )
            })
    }

    fn what_one_fill_item_does_within_area(
        &self,
        bound_band: &BandBoundToListAndGeometry,
        item: TileReplayItem,
        area_in_device_pixels: FloatRect,
    ) -> Option<WhatOneFillDoesWithinArea> {
        let TileReplayItem::PlacedRecordRange {
            chunk_index,
            range_index,
        } = item
        else {
            return None;
        };
        let range = bound_band.placed_record_range(chunk_index, range_index);
        let fill = range.device_aligned_fill.as_ref()?;
        // The inner rect's corners curve only within the outer rect's corners.
        let area_is_inside_unfilled_inner_rect = fill.rect.covers(area_in_device_pixels)
            && fill
                .unfilled_inner_rect_of_ring
                .is_some_and(|unfilled_inner_rect| unfilled_inner_rect.covers(area_in_device_pixels));
        if (fill.rect.misses(area_in_device_pixels) || area_is_inside_unfilled_inner_rect)
            && range.is_spread_by_no_filter
        {
            return Some(WhatOneFillDoesWithinArea::DrawsNothing);
        }
        let fill_covers_area = fill.rect.covers(area_in_device_pixels)
            && fill
                .unfilled_inner_rect_of_ring
                .is_none_or(|unfilled_inner_rect| unfilled_inner_rect.misses(area_in_device_pixels));
        (fill_covers_area
            && fill.draws_recorded_color_with_source_over
            && self.context_applies_no_clip_or_effect_within(range.context, area_in_device_pixels))
        .then_some(WhatOneFillDoesWithinArea::CoversItWith(fill.color))
    }

    fn item_can_lower_destination_alpha(&self, bound_band: &BandBoundToListAndGeometry, item: TileReplayItem) -> bool {
        let chunk = self.list.display_list.chunk(item.chunk_index());
        self.records_of_item(bound_band, item).iter().any(|record| {
            let context = chunk.runs()[record.run_index.0 as usize].context;
            record.can_lower_destination_alpha
                || self.effect_chain_applied_in_tiles(context.effect).any(|effect| {
                    matches!(
                        &self.list.tree.effect_nodes[effect.0 as usize].data,
                        EffectNodeData::Effects(effects) if effects.blend_mode.lowers_destination_alpha()
                    )
                })
        })
    }

    /// What a raster of `tile` holds over `area` of its cell, in the device pixels of the space, as its replay items
    /// tell in paint order from `clear_color`, the color its layer's surface starts as.
    pub fn coverage_of_tile(
        &self,
        band_tile_keys: &BandTileKeys,
        tile: &KeyedTile,
        area: IntRect,
        clear_color: Color,
    ) -> TileCoverage {
        let Some(bound_band) = band_tile_keys.bound_band.as_ref() else {
            return TileCoverage::Unknown;
        };
        if self.list.replays_every_record_into_every_tile || area.is_empty() {
            return TileCoverage::Unknown;
        }
        // Fills are walked in list order, which depth-sorted planes are not replayed in.
        if self.list.tree_sorts_planes_by_depth
            && tile
                .replay_items_in_paint_order
                .iter()
                .any(|&item| self.item_is_sorted_by_depth(bound_band, item))
        {
            return TileCoverage::Unknown;
        }
        let area_in_device_pixels = area.to_float();
        let mut coverage = TileCoverage::OneColor(clear_color);
        for &item in &tile.replay_items_in_paint_order {
            if matches!(item, TileReplayItem::PlacedRecordRange { .. })
                && bound_band.reach_of_item(item).part_of(area).is_empty()
            {
                continue;
            }
            coverage = match self.what_one_fill_item_does_within_area(bound_band, item, area_in_device_pixels) {
                Some(WhatOneFillDoesWithinArea::DrawsNothing) => coverage,
                Some(WhatOneFillDoesWithinArea::CoversItWith(color)) => coverage.under_plain_fill_of(color),
                None => coverage
                    .under_content_other_than_plain_fill(self.item_can_lower_destination_alpha(bound_band, item)),
            };
            if coverage == TileCoverage::Unknown {
                break;
            }
        }
        coverage
    }

    fn content_hash_of_tile(&self, bound_band: &BandBoundToListAndGeometry, tile: &KeyedTile) -> ContentHash {
        let mut hasher = ContentHasher::new();
        hasher.write_content_hash(tile.raster_settings_hash);
        hasher.write_i32(tile.coord.column);
        hasher.write_i32(tile.coord.row);
        hasher.write_bool(self.list.replays_every_record_into_every_tile);
        match tile.frame_edge_backdrop_blurs_mirror_at {
            Some(frame) => hasher.write_int_rect(frame),
            None => hasher.write_u32(0),
        }
        let mut near_area = tile.raster_margin_in_device_pixels.map(|backdrop_read_margin| {
            let margin = 2.0 * (ANTIALIASING_MARGIN_IN_DEVICE_PIXELS + backdrop_read_margin) as f32;
            ContentHashesNearArea::new(tile.coord.cell_rect().to_float().inflated(margin, margin))
        });
        for &item in &tile.replay_items_in_paint_order {
            hasher.write_content_hash(self.content_hash_of_replay_item(
                bound_band,
                item,
                tile.reads_frame_state,
                near_area.as_mut(),
            ));
            // Depth-sorted planes replay in an order their sorting context decides.
            if self.list.tree_sorts_planes_by_depth
                && let Some(root) = self.root_of_context_sorting_item_by_depth(bound_band, item)
            {
                hasher.write_u32(1);
                hasher.write_u32(root.map_or(u32::MAX, |root| root.0));
            }
        }
        hasher.finish()
    }

    fn item_replaying_record(
        &self,
        bound_band: &BandBoundToListAndGeometry,
        chunk_index: ChunkIndexInList,
        record_index: RecordIndexInChunk,
    ) -> TileReplayItem {
        let table = self.list.display_list.chunk(chunk_index).record_table();
        let record = table.record(record_index);
        let record_alone = TileReplayItem::MaskDeclarationRecord {
            chunk_index,
            record_index,
            begin_offset: record.offset,
            end_offset: record.end_offset,
        };
        let Some(&held_chunk_index) = bound_band.held_chunk_index_by_chunk_index.get(&chunk_index) else {
            return record_alone;
        };
        let held_chunk = &bound_band.held_chunks[held_chunk_index];
        let run_index = record.run_index;
        let record_is_held = held_chunk
            .held_offset_ranges_in_paint_order
            .iter()
            .any(|&(begin, end)| begin <= record.offset && record.offset < end);
        if !record_is_held || !held_chunk.held_run_indices.contains(&run_index) {
            return record_alone;
        }
        let ranges_of_run = &held_chunk.placement.ranges_of_each_run[run_index.0 as usize];
        if bound_band.runs_replayed_whole.contains_key(&(chunk_index, run_index)) {
            return TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index: ranges_of_run.whole_run_range_index,
            };
        }
        let (first_binned_range_index, end_binned_range_index) = ranges_of_run.first_and_end_binned_range_index;
        (first_binned_range_index.0..end_binned_range_index.0)
            .map(PlacedRecordRangeIndex)
            .find(|&range_index| {
                let range = &held_chunk.placement.record_ranges[range_index.0 as usize];
                range.first_record_index <= record_index && record_index < range.end_record_index
            })
            .map_or(record_alone, |range_index| TileReplayItem::PlacedRecordRange {
                chunk_index,
                range_index,
            })
    }

    fn bind_band_to_list_and_geometry(&self, stack: &LayerStack, band_index: BandIndex) -> BandBoundToListAndGeometry {
        let list = self.list;
        let display_list = list.display_list;
        let band = &stack.bands[band_index.0];
        let mut held_offset_ranges_by_chunk: Vec<(ChunkIndexInList, Vec<(u32, u32)>)> = Vec::new();
        for span in &band.record_spans {
            for (chunk_index, begin, end) in display_list.parts_of_span(*span) {
                match held_offset_ranges_by_chunk.last_mut() {
                    Some((last_chunk_index, held_offset_ranges)) if *last_chunk_index == chunk_index => {
                        match held_offset_ranges.last_mut() {
                            Some(last) if last.1 == begin => last.1 = end,
                            _ => held_offset_ranges.push((begin, end)),
                        }
                    }
                    _ => held_offset_ranges_by_chunk.push((chunk_index, vec![(begin, end)])),
                }
            }
        }
        let mut band_binning_inputs_hasher = ContentHasher::new();
        band_binning_inputs_hasher.write_bool(stack.base_layer == band.layer_below_all_cuts);
        let chunk_names = display_list.chunk_names();
        if list.replays_every_record_into_every_tile {
            let mut items_replayed_in_every_tile: Vec<(ListPosition, TileReplayItem)> = held_offset_ranges_by_chunk
                .iter()
                .flat_map(|(chunk_index, held_offset_ranges)| {
                    held_offset_ranges.iter().map(|&(begin_offset, end_offset)| {
                        (
                            ListPosition::new(*chunk_index, begin_offset),
                            TileReplayItem::SpanReplayedInEveryTile {
                                chunk_index: *chunk_index,
                                begin_offset,
                                end_offset,
                            },
                        )
                    })
                })
                .collect();
            let tree = list.tree;
            let mut masks_around_held_runs = Vec::new();
            for (chunk_index, held_offset_ranges) in &held_offset_ranges_by_chunk {
                for run in display_list.chunk(*chunk_index).runs() {
                    if !held_offset_ranges
                        .iter()
                        .any(|&(begin, end)| begin < run.offset + run.size && run.offset < end)
                    {
                        continue;
                    }
                    let mut effect = run.context.effect;
                    while !effect.is_none() {
                        let node = &tree.effect_nodes[effect.0 as usize];
                        if matches!(node.data, EffectNodeData::Mask(_)) && !masks_around_held_runs.contains(&effect) {
                            masks_around_held_runs.push(effect);
                        }
                        effect = node.parent;
                    }
                }
            }
            for &(mask_effect, declaration_position) in &list.mask_effect_and_position_of_declarations {
                let declaration_is_held =
                    held_offset_ranges_by_chunk
                        .iter()
                        .any(|(chunk_index, held_offset_ranges)| {
                            *chunk_index == declaration_position.chunk_index
                                && held_offset_ranges.iter().any(|&(begin, end)| {
                                    begin <= declaration_position.offset && declaration_position.offset < end
                                })
                        });
                if !masks_around_held_runs.contains(&mask_effect) || declaration_is_held {
                    continue;
                }
                let table = display_list.chunk(declaration_position.chunk_index).record_table();
                let record_index = table
                    .record_indices_between_offsets(declaration_position.offset, declaration_position.offset + 1)
                    .start;
                items_replayed_in_every_tile.push((
                    declaration_position,
                    TileReplayItem::MaskDeclarationRecord {
                        chunk_index: declaration_position.chunk_index,
                        record_index,
                        begin_offset: declaration_position.offset,
                        end_offset: table.record(record_index).end_offset,
                    },
                ));
            }
            return BandBoundToListAndGeometry {
                chunk_names,
                held_chunks: Vec::new(),
                held_chunk_index_by_chunk_index: HashMap::new(),
                runs_replayed_whole: HashMap::new(),
                backdrop_reads: Vec::new(),
                items_replayed_in_every_tile: Some(items_replayed_in_every_tile),
                binning_inputs_hash_beyond_held_chunks: band_binning_inputs_hasher.finish(),
            };
        }

        let mut held_chunks: Vec<ChunkHeldByBand> = Vec::with_capacity(held_offset_ranges_by_chunk.len());
        for (chunk_index, held_offset_ranges) in held_offset_ranges_by_chunk {
            let runs = display_list.chunk(chunk_index).runs();
            let held_run_indices: Vec<RunIndexInChunk> = (0..runs.len() as u32)
                .map(RunIndexInChunk)
                .filter(|&run_index| {
                    let run = &runs[run_index.0 as usize];
                    held_offset_ranges
                        .iter()
                        .any(|&(begin, end)| begin < run.offset + run.size && run.offset < end)
                })
                .collect();
            let placement = self.placement_of_runs_of_chunk(chunk_index, &held_run_indices);
            held_chunks.push(ChunkHeldByBand {
                chunk_index,
                placement,
                held_offset_ranges_in_paint_order: held_offset_ranges,
                held_run_indices,
                reach_of_held_records: Reach::Nothing,
                uses_listed_reached_cells: false,
                reach_past_listed_cells: Reach::Nothing,
                binning_inputs_hash: 0,
            });
        }
        let held_chunk_index_by_chunk_index: HashMap<ChunkIndexInList, usize> = held_chunks
            .iter()
            .enumerate()
            .map(|(held_chunk_index, held_chunk)| (held_chunk.chunk_index, held_chunk_index))
            .collect();
        let first_held_position_of_run = |held_chunk: &ChunkHeldByBand, run_index: RunIndexInChunk| {
            let run = &display_list.chunk(held_chunk.chunk_index).runs()[run_index.0 as usize];
            let begin = held_chunk
                .held_offset_ranges_in_paint_order
                .iter()
                .find(|&&(begin, end)| begin < run.offset + run.size && run.offset < end)
                .map_or(run.offset, |&(begin, _)| begin.max(run.offset));
            ListPosition::new(held_chunk.chunk_index, begin)
        };

        let tree = list.tree;
        let effect_composited_around_band = stack.nodes[band.placement_node_index.0]
            .composited_effect
            .unwrap_or(EffectNodeIndex::NONE);
        let mut effect_was_visited = vec![false; tree.effect_nodes.len()];
        let mut backdrop_reads = Vec::new();
        for held_chunk in &held_chunks {
            for &run_index in &held_chunk.held_run_indices {
                let mut effect = display_list.chunk(held_chunk.chunk_index).runs()[run_index.0 as usize]
                    .context
                    .effect;
                while !effect.is_none()
                    && effect != effect_composited_around_band
                    && !effect_was_visited[effect.0 as usize]
                {
                    effect_was_visited[effect.0 as usize] = true;
                    let node = &tree.effect_nodes[effect.0 as usize];
                    if matches!(&node.data, EffectNodeData::Effects(effects) if effects.backdrop_filter.is_some()) {
                        backdrop_reads.push(
                            self.backdrop_read_in_layer(effect, first_held_position_of_run(held_chunk, run_index)),
                        );
                    }
                    effect = node.parent;
                }
            }
        }
        for backdrop_read in &backdrop_reads {
            let position = backdrop_read.first_record_position;
            band_binning_inputs_hasher.write_chunk_name(chunk_names[position.chunk_index.0 as usize]);
            band_binning_inputs_hasher.write_u32(position.offset);
            band_binning_inputs_hasher.write_reach(backdrop_read.region_reach);
            band_binning_inputs_hasher.write_reach(backdrop_read.backdrop_read_reach);
            band_binning_inputs_hasher.write_i32(backdrop_read.read_reach_in_device_pixels.unwrap_or(-1));
            band_binning_inputs_hasher.write_i32(backdrop_read.spread_past_region_in_device_pixels);
        }

        // A mask or a depth sort reads every run of its run group, so those runs replay whole wherever any of them
        // shows.
        let run_groups = &list.run_groups;
        let mut reach_and_first_position_of_run_groups_replayed_whole: BTreeMap<
            RunGroupIndex,
            (Reach<IntRect>, ListPosition),
        > = BTreeMap::new();
        for held_chunk in &held_chunks {
            for &run_index in &held_chunk.held_run_indices {
                let run_group_index = run_groups
                    .group_of_run(display_list.first_run_of_chunk(held_chunk.chunk_index) + run_index.0 as usize);
                if !run_groups.group_is_under_mask(run_group_index)
                    && !run_groups.group_is_sorted_by_depth(run_group_index)
                {
                    continue;
                }
                let ranges_of_run = &held_chunk.placement.ranges_of_each_run[run_index.0 as usize];
                let whole_run_reach =
                    held_chunk.placement.record_ranges[ranges_of_run.whole_run_range_index.0 as usize].reach;
                let position = first_held_position_of_run(held_chunk, run_index);
                reach_and_first_position_of_run_groups_replayed_whole
                    .entry(run_group_index)
                    .and_modify(|(united_reach, first_position)| {
                        *united_reach = united_reach.united(whole_run_reach);
                        *first_position = (*first_position).min(position);
                    })
                    .or_insert((whole_run_reach, position));
            }
        }
        let mut runs_replayed_whole = HashMap::new();
        if !reach_and_first_position_of_run_groups_replayed_whole.is_empty() {
            for held_chunk in &held_chunks {
                for &run_index in &held_chunk.held_run_indices {
                    let run_group_index = run_groups
                        .group_of_run(display_list.first_run_of_chunk(held_chunk.chunk_index) + run_index.0 as usize);
                    if let Some(&(run_group_reach, first_position)) =
                        reach_and_first_position_of_run_groups_replayed_whole.get(&run_group_index)
                    {
                        runs_replayed_whole.insert(
                            (held_chunk.chunk_index, run_index),
                            RunReplayedWholeWhereverRunGroupShows {
                                reach: reach_extended_through_backdrop_reads(
                                    run_group_reach,
                                    first_position,
                                    &backdrop_reads,
                                ),
                                mask_declarations_replayed_with_run: Vec::new(),
                            },
                        );
                    }
                }
            }
        }
        let mut bound_band = BandBoundToListAndGeometry {
            chunk_names,
            held_chunks,
            held_chunk_index_by_chunk_index,
            runs_replayed_whole,
            backdrop_reads,
            items_replayed_in_every_tile: None,
            binning_inputs_hash_beyond_held_chunks: 0,
        };

        if !list.mask_effect_and_position_of_declarations.is_empty() && !bound_band.runs_replayed_whole.is_empty() {
            let mut mask_declarations_of_runs = Vec::new();
            for &(chunk_index, run_index) in bound_band.runs_replayed_whole.keys() {
                let whole_run_item = TileReplayItem::PlacedRecordRange {
                    chunk_index,
                    range_index: bound_band.held_chunk(chunk_index).placement.ranges_of_each_run[run_index.0 as usize]
                        .whole_run_range_index,
                };
                let mut mask_declarations = Vec::new();
                let mut effect = display_list.chunk(chunk_index).runs()[run_index.0 as usize]
                    .context
                    .effect;
                while !effect.is_none() {
                    let node = &tree.effect_nodes[effect.0 as usize];
                    if matches!(node.data, EffectNodeData::Mask(_)) {
                        for &(mask_effect, declaration_position) in &list.mask_effect_and_position_of_declarations {
                            if mask_effect != effect {
                                continue;
                            }
                            let table = display_list.chunk(declaration_position.chunk_index).record_table();
                            let record_index = table
                                .record_indices_between_offsets(
                                    declaration_position.offset,
                                    declaration_position.offset + 1,
                                )
                                .start;
                            let mask_declaration =
                                self.item_replaying_record(&bound_band, declaration_position.chunk_index, record_index);
                            if mask_declaration != whole_run_item && !mask_declarations.contains(&mask_declaration) {
                                mask_declarations.push(mask_declaration);
                            }
                        }
                    }
                    effect = node.parent;
                }
                mask_declarations_of_runs.push(((chunk_index, run_index), mask_declarations));
            }
            mask_declarations_of_runs.sort_unstable_by_key(|(run, _)| *run);
            for (run, mask_declarations) in mask_declarations_of_runs {
                for &mask_declaration in &mask_declarations {
                    band_binning_inputs_hasher
                        .write_chunk_name(bound_band.chunk_names[mask_declaration.chunk_index().0 as usize]);
                    band_binning_inputs_hasher.write_content_hash(self.content_hash_of_replay_item(
                        &bound_band,
                        mask_declaration,
                        false,
                        None,
                    ));
                }
                bound_band
                    .runs_replayed_whole
                    .get_mut(&run)
                    .expect("mask declarations are found for runs replayed whole")
                    .mask_declarations_replayed_with_run = mask_declarations;
            }
        }

        for held_chunk_index in 0..bound_band.held_chunks.len() {
            let held_chunk = &bound_band.held_chunks[held_chunk_index];
            let mut reach_of_runs_replayed_whole = Reach::Nothing;
            let mut reach_of_runs_binned_range_by_range = Reach::Nothing;
            let mut chunk_binning_inputs_hasher = ContentHasher::new();
            chunk_binning_inputs_hasher.write_chunk_name(bound_band.chunk_names[held_chunk.chunk_index.0 as usize]);
            chunk_binning_inputs_hasher.write_content_hash(held_chunk.placement.placement_inputs_hash);
            for &(begin, end) in &held_chunk.held_offset_ranges_in_paint_order {
                chunk_binning_inputs_hasher.write_u32(begin);
                chunk_binning_inputs_hasher.write_u32(end);
            }
            for &run_index in &held_chunk.held_run_indices {
                match bound_band.runs_replayed_whole.get(&(held_chunk.chunk_index, run_index)) {
                    Some(run_replayed_whole) => {
                        reach_of_runs_replayed_whole = reach_of_runs_replayed_whole.united(run_replayed_whole.reach);
                        chunk_binning_inputs_hasher.write_u32(run_index.0);
                        chunk_binning_inputs_hasher.write_reach(run_replayed_whole.reach);
                    }
                    None => {
                        reach_of_runs_binned_range_by_range = reach_of_runs_binned_range_by_range.united(
                            held_chunk.placement.ranges_of_each_run[run_index.0 as usize].reach_of_binned_ranges,
                        );
                    }
                }
            }
            // A backdrop filter can extend content painted before it past the cells that content reaches.
            let uses_listed_reached_cells = bound_band.backdrop_reads.is_empty();
            let reach_past_listed_cells = if uses_listed_reached_cells {
                reach_of_runs_replayed_whole.united(held_chunk.placement.reach_past_listed_cells)
            } else {
                reach_of_runs_replayed_whole.united(reach_extended_through_backdrop_reads(
                    reach_of_runs_binned_range_by_range,
                    ListPosition::new(
                        held_chunk.chunk_index,
                        held_chunk
                            .held_offset_ranges_in_paint_order
                            .first()
                            .map_or(0, |held_offset_range| held_offset_range.0),
                    ),
                    &bound_band.backdrop_reads,
                ))
            };
            let chunk_binning_inputs_hash = chunk_binning_inputs_hasher.finish();
            let held_chunk = &mut bound_band.held_chunks[held_chunk_index];
            held_chunk.reach_of_held_records = reach_past_listed_cells.united(reach_of_runs_binned_range_by_range);
            held_chunk.uses_listed_reached_cells = uses_listed_reached_cells;
            held_chunk.reach_past_listed_cells = reach_past_listed_cells;
            held_chunk.binning_inputs_hash = chunk_binning_inputs_hash;
        }
        bound_band.binning_inputs_hash_beyond_held_chunks = band_binning_inputs_hasher.finish();
        bound_band
    }

    fn for_each_held_item(
        bound_band: &BandBoundToListAndGeometry,
        held_chunk: &ChunkHeldByBand,
        mut visit: impl FnMut(ListPosition, TileReplayItem, Reach<IntRect>, &[TileReplayItem]),
    ) {
        for &run_index in &held_chunk.held_run_indices {
            let ranges_of_run = &held_chunk.placement.ranges_of_each_run[run_index.0 as usize];
            if let Some(run_replayed_whole) = bound_band.runs_replayed_whole.get(&(held_chunk.chunk_index, run_index)) {
                let whole_run_range =
                    &held_chunk.placement.record_ranges[ranges_of_run.whole_run_range_index.0 as usize];
                visit(
                    ListPosition::new(held_chunk.chunk_index, whole_run_range.begin_offset),
                    TileReplayItem::PlacedRecordRange {
                        chunk_index: held_chunk.chunk_index,
                        range_index: ranges_of_run.whole_run_range_index,
                    },
                    run_replayed_whole.reach,
                    &run_replayed_whole.mask_declarations_replayed_with_run,
                );
                continue;
            }
            let (first_binned_range_index, end_binned_range_index) = ranges_of_run.first_and_end_binned_range_index;
            for range_index in (first_binned_range_index.0..end_binned_range_index.0).map(PlacedRecordRangeIndex) {
                let range = &held_chunk.placement.record_ranges[range_index.0 as usize];
                let range_is_held = held_chunk
                    .held_offset_ranges_in_paint_order
                    .iter()
                    .any(|&(begin, end)| begin <= range.begin_offset && range.begin_offset < end);
                if !range_is_held {
                    continue;
                }
                let paint_position = ListPosition::new(held_chunk.chunk_index, range.begin_offset);
                visit(
                    paint_position,
                    TileReplayItem::PlacedRecordRange {
                        chunk_index: held_chunk.chunk_index,
                        range_index,
                    },
                    reach_extended_through_backdrop_reads(range.reach, paint_position, &bound_band.backdrop_reads),
                    &[],
                );
            }
        }
    }

    /// Binds the tiles of `band_index` to the list and the geometry of the frame and to the band in `stack`. A tile
    /// keyed before keeps its bin and its content hash where the chunks that can reach it and the barriers that cut it
    /// are as they were, in the same order, whatever barriers they are, and is forgotten otherwise.
    pub fn bind_band(&self, band_tile_keys: &mut BandTileKeys, stack: &LayerStack, band_index: BandIndex) {
        let bound_band = self.bind_band_to_list_and_geometry(stack, band_index);
        band_tile_keys.replays_whole_list = stack.replays_whole_list() && self.list.tree_sorts_planes_by_depth;
        let Some(previous_bound_band) = band_tile_keys.bound_band.replace(bound_band) else {
            band_tile_keys.cells.clear();
            return;
        };
        let bound_band = band_tile_keys.bound_band.as_ref().expect("the band was just bound");
        if band_tile_keys.cells.is_empty() {
            return;
        }
        band_tile_keys.tiles_reading_frame_state_need_rehashing = true;
        if bound_band.binning_inputs_hash_beyond_held_chunks
            != previous_bound_band.binning_inputs_hash_beyond_held_chunks
            || bound_band.items_replayed_in_every_tile.is_some()
        {
            band_tile_keys.cells.clear();
            return;
        }
        let band = &stack.bands[band_index.0];
        let held_chunks_reaching_cells =
            bound_band.held_chunks_reaching_cells_in_paint_order(band_tile_keys.cells.keys().copied());
        let chunk_index_by_name: HashMap<ChunkName, ChunkIndexInList> = bound_band
            .chunk_names
            .iter()
            .enumerate()
            .map(|(chunk_index, name)| (*name, ChunkIndexInList(chunk_index as u32)))
            .collect();
        band_tile_keys.cells.retain(|coord, cell| {
            let cell_rect = coord.cell_rect();
            let binning_inputs_hash =
                bound_band.binning_inputs_hash_of_cell(band, cell_rect, &held_chunks_reaching_cells[coord]);
            if binning_inputs_hash != cell.binning_inputs_hash {
                return false;
            }
            for tile in &mut cell.tiles {
                let Some(layer_index) =
                    band.layer_above_first_cuts_of_cell(cell_rect, tile.count_of_cell_cuts_below_layer)
                else {
                    return false;
                };
                tile.layer_index = layer_index;
                tile.cuts = band.cuts_of_cell(cell_rect);
                for item in &mut tile.replay_items_in_paint_order {
                    let chunk_name = previous_bound_band.chunk_names[item.chunk_index().0 as usize];
                    let Some(&chunk_index) = chunk_index_by_name.get(&chunk_name) else {
                        return false;
                    };
                    *item = item.with_chunk_index(chunk_index);
                }
            }
            true
        });
    }

    /// Keys the tiles of the layers of `band_index` within `shown_rect` that are not keyed yet: each replay item of the
    /// band that reaches a tile goes into the layer of the band its paint position falls in there, and the base layer,
    /// if it is one of the band's, holds every tile. Tiles keyed before are keyed again only where the frame state
    /// their replay items read, the raster settings of their layer, or the edge of `frame` that their backdrop blurs
    /// mirror at changed, and those outside `kept_rect` are forgotten. `frame` is where the frame is in the band's
    /// space, where that is not the frame's own. The band is bound to the list and the geometry of the frame.
    #[allow(clippy::too_many_arguments)]
    pub fn key_shown_tiles(
        &self,
        band_tile_keys: &mut BandTileKeys,
        stack: &LayerStack,
        band_index: BandIndex,
        raster_settings_of_layer: impl Fn(LayerIndex) -> LayerRasterSettings,
        shown_rect: IntRect,
        kept_rect: IntRect,
        frame: Option<IntRect>,
    ) -> TileKeyingCounts {
        let list = self.list;
        let band = &stack.bands[band_index.0];
        // A band of a received stack can have too many layers to look through one by one for each tile.
        let mut raster_settings_hash_by_layer: HashMap<LayerIndex, ContentHash> = HashMap::new();
        let mut raster_settings_hash_of = |layer_index: LayerIndex| {
            *raster_settings_hash_by_layer
                .entry(layer_index)
                .or_insert_with(|| raster_settings_of_layer(layer_index).content_hash())
        };
        band_tile_keys
            .cells
            .retain(|coord, _| !coord.cell_rect().intersected(kept_rect).is_empty());
        let bound_band = band_tile_keys
            .bound_band
            .as_ref()
            .expect("a band is bound before its tiles are keyed");

        let mut counts = TileKeyingCounts::default();
        let frame_state_changed = band_tile_keys.frame_state_hash != list.frame_state_hash
            || std::mem::take(&mut band_tile_keys.tiles_reading_frame_state_need_rehashing);
        band_tile_keys.frame_state_hash = list.frame_state_hash;
        for (coord, cell) in &mut band_tile_keys.cells {
            let frame_edge_backdrop_blurs_mirror_at =
                bound_band.frame_edge_backdrop_blurs_of_cell_mirror_at(coord.cell_rect(), frame);
            for tile in &mut cell.tiles {
                let raster_settings_hash = raster_settings_hash_of(tile.layer_index);
                if (frame_state_changed && tile.reads_frame_state)
                    || tile.raster_settings_hash != raster_settings_hash
                    || tile.frame_edge_backdrop_blurs_mirror_at != frame_edge_backdrop_blurs_mirror_at
                {
                    tile.raster_settings_hash = raster_settings_hash;
                    tile.frame_edge_backdrop_blurs_mirror_at = frame_edge_backdrop_blurs_mirror_at;
                    tile.content_hash = self.content_hash_of_tile(bound_band, tile);
                    counts.tiles_keyed += 1;
                }
            }
        }

        let new_coords: Vec<TileCoord> = tile_coords_touching(shown_rect)
            .filter(|coord| !band_tile_keys.cells.contains_key(coord))
            .collect();
        if new_coords.is_empty() {
            return counts;
        }
        let new_cells_area = new_coords
            .iter()
            .fold(IntRect::default(), |cells, coord| cells.united(coord.cell_rect()));
        type ItemsByLayerInCell = Vec<(LayerIndex, Vec<(ListPosition, TileReplayItem, Reach<IntRect>)>)>;
        let mut bins: BTreeMap<TileCoord, ItemsByLayerInCell> =
            new_coords.iter().map(|coord| (*coord, Vec::new())).collect();
        let mut add_to_bins = |paint_position: ListPosition,
                               item: TileReplayItem,
                               reach: Reach<IntRect>,
                               mask_declarations_replayed_with_item: &[TileReplayItem]| {
            let reach_in_new_cells = reach.part_of(new_cells_area);
            if reach_in_new_cells.is_empty() {
                return;
            }
            for coord in tile_coords_touching(reach_in_new_cells) {
                let Some(bin) = bins.get_mut(&coord) else {
                    continue;
                };
                let layer_index = band.layer_of_record_in_cell(paint_position, coord.cell_rect());
                // A frame composites only the base layer and the layers the stack says can hold content. A stretch and
                // a placed record range both take their reach from `RasterSpaceGeometry::reach_of_content`, the
                // stretch for the ink of its runs, so a range stays within the reach of its stretch wherever the
                // space's extent lets it show. A stack from another process can say otherwise, which only leaves out
                // what the item draws there.
                if layer_index != stack.base_layer && !stack.layers[layer_index.0].may_hold_content {
                    continue;
                }
                let items_of_layer = match bin
                    .iter_mut()
                    .position(|(bin_layer_index, _)| *bin_layer_index == layer_index)
                {
                    Some(index) => &mut bin[index].1,
                    None => {
                        bin.push((layer_index, Vec::new()));
                        &mut bin.last_mut().expect("a bin was just pushed").1
                    }
                };
                items_of_layer.push((paint_position, item, reach));
                for &mask_declaration in mask_declarations_replayed_with_item {
                    let span = bound_band.span_of(mask_declaration);
                    items_of_layer.push((span.begin, mask_declaration, bound_band.reach_of_item(mask_declaration)));
                }
            }
        };
        match &bound_band.items_replayed_in_every_tile {
            Some(items_replayed_in_every_tile) => {
                for &(paint_position, item) in items_replayed_in_every_tile {
                    add_to_bins(paint_position, item, Reach::Anywhere, &[]);
                }
            }
            None => {
                for held_chunk in &bound_band.held_chunks {
                    if held_chunk.reach_of_held_records.part_of(new_cells_area).is_empty() {
                        continue;
                    }
                    Self::for_each_held_item(bound_band, held_chunk, &mut add_to_bins);
                }
            }
        }

        let held_chunks_reaching_cells =
            bound_band.held_chunks_reaching_cells_in_paint_order(new_coords.iter().copied());
        let base_layer = (stack.base_layer == band.layer_below_all_cuts).then_some(stack.base_layer);
        for (coord, mut bin) in bins {
            if let Some(base_layer) = base_layer
                && !bin.iter().any(|(layer_index, _)| *layer_index == base_layer)
            {
                bin.push((base_layer, Vec::new()));
            }
            let cell = coord.cell_rect();
            let raster_margin_in_device_pixels = bound_band
                .backdrop_reads
                .iter()
                .filter(|backdrop_read| backdrop_read.region_reach.touches(cell))
                .try_fold(0i32, |margin, backdrop_read| {
                    Some(
                        margin
                            .saturating_add(backdrop_read.read_reach_in_device_pixels?)
                            .saturating_add(backdrop_read.spread_past_region_in_device_pixels),
                    )
                });
            let cuts = band.cuts_of_cell(cell);
            let frame_edge_backdrop_blurs_mirror_at =
                bound_band.frame_edge_backdrop_blurs_of_cell_mirror_at(cell, frame);
            let mut tiles = Vec::with_capacity(bin.len());
            for (layer_index, mut items_of_layer) in bin {
                items_of_layer.sort_unstable_by_key(|(paint_position, item, _)| (*paint_position, *item));
                items_of_layer.dedup_by_key(|(_, item, _)| *item);
                let content_bounds =
                    items_of_layer
                        .iter()
                        .fold(IntRect::default(), |bounds, (_, _, reach)| match reach {
                            Reach::Within(rect) => bounds.united(rect.intersected(cell)),
                            Reach::Anywhere => cell,
                            Reach::Nothing => bounds,
                        });
                let replay_items_in_paint_order: Vec<TileReplayItem> =
                    items_of_layer.into_iter().map(|(_, item, _)| item).collect();
                let reads_frame_state = replay_items_in_paint_order
                    .iter()
                    .any(|&item| self.item_reads_frame_state(bound_band, item));
                let mut tile = KeyedTile {
                    layer_index,
                    coord,
                    content_hash: 0,
                    raster_margin_in_device_pixels,
                    cuts: cuts.clone(),
                    content_bounds,
                    count_of_cell_cuts_below_layer: band.count_of_cuts_of_cell_below_layer(cell, layer_index),
                    replay_items_in_paint_order,
                    raster_settings_hash: raster_settings_hash_of(layer_index),
                    reads_frame_state,
                    frame_edge_backdrop_blurs_mirror_at,
                };
                tile.content_hash = self.content_hash_of_tile(bound_band, &tile);
                tiles.push(tile);
                counts.tiles_keyed += 1;
            }
            let band_holds_something_in_cell = !tiles.is_empty();
            if band_holds_something_in_cell {
                counts.cells_binned += 1;
            }
            let binning_inputs_hash =
                bound_band.binning_inputs_hash_of_cell(band, cell, &held_chunks_reaching_cells[&coord]);
            band_tile_keys.cells.insert(
                coord,
                CellOfBandTiles {
                    binning_inputs_hash,
                    tiles,
                },
            );
        }
        counts
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{
        CanvasId, DeclareMaskContent, DisplayListCommand, FillRect, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::display_list::layers::LayerStack;
    use crate::display_list::layers::planning::{LayerStackInputs, plan_layer_stack};
    use crate::display_list::placement::{RasterSpaceIndex, RasterSpaces};
    use crate::visual_context::{EffectsData, SpatialData, TransformData, TransformDataRole};
    use libgfx_rust::CompositingAndBlendingOperator;

    const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);
    const RED: Color = Color::from_rgb(255, 0, 0);
    const BLUE: Color = Color::from_rgb(0, 0, 255);
    const GREEN: Color = Color::from_rgb(0, 128, 0);

    struct NoLiveContentChanges;

    impl LiveContentVersions for NoLiveContentChanges {
        fn canvas_content_generation(&self, _: CanvasId) -> u64 {
            0
        }
        fn video_frame_id(&self, _: VideoSinkHandle) -> Option<u64> {
            None
        }
        fn image_frame_pixel_generation(&self, _: ImageFrameResourceId) -> Option<u64> {
            None
        }
        fn nested_context_frame_generation(&self, _: CompositorContextId) -> Option<u64> {
            None
        }
    }

    fn tree_with_root_isolation_effect() -> VisualContextTree {
        let mut tree = VisualContextTree::create(TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
            bounds: FloatRect::default(),
        });
        let root = tree.append_effect(
            EffectNodeData::Effects(EffectsData {
                isolation: false,
                opacity: 1.0,
                blend_mode: CompositingAndBlendingOperator::Normal,
                filter: None,
                backdrop_filter: None,
            }),
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

    fn fill(rect: IntRect, color: Color) -> FillRect {
        FillRect {
            rect,
            color,
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    #[derive(Default)]
    struct TestList {
        builder: DisplayListBuilder,
    }

    impl TestList {
        fn with<C: DisplayListCommand>(mut self, command: C, context: ContextRef) -> Self {
            self.builder.append(&command, &[], context);
            self
        }
    }

    // The recorder declares a mask's content under the effects of what the mask holds, so no page has a declaration made
    // around the mask.
    #[test]
    fn a_mask_declared_around_its_mask_is_replayed_with_what_it_masks() {
        use crate::visual_context::{MaskData, MaskLayerOrigin};
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let masked = tree.append_effect_node_under(
            root,
            EffectNodeData::Mask(MaskData {
                rect: IntRect::new(0, 0, 100, 100),
                kind: libgfx_rust::MaskKind::Alpha,
                origin: MaskLayerOrigin::CssMaskLayers,
                masks_backdrop: false,
            }),
        );
        let mut list = TestList::default().with(fill(VIEWPORT, GREEN), root);
        let declaration_position = ListPosition::new(ChunkIndexInList(0), list.builder.byte_size() as u32);
        let declaration = list.builder.begin_group::<DeclareMaskContent>(Vec::new());
        list.builder.append(&fill(IntRect::new(0, 0, 50, 50), RED), &[], root);
        let content = list.builder.group_content_span(&declaration);
        list.builder.finish_group(
            declaration,
            &DeclareMaskContent {
                rect: IntRect::new(0, 0, 100, 100),
                effect: masked.effect,
                content,
            },
            root,
        );
        let masked_position = ListPosition::new(ChunkIndexInList(0), list.builder.byte_size() as u32);
        list = list.with(fill(IntRect::new(10, 10, 80, 80), BLUE), masked);

        let display_list = DisplayListStorage::of_builder(&list.builder);
        let stack = plan_layer_stack(&LayerStackInputs {
            list: &display_list,
            tree: &tree,
            scroll_offsets: &[],
            viewport_rect: VIEWPORT,
        })
        .stack;
        assert_eq!(stack.why_list_is_one_layer, None);
        let list_keying = TileKeyingOfList::new(
            &display_list,
            &tree,
            &stack,
            FrameStateReadByReplay {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContentChanges,
            },
        );
        let spaces = RasterSpaces::of_spaces(&tree, stack.spaces.clone());
        let geometry = RasterSpaceGeometry::new(
            &tree,
            &spaces,
            RasterSpaceIndex::VIEWPORT,
            &[],
            FloatMatrix4x4::identity(),
        );
        let raster_space = TileKeyingOfRasterSpace::new(&list_keying, &geometry);
        let band = BandIndex(
            stack
                .bands
                .iter()
                .position(|band| band.record_spans.iter().any(|span| span.contains(masked_position)))
                .unwrap(),
        );
        let mut band_tile_keys = BandTileKeys::default();
        raster_space.bind_band(&mut band_tile_keys, &stack, band);
        raster_space.key_shown_tiles(
            &mut band_tile_keys,
            &stack,
            band,
            raster_settings_for_tests,
            VIEWPORT,
            VIEWPORT,
            None,
        );
        let tile_with_masked_content = band_tile_keys
            .tiles_within(TileCoord::new(0, 0).cell_rect())
            .find(|tile| {
                band_tile_keys
                    .record_spans_of_tiles([*tile])
                    .iter()
                    .any(|span| span.contains(masked_position))
            })
            .unwrap();
        let spans = band_tile_keys.record_spans_of_tiles([tile_with_masked_content]);
        assert!(spans.iter().any(|span| span.contains(declaration_position)));
        assert!(spans.windows(2).all(|pair| pair[0].end <= pair[1].begin));
    }

    fn raster_settings_for_tests(_: LayerIndex) -> LayerRasterSettings {
        LayerRasterSettings {
            raster_space_identity: 0,
            clear_color: Color::TRANSPARENT,
            root_group_color: Color::TRANSPARENT,
            viewport_to_raster_transform: FloatMatrix4x4::identity(),
            allows_lcd_text: false,
        }
    }

    fn chunk_of(command: FillRect, context: ContextRef) -> Arc<ListChunk> {
        let mut builder = DisplayListBuilder::new();
        builder.append(&command, &[], context);
        Arc::new(ListChunk::copied(builder.bytes(), builder.command_runs(), 0))
    }

    fn stack_of(display_list: &DisplayListStorage, tree: &VisualContextTree) -> LayerStack {
        plan_layer_stack(&LayerStackInputs {
            list: display_list,
            tree,
            scroll_offsets: &[],
            viewport_rect: VIEWPORT,
        })
        .stack
    }

    fn viewport_tile_hashes_of_only_band_keyed_from_nothing(
        display_list: &DisplayListStorage,
        tree: &VisualContextTree,
        stack: &LayerStack,
    ) -> Vec<(TileCoord, LayerIndex, ContentHash)> {
        let list_keying = TileKeyingOfList::new(
            display_list,
            tree,
            stack,
            FrameStateReadByReplay {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContentChanges,
            },
        );
        let spaces = RasterSpaces::of_spaces(tree, stack.spaces.clone());
        let geometry = RasterSpaceGeometry::new(
            tree,
            &spaces,
            RasterSpaceIndex::VIEWPORT,
            &[],
            FloatMatrix4x4::identity(),
        );
        let raster_space = TileKeyingOfRasterSpace::new(&list_keying, &geometry);
        let mut band_tile_keys = BandTileKeys::default();
        raster_space.bind_band(&mut band_tile_keys, stack, BandIndex(0));
        raster_space.key_shown_tiles(
            &mut band_tile_keys,
            stack,
            BandIndex(0),
            raster_settings_for_tests,
            VIEWPORT,
            VIEWPORT,
            None,
        );
        viewport_tile_hashes(&band_tile_keys)
    }

    fn viewport_tile_hashes(band_tile_keys: &BandTileKeys) -> Vec<(TileCoord, LayerIndex, ContentHash)> {
        band_tile_keys
            .tiles_within(VIEWPORT)
            .map(|tile| (tile.coord, tile.layer_index, tile.content_hash))
            .collect()
    }

    fn chunks_placed_counts_and_viewport_tile_hashes_of_frames_keyed_in_turn(
        frames: &[(&DisplayListStorage, &VisualContextTree, &LayerStack)],
    ) -> Vec<(u64, TileKeyingCounts, Vec<(TileCoord, LayerIndex, ContentHash)>)> {
        chunks_placed_counts_and_tile_hashes_of_band_keyed_in_turn(frames, RasterSpaceIndex::VIEWPORT, BandIndex(0))
    }

    fn chunks_placed_counts_and_tile_hashes_of_band_keyed_in_turn(
        frames: &[(&DisplayListStorage, &VisualContextTree, &LayerStack)],
        raster_space_index: RasterSpaceIndex,
        band_index: BandIndex,
    ) -> Vec<(u64, TileKeyingCounts, Vec<(TileCoord, LayerIndex, ContentHash)>)> {
        let mut band_tile_keys = BandTileKeys::default();
        frames
            .iter()
            .map(|&(display_list, tree, stack)| {
                let list_keying = TileKeyingOfList::new(
                    display_list,
                    tree,
                    stack,
                    FrameStateReadByReplay {
                        scrollbars: &[],
                        time_ns: 0,
                        live_content: &NoLiveContentChanges,
                    },
                );
                let spaces = RasterSpaces::of_spaces(tree, stack.spaces.clone());
                let geometry =
                    RasterSpaceGeometry::new(tree, &spaces, raster_space_index, &[], FloatMatrix4x4::identity());
                let raster_space = TileKeyingOfRasterSpace::new(&list_keying, &geometry);
                raster_space.bind_band(&mut band_tile_keys, stack, band_index);
                let counts = raster_space.key_shown_tiles(
                    &mut band_tile_keys,
                    stack,
                    band_index,
                    raster_settings_for_tests,
                    VIEWPORT,
                    VIEWPORT,
                    None,
                );
                (
                    raster_space.chunks_placed(),
                    counts,
                    viewport_tile_hashes(&band_tile_keys),
                )
            })
            .collect()
    }

    fn sliding_box_tree_and_list(
        overshooting_easing: bool,
    ) -> (VisualContextTree, Arc<DisplayListStorage>, SpatialNodeIndex) {
        use crate::animation_timing::{AnimationEffectTiming, TimeValue};
        use crate::host::{FfiVisualAnimationTargetKind, FfiVisualAnimationTransformOperationKind};
        use crate::visual_animation::{
            VisualAnimation, VisualAnimationKeyframe, VisualAnimationTransformOperation, VisualAnimationValue,
        };
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
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
            VISUAL_VIEWPORT_NODE_INDEX,
        );
        let translate_x = |pixels: f32| VisualAnimationKeyframe {
            offset: f64::from(pixels) / 400.0,
            easing: crate::easing::Easing::default(),
            value: VisualAnimationValue::Transform(vec![
                VisualAnimationTransformOperation::new(FfiVisualAnimationTransformOperationKind::TranslateX, &[pixels])
                    .unwrap(),
            ]),
        };
        let easing = if overshooting_easing {
            crate::easing::Easing::CubicBezier {
                x1: 0.3,
                y1: 1.6,
                x2: 0.6,
                y2: 1.0,
            }
        } else {
            crate::easing::Easing::default()
        };
        tree.set_visual_animations(vec![VisualAnimation {
            target_kind: FfiVisualAnimationTargetKind::Transform,
            node_indices: vec![moving_node.0],
            timing: AnimationEffectTiming {
                iteration_duration: TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            easing,
            keyframes: vec![translate_x(0.0), translate_x(400.0)],
            ..VisualAnimation::default()
        }]);
        let moving = ContextRef {
            spatial: moving_node,
            ..root
        };
        let display_list = DisplayListStorage::of_chunks(vec![
            chunk_of(fill(VIEWPORT, GREEN), root),
            chunk_of(fill(IntRect::new(100, 300, 50, 50), BLUE), moving),
            chunk_of(fill(IntRect::new(600, 400, 20, 20), RED), root),
        ])
        .unwrap();
        (tree, display_list, moving_node)
    }

    // An easing that overshoots leaves nothing to bound where the box goes, so it stays in the tiles of the viewport.
    #[test]
    fn an_animation_sample_places_again_only_the_chunks_under_the_node_it_moves() {
        let (tree, display_list, _) = sliding_box_tree_and_list(true);
        let at_start = tree.with_visual_animation_samples(0);
        let halfway = tree.with_visual_animation_samples(500_000_000);
        let stack = stack_of(&display_list, &at_start);
        assert_eq!(stack.spaces.len(), 2);
        assert_eq!(stack.nodes.len(), 1);
        assert_eq!(stack_of(&display_list, &halfway).layers, stack.layers);

        let turns = chunks_placed_counts_and_viewport_tile_hashes_of_frames_keyed_in_turn(&[
            (&display_list, &at_start, &stack),
            (&display_list, &halfway, &stack),
        ]);
        assert_eq!(turns[0].0, 3);
        // Only the chunk under the moving node is placed again, and only the cells the box leaves and enters are binned
        // again.
        let (chunks_placed, counts, hashes) = &turns[1];
        assert_eq!(*chunks_placed, 1);
        assert_eq!(counts.cells_binned, 2);
        assert_eq!(
            hashes,
            &viewport_tile_hashes_of_only_band_keyed_from_nothing(&display_list, &halfway, &stack)
        );
    }

    #[test]
    fn an_animation_sample_leaves_the_tiles_of_the_space_of_the_node_it_moves_alike() {
        let (tree, display_list, moving_node) = sliding_box_tree_and_list(false);
        let at_start = tree.with_visual_animation_samples(0);
        let halfway = tree.with_visual_animation_samples(500_000_000);
        let stack = stack_of(&display_list, &at_start);
        let animated_space = RasterSpaceIndex(1);
        assert_eq!(stack.spaces[animated_space.0].anchor, moving_node);
        let band_of_animated_space = BandIndex(
            stack
                .bands
                .iter()
                .position(|band| stack.nodes[band.placement_node_index.0].raster_space_index == animated_space)
                .expect("the moving box has a band in its space"),
        );
        let turns = chunks_placed_counts_and_tile_hashes_of_band_keyed_in_turn(
            &[(&display_list, &at_start, &stack), (&display_list, &halfway, &stack)],
            animated_space,
            band_of_animated_space,
        );
        assert_eq!(turns[0].0, 1);
        let (chunks_placed, counts, hashes) = &turns[1];
        assert_eq!(*chunks_placed, 0);
        assert_eq!(counts.cells_binned, 0);
        assert_eq!(hashes, &turns[0].2);
    }

    #[test]
    fn a_tile_kept_through_an_animation_sample_reads_the_sampled_background_color_again() {
        use crate::animation_timing::{AnimationEffectTiming, TimeValue};
        use crate::host::FfiVisualAnimationTargetKind;
        use crate::visual_animation::{VisualAnimation, VisualAnimationKeyframe, VisualAnimationValue};
        let mut tree = tree_with_root_isolation_effect();
        let root = root_context(&tree);
        let animated_color = tree.append_effect(
            EffectNodeData::BackgroundColorAnimation,
            root.effect,
            VISUAL_VIEWPORT_NODE_INDEX,
            ClipNodeIndex::NONE,
        );
        let color_at = |offset: f64, color: Color| VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: VisualAnimationValue::BackgroundColor(color),
        };
        tree.set_visual_animations(vec![VisualAnimation {
            target_kind: FfiVisualAnimationTargetKind::BackgroundColor,
            node_indices: vec![animated_color.0],
            timing: AnimationEffectTiming {
                iteration_duration: TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            keyframes: vec![color_at(0.0, RED), color_at(1.0, BLUE)],
            ..VisualAnimation::default()
        }]);
        let display_list = DisplayListStorage::of_chunks(vec![
            chunk_of(fill(VIEWPORT, GREEN), root),
            chunk_of(
                FillRect {
                    background_color_animation_effect: animated_color,
                    ..fill(IntRect::new(600, 400, 20, 20), RED)
                },
                ContextRef {
                    effect: animated_color,
                    ..root
                },
            ),
        ])
        .unwrap();
        let at_start = tree.with_visual_animation_samples(0);
        let halfway = tree.with_visual_animation_samples(500_000_000);
        let stack = stack_of(&display_list, &at_start);

        let turns = chunks_placed_counts_and_viewport_tile_hashes_of_frames_keyed_in_turn(&[
            (&display_list, &at_start, &stack),
            (&display_list, &halfway, &stack),
        ]);
        // Nothing moved, so every cell keeps its bin, but the cell of the box is keyed for the color it shows now.
        let (chunks_placed, counts, hashes) = &turns[1];
        assert_eq!(*chunks_placed, 0);
        assert_eq!(counts.cells_binned, 0);
        assert_eq!(
            hashes,
            &viewport_tile_hashes_of_only_band_keyed_from_nothing(&display_list, &halfway, &stack)
        );
        let changed: Vec<TileCoord> = turns[0]
            .2
            .iter()
            .zip(hashes)
            .filter(|(old, new)| old != new)
            .map(|(old, _)| old.0)
            .collect();
        assert_eq!(changed, [TileCoord::new(2, 1)]);
    }
}
