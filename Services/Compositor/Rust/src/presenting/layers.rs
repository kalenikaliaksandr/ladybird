/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a frame is painted in, each kept as tiles from one frame and one list to the next, and what of them the
//! window server shows above the frame by itself. The pixels of a tile are kept in a pool under the tile's key, which
//! digests all they depend on, so a layer finds the tiles another layer or an earlier stack rastered.

use super::layer_overlay::{
    FrameCounts, FrameOfLayerOverlay, LayerOfFrame, LayerOfFrameContent, MediaPresentation, RasterCause,
};
use super::{
    ContextPresenting, FrameToPaint, NestedContexts, PresentingEnvironment, apply_raster_transform,
    composite_inside_effects_of_frame, paint_overlays, rect_of_size, replay_into, replay_with_scroll_offsets,
    replaying_only,
};
use crate::host::ContextTimer;
use crate::raster_host::{
    FfiSublayer, FfiSublayerPlacement, LiveContent, MediaKind, PresentableSurfaceHandle, RasterHost,
    SublayerContentKind, SurfaceBlend, SurfaceHandle,
};
use libcompositing_rust::display_list::commands::{
    CanvasId, CompositorContextId, ImageFrameResourceId, SpatialNodeIndex, VideoSinkHandle,
};
use libcompositing_rust::display_list::layers::{
    LayerContent, LayerKey, LayerStack, LayerStackInputs, ListPlacement, PlacementKey, StackLayer,
    VIEWPORT_RASTER_SPACE, backdrop_filter_read_area, build_layer_stack, place_list, raster_space_anchors,
};
use libcompositing_rust::display_list::placement::{
    RasterSpace, RasterSpaces, SpaceGeometry, raster_scroll_offsets, spatial_nodes_that_move_on_their_own,
};
use libcompositing_rust::display_list::reach::Reach;
use libcompositing_rust::display_list::replay::ReplaySelection;
use libcompositing_rust::display_list::storage::ListSpan;
use libcompositing_rust::display_list::summary::LiveContent as LiveMedia;
use libcompositing_rust::display_list::tile_keys::{
    GroupTileKeys, KeyedTile, LayerConstants, ListOfFrame, LiveContentVersions, RasterSpaceOfFrame,
    ReplayReadsOutsideTheList, TileKey,
};
use libcompositing_rust::display_list::tiles::TileCoord;
use libcompositing_rust::display_list::window_server_placement::{WindowServerPlacement, WindowServerRejection};
use libcompositing_rust::scrolling::DisplayList;
use libcompositing_rust::time::{Duration, MonotonicTime};
use libcompositing_rust::visual_context::VisualContextTree;
use libgfx_rust::{Color, FloatMatrix4x4, FloatPoint, IntPoint, IntRect, IntSize, ScalingMode};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

const TILE_RASTER_SURFACE_RELEASE_DELAY: Duration = Duration::from_milliseconds(3000);
// How long a tile that no layer holds stays in the pool, for content that comes back to what it held.
const TIME_A_SPARE_TILE_STAYS_IN_THE_POOL: Duration = Duration::from_milliseconds(3000);

// The pool of presentable surfaces the overlays are shown from, apart from those of the painted layers.
const COMPOSITOR_OVERLAY_SURFACE_POOL: u32 = u32::MAX;

// Sublayers keep their identity from one frame to the next by what they show.
const CANVAS_SUBLAYER_IDENTITY_TAG: u64 = 1 << 60;
const VIDEO_SUBLAYER_IDENTITY_TAG: u64 = 2 << 60;
const PAINTED_LAYER_SUBLAYER_IDENTITY_TAG: u64 = 3 << 60;
const COMPOSITOR_OVERLAY_SUBLAYER_IDENTITY: u64 = 4 << 60;
// Canvas and video IDs count up from 1, and stay far below this.
const MEDIA_DRAWING_INDEX_SHIFT: u32 = 44;

// The pixels of the tile of one key, which every layer that holds a tile of that key shows.
struct PooledTile {
    surface: SurfaceHandle,
    // The part of its cell that the surface holds.
    rect: IntRect,
    holds_current_pixels: bool,
    // When a layer held the tile last, as of the latest frame.
    last_held_at: MonotonicTime,
}

type TilePool = HashMap<TileKey, PooledTile>;

// The surface that the window server shows a painted layer from.
struct PaintedLayerPresentation {
    pool: u32,
    latest_surface: Option<PresentableSurfaceHandle>,
    rect: IntRect,
    content_version: u64,
}

// What is kept of a layer under its key while it is in the stack: the keys of the tiles it holds, whose pixels the pool
// keeps, or for a canvas, video or nested context what was replayed of it.
#[derive(Default)]
struct LayerState {
    tiles: BTreeMap<TileCoord, TileKey>,
    // Changes whenever anything is drawn into the tiles, or the tiles change.
    content_version: u64,
    digest_of_replayed_records: Option<TileKey>,
    presentation: Option<PaintedLayerPresentation>,
}

impl LayerState {
    fn invalidate_tiles_shown_within(
        &mut self,
        tile_pool: &mut TilePool,
        invalidation: TileInvalidation,
        composite_offset: IntPoint,
    ) {
        let rect_in_raster_space = match invalidation {
            TileInvalidation::Nothing => return,
            TileInvalidation::WithinViewportRect(rect) => {
                Some(translated(rect, -composite_offset.x, -composite_offset.y))
            }
            TileInvalidation::Everywhere => None,
        };
        for key in self.tiles.values() {
            if let Some(tile) = tile_pool.get_mut(key)
                && rect_in_raster_space.is_none_or(|rect| !rect.intersected(tile.rect).is_empty())
            {
                tile.holds_current_pixels = false;
            }
        }
        self.digest_of_replayed_records = None;
    }

    fn rect_held(&self, tile_pool: &TilePool) -> IntRect {
        self.tiles
            .values()
            .filter_map(|key| tile_pool.get(key))
            .fold(IntRect::default(), |bounds, tile| bounds.united(tile.rect))
    }
}

/// Why the window server does not show a canvas, video or nested context layer by itself, which the frame then replays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhyMediaIsReplayed {
    ContextIsNotShownInARemoteLayer,
    NoGpu,
    ContextIsPresentedFromCpuMemory,
    Placement(WindowServerRejection),
    NoPlacement(LiveMedia),
    CanvasHasNoPresentableSurface,
    VideoHasNoFrame,
    HighDynamicRangeIsNotShownNow,
    WindowServerCannotShowThePlacement,
    TilesWerePaintedStraightIntoTheFrame,
    InsideAnEffectTheCompositorComposites,
    UnderALayerTheFrameHolds,
}

impl std::fmt::Display for WhyMediaIsReplayed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyMediaIsReplayed::ContextIsNotShownInARemoteLayer => "the context is not shown in a remote layer",
            WhyMediaIsReplayed::NoGpu => "the compositor has no GPU",
            WhyMediaIsReplayed::ContextIsPresentedFromCpuMemory => "the context is presented from CPU memory",
            WhyMediaIsReplayed::Placement(rejection) => return write!(formatter, "{rejection}"),
            WhyMediaIsReplayed::NoPlacement(LiveMedia::ImageFrame(_)) => "the window server does not show images",
            WhyMediaIsReplayed::NoPlacement(LiveMedia::CompositedContext(_)) => {
                "the window server does not show nested contexts"
            }
            WhyMediaIsReplayed::NoPlacement(_) => "where the window server would show it is not worked out",
            WhyMediaIsReplayed::CanvasHasNoPresentableSurface => "the canvas has no surface the window server can show",
            WhyMediaIsReplayed::VideoHasNoFrame => "the video has no frame",
            WhyMediaIsReplayed::HighDynamicRangeIsNotShownNow => {
                "the window server does not show high dynamic range now"
            }
            WhyMediaIsReplayed::WindowServerCannotShowThePlacement => "the window server cannot show it where it is",
            WhyMediaIsReplayed::TilesWerePaintedStraightIntoTheFrame => "tiles were painted straight into the frame",
            WhyMediaIsReplayed::InsideAnEffectTheCompositorComposites => {
                "it is inside an effect the compositor composites"
            }
            WhyMediaIsReplayed::UnderALayerTheFrameHolds => "it is under a layer the frame holds",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TileInvalidation {
    #[default]
    Nothing,
    Everywhere,
    WithinViewportRect(IntRect),
}

struct LiveContentVersionsOfFrame<'a> {
    live_content: &'a LiveContent,
    nested_contexts: &'a dyn NestedContexts,
}

impl LiveContentVersions for LiveContentVersionsOfFrame<'_> {
    fn canvas_content_generation(&self, canvas: CanvasId) -> u64 {
        self.live_content
            .canvases
            .get(&canvas.0)
            .map_or(0, |canvas| canvas.content_generation)
    }

    fn video_frame_id(&self, video_sink: VideoSinkHandle) -> Option<u64> {
        self.live_content
            .video_sinks
            .get(&video_sink.0)
            .and_then(|sink| sink.has_frame.then_some(sink.frame_id))
    }

    fn image_frame_pixel_generation(&self, image_frame: ImageFrameResourceId) -> Option<u64> {
        self.live_content.image_frames.get(&image_frame.0).copied()
    }

    fn nested_context_frame_generation(&self, nested_context: CompositorContextId) -> Option<u64> {
        self.nested_contexts.frame_generation_of(nested_context)
    }
}

/// Where a frame composites a layer: translated by the scroll offsets of its space's anchors, within its clip, and
/// whether the frame holds it or the window server shows it above the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompositingOfLayer {
    key: LayerKey,
    offset: IntPoint,
    clip: Option<IntRect>,
    shown_by_window_server: bool,
}

// The keys of the tiles of a group of a stack, which a frame takes from the context and gives back once painted, bound
// to the list and the geometry of the group's space they were binned against last.
struct KeyedGroupOfFrame {
    stack_generation: u64,
    group: usize,
    display_list: Rc<DisplayList>,
    geometry: Rc<SpaceGeometry>,
    keys: GroupTileKeys,
    // The tiles the frame shows, by their cell and their layer.
    shown_tiles: Vec<(TileCoord, usize)>,
}

