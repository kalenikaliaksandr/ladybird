/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The keys of the tiles of the layers of one group, which alone decide whether a tile is rastered again.
//!
//! A tile's key digests what it holds: the records of the group that can show in it, as the geometry of the group's
//! space places their visual contexts, and what those records read outside the list. A chunk is placed once for each
//! placement of the contexts of its runs, and the placement is kept on the chunk for every list that names it. A tile
//! is binned once and keeps its bin, and with it its key, for as long as the chunks that can reach it and the barriers
//! that cut it stay as they are and in the same order, whatever happens elsewhere in the list.

use super::builder::{HEADER_SIZE, for_each_command, has_inline_clip_in_its_own_space, inline_clips_of, read_command};
use super::commands::{
    ClipNodeIndex, ContextRef, DeclareMaskContent, DisplayListCommandRun, DisplayListCommandType,
    DrawCompositedContext, EffectNodeIndex, FillRect, FillRectWithRoundedCorners, FillRoundedRectRing, PaintCaret,
    PaintScrollBar, SpatialNodeIndex,
};
use super::effect_clip_plan::EffectClipPlan;
use super::effect_geometry::{Extent, changed_extent_through_filters, nearest_spreading_effects};
use super::layers::{LayerGroup, LayerStack};
use super::nested_records::for_each_command_including_nested;
use super::placement::{RunGroups, SpaceGeometry, intersect_bounds, is_finite_rect};
use super::reach::Reach;
use super::replay::{ScrollbarShownByFrame, caret_is_visible_at_time};
use super::storage::{ChunkName, DisplayListStorage, ListChunk, ListPosition, ListSpan};
use super::summary::{LiveContent, live_content_of};
use super::tiles::{TileCoord, reaches_too_many_cells_to_follow_one_by_one, tile_coords_touching};
use crate::display_list::commands::{CanvasId, CompositorContextId, ImageFrameResourceId, VideoSinkHandle};
use crate::visual_context::{
    ClipData, ClipNodeData, EffectNodeData, VisualContextTree, WorldTransforms, device_offset_for_index,
};
use foldhash::fast::{FixedState, FoldHasher};
use libgfx_rust::{Color, CornerRadii, FloatMatrix4x4, FloatPoint, FloatRect, IntRect, enclosing_int_rect};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

pub type TileKey = u128;

const LOW_HALF_SEED: u64 = 0x243f_6a88_85a3_08d3;
const HIGH_HALF_SEED: u64 = 0x1319_8a2e_0370_7344;
const INFLUENCE_MARGIN_IN_DEVICE_PIXELS: i32 = 3;
const LARGEST_BACKDROP_READ_REACH_IN_DEVICE_PIXELS: f32 = 2048.0;
const INFLUENCE_COORDINATE_LIMIT: f32 = 16777216.0;

struct KeyHasher {
    low: FoldHasher<'static>,
    high: FoldHasher<'static>,
}

impl KeyHasher {
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

    fn write_key(&mut self, key: TileKey) {
        self.write_u64(key as u64);
        self.write_u64((key >> 64) as u64);
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

    fn finish(&self) -> TileKey {
        (u128::from(self.high.finish()) << 64) | u128::from(self.low.finish())
    }
}

const READS_CARET_BLINK_PHASE: u8 = 1 << 0;
const READS_SCROLL_BAR_STATE: u8 = 1 << 1;
const READS_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS: u8 = 1 << 2;
const READS_SPACES_OF_INLINE_CLIPS: u8 = 1 << 3;

/// A top-level record of a chunk, with what placing and keying it reads of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChunkRecord {
    pub(crate) offset: u32,
    pub(crate) end_offset: u32,
    pub(crate) run_index: u32,
    pub(crate) command_type: DisplayListCommandType,
    pub(crate) bounding_rect: IntRect,
    // Whether the record draws within its bounding rect, rather than being compositor metadata.
    pub(crate) has_ink: bool,
    draws_something: bool,
    reads_outside_the_list: u8,
    // Hashes the record's bytes alone, which hold no offset into the chunk, so a record hashes alike wherever it is.
    content_hash: TileKey,
}

/// The top-level records of a chunk, by run, with what keying a tile needs of each.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordTable {
    records: Vec<ChunkRecord>,
    first_record_of_each_run: Vec<u32>,
}

fn what_a_record_reads_outside_the_list(
    header: &super::commands::DisplayListCommandHeader,
    payload: &[u8],
    record_bytes: &[u8],
) -> u8 {
    let mut reads = match header.command_type {
        DisplayListCommandType::PaintCaret => READS_CARET_BLINK_PHASE,
        DisplayListCommandType::PaintScrollBar => READS_SCROLL_BAR_STATE,
        _ => 0,
    };
    if header.inline_clip_count != 0 && has_inline_clip_in_its_own_space(header, payload) {
        reads |= READS_SPACES_OF_INLINE_CLIPS;
    }
    for_each_command_including_nested(record_bytes, &mut |command_type, _, payload| {
        if live_content_of(command_type, payload).is_some()
            || !background_color_animation_effect_of(command_type, payload).is_none()
        {
            reads |= READS_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS;
        }
    });
    reads
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
        let mut first_record_of_each_run = Vec::with_capacity(command_runs.len() + 1);
        for (run_index, run) in command_runs.iter().enumerate() {
            first_record_of_each_run.push(records.len() as u32);
            let run_bytes = &records_of_chunk[run.offset as usize..(run.offset + run.size) as usize];
            for_each_command(run_bytes, |header, offset_in_run, payload| {
                let offset = run.offset + offset_in_run as u32;
                let end_offset = offset + (HEADER_SIZE + payload.len()) as u32;
                let record_bytes = &records_of_chunk[offset as usize..end_offset as usize];
                let mut hasher = KeyHasher::new();
                hasher.write_bytes(record_bytes);
                let has_ink = header.has_bounding_rect && !header.command_type.is_compositor_metadata();
                records.push(ChunkRecord {
                    offset,
                    end_offset,
                    run_index: run_index as u32,
                    command_type: header.command_type,
                    bounding_rect: header.bounding_rect,
                    has_ink,
                    draws_something: header.has_bounding_rect
                        && !header.command_type.is_compositor_metadata()
                        && !header.bounding_rect.is_empty(),
                    reads_outside_the_list: what_a_record_reads_outside_the_list(header, payload, record_bytes),
                    content_hash: hasher.finish(),
                });
            });
        }
        first_record_of_each_run.push(records.len() as u32);
        Self {
            records,
            first_record_of_each_run,
        }
    }

    pub(crate) fn records(&self) -> &[ChunkRecord] {
        &self.records
    }

    pub(crate) fn records_of_run(&self, run_index: usize) -> std::ops::Range<u32> {
        self.first_record_of_each_run[run_index]..self.first_record_of_each_run[run_index + 1]
    }

    // The records from `begin_offset` up to `end_offset`.
    pub(crate) fn records_within(&self, begin_offset: u32, end_offset: u32) -> std::ops::Range<u32> {
        let first = self.records.partition_point(|record| record.offset < begin_offset);
        let end = self.records.partition_point(|record| record.offset < end_offset);
        first as u32..end as u32
    }
}

pub trait LiveContentVersions {
    fn canvas_content_generation(&self, canvas: CanvasId) -> u64;
    fn video_frame_id(&self, video_sink: VideoSinkHandle) -> Option<u64>;
    fn image_frame_pixel_generation(&self, image_frame: ImageFrameResourceId) -> Option<u64>;
    fn nested_context_frame_generation(&self, nested_context: CompositorContextId) -> Option<u64>;
}

#[derive(Clone, Copy)]
pub struct ReplayReadsOutsideTheList<'a> {
    pub scrollbars: &'a [ScrollbarShownByFrame],
    pub time_ns: i64,
    pub live_content: &'a dyn LiveContentVersions,
}

#[derive(Clone, Copy)]
pub struct LayerConstants {
    pub raster_space_identity: u64,
    pub clear_color: Color,
    pub root_group_color: Color,
    // How the device pixels of the viewport map into those of the raster.
    pub raster_transform: FloatMatrix4x4,
}

impl LayerConstants {
    fn key(&self) -> TileKey {
        let mut hasher = KeyHasher::new();
        hasher.write_u64(self.raster_space_identity);
        hasher.write_u32(self.clear_color.0);
        hasher.write_u32(self.root_group_color.0);
        hasher.write_matrix(&self.raster_transform);
        hasher.finish()
    }
}

pub struct ListOfFrame<'a> {
    display_list: &'a DisplayListStorage,
    run_groups: &'a RunGroups,
    effect_clips: &'a EffectClipPlan,
    tree: &'a VisualContextTree,
    reads_outside_the_list: ReplayReadsOutsideTheList<'a>,
    nearest_spreading_effects: Vec<EffectNodeIndex>,
    replays_every_record_into_every_tile: bool,
    tree_has_sorting_contexts: bool,
    // Digests what the list's records can read outside it in the frame: whether each caret shows, the scroll bars the
    // frame shows, and the version of each canvas, video, image and nested context the list draws.
    live_stamp: TileKey,
    // The mask each mask declaration declares the content of, with where the declaration is.
    mask_declarations: Vec<(EffectNodeIndex, ListPosition)>,
    // Per effect node, whether the compositor composites the effect, which the tiles of its content leave out along
    // with the effects around it.
    effects_the_compositor_composites: Vec<bool>,
}

impl<'a> ListOfFrame<'a> {
    /// The list of a frame, of which `stack` was worked out.
    pub fn new(
        display_list: &'a DisplayListStorage,
        tree: &'a VisualContextTree,
        stack: &LayerStack,
        reads_outside_the_list: ReplayReadsOutsideTheList<'a>,
    ) -> Self {
        let layering_obstacles = stack.layering_obstacles;
        let run_groups = display_list.run_groups(tree);
        let mask_declarations = display_list
            .summary()
            .mask_declarations
            .iter()
            .map(|declaration| {
                let (_, payload) = display_list.record_at(declaration.position());
                (
                    read_command::<DeclareMaskContent>(payload).effect,
                    declaration.position(),
                )
            })
            .collect();
        Self {
            display_list,
            run_groups,
            effect_clips: display_list.effect_clip_plan(tree),
            tree,
            reads_outside_the_list,
            nearest_spreading_effects: nearest_spreading_effects(tree),
            replays_every_record_into_every_tile: layering_obstacles.every_tile_replays_every_record(),
            tree_has_sorting_contexts: layering_obstacles.has_3d_sorting_contexts,
            live_stamp: live_stamp_of(display_list, &reads_outside_the_list),
            mask_declarations,
            effects_the_compositor_composites: {
                let mut composited = vec![false; tree.effect_nodes.len()];
                for effect in stack.effects_the_compositor_composites() {
                    composited[effect.0 as usize] = true;
                }
                composited
            },
        }
    }
}

fn live_stamp_of(display_list: &DisplayListStorage, outside: &ReplayReadsOutsideTheList<'_>) -> TileKey {
    let summary = display_list.summary();
    let mut hasher = KeyHasher::new();
    for caret in &summary.carets {
        let (_, payload) = display_list.record_at(caret.position());
        hasher.write_bool(caret_is_visible_at_time(
            &read_command::<PaintCaret>(payload),
            outside.time_ns,
        ));
    }
    for scrollbar in outside.scrollbars {
        hasher.write_u32(scrollbar.scroll_node_index.0);
        hasher.write_bool(scrollbar.vertical);
        hasher.write_float_rect(scrollbar.gutter_rect);
        hasher.write_float_rect(scrollbar.thumb_rect);
        hasher.write_f32(scrollbar.expansion);
        hasher.write_f32(scrollbar.opacity);
    }
    for live_content_record in &summary.live_content_records {
        match live_content_record.content {
            LiveContent::Canvas(canvas) => hasher.write_u64(outside.live_content.canvas_content_generation(canvas)),
            LiveContent::VideoFrame(video_sink) => {
                hasher.write_u64(outside.live_content.video_frame_id(video_sink).unwrap_or(u64::MAX));
            }
            LiveContent::ImageFrame(image_frame) => hasher.write_u64(
                outside
                    .live_content
                    .image_frame_pixel_generation(image_frame)
                    .unwrap_or(u64::MAX),
            ),
            LiveContent::CompositedContext(nested_context) => hasher.write_u64(
                outside
                    .live_content
                    .nested_context_frame_generation(nested_context)
                    .unwrap_or(u64::MAX),
            ),
        }
    }
    hasher.finish()
}

