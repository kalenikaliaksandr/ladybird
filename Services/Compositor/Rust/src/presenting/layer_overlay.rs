/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The compositor layers overlay: over each frame, the layers the frame is composited from, the tiles each of them holds
//! and how often each was rastered since the overlay was switched on, and the tiles rastered lately, highlighted by why
//! they were and fading out. What it shows of a frame is worked out into a plain model first, which is both drawn and
//! described to the compositor layers window.

use crate::overlays::{
    CompositorLayersOverlayState, OverlayLabel, OverlayLabelShaper, OverlayRecorder, OverlayRecords,
};
use libcompositing_rust::display_list::layers::{LayerIdentity, SpaceToFrame};
use libcompositing_rust::display_list::tiles::TileCoord;
use libcompositing_rust::ipc_types::{
    Compositing_CompositorLayerDescription as CompositorLayerDescription,
    Compositing_CompositorLayersDescription as CompositorLayersDescription,
    Compositing_CompositorTileHighlightDescription as CompositorTileHighlightDescription,
};
use libcompositing_rust::time::{Duration, MonotonicTime};
use libgfx_rust::{Color, FloatPoint, IntPoint, IntRect, IntSize};
use std::collections::{BTreeMap, HashMap, btree_map};
use std::hash::{DefaultHasher, Hash, Hasher};

pub const RASTER_HIGHLIGHT_FADE_DURATION: Duration = Duration::from_milliseconds(500);
const MAXIMUM_CACHED_LABEL_COUNT: usize = 4096;
const LABEL_FONT_SIZE_IN_POINTS: f32 = 10.0;
const SHORTEST_TIME_BETWEEN_PUBLISHED_DESCRIPTIONS: Duration = Duration::from_milliseconds(100);
const BYTES_PER_PIXEL: u64 = 4;
const RASTER_HIGHLIGHT_ALPHA_BEFORE_FADING: u8 = 115;
const ONE_COLOR_TILE_TINT_ALPHA: u8 = 40;
const LABEL_BACKGROUND_COLOR: Color = Color::from_rgba(16, 18, 24, 200);
const LABEL_TEXT_COLOR: Color = Color::from_rgba(255, 255, 255, 255);
const OUTLINE_HALO_COLOR: Color = Color::from_rgba(0, 0, 0, 150);

/// Why a tile was rastered, as the compositor tells the cases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterCause {
    /// The layer held no tile at the cell.
    NewlyShown,
    /// The tile held pixels of other content.
    ContentHashChanged,
    /// The frame made every tile stale, or rastering the tile failed before.
    HeldPixelsInvalidated,
    /// The tile held less of its cell than the frame shows.
    HeldLessThanNeeded,
    /// The tile's surface was on another backend than the frame's.
    HeldOnAnotherBackend,
    /// No surface could be had for the tile, so the frame was painted straight from the list where the tile shows.
    PaintedStraightIntoTheFrame,
}

impl RasterCause {
    pub const ALL: [RasterCause; 6] = [
        RasterCause::NewlyShown,
        RasterCause::ContentHashChanged,
        RasterCause::HeldPixelsInvalidated,
        RasterCause::HeldLessThanNeeded,
        RasterCause::HeldOnAnotherBackend,
        RasterCause::PaintedStraightIntoTheFrame,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RasterCause::NewlyShown => "newly shown",
            RasterCause::ContentHashChanged => "key changed",
            RasterCause::HeldPixelsInvalidated => "held pixels invalidated",
            RasterCause::HeldLessThanNeeded => "held less than needed",
            RasterCause::HeldOnAnotherBackend => "held on another backend",
            RasterCause::PaintedStraightIntoTheFrame => "painted straight into the frame",
        }
    }

    pub fn highlight_color(self) -> Color {
        match self {
            RasterCause::NewlyShown => Color::from_rgb(34, 197, 94),
            RasterCause::ContentHashChanged => Color::from_rgb(239, 68, 68),
            RasterCause::HeldPixelsInvalidated => Color::from_rgb(245, 158, 11),
            RasterCause::HeldLessThanNeeded => Color::from_rgb(59, 130, 246),
            RasterCause::HeldOnAnotherBackend => Color::from_rgb(168, 85, 247),
            RasterCause::PaintedStraightIntoTheFrame => Color::from_rgb(236, 72, 153),
        }
    }
}