impl KeyedGroupOfFrame {
    fn tile(&self, index: usize) -> &KeyedTile {
        let (coord, layer) = self.shown_tiles[index];
        self.keys.tile(coord, layer).expect("a shown tile is keyed")
    }
}

pub struct ShownTilesOfLayer {
    group: usize,
    // Indices into the tiles of the keyed group.
    tiles: Vec<usize>,
    rects_needed: Vec<IntRect>,
}

#[derive(Default)]
pub struct TilePass {
    viewport_rect: IntRect,
    groups: Vec<KeyedGroupOfFrame>,
    // Indexed like the layers of the stack.
    shown_tiles_of_layers: Vec<Option<ShownTilesOfLayer>>,
    digests_of_replayed_records: Vec<Option<TileKey>>,
    // What compositing the nodes of composited effects reads of each effect, by node.
    digests_of_composited_effects: HashMap<PlacementKey, TileKey>,
}

/// What a frame paints of its layers.
pub struct LayerUpdate {
    pub tile_invalidation: TileInvalidation,
    pub tile_pass: Option<TilePass>,
    pub composite_offsets: Vec<IntPoint>,
    /// From the base layer up, the layers that can hold something, and where the frame composites each.
    pub compositing: Vec<CompositingOfLayer>,
    pub shown_by_window_server: bool,
    /// In the raster coordinates of the frame, what each composited effect that reads past each of its pixels reads
    /// and draws as the frame places it.
    pub read_areas_of_composited_effects: Vec<IntRect>,
}

impl LayerUpdate {
    /// The damage grown over all that a composited effect reads wherever it reaches that, as compositing the effect
    /// reads what is composited below it, which the frame painted before holds only under what it composited above.
    pub fn damage_grown_over_what_composited_effects_read(&self, mut damage: IntRect) -> IntRect {
        loop {
            let mut grew = false;
            for &read_area in &self.read_areas_of_composited_effects {
                if !read_area.intersected(damage).is_empty() && !damage.contains_rect(read_area) {
                    damage = damage.united(read_area);
                    grew = true;
                }
            }
            if !grew {
                return damage;
            }
        }
    }
}

// The geometry of a space as a frame worked it out, which later frames reuse while the tree copy, the spaces and the
// raster scroll offsets of the space stay, as they do while the space's own anchor scrolls.
struct KnownSpaceGeometry {
    tree_copy: u64,
    anchors_of_spaces: Vec<SpatialNodeIndex>,
    space: usize,
    viewport: ViewportRaster,
    geometry: Rc<SpaceGeometry>,
}

// The viewport in the device pixels of the raster, and how those of the viewport map into them.
#[derive(Clone, Copy, PartialEq)]
struct ViewportRaster {
    rect: IntRect,
    transform: FloatMatrix4x4,
}

// Where the parts of a list were placed for a frame, and what that depended on: the list, the tree copy, the anchors of
// the spaces, which change as a sticky box starts or stops being held, and the scroll offsets of the nodes that neither
// anchor a space nor move on their own.
struct PlacementOfFrame {
    display_list: Rc<DisplayList>,
    tree_copy: u64,
    viewport_rect: IntRect,
    raster_transform: FloatMatrix4x4,
    scroll_offsets_placement_depends_on: Vec<FloatPoint>,
    anchors: Vec<SpatialNodeIndex>,
    placement: ListPlacement,
}

/// The layers of the frames of a context under their keys, the pool of their tiles, and what of them the window server
/// shows.
#[derive(Default)]
pub struct ContextLayers {
    stack: Option<LayerStack>,
    placement_of_latest_frame: Option<PlacementOfFrame>,
    states: HashMap<LayerKey, LayerState>,
    tile_pool: TilePool,
    // How the latest frame composited its layers, from the base layer up.
    compositing_of_latest_frame: Vec<CompositingOfLayer>,
    last_content_version: u64,
    // Where damage reaches several tiles of a layer, the layer is replayed once into this surface, and its damage
    // copied into the tiles.
    tile_raster_surface: Option<SurfaceHandle>,
    tiles_rasterized_since_last_check: u64,
    most_tiles_rasterized_by_a_frame_since_last_check: u64,
    tiles_rasterized_by_this_frame: u64,
    layer_stacks_computed_since_last_check: u64,
    layers_created_since_last_check: u64,
    chunks_placed_since_last_check: u64,
    tiles_binned_since_last_check: u64,
    chunks_placed_by_this_frame: u64,
    tiles_binned_by_this_frame: u64,
    stack_changed_shape_with_this_frame: bool,
    // Whether the window server shows the layers above the base layer of the latest frame.
    window_server_shows_media: bool,
    latest_overlay_surface: Option<PresentableSurfaceHandle>,
    overlay_rect: IntRect,
    known_space_geometries: Vec<KnownSpaceGeometry>,
    next_presentable_surface_pool: u32,
    // Counts the stacks of another shape worked out, so that what was keyed against one is not taken for another.
    stack_generation: u64,
    // The keys of the tiles of the groups of the latest frame, kept for the next one.
    keyed_groups: Vec<KeyedGroupOfFrame>,
    // What compositing the nodes of composited effects read of each effect in the latest frame, by node.
    digests_of_composited_effects: HashMap<PlacementKey, TileKey>,
}

/// What the test hook reports of the layers since it last asked.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerCountsSinceLastCheck {
    pub tiles_rasterized: u64,
    pub most_tiles_rasterized_by_a_frame: u64,
    pub layer_stacks_computed: u64,
    pub layers_created: u64,
    pub chunks_placed: u64,
    pub tiles_binned: u64,
}

impl ContextLayers {
    pub fn has_layers(&self) -> bool {
        self.stack.is_some()
    }

    pub fn window_server_shows_media(&self) -> bool {
        self.window_server_shows_media
    }

    pub fn compositing_of_latest_frame(&self) -> &[CompositingOfLayer] {
        &self.compositing_of_latest_frame
    }

    pub fn take_counts_since_last_check(&mut self) -> LayerCountsSinceLastCheck {
        LayerCountsSinceLastCheck {
            tiles_rasterized: std::mem::take(&mut self.tiles_rasterized_since_last_check),
            most_tiles_rasterized_by_a_frame: std::mem::take(
                &mut self.most_tiles_rasterized_by_a_frame_since_last_check,
            ),
            layer_stacks_computed: std::mem::take(&mut self.layer_stacks_computed_since_last_check),
            layers_created: std::mem::take(&mut self.layers_created_since_last_check),
            chunks_placed: std::mem::take(&mut self.chunks_placed_since_last_check),
            tiles_binned: std::mem::take(&mut self.tiles_binned_since_last_check),
        }
    }

    pub fn spare_tile_eviction_timer_fired(&mut self, env: &PresentingEnvironment<'_>) {
        self.evict_spare_tiles(env);
    }

    // Drops from the pool the tiles that no layer has held for long enough, or whose pixels are not current, and then
    // the least recently held of the others for as long as they hold more pixels than the tiles layers hold. Keeps the
    // timer running for those that stay.
    fn evict_spare_tiles(&mut self, env: &PresentingEnvironment<'_>) {
        let held: HashSet<TileKey> = self
            .states
            .values()
            .flat_map(|state| state.tiles.values().copied())
            .collect();
        let pixels_of = |tile: &PooledTile| tile.rect.width.max(0) as u64 * tile.rect.height.max(0) as u64;
        let mut pixels_of_held_tiles = 0u64;
        let mut spare_tiles: Vec<(MonotonicTime, TileKey, u64)> = Vec::new();
        self.tile_pool.retain(|key, tile| {
            if held.contains(key) {
                tile.last_held_at = env.now;
                pixels_of_held_tiles += pixels_of(tile);
                return true;
            }
            if !tile.holds_current_pixels || env.now - tile.last_held_at >= TIME_A_SPARE_TILE_STAYS_IN_THE_POOL {
                return false;
            }
            spare_tiles.push((tile.last_held_at, *key, pixels_of(tile)));
            true
        });
        spare_tiles.sort_unstable();
        let mut pixels_of_spare_tiles: u64 = spare_tiles.iter().map(|(_, _, pixels)| pixels).sum();
        let mut evicted = 0;
        for (_, key, pixels) in &spare_tiles {
            if pixels_of_spare_tiles <= pixels_of_held_tiles {
                break;
            }
            self.tile_pool.remove(key);
            pixels_of_spare_tiles -= pixels;
            evicted += 1;
        }
        if let Some((least_recently_held_at, _, _)) = spare_tiles.get(evicted) {
            env.host.set_timer(
                env.context_id,
                ContextTimer::SpareTileEviction,
                TIME_A_SPARE_TILE_STAYS_IN_THE_POOL - (env.now - *least_recently_held_at),
            );
        }
    }

    // How many layers hold the tile of each key, or show it with the frame that `tile_pass` worked out.
    fn holders_of_tiles(&self, tile_pass: &TilePass) -> HashMap<TileKey, usize> {
        let mut holders: HashMap<TileKey, usize> = HashMap::new();
        for key in self.states.values().flat_map(|state| state.tiles.values()) {
            *holders.entry(*key).or_default() += 1;
        }
        for shown in tile_pass.shown_tiles_of_layers.iter().flatten() {
            let keyed = &tile_pass.groups[shown.group];
            for &tile_index in &shown.tiles {
                *holders.entry(keyed.tile(tile_index).key).or_default() += 1;
            }
        }
        holders
    }

    pub fn tile_raster_surface_release_timer_fired(&mut self) {
        self.tile_raster_surface = None;
    }

    fn tile_raster_surface_of_size(
        &mut self,
        env: &PresentingEnvironment<'_>,
        size: IntSize,
        like: &SurfaceHandle,
    ) -> Option<SurfaceHandle> {
        let fits = self.tile_raster_surface.as_ref().is_some_and(|surface| {
            env.raster.surface_size(surface) == size && env.raster.surfaces_share_backend(surface, like)
        });
        if !fits {
            self.tile_raster_surface = None;
            self.tile_raster_surface = Some(create_surface_like(env.raster, size, like)?);
        }
        env.host.set_timer(
            env.context_id,
            ContextTimer::TileRasterSurfaceRelease,
            TILE_RASTER_SURFACE_RELEASE_DELAY,
        );
        self.tile_raster_surface.clone()
    }