fn influence_of_device_rect(rect: Option<FloatRect>) -> Reach<IntRect> {
    let Some(rect) = rect else {
        return Reach::Anywhere;
    };
    if !is_finite_rect(rect) {
        return Reach::Anywhere;
    }
    let rect = rect.intersected(FloatRect::new(
        -INFLUENCE_COORDINATE_LIMIT,
        -INFLUENCE_COORDINATE_LIMIT,
        2.0 * INFLUENCE_COORDINATE_LIMIT,
        2.0 * INFLUENCE_COORDINATE_LIMIT,
    ));
    if rect.is_empty() {
        return Reach::Nothing;
    }
    Reach::Within(enclosing_int_rect(rect).inflated_edges(
        INFLUENCE_MARGIN_IN_DEVICE_PIXELS,
        INFLUENCE_MARGIN_IN_DEVICE_PIXELS,
        INFLUENCE_MARGIN_IN_DEVICE_PIXELS,
        INFLUENCE_MARGIN_IN_DEVICE_PIXELS,
    ))
}

struct BackdropReadInLayer {
    first_record_position: ListPosition,
    region_influence: Reach<IntRect>,
    read_rect: Reach<IntRect>,
    read_reach_in_device_pixels: Option<i32>,
    spread_past_region_in_device_pixels: i32,
    // For a filter that mirrors its blur at the edge of its region, which then stops at the edge of the frame, where
    // that region is.
    region_its_blur_mirrors_at: Option<Reach<IntRect>>,
}

impl BackdropReadInLayer {
    fn blur_mirrors_at_the_edge_of(&self, frame: IntRect) -> bool {
        match self.region_its_blur_mirrors_at {
            None | Some(Reach::Nothing) => false,
            Some(Reach::Within(region)) => !frame.contains_rect(region),
            Some(Reach::Anywhere) => true,
        }
    }
}

impl GroupBinding {
    // Where the frame is, where a backdrop blur that shows in the tile at `cell` mirrors at its edge.
    fn frame_the_backdrop_blurs_of_cell_mirror_at(&self, cell: IntRect, frame: Option<IntRect>) -> Option<IntRect> {
        frame.filter(|frame| {
            self.backdrops
                .iter()
                .any(|backdrop| backdrop.region_influence.touches(cell) && backdrop.blur_mirrors_at_the_edge_of(*frame))
        })
    }
}

// Where content painted at `position` can show once the backdrop filters of the layer that read it after it are taken
// in: what a filter reads beyond the part of its region a tile holds has to be in the tile too.
fn influence_extended_through_backdrops(
    mut influence: Reach<IntRect>,
    position: ListPosition,
    backdrops: &[BackdropReadInLayer],
) -> Reach<IntRect> {
    if backdrops.is_empty() {
        return influence;
    }
    for _ in 0..=backdrops.len() {
        let mut influence_grew = false;
        for backdrop in backdrops {
            if position >= backdrop.first_record_position {
                continue;
            }
            let Reach::Within(rect) = influence else {
                continue;
            };
            if !backdrop.read_rect.touches(rect) {
                continue;
            }
            let reached = match (backdrop.region_influence, backdrop.read_reach_in_device_pixels) {
                (Reach::Within(region), Some(reach)) => {
                    let distance = reach + backdrop.spread_past_region_in_device_pixels;
                    Reach::Within(
                        rect.inflated_edges(distance, distance, distance, distance)
                            .intersected(region),
                    )
                }
                (region_influence, _) => region_influence,
            };
            let extended = influence.united(reached);
            if extended != influence {
                influence = extended;
                influence_grew = true;
            }
        }
        if !influence_grew {
            break;
        }
    }
    influence
}

/// A rect that the space maps to a rect of device pixels without rotating, skewing or mirroring it, with the radii of its
/// corners in device pixels, top left first. What it draws or clips within a part of the space depends only on where its
/// edges and corners are near that part.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RectInDevicePixels {
    rect: FloatRect,
    horizontal_and_vertical_corner_radii: [(f32, f32); 4],
}