/// How a canvas, video or nested context layer reaches the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaPresentation {
    ShownByWindowServer,
    Replayed { why: String },
}

impl std::fmt::Display for MediaPresentation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaPresentation::ShownByWindowServer => formatter.write_str("shown by the window server"),
            MediaPresentation::Replayed { why } => write!(formatter, "replayed: {why}"),
        }
    }
}

/// What a frame did to bring the tiles of its layers up to date.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameWorkCounts {
    pub tiles_rastered: u64,
    pub shown_tiles_of_one_color: u64,
    pub tiles_covered_by_opaque_tiles_above: u64,
    pub cells_binned: u64,
    pub chunks_placed: u64,
    /// Whether the frame took a plan of layers into use that the frame before was not drawn with.
    pub took_plan_into_use: bool,
}

/// A layer as a frame composites it, which the overlay is worked out from.
pub struct LayerOfFrame {
    pub identity: LayerIdentity,
    /// What the display list dump calls the layer.
    pub name: String,
    /// From the raster coordinates of the layer's space into those of the frame.
    pub space_to_frame: SpaceToFrame,
    /// In the raster coordinates of the frame.
    pub composite_clip: Option<IntRect>,
    pub content: LayerOfFrameContent,
}

/// A tile a layer holds. A tile of one color has no surface.
pub struct HeldTile {
    pub coord: TileCoord,
    pub held_part_of_cell: IntRect,
    pub is_one_color: bool,
}

pub enum LayerOfFrameContent {
    Painted {
        held_tiles: Vec<HeldTile>,
    },
    Media {
        reach: IntRect,
        presentation: MediaPresentation,
    },
}

/// What the overlay is worked out from for a frame.
pub struct FrameOfLayerOverlay {
    pub viewport_rect: IntRect,
    pub painted_at: MonotonicTime,
    /// From the base layer up.
    pub layers: Vec<LayerOfFrame>,
    pub why_list_is_one_layer: Option<String>,
    /// The layer stack's lines of composited effects, media without layers and effects reading only their own layer.
    pub notes_of_layer_stack: Vec<String>,
    pub work_counts: FrameWorkCounts,
}

/// The last time a tile was rastered, as the overlay shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastRaster {
    pub cause: RasterCause,
    pub rect_in_layer_space: IntRect,
    pub age: Duration,
}

impl LastRaster {
    pub fn is_highlighted(&self) -> bool {
        self.age < RASTER_HIGHLIGHT_FADE_DURATION
    }