    fn geometry_of_space(
        &mut self,
        tree: &VisualContextTree,
        spaces: &[RasterSpace],
        raster_spaces: &mut Option<RasterSpaces>,
        space: usize,
        scroll_offsets: &[FloatPoint],
        viewport: ViewportRaster,
    ) -> Rc<SpaceGeometry> {
        let anchors_match = |known: &KnownSpaceGeometry| {
            known.anchors_of_spaces.len() == spaces.len()
                && known
                    .anchors_of_spaces
                    .iter()
                    .zip(spaces)
                    .all(|(anchor, space)| *anchor == space.anchor)
        };
        let raster_offsets = raster_scroll_offsets(spaces, space, scroll_offsets);
        if let Some(known) = self.known_space_geometries.iter().find(|known| {
            known.tree_copy == tree.copy.number()
                && known.space == space
                && known.viewport == viewport
                && anchors_match(known)
                && known.geometry.raster_scroll_offsets == raster_offsets
        }) {
            return known.geometry.clone();
        }
        let raster_spaces = raster_spaces.get_or_insert_with(|| RasterSpaces::of_spaces(tree, spaces.to_vec()));
        let geometry = Rc::new(SpaceGeometry::new(
            tree,
            raster_spaces,
            space,
            scroll_offsets,
            viewport.rect,
            viewport.transform,
        ));
        self.known_space_geometries.retain(|known| {
            known.tree_copy == tree.copy.number()
                && known.viewport == viewport
                && !(known.space == space && anchors_match(known))
        });
        self.known_space_geometries.push(KnownSpaceGeometry {
            tree_copy: tree.copy.number(),
            anchors_of_spaces: spaces.iter().map(|space| space.anchor).collect(),
            space,
            viewport,
            geometry: geometry.clone(),
        });
        geometry
    }
}

// Replays into the scratch surface over `bounds`, and copies what lands on each tile into it.
fn raster_through_scratch(
    raster: &RasterHost,
    scratch: &SurfaceHandle,
    tiles: &[(SurfaceHandle, IntRect)],
    bounds: IntRect,
    clear_color: Color,
    replay: &dyn Fn(&SurfaceHandle),
) {
    let save_count = raster.save(scratch);
    raster.clear(scratch, clear_color);
    raster.translate(scratch, -bounds.x as f32, -bounds.y as f32);
    replay(scratch);
    raster.restore_to_count(scratch, save_count);
    for (surface, rect) in tiles {
        raster.draw_surface(
            surface,
            scratch,
            translated(*rect, -bounds.x, -bounds.y),
            rect_of_size(size_of(*rect)),
            SurfaceBlend::Source,
        );
    }
}

// A surface on the backend of the one given, or none, as one that falls back to the CPU on a GPU backend would be
// allocated again with every frame.
fn create_surface_like(raster: &RasterHost, size: IntSize, like: &SurfaceHandle) -> Option<SurfaceHandle> {
    raster
        .create_surface(size, Some(like))
        .filter(|surface| raster.surfaces_share_backend(surface, like))
}

fn size_of(rect: IntRect) -> IntSize {
    IntSize {
        width: rect.width,
        height: rect.height,
    }
}

fn translated(rect: IntRect, dx: i32, dy: i32) -> IntRect {
    IntRect::new(rect.x + dx, rect.y + dy, rect.width, rect.height)
}

// What of the viewport a layer can show, in the raster coordinates of its space.
fn rect_shown_of(compositing: &CompositingOfLayer, viewport_rect: IntRect) -> IntRect {
    let shown_in_viewport = match compositing.clip {
        Some(clip) => viewport_rect.intersected(clip),
        None => viewport_rect,
    };
    translated(shown_in_viewport, -compositing.offset.x, -compositing.offset.y)
}

// Where the frame is in the raster coordinates of a space other than the viewport's, which the space's offset in the
// frame moves.
fn frame_in_space(space: usize, composite_offset: IntPoint, viewport_rect: IntRect) -> Option<IntRect> {
    (space != VIEWPORT_RASTER_SPACE).then(|| translated(viewport_rect, -composite_offset.x, -composite_offset.y))
}

// A layer of a space that scrolls keeps tiles a little past what the viewport shows of it, for scrolls back.
fn tile_area_of(space: usize, rect_shown: IntRect, viewport_rect: IntRect) -> IntRect {
    if space == VIEWPORT_RASTER_SPACE {
        return viewport_rect;
    }
    if rect_shown.is_empty() {
        return IntRect::default();
    }
    let (half_width, half_height) = (viewport_rect.width / 2, viewport_rect.height / 2);
    rect_shown.inflated_edges(half_height, half_width, half_height, half_width)
}

// Why a layer rasters a tile it needs, given the tile it held at the cell, of the same key or another, if any.
fn raster_cause_of(
    held: Option<(&PooledTile, bool)>,
    rect_needed: IntRect,
    raster: &RasterHost,
    like: &SurfaceHandle,
) -> RasterCause {
    let Some((held, held_key_is_needed)) = held else {
        return RasterCause::NewlyShown;
    };
    if !held.rect.contains_rect(rect_needed) {
        return RasterCause::HeldLessThanNeeded;
    }
    if !raster.surfaces_share_backend(&held.surface, like) {
        return RasterCause::HeldOnAnotherBackend;
    }
    if held_key_is_needed {
        RasterCause::HeldPixelsInvalidated
    } else {
        RasterCause::KeyChanged
    }
}

// Draws the tiles over `damage_rect` of the surface, with the layer's space translated by `offset` into the surface's.
#[allow(clippy::too_many_arguments)]
fn composite_layer_tiles(
    raster: &RasterHost,
    surface: &SurfaceHandle,
    state: &LayerState,
    tile_pool: &TilePool,
    damage_rect: IntRect,
    blend: SurfaceBlend,
    offset: IntPoint,
    composite_clip: Option<IntRect>,
) {
    let area = composite_clip.map_or(damage_rect, |clip| damage_rect.intersected(clip));
    let save_count = raster.save(surface);
    if let Some(clip) = composite_clip {
        raster.clip_rect(surface, clip);
    }
    for tile in state.tiles.values().filter_map(|key| tile_pool.get(key)) {
        let rect = translated(tile.rect, offset.x, offset.y);
        if rect.intersected(area).is_empty() {
            continue;
        }
        raster.draw_surface(surface, &tile.surface, rect_of_size(size_of(rect)), rect, blend);
    }
    raster.restore_to_count(surface, save_count);
}

fn sublayer_placement_of(placement: &WindowServerPlacement) -> FfiSublayerPlacement {
    let rounded_clip = placement.rounded_clip;
    FfiSublayerPlacement {
        content_rect: placement.content_rect,
        content_to_frame: placement.content_to_viewport.values,
        clip_rect: placement.clip_rect,
        has_rounded_clip: rounded_clip.is_some(),
        rounded_clip_rect: rounded_clip.map(|clip| clip.rect).unwrap_or_default(),
        rounded_clip_radius: rounded_clip.map_or(0.0, |clip| clip.radius),
        rounds_top_left: rounded_clip.is_some_and(|clip| clip.rounded_corners.top_left),
        rounds_top_right: rounded_clip.is_some_and(|clip| clip.rounded_corners.top_right),
        rounds_bottom_right: rounded_clip.is_some_and(|clip| clip.rounded_corners.bottom_right),
        rounds_bottom_left: rounded_clip.is_some_and(|clip| clip.rounded_corners.bottom_left),
        scaling_mode: placement.scaling_mode,
    }
}

// A sublayer that shows a surface at its own rect, unclipped and untransformed.
fn surface_sublayer(identity: u64, surface: &PresentableSurfaceHandle, rect: IntRect) -> FfiSublayer {
    FfiSublayer {
        identity,
        content_kind: SublayerContentKind::PresentableSurface,
        media_id: 0,
        presentable_surface: surface.as_ptr(),
        placement: FfiSublayerPlacement {
            content_rect: rect,
            content_to_frame: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            clip_rect: rect.to_float(),
            scaling_mode: ScalingMode::NearestNeighbor,
            ..Default::default()
        },
    }
}

// The scroll offsets with those of the nodes that the compositor scrolls or that move on their own left out: what the
// placement of a list depends on of the offsets, as content that moves with those is rastered in a space that follows
// them or placed wherever they could take it.
fn scroll_offsets_placement_depends_on(
    tree: &VisualContextTree,
    scroll_offsets: &[FloatPoint],
    moves_on_its_own: &[bool],
) -> Vec<FloatPoint> {
    let scrolled_by_the_compositor = &tree.compositor_scroll_table().nodes;
    scroll_offsets
        .iter()
        .enumerate()
        .map(|(index, offset)| {
            let left_out = moves_on_its_own.get(index).copied().unwrap_or(false)
                || scrolled_by_the_compositor
                    .iter()
                    .any(|record| record.scroll_node_index.0 as usize == index);
            if left_out { FloatPoint::default() } else { *offset }
        })
        .collect()
}

fn compositing_of_layer(stack: &LayerStack, layer: usize, composite_offsets: &[IntPoint]) -> CompositingOfLayer {
    CompositingOfLayer {
        key: stack.layers[layer].key,
        offset: composite_offsets[stack.space_of_layer(layer)],
        clip: stack.composite_clip_in_viewport(layer, composite_offsets),
        shown_by_window_server: false,
    }
}

