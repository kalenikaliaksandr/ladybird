/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The compositor layers overlay: over each frame, the layers the frame is composited from, the tiles each of them holds
//! and how often each was rastered since the overlay was switched on, and the tiles rastered lately, highlighted by why
//! they were and fading out. What it shows of a frame is worked out into a plain model first, which is both drawn and
//! written out as text.

use crate::overlays::{OverlayLabel, OverlayLabelShaper, OverlayRecorder, OverlayRecords};
use libcompositing_rust::display_list::layers::LayerKey;
use libcompositing_rust::display_list::tiles::TileCoord;
use libcompositing_rust::time::{Duration, MonotonicTime};
use libgfx_rust::{Color, FloatPoint, IntPoint, IntRect, IntSize};
use std::collections::{BTreeMap, HashMap, btree_map};
use std::hash::{DefaultHasher, Hash, Hasher};

pub const RASTER_HIGHLIGHT_FADE: Duration = Duration::from_milliseconds(500);
const MAXIMUM_CACHED_LABEL_COUNT: usize = 4096;
const BYTES_PER_PIXEL: u64 = 4;
const HIGHLIGHT_ALPHA: u8 = 115;
const LABEL_BACKGROUND_COLOR: Color = Color::from_rgba(16, 18, 24, 200);
const LABEL_TEXT_COLOR: Color = Color::from_rgba(255, 255, 255, 255);
const PANEL_BACKGROUND_COLOR: Color = Color::from_rgba(16, 18, 24, 175);
// Drawn around each outline, so that it shows over light and dark pages alike.
const OUTLINE_HALO_COLOR: Color = Color::from_rgba(0, 0, 0, 150);
const BACKDROP_LAYER_TEXT: &str = "filters what is composited below it";

/// Why a tile was rastered, as the compositor tells the cases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterCause {
    /// The layer held no tile at the cell.
    NewlyShown,
    /// The tile held the pixels of another key.
    KeyChanged,
    /// Damage that a frame was made to repaint reached the tile, or rastering the tile failed before.
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
        RasterCause::KeyChanged,
        RasterCause::HeldPixelsInvalidated,
        RasterCause::HeldLessThanNeeded,
        RasterCause::HeldOnAnotherBackend,
        RasterCause::PaintedStraightIntoTheFrame,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RasterCause::NewlyShown => "newly shown",
            RasterCause::KeyChanged => "key changed",
            RasterCause::HeldPixelsInvalidated => "held pixels invalidated",
            RasterCause::HeldLessThanNeeded => "held less than needed",
            RasterCause::HeldOnAnotherBackend => "held on another backend",
            RasterCause::PaintedStraightIntoTheFrame => "painted straight into the frame",
        }
    }

    pub fn color(self) -> Color {
        match self {
            RasterCause::NewlyShown => Color::from_rgb(34, 197, 94),
            RasterCause::KeyChanged => Color::from_rgb(239, 68, 68),
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

/// What a frame did with its layers, besides rastering tiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCounts {
    pub tiles_rastered: u64,
    pub tiles_binned: u64,
    pub chunks_placed: u64,
    pub stack_changed_shape: bool,
}

/// A layer as a frame composites it, which the overlay is worked out from.
pub struct LayerOfFrame {
    pub key: LayerKey,
    /// What the display list dump calls the layer.
    pub name: String,
    /// From the raster coordinates of the layer's space into those of the frame.
    pub composite_offset: IntPoint,
    /// In the raster coordinates of the frame.
    pub composite_clip: Option<IntRect>,
    pub content: LayerOfFrameContent,
}

pub enum LayerOfFrameContent {
    /// The tiles the layer holds, with the part of its cell each holds.
    Painted { held_tiles: Vec<(TileCoord, IntRect)> },
    Media {
        region: IntRect,
        presentation: MediaPresentation,
    },
    /// What a backdrop layer reads and draws, in the raster coordinates of its space.
    Backdrop { region: IntRect },
}

/// What the overlay is worked out from for a frame.
pub struct FrameOfLayerOverlay {
    pub viewport_rect: IntRect,
    pub painted_at: MonotonicTime,
    /// From the base layer up.
    pub layers: Vec<LayerOfFrame>,
    pub single_layer_reason: Option<String>,
    pub media_and_backdrop_filters_without_layers: Vec<String>,
    pub counts: FrameCounts,
}

/// The last time a tile was rastered, as the overlay shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastRaster {
    pub cause: RasterCause,
    /// In the raster coordinates of the layer's space.
    pub rect: IntRect,
    pub age: Duration,
}

impl LastRaster {
    pub fn is_highlighted(&self) -> bool {
        self.age < RASTER_HIGHLIGHT_FADE
    }