impl RectInDevicePixels {
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
        let in_device_pixels = |radius: &libgfx_rust::CornerRadius| {
            (
                radius.horizontal_radius as f32 * affine.a(),
                radius.vertical_radius as f32 * affine.d(),
            )
        };
        Some(Self {
            rect,
            horizontal_and_vertical_corner_radii: [
                in_device_pixels(&corner_radii.top_left),
                in_device_pixels(&corner_radii.top_right),
                in_device_pixels(&corner_radii.bottom_right),
                in_device_pixels(&corner_radii.bottom_left),
            ],
        })
    }

    // The rect clipped to `area`, or the whole rect where a rounded corner of it reaches into `area`.
    fn write_part_within(&self, hasher: &mut KeyHasher, area: FloatRect) {
        let rect = self.rect;
        let [top_left, top_right, bottom_right, bottom_left] = self.horizontal_and_vertical_corner_radii;
        let corners = [
            FloatRect::new(rect.x, rect.y, top_left.0, top_left.1),
            FloatRect::new(rect.right() - top_right.0, rect.y, top_right.0, top_right.1),
            FloatRect::new(
                rect.right() - bottom_right.0,
                rect.bottom() - bottom_right.1,
                bottom_right.0,
                bottom_right.1,
            ),
            FloatRect::new(rect.x, rect.bottom() - bottom_left.1, bottom_left.0, bottom_left.1),
        ];
        for (horizontal, vertical) in self.horizontal_and_vertical_corner_radii {
            hasher.write_f32(horizontal);
            hasher.write_f32(vertical);
        }
        if corners
            .iter()
            .any(|corner| !corner.is_empty() && !corner.intersected(area).is_empty())
        {
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

/// A fill of one color over a rect in device pixels, which shows in a tile only as far as the part of the rect near it.
/// A ring fills its rect but for the rect inside its bands, and shows in a tile as far as the parts of both near it.
#[derive(Clone, Copy, Debug)]
struct FillOfARectInDevicePixels {
    rect: RectInDevicePixels,
    rect_inside_the_bands_of_a_ring: Option<RectInDevicePixels>,
    digest_of_the_fill_without_its_rect: TileKey,
}

#[derive(Clone, Copy, Debug)]
struct ChunkEntry {
    run: u32,
    first_record: u32,
    end_record: u32,
    begin_offset: u32,
    end_offset: u32,
    influence: Reach<IntRect>,
    // What the records hold, as the geometry of the space places them.
    static_digest: TileKey,
    reads_outside_the_list: bool,
    context: ContextRef,
    // Whether the records show in a tile only as far as they reach near it, as no filter spreads them, so that the rects
    // of their clips and fills are told by their parts near the tile.
    shows_in_a_tile_only_as_far_as_it_reaches_near_it: bool,
    // For an entry that is one fill of a rect in device pixels, that fill.
    fill_of_a_rect_in_device_pixels: Option<FillOfARectInDevicePixels>,
}

// The digests of clips and effects, with the rects of clips told by their parts within `area`, as a tile near the area
// shows them, worked out once per tile.
struct DigestsWithinAnArea {
    area: FloatRect,
    clips: HashMap<u32, TileKey>,
    effects: HashMap<u32, TileKey>,
}

impl DigestsWithinAnArea {
    fn new(area: FloatRect) -> Self {
        Self {
            area,
            clips: HashMap::new(),
            effects: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PlacedRun {
    // The entries the run's records are binned as one by one, and the union of where they can show.
    entries: (u32, u32),
    reach: Reach<IntRect>,
    // The entry that holds every record of the run, which is binned when the run is replayed whole.
    whole: u32,
}

/// What the records of a chunk hold in one raster space, as the geometry of the space places the visual contexts of
/// the chunk's runs it holds: the entries the records are binned into tiles as, where each can show, and the digest of
/// what each draws. A chunk keeps its latest placements for every list that names it.
pub struct PlacedChunk {
    key: TileKey,
    entries: Vec<ChunkEntry>,
    // Per run of the chunk, empty for the runs the space does not hold.
    runs: Vec<PlacedRun>,
    // The cells that the entries binned record by record reach, and where those that reach too many cells to list them
    // can show.
    reached_cells: Vec<TileCoord>,
    reach_past_listed_cells: Reach<IntRect>,
}

impl PlacedChunk {
    pub fn key(&self) -> TileKey {
        self.key
    }
}

/// What a tile replays of its list, named by where it is in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TileEntry {
    /// An entry of the placement of the chunk at `chunk`.
    Placed { chunk: u32, entry: u32 },
    /// A record that no entry of the group holds, replayed for the mask content it declares.
    Record {
        chunk: u32,
        record: u32,
        begin_offset: u32,
        end_offset: u32,
    },
    /// Records of the group that every tile replays, where none of them can be followed into tiles.
    Span {
        chunk: u32,
        begin_offset: u32,
        end_offset: u32,
    },
}

impl TileEntry {
    fn chunk(&self) -> u32 {
        match *self {
            TileEntry::Placed { chunk, .. } | TileEntry::Record { chunk, .. } | TileEntry::Span { chunk, .. } => chunk,
        }
    }

    fn in_chunk(self, chunk: u32) -> Self {
        match self {
            TileEntry::Placed { entry, .. } => TileEntry::Placed { chunk, entry },
            TileEntry::Record {
                record,
                begin_offset,
                end_offset,
                ..
            } => TileEntry::Record {
                chunk,
                record,
                begin_offset,
                end_offset,
            },
            TileEntry::Span {
                begin_offset,
                end_offset,
                ..
            } => TileEntry::Span {
                chunk,
                begin_offset,
                end_offset,
            },
        }
    }
}

/// A tile of a layer of a group, with the key of what it holds, the entries of the group replayed into it, and how far
/// past its cell content is read to raster it.
pub struct KeyedTile {
    pub layer: usize,
    pub coord: TileCoord,
    pub key: TileKey,
    pub raster_margin_in_device_pixels: Option<i32>,
    /// Which barriers cut the tile: tiles of a layer that are cut alike hold every entry that reaches them alike, so
    /// one replay can raster them together.
    pub cuts: u64,
    /// The part of the tile's cell that what it holds can reach.
    pub content_bounds: IntRect,
    // How many of the barriers that cut the cell are below the tile's layer, which names the layer in any stack whose
    // group cuts the cell alike.
    cuts_of_its_cell_below_it: usize,
    // In paint order.
    entries: Vec<TileEntry>,
    constants_key: TileKey,
    // Whether an entry of the tile reads something outside the list, such as the frame's time or a canvas.
    reads_outside_the_list: bool,
    // Where the frame was in the space as the tile was keyed, where the region of a backdrop blur that shows in the tile
    // reaches past it, as the blur then mirrors at the frame's edge, as it does in a raster of the frame.
    frame_its_backdrop_blurs_mirror_at: Option<IntRect>,
}

// The tiles of the layers of the group in one cell, binned together, and the digest of what binning them read: the
// chunks that can reach the cell and the barriers that cut it, in paint order.
struct BinnedCell {
    signature: TileKey,
    tiles: Vec<KeyedTile>,
}

// A chunk that the group holds records of, bound to one list and one geometry of the group's space.
struct BoundChunk {
    position: u32,
    placed: Arc<PlacedChunk>,
    // The offsets of the records of the chunk the group holds, in paint order.
    held: Vec<(u32, u32)>,
    held_runs: Vec<u32>,
    // Where what the group holds of the chunk can show: within its listed cells, where those are listed, and within
    // `reach_past_listed_cells`, and nowhere outside `reach` in any case.
    reach: Reach<IntRect>,
    lists_reached_cells: bool,
    reach_past_listed_cells: Reach<IntRect>,
    // What binning reads of the chunk.
    digest: TileKey,
}

// A run whose group effect is under a mask: it is replayed whole wherever any of its group shows, with the records
// that declare the content of the masks it is under.
struct WholeRun {
    influence: Reach<IntRect>,
    companions: Vec<TileEntry>,
}

// What the tiles of a group are binned from in a list and a geometry of the group's space.
struct GroupBinding {
    chunk_names: Arc<[ChunkName]>,
    chunks: Vec<BoundChunk>,
    bound_chunk_at: HashMap<u32, usize>,
    whole_runs: HashMap<(u32, u32), WholeRun>,
    backdrops: Vec<BackdropReadInLayer>,
    // The records of the group, which every tile replays, where nothing of the group can be followed into tiles.
    replayed_everywhere: Option<Vec<(ListPosition, TileEntry)>>,
    // What binning reads of the group beyond the chunks that reach a tile.
    digest: TileKey,
}

impl GroupBinding {
    fn placed_entry(&self, chunk: u32, entry: u32) -> &ChunkEntry {
        &self.chunks[self.bound_chunk_at[&chunk]].placed.entries[entry as usize]
    }

    // Where an entry can show, as binning places it.
    fn influence_of(&self, entry: TileEntry) -> Reach<IntRect> {
        let TileEntry::Placed { chunk, entry } = entry else {
            return Reach::Nothing;
        };
        let bound = &self.chunks[self.bound_chunk_at[&chunk]];
        let placed = &bound.placed.entries[entry as usize];
        if let Some(whole_run) = self.whole_runs.get(&(chunk, placed.run))
            && bound.placed.runs[placed.run as usize].whole == entry
        {
            return whole_run.influence;
        }
        influence_extended_through_backdrops(
            placed.influence,
            ListPosition::new(chunk, placed.begin_offset),
            &self.backdrops,
        )
    }

    fn span_of(&self, entry: TileEntry) -> ListSpan {
        let (chunk, begin_offset, end_offset) = match entry {
            TileEntry::Placed { chunk, entry } => {
                let placed = self.placed_entry(chunk, entry);
                (chunk, placed.begin_offset, placed.end_offset)
            }
            TileEntry::Record {
                chunk,
                begin_offset,
                end_offset,
                ..
            }
            | TileEntry::Span {
                chunk,
                begin_offset,
                end_offset,
            } => (chunk, begin_offset, end_offset),
        };
        ListSpan {
            begin: ListPosition::new(chunk, begin_offset),
            end: ListPosition::new(chunk, end_offset),
        }
    }

    // Per cell, the bound chunks that can reach it, in paint order.
    fn chunks_reaching(&self, coords: impl IntoIterator<Item = TileCoord>) -> BTreeMap<TileCoord, Vec<usize>> {
        let mut chunks_of_cells: BTreeMap<TileCoord, Vec<usize>> =
            coords.into_iter().map(|coord| (coord, Vec::new())).collect();
        let area = chunks_of_cells
            .keys()
            .fold(IntRect::default(), |area, coord| area.united(coord.cell_rect()));
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.reach.part_of(area).is_empty() {
                continue;
            }
            let mut note = |coord: TileCoord| {
                if let Some(chunks) = chunks_of_cells.get_mut(&coord)
                    && chunks.last() != Some(&index)
                {
                    chunks.push(index);
                }
            };
            if chunk.lists_reached_cells {
                for coord in &chunk.placed.reached_cells {
                    note(*coord);
                }
            }
            let reached_past_listed_cells = chunk.reach_past_listed_cells.part_of(area);
            if !reached_past_listed_cells.is_empty() {
                for coord in tile_coords_touching(reached_past_listed_cells) {
                    note(coord);
                }
            }
        }
        chunks_of_cells
    }

    // What binning the cell at `cell` reads: the chunks that can reach it and the barriers that cut it, in paint order.
    fn signature_of_cell(&self, layer_group: &LayerGroup, cell: IntRect, chunks: &[usize]) -> TileKey {
        let mut hasher = KeyHasher::new();
        let mut cuts = layer_group.cuts.iter().filter(|cut| cut.cuts_cell(cell)).peekable();
        // What decides the layer of an entry is which cuts are painted before it, so a cut is told by where it is among
        // the chunks that reach the cell, and by its offset where it is in one of those. Which barrier a cut is does not
        // matter, as the tiles of the cell name their layers by how many of the cuts of the cell are below them, and
        // neither do the cuts painted after every chunk that reaches the cell.
        let write_cut = |hasher: &mut KeyHasher, offset_in_chunk_reaching_cell: Option<u32>| {
            hasher.write_u32(2);
            hasher.write_u32(offset_in_chunk_reaching_cell.unwrap_or(u32::MAX));
        };
        for &index in chunks {
            let chunk = &self.chunks[index];
            while cuts.next_if(|cut| cut.position.chunk < chunk.position).is_some() {
                write_cut(&mut hasher, None);
            }
            hasher.write_u32(1);
            hasher.write_key(chunk.digest);
            while let Some(cut) = cuts.next_if(|cut| cut.position.chunk == chunk.position) {
                write_cut(&mut hasher, Some(cut.position.offset));
            }
        }
        // A cut painted after every chunk that reaches the cell has nothing above it there.
        hasher.finish()
    }
}

/// What the tiles of the layers of one group hold, kept from frame to frame, from list to list and from stack to stack:
/// the tiles keyed so far, binned against the list, the geometry and the group of the stack they are bound to. A tile is
/// binned again only where a chunk that can reach it or a barrier that cuts it changed, and keyed again only where
/// what it reads outside the list or the constants of its layer changed.
#[derive(Default)]
pub struct GroupTileKeys {
    cells: BTreeMap<TileCoord, BinnedCell>,
    binding: Option<GroupBinding>,
    live_stamp: TileKey,
    // What a record reads outside the list can depend on the tree and the geometry it is bound to, such as a sampled
    // background color or the space of an inline clip, so a tile kept through a binding reads it again.
    tiles_reading_outside_the_list_are_rebound: bool,
    pub replays_whole_list: bool,
}

/// What keying the shown tiles of a group did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileKeying {
    pub tiles_keyed: usize,
    /// The cells binned anew where the group holds something.
    pub cells_binned: usize,
}

impl GroupTileKeys {
    pub fn tile(&self, coord: TileCoord, layer: usize) -> Option<&KeyedTile> {
        self.cells.get(&coord)?.tiles.iter().find(|tile| tile.layer == layer)
    }

    /// The keyed tiles within `rect`, by row and then by layer.
    pub fn tiles_within(&self, rect: IntRect) -> impl Iterator<Item = &KeyedTile> {
        tile_coords_touching(rect).flat_map(|coord| self.cells.get(&coord).into_iter().flat_map(|cell| &cell.tiles))
    }

    pub fn record_spans_of_tiles<'a>(&self, tiles: impl IntoIterator<Item = &'a KeyedTile>) -> Vec<ListSpan> {
        let binding = self.binding.as_ref().expect("keyed tiles are bound to a list");
        let mut spans: Vec<ListSpan> = tiles
            .into_iter()
            .flat_map(|tile| tile.entries.iter().map(|entry| binding.span_of(*entry)))
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

pub struct RasterSpaceOfFrame<'a> {
    list: &'a ListOfFrame<'a>,
    geometry: &'a SpaceGeometry,
    spatial_digests: RefCell<Vec<Option<TileKey>>>,
    clip_digests: RefCell<Vec<Option<TileKey>>>,
    effect_digests: RefCell<Vec<Option<TileKey>>>,
    chunks_placed: Cell<u64>,
}

/// Whether a backdrop filter reads nothing past each pixel, or only through blurs that mirror at the edge of what they
/// read, so that what it reads past the edge of the frame does not show.
pub(crate) fn backdrop_filter_reads_nothing_past_the_edge_it_mirrors_at(filter: &[u8]) -> bool {
    let probe = FloatRect::new(0.0, 0.0, 1.0, 1.0);
    let reads_past_each_pixel = crate::filter_bytes::changed_output_bounds(filter, probe) != Some(probe);
    !reads_past_each_pixel || libgfx_rust::filter::Filter::serialized_has_mirroring_blur(filter)
}

pub(crate) fn backdrop_filter_read_reach_in_device_pixels(
    filter: &[u8],
    effect_to_device: &FloatMatrix4x4,
) -> Option<i32> {
    if !effect_to_device.is_2d_affine() {
        return None;
    }
    let probe = FloatRect::new(0.0, 0.0, 1.0, 1.0);
    let reached = crate::filter_bytes::changed_output_bounds(filter, probe)?;
    let reach_in_effect_space = [
        probe.x - reached.x,
        reached.right() - probe.right(),
        probe.y - reached.y,
        reached.bottom() - probe.bottom(),
    ]
    .into_iter()
    .fold(0.0f32, f32::max);
    let affine = effect_to_device.extract_2d_affine();
    let largest_stretch = affine.a().abs() + affine.b().abs() + affine.c().abs() + affine.d().abs();
    let reach = (reach_in_effect_space * largest_stretch).ceil() + INFLUENCE_MARGIN_IN_DEVICE_PIXELS as f32;
    (reach.is_finite() && reach <= LARGEST_BACKDROP_READ_REACH_IN_DEVICE_PIXELS).then_some(reach as i32)
}

impl<'a> RasterSpaceOfFrame<'a> {
    pub fn new(list: &'a ListOfFrame<'a>, geometry: &'a SpaceGeometry) -> Self {
        let tree = list.tree;
        Self {
            list,
            geometry,
            spatial_digests: RefCell::new(vec![None; tree.spatial_nodes.len()]),
            clip_digests: RefCell::new(vec![None; tree.clip_nodes.len()]),
            effect_digests: RefCell::new(vec![None; tree.effect_nodes.len()]),
            chunks_placed: Cell::new(0),
        }
    }

    /// How many chunks were placed in the space since it was made, rather than found placed alike before.
    pub fn chunks_placed(&self) -> u64 {
        self.chunks_placed.get()
    }

    fn filter_output_clip_bounds(&self, context: ContextRef) -> Option<FloatRect> {
        let clip = self
            .list
            .effect_clips
            .clip_outside_every_filter(self.list.tree, context);
        if clip.is_none() {
            None
        } else {
            self.geometry.clip_bounds_of_clips_rastered_in_space[clip.0 as usize]
        }
    }

    fn influence_of_content_within(&self, rect: FloatRect, context: ContextRef) -> Reach<IntRect> {
        if rect.is_empty()
            || self.geometry.culling.context_culls_everything(context)
            || self.geometry.world.backface_culled[context.spatial.0 as usize]
        {
            return Reach::Nothing;
        }
        let list = self.list;
        let spread_by_filters =
            !context.effect.is_none() && !list.nearest_spreading_effects[context.effect.0 as usize].is_none();
        let (space, extent) = changed_extent_through_filters(
            list.tree,
            &list.nearest_spreading_effects,
            context.spatial,
            rect,
            context.effect,
            &self.geometry.world,
        );
        let content_maps_affinely = self.geometry.world.draw_matrix(space).is_2d_affine()
            && self.geometry.world.draw_matrix(context.spatial).is_2d_affine();
        let bounds = match extent {
            Extent::Nothing => return Reach::Nothing,
            Extent::Within(rect) if content_maps_affinely => {
                Some(self.geometry.world.map_rect_to_root(space, rect)).filter(|rect| is_finite_rect(*rect))
            }
            _ => None,
        };
        let limit = if spread_by_filters {
            self.filter_output_clip_bounds(context)
        } else if context.clip.is_none() {
            None
        } else {
            self.geometry.clip_bounds_of_clips_rastered_in_space[context.clip.0 as usize]
        };
        influence_of_device_rect(intersect_bounds(bounds, limit))
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
        let backdrop = effects
            .backdrop_filter
            .as_ref()
            .expect("only effects with a backdrop filter are read as backdrops");
        let region = backdrop.region.to_float();
        let backdrop_context = ContextRef {
            spatial: node.spatial,
            clip: self.list.effect_clips.output_clip(effect),
            effect,
        };
        let region_influence = self.influence_of_content_within(region, backdrop_context);
        let effect_to_device = self.geometry.world.draw_matrix(node.spatial);
        let read_reach = backdrop_filter_read_reach_in_device_pixels(&backdrop.filter, &effect_to_device);
        let region_in_device_pixels = read_reach
            .map(|_| self.geometry.world.map_rect_to_root(node.spatial, region))
            .filter(|rect| is_finite_rect(*rect))
            .map(enclosing_int_rect);
        let read_rect = match (region_in_device_pixels, read_reach) {
            (Some(region), Some(reach)) => Reach::Within(region.inflated_edges(reach, reach, reach, reach)),
            _ => Reach::Anywhere,
        };
        let spread_past_region = match (region_influence, region_in_device_pixels) {
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
        let region_its_blur_mirrors_at = libgfx_rust::filter::Filter::serialized_has_mirroring_blur(&backdrop.filter)
            .then(|| {
                Some(self.geometry.world.map_rect_to_root(node.spatial, region))
                    .filter(|rect| is_finite_rect(*rect))
                    .map_or(Reach::Anywhere, |rect| Reach::Within(enclosing_int_rect(rect)))
            });
        BackdropReadInLayer {
            first_record_position,
            region_influence,
            read_rect,
            read_reach_in_device_pixels: read_reach.filter(|_| region_in_device_pixels.is_some()),
            spread_past_region_in_device_pixels: spread_past_region,
            region_its_blur_mirrors_at,
        }
    }

    fn spatial_digest(&self, spatial: crate::display_list::commands::SpatialNodeIndex) -> TileKey {
        if let Some(digest) = self.spatial_digests.borrow()[spatial.0 as usize] {
            return digest;
        }
        let mut hasher = KeyHasher::new();
        hasher.write_matrix(&self.geometry.world.draw_matrix(spatial));
        hasher.write_bool(self.geometry.world.backface_culled[spatial.0 as usize]);
        let digest = hasher.finish();
        self.spatial_digests.borrow_mut()[spatial.0 as usize] = Some(digest);
        digest
    }

    fn clip_digest(&self, clip: ClipNodeIndex) -> TileKey {
        if clip.is_none() {
            return 0;
        }
        if let Some(digest) = self.clip_digests.borrow()[clip.0 as usize] {
            return digest;
        }
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        if self
            .geometry
            .clips_left_to_composite
            .get(clip.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            let digest = self.clip_digest(node.parent);
            self.clip_digests.borrow_mut()[clip.0 as usize] = Some(digest);
            return digest;
        }
        let mut hasher = KeyHasher::new();
        hasher.write_key(self.clip_digest(node.parent));
        self.write_clip_node(&mut hasher, clip);
        let digest = hasher.finish();
        self.clip_digests.borrow_mut()[clip.0 as usize] = Some(digest);
        digest
    }

    fn write_clip_node(&self, hasher: &mut KeyHasher, clip: ClipNodeIndex) {
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        hasher.write_key(self.spatial_digest(node.spatial));
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

    // As `clip_digest`, with the rect of each clip that the space maps to a rect of device pixels told by its part
    // within `within.area`, which is all of it that a tile near the area shows.
    fn clip_digest_within(&self, clip: ClipNodeIndex, within: &mut DigestsWithinAnArea) -> TileKey {
        if clip.is_none() {
            return 0;
        }
        if let Some(&digest) = within.clips.get(&clip.0) {
            return digest;
        }
        let node = &self.list.tree.clip_nodes[clip.0 as usize];
        let left_to_composite = self
            .geometry
            .clips_left_to_composite
            .get(clip.0 as usize)
            .copied()
            .unwrap_or(false);
        let parent_digest = self.clip_digest_within(node.parent, within);
        let digest = if left_to_composite {
            parent_digest
        } else {
            let mut hasher = KeyHasher::new();
            hasher.write_key(parent_digest);
            let rect_in_device_pixels = match &node.data {
                ClipNodeData::Rect(ClipData {
                    rect,
                    corner_radii,
                    mode,
                }) => RectInDevicePixels::of(&self.geometry.world, node.spatial, *rect, corner_radii)
                    .map(|rect| (rect, *mode)),
                _ => None,
            };
            match rect_in_device_pixels {
                Some((rect, mode)) => {
                    hasher.write_u32(4);
                    hasher.write_bool(node.clips_everything);
                    hasher.write_u32(mode as u32);
                    rect.write_part_within(&mut hasher, within.area);
                }
                None => self.write_clip_node(&mut hasher, clip),
            }
            hasher.finish()
        };
        within.clips.insert(clip.0, digest);
        digest
    }

    fn effect_digest(&self, effect: EffectNodeIndex) -> TileKey {
        if effect.is_none() {
            return 0;
        }
        if let Some(digest) = self.effect_digests.borrow()[effect.0 as usize] {
            return digest;
        }
        let node = &self.list.tree.effect_nodes[effect.0 as usize];
        let digest = self.digest_of_effect_node(
            effect,
            self.effect_digest(node.parent),
            self.clip_digest(self.list.effect_clips.output_clip(effect)),
        );
        self.effect_digests.borrow_mut()[effect.0 as usize] = Some(digest);
        digest
    }

    // As `effect_digest`, with the clips of the effects told as `clip_digest_within` tells them.
    fn effect_digest_within(&self, effect: EffectNodeIndex, within: &mut DigestsWithinAnArea) -> TileKey {
        if effect.is_none() {
            return 0;
        }
        if let Some(&digest) = within.effects.get(&effect.0) {
            return digest;
        }
        let node = &self.list.tree.effect_nodes[effect.0 as usize];
        let parent_digest = self.effect_digest_within(node.parent, within);
        let output_clip_digest = self.clip_digest_within(self.list.effect_clips.output_clip(effect), within);
        let digest = self.digest_of_effect_node(effect, parent_digest, output_clip_digest);
        within.effects.insert(effect.0, digest);
        digest
    }

    fn digest_of_effect_node(
        &self,
        effect: EffectNodeIndex,
        parent_digest: TileKey,
        output_clip_digest: TileKey,
    ) -> TileKey {
        let tree = self.list.tree;
        let node = &tree.effect_nodes[effect.0 as usize];
        let mut hasher = KeyHasher::new();
        hasher.write_key(parent_digest);
        hasher.write_key(self.spatial_digest(node.spatial));
        hasher.write_key(output_clip_digest);
        hasher.write_bool(tree.root_isolation_effect == Some(effect));
        // A tile replays content under an effect the compositor composites without the effect.
        hasher.write_bool(self.list.effects_the_compositor_composites[effect.0 as usize]);
        match &node.data {
            EffectNodeData::Effects(effects) => {
                hasher.write_u32(1);
                hasher.write_bool(effects.isolation);
                hasher.write_f32(effects.opacity);
                hasher.write_u32(effects.blend_mode as u32);
                match &effects.filter {
                    Some(filter) => hasher.write_bytes(filter),
                    None => hasher.write_u64(u64::MAX),
                }
                match &effects.backdrop_filter {
                    Some(backdrop) => {
                        hasher.write_bytes(&backdrop.filter);
                        hasher.write_int_rect(backdrop.region);
                        hasher.write_corner_radii(&backdrop.corner_radii);
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

    fn context_digest(&self, context: ContextRef) -> TileKey {
        let mut hasher = KeyHasher::new();
        hasher.write_key(self.spatial_digest(context.spatial));
        hasher.write_key(self.clip_digest(context.clip));
        hasher.write_key(self.effect_digest(context.effect));
        hasher.finish()
    }

    // What an entry holds as a tile near `within.area` shows it: its fill, or each of its records, under its context,
    // with the rects of its fill and its clips told by their parts within the area.
    fn digest_of_placed_entry_within(
        &self,
        chunk: &ListChunk,
        entry: &ChunkEntry,
        within: &mut DigestsWithinAnArea,
    ) -> TileKey {
        let mut hasher = KeyHasher::new();
        hasher.write_key(self.spatial_digest(entry.context.spatial));
        hasher.write_key(self.clip_digest_within(entry.context.clip, within));
        hasher.write_key(self.effect_digest_within(entry.context.effect, within));
        match &entry.fill_of_a_rect_in_device_pixels {
            Some(fill) => {
                hasher.write_key(fill.digest_of_the_fill_without_its_rect);
                fill.rect.write_part_within(&mut hasher, within.area);
                // The corners of the rect inside a ring's bands curve only within those of the ring's own rect, which
                // the part of that rect tells of.
                if let Some(rect_inside_the_bands) = &fill.rect_inside_the_bands_of_a_ring {
                    rect_inside_the_bands.write_part_within(&mut hasher, within.area);
                }
            }
            None => {
                for record in &chunk.record_table().records[entry.first_record as usize..entry.end_record as usize] {
                    hasher.write_key(record.content_hash);
                }
            }
        }
        hasher.finish()
    }

    fn write_what_a_record_reads_outside_the_list(
        &self,
        hasher: &mut KeyHasher,
        chunk: &ListChunk,
        record: &ChunkRecord,
    ) {
        let list = self.list;
        let outside = &list.reads_outside_the_list;
        let record_bytes = &chunk.records()[record.offset as usize..record.end_offset as usize];
        let (header, payload) = chunk.record_at(record.offset);
        if record.reads_outside_the_list & READS_CARET_BLINK_PHASE != 0 {
            hasher.write_bool(caret_is_visible_at_time(
                &read_command::<PaintCaret>(payload),
                outside.time_ns,
            ));
        }
        if record.reads_outside_the_list & READS_SCROLL_BAR_STATE != 0 {
            let command = read_command::<PaintScrollBar>(payload);
            let shown_by_frame = outside.scrollbars.iter().find(|scrollbar| {
                scrollbar.scroll_node_index == command.scroll_node_index && scrollbar.vertical == command.vertical
            });
            match shown_by_frame {
                Some(scrollbar) => {
                    hasher.write_u32(1);
                    hasher.write_float_rect(scrollbar.gutter_rect);
                    hasher.write_float_rect(scrollbar.thumb_rect);
                    hasher.write_f32(scrollbar.expansion);
                    hasher.write_f32(scrollbar.opacity);
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
        if record.reads_outside_the_list & READS_SPACES_OF_INLINE_CLIPS != 0 {
            for clip in inline_clips_of(&header, payload).filter(|clip| clip.has_own_space) {
                match self.geometry.world.draw_matrix_if_known(clip.space) {
                    Some(matrix) => hasher.write_matrix(&matrix),
                    None => hasher.write_u64(u64::MAX),
                }
            }
        }
        if record.reads_outside_the_list & READS_LIVE_CONTENT_OR_SAMPLED_BACKGROUND_COLORS == 0 {
            return;
        }
        let tree = list.tree;
        for_each_command_including_nested(record_bytes, &mut |command_type, _, payload| {
            match live_content_of(command_type, payload) {
                Some(LiveContent::Canvas(canvas)) => {
                    hasher.write_u32(1);
                    hasher.write_u64(outside.live_content.canvas_content_generation(canvas));
                }
                Some(LiveContent::VideoFrame(video_sink)) => {
                    hasher.write_u32(2);
                    hasher.write_u64(outside.live_content.video_frame_id(video_sink).unwrap_or(u64::MAX));
                }
                Some(LiveContent::ImageFrame(image_frame)) => {
                    hasher.write_u32(3);
                    hasher.write_u64(
                        outside
                            .live_content
                            .image_frame_pixel_generation(image_frame)
                            .unwrap_or(u64::MAX),
                    );
                }
                Some(LiveContent::CompositedContext(nested_context)) => {
                    hasher.write_u32(4);
                    hasher.write_u64(
                        outside
                            .live_content
                            .nested_context_frame_generation(nested_context)
                            .unwrap_or(u64::MAX),
                    );
                    let command = read_command::<DrawCompositedContext>(payload);
                    hasher.write_bool(tree.translations_snap_to_device_pixels(command.spatial_node_index));
                }
                None => {}
            }
            let background_color_animation_effect = background_color_animation_effect_of(command_type, payload);
            if !background_color_animation_effect.is_none() {
                hasher.write_u32(
                    tree.sampled_background_color(background_color_animation_effect)
                        .map_or(u32::MAX, |color| color.0),
                );
                hasher.write_bool(
                    tree.sampled_background_color(background_color_animation_effect)
                        .is_some(),
                );
            }
        });
    }

    // What the records of the chunk hold, as the space's geometry places them.
    fn static_digest_of_records(&self, chunk: &ListChunk, records: std::ops::Range<u32>) -> TileKey {
        let table = chunk.record_table();
        let mut hasher = KeyHasher::new();
        let mut run_of_previous_record = u32::MAX;
        for record in &table.records[records.start as usize..records.end as usize] {
            if record.run_index != run_of_previous_record {
                run_of_previous_record = record.run_index;
                hasher.write_key(self.context_digest(chunk.runs()[record.run_index as usize].context));
            }
            hasher.write_key(record.content_hash);
        }
        hasher.finish()
    }

    // What the records of the chunk read outside the list, or none where none of them does.
    fn live_digest_of_records(&self, chunk: &ListChunk, records: std::ops::Range<u32>) -> Option<TileKey> {
        let table = chunk.record_table();
        let records = &table.records[records.start as usize..records.end as usize];
        if records.iter().all(|record| record.reads_outside_the_list == 0) {
            return None;
        }
        let mut hasher = KeyHasher::new();
        for record in records.iter().filter(|record| record.reads_outside_the_list != 0) {
            self.write_what_a_record_reads_outside_the_list(&mut hasher, chunk, record);
        }
        Some(hasher.finish())
    }

    fn digest_of_records(&self, chunk: &ListChunk, records: std::ops::Range<u32>) -> TileKey {
        let mut hasher = KeyHasher::new();
        hasher.write_key(self.static_digest_of_records(chunk, records.clone()));
        if let Some(live_digest) = self.live_digest_of_records(chunk, records) {
            hasher.write_key(live_digest);
        }
        hasher.finish()
    }

    /// The digest of what compositing the layers of a node inside `effect` reads of the effect: its values, where it
    /// is, and the clips it is entered within, which changes wherever the composited effect draws differently.
    pub fn digest_of_composited_effect(&self, effect: EffectNodeIndex) -> TileKey {
        self.effect_digest(effect)
    }

    /// The digest of what the records within `span` draw in the frame.
    pub fn digest_of_records_within(&self, span: ListSpan) -> TileKey {
        let display_list = self.list.display_list;
        let mut hasher = KeyHasher::new();
        for (chunk_index, begin_offset, end_offset) in display_list.parts_of_span(span) {
            let chunk = display_list.chunk(chunk_index);
            let table = chunk.record_table();
            let records = table.records_within(begin_offset, end_offset);
            let mut first = records.start;
            while first < records.end {
                let run = table.records[first as usize].run_index;
                let end = table.records_of_run(run as usize).end.min(records.end);
                hasher.write_key(self.digest_of_records(chunk, first..end));
                first = end;
            }
        }
        hasher.finish()
    }

    fn entry_of_records(
        &self,
        chunk: &ListChunk,
        run: u32,
        records: std::ops::Range<u32>,
        influence: Reach<IntRect>,
    ) -> ChunkEntry {
        let table = chunk.record_table();
        let context = chunk.runs()[run as usize].context;
        let spread_by_filters =
            !context.effect.is_none() && !self.list.nearest_spreading_effects[context.effect.0 as usize].is_none();
        ChunkEntry {
            run,
            first_record: records.start,
            end_record: records.end,
            begin_offset: table.records[records.start as usize].offset,
            end_offset: table.records[records.end as usize - 1].end_offset,
            influence,
            static_digest: self.static_digest_of_records(chunk, records.clone()),
            reads_outside_the_list: table.records[records.start as usize..records.end as usize]
                .iter()
                .any(|record| record.reads_outside_the_list != 0),
            context,
            shows_in_a_tile_only_as_far_as_it_reaches_near_it: !spread_by_filters,
            fill_of_a_rect_in_device_pixels: (records.len() == 1)
                .then(|| self.fill_of_a_rect_in_device_pixels(chunk, &table.records[records.start as usize], context))
                .flatten(),
        }
    }

    // A record that fills a rect with one color, wherever its space maps the rect to one of device pixels.
    fn fill_of_a_rect_in_device_pixels(
        &self,
        chunk: &ListChunk,
        record: &ChunkRecord,
        context: ContextRef,
    ) -> Option<FillOfARectInDevicePixels> {
        let (header, payload) = chunk.record_at(record.offset);
        if header.inline_clip_count != 0 || header.has_inline_transform {
            return None;
        }
        let mut hasher = KeyHasher::new();
        hasher.write_u32(header.command_type as u32);
        let (rect, corner_radii, rect_inside_the_bands_of_a_ring) = match header.command_type {
            DisplayListCommandType::FillRect => {
                let fill = read_command::<FillRect>(payload);
                hasher.write_u32(fill.color.0);
                hasher.write_u32(fill.compositing_and_blending_operator as u32);
                hasher.write_u32(fill.background_color_animation_effect.0);
                (fill.rect, CornerRadii::default(), None)
            }
            DisplayListCommandType::FillRectWithRoundedCorners => {
                let fill = read_command::<FillRectWithRoundedCorners>(payload);
                hasher.write_u32(fill.color.0);
                hasher.write_u32(fill.background_color_animation_effect.0);
                (fill.rect, fill.corner_radii, None)
            }
            DisplayListCommandType::FillRoundedRectRing => {
                let ring = read_command::<FillRoundedRectRing>(payload);
                hasher.write_u32(ring.color.0);
                let top_width = ring.top_width.clamp(0, ring.rect.height.max(0));
                let bottom_width = ring.bottom_width.clamp(0, (ring.rect.height - top_width).max(0));
                let left_width = ring.left_width.clamp(0, ring.rect.width.max(0));
                let right_width = ring.right_width.clamp(0, (ring.rect.width - left_width).max(0));
                for width in [top_width, right_width, bottom_width, left_width] {
                    hasher.write_i32(width);
                }
                let rect_inside_the_bands = IntRect::new(
                    ring.rect.x + left_width,
                    ring.rect.y + top_width,
                    ring.rect.width - left_width - right_width,
                    ring.rect.height - top_width - bottom_width,
                );
                (ring.rect, ring.corner_radii, Some(rect_inside_the_bands))
            }
            _ => return None,
        };
        let rect_inside_the_bands_of_a_ring = match rect_inside_the_bands_of_a_ring {
            Some(rect_inside_the_bands) => Some(RectInDevicePixels::of(
                &self.geometry.world,
                context.spatial,
                rect_inside_the_bands.to_float(),
                &CornerRadii::default(),
            )?),
            None => None,
        };
        Some(FillOfARectInDevicePixels {
            rect: RectInDevicePixels::of(&self.geometry.world, context.spatial, rect.to_float(), &corner_radii)?,
            rect_inside_the_bands_of_a_ring,
            digest_of_the_fill_without_its_rect: hasher.finish(),
        })
    }

    fn place_chunk(&self, chunk: &ListChunk, runs: &[u32], key: TileKey) -> PlacedChunk {
        let table = chunk.record_table();
        let mut entries = Vec::new();
        let mut placed_runs = vec![PlacedRun::default(); chunk.runs().len()];
        for &run_index in runs {
            let run = &chunk.runs()[run_index as usize];
            let records = table.records_of_run(run_index as usize);
            let run_influence = self.influence_of_content_within(run.ink_bounds.to_float(), run.context);
            let first_entry = entries.len() as u32;
            let mut reach = Reach::Nothing;
            if run_influence != Reach::Nothing {
                if !self.geometry.world.draw_matrix(run.context.spatial).is_2d_affine() {
                    entries.push(self.entry_of_records(chunk, run_index, records.clone(), run_influence));
                    reach = run_influence;
                } else {
                    for record_index in records.clone() {
                        let record = &table.records[record_index as usize];
                        if !record.draws_something {
                            continue;
                        }
                        let influence = self.influence_of_content_within(record.bounding_rect.to_float(), run.context);
                        if influence == Reach::Nothing {
                            continue;
                        }
                        entries.push(self.entry_of_records(
                            chunk,
                            run_index,
                            record_index..record_index + 1,
                            influence,
                        ));
                        reach = reach.united(influence);
                    }
                }
            }
            let end_entry = entries.len() as u32;
            entries.push(self.entry_of_records(chunk, run_index, records, run_influence));
            placed_runs[run_index as usize] = PlacedRun {
                entries: (first_entry, end_entry),
                reach,
                whole: end_entry,
            };
        }
        let mut reached_cells = Vec::new();
        let mut reach_past_listed_cells = Reach::Nothing;
        for placed_run in &placed_runs {
            for entry in &entries[placed_run.entries.0 as usize..placed_run.entries.1 as usize] {
                match entry.influence {
                    // Beyond that, the chunk is taken to reach every cell its entry can.
                    Reach::Within(rect) if !reaches_too_many_cells_to_follow_one_by_one(rect) => {
                        reached_cells.extend(tile_coords_touching(rect));
                    }
                    influence => reach_past_listed_cells = reach_past_listed_cells.united(influence),
                }
            }
        }
        reached_cells.sort_unstable();
        reached_cells.dedup();
        PlacedChunk {
            key,
            entries,
            runs: placed_runs,
            reached_cells,
            reach_past_listed_cells,
        }
    }

    // The placement of the given runs of the chunk at `chunk_index` in the space, kept on the chunk under the
    // placement of the runs' contexts.
    fn placed_chunk(&self, chunk_index: u32, runs: &[u32]) -> Arc<PlacedChunk> {
        let chunk = self.list.display_list.chunk(chunk_index);
        let mut hasher = KeyHasher::new();
        for &run in runs {
            hasher.write_u32(run);
            hasher.write_key(self.context_digest(chunk.runs()[run as usize].context));
        }
        let key = hasher.finish();
        let (placed, newly_placed) = chunk.placement(key, || self.place_chunk(chunk, runs, key));
        if newly_placed {
            self.chunks_placed.set(self.chunks_placed.get() + 1);
        }
        placed
    }

    // What an entry holds and reads, with what it holds told as a tile near `within` shows it where that is given and
    // the entry shows only as far as it reaches near the tile.
    fn digest_of_tile_entry(
        &self,
        binding: &GroupBinding,
        entry: TileEntry,
        live: bool,
        within: Option<&mut DigestsWithinAnArea>,
    ) -> TileKey {
        let display_list = self.list.display_list;
        let chunk = display_list.chunk(entry.chunk());
        let (static_digest, records) = match entry {
            TileEntry::Placed { chunk: position, entry } => {
                let placed = binding.placed_entry(position, entry);
                let digest = match within {
                    Some(within) if placed.shows_in_a_tile_only_as_far_as_it_reaches_near_it => {
                        self.digest_of_placed_entry_within(chunk, placed, within)
                    }
                    _ => placed.static_digest,
                };
                (digest, placed.first_record..placed.end_record)
            }
            TileEntry::Record { record, .. } => (
                self.static_digest_of_records(chunk, record..record + 1),
                record..record + 1,
            ),
            TileEntry::Span {
                begin_offset,
                end_offset,
                ..
            } => {
                let records = chunk.record_table().records_within(begin_offset, end_offset);
                (self.static_digest_of_records(chunk, records.clone()), records)
            }
        };
        let live_digest = if live {
            self.live_digest_of_records(chunk, records)
        } else {
            None
        };
        match live_digest {
            None => static_digest,
            Some(live_digest) => {
                let mut hasher = KeyHasher::new();
                hasher.write_key(static_digest);
                hasher.write_key(live_digest);
                hasher.finish()
            }
        }
    }

    fn tile_entry_reads_outside_the_list(&self, binding: &GroupBinding, entry: TileEntry) -> bool {
        let records = match entry {
            TileEntry::Placed { chunk, entry } => return binding.placed_entry(chunk, entry).reads_outside_the_list,
            TileEntry::Record { record, .. } => record..record + 1,
            TileEntry::Span {
                chunk,
                begin_offset,
                end_offset,
            } => self
                .list
                .display_list
                .chunk(chunk)
                .record_table()
                .records_within(begin_offset, end_offset),
        };
        let table = self.list.display_list.chunk(entry.chunk()).record_table();
        table.records[records.start as usize..records.end as usize]
            .iter()
            .any(|record| record.reads_outside_the_list != 0)
    }

    fn key_of_tile(&self, binding: &GroupBinding, tile: &KeyedTile) -> TileKey {
        let mut hasher = KeyHasher::new();
        hasher.write_key(tile.constants_key);
        hasher.write_i32(tile.coord.column);
        hasher.write_i32(tile.coord.row);
        hasher.write_bool(self.list.replays_every_record_into_every_tile);
        match tile.frame_its_backdrop_blurs_mirror_at {
            Some(frame) => hasher.write_int_rect(frame),
            None => hasher.write_u32(0),
        }
        // A record shows in the tile as far as it reaches within antialiasing of the cell, and past that as far as a
        // backdrop filter in the tile reads.
        let mut within = tile.raster_margin_in_device_pixels.map(|raster_margin| {
            let margin = 2.0 * (INFLUENCE_MARGIN_IN_DEVICE_PIXELS + raster_margin) as f32;
            DigestsWithinAnArea::new(tile.coord.cell_rect().to_float().inflated(margin, margin))
        });
        for &entry in &tile.entries {
            hasher.write_key(self.digest_of_tile_entry(binding, entry, tile.reads_outside_the_list, within.as_mut()));
        }
        hasher.finish()
    }

    // The entry of the group that holds the record at `record` of the chunk at `chunk`, or the record alone where none
    // does.
    fn entry_holding_record(&self, binding: &GroupBinding, chunk: u32, record: u32) -> TileEntry {
        let table = self.list.display_list.chunk(chunk).record_table();
        let chunk_record = &table.records[record as usize];
        let alone = TileEntry::Record {
            chunk,
            record,
            begin_offset: chunk_record.offset,
            end_offset: chunk_record.end_offset,
        };
        let Some(&bound_index) = binding.bound_chunk_at.get(&chunk) else {
            return alone;
        };
        let bound = &binding.chunks[bound_index];
        let run = chunk_record.run_index;
        let held = bound
            .held
            .iter()
            .any(|&(begin, end)| begin <= chunk_record.offset && chunk_record.offset < end);
        if !held || !bound.held_runs.contains(&run) {
            return alone;
        }
        let placed_run = &bound.placed.runs[run as usize];
        if binding.whole_runs.contains_key(&(chunk, run)) {
            return TileEntry::Placed {
                chunk,
                entry: placed_run.whole,
            };
        }
        (placed_run.entries.0..placed_run.entries.1)
            .find(|&entry| {
                let placed = &bound.placed.entries[entry as usize];
                placed.first_record <= record && record < placed.end_record
            })
            .map_or(alone, |entry| TileEntry::Placed { chunk, entry })
    }

    // Binds the group to the list and the geometry of the frame: places the chunks it holds, and works out what binning
    // its tiles reads of the list beyond those chunks.
    fn binding_of_group(&self, stack: &LayerStack, group: usize) -> GroupBinding {
        let list = self.list;
        let display_list = list.display_list;
        let layer_group = &stack.groups[group];
        let mut held_of_chunks: Vec<(u32, Vec<(u32, u32)>)> = Vec::new();
        for span in &layer_group.record_spans {
            for (chunk, begin, end) in display_list.parts_of_span(*span) {
                match held_of_chunks.last_mut() {
                    Some((last_chunk, held)) if *last_chunk == chunk => match held.last_mut() {
                        Some(last) if last.1 == begin => last.1 = end,
                        _ => held.push((begin, end)),
                    },
                    _ => held_of_chunks.push((chunk, vec![(begin, end)])),
                }
            }
        }
        let mut digest_hasher = KeyHasher::new();
        digest_hasher.write_bool(stack.base_layer == layer_group.layer_below_every_cut);
        let chunk_names = display_list.chunk_names();
        if list.replays_every_record_into_every_tile {
            let mut entries: Vec<(ListPosition, TileEntry)> = held_of_chunks
                .iter()
                .flat_map(|(chunk, held)| {
                    held.iter().map(|&(begin_offset, end_offset)| {
                        (
                            ListPosition::new(*chunk, begin_offset),
                            TileEntry::Span {
                                chunk: *chunk,
                                begin_offset,
                                end_offset,
                            },
                        )
                    })
                })
                .collect();
            // The declarations of the content of the masks the group is under go with it, wherever they are.
            let tree = list.tree;
            let mut masks = Vec::new();
            for (chunk, held) in &held_of_chunks {
                for run in display_list.chunk(*chunk).runs() {
                    if !held
                        .iter()
                        .any(|&(begin, end)| begin < run.offset + run.size && run.offset < end)
                    {
                        continue;
                    }
                    let mut effect = run.context.effect;
                    while !effect.is_none() {
                        let node = &tree.effect_nodes[effect.0 as usize];
                        if matches!(node.data, EffectNodeData::Mask(_)) && !masks.contains(&effect) {
                            masks.push(effect);
                        }
                        effect = node.parent;
                    }
                }
            }
            for &(declared, position) in &list.mask_declarations {
                let is_held = held_of_chunks.iter().any(|(chunk, held)| {
                    *chunk == position.chunk
                        && held
                            .iter()
                            .any(|&(begin, end)| begin <= position.offset && position.offset < end)
                });
                if !masks.contains(&declared) || is_held {
                    continue;
                }
                let table = display_list.chunk(position.chunk).record_table();
                let record = table.records_within(position.offset, position.offset + 1).start;
                entries.push((
                    position,
                    TileEntry::Record {
                        chunk: position.chunk,
                        record,
                        begin_offset: position.offset,
                        end_offset: table.records[record as usize].end_offset,
                    },
                ));
            }
            return GroupBinding {
                chunk_names,
                chunks: Vec::new(),
                bound_chunk_at: HashMap::new(),
                whole_runs: HashMap::new(),
                backdrops: Vec::new(),
                replayed_everywhere: Some(entries),
                digest: digest_hasher.finish(),
            };
        }

        let mut chunks: Vec<BoundChunk> = Vec::with_capacity(held_of_chunks.len());
        for (position, held) in held_of_chunks {
            let runs = display_list.chunk(position).runs();
            let held_runs: Vec<u32> = (0..runs.len() as u32)
                .filter(|&run| {
                    let run = &runs[run as usize];
                    held.iter()
                        .any(|&(begin, end)| begin < run.offset + run.size && run.offset < end)
                })
                .collect();
            let placed = self.placed_chunk(position, &held_runs);
            chunks.push(BoundChunk {
                position,
                placed,
                held,
                held_runs,
                reach: Reach::Nothing,
                lists_reached_cells: false,
                reach_past_listed_cells: Reach::Nothing,
                digest: 0,
            });
        }
        let bound_chunk_at: HashMap<u32, usize> = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| (chunk.position, index))
            .collect();
        let first_held_position_of_run = |chunk: &BoundChunk, run: u32| {
            let run = &display_list.chunk(chunk.position).runs()[run as usize];
            let begin = chunk
                .held
                .iter()
                .find(|&&(begin, end)| begin < run.offset + run.size && run.offset < end)
                .map_or(run.offset, |&(begin, _)| begin.max(run.offset));
            ListPosition::new(chunk.position, begin)
        };

        // The backdrop filters that content of the group reads its backdrop through, each with where the first content
        // under it is. The effect the compositor composites the group inside, and those around it, are left out of the
        // tiles.
        let tree = list.tree;
        let composited_effect = stack.nodes[layer_group.node]
            .composited_effect
            .unwrap_or(EffectNodeIndex::NONE);
        let mut seen_effects = vec![false; tree.effect_nodes.len()];
        let mut backdrops = Vec::new();
        for chunk in &chunks {
            for &run in &chunk.held_runs {
                let mut effect = display_list.chunk(chunk.position).runs()[run as usize].context.effect;
                while !effect.is_none() && effect != composited_effect && !seen_effects[effect.0 as usize] {
                    seen_effects[effect.0 as usize] = true;
                    let node = &tree.effect_nodes[effect.0 as usize];
                    if matches!(&node.data, EffectNodeData::Effects(effects) if effects.backdrop_filter.is_some()) {
                        backdrops.push(self.backdrop_read_in_layer(effect, first_held_position_of_run(chunk, run)));
                    }
                    effect = node.parent;
                }
            }
        }
        for backdrop in &backdrops {
            let position = backdrop.first_record_position;
            digest_hasher.write_chunk_name(chunk_names[position.chunk as usize]);
            digest_hasher.write_u32(position.offset);
            digest_hasher.write_reach(backdrop.region_influence);
            digest_hasher.write_reach(backdrop.read_rect);
            digest_hasher.write_i32(backdrop.read_reach_in_device_pixels.unwrap_or(-1));
            digest_hasher.write_i32(backdrop.spread_past_region_in_device_pixels);
        }

        // The runs of a group effect under a mask are replayed whole wherever any of the group shows.
        let run_groups = list.run_groups;
        let mut masked_groups: BTreeMap<usize, (Reach<IntRect>, ListPosition)> = BTreeMap::new();
        for chunk in &chunks {
            for &run in &chunk.held_runs {
                let group = run_groups.group_of_run(display_list.first_run_of_chunk(chunk.position) + run as usize);
                if !run_groups.group_is_under_mask(group) {
                    continue;
                }
                let placed_run = &chunk.placed.runs[run as usize];
                let influence = chunk.placed.entries[placed_run.whole as usize].influence;
                let position = first_held_position_of_run(chunk, run);
                masked_groups
                    .entry(group)
                    .and_modify(|(united, first)| {
                        *united = united.united(influence);
                        *first = (*first).min(position);
                    })
                    .or_insert((influence, position));
            }
        }
        let mut whole_runs = HashMap::new();
        if !masked_groups.is_empty() {
            for chunk in &chunks {
                for &run in &chunk.held_runs {
                    let group = run_groups.group_of_run(display_list.first_run_of_chunk(chunk.position) + run as usize);
                    if let Some(&(influence, first_position)) = masked_groups.get(&group) {
                        whole_runs.insert(
                            (chunk.position, run),
                            WholeRun {
                                influence: influence_extended_through_backdrops(influence, first_position, &backdrops),
                                companions: Vec::new(),
                            },
                        );
                    }
                }
            }
        }
        let mut binding = GroupBinding {
            chunk_names,
            chunks,
            bound_chunk_at,
            whole_runs,
            backdrops,
            replayed_everywhere: None,
            digest: 0,
        };

        // A tile that replays content under a mask replays the declarations of the mask's content with it, wherever they
        // are and whatever they reach.
        if !list.mask_declarations.is_empty() && !binding.whole_runs.is_empty() {
            let mut companions_of_runs = Vec::new();
            for &(chunk_position, run) in binding.whole_runs.keys() {
                let own_entry = TileEntry::Placed {
                    chunk: chunk_position,
                    entry: binding.chunks[binding.bound_chunk_at[&chunk_position]].placed.runs[run as usize].whole,
                };
                let mut companions = Vec::new();
                let mut effect = display_list.chunk(chunk_position).runs()[run as usize].context.effect;
                while !effect.is_none() {
                    let node = &tree.effect_nodes[effect.0 as usize];
                    if matches!(node.data, EffectNodeData::Mask(_)) {
                        for &(declared, position) in &list.mask_declarations {
                            if declared != effect {
                                continue;
                            }
                            let table = display_list.chunk(position.chunk).record_table();
                            let record = table.records_within(position.offset, position.offset + 1).start;
                            let companion = self.entry_holding_record(&binding, position.chunk, record);
                            if companion != own_entry && !companions.contains(&companion) {
                                companions.push(companion);
                            }
                        }
                    }
                    effect = node.parent;
                }
                companions_of_runs.push(((chunk_position, run), companions));
            }
            companions_of_runs.sort_unstable_by_key(|(run, _)| *run);
            for (run, companions) in companions_of_runs {
                for &companion in &companions {
                    digest_hasher.write_chunk_name(binding.chunk_names[companion.chunk() as usize]);
                    digest_hasher.write_key(self.digest_of_tile_entry(&binding, companion, false, None));
                }
                binding
                    .whole_runs
                    .get_mut(&run)
                    .expect("companions are found for runs replayed whole")
                    .companions = companions;
            }
        }

        // Where what the group holds of each chunk can show, and what binning reads of the chunk.
        for index in 0..binding.chunks.len() {
            let chunk = &binding.chunks[index];
            let mut reach = Reach::Nothing;
            let mut reach_of_runs_binned_by_record = Reach::Nothing;
            let mut hasher = KeyHasher::new();
            hasher.write_chunk_name(binding.chunk_names[chunk.position as usize]);
            hasher.write_key(chunk.placed.key);
            for &(begin, end) in &chunk.held {
                hasher.write_u32(begin);
                hasher.write_u32(end);
            }
            for &run in &chunk.held_runs {
                match binding.whole_runs.get(&(chunk.position, run)) {
                    Some(whole_run) => {
                        reach = reach.united(whole_run.influence);
                        hasher.write_u32(run);
                        hasher.write_reach(whole_run.influence);
                    }
                    None => {
                        reach_of_runs_binned_by_record =
                            reach_of_runs_binned_by_record.united(chunk.placed.runs[run as usize].reach);
                    }
                }
            }
            // A backdrop filter can extend where content painted before it shows past the cells it reaches.
            let lists_reached_cells = binding.backdrops.is_empty();
            let reach_past_listed_cells = if lists_reached_cells {
                reach.united(chunk.placed.reach_past_listed_cells)
            } else {
                reach.united(influence_extended_through_backdrops(
                    reach_of_runs_binned_by_record,
                    ListPosition::new(chunk.position, chunk.held.first().map_or(0, |held| held.0)),
                    &binding.backdrops,
                ))
            };
            let digest = hasher.finish();
            let chunk = &mut binding.chunks[index];
            chunk.reach = reach_past_listed_cells.united(reach_of_runs_binned_by_record);
            chunk.lists_reached_cells = lists_reached_cells;
            chunk.reach_past_listed_cells = reach_past_listed_cells;
            chunk.digest = digest;
        }
        binding.digest = digest_hasher.finish();
        binding
    }

    // Calls `visit` with each entry of the chunk the group holds, where it is painted, where it can show, and the
    // entries replayed with it.
    fn for_each_held_entry(
        binding: &GroupBinding,
        chunk: &BoundChunk,
        mut visit: impl FnMut(ListPosition, TileEntry, Reach<IntRect>, &[TileEntry]),
    ) {
        for &run in &chunk.held_runs {
            let placed_run = &chunk.placed.runs[run as usize];
            if let Some(whole_run) = binding.whole_runs.get(&(chunk.position, run)) {
                let entry = &chunk.placed.entries[placed_run.whole as usize];
                visit(
                    ListPosition::new(chunk.position, entry.begin_offset),
                    TileEntry::Placed {
                        chunk: chunk.position,
                        entry: placed_run.whole,
                    },
                    whole_run.influence,
                    &whole_run.companions,
                );
                continue;
            }
            for entry_index in placed_run.entries.0..placed_run.entries.1 {
                let entry = &chunk.placed.entries[entry_index as usize];
                let held = chunk
                    .held
                    .iter()
                    .any(|&(begin, end)| begin <= entry.begin_offset && entry.begin_offset < end);
                if !held {
                    continue;
                }
                let position = ListPosition::new(chunk.position, entry.begin_offset);
                visit(
                    position,
                    TileEntry::Placed {
                        chunk: chunk.position,
                        entry: entry_index,
                    },
                    influence_extended_through_backdrops(entry.influence, position, &binding.backdrops),
                    &[],
                );
            }
        }
    }

    /// Binds the tiles of `group` to the list and the geometry of the frame and to the group in `stack`. A tile keyed
    /// before keeps its bin and its key where the chunks that can reach it and the barriers that cut it are as they
    /// were, in the same order, whatever barriers they are, and is forgotten otherwise.
    pub fn bind_group(&self, keys: &mut GroupTileKeys, stack: &LayerStack, group: usize) {
        let binding = self.binding_of_group(stack, group);
        keys.replays_whole_list = stack.replays_whole_list() && self.list.tree_has_sorting_contexts;
        let Some(old_binding) = keys.binding.replace(binding) else {
            keys.cells.clear();
            return;
        };
        let binding = keys.binding.as_ref().expect("the binding was just replaced");
        if keys.cells.is_empty() {
            return;
        }
        keys.tiles_reading_outside_the_list_are_rebound = true;
        if binding.digest != old_binding.digest || binding.replayed_everywhere.is_some() {
            keys.cells.clear();
            return;
        }
        let layer_group = &stack.groups[group];
        let chunks_reaching = binding.chunks_reaching(keys.cells.keys().copied());
        let positions_of_names: HashMap<ChunkName, u32> = binding
            .chunk_names
            .iter()
            .enumerate()
            .map(|(position, name)| (*name, position as u32))
            .collect();
        keys.cells.retain(|coord, cell| {
            let cell_rect = coord.cell_rect();
            let signature = binding.signature_of_cell(layer_group, cell_rect, &chunks_reaching[coord]);
            if signature != cell.signature {
                return false;
            }
            for tile in &mut cell.tiles {
                let Some(layer) = layer_group.layer_above_cuts_of_cell(cell_rect, tile.cuts_of_its_cell_below_it)
                else {
                    return false;
                };
                tile.layer = layer;
                tile.cuts = layer_group.cuts_of_cell(cell_rect);
                for entry in &mut tile.entries {
                    let name = old_binding.chunk_names[entry.chunk() as usize];
                    let Some(&position) = positions_of_names.get(&name) else {
                        return false;
                    };
                    *entry = entry.in_chunk(position);
                }
            }
            true
        });
    }

    /// Keys the tiles of the layers of `group` within `shown_rect` that are not keyed yet: each entry of the group that
    /// reaches a tile goes into the layer of the group its paint position falls in there, and the base layer, if it is
    /// one of the group's, holds every tile. Tiles keyed before are keyed again only where what their entries read
    /// outside the list, the constants of their layer, or the edge of `frame` that their backdrop blurs mirror at
    /// changed, and those outside `kept_rect` are forgotten. `frame` is where the frame is in the group's space, where
    /// that is not the frame's own. The group is bound to the list and the geometry of the frame.
    #[allow(clippy::too_many_arguments)]
    pub fn key_shown_tiles(
        &self,
        keys: &mut GroupTileKeys,
        stack: &LayerStack,
        group: usize,
        constants_of_layer: impl Fn(usize) -> LayerConstants,
        shown_rect: IntRect,
        kept_rect: IntRect,
        frame: Option<IntRect>,
    ) -> TileKeying {
        let list = self.list;
        let layer_group = &stack.groups[group];
        let mut constants_keys: Vec<(usize, TileKey)> = Vec::new();
        let mut constants_key_of = |layer: usize| match constants_keys.iter().find(|(known, _)| *known == layer) {
            Some((_, key)) => *key,
            None => {
                let key = constants_of_layer(layer).key();
                constants_keys.push((layer, key));
                key
            }
        };
        keys.cells
            .retain(|coord, _| !coord.cell_rect().intersected(kept_rect).is_empty());
        let binding = keys
            .binding
            .as_ref()
            .expect("a group is bound before its tiles are keyed");

        let mut keying = TileKeying::default();
        let live_changed =
            keys.live_stamp != list.live_stamp || std::mem::take(&mut keys.tiles_reading_outside_the_list_are_rebound);
        keys.live_stamp = list.live_stamp;
        for (coord, cell) in &mut keys.cells {
            let frame_backdrop_blurs_mirror_at =
                binding.frame_the_backdrop_blurs_of_cell_mirror_at(coord.cell_rect(), frame);
            for tile in &mut cell.tiles {
                let constants_key = constants_key_of(tile.layer);
                if (live_changed && tile.reads_outside_the_list)
                    || tile.constants_key != constants_key
                    || tile.frame_its_backdrop_blurs_mirror_at != frame_backdrop_blurs_mirror_at
                {
                    tile.constants_key = constants_key;
                    tile.frame_its_backdrop_blurs_mirror_at = frame_backdrop_blurs_mirror_at;
                    tile.key = self.key_of_tile(binding, tile);
                    keying.tiles_keyed += 1;
                }
            }
        }

        let new_coords: Vec<TileCoord> = tile_coords_touching(shown_rect)
            .filter(|coord| !keys.cells.contains_key(coord))
            .collect();
        if new_coords.is_empty() {
            return keying;
        }
        let new_cells = new_coords
            .iter()
            .fold(IntRect::default(), |cells, coord| cells.united(coord.cell_rect()));
        // Per new cell, the layers that hold something there, each with its entries and where they are painted.
        type Bin = Vec<(usize, Vec<(ListPosition, TileEntry, Reach<IntRect>)>)>;
        let mut bins: BTreeMap<TileCoord, Bin> = new_coords.iter().map(|coord| (*coord, Vec::new())).collect();
        let mut add_to_bins =
            |position: ListPosition, entry: TileEntry, influence: Reach<IntRect>, companions: &[TileEntry]| {
                let reached = influence.part_of(new_cells);
                if reached.is_empty() {
                    return;
                }
                for coord in tile_coords_touching(reached) {
                    let Some(bin) = bins.get_mut(&coord) else {
                        continue;
                    };
                    let layer = layer_group.layer_at(position, coord.cell_rect());
                    // The stack composites only the layers its units reach, and units work out where content can
                    // show apart from entries, as `SpaceGeometry::ink_device_rect` rather than
                    // `influence_of_content_within`, so an entry has to stay within the reach of its unit.
                    debug_assert!(
                        stack.layers[layer].may_hold_content,
                        "an entry at {position:?} goes into L{layer}, which no unit reaches, in the tile at {coord}"
                    );
                    let layer_entries = match bin.iter_mut().position(|(bin_layer, _)| *bin_layer == layer) {
                        Some(index) => &mut bin[index].1,
                        None => {
                            bin.push((layer, Vec::new()));
                            &mut bin.last_mut().expect("a bin was just pushed").1
                        }
                    };
                    layer_entries.push((position, entry, influence));
                    for &companion in companions {
                        let span = binding.span_of(companion);
                        layer_entries.push((span.begin, companion, binding.influence_of(companion)));
                    }
                }
            };
        match &binding.replayed_everywhere {
            Some(entries) => {
                for &(position, entry) in entries {
                    add_to_bins(position, entry, Reach::Anywhere, &[]);
                }
            }
            None => {
                for chunk in &binding.chunks {
                    if chunk.reach.part_of(new_cells).is_empty() {
                        continue;
                    }
                    Self::for_each_held_entry(binding, chunk, &mut add_to_bins);
                }
            }
        }

        let chunks_reaching = binding.chunks_reaching(new_coords.iter().copied());
        let base_layer = (stack.base_layer == layer_group.layer_below_every_cut).then_some(stack.base_layer);
        for (coord, mut bin) in bins {
            if let Some(base_layer) = base_layer
                && !bin.iter().any(|(layer, _)| *layer == base_layer)
            {
                bin.push((base_layer, Vec::new()));
            }
            let cell = coord.cell_rect();
            let raster_margin_in_device_pixels = binding
                .backdrops
                .iter()
                .filter(|backdrop| backdrop.region_influence.touches(cell))
                .try_fold(0i32, |margin, backdrop| {
                    Some(
                        margin
                            .saturating_add(backdrop.read_reach_in_device_pixels?)
                            .saturating_add(backdrop.spread_past_region_in_device_pixels),
                    )
                });
            let cuts = layer_group.cuts_of_cell(cell);
            let frame_its_backdrop_blurs_mirror_at = binding.frame_the_backdrop_blurs_of_cell_mirror_at(cell, frame);
            let mut tiles = Vec::with_capacity(bin.len());
            for (layer, mut layer_entries) in bin {
                layer_entries.sort_unstable_by_key(|(position, entry, _)| (*position, *entry));
                layer_entries.dedup_by_key(|(_, entry, _)| *entry);
                let content_bounds = layer_entries
                    .iter()
                    .fold(IntRect::default(), |bounds, (_, _, influence)| match influence {
                        Reach::Within(rect) => bounds.united(rect.intersected(cell)),
                        Reach::Anywhere => cell,
                        Reach::Nothing => bounds,
                    });
                let entries: Vec<TileEntry> = layer_entries.into_iter().map(|(_, entry, _)| entry).collect();
                let reads_outside_the_list = entries
                    .iter()
                    .any(|&entry| self.tile_entry_reads_outside_the_list(binding, entry));
                let mut tile = KeyedTile {
                    layer,
                    coord,
                    key: 0,
                    raster_margin_in_device_pixels,
                    cuts,
                    content_bounds,
                    cuts_of_its_cell_below_it: layer_group.cuts_of_cell_below_layer(cell, layer),
                    entries,
                    constants_key: constants_key_of(layer),
                    reads_outside_the_list,
                    frame_its_backdrop_blurs_mirror_at,
                };
                tile.key = self.key_of_tile(binding, &tile);
                tiles.push(tile);
                keying.tiles_keyed += 1;
            }
            // A cell that nothing of the group reaches is noted as holding nothing, which is not binning it.
            if !tiles.is_empty() {
                keying.cells_binned += 1;
            }
            let signature = binding.signature_of_cell(layer_group, cell, &chunks_reaching[&coord]);
            keys.cells.insert(coord, BinnedCell { signature, tiles });
        }
        keying
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{CanvasId, DisplayListCommand, FillRect, VISUAL_VIEWPORT_NODE_INDEX};
    use crate::display_list::layers::{LayerStack, LayerStackInputs, layer_stack_of};
    use crate::display_list::placement::RasterSpaces;
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
        let declaration_position = ListPosition::new(0, list.builder.byte_size() as u32);
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
        let masked_position = ListPosition::new(0, list.builder.byte_size() as u32);
        list = list.with(fill(IntRect::new(10, 10, 80, 80), BLUE), masked);

        let display_list = DisplayListStorage::of_builder(&list.builder);
        let stack = layer_stack_of(&LayerStackInputs {
            list: &display_list,
            tree: &tree,
            scroll_offsets: &[],
            viewport_rect: VIEWPORT,
            raster_transform: FloatMatrix4x4::identity(),
        });
        assert_eq!(stack.unlayered_reason, None);
        let list_of_frame = ListOfFrame::new(
            &display_list,
            &tree,
            &stack,
            ReplayReadsOutsideTheList {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContentChanges,
            },
        );
        let spaces = RasterSpaces::of_spaces(&tree, stack.spaces.clone());
        let geometry = SpaceGeometry::new(&tree, &spaces, 0, &[], VIEWPORT, FloatMatrix4x4::identity());
        let raster_space = RasterSpaceOfFrame::new(&list_of_frame, &geometry);
        let group = stack
            .groups
            .iter()
            .position(|group| group.record_spans.iter().any(|span| span.contains(masked_position)))
            .unwrap();
        let mut keys = GroupTileKeys::default();
        raster_space.bind_group(&mut keys, &stack, group);
        let constants = |_| LayerConstants {
            raster_space_identity: 0,
            clear_color: Color::TRANSPARENT,
            root_group_color: Color::TRANSPARENT,
            raster_transform: FloatMatrix4x4::identity(),
        };
        raster_space.key_shown_tiles(&mut keys, &stack, group, constants, VIEWPORT, VIEWPORT, None);
        let tile_with_masked_content = keys
            .tiles_within(TileCoord::new(0, 0).cell_rect())
            .find(|tile| {
                keys.record_spans_of_tiles([*tile])
                    .iter()
                    .any(|span| span.contains(masked_position))
            })
            .unwrap();
        let spans = keys.record_spans_of_tiles([tile_with_masked_content]);
        assert!(spans.iter().any(|span| span.contains(declaration_position)));
        assert!(spans.windows(2).all(|pair| pair[0].end <= pair[1].begin));
    }

    fn constants_for_tests(_: usize) -> LayerConstants {
        LayerConstants {
            raster_space_identity: 0,
            clear_color: Color::TRANSPARENT,
            root_group_color: Color::TRANSPARENT,
            raster_transform: FloatMatrix4x4::identity(),
        }
    }

    fn chunk_of(command: FillRect, context: ContextRef) -> Arc<ListChunk> {
        let mut builder = DisplayListBuilder::new();
        builder.append(&command, &[], context);
        Arc::new(ListChunk::copied(builder.bytes(), builder.command_runs(), 0))
    }

    fn stack_of(display_list: &DisplayListStorage, tree: &VisualContextTree) -> LayerStack {
        layer_stack_of(&LayerStackInputs {
            list: display_list,
            tree,
            scroll_offsets: &[],
            viewport_rect: VIEWPORT,
            raster_transform: FloatMatrix4x4::identity(),
        })
    }

    // The key of each tile of the viewport in the only group of the stack, keyed from nothing.
    fn keys_from_nothing(
        display_list: &DisplayListStorage,
        tree: &VisualContextTree,
        stack: &LayerStack,
    ) -> Vec<(TileCoord, usize, TileKey)> {
        let list_of_frame = ListOfFrame::new(
            display_list,
            tree,
            stack,
            ReplayReadsOutsideTheList {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContentChanges,
            },
        );
        let spaces = RasterSpaces::of_spaces(tree, stack.spaces.clone());
        let geometry = SpaceGeometry::new(tree, &spaces, 0, &[], VIEWPORT, FloatMatrix4x4::identity());
        let raster_space = RasterSpaceOfFrame::new(&list_of_frame, &geometry);
        let mut keys = GroupTileKeys::default();
        raster_space.bind_group(&mut keys, stack, 0);
        raster_space.key_shown_tiles(&mut keys, stack, 0, constants_for_tests, VIEWPORT, VIEWPORT, None);
        keyed_tiles(&keys)
    }

    fn keyed_tiles(keys: &GroupTileKeys) -> Vec<(TileCoord, usize, TileKey)> {
        keys.tiles_within(VIEWPORT)
            .map(|tile| (tile.coord, tile.layer, tile.key))
            .collect()
    }

    // Keys every tile of the viewport in the only group of `stack` for each frame in turn, with what was keyed for one
    // frame kept for the next, and answers how many chunks each frame placed, what keying it did, and its keys.
    fn keyed_in_turn(
        frames: &[(&DisplayListStorage, &VisualContextTree, &LayerStack)],
    ) -> Vec<(u64, TileKeying, Vec<(TileCoord, usize, TileKey)>)> {
        let mut keys = GroupTileKeys::default();
        frames
            .iter()
            .map(|&(display_list, tree, stack)| {
                let list_of_frame = ListOfFrame::new(
                    display_list,
                    tree,
                    stack,
                    ReplayReadsOutsideTheList {
                        scrollbars: &[],
                        time_ns: 0,
                        live_content: &NoLiveContentChanges,
                    },
                );
                let spaces = RasterSpaces::of_spaces(tree, stack.spaces.clone());
                let geometry = SpaceGeometry::new(tree, &spaces, 0, &[], VIEWPORT, FloatMatrix4x4::identity());
                let raster_space = RasterSpaceOfFrame::new(&list_of_frame, &geometry);
                raster_space.bind_group(&mut keys, stack, 0);
                let keying =
                    raster_space.key_shown_tiles(&mut keys, stack, 0, constants_for_tests, VIEWPORT, VIEWPORT, None);
                (raster_space.chunks_placed(), keying, keyed_tiles(&keys))
            })
            .collect()
    }

    #[test]
    fn an_animation_sample_places_again_only_the_chunks_under_the_node_it_moves() {
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
            ..root
        };
        let display_list = DisplayListStorage::of_chunks(vec![
            chunk_of(fill(VIEWPORT, GREEN), root),
            chunk_of(fill(IntRect::new(100, 300, 50, 50), BLUE), moving),
            chunk_of(fill(IntRect::new(600, 400, 20, 20), RED), root),
        ])
        .unwrap();
        let at_start = tree.with_visual_animation_samples(0);
        let halfway = tree.with_visual_animation_samples(500_000_000);
        let stack = stack_of(&display_list, &at_start);
        assert_eq!(stack_of(&display_list, &halfway).layers, stack.layers);

        let turns = keyed_in_turn(&[(&display_list, &at_start, &stack), (&display_list, &halfway, &stack)]);
        assert_eq!(turns[0].0, 3);
        // Only the chunk under the moving node is placed again, and only the cells the box leaves and enters are binned
        // again.
        let (chunks_placed, keying, keys) = &turns[1];
        assert_eq!(*chunks_placed, 1);
        assert_eq!(keying.cells_binned, 2);
        assert_eq!(keys, &keys_from_nothing(&display_list, &halfway, &stack));
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

        let turns = keyed_in_turn(&[(&display_list, &at_start, &stack), (&display_list, &halfway, &stack)]);
        // Nothing moved, so every cell keeps its bin, but the cell of the box is keyed for the color it shows now.
        let (chunks_placed, keying, keys) = &turns[1];
        assert_eq!(*chunks_placed, 0);
        assert_eq!(keying.cells_binned, 0);
        assert_eq!(keys, &keys_from_nothing(&display_list, &halfway, &stack));
        let changed: Vec<TileCoord> = turns[0]
            .2
            .iter()
            .zip(keys)
            .filter(|(old, new)| old != new)
            .map(|(old, _)| old.0)
            .collect();
        assert_eq!(changed, [TileCoord::new(2, 1)]);
    }
}