impl ContextPresenting {
    /// Works out the layers of the frame within `viewport_rect`, in the device pixels of the raster: keeps the stack
    /// while where the list's parts are placed stays, and works it out again otherwise.
    pub(super) fn layer_update_for(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        viewport_rect: IntRect,
        live_content: &LiveContent,
    ) -> LayerUpdate {
        let raster_transform = self.raster_transform();
        let storage = frame.display_list.storage();
        let tree = frame.frame_inputs.visual_context_tree.as_arc();
        let scroll_offsets = frame.frame_inputs.presented_scroll_state_snapshot.device_offsets();
        self.layers.chunks_placed_by_this_frame = 0;
        self.layers.tiles_binned_by_this_frame = 0;
        self.layers.stack_changed_shape_with_this_frame = false;
        let inputs = LayerStackInputs {
            list: storage,
            tree,
            scroll_offsets,
            viewport_rect,
            raster_transform,
        };

        let moves_on_its_own = spatial_nodes_that_move_on_their_own(tree);
        // A space that scrolls composites at whole device pixels of the viewport, which a raster that scales them or
        // shifts them by a fraction does not have, so such a raster has the viewport's space alone.
        let anchors = if raster_transform == FloatMatrix4x4::identity() {
            raster_space_anchors(tree, scroll_offsets, &moves_on_its_own)
        } else {
            Vec::new()
        };
        let offsets_placement_depends_on = scroll_offsets_placement_depends_on(tree, scroll_offsets, &moves_on_its_own);
        let placement_is_known = self.layers.placement_of_latest_frame.as_ref().is_some_and(|known| {
            Rc::ptr_eq(&known.display_list, frame.display_list)
                && known.tree_copy == tree.copy.number()
                && known.viewport_rect == viewport_rect
                && known.raster_transform == raster_transform
                && known.anchors == anchors
                && known.scroll_offsets_placement_depends_on == offsets_placement_depends_on
        });
        if !placement_is_known {
            let spaces = RasterSpaces::at_anchors(tree, &anchors, viewport_rect);
            let spaces_at_anchors = spaces.spaces().to_vec();
            let mut raster_spaces = Some(spaces);
            let geometries: Vec<Rc<SpaceGeometry>> = (0..spaces_at_anchors.len())
                .map(|space| {
                    self.layers.geometry_of_space(
                        tree,
                        &spaces_at_anchors,
                        &mut raster_spaces,
                        space,
                        scroll_offsets,
                        ViewportRaster {
                            rect: viewport_rect,
                            transform: raster_transform,
                        },
                    )
                })
                .collect();
            let spaces = raster_spaces.expect("the spaces stay until the list is placed among them");
            let placement = place_list(&inputs, spaces, &geometries);
            self.layers.placement_of_latest_frame = Some(PlacementOfFrame {
                display_list: frame.display_list.clone(),
                tree_copy: tree.copy.number(),
                viewport_rect,
                raster_transform,
                scroll_offsets_placement_depends_on: offsets_placement_depends_on,
                anchors,
                placement,
            });
        }
        let known = self
            .layers
            .placement_of_latest_frame
            .as_ref()
            .expect("the placement of the frame is known");
        let stack_still_holds = self
            .layers
            .stack
            .as_ref()
            .is_some_and(|stack| stack.placement_at_every_anchor == known.placement);
        if !stack_still_holds {
            let stack = build_layer_stack(&inputs, &known.anchors, known.placement.clone());
            // A stack of the same shape keeps what was keyed against the one it replaces, as where the list's records
            // are is bound anew to every list.
            let keeps_its_shape = self
                .layers
                .stack
                .as_ref()
                .is_some_and(|current| current.has_the_shape_of(&stack));
            if !keeps_its_shape {
                self.layers.stack_generation += 1;
                self.layers.layer_stacks_computed_since_last_check += 1;
                self.layers.stack_changed_shape_with_this_frame = true;
            }
            self.layers.stack = Some(stack);
        }
        let stack = self.layers.stack.as_mut().expect("a frame with layers has a stack");
        stack.update_window_server_placements(storage, tree, scroll_offsets, viewport_rect);
        let stack = self.layers.stack.as_ref().expect("a frame with layers has a stack");
        let composite_offsets = stack.composite_offsets(scroll_offsets);
        let shown_by_window_server_of_layers = if self.why_window_server_shows_no_media_of_this_context(env).is_none() {
            stack.layers_the_window_server_shows(&composite_offsets, |layer| {
                self.why_window_server_does_not_show(env, &stack.layers[layer], live_content)
                    .is_none()
            })
        } else {
            vec![false; stack.layers.len()]
        };
        let compositing: Vec<CompositingOfLayer> = (stack.base_layer..stack.layers.len())
            .filter(|&layer| layer == stack.base_layer || stack.layers[layer].may_hold_content)
            .map(|layer| CompositingOfLayer {
                shown_by_window_server: shown_by_window_server_of_layers[layer],
                ..compositing_of_layer(stack, layer, &composite_offsets)
            })
            .collect();
        let shown_by_window_server = compositing.iter().any(|compositing| compositing.shown_by_window_server);
        LayerUpdate {
            tile_invalidation: TileInvalidation::Nothing,
            tile_pass: None,
            composite_offsets,
            compositing,
            shown_by_window_server,
            read_areas_of_composited_effects: Vec::new(),
        }
    }

    /// Keys the tiles the frame shows of each layer, and answers the rect of the viewport that the tiles to raster
    /// again, the tiles that go, and the media replayed anew cover.
    pub(super) fn prepare_tile_pass(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        live_content: &LiveContent,
        layer_update: &mut LayerUpdate,
        viewport_rect: IntRect,
    ) -> IntRect {
        let stack = self.layers.stack.take().expect("a frame with layers has a stack");
        let stale_rect = self.prepare_tile_pass_of_stack(env, frame, live_content, layer_update, viewport_rect, &stack);
        self.layers.stack = Some(stack);
        stale_rect
    }