    pub fn highlight_opacity(&self) -> f32 {
        if !self.is_highlighted() {
            return 0.0;
        }
        1.0 - self.age.nanoseconds() as f32 / RASTER_HIGHLIGHT_FADE_DURATION.nanoseconds() as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelTile {
    pub coord: TileCoord,
    /// None where the layer holds no tile at the cell, as where a tile was painted straight into the frame.
    pub held_part_of_cell: Option<IntRect>,
    pub is_one_color: bool,
    pub raster_count_since_overlay_switched_on: u32,
    pub last_raster: Option<LastRaster>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelLayerContent {
    Painted {
        tiles: Vec<ModelTile>,
        bytes_held_by_tile_surfaces: u64,
    },
    Media {
        reach: IntRect,
        presentation: MediaPresentation,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelLayer {
    /// Names the layer in the descriptions the compositor publishes, for as long as its identity stays.
    pub id: u64,
    pub name: String,
    pub color: Color,
    /// Whether the frames leave the layer out.
    pub hidden: bool,
    pub space_to_frame: SpaceToFrame,
    pub composite_clip: Option<IntRect>,
    pub content: ModelLayerContent,
}

impl ModelLayer {
    fn visible_rect_in_frame(&self, viewport_rect: IntRect) -> IntRect {
        self.composite_clip
            .map_or(viewport_rect, |clip| viewport_rect.intersected(clip))
    }

    fn rect_in_frame(&self, rect_in_layer_space: IntRect) -> IntRect {
        self.space_to_frame.rect_in_frame(rect_in_layer_space)
    }

    fn bounds_in_layer_space(&self) -> IntRect {
        match &self.content {
            ModelLayerContent::Painted { tiles, .. } => tiles
                .iter()
                .filter_map(|tile| tile.held_part_of_cell)
                .fold(IntRect::default(), IntRect::united),
            ModelLayerContent::Media { reach, .. } => *reach,
        }
    }
}

/// What the overlay shows over a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerOverlayModel {
    pub viewport_rect: IntRect,
    /// From the base layer up.
    pub layers: Vec<ModelLayer>,
    pub why_list_is_one_layer: Option<String>,
    pub notes_of_layer_stack: Vec<String>,
    pub work_counts_of_frame: FrameWorkCounts,
    /// Until when some tile stays highlighted.
    pub highlights_fade_until: Option<MonotonicTime>,
}

fn count_with_noun(count: u64, singular_noun: &str, plural_noun: &str) -> String {
    if count == 1 {
        format!("1 {singular_noun}")
    } else {
        format!("{count} {plural_noun}")
    }
}

fn memory_size_text(bytes: u64) -> String {
    const MEBIBYTE: u64 = 1024 * 1024;
    if bytes < MEBIBYTE {
        return format!("{} KiB", bytes.div_ceil(1024));
    }
    format!("{:.1} MiB", bytes as f64 / MEBIBYTE as f64)
}

impl LayerOverlayModel {
    fn details_of_layer(layer: &ModelLayer) -> String {
        match &layer.content {
            ModelLayerContent::Painted {
                tiles,
                bytes_held_by_tile_surfaces,
            } => {
                let held_tile_count = tiles.iter().filter(|tile| tile.held_part_of_cell.is_some()).count() as u64;
                let one_color_tile_count = tiles.iter().filter(|tile| tile.is_one_color).count();
                format!(
                    "{} ({one_color_tile_count} of one color), {}",
                    count_with_noun(held_tile_count, "tile", "tiles"),
                    memory_size_text(*bytes_held_by_tile_surfaces)
                )
            }
            ModelLayerContent::Media { presentation, .. } => presentation.to_string(),
        }
    }

    fn summary_of_last_frame(&self) -> String {
        let counts = self.work_counts_of_frame;
        format!(
            "last frame: {} rastered, {} of one color, {} covered, {} binned, {} placed, {}",
            count_with_noun(counts.tiles_rastered, "tile", "tiles"),
            counts.shown_tiles_of_one_color,
            counts.tiles_covered_by_opaque_tiles_above,
            count_with_noun(counts.cells_binned, "tile", "tiles"),
            count_with_noun(counts.chunks_placed, "chunk", "chunks"),
            if counts.took_plan_into_use {
                "a new plan was taken into use"
            } else {
                "the stack was kept"
            }
        )
    }

    pub fn description_for_layers_window(&self) -> CompositorLayersDescription {
        let rgb_of_color =
            |color: Color| u32::from(color.red()) << 16 | u32::from(color.green()) << 8 | u32::from(color.blue());
        let mut notes: Vec<String> = Vec::new();
        if let Some(why) = &self.why_list_is_one_layer {
            notes.push(format!("single layer: {why}"));
        }
        notes.extend(self.notes_of_layer_stack.iter().cloned());
        CompositorLayersDescription {
            layers: self
                .layers
                .iter()
                .map(|layer| CompositorLayerDescription {
                    id: layer.id,
                    name: layer.name.clone().into(),
                    details: Self::details_of_layer(layer).into(),
                    rgb_of_outline: rgb_of_color(layer.color),
                    hidden: layer.hidden,
                })
                .collect(),
            notes: notes.into_iter().map(Into::into).collect(),
            summary_of_last_frame: self.summary_of_last_frame().into(),
            tile_highlights: RasterCause::ALL
                .iter()
                .map(|cause| CompositorTileHighlightDescription {
                    raster_cause: cause.name().to_owned().into(),
                    rgb: rgb_of_color(cause.highlight_color()),
                })
                .collect(),
        }
    }
}

fn tile_label_text(tile: &ModelTile) -> String {
    format!(
        "{},{} x{}",
        tile.coord.column, tile.coord.row, tile.raster_count_since_overlay_switched_on
    )
}

fn layer_label_text(layer: &ModelLayer) -> String {
    match &layer.content {
        ModelLayerContent::Painted { .. } => layer.name.clone(),
        ModelLayerContent::Media { presentation, .. } => format!("{}: {presentation}", layer.name),
    }
}

/// Names the layer in the descriptions the compositor publishes, for as long as its identity stays.
pub fn id_of_layer(identity: &LayerIdentity) -> u64 {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}

/// A color of the layer's own, which stays as long as its identity does, wherever the layer is in the stack.
pub fn color_of_layer(identity: &LayerIdentity) -> Color {
    let hash = id_of_layer(identity);
    let hue = (hash % 360) as f32;
    let lighter_shade_tells_apart_similar_hues = hash & (1 << 40) != 0;
    let lightness = if lighter_shade_tells_apart_similar_hues {
        0.62
    } else {
        0.5
    };
    color_of_hsl(hue, 0.9, lightness)
}

fn color_of_hsl(hue_in_degrees: f32, saturation: f32, lightness: f32) -> Color {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue_in_degrees / 60.0;
    let second_largest_component = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, second_largest_component, 0.0),
        1 => (second_largest_component, chroma, 0.0),
        2 => (0.0, chroma, second_largest_component),
        3 => (0.0, second_largest_component, chroma),
        4 => (second_largest_component, 0.0, chroma),
        _ => (chroma, 0.0, second_largest_component),
    };
    let lightness_added_to_each_component = lightness - chroma / 2.0;
    let channel = |component: f32| {
        ((component + lightness_added_to_each_component) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgb(channel(red), channel(green), channel(blue))
}

#[derive(Clone, Copy, Debug)]
struct RasterHistoryOfTile {
    raster_count_since_overlay_switched_on: u32,
    last_cause: RasterCause,
    last_rect_in_layer_space: IntRect,
    last_rastered_at: MonotonicTime,
}

#[derive(Default)]
struct ShapedLabelCache {
    font_family: Option<String>,
    device_pixel_ratio: f64,
    label_by_text: HashMap<String, Option<OverlayLabel>>,
}

impl ShapedLabelCache {
    fn shape_labels_not_cached_yet(
        &mut self,
        texts: &[String],
        font_family: Option<&str>,
        device_pixel_ratio: f64,
        shaper: &dyn OverlayLabelShaper,
    ) {
        if self.font_family.as_deref() != font_family
            || self.device_pixel_ratio != device_pixel_ratio
            || self.label_by_text.len() > MAXIMUM_CACHED_LABEL_COUNT
        {
            self.label_by_text.clear();
            self.font_family = font_family.map(str::to_owned);
            self.device_pixel_ratio = device_pixel_ratio;
        }
        for text in texts {
            if self.label_by_text.contains_key(text) {
                continue;
            }
            let label = shaper.shape_overlay_label(font_family, LABEL_FONT_SIZE_IN_POINTS, device_pixel_ratio, text);
            self.label_by_text.insert(text.clone(), label);
        }
    }

    fn label_of_text(&self, text: &str) -> Option<&OverlayLabel> {
        self.label_by_text.get(text)?.as_ref()
    }
}

/// The overlay as of the frame painted last, and how it is drawn over that frame.
pub struct LayerOverlayFrame {
    pub model: LayerOverlayModel,
    pub records: OverlayRecords,
}

/// What the overlay keeps while it is switched on: how often each tile was rastered, and why last, and what the
/// compositor layers window was told last.
#[derive(Default)]
pub struct LayerOverlay {
    state_from_ui: CompositorLayersOverlayState,
    raster_history_of_tiles_by_layer: HashMap<LayerIdentity, BTreeMap<TileCoord, RasterHistoryOfTile>>,
    shaped_labels: ShapedLabelCache,
    latest_frame: Option<LayerOverlayFrame>,
    publish_time_and_description_published_last: Option<(MonotonicTime, CompositorLayersDescription)>,
    unpublished_description_waits_until: Option<MonotonicTime>,
}

impl LayerOverlay {
    /// Takes in that a frame painted at `painted_at` rastered the tile at `coord` of the layer, or painted it straight
    /// into the frame.
    pub fn record_raster(
        &mut self,
        layer_identity: LayerIdentity,
        coord: TileCoord,
        cause: RasterCause,
        rect_in_layer_space: IntRect,
        painted_at: MonotonicTime,
    ) {
        let history = self
            .raster_history_of_tiles_by_layer
            .entry(layer_identity)
            .or_default()
            .entry(coord)
            .or_insert(RasterHistoryOfTile {
                raster_count_since_overlay_switched_on: 0,
                last_cause: cause,
                last_rect_in_layer_space: rect_in_layer_space,
                last_rastered_at: painted_at,
            });
        if cause != RasterCause::PaintedStraightIntoTheFrame {
            history.raster_count_since_overlay_switched_on += 1;
        }
        history.last_cause = cause;
        history.last_rect_in_layer_space = rect_in_layer_space;
        history.last_rastered_at = painted_at;
    }

    pub fn latest_frame(&self) -> Option<&LayerOverlayFrame> {
        self.latest_frame.as_ref()
    }

    /// Works out the overlay of a frame from the tiles rastered so far, and records how it is drawn.
    pub fn update_with_frame(
        &mut self,
        frame: FrameOfLayerOverlay,
        device_pixel_ratio: f64,
        shaper: &dyn OverlayLabelShaper,
    ) -> &LayerOverlayFrame {
        self.raster_history_of_tiles_by_layer
            .retain(|identity, _| frame.layers.iter().any(|layer| layer.identity == *identity));
        let model = self.model_of_frame(frame);
        let records = self.record_drawing_of_model(&model, device_pixel_ratio, shaper);
        self.latest_frame.insert(LayerOverlayFrame { model, records })
    }

    pub fn hides_layer(&self, identity: &LayerIdentity) -> bool {
        self.state_from_ui.hidden_layer_ids.contains(&id_of_layer(identity))
    }

    /// What the compositor layers window is to show of the frame painted last, unless it shows that already or was
    /// told what changed too short a time ago, when a later frame tells it.
    pub fn take_description_to_publish(&mut self, now: MonotonicTime) -> Option<CompositorLayersDescription> {
        let description = self.latest_frame.as_ref()?.model.description_for_layers_window();
        self.unpublished_description_waits_until = None;
        if let Some((published_at, published)) = &self.publish_time_and_description_published_last {
            if *published == description {
                return None;
            }
            let earliest_next_publish_time = *published_at + SHORTEST_TIME_BETWEEN_PUBLISHED_DESCRIPTIONS;
            if now < earliest_next_publish_time {
                self.unpublished_description_waits_until = Some(earliest_next_publish_time);
                return None;
            }
        }
        self.publish_time_and_description_published_last = Some((now, description.clone()));
        Some(description)
    }

    /// Until when frames are needed without anything else changing: while a highlight fades, and until the compositor
    /// layers window can be told what changed.
    pub fn needs_frames_until(&self) -> Option<MonotonicTime> {
        let highlights_fade_until = self
            .latest_frame
            .as_ref()
            .and_then(|frame| frame.model.highlights_fade_until);
        highlights_fade_until.max(self.unpublished_description_waits_until)
    }

    pub fn model_of_frame(&self, frame: FrameOfLayerOverlay) -> LayerOverlayModel {
        let painted_at = frame.painted_at;
        let mut highlights_fade_until: Option<MonotonicTime> = None;
        let layers = frame
            .layers
            .into_iter()
            .map(|layer| {
                let color = color_of_layer(&layer.identity);
                let content = match layer.content {
                    LayerOfFrameContent::Painted { held_tiles } => {
                        let bytes_held_by_tile_surfaces = held_tiles
                            .iter()
                            .filter(|tile| !tile.is_one_color)
                            .map(|tile| {
                                let held_part = tile.held_part_of_cell;
                                held_part.width.max(0) as u64 * held_part.height.max(0) as u64 * BYTES_PER_PIXEL
                            })
                            .sum();
                        let mut tiles: BTreeMap<TileCoord, ModelTile> = held_tiles
                            .into_iter()
                            .map(|tile| {
                                (
                                    tile.coord,
                                    ModelTile {
                                        coord: tile.coord,
                                        held_part_of_cell: Some(tile.held_part_of_cell),
                                        is_one_color: tile.is_one_color,
                                        raster_count_since_overlay_switched_on: 0,
                                        last_raster: None,
                                    },
                                )
                            })
                            .collect();
                        let raster_history_of_tiles = self.raster_history_of_tiles_by_layer.get(&layer.identity);
                        for (&coord, history) in raster_history_of_tiles.into_iter().flatten() {
                            let last_raster = LastRaster {
                                cause: history.last_cause,
                                rect_in_layer_space: history.last_rect_in_layer_space,
                                age: painted_at - history.last_rastered_at,
                            };
                            if last_raster.is_highlighted() {
                                let fades_at = history.last_rastered_at + RASTER_HIGHLIGHT_FADE_DURATION;
                                highlights_fade_until =
                                    Some(highlights_fade_until.map_or(fades_at, |until| until.max(fades_at)));
                            }
                            let shows_cell_holding_no_tile = last_raster.is_highlighted();
                            let tile = match tiles.entry(coord) {
                                btree_map::Entry::Occupied(entry) => entry.into_mut(),
                                btree_map::Entry::Vacant(entry) if shows_cell_holding_no_tile => {
                                    entry.insert(ModelTile {
                                        coord,
                                        held_part_of_cell: None,
                                        is_one_color: false,
                                        raster_count_since_overlay_switched_on: 0,
                                        last_raster: None,
                                    })
                                }
                                btree_map::Entry::Vacant(_) => continue,
                            };
                            tile.raster_count_since_overlay_switched_on =
                                history.raster_count_since_overlay_switched_on;
                            tile.last_raster = Some(last_raster);
                        }
                        ModelLayerContent::Painted {
                            tiles: tiles.into_values().collect(),
                            bytes_held_by_tile_surfaces,
                        }
                    }
                    LayerOfFrameContent::Media { reach, presentation } => {
                        ModelLayerContent::Media { reach, presentation }
                    }
                };
                ModelLayer {
                    id: id_of_layer(&layer.identity),
                    name: layer.name,
                    color,
                    hidden: self.hides_layer(&layer.identity),
                    space_to_frame: layer.space_to_frame,
                    composite_clip: layer.composite_clip,
                    content,
                }
            })
            .collect();
        LayerOverlayModel {
            viewport_rect: frame.viewport_rect,
            layers,
            why_list_is_one_layer: frame.why_list_is_one_layer,
            notes_of_layer_stack: frame.notes_of_layer_stack,
            work_counts_of_frame: frame.work_counts,
            highlights_fade_until,
        }
    }

    fn record_drawing_of_model(
        &mut self,
        model: &LayerOverlayModel,
        device_pixel_ratio: f64,
        shaper: &dyn OverlayLabelShaper,
    ) -> OverlayRecords {
        let mut label_texts: Vec<String> = Vec::new();
        for layer in model.layers.iter().filter(|layer| !layer.hidden) {
            label_texts.push(layer_label_text(layer));
            if let ModelLayerContent::Painted { tiles, .. } = &layer.content {
                label_texts.extend(
                    tiles
                        .iter()
                        .filter(|tile| tile.held_part_of_cell.is_some())
                        .map(tile_label_text),
                );
            }
        }
        self.shaped_labels.shape_labels_not_cached_yet(
            &label_texts,
            self.state_from_ui.font_family.as_deref(),
            device_pixel_ratio,
            shaper,
        );
        let drawing = OverlayDrawing {
            shaped_labels: &self.shaped_labels,
            scale: device_pixel_ratio.max(1.0) as f32,
        };
        OverlayRecords::record(|recorder| drawing.draw(recorder, model))
    }
}

struct OverlayDrawing<'a> {
    shaped_labels: &'a ShapedLabelCache,
    scale: f32,
}

impl OverlayDrawing<'_> {
    fn device_pixels(&self, logical_pixels: f32) -> i32 {
        (logical_pixels * self.scale).round().max(1.0) as i32
    }

    fn draw(&self, recorder: &mut OverlayRecorder<'_>, model: &LayerOverlayModel) {
        let viewport_rect = model.viewport_rect;
        let shown_layers = || model.layers.iter().filter(|layer| !layer.hidden);
        for layer in shown_layers() {
            self.draw_layer(recorder, layer, viewport_rect);
        }
        let mut rects_of_drawn_labels = Vec::new();
        for layer in shown_layers().rev() {
            self.draw_name_of_layer(recorder, layer, viewport_rect, &mut rects_of_drawn_labels);
        }
        for layer in shown_layers().rev() {
            self.draw_tile_labels_of_layer(recorder, layer, viewport_rect, &mut rects_of_drawn_labels);
        }
    }

    fn fill_within(recorder: &mut OverlayRecorder<'_>, rect: IntRect, color: Color, within: IntRect) {
        let rect = rect.intersected(within);
        if !rect.is_empty() {
            recorder.fill_rect(rect, color);
        }
    }

    fn stroke_within(recorder: &mut OverlayRecorder<'_>, rect: IntRect, thickness: i32, color: Color, within: IntRect) {
        if rect.is_empty() {
            return;
        }
        let thickness = thickness.min(rect.width / 2).min(rect.height / 2).max(1);
        let inner_height = rect.height - 2 * thickness;
        for edge in [
            IntRect::new(rect.x, rect.y, rect.width, thickness),
            IntRect::new(rect.x, rect.bottom() - thickness, rect.width, thickness),
            IntRect::new(rect.x, rect.y + thickness, thickness, inner_height),
            IntRect::new(rect.right() - thickness, rect.y + thickness, thickness, inner_height),
        ] {
            Self::fill_within(recorder, edge, color, within);
        }
    }

    fn draw_layer(&self, recorder: &mut OverlayRecorder<'_>, layer: &ModelLayer, viewport_rect: IntRect) {
        let visible_rect = layer.visible_rect_in_frame(viewport_rect);
        if visible_rect.is_empty() {
            return;
        }
        if let ModelLayerContent::Painted { tiles, .. } = &layer.content {
            for tile in tiles {
                let Some(last_raster) = tile.last_raster.filter(LastRaster::is_highlighted) else {
                    continue;
                };
                let alpha =
                    (f32::from(RASTER_HIGHLIGHT_ALPHA_BEFORE_FADING) * last_raster.highlight_opacity()).round() as u8;
                Self::fill_within(
                    recorder,
                    layer.rect_in_frame(last_raster.rect_in_layer_space),
                    last_raster.cause.highlight_color().with_alpha(alpha),
                    visible_rect,
                );
            }
            for tile in tiles.iter().filter(|tile| tile.is_one_color) {
                if let Some(held_part_of_cell) = tile.held_part_of_cell {
                    Self::fill_within(
                        recorder,
                        layer.rect_in_frame(held_part_of_cell),
                        layer.color.with_alpha(ONE_COLOR_TILE_TINT_ALPHA),
                        visible_rect,
                    );
                }
            }
            let grid_color = layer.color.with_alpha(150);
            for held_part_of_cell in tiles.iter().filter_map(|tile| tile.held_part_of_cell) {
                Self::stroke_within(
                    recorder,
                    layer.rect_in_frame(held_part_of_cell),
                    self.device_pixels(1.0),
                    grid_color,
                    visible_rect,
                );
            }
        }
        let bounds_in_frame = layer.rect_in_frame(layer.bounds_in_layer_space());
        let halo_thickness = self.device_pixels(1.0);
        Self::stroke_within(
            recorder,
            bounds_in_frame,
            self.device_pixels(2.0) + 2 * halo_thickness,
            OUTLINE_HALO_COLOR,
            visible_rect,
        );
        Self::stroke_within(
            recorder,
            bounds_in_frame.inflated(-2 * halo_thickness, -2 * halo_thickness),
            self.device_pixels(2.0),
            layer.color,
            visible_rect,
        );
    }

    fn label_size(&self, label: &OverlayLabel, has_swatch: bool) -> IntSize {
        let text_height = (label.descent - label.ascent).ceil() as i32;
        let swatch_width = if has_swatch {
            text_height + self.device_pixels(3.0)
        } else {
            0
        };
        IntSize {
            width: label.width.ceil() as i32 + swatch_width + 2 * self.device_pixels(3.0),
            height: text_height + 2 * self.device_pixels(1.0),
        }
    }

    fn draw_label(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        label: &OverlayLabel,
        top_left: IntPoint,
        swatch: Option<Color>,
        background: Option<Color>,
    ) {
        let size = self.label_size(label, swatch.is_some());
        let text_height = (label.descent - label.ascent).ceil() as i32;
        if let Some(background) = background {
            recorder.fill_rect(
                IntRect::new(top_left.x, top_left.y, size.width, size.height),
                background,
            );
        }
        let mut text_x = top_left.x + self.device_pixels(3.0);
        if let Some(swatch) = swatch {
            let swatch_rect = IntRect::new(text_x, top_left.y + self.device_pixels(1.0), text_height, text_height);
            recorder.fill_rect(swatch_rect, swatch);
            text_x += text_height + self.device_pixels(3.0);
        }
        let baseline = FloatPoint {
            x: text_x as f32,
            y: (top_left.y + self.device_pixels(1.0)) as f32 - label.ascent,
        };
        recorder.draw_label(label, baseline, LABEL_TEXT_COLOR, self.scale);
    }

    fn draw_name_of_layer(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        layer: &ModelLayer,
        viewport_rect: IntRect,
        rects_of_drawn_labels: &mut Vec<IntRect>,
    ) {
        let visible_rect = layer.visible_rect_in_frame(viewport_rect);
        let visible_bounds = layer
            .rect_in_frame(layer.bounds_in_layer_space())
            .intersected(visible_rect);
        let Some(label) = self.shaped_labels.label_of_text(&layer_label_text(layer)) else {
            return;
        };
        if visible_bounds.is_empty() {
            return;
        }
        let size = self.label_size(label, true);
        let inset = self.device_pixels(3.0);
        let mut rect = IntRect::new(
            visible_bounds.x + inset,
            visible_bounds.y + inset,
            size.width,
            size.height,
        );
        while rects_of_drawn_labels
            .iter()
            .any(|other| !other.intersected(rect).is_empty())
        {
            rect.y += size.height + self.device_pixels(1.0);
        }
        if !visible_bounds.contains_point(IntPoint { x: rect.x, y: rect.y }) {
            return;
        }
        rects_of_drawn_labels.push(rect);
        recorder.clipped_to(visible_rect, |recorder| {
            self.draw_label(
                recorder,
                label,
                IntPoint { x: rect.x, y: rect.y },
                Some(layer.color),
                Some(LABEL_BACKGROUND_COLOR),
            );
        });
    }

    fn draw_tile_labels_of_layer(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        layer: &ModelLayer,
        viewport_rect: IntRect,
        rects_of_drawn_labels: &mut Vec<IntRect>,
    ) {
        let ModelLayerContent::Painted { tiles, .. } = &layer.content else {
            return;
        };
        let visible_rect = layer.visible_rect_in_frame(viewport_rect);
        let inset = self.device_pixels(3.0);
        recorder.clipped_to(visible_rect, |recorder| {
            for tile in tiles {
                let Some(held_part_of_cell) = tile.held_part_of_cell else {
                    continue;
                };
                let held_rect_in_frame = layer.rect_in_frame(held_part_of_cell);
                let Some(label) = self.shaped_labels.label_of_text(&tile_label_text(tile)) else {
                    continue;
                };
                let size = self.label_size(label, false);
                let rect = IntRect::new(
                    held_rect_in_frame.right() - size.width - inset,
                    held_rect_in_frame.bottom() - size.height - inset,
                    size.width,
                    size.height,
                );
                if !visible_rect.contains_rect(rect)
                    || rects_of_drawn_labels
                        .iter()
                        .any(|other| !other.intersected(rect).is_empty())
                {
                    continue;
                }
                rects_of_drawn_labels.push(rect);
                self.draw_label(
                    recorder,
                    label,
                    IntPoint { x: rect.x, y: rect.y },
                    None,
                    Some(LABEL_BACKGROUND_COLOR),
                );
            }
        });
    }
}

/// The overlay while it is switched on, and whether the frame painted last showed it, which the frame after then
/// erases.
#[derive(Default)]
pub struct LayerOverlaySwitch {
    overlay: Option<LayerOverlay>,
    shown_by_latest_frame: bool,
}

impl LayerOverlaySwitch {
    /// Switches the overlay on or off from the frame on, dropping all it kept as it is switched off.
    pub fn switch_for_frame(&mut self, state_from_ui: Option<&CompositorLayersOverlayState>) {
        let Some(state_from_ui) = state_from_ui else {
            self.overlay = None;
            return;
        };
        let overlay = self.overlay.get_or_insert_default();
        if overlay.state_from_ui != *state_from_ui {
            overlay.state_from_ui = state_from_ui.clone();
        }
    }

    /// Whether the frame repaints all of the viewport, to draw the overlay over it or to erase it.
    pub fn repaints_everything(&self) -> bool {
        self.overlay.is_some() || self.shown_by_latest_frame
    }

    pub fn did_paint_frame(&mut self) {
        self.shown_by_latest_frame = self.overlay.is_some();
    }

    pub fn overlay(&self) -> Option<&LayerOverlay> {
        self.overlay.as_ref()
    }

    pub fn overlay_mut(&mut self) -> Option<&mut LayerOverlay> {
        self.overlay.as_mut()
    }

    pub fn latest_frame(&self) -> Option<&LayerOverlayFrame> {
        self.overlay.as_ref()?.latest_frame()
    }

    pub fn needs_frames_until(&self) -> Option<MonotonicTime> {
        self.overlay.as_ref()?.needs_frames_until()
    }

    pub fn hides_layer(&self, identity: &LayerIdentity) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|overlay| overlay.hides_layer(identity))
    }
}