    pub fn highlight_opacity(&self) -> f32 {
        if !self.is_highlighted() {
            return 0.0;
        }
        1.0 - self.age.nanoseconds() as f32 / RASTER_HIGHLIGHT_FADE.nanoseconds() as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelTile {
    pub coord: TileCoord,
    /// Where the layer holds no tile at the cell, which a tile painted straight into the frame shows, none.
    pub held_rect: Option<IntRect>,
    /// Since the overlay was switched on.
    pub raster_count: u32,
    pub last_raster: Option<LastRaster>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelLayerContent {
    Painted {
        tiles: Vec<ModelTile>,
        bytes_held: u64,
    },
    Media {
        region: IntRect,
        presentation: MediaPresentation,
    },
    Backdrop {
        region: IntRect,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelLayer {
    pub name: String,
    pub color: Color,
    pub composite_offset: IntPoint,
    pub composite_clip: Option<IntRect>,
    pub content: ModelLayerContent,
}

impl ModelLayer {
    // Where the layer can show, in the raster coordinates of the frame.
    fn visible_rect(&self, viewport_rect: IntRect) -> IntRect {
        self.composite_clip
            .map_or(viewport_rect, |clip| viewport_rect.intersected(clip))
    }

    fn in_frame(&self, rect: IntRect) -> IntRect {
        translated(rect, self.composite_offset)
    }

    // What the layer covers in the raster coordinates of its space: the tiles it holds, or its region.
    fn bounds(&self) -> IntRect {
        match &self.content {
            ModelLayerContent::Painted { tiles, .. } => tiles
                .iter()
                .filter_map(|tile| tile.held_rect)
                .fold(IntRect::default(), IntRect::united),
            ModelLayerContent::Media { region, .. } | ModelLayerContent::Backdrop { region } => *region,
        }
    }

    fn short_name(&self) -> &str {
        self.name.split_once(' ').map_or(self.name.as_str(), |(index, _)| index)
    }
}

/// What the overlay shows over a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerOverlayModel {
    pub viewport_rect: IntRect,
    /// From the base layer up.
    pub layers: Vec<ModelLayer>,
    pub single_layer_reason: Option<String>,
    pub media_and_backdrop_filters_without_layers: Vec<String>,
    pub last_frame: FrameCounts,
    /// Until when some tile stays highlighted.
    pub highlights_fade_until: Option<MonotonicTime>,
}

fn plural(count: u64, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn amount_of_memory(bytes: u64) -> String {
    const MEBIBYTE: u64 = 1024 * 1024;
    if bytes < MEBIBYTE {
        return format!("{} KiB", bytes.div_ceil(1024));
    }
    format!("{:.1} MiB", bytes as f64 / MEBIBYTE as f64)
}

fn rect_text(rect: IntRect) -> String {
    format!("[{},{} {}x{}]", rect.x, rect.y, rect.width, rect.height)
}

fn translated(rect: IntRect, offset: IntPoint) -> IntRect {
    IntRect::new(rect.x + offset.x, rect.y + offset.y, rect.width, rect.height)
}

impl LayerOverlayModel {
    fn layer_row_text(layer: &ModelLayer) -> String {
        match &layer.content {
            ModelLayerContent::Painted { tiles, bytes_held } => {
                let held = tiles.iter().filter(|tile| tile.held_rect.is_some()).count() as u64;
                format!(
                    "{}: {}, {}",
                    layer.name,
                    plural(held, "tile", "tiles"),
                    amount_of_memory(*bytes_held)
                )
            }
            ModelLayerContent::Media { presentation, .. } => format!("{}: {presentation}", layer.name),
            ModelLayerContent::Backdrop { .. } => format!("{}: {BACKDROP_LAYER_TEXT}", layer.name),
        }
    }

    fn last_frame_text(&self) -> String {
        let counts = self.last_frame;
        format!(
            "last frame: {} rastered, {} binned, {} placed, {}",
            plural(counts.tiles_rastered, "tile", "tiles"),
            plural(counts.tiles_binned, "tile", "tiles"),
            plural(counts.chunks_placed, "chunk", "chunks"),
            if counts.stack_changed_shape {
                "the stack changed shape"
            } else {
                "the stack kept its shape"
            }
        )
    }

    fn panel_rows(&self) -> Vec<PanelRow> {
        let mut rows: Vec<PanelRow> = self
            .layers
            .iter()
            .map(|layer| vec![(Some(layer.color), Self::layer_row_text(layer))])
            .collect();
        if let Some(reason) = &self.single_layer_reason {
            rows.push(vec![(None, format!("single layer: {reason}"))]);
        }
        rows.extend(
            self.media_and_backdrop_filters_without_layers
                .iter()
                .map(|name| vec![(None, name.clone())]),
        );
        rows.push(vec![(None, self.last_frame_text())]);
        for causes in RasterCause::ALL.chunks(3) {
            rows.push(
                causes
                    .iter()
                    .map(|cause| (Some(cause.color()), cause.name().to_owned()))
                    .collect(),
            );
        }
        rows
    }
}

// A row of the panel: labels side by side, each with the color of the swatch in front of it, if it has one.
type PanelRow = Vec<(Option<Color>, String)>;

fn tile_label_text(tile: &ModelTile) -> String {
    format!("{},{} x{}", tile.coord.column, tile.coord.row, tile.raster_count)
}

fn layer_label_text(layer: &ModelLayer) -> String {
    match &layer.content {
        ModelLayerContent::Painted { .. } => layer.name.clone(),
        ModelLayerContent::Media { presentation, .. } => format!("{}: {presentation}", layer.name),
        ModelLayerContent::Backdrop { .. } => format!("{}: {BACKDROP_LAYER_TEXT}", layer.name),
    }
}

/// One line per layer from the base layer up, followed by a line per tile of it that was rastered since the overlay was
/// switched on, then the media without layers, why the list is a single layer if it is, what the last frame did, and
/// which tiles are highlighted. Only the last two depend on when the frame was painted.
impl std::fmt::Display for LayerOverlayModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for layer in &self.layers {
            writeln!(formatter, "{}", Self::layer_row_text(layer))?;
            let ModelLayerContent::Painted { tiles, .. } = &layer.content else {
                continue;
            };
            for tile in tiles {
                let Some(last_raster) = tile.last_raster else {
                    continue;
                };
                writeln!(
                    formatter,
                    "    tile {},{}: rastered {}, last {} over {}",
                    tile.coord.column,
                    tile.coord.row,
                    plural(u64::from(tile.raster_count), "time", "times"),
                    last_raster.cause.name(),
                    rect_text(last_raster.rect)
                )?;
            }
        }
        for name in &self.media_and_backdrop_filters_without_layers {
            writeln!(formatter, "{name}")?;
        }
        if let Some(reason) = &self.single_layer_reason {
            writeln!(formatter, "single layer: {reason}")?;
        }
        writeln!(formatter, "{}", self.last_frame_text())?;
        for layer in &self.layers {
            let ModelLayerContent::Painted { tiles, .. } = &layer.content else {
                continue;
            };
            for tile in tiles {
                if let Some(last_raster) = tile.last_raster.filter(LastRaster::is_highlighted) {
                    writeln!(
                        formatter,
                        "highlighted: {} tile {},{}: {}, {} ms old",
                        layer.short_name(),
                        tile.coord.column,
                        tile.coord.row,
                        last_raster.cause.name(),
                        last_raster.age.nanoseconds() / 1_000_000
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// A color of the layer's own, which stays as long as its key does, wherever the layer is in the stack.
pub fn color_of_layer(key: &LayerKey) -> Color {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let hash = hasher.finish();
    let hue = (hash % 360) as f32;
    // Two lightnesses tell apart layers of hues alike, and both stay readable over light and dark pages.
    let lightness = if hash & (1 << 40) == 0 { 0.5 } else { 0.62 };
    color_of_hue(hue, 0.9, lightness)
}

fn color_of_hue(hue_in_degrees: f32, saturation: f32, lightness: f32) -> Color {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue_in_degrees / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let lightest = lightness - chroma / 2.0;
    let channel = |value: f32| ((value + lightest) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb(channel(red), channel(green), channel(blue))
}

#[derive(Clone, Copy, Debug)]
struct TileRasterRecord {
    raster_count: u32,
    last_cause: RasterCause,
    last_rect: IntRect,
    last_rastered_at: MonotonicTime,
}

// The labels shaped for earlier frames, which most later ones show again.
#[derive(Default)]
struct LabelCache {
    device_pixel_ratio: f64,
    labels: HashMap<String, Option<OverlayLabel>>,
}

impl LabelCache {
    fn shape_missing(&mut self, texts: &[String], device_pixel_ratio: f64, shaper: &dyn OverlayLabelShaper) {
        if self.device_pixel_ratio != device_pixel_ratio || self.labels.len() > MAXIMUM_CACHED_LABEL_COUNT {
            self.labels.clear();
            self.device_pixel_ratio = device_pixel_ratio;
        }
        for text in texts {
            if self.labels.contains_key(text) {
                continue;
            }
            let label = shaper.shape_overlay_label(None, device_pixel_ratio, text);
            self.labels.insert(text.clone(), label);
        }
    }

    fn get(&self, text: &str) -> Option<&OverlayLabel> {
        self.labels.get(text)?.as_ref()
    }
}

/// The overlay as of the frame painted last, and how it is drawn over that frame.
pub struct LayerOverlayFrame {
    pub model: LayerOverlayModel,
    pub records: OverlayRecords,
}

/// What the overlay keeps while it is switched on: how often each tile was rastered, and why last.
#[derive(Default)]
pub struct LayerOverlay {
    tile_records: HashMap<LayerKey, BTreeMap<TileCoord, TileRasterRecord>>,
    labels: LabelCache,
    latest_frame: Option<LayerOverlayFrame>,
}

impl LayerOverlay {
    /// Takes in that a frame painted at `at` rastered the tile at `coord` of the layer over `rect` of the raster
    /// coordinates of the layer's space, or painted it straight into the frame.
    pub fn record_raster(
        &mut self,
        layer: LayerKey,
        coord: TileCoord,
        cause: RasterCause,
        rect: IntRect,
        at: MonotonicTime,
    ) {
        let record = self
            .tile_records
            .entry(layer)
            .or_default()
            .entry(coord)
            .or_insert(TileRasterRecord {
                raster_count: 0,
                last_cause: cause,
                last_rect: rect,
                last_rastered_at: at,
            });
        if cause != RasterCause::PaintedStraightIntoTheFrame {
            record.raster_count += 1;
        }
        record.last_cause = cause;
        record.last_rect = rect;
        record.last_rastered_at = at;
    }

    pub fn latest_frame(&self) -> Option<&LayerOverlayFrame> {
        self.latest_frame.as_ref()
    }

    /// Works out the overlay of a frame from the tiles rastered so far, and records how it is drawn.
    pub fn take_frame(
        &mut self,
        frame: FrameOfLayerOverlay,
        device_pixel_ratio: f64,
        shaper: &dyn OverlayLabelShaper,
    ) -> &LayerOverlayFrame {
        self.tile_records
            .retain(|key, _| frame.layers.iter().any(|layer| layer.key == *key));
        let model = self.model_of_frame(frame);
        let records = self.record_drawing(&model, device_pixel_ratio, shaper);
        self.latest_frame.insert(LayerOverlayFrame { model, records })
    }

    pub fn model_of_frame(&self, frame: FrameOfLayerOverlay) -> LayerOverlayModel {
        let painted_at = frame.painted_at;
        let mut highlights_fade_until: Option<MonotonicTime> = None;
        let layers = frame
            .layers
            .into_iter()
            .map(|layer| {
                let color = color_of_layer(&layer.key);
                let content = match layer.content {
                    LayerOfFrameContent::Painted { held_tiles } => {
                        let bytes_held = held_tiles
                            .iter()
                            .map(|(_, rect)| rect.width.max(0) as u64 * rect.height.max(0) as u64 * BYTES_PER_PIXEL)
                            .sum();
                        let mut tiles: BTreeMap<TileCoord, ModelTile> = held_tiles
                            .into_iter()
                            .map(|(coord, rect)| {
                                (
                                    coord,
                                    ModelTile {
                                        coord,
                                        held_rect: Some(rect),
                                        raster_count: 0,
                                        last_raster: None,
                                    },
                                )
                            })
                            .collect();
                        for (&coord, record) in self.tile_records.get(&layer.key).into_iter().flatten() {
                            let last_raster = LastRaster {
                                cause: record.last_cause,
                                rect: record.last_rect,
                                age: painted_at - record.last_rastered_at,
                            };
                            if last_raster.is_highlighted() {
                                let fades_at = record.last_rastered_at + RASTER_HIGHLIGHT_FADE;
                                highlights_fade_until =
                                    Some(highlights_fade_until.map_or(fades_at, |t| t.max(fades_at)));
                            }
                            // A cell the layer holds no tile at shows only while it is highlighted.
                            let tile = match tiles.entry(coord) {
                                btree_map::Entry::Occupied(entry) => entry.into_mut(),
                                btree_map::Entry::Vacant(entry) if last_raster.is_highlighted() => {
                                    entry.insert(ModelTile {
                                        coord,
                                        held_rect: None,
                                        raster_count: 0,
                                        last_raster: None,
                                    })
                                }
                                btree_map::Entry::Vacant(_) => continue,
                            };
                            tile.raster_count = record.raster_count;
                            tile.last_raster = Some(last_raster);
                        }
                        ModelLayerContent::Painted {
                            tiles: tiles.into_values().collect(),
                            bytes_held,
                        }
                    }
                    LayerOfFrameContent::Media { region, presentation } => {
                        ModelLayerContent::Media { region, presentation }
                    }
                    LayerOfFrameContent::Backdrop { region } => ModelLayerContent::Backdrop { region },
                };
                ModelLayer {
                    name: layer.name,
                    color,
                    composite_offset: layer.composite_offset,
                    composite_clip: layer.composite_clip,
                    content,
                }
            })
            .collect();
        LayerOverlayModel {
            viewport_rect: frame.viewport_rect,
            layers,
            single_layer_reason: frame.single_layer_reason,
            media_and_backdrop_filters_without_layers: frame.media_and_backdrop_filters_without_layers,
            last_frame: frame.counts,
            highlights_fade_until,
        }
    }

    fn record_drawing(
        &mut self,
        model: &LayerOverlayModel,
        device_pixel_ratio: f64,
        shaper: &dyn OverlayLabelShaper,
    ) -> OverlayRecords {
        let mut texts: Vec<String> = Vec::new();
        for layer in &model.layers {
            texts.push(layer_label_text(layer));
            if let ModelLayerContent::Painted { tiles, .. } = &layer.content {
                texts.extend(
                    tiles
                        .iter()
                        .filter(|tile| tile.held_rect.is_some())
                        .map(tile_label_text),
                );
            }
        }
        let panel_rows = model.panel_rows();
        texts.extend(panel_rows.iter().flatten().map(|(_, text)| text.clone()));
        self.labels.shape_missing(&texts, device_pixel_ratio, shaper);
        let drawing = Drawing {
            labels: &self.labels,
            scale: device_pixel_ratio.max(1.0) as f32,
        };
        OverlayRecords::record(|recorder| drawing.draw(recorder, model, &panel_rows))
    }
}

struct Drawing<'a> {
    labels: &'a LabelCache,
    scale: f32,
}

impl Drawing<'_> {
    fn length(&self, length: f32) -> i32 {
        (length * self.scale).round().max(1.0) as i32
    }

    fn draw(&self, recorder: &mut OverlayRecorder<'_>, model: &LayerOverlayModel, panel_rows: &[PanelRow]) {
        let viewport_rect = model.viewport_rect;
        for layer in &model.layers {
            self.draw_layer(recorder, layer, viewport_rect);
        }
        // Layers of one space often hold tiles of one grid, so a label goes only where no other label is.
        let mut labeled = Vec::new();
        for layer in model.layers.iter().rev() {
            self.draw_name_of_layer(recorder, layer, viewport_rect, &mut labeled);
        }
        for layer in model.layers.iter().rev() {
            self.draw_tile_labels_of_layer(recorder, layer, viewport_rect, &mut labeled);
        }
        self.draw_panel(recorder, panel_rows, viewport_rect);
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
        let visible = layer.visible_rect(viewport_rect);
        if visible.is_empty() {
            return;
        }
        if let ModelLayerContent::Painted { tiles, .. } = &layer.content {
            for tile in tiles {
                let Some(last_raster) = tile.last_raster.filter(LastRaster::is_highlighted) else {
                    continue;
                };
                let alpha = (f32::from(HIGHLIGHT_ALPHA) * last_raster.highlight_opacity()).round() as u8;
                Self::fill_within(
                    recorder,
                    layer.in_frame(last_raster.rect),
                    last_raster.cause.color().with_alpha(alpha),
                    visible,
                );
            }
            let grid_color = layer.color.with_alpha(150);
            for held_rect in tiles.iter().filter_map(|tile| tile.held_rect) {
                Self::stroke_within(
                    recorder,
                    layer.in_frame(held_rect),
                    self.length(1.0),
                    grid_color,
                    visible,
                );
            }
        }
        let bounds = layer.in_frame(layer.bounds());
        let halo = self.length(1.0);
        Self::stroke_within(
            recorder,
            bounds,
            self.length(2.0) + 2 * halo,
            OUTLINE_HALO_COLOR,
            visible,
        );
        Self::stroke_within(
            recorder,
            bounds.inflated(-2 * halo, -2 * halo),
            self.length(2.0),
            layer.color,
            visible,
        );
    }

    fn label_size(&self, label: &OverlayLabel, has_swatch: bool) -> IntSize {
        let text_height = (label.descent - label.ascent).ceil() as i32;
        let swatch_width = if has_swatch { text_height + self.length(3.0) } else { 0 };
        IntSize {
            width: label.width.ceil() as i32 + swatch_width + 2 * self.length(3.0),
            height: text_height + 2 * self.length(1.0),
        }
    }

    // Draws the label with its top left corner at `origin`, on a background of its own.
    fn draw_label(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        label: &OverlayLabel,
        origin: IntPoint,
        swatch: Option<Color>,
        background: Option<Color>,
    ) {
        let size = self.label_size(label, swatch.is_some());
        let text_height = (label.descent - label.ascent).ceil() as i32;
        if let Some(background) = background {
            recorder.fill_rect(IntRect::new(origin.x, origin.y, size.width, size.height), background);
        }
        let mut text_x = origin.x + self.length(3.0);
        if let Some(swatch) = swatch {
            let swatch_rect = IntRect::new(text_x, origin.y + self.length(1.0), text_height, text_height);
            recorder.fill_rect(swatch_rect, swatch);
            text_x += text_height + self.length(3.0);
        }
        let baseline = FloatPoint {
            x: text_x as f32,
            y: (origin.y + self.length(1.0)) as f32 - label.ascent,
        };
        recorder.draw_label(label, baseline, LABEL_TEXT_COLOR, self.scale);
    }

    // Names the layer at the top left corner of what shows of it, or below the labels already there.
    fn draw_name_of_layer(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        layer: &ModelLayer,
        viewport_rect: IntRect,
        labeled: &mut Vec<IntRect>,
    ) {
        let visible = layer.visible_rect(viewport_rect);
        let visible_bounds = layer.in_frame(layer.bounds()).intersected(visible);
        let Some(label) = self.labels.get(&layer_label_text(layer)) else {
            return;
        };
        if visible_bounds.is_empty() {
            return;
        }
        let size = self.label_size(label, true);
        let inset = self.length(3.0);
        let mut rect = IntRect::new(
            visible_bounds.x + inset,
            visible_bounds.y + inset,
            size.width,
            size.height,
        );
        while labeled.iter().any(|other| !other.intersected(rect).is_empty()) {
            rect.y += size.height + self.length(1.0);
        }
        if !visible_bounds.contains_point(IntPoint { x: rect.x, y: rect.y }) {
            return;
        }
        labeled.push(rect);
        recorder.clipped_to(visible, |recorder| {
            self.draw_label(
                recorder,
                label,
                IntPoint { x: rect.x, y: rect.y },
                Some(layer.color),
                Some(LABEL_BACKGROUND_COLOR),
            );
        });
    }

    // Labels each tile the layer holds at the bottom right corner of what it holds, where no other label is.
    fn draw_tile_labels_of_layer(
        &self,
        recorder: &mut OverlayRecorder<'_>,
        layer: &ModelLayer,
        viewport_rect: IntRect,
        labeled: &mut Vec<IntRect>,
    ) {
        let ModelLayerContent::Painted { tiles, .. } = &layer.content else {
            return;
        };
        let visible = layer.visible_rect(viewport_rect);
        let inset = self.length(3.0);
        recorder.clipped_to(visible, |recorder| {
            for tile in tiles {
                let Some(held_rect) = tile.held_rect else {
                    continue;
                };
                let tile_rect = layer.in_frame(held_rect);
                let Some(label) = self.labels.get(&tile_label_text(tile)) else {
                    continue;
                };
                let size = self.label_size(label, false);
                let rect = IntRect::new(
                    tile_rect.right() - size.width - inset,
                    tile_rect.bottom() - size.height - inset,
                    size.width,
                    size.height,
                );
                if !visible.contains_rect(rect) || labeled.iter().any(|other| !other.intersected(rect).is_empty()) {
                    continue;
                }
                labeled.push(rect);
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

    fn draw_panel(&self, recorder: &mut OverlayRecorder<'_>, rows: &[PanelRow], viewport_rect: IntRect) {
        let gap = self.length(10.0);
        let rows: Vec<Vec<(Option<Color>, &OverlayLabel, IntSize)>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .filter_map(|(swatch, text)| {
                        let label = self.labels.get(text)?;
                        Some((*swatch, label, self.label_size(label, swatch.is_some())))
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        if rows.is_empty() {
            return;
        }
        let row_width = |row: &[(Option<Color>, &OverlayLabel, IntSize)]| {
            row.iter().map(|(_, _, size)| size.width).sum::<i32>() + gap * (row.len() as i32 - 1)
        };
        let row_height = |row: &[(Option<Color>, &OverlayLabel, IntSize)]| {
            row.iter().map(|(_, _, size)| size.height).max().unwrap_or(0)
        };
        let padding = self.length(6.0);
        let width = rows.iter().map(|row| row_width(row)).max().unwrap_or(0) + 2 * padding;
        let height = rows.iter().map(|row| row_height(row)).sum::<i32>() + 2 * padding;
        let margin = self.length(8.0);
        let panel = IntRect::new(
            viewport_rect.x + margin,
            viewport_rect.bottom() - height - margin,
            width,
            height,
        );
        recorder.clipped_to(viewport_rect, |recorder| {
            recorder.fill_rect(panel, PANEL_BACKGROUND_COLOR);
            let mut y = panel.y + padding;
            for row in &rows {
                let mut x = panel.x + padding;
                for (swatch, label, size) in row {
                    self.draw_label(recorder, label, IntPoint { x, y }, *swatch, None);
                    x += size.width + gap;
                }
                y += row_height(row);
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
    pub fn switch_for_frame(&mut self, shown: bool) {
        if !shown {
            self.overlay = None;
        } else if self.overlay.is_none() {
            self.overlay = Some(LayerOverlay::default());
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

    pub fn highlights_fade_until(&self) -> Option<MonotonicTime> {
        self.latest_frame()?.model.highlights_fade_until
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libcompositing_rust::display_list::layers::PlacementKey;
    use std::cell::Cell;

    fn painted_key(node: u64) -> LayerKey {
        LayerKey::Painted {
            node: PlacementKey(node),
            band: 0,
            above: None,
        }
    }

    fn at(milliseconds: i64) -> MonotonicTime {
        MonotonicTime::from_nanoseconds(milliseconds * 1_000_000)
    }

    fn painted_layer(key: LayerKey, name: &str, held: &[(i32, i32)]) -> LayerOfFrame {
        LayerOfFrame {
            key,
            name: name.into(),
            composite_offset: IntPoint::default(),
            composite_clip: None,
            content: LayerOfFrameContent::Painted {
                held_tiles: held
                    .iter()
                    .map(|&(column, row)| {
                        let coord = TileCoord::new(column, row);
                        (coord, coord.cell_rect())
                    })
                    .collect(),
            },
        }
    }

    fn frame(painted_at: MonotonicTime, layers: Vec<LayerOfFrame>) -> FrameOfLayerOverlay {
        FrameOfLayerOverlay {
            viewport_rect: IntRect::new(0, 0, 800, 600),
            painted_at,
            layers,
            single_layer_reason: None,
            media_and_backdrop_filters_without_layers: Vec::new(),
            counts: FrameCounts::default(),
        }
    }

    fn tiles_of(model: &LayerOverlayModel, layer: usize) -> &[ModelTile] {
        match &model.layers[layer].content {
            ModelLayerContent::Painted { tiles, .. } => tiles,
            ModelLayerContent::Media { .. } | ModelLayerContent::Backdrop { .. } => panic!("a painted layer"),
        }
    }

    fn highlighted(model: &LayerOverlayModel, layer: usize) -> Vec<(TileCoord, RasterCause)> {
        tiles_of(model, layer)
            .iter()
            .filter_map(|tile| {
                let last_raster = tile.last_raster.filter(LastRaster::is_highlighted)?;
                Some((tile.coord, last_raster.cause))
            })
            .collect()
    }

    #[derive(Default)]
    struct CountingShaper {
        shaped: Cell<u64>,
    }

    impl OverlayLabelShaper for CountingShaper {
        fn shape_overlay_label(&self, _: Option<&str>, _: f64, text: &str) -> Option<OverlayLabel> {
            self.shaped.set(self.shaped.get() + 1);
            Some(OverlayLabel {
                font_id: 1,
                glyph_ids: text.bytes().map(u32::from).collect(),
                glyph_x_positions: (0..text.len()).map(|index| index as f32 * 7.0).collect(),
                width: text.len() as f32 * 7.0,
                ascent: -10.0,
                descent: 3.0,
                font_ascent: 10.0,
            })
        }
    }

    #[test]
    fn only_the_tiles_a_frame_rastered_are_highlighted_with_why_they_were() {
        let key = painted_key(1);
        let mut overlay = LayerOverlay::default();
        overlay.record_raster(
            key,
            TileCoord::new(1, 0),
            RasterCause::KeyChanged,
            TileCoord::new(1, 0).cell_rect(),
            at(1000),
        );
        overlay.record_raster(
            key,
            TileCoord::new(0, 2),
            RasterCause::NewlyShown,
            IntRect::new(0, 512, 100, 40),
            at(1000),
        );
        let model = overlay.model_of_frame(frame(
            at(1000),
            vec![painted_layer(key, "[L0] base", &[(0, 0), (1, 0), (0, 2)])],
        ));
        assert_eq!(
            highlighted(&model, 0),
            [
                (TileCoord::new(1, 0), RasterCause::KeyChanged),
                (TileCoord::new(0, 2), RasterCause::NewlyShown)
            ]
        );
        let newly_shown = &tiles_of(&model, 0)[2];
        assert_eq!(newly_shown.last_raster.unwrap().rect, IntRect::new(0, 512, 100, 40));
        assert_eq!(tiles_of(&model, 0)[0].last_raster, None);
        assert_eq!(model.highlights_fade_until, Some(at(1500)));
    }

    #[test]
    fn highlights_fade_over_half_a_second_and_are_gone_after() {
        let key = painted_key(1);
        let mut overlay = LayerOverlay::default();
        overlay.record_raster(
            key,
            TileCoord::new(0, 0),
            RasterCause::KeyChanged,
            TileCoord::new(0, 0).cell_rect(),
            at(1000),
        );
        let opacity_at = |time: i64| {
            let model = overlay.model_of_frame(frame(at(time), vec![painted_layer(key, "[L0] base", &[(0, 0)])]));
            tiles_of(&model, 0)[0].last_raster.unwrap().highlight_opacity()
        };
        assert_eq!(opacity_at(1000), 1.0);
        assert!((opacity_at(1250) - 0.5).abs() < 0.001);
        assert_eq!(opacity_at(1500), 0.0);
        let model = overlay.model_of_frame(frame(at(1500), vec![painted_layer(key, "[L0] base", &[(0, 0)])]));
        assert!(highlighted(&model, 0).is_empty());
        assert_eq!(model.highlights_fade_until, None);
        assert_eq!(tiles_of(&model, 0)[0].raster_count, 1);
    }

    #[test]
    fn a_tile_counts_its_rasters_but_not_the_frames_painted_straight_where_it_shows() {
        let key = painted_key(1);
        let coord = TileCoord::new(2, 1);
        let mut overlay = LayerOverlay::default();
        overlay.record_raster(key, coord, RasterCause::NewlyShown, coord.cell_rect(), at(0));
        overlay.record_raster(key, coord, RasterCause::KeyChanged, coord.cell_rect(), at(100));
        overlay.record_raster(
            key,
            coord,
            RasterCause::PaintedStraightIntoTheFrame,
            coord.cell_rect(),
            at(200),
        );
        let model = overlay.model_of_frame(frame(at(200), vec![painted_layer(key, "[L0] base", &[])]));
        let tile = &tiles_of(&model, 0)[0];
        assert_eq!(tile.raster_count, 2);
        assert_eq!(tile.held_rect, None);
        assert_eq!(
            tile.last_raster.unwrap().cause,
            RasterCause::PaintedStraightIntoTheFrame
        );

        // A cell the layer holds no tile at leaves the model with its highlight.
        let model = overlay.model_of_frame(frame(at(700), vec![painted_layer(key, "[L0] base", &[])]));
        assert!(tiles_of(&model, 0).is_empty());
    }

    #[test]
    fn frames_that_raster_nothing_leave_the_counts_as_they_are() {
        let key = painted_key(1);
        let mut overlay = LayerOverlay::default();
        overlay.record_raster(
            key,
            TileCoord::new(0, 0),
            RasterCause::KeyChanged,
            TileCoord::new(0, 0).cell_rect(),
            at(0),
        );
        let shaper = CountingShaper::default();
        for time in [0, 16, 33, 50] {
            overlay.take_frame(
                frame(at(time), vec![painted_layer(key, "[L0] base", &[(0, 0)])]),
                1.0,
                &shaper,
            );
        }
        let model = &overlay.latest_frame().unwrap().model;
        assert_eq!(tiles_of(model, 0)[0].raster_count, 1);
        assert_eq!(highlighted(model, 0), [(TileCoord::new(0, 0), RasterCause::KeyChanged)]);
    }

    #[test]
    fn a_layer_keeps_its_color_wherever_it_is_in_the_stack() {
        let (below, above) = (painted_key(1), painted_key(2));
        let overlay = LayerOverlay::default();
        let first = overlay.model_of_frame(frame(
            at(0),
            vec![
                painted_layer(below, "[L0] base", &[]),
                painted_layer(above, "[L1] painted", &[]),
            ],
        ));
        let second = overlay.model_of_frame(frame(
            at(16),
            vec![
                painted_layer(painted_key(3), "[L0] base", &[]),
                painted_layer(above, "[L1] painted", &[]),
                painted_layer(below, "[L2] painted", &[]),
            ],
        ));
        assert_eq!(first.layers[0].color, second.layers[2].color);
        assert_eq!(first.layers[1].color, second.layers[1].color);
        assert_ne!(first.layers[0].color, first.layers[1].color);
    }

    #[test]
    fn labels_are_shaped_once_for_the_frames_that_show_them_again() {
        let key = painted_key(1);
        let mut overlay = LayerOverlay::default();
        let shaper = CountingShaper::default();
        let layers = || vec![painted_layer(key, "[L0] base", &[(0, 0), (1, 0), (0, 1)])];
        overlay.take_frame(frame(at(0), layers()), 2.0, &shaper);
        let shaped_by_first_frame = shaper.shaped.get();
        assert!(shaped_by_first_frame > 0);
        overlay.take_frame(frame(at(16), layers()), 2.0, &shaper);
        assert_eq!(shaper.shaped.get(), shaped_by_first_frame);

        // A tile rastered again is labeled anew, and that label alone is shaped.
        overlay.record_raster(
            key,
            TileCoord::new(1, 0),
            RasterCause::KeyChanged,
            TileCoord::new(1, 0).cell_rect(),
            at(20),
        );
        overlay.take_frame(frame(at(33), layers()), 2.0, &shaper);
        assert_eq!(shaper.shaped.get(), shaped_by_first_frame + 1);
    }

    #[test]
    fn the_records_of_a_layer_go_once_it_leaves_the_stack() {
        let (stays, leaves) = (painted_key(1), painted_key(2));
        let mut overlay = LayerOverlay::default();
        for key in [stays, leaves] {
            overlay.record_raster(
                key,
                TileCoord::new(0, 0),
                RasterCause::NewlyShown,
                IntRect::new(0, 0, 10, 10),
                at(0),
            );
        }
        let shaper = CountingShaper::default();
        overlay.take_frame(
            frame(at(16), vec![painted_layer(stays, "[L0] base", &[(0, 0)])]),
            1.0,
            &shaper,
        );
        let model = overlay.model_of_frame(frame(
            at(32),
            vec![
                painted_layer(stays, "[L0] base", &[(0, 0)]),
                painted_layer(leaves, "[L1] painted", &[(0, 0)]),
            ],
        ));
        assert_eq!(tiles_of(&model, 0)[0].raster_count, 1);
        assert_eq!(tiles_of(&model, 1)[0].raster_count, 0);
    }

    #[test]
    fn the_text_lists_the_layers_and_the_tiles_rastered_since_the_overlay_was_switched_on() {
        let key = painted_key(1);
        let mut overlay = LayerOverlay::default();
        overlay.record_raster(
            key,
            TileCoord::new(1, 0),
            RasterCause::KeyChanged,
            TileCoord::new(1, 0).cell_rect(),
            at(0),
        );
        let mut frame = frame(
            at(10),
            vec![
                painted_layer(key, "[L0] base space=document", &[(0, 0), (1, 0)]),
                LayerOfFrame {
                    key: LayerKey::Media {
                        content: libcompositing_rust::display_list::summary::LiveContent::Canvas(
                            libcompositing_rust::display_list::commands::CanvasId(1),
                        ),
                        ordinal: 0,
                    },
                    name: "[L1] canvas #0 space=document region=[8,8 300x150]".into(),
                    composite_offset: IntPoint::default(),
                    composite_clip: None,
                    content: LayerOfFrameContent::Media {
                        region: IntRect::new(8, 8, 300, 150),
                        presentation: MediaPresentation::Replayed {
                            why: "the context is not shown in a remote layer".into(),
                        },
                    },
                },
            ],
        );
        frame.counts.tiles_rastered = 1;
        frame.media_and_backdrop_filters_without_layers = vec!["no layer for video: inside a group effect".into()];
        assert_eq!(
            overlay.model_of_frame(frame).to_string(),
            "[L0] base space=document: 2 tiles, 512 KiB\n\
             \x20   tile 1,0: rastered 1 time, last key changed over [256,0 256x256]\n\
             [L1] canvas #0 space=document region=[8,8 300x150]: replayed: the context is not shown in a remote layer\n\
             no layer for video: inside a group effect\n\
             last frame: 1 tile rastered, 0 tiles binned, 0 chunks placed, the stack kept its shape\n\
             highlighted: [L0] tile 1,0: key changed, 10 ms old\n"
        );
    }

    #[test]
    fn nothing_is_kept_while_the_overlay_is_off_and_it_is_erased_with_one_more_frame() {
        let mut switch = LayerOverlaySwitch::default();
        switch.switch_for_frame(false);
        assert!(switch.overlay_mut().is_none());
        assert!(!switch.repaints_everything());
        switch.did_paint_frame();

        switch.switch_for_frame(true);
        switch.overlay_mut().unwrap().record_raster(
            painted_key(1),
            TileCoord::new(0, 0),
            RasterCause::NewlyShown,
            IntRect::new(0, 0, 10, 10),
            at(0),
        );
        assert!(switch.repaints_everything());
        switch.did_paint_frame();

        switch.switch_for_frame(false);
        assert!(switch.overlay().is_none());
        assert_eq!(switch.highlights_fade_until(), None);
        assert!(switch.repaints_everything());
        switch.did_paint_frame();
        assert!(!switch.repaints_everything());

        // Switched on again, it counts from nothing.
        switch.switch_for_frame(true);
        let model = switch.overlay().unwrap().model_of_frame(frame(
            at(10),
            vec![painted_layer(painted_key(1), "[L0] base", &[(0, 0)])],
        ));
        assert_eq!(tiles_of(&model, 0)[0].raster_count, 0);
        assert_eq!(tiles_of(&model, 0)[0].last_raster, None);
    }
}