    fn prepare_tile_pass_of_stack(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        live_content: &LiveContent,
        layer_update: &mut LayerUpdate,
        viewport_rect: IntRect,
        stack: &LayerStack,
    ) -> IntRect {
        let storage = frame.display_list.storage();
        let tree = frame.frame_inputs.visual_context_tree.as_arc();
        let scroll_offsets = frame.frame_inputs.presented_scroll_state_snapshot.device_offsets();
        let composite_offsets = &layer_update.composite_offsets;
        for compositing in &layer_update.compositing {
            if let Some(state) = self.layers.states.get_mut(&compositing.key) {
                state.invalidate_tiles_shown_within(
                    &mut self.layers.tile_pool,
                    layer_update.tile_invalidation,
                    compositing.offset,
                );
            }
        }
        // Whatever made every tile stale holds for the tiles that no layer holds as much.
        if layer_update.tile_invalidation == TileInvalidation::Everywhere {
            for tile in self.layers.tile_pool.values_mut() {
                tile.holds_current_pixels = false;
            }
        }

        let raster_transform = self.raster_transform();
        let mut raster_spaces = None;
        let geometries: Vec<Rc<SpaceGeometry>> = (0..stack.spaces.len())
            .map(|space| {
                self.layers.geometry_of_space(
                    tree,
                    &stack.spaces,
                    &mut raster_spaces,
                    space,
                    scroll_offsets,
                    ViewportRaster {
                        rect: viewport_rect,
                        transform: raster_transform,
                    },
                )
            })
            .collect();
        let scrollbars = frame.frame_inputs.scrollbars_shown_for_the_page();
        let live_content_versions = LiveContentVersionsOfFrame {
            live_content,
            nested_contexts: env.nested_contexts,
        };
        let list = ListOfFrame::new(
            storage,
            tree,
            stack,
            ReplayReadsOutsideTheList {
                scrollbars: &scrollbars,
                time_ns: frame.frame_inputs.painted_at.nanoseconds(),
                live_content: &live_content_versions,
            },
        );
        let raster_spaces: Vec<RasterSpaceOfFrame<'_>> = geometries
            .iter()
            .map(|geometry| RasterSpaceOfFrame::new(&list, geometry))
            .collect();
        let surface_clear_color = frame.display_list.surface_clear_color().unwrap_or(Color::TRANSPARENT);
        let root_group_color = frame.display_list.root_group_color();
        let constants_of_layer = |layer: usize| LayerConstants {
            raster_space_identity: stack.spaces[stack.space_of_layer(layer)].identity,
            clear_color: if layer == stack.base_layer {
                surface_clear_color
            } else {
                Color::TRANSPARENT
            },
            root_group_color,
            raster_transform,
        };

        let mut pass = TilePass {
            viewport_rect,
            shown_tiles_of_layers: (0..stack.layers.len()).map(|_| None).collect(),
            digests_of_replayed_records: vec![None; stack.layers.len()],
            ..TilePass::default()
        };
        for (group_index, group) in stack.groups.iter().enumerate() {
            // A group whose layers are all below the base layer holds nothing.
            let has_layer_from_base_up = (stack.base_layer..stack.layers.len()).any(
                |layer| matches!(stack.layers[layer].content, LayerContent::Painted { group: of } if of == group_index),
            );
            if !has_layer_from_base_up {
                continue;
            }
            let space = stack.nodes[group.node].space;
            let compositing_of_group = compositing_of_layer(stack, group.layer_below_every_cut, composite_offsets);
            let shown_rect = rect_shown_of(&compositing_of_group, viewport_rect);
            let known =
                self.layers.keyed_groups.iter().position(|known| {
                    known.stack_generation == self.layers.stack_generation && known.group == group_index
                });
            let mut keyed = match known {
                Some(index) => self.layers.keyed_groups.swap_remove(index),
                None => KeyedGroupOfFrame {
                    stack_generation: self.layers.stack_generation,
                    group: group_index,
                    display_list: frame.display_list.clone(),
                    geometry: geometries[space].clone(),
                    keys: GroupTileKeys::default(),
                    shown_tiles: Vec::new(),
                },
            };
            if known.is_none()
                || !Rc::ptr_eq(&keyed.display_list, frame.display_list)
                || !Rc::ptr_eq(&keyed.geometry, &geometries[space])
            {
                raster_spaces[space].bind_group(&mut keyed.keys, stack, group_index);
                keyed.display_list = frame.display_list.clone();
                keyed.geometry = geometries[space].clone();
            }
            let keying = raster_spaces[space].key_shown_tiles(
                &mut keyed.keys,
                stack,
                group_index,
                constants_of_layer,
                shown_rect,
                tile_area_of(space, shown_rect, viewport_rect),
                frame_in_space(space, compositing_of_group.offset, viewport_rect),
            );
            self.layers.tiles_binned_since_last_check += keying.cells_binned as u64;
            self.layers.tiles_binned_by_this_frame += keying.cells_binned as u64;
            keyed.shown_tiles = keyed
                .keys
                .tiles_within(shown_rect)
                .map(|tile| (tile.coord, tile.layer))
                .collect();
            let keyed_group = pass.groups.len();
            for (tile_index, &(coord, layer)) in keyed.shown_tiles.iter().enumerate() {
                if layer < stack.base_layer {
                    continue;
                }
                let tile = keyed.tile(tile_index);
                debug_assert_eq!(tile.coord, coord);
                let mut rect_needed = if tile.layer == stack.base_layer {
                    tile.coord.cell_rect()
                } else {
                    tile.content_bounds
                };
                if space == VIEWPORT_RASTER_SPACE {
                    rect_needed = rect_needed.intersected(viewport_rect);
                }
                if rect_needed.is_empty() {
                    continue;
                }
                let shown = pass.shown_tiles_of_layers[tile.layer].get_or_insert_with(|| ShownTilesOfLayer {
                    group: keyed_group,
                    tiles: Vec::new(),
                    rects_needed: Vec::new(),
                });
                shown.tiles.push(tile_index);
                shown.rects_needed.push(rect_needed);
            }
            pass.groups.push(keyed);
        }

        let chunks_placed = raster_spaces
            .iter()
            .map(|raster_space| raster_space.chunks_placed())
            .sum::<u64>();
        self.layers.chunks_placed_since_last_check += chunks_placed;
        self.layers.chunks_placed_by_this_frame += chunks_placed;

        let mut stale_rect = IntRect::default();
        // A composited effect that changed, or moved within its space, as its digest then says, composited elsewhere or
        // otherwise before. One that reads past each of its pixels reads what is composited below it all over its read
        // area.
        let mut read_areas_of_composited_effects = Vec::new();
        for composited in &stack.composited_effects {
            let node = &stack.nodes[composited.node];
            let offset = composite_offsets[node.space];
            let clip_in_viewport = |rect: IntRect| {
                let rect = translated(rect, offset.x, offset.y).intersected(viewport_rect);
                match stack.composite_clip_of_node_in_viewport(composited.node, composite_offsets) {
                    Some(clip) => rect.intersected(clip),
                    None => rect,
                }
            };
            let region_in_viewport = match node.region_in_parent {
                Reach::Within(region) => clip_in_viewport(region),
                Reach::Nothing => IntRect::default(),
                Reach::Anywhere => clip_in_viewport(viewport_rect),
            };
            let digest = raster_spaces[node.space].digest_of_composited_effect(composited.effect);
            if self.layers.digests_of_composited_effects.get(&node.key) != Some(&digest) {
                stale_rect = stale_rect.united(region_in_viewport);
            }
            pass.digests_of_composited_effects.insert(node.key, digest);
            // A filter that spreads what the effect composites reads all of it, which a frame painting part of the
            // effect inside it would cut.
            if composited.filter_spreads_what_it_composites {
                read_areas_of_composited_effects.push(region_in_viewport);
            } else if composited.read_area != Reach::Nothing {
                let read_area_in_this_frame =
                    match backdrop_filter_read_area(tree, &geometries[node.space], composited.effect) {
                        Reach::Within(read_area) => clip_in_viewport(read_area),
                        Reach::Nothing => IntRect::default(),
                        Reach::Anywhere => region_in_viewport,
                    };
                read_areas_of_composited_effects.push(read_area_in_this_frame);
            }
        }
        for compositing in &layer_update.compositing {
            let Some(layer_index) = stack.layers.iter().position(|layer| layer.key == compositing.key) else {
                continue;
            };
            let layer = &stack.layers[layer_index];
            let clip_in_viewport = |rect: IntRect| {
                let rect = translated(rect, compositing.offset.x, compositing.offset.y).intersected(viewport_rect);
                compositing.clip.map_or(rect, |clip| rect.intersected(clip))
            };
            let state = self.layers.states.get(&layer.key);
            if let LayerContent::Media {
                record_span, region, ..
            } = &layer.content
            {
                let space = stack.space_of_layer(layer_index);
                let digest = raster_spaces[space].digest_of_records_within(*record_span);
                if !compositing.shown_by_window_server
                    && state.and_then(|state| state.digest_of_replayed_records) != Some(digest)
                {
                    stale_rect = stale_rect.united(clip_in_viewport(*region));
                }
                pass.digests_of_replayed_records[layer_index] = Some(digest);
                continue;
            }
            let shown = pass.shown_tiles_of_layers[layer_index].as_ref();
            let Some(state) = state else {
                if let Some(shown) = shown {
                    for rect_needed in &shown.rects_needed {
                        stale_rect = stale_rect.united(clip_in_viewport(*rect_needed));
                    }
                }
                continue;
            };
            let shown_rect = rect_shown_of(compositing, viewport_rect);
            let tile_pool = &self.layers.tile_pool;
            // Tiles the layer held where it shows that it no longer holds go, and what they showed is painted again.
            for (coord, key) in &state.tiles {
                let Some(tile) = tile_pool.get(key) else {
                    continue;
                };
                if tile.rect.intersected(shown_rect).is_empty() {
                    continue;
                }
                let still_held = shown.is_some_and(|shown| {
                    shown
                        .tiles
                        .iter()
                        .any(|&tile_index| pass.groups[shown.group].tile(tile_index).coord == *coord)
                });
                if !still_held {
                    stale_rect = stale_rect.united(clip_in_viewport(tile.rect));
                }
            }
            let Some(shown) = shown else {
                continue;
            };
            let keyed = &pass.groups[shown.group];
            // A tile the layer finds in the pool under another key than it held at the cell is not rastered, but what it
            // shows changed.
            for (&tile_index, rect_needed) in shown.tiles.iter().zip(&shown.rects_needed) {
                let tile = keyed.tile(tile_index);
                let kept = state.tiles.get(&tile.coord) == Some(&tile.key)
                    && tile_pool
                        .get(&tile.key)
                        .is_some_and(|held| held.rect.contains_rect(*rect_needed) && held.holds_current_pixels);
                if !kept {
                    stale_rect = stale_rect.united(clip_in_viewport(*rect_needed));
                }
            }
        }
        layer_update.tile_pass = Some(pass);
        layer_update.read_areas_of_composited_effects = read_areas_of_composited_effects;
        stale_rect
    }

    fn why_window_server_shows_no_media_of_this_context(
        &self,
        env: &PresentingEnvironment<'_>,
    ) -> Option<WhyMediaIsReplayed> {
        if !self.shown_in_remote_layer {
            return Some(WhyMediaIsReplayed::ContextIsNotShownInARemoteLayer);
        }
        if !env.has_gpu {
            return Some(WhyMediaIsReplayed::NoGpu);
        }
        if self.presents_from_cpu_memory {
            return Some(WhyMediaIsReplayed::ContextIsPresentedFromCpuMemory);
        }
        debug_assert!(self.remote_layer_may_show_media(env.has_gpu));
        None
    }

    // Why the window server cannot show the content of a canvas, video or nested context layer by itself, if it cannot.
    fn why_window_server_does_not_show(
        &self,
        env: &PresentingEnvironment<'_>,
        layer: &StackLayer,
        live_content: &LiveContent,
    ) -> Option<WhyMediaIsReplayed> {
        let LayerContent::Media {
            content,
            window_server_placement,
            ..
        } = &layer.content
        else {
            return None;
        };
        let placement = match window_server_placement {
            Some(Ok(placement)) => placement,
            Some(Err(rejection)) => return Some(WhyMediaIsReplayed::Placement(*rejection)),
            None => return Some(WhyMediaIsReplayed::NoPlacement(*content)),
        };
        let (kind, id) = match content {
            LiveMedia::Canvas(id) => {
                let has_presentable_surface = live_content
                    .canvases
                    .get(&id.0)
                    .is_some_and(|canvas| canvas.has_presentable_surface);
                if !has_presentable_surface {
                    return Some(WhyMediaIsReplayed::CanvasHasNoPresentableSurface);
                }
                (MediaKind::Canvas, id.0)
            }
            LiveMedia::VideoFrame(id) => {
                let Some(sink) = live_content.video_sinks.get(&id.0).filter(|sink| sink.has_frame) else {
                    return Some(WhyMediaIsReplayed::VideoHasNoFrame);
                };
                if !env.window_server_may_show_high_dynamic_range && sink.frame_holds_high_dynamic_range {
                    return Some(WhyMediaIsReplayed::HighDynamicRangeIsNotShownNow);
                }
                (MediaKind::Video, id.0)
            }
            LiveMedia::ImageFrame(_) | LiveMedia::CompositedContext(_) => {
                return Some(WhyMediaIsReplayed::NoPlacement(*content));
            }
        };
        if !env.raster.remote_layer_can_show(
            env.context_id,
            kind,
            id,
            &sublayer_placement_of(placement),
            self.device_pixel_ratio,
        ) {
            return Some(WhyMediaIsReplayed::WindowServerCannotShowThePlacement);
        }
        None
    }

    // Works out the compositor layers overlay of the frame, while it is switched on.
    #[allow(clippy::too_many_arguments)]
    fn take_layer_overlay_frame(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        stack: &LayerStack,
        compositing: &[CompositingOfLayer],
        shown_by_window_server: bool,
        tiles_painted_straight: bool,
        live_content: &LiveContent,
    ) {
        if self.layer_overlay.overlay().is_none() {
            return;
        }
        let context_shows_no_media = self.why_window_server_shows_no_media_of_this_context(env);
        let layers = compositing
            .iter()
            .filter_map(|compositing| {
                let index = stack.layers.iter().position(|layer| layer.key == compositing.key)?;
                let layer = &stack.layers[index];
                let content = match &layer.content {
                    LayerContent::Painted { .. } => LayerOfFrameContent::Painted {
                        held_tiles: self
                            .layers
                            .states
                            .get(&compositing.key)
                            .map(|state| {
                                state
                                    .tiles
                                    .iter()
                                    .filter_map(|(coord, key)| Some((*coord, self.layers.tile_pool.get(key)?.rect)))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    LayerContent::Media { region, .. } => LayerOfFrameContent::Media {
                        region: *region,
                        presentation: if shown_by_window_server && compositing.shown_by_window_server {
                            MediaPresentation::ShownByWindowServer
                        } else {
                            let why = context_shows_no_media
                                .or_else(|| self.why_window_server_does_not_show(env, layer, live_content))
                                .unwrap_or(if tiles_painted_straight {
                                    WhyMediaIsReplayed::TilesWerePaintedStraightIntoTheFrame
                                } else if !stack.composited_groups_around_layer(index).is_empty() {
                                    WhyMediaIsReplayed::InsideAnEffectTheCompositorComposites
                                } else {
                                    WhyMediaIsReplayed::UnderALayerTheFrameHolds
                                });
                            MediaPresentation::Replayed { why: why.to_string() }
                        },
                    },
                };
                Some(LayerOfFrame {
                    key: compositing.key,
                    name: stack.name_of_layer(index),
                    composite_offset: compositing.offset,
                    composite_clip: compositing.clip,
                    content,
                })
            })
            .collect();
        let overlay_frame = FrameOfLayerOverlay {
            viewport_rect: self.raster_rect(),
            painted_at: frame.frame_inputs.painted_at,
            layers,
            single_layer_reason: stack.unlayered_reason.map(|reason| reason.to_string()),
            composited_effects_and_what_got_no_layer: stack
                .names_of_composited_effects()
                .chain(stack.names_of_media_without_layers())
                .chain(stack.names_of_effects_reading_in_their_tiles())
                .collect(),
            counts: FrameCounts {
                tiles_rastered: self.layers.tiles_rasterized_by_this_frame,
                tiles_binned: self.layers.tiles_binned_by_this_frame,
                chunks_placed: self.layers.chunks_placed_by_this_frame,
                stack_changed_shape: self.layers.stack_changed_shape_with_this_frame,
            },
        };
        let device_pixel_ratio = self.device_pixel_ratio;
        let shaper = env.host.overlay_label_shaper(env.context_id);
        if let Some(overlay) = self.layer_overlay.overlay_mut() {
            overlay.take_frame(overlay_frame, device_pixel_ratio, &shaper);
        }
    }

    /// Takes in what the frame painted of its layers: how it composited them, and which layers left the stack, whose
    /// tiles stay in the pool for a while.
    pub(super) fn commit_layer_update(&mut self, env: &PresentingEnvironment<'_>, layer_update: LayerUpdate) {
        let layers = &mut self.layers;
        layers.window_server_shows_media = layer_update.shown_by_window_server;
        if let Some(tile_pass) = &layer_update.tile_pass
            && let Some(stack) = &layers.stack
        {
            for (layer, digest) in stack.layers.iter().zip(&tile_pass.digests_of_replayed_records) {
                if digest.is_some()
                    && let Some(state) = layers.states.get_mut(&layer.key)
                {
                    state.digest_of_replayed_records = *digest;
                }
            }
        }
        layers.states.retain(|key, state| {
            let in_stack = layer_update
                .compositing
                .iter()
                .any(|compositing| compositing.key == *key);
            if !in_stack && let Some(presentation) = &state.presentation {
                env.raster
                    .drop_presentable_surface_pool(env.context_id, presentation.pool);
            }
            in_stack
        });
        layers.compositing_of_latest_frame = layer_update.compositing;
        if let Some(tile_pass) = layer_update.tile_pass {
            layers.keyed_groups = tile_pass.groups;
            layers.digests_of_composited_effects = tile_pass.digests_of_composited_effects;
        }
        layers.evict_spare_tiles(env);
    }

    /// Paints the frame from its layers over `damage_rect`, and answers the rect it painted, which grows to the whole
    /// raster where the frame has to hold what the window server was to show above it.
    pub(super) fn paint_layered_frame(
        &mut self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
        layer_update: &mut LayerUpdate,
        live_content: &LiveContent,
    ) -> IntRect {
        let stack = self.layers.stack.take().expect("a layered frame has a stack");
        let painted_rect = self.paint_layers(env, surface, frame, damage_rect, layer_update, &stack, live_content);
        self.layers.stack = Some(stack);
        painted_rect
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_layers(
        &mut self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        mut damage_rect: IntRect,
        layer_update: &mut LayerUpdate,
        stack: &LayerStack,
        live_content: &LiveContent,
    ) -> IntRect {
        let raster = env.raster;
        let surface_clear_color = frame.display_list.surface_clear_color().unwrap_or(Color::TRANSPARENT);
        raster.start_raster_cache_frame(env.context_id);
        // A store can miss the damage of several frames, which joined can reach part of what a composited effect reads.
        damage_rect = layer_update.damage_grown_over_what_composited_effects_read(damage_rect);
        let tile_pass = layer_update
            .tile_pass
            .as_ref()
            .expect("a layered frame worked out its tiles before it was painted");
        self.layers.tiles_rasterized_by_this_frame = 0;
        let holders_of_tiles = self.layers.holders_of_tiles(tile_pass);
        let mut tile_pool = std::mem::take(&mut self.layers.tile_pool);
        let mut painted_straight = Vec::new();
        for compositing in &layer_update.compositing {
            let Some(layer_index) = stack.layers.iter().position(|layer| layer.key == compositing.key) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(entry) = self.layers.states.entry(compositing.key) {
                entry.insert(LayerState::default());
                self.layers.layers_created_since_last_check += 1;
            }
            if matches!(stack.layers[layer_index].content, LayerContent::Media { .. }) {
                continue;
            }
            let clear_color = if layer_index == stack.base_layer {
                surface_clear_color
            } else {
                Color::TRANSPARENT
            };
            let mut state = self
                .layers
                .states
                .remove(&compositing.key)
                .expect("the state of the layer was just made sure of");
            painted_straight.extend(self.raster_layer_tiles(
                env,
                surface,
                frame,
                stack,
                layer_index,
                compositing,
                &mut state,
                &mut tile_pool,
                &holders_of_tiles,
                tile_pass,
                clear_color,
            ));
            self.layers.states.insert(compositing.key, state);
        }
        self.layers.tile_pool = tile_pool;

        // The overlay goes into the surface the window server shows the overlays from, so it is worked out first, as if
        // that went well.
        let expected_shown_by_window_server = layer_update.shown_by_window_server && painted_straight.is_empty();
        self.take_layer_overlay_frame(
            env,
            frame,
            stack,
            &layer_update.compositing,
            expected_shown_by_window_server,
            !painted_straight.is_empty(),
            live_content,
        );

        // The window server shows the layers it can show from surfaces of its own, and the overlays above them, unless
        // some could not be made, when the frame holds all of it instead.
        if layer_update.shown_by_window_server {
            let shown = painted_straight.is_empty()
                && self.present_painted_layers_to_window_server(env, stack, &layer_update.compositing, surface)
                && self.present_compositor_overlays_to_window_server(env, frame, surface);
            if !shown {
                layer_update.shown_by_window_server = false;
                for compositing in &mut layer_update.compositing {
                    compositing.shown_by_window_server = false;
                }
                damage_rect = self.raster_rect();
            }
        }
        if layer_update.shown_by_window_server != expected_shown_by_window_server {
            self.take_layer_overlay_frame(
                env,
                frame,
                stack,
                &layer_update.compositing,
                layer_update.shown_by_window_server,
                !painted_straight.is_empty(),
                live_content,
            );
        }

        let save_count = raster.save(surface);
        raster.clip_rect(surface, damage_rect);
        if let Some(base) = layer_update.compositing.first()
            && let Some(state) = self.layers.states.get(&base.key)
        {
            composite_layer_tiles(
                raster,
                surface,
                state,
                &self.layers.tile_pool,
                damage_rect,
                SurfaceBlend::Source,
                base.offset,
                base.clip,
            );
        }
        self.composite_layers_above_base_of_stack(
            env,
            surface,
            frame,
            damage_rect,
            stack,
            &layer_update.compositing,
            false,
        );
        if !layer_update.shown_by_window_server {
            for rect in painted_straight {
                let rect = rect.intersected(damage_rect);
                if rect.is_empty() {
                    continue;
                }
                let save_count = raster.save(surface);
                raster.clip_rect(surface, rect);
                raster.clear(surface, surface_clear_color);
                self.apply_raster_transform(raster, surface);
                replay_into(env, surface, frame, ReplaySelection::default());
                raster.restore_to_count(surface, save_count);
            }
            self.apply_raster_transform(raster, surface);
            paint_overlays(env, surface, frame.frame_inputs, true);
            self.paint_layer_overlay(env, surface);
        }
        raster.restore_to_count(surface, save_count);
        damage_rect
    }

    // Answers where in the raster the tiles that could not be allocated or rastered show, which the frame paints straight
    // instead, and which are tried again with the next frame. A tile the pool holds current pixels of for its key is not
    // rastered again, whichever layer rastered it.
    #[allow(clippy::too_many_arguments)]
    fn raster_layer_tiles(
        &mut self,
        env: &PresentingEnvironment<'_>,
        like: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        stack: &LayerStack,
        layer_index: usize,
        compositing: &CompositingOfLayer,
        state: &mut LayerState,
        tile_pool: &mut TilePool,
        holders_of_tiles: &HashMap<TileKey, usize>,
        tile_pass: &TilePass,
        clear_color: Color,
    ) -> Vec<IntRect> {
        let raster = env.raster;
        let viewport_rect = tile_pass.viewport_rect;
        let shown_in_raster = |rect: IntRect| {
            let rect = translated(rect, compositing.offset.x, compositing.offset.y).intersected(viewport_rect);
            compositing.clip.map_or(rect, |clip| rect.intersected(clip))
        };
        let space = stack.space_of_layer(layer_index);
        let shown = tile_pass.shown_tiles_of_layers[layer_index].as_ref();
        let shown_rect = rect_shown_of(compositing, viewport_rect);
        let tile_area = tile_area_of(space, shown_rect, viewport_rect);
        let keyed = shown.map(|shown| &tile_pass.groups[shown.group]);
        let shown_coords: Vec<TileCoord> = match (shown, keyed) {
            (Some(shown), Some(keyed)) => shown
                .tiles
                .iter()
                .map(|&tile_index| keyed.tile(tile_index).coord)
                .collect(),
            _ => Vec::new(),
        };

        // Tiles stay where the layer may show them later, but not where it shows and no longer holds anything.
        let tile_count = state.tiles.len();
        state.tiles.retain(|coord, key| {
            !coord.cell_rect().intersected(tile_area).is_empty()
                && tile_pool
                    .get(key)
                    .is_some_and(|tile| tile.rect.intersected(shown_rect).is_empty() || shown_coords.contains(coord))
        });
        let mut tiles_changed = state.tiles.len() != tile_count;

        let (Some(shown), Some(keyed)) = (shown, keyed) else {
            if tiles_changed {
                self.layers.last_content_version += 1;
                state.content_version = self.layers.last_content_version;
            }
            return Vec::new();
        };
        let painted_at = frame.frame_inputs.painted_at;
        let records_rasters = self.layer_overlay.overlay().is_some();
        let mut causes_of_rasters: Vec<(usize, RasterCause)> = Vec::new();
        let mut unrastered = Vec::new();
        let mut tiles_to_raster = Vec::new();
        for (&tile_index, &rect_needed) in shown.tiles.iter().zip(&shown.rects_needed) {
            let keyed_tile = keyed.tile(tile_index);
            let (coord, key) = (keyed_tile.coord, keyed_tile.key);
            let held_key = state.tiles.get(&coord).copied();
            let cause = records_rasters.then(|| {
                let held = held_key.and_then(|held_key| Some((tile_pool.get(&held_key)?, held_key == key)));
                raster_cause_of(held, rect_needed, raster, like)
            });
            let fits = |tile: &PooledTile| {
                tile.rect.contains_rect(rect_needed) && raster.surfaces_share_backend(&tile.surface, like)
            };
            if held_key != Some(key) {
                state.tiles.insert(coord, key);
                tiles_changed = true;
            }
            match tile_pool.get(&key) {
                Some(tile) if fits(tile) && tile.holds_current_pixels => continue,
                Some(tile) if fits(tile) => {}
                _ => {
                    // The surface of the tile the layer held at the cell takes the new key where no other layer holds
                    // that tile or shows it now, as its pixels then serve nothing, and where it holds the very part
                    // of the cell a new surface would: the GPU antialiases some content, such as rounded clips, by
                    // where in its surface the content lands, so a surface holding another part would raster it
                    // unlike a new one.
                    let surface_held_by_this_layer_alone = held_key
                        .filter(|held_key| {
                            *held_key != key
                                && holders_of_tiles.get(held_key).copied().unwrap_or(0) <= 1
                                && tile_pool.get(held_key).is_some_and(|held| {
                                    held.rect == rect_needed && raster.surfaces_share_backend(&held.surface, like)
                                })
                        })
                        .and_then(|held_key| tile_pool.remove(&held_key))
                        .map(|tile| tile.surface);
                    let surface = surface_held_by_this_layer_alone
                        .or_else(|| create_surface_like(raster, size_of(rect_needed), like));
                    let Some(surface) = surface else {
                        state.tiles.remove(&coord);
                        tiles_changed = true;
                        unrastered.push(shown_in_raster(rect_needed));
                        if let Some(overlay) = self.layer_overlay.overlay_mut() {
                            overlay.record_raster(
                                compositing.key,
                                coord,
                                RasterCause::PaintedStraightIntoTheFrame,
                                rect_needed,
                                painted_at,
                            );
                        }
                        continue;
                    };
                    tile_pool.insert(
                        key,
                        PooledTile {
                            surface,
                            rect: rect_needed,
                            holds_current_pixels: false,
                            last_held_at: env.now,
                        },
                    );
                }
            }
            tiles_to_raster.push(tile_index);
            if let Some(cause) = cause {
                causes_of_rasters.push((tile_index, cause));
            }
        }
        if tiles_changed || !tiles_to_raster.is_empty() {
            self.layers.last_content_version += 1;
            state.content_version = self.layers.last_content_version;
        }
        if tiles_to_raster.is_empty() {
            return unrastered;
        }
        self.layers.tiles_rasterized_since_last_check += tiles_to_raster.len() as u64;
        self.layers.tiles_rasterized_by_this_frame += tiles_to_raster.len() as u64;
        self.layers.most_tiles_rasterized_by_a_frame_since_last_check = self
            .layers
            .most_tiles_rasterized_by_a_frame_since_last_check
            .max(self.layers.tiles_rasterized_by_this_frame);
        let geometry = &tile_pass.groups[shown.group].geometry;
        let clips_left_to_composite =
            (!geometry.clips_left_to_composite.is_empty()).then_some(&geometry.clips_left_to_composite[..]);
        let (raster_scale, raster_translation) = (self.raster_scale, self.raster_translation);
        let composited_effect = stack.nodes[stack.layers[layer_index].node].composited_effect;
        let frame_in_space_of_layer = frame_in_space(space, compositing.offset, viewport_rect);
        let replay = |target: &SurfaceHandle, spans: &[ListSpan]| {
            let mut selection = if keyed.keys.replays_whole_list {
                ReplaySelection::default()
            } else {
                replaying_only(spans, false)
            };
            selection.clips_left_to_composite = clips_left_to_composite;
            selection.composited_effect = composited_effect;
            selection.frame_backdrop_blurs_mirror_at = frame_in_space_of_layer.map(IntRect::to_float);
            apply_raster_transform(raster, target, raster_scale, raster_translation);
            replay_with_scroll_offsets(env, target, frame, selection, &geometry.raster_scroll_offsets);
        };

        let mut tiles_rastered = tiles_to_raster.clone();
        // Tiles that the same barriers cut select the same records wherever those reach, so one replay can raster them
        // together.
        let mut batches: Vec<(u64, Vec<usize>)> = Vec::new();
        for &tile_index in &tiles_to_raster {
            let cuts = keyed.tile(tile_index).cuts;
            match batches.iter_mut().find(|(batch_cuts, _)| *batch_cuts == cuts) {
                Some((_, batch)) => batch.push(tile_index),
                None => batches.push((cuts, vec![tile_index])),
            }
        }
        let pooled = |tile_pool: &TilePool, tile_index: usize| {
            tile_pool
                .get(&keyed.tile(tile_index).key)
                .map(|tile| (tile.surface.clone(), tile.rect))
        };
        for (cuts, batch) in &batches {
            let largest_margin = batch.iter().try_fold(0i32, |largest, &tile_index| {
                Some(largest.max(keyed.tile(tile_index).raster_margin_in_device_pixels?))
            });
            if largest_margin == Some(0) && batch.len() == 1 {
                let keyed_tile = keyed.tile(batch[0]);
                let spans = keyed.keys.record_spans_of_tiles([keyed_tile]);
                let (surface, rect) = pooled(tile_pool, batch[0]).expect("a tile to raster is in the pool");
                let save_count = raster.save(&surface);
                raster.translate(&surface, -rect.x as f32, -rect.y as f32);
                raster.clip_rect(&surface, rect);
                raster.clear(&surface, clear_color);
                replay(&surface, &spans);
                raster.restore_to_count(&surface, save_count);
                continue;
            }
            // Where how far a backdrop filter reads is not known, every shown tile that the same barriers cut is
            // replayed, so that what it reads is there.
            let tiles_read: Vec<usize> = match largest_margin {
                Some(_) => batch.clone(),
                None => shown
                    .tiles
                    .iter()
                    .copied()
                    .filter(|&tile_index| keyed.tile(tile_index).cuts == *cuts)
                    .collect(),
            };
            let margin = largest_margin.unwrap_or(0);
            let rect_held_by_tiles = |tile_indices: &[usize]| {
                tile_indices.iter().fold(IntRect::default(), |bounds, &tile_index| {
                    match pooled(tile_pool, tile_index) {
                        Some((_, rect)) => bounds.united(rect),
                        None => bounds,
                    }
                })
            };
            let mut bounds = rect_held_by_tiles(&tiles_read).inflated_edges(margin, margin, margin, margin);
            if space == VIEWPORT_RASTER_SPACE {
                bounds = bounds.intersected(viewport_rect);
            }
            bounds = bounds.united(rect_held_by_tiles(batch));
            let spans = keyed
                .keys
                .record_spans_of_tiles(tiles_read.iter().map(|&tile_index| keyed.tile(tile_index)));
            let tiles: Vec<(SurfaceHandle, IntRect)> = batch
                .iter()
                .map(|&tile_index| pooled(tile_pool, tile_index).expect("a tile to raster is in the pool"))
                .collect();
            let scratch = self.layers.tile_raster_surface_of_size(env, size_of(bounds), like);
            if let Some(scratch) = &scratch {
                raster_through_scratch(raster, scratch, &tiles, bounds, clear_color, &|target| {
                    replay(target, &spans);
                });
            }
            let rastered = scratch.is_some();
            if !rastered {
                for &tile_index in batch {
                    let keyed_tile = keyed.tile(tile_index);
                    let tile = tile_pool
                        .get_mut(&keyed_tile.key)
                        .expect("a tile to raster is in the pool");
                    tile.holds_current_pixels = false;
                    unrastered.push(shown_in_raster(tile.rect));
                    if let Some(overlay) = self.layer_overlay.overlay_mut() {
                        overlay.record_raster(
                            compositing.key,
                            keyed_tile.coord,
                            RasterCause::PaintedStraightIntoTheFrame,
                            tile.rect,
                            painted_at,
                        );
                    }
                }
                tiles_rastered.retain(|tile_index| !batch.contains(tile_index));
            }
        }
        for &tile_index in &tiles_rastered {
            if let Some(tile) = tile_pool.get_mut(&keyed.tile(tile_index).key) {
                tile.holds_current_pixels = true;
            }
        }
        if let Some(overlay) = self.layer_overlay.overlay_mut() {
            for (tile_index, cause) in causes_of_rasters {
                if !tiles_rastered.contains(&tile_index) {
                    continue;
                }
                let keyed_tile = keyed.tile(tile_index);
                if let Some(tile) = tile_pool.get(&keyed_tile.key) {
                    overlay.record_raster(compositing.key, keyed_tile.coord, cause, tile.rect, painted_at);
                }
            }
        }
        unrastered
    }

    /// Composites the layers above the base layer that the window server showed with the latest frame into the
    /// surface, which holds the rest of the frame, as for a snapshot of the frame on screen.
    pub(super) fn composite_layers_shown_by_window_server(
        &self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
    ) {
        let Some(stack) = &self.layers.stack else {
            return;
        };
        self.composite_layers_above_base_of_stack(
            env,
            surface,
            frame,
            damage_rect,
            stack,
            &self.layers.compositing_of_latest_frame,
            true,
        );
    }

    // The layers above the base layer that the window server shows, or those the frame holds, go over it in the order
    // of the stack, inside the effects the compositor composites them inside, which it enters and leaves as the bands
    // of their nodes start and end.
    #[allow(clippy::too_many_arguments)]
    fn composite_layers_above_base_of_stack(
        &self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
        stack: &LayerStack,
        compositing: &[CompositingOfLayer],
        shown_by_window_server: bool,
    ) {
        if compositing
            .iter()
            .skip(1)
            .all(|compositing| compositing.shown_by_window_server != shown_by_window_server)
        {
            return;
        }
        let tile_pool = &self.layers.tile_pool;
        composite_inside_effects_of_frame(env, surface, frame, self.raster_transform(), |compositing_session| {
            let mut entered_groups: Vec<usize> = Vec::new();
            for compositing in compositing
                .iter()
                .skip(1)
                .filter(|compositing| compositing.shown_by_window_server == shown_by_window_server)
            {
                let Some(layer_index) = stack.layers.iter().position(|layer| layer.key == compositing.key) else {
                    continue;
                };
                let groups = stack.composited_groups_around_layer(layer_index);
                let groups_still_entered = entered_groups
                    .iter()
                    .zip(&groups)
                    .take_while(|(entered, group)| entered == group)
                    .count();
                while entered_groups.len() > groups_still_entered {
                    compositing_session.leave_effect();
                    entered_groups.pop();
                }
                for &group in &groups[groups_still_entered..] {
                    let effect = stack.nodes[stack.groups[group].node]
                        .composited_effect
                        .expect("a composited group is of a composited node");
                    compositing_session.enter_effect(effect);
                    entered_groups.push(group);
                }
                let layer = &stack.layers[layer_index];
                if let LayerContent::Media {
                    record_span, region, ..
                } = &layer.content
                {
                    let mut rect = translated(*region, compositing.offset.x, compositing.offset.y);
                    if let Some(clip) = compositing.clip {
                        rect = rect.intersected(clip);
                    }
                    if !rect.intersected(damage_rect).is_empty() {
                        compositing_session.replay_records(*record_span);
                    }
                    continue;
                }
                let Some(state) = self.layers.states.get(&compositing.key) else {
                    continue;
                };
                let area = compositing
                    .clip
                    .map_or(damage_rect, |clip| damage_rect.intersected(clip));
                for tile in state.tiles.values().filter_map(|key| tile_pool.get(key)) {
                    let rect = translated(tile.rect, compositing.offset.x, compositing.offset.y);
                    if rect.intersected(area).is_empty() {
                        continue;
                    }
                    compositing_session.draw_raster(
                        tile.surface.as_ptr(),
                        rect_of_size(size_of(rect)),
                        rect,
                        compositing.clip,
                    );
                }
            }
            for _ in entered_groups {
                compositing_session.leave_effect();
            }
        });
    }

    // Each painted layer goes to the window server as its own surface, which is copied from its tiles when they
    // change. The window server may still show the surface of the frame before, so the copy goes into another one.
    fn present_painted_layers_to_window_server(
        &mut self,
        env: &PresentingEnvironment<'_>,
        stack: &LayerStack,
        compositing: &[CompositingOfLayer],
        like: &SurfaceHandle,
    ) -> bool {
        let raster = env.raster;
        let viewport_rect = rect_of_size(self.viewport_size);
        let layers = &mut self.layers;
        for compositing in compositing
            .iter()
            .skip(1)
            .filter(|compositing| compositing.shown_by_window_server)
        {
            let is_painted = stack
                .layers
                .iter()
                .find(|layer| layer.key == compositing.key)
                .is_some_and(|layer| matches!(layer.content, LayerContent::Painted { .. }));
            if !is_painted {
                continue;
            }
            let Some(state) = layers.states.get_mut(&compositing.key) else {
                continue;
            };
            // A layer of a space that scrolls is shown as far as the viewport shows it, where it composites.
            let held = state.rect_held(&layers.tile_pool);
            let mut rect = translated(held, compositing.offset.x, compositing.offset.y).intersected(viewport_rect);
            if let Some(clip) = compositing.clip {
                rect = rect.intersected(clip);
            }
            if rect.is_empty() {
                if let Some(presentation) = &mut state.presentation {
                    presentation.latest_surface = None;
                }
                continue;
            }
            let presentation = state.presentation.get_or_insert_with(|| {
                let pool = layers.next_presentable_surface_pool;
                layers.next_presentable_surface_pool += 1;
                PaintedLayerPresentation {
                    pool,
                    latest_surface: None,
                    rect: IntRect::default(),
                    content_version: 0,
                }
            });
            if presentation.latest_surface.is_some()
                && presentation.rect == rect
                && presentation.content_version == state.content_version
            {
                continue;
            }
            let pool = presentation.pool;
            let Some(presentable) = raster.next_presentable_surface(env.context_id, pool, size_of(rect), like) else {
                return false;
            };
            let target = raster.painting_surface_of(&presentable);
            raster.clear(&target, Color::TRANSPARENT);
            let save_count = raster.save(&target);
            raster.translate(&target, -rect.x as f32, -rect.y as f32);
            composite_layer_tiles(
                raster,
                &target,
                state,
                &layers.tile_pool,
                rect,
                SurfaceBlend::Source,
                compositing.offset,
                compositing.clip,
            );
            raster.restore_to_count(&target, save_count);
            let content_version = state.content_version;
            let presentation = state
                .presentation
                .as_mut()
                .expect("the presentation was just made sure of");
            presentation.latest_surface = Some(presentable);
            presentation.rect = rect;
            presentation.content_version = content_version;
        }
        true
    }

    fn present_compositor_overlays_to_window_server(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        like: &SurfaceHandle,
    ) -> bool {
        let raster = env.raster;
        let mut bounds = frame.frame_inputs.overlays.bounds;
        if self.layer_overlay.latest_frame().is_some() {
            bounds = bounds.united(self.raster_rect());
        }
        self.layers.overlay_rect = bounds;
        if bounds.is_empty() {
            self.layers.latest_overlay_surface = None;
            return true;
        }
        let bounds_size = IntSize {
            width: bounds.width,
            height: bounds.height,
        };
        let Some(presentable) =
            raster.next_presentable_surface(env.context_id, COMPOSITOR_OVERLAY_SURFACE_POOL, bounds_size, like)
        else {
            return false;
        };
        let target = raster.painting_surface_of(&presentable);
        raster.clear(&target, Color::TRANSPARENT);
        let save_count = raster.save(&target);
        raster.translate(&target, -bounds.x as f32, -bounds.y as f32);
        paint_overlays(env, &target, frame.frame_inputs, true);
        self.paint_layer_overlay(env, &target);
        raster.restore_to_count(&target, save_count);
        self.layers.latest_overlay_surface = Some(presentable);
        true
    }

    pub(super) fn sublayers_shown_by_window_server(&self, live_content: &LiveContent) -> Vec<FfiSublayer> {
        let layers = &self.layers;
        let mut sublayers = Vec::new();
        let Some(stack) = layers.stack.as_ref().filter(|_| layers.window_server_shows_media) else {
            return sublayers;
        };
        for compositing in layers
            .compositing_of_latest_frame
            .iter()
            .skip(1)
            .filter(|compositing| compositing.shown_by_window_server)
        {
            let Some(layer) = stack.layers.iter().find(|layer| layer.key == compositing.key) else {
                continue;
            };
            if let LayerContent::Media {
                content,
                window_server_placement,
                ..
            } = &layer.content
            {
                let Some(Ok(placement)) = window_server_placement else {
                    continue;
                };
                let LayerKey::Media { ordinal, .. } = layer.key else {
                    unreachable!("a media layer has a media key");
                };
                let (kind, id, identity, has_content) = match content {
                    LiveMedia::Canvas(id) => (
                        SublayerContentKind::Canvas,
                        id.0,
                        CANVAS_SUBLAYER_IDENTITY_TAG | id.0,
                        live_content
                            .canvases
                            .get(&id.0)
                            .is_some_and(|canvas| canvas.has_presentable_surface),
                    ),
                    LiveMedia::VideoFrame(id) => (
                        SublayerContentKind::Video,
                        id.0,
                        VIDEO_SUBLAYER_IDENTITY_TAG | id.0,
                        live_content
                            .video_sinks
                            .get(&id.0)
                            .is_some_and(|sink| sink.has_frame && sink.frame_has_surface),
                    ),
                    LiveMedia::ImageFrame(_) | LiveMedia::CompositedContext(_) => continue,
                };
                if !has_content {
                    continue;
                }
                sublayers.push(FfiSublayer {
                    identity: identity | (u64::from(ordinal) << MEDIA_DRAWING_INDEX_SHIFT),
                    content_kind: kind,
                    media_id: id,
                    presentable_surface: std::ptr::null_mut(),
                    placement: sublayer_placement_of(placement),
                });
                continue;
            }
            let Some(presentation) = layers
                .states
                .get(&compositing.key)
                .and_then(|state| state.presentation.as_ref())
            else {
                continue;
            };
            let Some(surface) = &presentation.latest_surface else {
                continue;
            };
            sublayers.push(surface_sublayer(
                PAINTED_LAYER_SUBLAYER_IDENTITY_TAG | u64::from(presentation.pool),
                surface,
                presentation.rect,
            ));
        }
        if let Some(surface) = &layers.latest_overlay_surface {
            sublayers.push(surface_sublayer(
                COMPOSITOR_OVERLAY_SUBLAYER_IDENTITY,
                surface,
                layers.overlay_rect,
            ));
        }
        sublayers
    }
}
