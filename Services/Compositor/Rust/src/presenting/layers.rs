/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a frame is painted in, each cached as tiles from one frame and one list to the next, and what of them the
//! window server shows above the frame by itself. The pixels of a tile are pooled under the tile's content hash, which
//! hashes all they depend on, so a layer finds the tiles another layer or an earlier stack rastered.

use super::layer_overlay::{
    FrameOfLayerOverlay, FrameWorkCounts, HeldTile, LayerOfFrame, LayerOfFrameContent, MediaPresentation, RasterCause,
};
use super::{
    ContextPresenting, FrameToPaint, NestedContexts, PresentingEnvironment, apply_raster_transform,
    composite_inside_effects_of_frame, paint_overlays, rect_of_size, replay_into, replay_with_scroll_offsets,
    replaying_only,
};
use crate::context_scene::ReceivedLayerPlan;
use crate::host::ContextTimer;
use crate::raster_host::{
    FfiSublayer, FfiSublayerPlacement, LiveContent, MediaKind, PresentableSurfaceHandle, RasterHost,
    SublayerContentKind, SurfaceBlend, SurfaceHandle,
};
use libcompositing_rust::display_list::commands::{
    CanvasId, CompositorContextId, ImageFrameResourceId, VideoSinkHandle,
};
use libcompositing_rust::display_list::effect_geometry::spreading_filter_of;
use libcompositing_rust::display_list::layers::{
    BandIndex, BarrierCutsOfCell, LayerContent, LayerIdentity, LayerIndex, LayerStack, LayeringObstacles,
    NodeNamesFromIndices, PlacementNodeBand, PlacementNodeIdentity, PlacementNodeIndex, RasterSpaceAnchorKind,
    RasterSpaceIndex, SpaceToFrame, StackLayer, WhyListIsOneLayer, WindowServerPlacementsOfMedia,
    backdrop_filter_read_reach,
};
use libcompositing_rust::display_list::placement::{RasterSpaceGeometry, RasterSpaces, raster_scroll_offsets};
use libcompositing_rust::display_list::reach::Reach;
use libcompositing_rust::display_list::replay::{CompositeClipsOfRaster, ReplaySelection};
use libcompositing_rust::display_list::storage::ChunkName;
use libcompositing_rust::display_list::summary::LiveContent as LiveMedia;
use libcompositing_rust::display_list::tile_keys::{
    BandTileKeys, ContentHash, FrameStateReadByReplay, LayerRasterSettings, LiveContentVersions, TileCoverage,
    TileKeyingOfList, TileKeyingOfRasterSpace, composited_filter_read_reach_in_device_pixels,
};
use libcompositing_rust::display_list::tiles::{CellRange, TileCoord, entries_within};
use libcompositing_rust::display_list::window_server_placement::{
    WhyWindowServerCannotShowMedia, WindowServerPlacement,
};
use libcompositing_rust::fast_hash::fast_hasher;
use libcompositing_rust::scrolling::DisplayList;
use libcompositing_rust::time::{Duration, MonotonicTime};
use libcompositing_rust::visual_context::{VisualContextTree, WorldTransforms};
use libgfx_rust::{Color, CornerRadii, FloatMatrix4x4, FloatPoint, IntPoint, IntRect, IntSize, ScalingMode};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hasher;
use std::rc::{Rc, Weak};

const TILE_RASTER_SCRATCH_SURFACE_RELEASE_DELAY: Duration = Duration::from_milliseconds(3000);
const TIME_SPARE_TILE_STAYS_POOLED_FOR_CONTENT_THAT_COMES_BACK: Duration = Duration::from_milliseconds(3000);

const PRESENTABLE_SURFACE_POOL_OF_COMPOSITOR_OVERLAYS: u32 = u32::MAX;

const CANVAS_SUBLAYER_IDENTITY_TAG: u64 = 1 << 60;
const VIDEO_SUBLAYER_IDENTITY_TAG: u64 = 2 << 60;
const PAINTED_LAYER_SUBLAYER_IDENTITY_TAG: u64 = 3 << 60;
const COMPOSITOR_OVERLAY_SUBLAYER_IDENTITY: u64 = 4 << 60;
// The checks of a plan keep the ordinal of a media layer below the most media layers a plan makes, so it stays clear of
// the tag.
const MEDIA_ORDINAL_SHIFT_PAST_CANVAS_AND_VIDEO_IDS: u32 = 44;

struct PooledTile {
    pixels: TilePixels,
    held_part_of_cell: IntRect,
    holds_current_pixels: bool,
    last_held_at: MonotonicTime,
}

enum TilePixels {
    Surface {
        surface: SurfaceHandle,
        every_pixel_is_opaque: bool,
    },
    OneColor(Color),
}

impl PooledTile {
    fn holds_current_pixels_over_ignoring_backend(&self, part_of_cell_needed: IntRect) -> bool {
        self.holds_current_pixels && self.held_part_of_cell.contains_rect(part_of_cell_needed)
    }

    fn surface(&self) -> Option<&SurfaceHandle> {
        match &self.pixels {
            TilePixels::Surface { surface, .. } => Some(surface),
            TilePixels::OneColor(_) => None,
        }
    }

    fn coverage(&self) -> TileCoverage {
        match self.pixels {
            TilePixels::Surface {
                every_pixel_is_opaque: true,
                ..
            } => TileCoverage::Opaque,
            TilePixels::Surface {
                every_pixel_is_opaque: false,
                ..
            } => TileCoverage::Unknown,
            TilePixels::OneColor(color) => TileCoverage::OneColor(color),
        }
    }
}

type TilePool = HashMap<ContentHash, PooledTile>;

struct PaintedLayerPresentation {
    presentable_surface_pool: u32,
    latest_surface: Option<PresentableSurfaceHandle>,
    rect_in_frame: IntRect,
    copied_content_version: u64,
    // Under a composite clip, the same rect of the frame shows other tiles once the layer's space scrolls.
    copied_with_compositing: Option<LayerCompositingInFrame>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MediaRecordsReplayedInFrame {
    content_hash: ContentHash,
    reach_in_frame: IntRect,
}

#[derive(Default)]
struct CachedLayer {
    held_tiles: BTreeMap<TileCoord, ContentHash>,
    content_version_of_held_tiles: u64,
    media_replayed_in_latest_frame: Option<MediaRecordsReplayedInFrame>,
    window_server_presentation: Option<PaintedLayerPresentation>,
}

fn pooled_held_tiles<'a>(
    held_tiles: &'a BTreeMap<TileCoord, ContentHash>,
    tile_pool: &'a TilePool,
) -> impl Iterator<Item = (TileCoord, &'a PooledTile)> {
    held_tiles
        .iter()
        .filter_map(|(coord, content_hash)| Some((*coord, tile_pool.get(content_hash)?)))
}

impl CachedLayer {
    fn bounds_of_held_tiles(&self, tile_pool: &TilePool) -> IntRect {
        pooled_held_tiles(&self.held_tiles, tile_pool).fold(IntRect::default(), |bounds, (_, tile)| {
            bounds.united(tile.held_part_of_cell)
        })
    }

    fn bump_content_version_of_held_tiles(&mut self) {
        self.content_version_of_held_tiles = self.content_version_of_held_tiles.wrapping_add(1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhyMediaIsReplayed {
    ContextIsNotShownInARemoteLayer,
    NoGpu,
    ContextIsPresentedFromCpuMemory,
    Placement(WhyWindowServerCannotShowMedia),
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

/// Where a frame composites a layer: offset by the scroll offsets of its space's anchors, within its composite clip
/// and the rounded composite clips inside it, and whether the frame holds it or the window server shows it above the
/// frame.
#[derive(Clone, Debug)]
pub struct LayerCompositingInFrame {
    /// Where the layer is in the stack the frame draws with.
    layer_index: LayerIndex,
    identity: LayerIdentity,
    space_to_frame: SpaceToFrame,
    composite_clip: Option<IntRect>,
    rounded_composite_clips: Rc<[(IntRect, CornerRadii)]>,
    shown_by_window_server: bool,
}

// Two frames composite a layer alike wherever their stacks hold it.
impl PartialEq for LayerCompositingInFrame {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            layer_index: _,
            identity,
            space_to_frame,
            composite_clip,
            rounded_composite_clips,
            shown_by_window_server,
        } = self;
        *identity == other.identity
            && *space_to_frame == other.space_to_frame
            && *composite_clip == other.composite_clip
            && *rounded_composite_clips == other.rounded_composite_clips
            && *shown_by_window_server == other.shown_by_window_server
    }
}

impl LayerCompositingInFrame {
    fn rect_in_frame(&self, rect_in_layer_space: IntRect, within: IntRect) -> IntRect {
        rect_in_frame_of_space(rect_in_layer_space, self.space_to_frame, self.composite_clip, within)
    }

    fn rect_shown_in_layer_space(&self, viewport_rect: IntRect, composited_filter_read_reach: i32) -> IntRect {
        let shown_in_viewport = match self.composite_clip {
            Some(composite_clip) => viewport_rect.intersected(composite_clip),
            None => viewport_rect,
        };
        inflated(
            self.space_to_frame.rect_in_space(shown_in_viewport),
            composited_filter_read_reach,
        )
    }
}

fn any_layer_shown_by_window_server(compositing_of_layers: &[LayerCompositingInFrame]) -> bool {
    compositing_of_layers
        .iter()
        .any(|compositing| compositing.shown_by_window_server)
}

struct ListAndGeometryOfBandTileKeys {
    display_list: Rc<DisplayList>,
    geometry: Rc<RasterSpaceGeometry>,
    band: PlacementNodeBand,
    base_layer: LayerIndex,
}

#[derive(Default)]
struct BandTileKeysOfFrame {
    identities: BandIdentities,
    bound_to_list_and_geometry: Option<ListAndGeometryOfBandTileKeys>,
    tile_keys: BandTileKeys,
}

impl BandTileKeysOfFrame {
    fn bound_geometry(&self) -> &RasterSpaceGeometry {
        &self
            .bound_to_list_and_geometry
            .as_ref()
            .expect("the tile keys of a band of a frame are bound")
            .geometry
    }
}

// The first identity holds while bands of the node painted before this one come or go, the second while the records
// the band starts with change.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct BandIdentities {
    identity_by_first_record_position: u64,
    identity_by_band_ordinal: u64,
}

impl BandIdentities {
    fn of(stack: &LayerStack, band_index: BandIndex, chunk_names: &[ChunkName]) -> Self {
        let band = &stack.bands[band_index.0];
        let node_identity_hash = stack.nodes[band.placement_node_index.0].identity.0;
        let first_record_position = band.first_record_position;
        let mut first_record_position_hasher = fast_hasher();
        first_record_position_hasher.write_u8(1);
        first_record_position_hasher.write_u64(node_identity_hash);
        if let Some(chunk_name) = chunk_names.get(first_record_position.chunk_index.0 as usize) {
            first_record_position_hasher.write_u64(chunk_name.id.0[0]);
            first_record_position_hasher.write_u64(chunk_name.id.0[1]);
            first_record_position_hasher.write_u32(chunk_name.occurrence);
        }
        first_record_position_hasher.write_u32(first_record_position.offset);
        let mut band_ordinal_hasher = fast_hasher();
        band_ordinal_hasher.write_u8(2);
        band_ordinal_hasher.write_u64(node_identity_hash);
        band_ordinal_hasher.write_u32(band.band_ordinal_in_node);
        Self {
            identity_by_first_record_position: first_record_position_hasher.finish(),
            identity_by_band_ordinal: band_ordinal_hasher.finish(),
        }
    }
}

const BAND_IDENTITY_KINDS_IN_MATCHING_ORDER: [fn(&BandIdentities) -> u64; 2] = [
    |identities| identities.identity_by_first_record_position,
    |identities| identities.identity_by_band_ordinal,
];

// Per identity kind, where the tile keys of the latest frame are that have each identity.
struct PositionsOfBandTileKeysByIdentity([HashMap<u64, BTreeSet<usize>>; 2]);

impl PositionsOfBandTileKeysByIdentity {
    fn of(band_tile_keys: &[BandTileKeysOfFrame]) -> Self {
        let mut positions = Self([HashMap::new(), HashMap::new()]);
        for (position, band_tile_keys) in band_tile_keys.iter().enumerate() {
            positions.insert(&band_tile_keys.identities, position);
        }
        positions
    }

    fn insert(&mut self, identities: &BandIdentities, position: usize) {
        for (positions_by_identity, identity_of) in self.0.iter_mut().zip(BAND_IDENTITY_KINDS_IN_MATCHING_ORDER) {
            positions_by_identity
                .entry(identity_of(identities))
                .or_default()
                .insert(position);
        }
    }

    fn remove(&mut self, identities: &BandIdentities, position: usize) {
        for (positions_by_identity, identity_of) in self.0.iter_mut().zip(BAND_IDENTITY_KINDS_IN_MATCHING_ORDER) {
            if let Some(positions) = positions_by_identity.get_mut(&identity_of(identities)) {
                positions.remove(&position);
            }
        }
    }

    fn first_position(&self, identity_kind: usize, identity: u64) -> Option<usize> {
        self.0[identity_kind].get(&identity)?.first().copied()
    }
}

// Per identity kind in order, each band not matched yet takes the first of the latest frame's tile keys that has its
// identity, which leaves the last of them in its place. A band's tile keys are found by their identity, as the bands of
// a received stack are too many to compare each with each.
fn reuse_tile_keys_of_matching_bands(
    band_tile_keys_of_latest_frame: &mut Vec<BandTileKeysOfFrame>,
    identities_of_bands: &[BandIdentities],
) -> Vec<Option<BandTileKeysOfFrame>> {
    let mut reused_by_band: Vec<Option<BandTileKeysOfFrame>> = identities_of_bands.iter().map(|_| None).collect();
    let mut positions = PositionsOfBandTileKeysByIdentity::of(band_tile_keys_of_latest_frame);
    for (identity_kind, identity_of) in BAND_IDENTITY_KINDS_IN_MATCHING_ORDER.into_iter().enumerate() {
        for (reused, identities) in reused_by_band.iter_mut().zip(identities_of_bands) {
            if reused.is_some() {
                continue;
            }
            let Some(position) = positions.first_position(identity_kind, identity_of(identities)) else {
                continue;
            };
            let last_position = band_tile_keys_of_latest_frame.len() - 1;
            positions.remove(&band_tile_keys_of_latest_frame[position].identities, position);
            if position != last_position {
                let last_identities = band_tile_keys_of_latest_frame[last_position].identities;
                positions.remove(&last_identities, last_position);
                positions.insert(&last_identities, position);
            }
            *reused = Some(band_tile_keys_of_latest_frame.swap_remove(position));
        }
    }
    reused_by_band
}

/// The tile keys of the bands of the latest frame that keyed its tiles, painted or not, by the index of their band in
/// the stack it drew with.
enum BandTileKeysOfLatestFrame {
    OfSameStack(Vec<Option<BandTileKeysOfFrame>>),
    OfOtherStack(Vec<Option<BandTileKeysOfFrame>>),
}

impl BandTileKeysOfLatestFrame {
    /// By band index, the tile keys each band of the stack takes: after a frame drawn with the same stack, those of
    /// the same index, and otherwise those found by the identities of the band, or new ones, with the identities of the
    /// band. Answers too whether they are of the same stack.
    fn taken_by_bands(
        self,
        identities_of_bands: impl FnOnce() -> Vec<BandIdentities>,
    ) -> (Vec<Option<BandTileKeysOfFrame>>, bool) {
        match self {
            Self::OfSameStack(band_tile_keys_by_band) => (band_tile_keys_by_band, true),
            Self::OfOtherStack(band_tile_keys_by_band) => {
                let identities_of_bands = identities_of_bands();
                let mut band_tile_keys_of_other_stack = band_tile_keys_by_band.into_iter().flatten().collect();
                let taken_by_band =
                    reuse_tile_keys_of_matching_bands(&mut band_tile_keys_of_other_stack, &identities_of_bands)
                        .into_iter()
                        .zip(identities_of_bands)
                        .map(|(taken, identities)| {
                            Some(BandTileKeysOfFrame {
                                identities,
                                ..taken.unwrap_or_default()
                            })
                        })
                        .collect();
                (taken_by_band, false)
            }
        }
    }
}

struct TileShownInFrame {
    coord: TileCoord,
    content_hash: ContentHash,
    part_of_cell_needed: IntRect,
    cuts: BarrierCutsOfCell,
    raster_margin_in_device_pixels: Option<i32>,
    coverage: TileCoverage,
}

pub struct ShownTilesOfLayer {
    band_index: BandIndex,
    tiles: Vec<TileShownInFrame>,
}

#[derive(Default)]
pub struct FrameTileState {
    viewport_rect: IntRect,
    shown_tiles_by_layer: Vec<Option<ShownTilesOfLayer>>,
    media_replayed_by_layer: Vec<Option<MediaRecordsReplayedInFrame>>,
    composited_filter_read_reach_by_layer: Vec<i32>,
    frame_allows_lcd_text: bool,
    content_hash_by_composited_node: HashMap<PlacementNodeIdentity, ContentHash>,
}

impl FrameTileState {
    fn shown_tiles_of_layer(&self, layer_index: LayerIndex) -> &[TileShownInFrame] {
        self.shown_tiles_by_layer[layer_index.0]
            .as_ref()
            .map_or(&[], |shown| shown.tiles.as_slice())
    }
}

/// What a frame paints of its layers.
pub struct LayerUpdate {
    pub tile_state: Option<FrameTileState>,
    pub composite_offsets: Vec<IntPoint>,
    pub spaces_to_frame: Vec<SpaceToFrame>,
    /// From the base layer up, the layers that can hold something, and where the frame composites each.
    pub compositing_of_layers: Vec<LayerCompositingInFrame>,
    /// What each composited effect that reads past each of its pixels reads and draws as the frame places it.
    pub read_reaches_of_composited_effects_in_frame: Vec<IntRect>,
}

impl LayerUpdate {
    pub fn shown_by_window_server(&self) -> bool {
        any_layer_shown_by_window_server(&self.compositing_of_layers)
    }

    /// The damage grown over all that a composited effect reads wherever it reaches that, as compositing the effect
    /// reads what is composited below it, which the frame painted before holds only under what it composited above.
    pub fn damage_grown_over_what_composited_effects_read(&self, mut damage: IntRect) -> IntRect {
        loop {
            let mut grew = false;
            for &read_reach in &self.read_reaches_of_composited_effects_in_frame {
                if !read_reach.intersected(damage).is_empty() && !damage.contains_rect(read_reach) {
                    damage = damage.united(read_reach);
                    grew = true;
                }
            }
            if !grew {
                return damage;
            }
        }
    }
}

struct CachedRasterSpaceGeometry {
    tree_copy_number: u64,
    tree_copy_without_animation_samples: u64,
    geometry: Rc<RasterSpaceGeometry>,
}

/// A stack, with what compositing its layers reads of it.
#[derive(Clone)]
struct CompositedStack {
    stack: Rc<LayerStack>,
    /// Per layer, the bands it is in, outermost first, whose nodes the compositor composites inside their effects.
    composited_bands_around_layer: Rc<[Vec<BandIndex>]>,
}

impl CompositedStack {
    fn composited_bands_around_layer(&self, layer_index: LayerIndex) -> &[BandIndex] {
        &self.composited_bands_around_layer[layer_index.0]
    }

    /// The layer a frame composited, which a frame worked out after it can hold elsewhere in another stack.
    fn layer_composited_as(&self, compositing: &LayerCompositingInFrame) -> Option<(LayerIndex, &StackLayer)> {
        match self.stack.layers.get(compositing.layer_index.0) {
            Some(layer) if layer.identity == compositing.identity => Some((compositing.layer_index, layer)),
            _ => self.stack.layer_with_identity(compositing.identity),
        }
    }
}

/// The stack that frames draw with, and what they keep of it from one frame to the next while they draw with the same
/// plan, whose trees hold the same nodes and differ only in their values.
struct StackOfFrames {
    /// The plan WebContent sent, or none for one layer that replays the whole list, for a frame that is not drawn as a
    /// plan was made for. One plan stands for one list, one tree without the samples of the compositor's animations,
    /// one viewport, and the scroll offsets the plan reads.
    layer_plan: Option<ReceivedLayerPlan>,
    stack: Rc<LayerStack>,
    viewport_to_raster_transform: FloatMatrix4x4,
    /// Per layer, the bands it is in, outermost first, whose nodes the compositor composites inside their effects.
    composited_bands_around_layer: Rc<[Vec<BandIndex>]>,
    raster_spaces: Option<RasterSpaces>,
    geometry_by_space: Vec<Option<CachedRasterSpaceGeometry>>,
    band_tile_keys_of_latest_frame: BandTileKeysOfLatestFrame,
    /// Per layer, what the layers overlay calls it, worked out by the first frame that draws the overlay.
    names_of_layers_for_overlay: OnceCell<Vec<Rc<str>>>,
}

impl StackOfFrames {
    /// Keeps the geometries of the spaces of the stack the latest frame drew with where the spaces are at the same
    /// anchors under the same raster transform, as the geometry of a space does not depend on the viewport.
    fn new(
        layer_plan: Option<ReceivedLayerPlan>,
        stack: Rc<LayerStack>,
        viewport_to_raster_transform: FloatMatrix4x4,
        latest: Option<StackOfFrames>,
    ) -> Self {
        let mut geometry_by_space: Vec<_> = (0..stack.spaces.len()).map(|_| None).collect();
        let mut band_tile_keys_of_latest_frame = Vec::new();
        if let Some(latest) = latest {
            if latest.viewport_to_raster_transform == viewport_to_raster_transform
                && latest
                    .stack
                    .spaces
                    .iter()
                    .map(|space| space.anchor)
                    .eq(stack.spaces.iter().map(|space| space.anchor))
            {
                geometry_by_space = latest.geometry_by_space;
            }
            let (BandTileKeysOfLatestFrame::OfSameStack(band_tile_keys_by_band)
            | BandTileKeysOfLatestFrame::OfOtherStack(band_tile_keys_by_band)) = latest.band_tile_keys_of_latest_frame;
            band_tile_keys_of_latest_frame = band_tile_keys_by_band;
        }
        Self {
            layer_plan,
            composited_bands_around_layer: (0..stack.layers.len())
                .map(|layer_index| stack.composited_bands_around_layer(LayerIndex(layer_index)))
                .collect(),
            stack,
            viewport_to_raster_transform,
            raster_spaces: None,
            geometry_by_space,
            band_tile_keys_of_latest_frame: BandTileKeysOfLatestFrame::OfOtherStack(band_tile_keys_of_latest_frame),
            names_of_layers_for_overlay: OnceCell::new(),
        }
    }

    fn composited_bands_around_layer(&self, layer_index: LayerIndex) -> &[BandIndex] {
        &self.composited_bands_around_layer[layer_index.0]
    }

    fn composited_stack(&self) -> CompositedStack {
        CompositedStack {
            stack: self.stack.clone(),
            composited_bands_around_layer: self.composited_bands_around_layer.clone(),
        }
    }

    /// The tile keys of a band as the latest frame that keyed its tiles with this stack keyed them.
    fn band_tile_keys_keyed_with_this_stack(&self, band_index: BandIndex) -> Option<&BandTileKeysOfFrame> {
        match &self.band_tile_keys_of_latest_frame {
            BandTileKeysOfLatestFrame::OfSameStack(band_tile_keys_by_band) => {
                band_tile_keys_by_band.get(band_index.0)?.as_ref()
            }
            BandTileKeysOfLatestFrame::OfOtherStack(_) => None,
        }
    }

    /// Per raster space, its geometry under the tree and the scroll offsets of the frame.
    fn geometries_of_spaces(
        &mut self,
        tree: &VisualContextTree,
        scroll_offsets: &[FloatPoint],
    ) -> Vec<Rc<RasterSpaceGeometry>> {
        let stack = &self.stack;
        let mut geometries = Vec::with_capacity(stack.spaces.len());
        for (space_index, cached_geometry) in self.geometry_by_space.iter_mut().enumerate() {
            let raster_space_index = RasterSpaceIndex(space_index);
            // What reads the geometry of a space of a plan that holds nothing that moves within it reads no value the
            // compositor animates, and no scroll offset that the plan leaves out.
            let geometry_readers_leave_out_offset_of_node = self
                .layer_plan
                .as_ref()
                .filter(|_| !stack.space_holds_content_that_moves_within_it(raster_space_index))
                .map(|layer_plan| layer_plan.leaves_out_scroll_offset_of_node.as_slice());
            let raster_scroll_offsets_of_space =
                raster_scroll_offsets(&stack.spaces, raster_space_index, scroll_offsets);
            if let Some(cached) = cached_geometry
                && (cached.tree_copy_number == tree.copy.number()
                    || (geometry_readers_leave_out_offset_of_node.is_some()
                        && cached.tree_copy_without_animation_samples == tree.copy_without_animation_samples()))
                && match geometry_readers_leave_out_offset_of_node {
                    Some(geometry_readers_leave_out_offset_of_node) => cached
                        .geometry
                        .raster_scroll_offsets
                        .iter()
                        .zip(&raster_scroll_offsets_of_space)
                        .enumerate()
                        .all(|(node_index, (cached_offset, offset))| {
                            cached_offset == offset
                                || geometry_readers_leave_out_offset_of_node
                                    .get(node_index)
                                    .copied()
                                    .unwrap_or(false)
                        }),
                    None => cached.geometry.raster_scroll_offsets == raster_scroll_offsets_of_space,
                }
            {
                geometries.push(cached.geometry.clone());
                continue;
            }
            let raster_spaces = self
                .raster_spaces
                .get_or_insert_with(|| RasterSpaces::of_spaces(tree, stack.spaces.clone()));
            let geometry = Rc::new(RasterSpaceGeometry::new(
                tree,
                raster_spaces,
                raster_space_index,
                scroll_offsets,
                self.viewport_to_raster_transform,
            ));
            *cached_geometry = Some(CachedRasterSpaceGeometry {
                tree_copy_number: tree.copy.number(),
                tree_copy_without_animation_samples: tree.copy_without_animation_samples(),
                geometry: geometry.clone(),
            });
            geometries.push(geometry);
        }
        geometries
    }
}

/// What of a list and a tree keeps the tiles of a stack of one layer from replaying only the records that reach them,
/// worked out once for both.
struct LayeringObstaclesOfListAndTree {
    // Keeps the address of the list from being taken by another list.
    display_list: Weak<DisplayList>,
    tree_copy_number: u64,
    obstacles: LayeringObstacles,
}

/// The layers of the frames of a context under their identities, the pool of their tiles, and what of them the window
/// server shows.
#[derive(Default)]
pub struct ContextLayers {
    /// The stack the latest frame was drawn with, painted or not, and what frames keep of it.
    stack_of_frames: Option<StackOfFrames>,
    /// The stack the latest painted frame was drawn with, which a snapshot of the frame on screen and what the window
    /// server shows above it are composited with until another frame is painted.
    stack_of_painted_frame: Option<CompositedStack>,
    layering_obstacles_of_frames_not_drawn_as_planned: Option<LayeringObstaclesOfListAndTree>,
    /// Where the window server can show the media of the stack, as the latest frame worked it out.
    window_server_placements_of_media: WindowServerPlacementsOfMedia,
    cached_layer_by_identity: HashMap<LayerIdentity, CachedLayer>,
    tile_pool: TilePool,
    compositing_of_latest_frame: Vec<LayerCompositingInFrame>,
    tile_raster_scratch_surface: Option<SurfaceHandle>,
    work_counts_of_this_frame: FrameWorkCounts,
    latest_compositor_overlays_surface: Option<PresentableSurfaceHandle>,
    compositor_overlays_rect: IntRect,
    next_presentable_surface_pool: u32,
    content_hash_by_composited_node_of_latest_frame: HashMap<PlacementNodeIdentity, ContentHash>,
}

impl ContextLayers {
    pub fn has_layers(&self) -> bool {
        self.stack_of_painted_frame.is_some()
    }

    /// Whether the window server shows layers above the base layer of the latest frame.
    pub fn window_server_shows_media(&self) -> bool {
        any_layer_shown_by_window_server(&self.compositing_of_latest_frame)
    }

    pub fn compositing_of_latest_frame(&self) -> &[LayerCompositingInFrame] {
        &self.compositing_of_latest_frame
    }

    pub fn spare_tile_eviction_timer_fired(&mut self, env: &PresentingEnvironment<'_>) {
        self.evict_spare_tiles(env);
    }

    fn invalidate_every_tile_and_media_replay(&mut self, compositing_of_layers: &[LayerCompositingInFrame]) {
        for compositing in compositing_of_layers {
            if let Some(cached_layer) = self.cached_layer_by_identity.get_mut(&compositing.identity) {
                cached_layer.media_replayed_in_latest_frame = None;
            }
        }
        for tile in self.tile_pool.values_mut() {
            tile.holds_current_pixels = false;
        }
    }

    /// The stack of one layer that replays the whole list, for a frame that is not drawn as a plan was made for, with
    /// what of the list and the tree keeps its tiles from replaying only the records that reach them, as a plan of one
    /// layer has.
    fn one_layer_stack_of_frame(
        &mut self,
        display_list: &Rc<DisplayList>,
        tree: &VisualContextTree,
        viewport_rect: IntRect,
    ) -> LayerStack {
        let obstacles = match &self.layering_obstacles_of_frames_not_drawn_as_planned {
            Some(of_list_and_tree)
                if of_list_and_tree.tree_copy_number == tree.copy.number()
                    && std::ptr::eq(of_list_and_tree.display_list.as_ptr(), Rc::as_ptr(display_list)) =>
            {
                of_list_and_tree.obstacles
            }
            _ => {
                let obstacles = LayeringObstacles::of(display_list.storage(), tree);
                self.layering_obstacles_of_frames_not_drawn_as_planned = Some(LayeringObstaclesOfListAndTree {
                    display_list: Rc::downgrade(display_list),
                    tree_copy_number: tree.copy.number(),
                    obstacles,
                });
                obstacles
            }
        };
        let mut stack = LayerStack::single_layer(
            viewport_rect,
            display_list.storage().whole_span(),
            Some(WhyListIsOneLayer::FrameIsNotDrawnAsPlanned),
        );
        stack.layering_obstacles = obstacles;
        stack
    }

    /// Takes the stack a frame draws with: the plan WebContent sent, or for a frame that is not drawn as a plan was
    /// made for, one layer that replays the whole list. Keeps what frames kept of the stack where the frame before drew
    /// with the same plan.
    fn take_stack_of_frame(
        &mut self,
        layer_plan: Option<&ReceivedLayerPlan>,
        one_layer: impl FnOnce(&mut Self) -> LayerStack,
        viewport_to_raster_transform: FloatMatrix4x4,
    ) -> Rc<LayerStack> {
        let latest_plan = self
            .stack_of_frames
            .as_ref()
            .and_then(|stack_of_frames| stack_of_frames.layer_plan.as_ref());
        if let Some(layer_plan) = layer_plan
            && latest_plan.is_some_and(|latest_plan| Rc::ptr_eq(&latest_plan.stack, &layer_plan.stack))
        {
            return layer_plan.stack.clone();
        }
        self.work_counts_of_this_frame.took_plan_into_use = layer_plan.is_some();
        let stack = match layer_plan {
            Some(layer_plan) => layer_plan.stack.clone(),
            None => Rc::new(one_layer(self)),
        };
        self.stack_of_frames = Some(StackOfFrames::new(
            layer_plan.cloned(),
            stack.clone(),
            viewport_to_raster_transform,
            self.stack_of_frames.take(),
        ));
        stack
    }

    /// Takes in what a frame that is painted, or that shows nothing new, did with its layers: the stack it drew with,
    /// how it composited the layers, and the media and composited effects it drew, which what reads the frame on screen
    /// reads from then on.
    fn take_in_painted_frame(&mut self, layer_update: LayerUpdate) {
        if let Some(stack_of_frames) = &self.stack_of_frames {
            if let Some(tile_state) = layer_update.tile_state {
                for (layer, media_replayed) in stack_of_frames
                    .stack
                    .layers
                    .iter()
                    .zip(&tile_state.media_replayed_by_layer)
                {
                    if media_replayed.is_some()
                        && let Some(cached_layer) = self.cached_layer_by_identity.get_mut(&layer.identity)
                    {
                        cached_layer.media_replayed_in_latest_frame = *media_replayed;
                    }
                }
                self.content_hash_by_composited_node_of_latest_frame = tile_state.content_hash_by_composited_node;
            }
            self.stack_of_painted_frame = Some(stack_of_frames.composited_stack());
        }
        self.compositing_of_latest_frame = layer_update.compositing_of_layers;
    }

    fn evict_spare_tiles(&mut self, env: &PresentingEnvironment<'_>) {
        let held_content_hashes: HashSet<ContentHash> = self
            .cached_layer_by_identity
            .values()
            .flat_map(|cached_layer| cached_layer.held_tiles.values().copied())
            .collect();
        let surface_pixel_count_of = |tile: &PooledTile| match tile.pixels {
            TilePixels::Surface { .. } => {
                tile.held_part_of_cell.width.max(0) as u64 * tile.held_part_of_cell.height.max(0) as u64
            }
            TilePixels::OneColor(_) => 0,
        };
        let mut pixel_count_of_held_tiles = 0u64;
        let mut spare_tiles: Vec<(MonotonicTime, ContentHash, u64)> = Vec::new();
        self.tile_pool.retain(|content_hash, tile| {
            if held_content_hashes.contains(content_hash) {
                tile.last_held_at = env.now;
                pixel_count_of_held_tiles += surface_pixel_count_of(tile);
                return true;
            }
            if !tile.holds_current_pixels
                || env.now - tile.last_held_at >= TIME_SPARE_TILE_STAYS_POOLED_FOR_CONTENT_THAT_COMES_BACK
            {
                return false;
            }
            spare_tiles.push((tile.last_held_at, *content_hash, surface_pixel_count_of(tile)));
            true
        });
        let spare_tiles_least_recently_held_first = {
            spare_tiles.sort_unstable();
            spare_tiles
        };
        let spare_pixel_budget = pixel_count_of_held_tiles;
        let mut pixel_count_of_spare_tiles: u64 = spare_tiles_least_recently_held_first
            .iter()
            .map(|(_, _, pixel_count)| pixel_count)
            .sum();
        let mut evicted_count = 0;
        for (_, content_hash, pixel_count) in &spare_tiles_least_recently_held_first {
            if pixel_count_of_spare_tiles <= spare_pixel_budget {
                break;
            }
            self.tile_pool.remove(content_hash);
            pixel_count_of_spare_tiles -= pixel_count;
            evicted_count += 1;
        }
        if let Some((least_recently_held_at, _, _)) = spare_tiles_least_recently_held_first.get(evicted_count) {
            env.host.set_timer(
                env.context_id,
                ContextTimer::SpareTileEviction,
                TIME_SPARE_TILE_STAYS_POOLED_FOR_CONTENT_THAT_COMES_BACK - (env.now - *least_recently_held_at),
            );
        }
    }

    fn holder_count_by_content_hash(&self, tile_state: &FrameTileState) -> HashMap<ContentHash, usize> {
        let mut holder_count_by_content_hash: HashMap<ContentHash, usize> = HashMap::new();
        for content_hash in self
            .cached_layer_by_identity
            .values()
            .flat_map(|cached_layer| cached_layer.held_tiles.values())
        {
            *holder_count_by_content_hash.entry(*content_hash).or_default() += 1;
        }
        for tile in tile_state
            .shown_tiles_by_layer
            .iter()
            .flatten()
            .flat_map(|shown| &shown.tiles)
        {
            *holder_count_by_content_hash.entry(tile.content_hash).or_default() += 1;
        }
        holder_count_by_content_hash
    }

    pub fn tile_raster_scratch_surface_release_timer_fired(&mut self) {
        self.tile_raster_scratch_surface = None;
    }

    fn tile_raster_scratch_surface_of_size(
        &mut self,
        env: &PresentingEnvironment<'_>,
        size: IntSize,
        like: &SurfaceHandle,
    ) -> Option<SurfaceHandle> {
        let cached_surface_fits = self.tile_raster_scratch_surface.as_ref().is_some_and(|surface| {
            env.raster.surface_size(surface) == size && env.raster.surfaces_share_backend(surface, like)
        });
        if !cached_surface_fits {
            self.tile_raster_scratch_surface = None;
            self.tile_raster_scratch_surface = Some(create_surface_on_backend_of(env.raster, size, like)?);
        }
        env.host.set_timer(
            env.context_id,
            ContextTimer::TileRasterScratchSurfaceRelease,
            TILE_RASTER_SCRATCH_SURFACE_RELEASE_DELAY,
        );
        self.tile_raster_scratch_surface.clone()
    }
}

fn replay_into_scratch_and_copy_into_tiles(
    raster: &RasterHost,
    scratch: &SurfaceHandle,
    target_tiles: &[(SurfaceHandle, IntRect)],
    scratch_rect_in_space: IntRect,
    clear_color: Color,
    replay: &dyn Fn(&SurfaceHandle),
) {
    let save_count = raster.save(scratch);
    raster.clear(scratch, clear_color);
    raster.translate(
        scratch,
        -scratch_rect_in_space.x as f32,
        -scratch_rect_in_space.y as f32,
    );
    replay(scratch);
    raster.restore_to_count(scratch, save_count);
    for (tile_surface, tile_rect_in_space) in target_tiles {
        raster.draw_surface(
            tile_surface,
            scratch,
            tile_rect_in_space.translated(-scratch_rect_in_space.x, -scratch_rect_in_space.y),
            rect_of_size(size_of(*tile_rect_in_space)),
            SurfaceBlend::Source,
        );
    }
}

// A surface that fell back to the CPU on a GPU backend would be allocated again with every frame.
fn create_surface_on_backend_of(raster: &RasterHost, size: IntSize, like: &SurfaceHandle) -> Option<SurfaceHandle> {
    let inherits_lcd_text = true;
    raster
        .create_surface(size, Some(like), inherits_lcd_text)
        .filter(|surface| raster.surfaces_share_backend(surface, like))
}

fn size_of(rect: IntRect) -> IntSize {
    IntSize {
        width: rect.width,
        height: rect.height,
    }
}

fn rect_in_frame_of_space(
    rect_in_space: IntRect,
    space_to_frame: SpaceToFrame,
    composite_clip: Option<IntRect>,
    within: IntRect,
) -> IntRect {
    let rect_in_frame = space_to_frame.rect_in_frame(rect_in_space).intersected(within);
    composite_clip.map_or(rect_in_frame, |composite_clip| {
        rect_in_frame.intersected(composite_clip)
    })
}

fn frame_rect_in_space_unless_viewport_or_transformed(
    raster_space_index: RasterSpaceIndex,
    space_to_frame: SpaceToFrame,
    viewport_rect: IntRect,
) -> Option<IntRect> {
    match space_to_frame {
        SpaceToFrame::Offset(offset) if raster_space_index != RasterSpaceIndex::VIEWPORT => {
            Some(viewport_rect.translated(-offset.x, -offset.y))
        }
        _ => None,
    }
}

fn rect_of_kept_tiles(
    raster_space_index: RasterSpaceIndex,
    rect_shown: IntRect,
    viewport_rect: IntRect,
    composited_filter_read_reach: i32,
) -> IntRect {
    if raster_space_index == RasterSpaceIndex::VIEWPORT {
        return inflated(viewport_rect, composited_filter_read_reach);
    }
    if rect_shown.is_empty() {
        return IntRect::default();
    }
    let (margin_x_kept_for_scrolling_back, margin_y_kept_for_scrolling_back) =
        (viewport_rect.width / 2, viewport_rect.height / 2);
    rect_shown.inflated_edges(
        margin_y_kept_for_scrolling_back,
        margin_x_kept_for_scrolling_back,
        margin_y_kept_for_scrolling_back,
        margin_x_kept_for_scrolling_back,
    )
}

fn inflated(rect: IntRect, margin: i32) -> IntRect {
    if rect.is_empty() || margin == 0 {
        return rect;
    }
    rect.inflated_edges(margin, margin, margin, margin)
}

fn composited_filter_read_reach_by_layer(
    stack_of_frames: &StackOfFrames,
    tree: &VisualContextTree,
    geometries: &[Rc<RasterSpaceGeometry>],
) -> Vec<i32> {
    let stack = &stack_of_frames.stack;
    if stack.composited_effects.is_empty() {
        return vec![0; stack.layers.len()];
    }
    let mut filter_read_reach_by_composited_node: HashMap<PlacementNodeIndex, i32> = HashMap::new();
    stack_of_frames
        .composited_bands_around_layer
        .iter()
        .map(|composited_bands_around_layer| {
            composited_bands_around_layer
                .iter()
                .map(|band_index| {
                    let node_index = stack.bands[band_index.0].placement_node_index;
                    *filter_read_reach_by_composited_node
                        .entry(node_index)
                        .or_insert_with(|| {
                            let node = &stack.nodes[node_index.0];
                            let effect = node
                                .composited_effect
                                .expect("a composited band is of a composited node");
                            let effect_node = &tree.effect_nodes[effect.0 as usize];
                            spreading_filter_of(effect_node).map_or(0, |filter| {
                                composited_filter_read_reach_in_device_pixels(
                                    filter,
                                    &geometries[node.raster_space_index.0]
                                        .world
                                        .draw_matrix(effect_node.spatial),
                                )
                            })
                        })
                })
                .fold(0, i32::saturating_add)
        })
        .collect()
}

fn raster_cause_of(
    tile_held_at_cell: Option<(&PooledTile, bool)>,
    part_of_cell_needed: IntRect,
    raster: &RasterHost,
    like: &SurfaceHandle,
) -> RasterCause {
    let Some((held_tile, held_content_hash_is_needed)) = tile_held_at_cell else {
        return RasterCause::NewlyShown;
    };
    if !held_tile.held_part_of_cell.contains_rect(part_of_cell_needed) {
        return RasterCause::HeldLessThanNeeded;
    }
    if held_tile
        .surface()
        .is_some_and(|surface| !raster.surfaces_share_backend(surface, like))
    {
        return RasterCause::HeldOnAnotherBackend;
    }
    if held_content_hash_is_needed {
        RasterCause::HeldPixelsInvalidated
    } else {
        RasterCause::ContentHashChanged
    }
}

enum TileDraw<'a> {
    TileSurface {
        surface: &'a SurfaceHandle,
        rect_in_layer_space: IntRect,
    },
    FillOfJoinedOneColorTiles {
        rect_in_layer_space: IntRect,
        color: Color,
    },
}

fn draws_of_held_tiles_reaching_into<'a>(
    held_tiles: &'a BTreeMap<TileCoord, ContentHash>,
    tile_pool: &'a TilePool,
    compositing: &LayerCompositingInFrame,
    area_in_frame: IntRect,
) -> Vec<TileDraw<'a>> {
    let mut draws = Vec::new();
    let mut one_color_fills = Vec::new();
    // A transform can map the area to many more cells than the layer holds tiles at.
    let area_in_layer_space = compositing.space_to_frame.rect_in_space(area_in_frame);
    for (_, content_hash) in entries_within(held_tiles, CellRange::touched_by(area_in_layer_space)) {
        let Some(tile) = tile_pool.get(content_hash) else {
            continue;
        };
        if compositing
            .rect_in_frame(tile.held_part_of_cell, area_in_frame)
            .is_empty()
        {
            continue;
        }
        let rect_in_layer_space = tile.held_part_of_cell;
        match &tile.pixels {
            TilePixels::Surface { surface, .. } => draws.push(TileDraw::TileSurface {
                surface,
                rect_in_layer_space,
            }),
            TilePixels::OneColor(color) => one_color_fills.push((rect_in_layer_space, *color)),
        }
    }
    draws.extend(joined_rects_of_grid_given_row_by_row(one_color_fills).into_iter().map(
        |(rect_in_layer_space, color)| TileDraw::FillOfJoinedOneColorTiles {
            rect_in_layer_space,
            color,
        },
    ));
    draws
}

fn joined_rects_of_grid_given_row_by_row<T: Copy + PartialEq>(
    rects: impl IntoIterator<Item = (IntRect, T)>,
) -> Vec<(IntRect, T)> {
    let mut rects_joined_within_rows: Vec<(IntRect, T)> = Vec::new();
    for (rect, value) in rects {
        if rect.is_empty() {
            continue;
        }
        if let Some((left, left_value)) = rects_joined_within_rows.last_mut()
            && *left_value == value
            && left.y == rect.y
            && left.height == rect.height
            && left.right() == rect.x
        {
            left.width += rect.width;
            continue;
        }
        rects_joined_within_rows.push((rect, value));
    }
    let mut joined: Vec<(IntRect, T)> = Vec::new();
    for (row_rect, value) in rects_joined_within_rows {
        if let Some((above, _)) = joined.iter_mut().find(|(above, above_value)| {
            *above_value == value
                && above.x == row_rect.x
                && above.width == row_rect.width
                && above.bottom() == row_rect.y
        }) {
            above.height += row_rect.height;
            continue;
        }
        joined.push((row_rect, value));
    }
    joined
}

fn rect_is_covered_by_union_of(rect: IntRect, covering_rects: &[IntRect]) -> bool {
    if covering_rects.iter().any(|cover| cover.contains_rect(rect)) {
        return true;
    }
    if covering_rects.iter().all(|cover| cover.intersected(rect).is_empty()) {
        return false;
    }
    let mut uncovered_parts = vec![rect];
    for &cover in covering_rects {
        uncovered_parts = uncovered_parts
            .into_iter()
            .flat_map(|part| {
                let overlap = part.intersected(cover);
                if overlap.is_empty() {
                    return vec![part];
                }
                let above_overlap = IntRect::new(part.x, part.y, part.width, overlap.y - part.y);
                let below_overlap =
                    IntRect::new(part.x, overlap.bottom(), part.width, part.bottom() - overlap.bottom());
                let left_of_overlap = IntRect::new(part.x, overlap.y, overlap.x - part.x, overlap.height);
                let right_of_overlap = IntRect::new(
                    overlap.right(),
                    overlap.y,
                    part.right() - overlap.right(),
                    overlap.height,
                );
                [above_overlap, below_overlap, left_of_overlap, right_of_overlap]
                    .into_iter()
                    .filter(|piece| !piece.is_empty())
                    .collect()
            })
            .collect();
        if uncovered_parts.is_empty() {
            return true;
        }
    }
    uncovered_parts.is_empty()
}

// A backdrop filter that reads past its tile by an unknown reach reads its layer's tiles wherever they are, so none of
// them may be hidden.
fn hide_tiles_covered_by_opaque_tiles_above(
    stack_of_frames: &StackOfFrames,
    tile_state: &mut FrameTileState,
    compositing_of_layers: &[LayerCompositingInFrame],
    read_reaches_of_composited_effects_in_frame: &[IntRect],
    viewport_rect: IntRect,
    is_hidden_from_view: impl Fn(&LayerIdentity) -> bool,
) -> usize {
    let mut opaque_rects_in_frame_above: Vec<IntRect> = Vec::new();
    let mut hidden_tile_count = 0;
    for compositing in compositing_of_layers.iter().rev() {
        let layer_index = compositing.layer_index;
        let layer = &stack_of_frames.stack.layers[layer_index.0];
        if !matches!(layer.content, LayerContent::Painted { .. }) {
            continue;
        }
        let Some(shown) = tile_state.shown_tiles_by_layer[layer_index.0].as_mut() else {
            continue;
        };
        let every_backdrop_read_reach_is_known = shown
            .tiles
            .iter()
            .all(|tile| tile.raster_margin_in_device_pixels.is_some());
        if !opaque_rects_in_frame_above.is_empty() && every_backdrop_read_reach_is_known {
            let tile_count_before = shown.tiles.len();
            shown.tiles.retain(|tile| {
                let needed_rect_in_frame = compositing.rect_in_frame(tile.part_of_cell_needed, viewport_rect);
                let is_read_by_composited_effect = read_reaches_of_composited_effects_in_frame
                    .iter()
                    .any(|read_reach| !read_reach.intersected(needed_rect_in_frame).is_empty());
                needed_rect_in_frame.is_empty()
                    || is_read_by_composited_effect
                    || !rect_is_covered_by_union_of(needed_rect_in_frame, &opaque_rects_in_frame_above)
            });
            hidden_tile_count += tile_count_before - shown.tiles.len();
        }
        let frame_holds_layer = !compositing.shown_by_window_server;
        let is_inside_composited_effects = !stack_of_frames.composited_bands_around_layer(layer_index).is_empty();
        let covers_layers_below = frame_holds_layer
            && !is_inside_composited_effects
            && compositing.rounded_composite_clips.is_empty()
            && matches!(compositing.space_to_frame, SpaceToFrame::Offset(_))
            && !is_hidden_from_view(&compositing.identity);
        if covers_layers_below {
            let opaque_rects_in_frame = shown
                .tiles
                .iter()
                .filter(|tile| tile.coverage.is_opaque())
                .map(|tile| (compositing.rect_in_frame(tile.part_of_cell_needed, viewport_rect), ()));
            opaque_rects_in_frame_above.extend(
                joined_rects_of_grid_given_row_by_row(opaque_rects_in_frame)
                    .into_iter()
                    .map(|(rect, _)| rect),
            );
        }
    }
    hidden_tile_count
}

fn copy_held_tiles_into(
    raster: &RasterHost,
    target: &SurfaceHandle,
    held_tiles: &BTreeMap<TileCoord, ContentHash>,
    tile_pool: &TilePool,
    area_in_frame: IntRect,
    compositing: &LayerCompositingInFrame,
) {
    let SpaceToFrame::Offset(composite_offset) = compositing.space_to_frame else {
        unreachable!("the frame holds the layers of spaces it composites through a transform");
    };
    let save_count = raster.save(target);
    if let Some(composite_clip) = compositing.composite_clip {
        raster.clip_rect(target, composite_clip);
    }
    for draw in draws_of_held_tiles_reaching_into(held_tiles, tile_pool, compositing, area_in_frame) {
        match draw {
            TileDraw::TileSurface {
                surface,
                rect_in_layer_space,
            } => {
                raster.draw_surface(
                    target,
                    surface,
                    rect_of_size(size_of(rect_in_layer_space)),
                    rect_in_layer_space.translated(composite_offset.x, composite_offset.y),
                    SurfaceBlend::Source,
                );
            }
            TileDraw::FillOfJoinedOneColorTiles {
                rect_in_layer_space,
                color,
            } => {
                let save_count = raster.save(target);
                raster.clip_rect(
                    target,
                    rect_in_layer_space.translated(composite_offset.x, composite_offset.y),
                );
                raster.clear(target, color);
                raster.restore_to_count(target, save_count);
            }
        }
    }
    raster.restore_to_count(target, save_count);
}

fn layer_allows_lcd_text(
    stack_of_frames: &StackOfFrames,
    layer_index: LayerIndex,
    frame_allows_lcd_text: bool,
) -> bool {
    frame_allows_lcd_text
        && layer_index == stack_of_frames.stack.base_layer
        && stack_of_frames.composited_bands_around_layer(layer_index).is_empty()
}

fn clear_color_of_layer(stack: &LayerStack, layer_index: LayerIndex, surface_clear_color: Color) -> Color {
    if layer_index == stack.base_layer {
        surface_clear_color
    } else {
        Color::TRANSPARENT
    }
}

fn held_tile_lies_in_shown_rect_at_unshown_cell(
    coord: TileCoord,
    held_tile: &PooledTile,
    rect_shown_in_layer_space: IntRect,
    cells_shown: &HashSet<TileCoord>,
) -> bool {
    !held_tile
        .held_part_of_cell
        .intersected(rect_shown_in_layer_space)
        .is_empty()
        && !cells_shown.contains(&coord)
}

fn cells_shown(shown: &[TileShownInFrame]) -> HashSet<TileCoord> {
    shown.iter().map(|tile| tile.coord).collect()
}

fn stale_rect_of_painted_layer(
    cached_layer: &CachedLayer,
    tile_pool: &TilePool,
    shown: &[TileShownInFrame],
    compositing: &LayerCompositingInFrame,
    viewport_rect: IntRect,
    composited_filter_read_reach: i32,
) -> IntRect {
    let rect_shown_in_layer_space = compositing.rect_shown_in_layer_space(viewport_rect, composited_filter_read_reach);
    let in_frame_as_far_as_filters_around_layer_read = |rect_in_layer_space: IntRect| {
        compositing.rect_in_frame(
            inflated(rect_in_layer_space, composited_filter_read_reach),
            viewport_rect,
        )
    };
    let mut stale_rect = IntRect::default();
    let cells_shown = cells_shown(shown);
    for (coord, held_tile) in pooled_held_tiles(&cached_layer.held_tiles, tile_pool) {
        if held_tile_lies_in_shown_rect_at_unshown_cell(coord, held_tile, rect_shown_in_layer_space, &cells_shown) {
            stale_rect = stale_rect.united(in_frame_as_far_as_filters_around_layer_read(
                held_tile.held_part_of_cell,
            ));
        }
    }
    for tile in shown {
        let held_content_hash = cached_layer.held_tiles.get(&tile.coord);
        let tile_is_unchanged = held_content_hash == Some(&tile.content_hash)
            && tile_pool
                .get(&tile.content_hash)
                .is_some_and(|pooled| pooled.holds_current_pixels_over_ignoring_backend(tile.part_of_cell_needed));
        if tile_is_unchanged {
            continue;
        }
        stale_rect = stale_rect.united(in_frame_as_far_as_filters_around_layer_read(tile.part_of_cell_needed));
        if let Some(held_tile) = held_content_hash.and_then(|held_content_hash| tile_pool.get(held_content_hash)) {
            stale_rect = stale_rect.united(in_frame_as_far_as_filters_around_layer_read(
                held_tile.held_part_of_cell,
            ));
        }
    }
    stale_rect
}

/// Where a frame that composites its layers as the frame before did, except through other transforms of spaces whose
/// anchors the compositor animates, needs painting: where those layers were in the frame before, as the tiles that frame
/// held of them and their reach tell, and where they are now. None for a frame that composites anything else
/// differently. `stack` is the stack of the frame.
fn damage_of_layers_moved_by_animated_transforms(
    stack: &LayerStack,
    compositing_of_latest_frame: &[LayerCompositingInFrame],
    cached_layer_by_identity: &HashMap<LayerIdentity, CachedLayer>,
    tile_pool: &TilePool,
    layer_update: &LayerUpdate,
    viewport_rect: IntRect,
) -> Option<IntRect> {
    if compositing_of_latest_frame.len() != layer_update.compositing_of_layers.len() {
        return None;
    }
    let composited_filter_read_reach_by_layer = layer_update
        .tile_state
        .as_ref()
        .map(|tile_state| &tile_state.composited_filter_read_reach_by_layer);
    let mut damage = IntRect::default();
    for (latest, current) in compositing_of_latest_frame
        .iter()
        .zip(&layer_update.compositing_of_layers)
    {
        if latest == current {
            continue;
        }
        let moved_by_animated_transform = matches!(
            (latest.space_to_frame, current.space_to_frame),
            (SpaceToFrame::Transform(_), SpaceToFrame::Transform(_))
        ) && LayerCompositingInFrame {
            space_to_frame: current.space_to_frame,
            ..latest.clone()
        } == *current;
        if !moved_by_animated_transform {
            return None;
        }
        let composited_filter_read_reach = composited_filter_read_reach_by_layer
            .and_then(|reaches| reaches.get(current.layer_index.0).copied())
            .unwrap_or(0);
        let reach_in_layer_space = stack.layers[current.layer_index.0].reach;
        // A new list can shrink the reach of the layer, and the frame before showed every tile it held of the layer.
        let held_in_latest_frame = cached_layer_by_identity
            .get(&latest.identity)
            .map_or(IntRect::default(), |cached_layer| {
                cached_layer.bounds_of_held_tiles(tile_pool)
            });
        damage = damage
            .united(latest.rect_in_frame(
                inflated(
                    reach_in_layer_space.united(held_in_latest_frame),
                    composited_filter_read_reach,
                ),
                viewport_rect,
            ))
            .united(current.rect_in_frame(
                inflated(reach_in_layer_space, composited_filter_read_reach),
                viewport_rect,
            ));
    }
    Some(damage)
}

struct CompositedEffectsOfFrame {
    stale_rect: IntRect,
    read_reaches_in_frame: Vec<IntRect>,
    content_hash_by_composited_node: HashMap<PlacementNodeIdentity, ContentHash>,
}

#[allow(clippy::too_many_arguments)]
fn composited_effects_of_frame(
    stack: &LayerStack,
    tree: &VisualContextTree,
    geometries: &[Rc<RasterSpaceGeometry>],
    tile_keying_by_raster_space: &[TileKeyingOfRasterSpace<'_>],
    composite_offsets: &[IntPoint],
    spaces_to_frame: &[SpaceToFrame],
    viewport_rect: IntRect,
    content_hash_by_composited_node_of_latest_frame: &HashMap<PlacementNodeIdentity, ContentHash>,
) -> CompositedEffectsOfFrame {
    let mut effects = CompositedEffectsOfFrame {
        stale_rect: IntRect::default(),
        read_reaches_in_frame: Vec::new(),
        content_hash_by_composited_node: HashMap::new(),
    };
    for composited in &stack.composited_effects {
        let node = &stack.nodes[composited.placement_node_index.0];
        let space_to_frame = spaces_to_frame[node.raster_space_index.0];
        let composite_clip =
            stack.composite_clip_of_node_in_viewport(composited.placement_node_index, composite_offsets);
        let rect_in_frame_of_reach = |reach: Reach<IntRect>, rect_if_anywhere: IntRect| match reach {
            Reach::Within(rect) => rect_in_frame_of_space(rect, space_to_frame, composite_clip, viewport_rect),
            Reach::Nothing => IntRect::default(),
            Reach::Anywhere => rect_if_anywhere,
        };
        let viewport_rect_within_composite_clip = composite_clip.map_or(viewport_rect, |composite_clip| {
            viewport_rect.intersected(composite_clip)
        });
        let node_reach_in_frame =
            rect_in_frame_of_reach(node.reach_in_parent_node, viewport_rect_within_composite_clip);
        let content_hash =
            tile_keying_by_raster_space[node.raster_space_index.0].content_hash_of_composited_effect(composited.effect);
        if content_hash_by_composited_node_of_latest_frame.get(&node.identity) != Some(&content_hash) {
            effects.stale_rect = effects.stale_rect.united(node_reach_in_frame);
        }
        effects
            .content_hash_by_composited_node
            .insert(node.identity, content_hash);
        if composited.filter_spreads_what_it_composites {
            effects.read_reaches_in_frame.push(node_reach_in_frame);
        } else if composited.reads_what_is_composited_below_it {
            let backdrop_read_reach =
                backdrop_filter_read_reach(tree, &geometries[node.raster_space_index.0], composited.effect);
            effects
                .read_reaches_in_frame
                .push(rect_in_frame_of_reach(backdrop_read_reach, node_reach_in_frame));
        }
    }
    effects
}

struct TileToRaster {
    shown_tile_index: usize,
    raster_cause_for_overlay: Option<RasterCause>,
    was_rastered: bool,
}

fn batch_tiles_to_raster_by_cuts(shown: &[TileShownInFrame], tiles_to_raster: &[TileToRaster]) -> Vec<Vec<usize>> {
    let mut batches: Vec<(&BarrierCutsOfCell, Vec<usize>)> = Vec::new();
    for (position_in_tiles_to_raster, tile_to_raster) in tiles_to_raster.iter().enumerate() {
        let cuts = &shown[tile_to_raster.shown_tile_index].cuts;
        match batches.iter_mut().find(|(batch_cuts, _)| *batch_cuts == cuts) {
            Some((_, batch)) => batch.push(position_in_tiles_to_raster),
            None => batches.push((cuts, vec![position_in_tiles_to_raster])),
        }
    }
    batches.into_iter().map(|(_, batch)| batch).collect()
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

fn unclipped_untransformed_surface_sublayer(
    identity: u64,
    surface: &PresentableSurfaceHandle,
    rect_in_frame: IntRect,
) -> FfiSublayer {
    FfiSublayer {
        identity,
        content_kind: SublayerContentKind::PresentableSurface,
        media_id: 0,
        presentable_surface: surface.as_ptr(),
        placement: FfiSublayerPlacement {
            content_rect: rect_in_frame,
            content_to_frame: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            clip_rect: rect_in_frame.to_float(),
            scaling_mode: ScalingMode::NearestNeighbor,
            ..Default::default()
        },
    }
}

fn lists_hold_same_chunks(cached_list: &Rc<DisplayList>, list: &Rc<DisplayList>) -> bool {
    Rc::ptr_eq(cached_list, list) || cached_list.storage().chunk_names() == list.storage().chunk_names()
}

/// Per raster space, how the frame composites its rasters, from the anchors' draw matrices in the frame where the stack
/// has spaces whose anchors the compositor animates.
fn spaces_to_frame_of(
    stack: &LayerStack,
    tree: &VisualContextTree,
    scroll_offsets: &[FloatPoint],
    composite_offsets: &[IntPoint],
) -> Vec<SpaceToFrame> {
    let has_space_composited_through_transform = stack
        .spaces
        .iter()
        .any(|space| matches!(space.anchor_kind, RasterSpaceAnchorKind::AnimatedTransform { .. }));
    let world_of_frame = has_space_composited_through_transform
        .then(|| WorldTransforms::new(tree, scroll_offsets, Some(FloatMatrix4x4::identity())));
    stack.spaces_to_frame(composite_offsets, |anchor| {
        world_of_frame
            .as_ref()
            .map_or(FloatMatrix4x4::identity(), |world| world.draw_matrix(anchor))
    })
}

fn compositing_of_layer(
    stack: &LayerStack,
    layer_index: LayerIndex,
    composite_offsets: &[IntPoint],
    spaces_to_frame: &[SpaceToFrame],
) -> LayerCompositingInFrame {
    LayerCompositingInFrame {
        layer_index,
        identity: stack.layers[layer_index.0].identity,
        space_to_frame: spaces_to_frame[stack.space_of_layer(layer_index).0],
        composite_clip: stack.composite_clip_of_layer_in_viewport(layer_index, composite_offsets),
        rounded_composite_clips: stack
            .rounded_composite_clips_of_node_in_viewport(
                stack.layers[layer_index.0].placement_node_index,
                composite_offsets,
            )
            .into(),
        shown_by_window_server: false,
    }
}

impl ContextPresenting {
    /// Works out the layers of the frame within `viewport_rect`, in the device pixels of the raster: those of the plan
    /// WebContent sent where the frame is drawn as the plan was made for, and one layer that replays the whole list
    /// otherwise.
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
        self.layers.work_counts_of_this_frame = FrameWorkCounts::default();
        // A plan places spaces that scroll at whole device pixels of the viewport it was made within, which a raster
        // that scales them or shifts them by a fraction lacks, as does a viewport of another size. So only a stack of
        // one layer, in the viewport's space, is drawn under a raster transform other than identity, and what
        // composites the spaces and effects of a plan does not apply the raster transform.
        let layer_plan = frame.frame_inputs.layer_plan.as_ref().filter(|layer_plan| {
            raster_transform == FloatMatrix4x4::identity() && layer_plan.viewport_rect == viewport_rect
        });
        let one_layer =
            |layers: &mut ContextLayers| layers.one_layer_stack_of_frame(frame.display_list, tree, viewport_rect);
        let stack = self.layers.take_stack_of_frame(layer_plan, one_layer, raster_transform);
        let stack = stack.as_ref();
        self.layers.window_server_placements_of_media =
            stack.window_server_placements_of_media(storage, tree, scroll_offsets, viewport_rect);
        let composite_offsets = stack.composite_offsets(scroll_offsets);
        let spaces_to_frame = spaces_to_frame_of(stack, tree, scroll_offsets, &composite_offsets);
        let shown_by_window_server_by_layer = if self.why_window_server_shows_no_media_of_this_context(env).is_none() {
            stack.layers_the_window_server_shows(&composite_offsets, |layer_index| {
                self.why_window_server_does_not_show_media_layer(
                    env,
                    layer_index,
                    &stack.layers[layer_index.0],
                    live_content,
                )
                .is_none()
            })
        } else {
            vec![false; stack.layers.len()]
        };
        let compositing_of_layers: Vec<LayerCompositingInFrame> = (stack.base_layer.0..stack.layers.len())
            .map(LayerIndex)
            .filter(|&layer_index| layer_index == stack.base_layer || stack.layers[layer_index.0].may_hold_content)
            .map(|layer_index| LayerCompositingInFrame {
                shown_by_window_server: shown_by_window_server_by_layer[layer_index.0],
                ..compositing_of_layer(stack, layer_index, &composite_offsets, &spaces_to_frame)
            })
            .collect();
        LayerUpdate {
            tile_state: None,
            composite_offsets,
            spaces_to_frame,
            compositing_of_layers,
            read_reaches_of_composited_effects_in_frame: Vec::new(),
        }
    }

    /// Keys the tiles the frame shows of each layer, and answers the rect of the viewport that the tiles to raster
    /// again, the tiles that go, and the media replayed anew cover. `invalidates_every_tile` makes every tile stale
    /// whatever its content hash, as when the frame draws content that the list does not name. What a frame is made to
    /// repaint otherwise leaves the tiles as they are: it is either drawn over the layers by the compositor, as its
    /// scroll bars are, or told by the content hashes of the tiles that hold it, as a caret's blink is.
    pub(super) fn prepare_frame_tile_state(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        live_content: &LiveContent,
        layer_update: &mut LayerUpdate,
        viewport_rect: IntRect,
        invalidates_every_tile: bool,
    ) -> IntRect {
        if invalidates_every_tile {
            self.layers
                .invalidate_every_tile_and_media_replay(&layer_update.compositing_of_layers);
        }
        self.with_stack_of_frames(|this, stack_of_frames| {
            let (stale_rect, band_tile_keys_by_band) = this.prepare_frame_tile_state_of_stack(
                env,
                frame,
                live_content,
                layer_update,
                viewport_rect,
                stack_of_frames,
            );
            // The frame paints its tiles from the stack, which keeps the tile keys where the frame is not painted.
            stack_of_frames.band_tile_keys_of_latest_frame =
                BandTileKeysOfLatestFrame::OfSameStack(band_tile_keys_by_band);
            stale_rect
        })
    }

    /// Where a frame that composites its layers as the last painted one did, except through other transforms of spaces
    /// whose anchors the compositor animates, needs painting: where those layers were and are. None for a frame that
    /// composites anything else differently.
    pub(super) fn damage_of_layers_moved_by_animated_transforms(
        &self,
        layer_update: &LayerUpdate,
        viewport_rect: IntRect,
    ) -> Option<IntRect> {
        damage_of_layers_moved_by_animated_transforms(
            &self.layers.stack_of_frames.as_ref()?.stack,
            &self.layers.compositing_of_latest_frame,
            &self.layers.cached_layer_by_identity,
            &self.layers.tile_pool,
            layer_update,
            viewport_rect,
        )
    }

    fn with_stack_of_frames<R>(&mut self, work: impl FnOnce(&mut Self, &mut StackOfFrames) -> R) -> R {
        let mut stack_of_frames = self
            .layers
            .stack_of_frames
            .take()
            .expect("a frame with layers has a stack");
        let result = work(self, &mut stack_of_frames);
        self.layers.stack_of_frames = Some(stack_of_frames);
        result
    }

    fn prepare_frame_tile_state_of_stack(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        live_content: &LiveContent,
        layer_update: &mut LayerUpdate,
        viewport_rect: IntRect,
        stack_of_frames: &mut StackOfFrames,
    ) -> (IntRect, Vec<Option<BandTileKeysOfFrame>>) {
        let storage = frame.display_list.storage();
        let tree = frame.frame_inputs.visual_context_tree.as_arc();
        let scroll_offsets = frame.frame_inputs.presented_scroll_state_snapshot.device_offsets();
        let composite_offsets = &layer_update.composite_offsets;
        let raster_transform = self.raster_transform();
        let geometries = stack_of_frames.geometries_of_spaces(tree, scroll_offsets);
        let band_tile_keys_of_latest_frame = std::mem::replace(
            &mut stack_of_frames.band_tile_keys_of_latest_frame,
            BandTileKeysOfLatestFrame::OfSameStack(Vec::new()),
        );
        let stack_of_frames = &*stack_of_frames;
        let stack = stack_of_frames.stack.as_ref();
        let scrollbars = frame.frame_inputs.scrollbars_shown_for_the_page();
        let live_content_versions = LiveContentVersionsOfFrame {
            live_content,
            nested_contexts: env.nested_contexts,
        };
        let tile_keying_of_list = TileKeyingOfList::new(
            storage,
            tree,
            stack,
            FrameStateReadByReplay {
                scrollbars: &scrollbars,
                time_ns: frame.frame_inputs.painted_at.nanoseconds(),
                live_content: &live_content_versions,
            },
        );
        let tile_keying_by_raster_space: Vec<TileKeyingOfRasterSpace<'_>> = geometries
            .iter()
            .map(|geometry| TileKeyingOfRasterSpace::new(&tile_keying_of_list, geometry))
            .collect();
        let surface_clear_color = frame.surface_clear_color();
        let root_group_color = frame.display_list.root_group_color();
        let frame_allows_lcd_text = self.allows_lcd_text(frame.display_list, frame.frame_inputs);
        let raster_settings_of_layer = |layer_index: LayerIndex| LayerRasterSettings {
            raster_space_identity: stack.spaces[stack.space_of_layer(layer_index).0].identity,
            clear_color: clear_color_of_layer(stack, layer_index, surface_clear_color),
            root_group_color,
            viewport_to_raster_transform: raster_transform,
            allows_lcd_text: layer_allows_lcd_text(stack_of_frames, layer_index, frame_allows_lcd_text),
        };

        let chunk_names = storage.chunk_names();
        // The tile keys that a frame drawn with the same stack left are bound to the list, the bands and the base layer
        // of this frame, as frames drawn with one stack draw one list.
        let (mut band_tile_keys_by_band, band_tile_keys_are_of_same_stack) = band_tile_keys_of_latest_frame
            .taken_by_bands(|| {
                (0..stack.bands.len())
                    .map(|band_index| BandIdentities::of(stack, BandIndex(band_index), &chunk_names))
                    .collect()
            });
        band_tile_keys_by_band.resize_with(stack.bands.len(), || None);
        let composited_filter_read_reach_by_layer =
            composited_filter_read_reach_by_layer(stack_of_frames, tree, &geometries);
        let mut tile_state = FrameTileState {
            viewport_rect,
            shown_tiles_by_layer: (0..stack.layers.len()).map(|_| None).collect(),
            media_replayed_by_layer: vec![None; stack.layers.len()],
            composited_filter_read_reach_by_layer,
            frame_allows_lcd_text,
            ..FrameTileState::default()
        };
        for (band_position, band) in stack.bands.iter().enumerate() {
            let band_index = BandIndex(band_position);
            let band_tile_keys_of_latest_frame = band_tile_keys_by_band[band_position].take();
            if !stack.band_has_layer_at_or_above_base_layer(band_index) {
                continue;
            }
            let raster_space_index = stack.nodes[band.placement_node_index.0].raster_space_index;
            let tile_keying = &tile_keying_by_raster_space[raster_space_index.0];
            let compositing_of_layer_below_all_cuts = compositing_of_layer(
                stack,
                band.layer_below_all_cuts,
                composite_offsets,
                &layer_update.spaces_to_frame,
            );
            let composited_filter_read_reach =
                tile_state.composited_filter_read_reach_by_layer[band.layer_below_all_cuts.0];
            let shown_rect = compositing_of_layer_below_all_cuts
                .rect_shown_in_layer_space(viewport_rect, composited_filter_read_reach);
            let mut band_tile_keys = band_tile_keys_of_latest_frame.unwrap_or_else(|| BandTileKeysOfFrame {
                identities: BandIdentities::of(stack, band_index, &chunk_names),
                ..BandTileKeysOfFrame::default()
            });
            let still_bound_to_same_list_and_geometry =
                band_tile_keys.bound_to_list_and_geometry.as_ref().is_some_and(|bound| {
                    Rc::ptr_eq(&bound.geometry, &geometries[raster_space_index.0])
                        && (band_tile_keys_are_of_same_stack
                            || (lists_hold_same_chunks(&bound.display_list, frame.display_list)
                                && bound.band == *band
                                && bound.base_layer == stack.base_layer))
                });
            match &mut band_tile_keys.bound_to_list_and_geometry {
                Some(bound) if still_bound_to_same_list_and_geometry => bound.display_list = frame.display_list.clone(),
                _ => {
                    tile_keying.bind_band(&mut band_tile_keys.tile_keys, stack, band_index);
                    band_tile_keys.bound_to_list_and_geometry = Some(ListAndGeometryOfBandTileKeys {
                        display_list: frame.display_list.clone(),
                        geometry: geometries[raster_space_index.0].clone(),
                        band: band.clone(),
                        base_layer: stack.base_layer,
                    });
                }
            }
            let keying_counts = tile_keying.key_shown_tiles(
                &mut band_tile_keys.tile_keys,
                stack,
                band_index,
                raster_settings_of_layer,
                shown_rect,
                rect_of_kept_tiles(
                    raster_space_index,
                    shown_rect,
                    viewport_rect,
                    composited_filter_read_reach,
                ),
                frame_rect_in_space_unless_viewport_or_transformed(
                    raster_space_index,
                    compositing_of_layer_below_all_cuts.space_to_frame,
                    viewport_rect,
                ),
            );
            self.layers.work_counts_of_this_frame.cells_binned += keying_counts.cells_binned as u64;
            for tile in band_tile_keys
                .tile_keys
                .tiles_within(shown_rect)
                .filter(|tile| tile.layer_index >= stack.base_layer)
            {
                let mut part_of_cell_needed = if tile.layer_index == stack.base_layer {
                    tile.coord.cell_rect()
                } else {
                    tile.content_bounds
                };
                if raster_space_index == RasterSpaceIndex::VIEWPORT {
                    part_of_cell_needed =
                        part_of_cell_needed.intersected(inflated(viewport_rect, composited_filter_read_reach));
                }
                if part_of_cell_needed.is_empty() {
                    continue;
                }
                let coverage = match self.layers.tile_pool.get(&tile.content_hash) {
                    Some(pooled) if pooled.holds_current_pixels_over_ignoring_backend(part_of_cell_needed) => {
                        pooled.coverage()
                    }
                    _ => tile_keying.coverage_of_tile(
                        &band_tile_keys.tile_keys,
                        tile,
                        part_of_cell_needed,
                        clear_color_of_layer(stack, tile.layer_index, surface_clear_color),
                    ),
                };
                let shown =
                    tile_state.shown_tiles_by_layer[tile.layer_index.0].get_or_insert_with(|| ShownTilesOfLayer {
                        band_index,
                        tiles: Vec::new(),
                    });
                shown.tiles.push(TileShownInFrame {
                    coord: tile.coord,
                    content_hash: tile.content_hash,
                    part_of_cell_needed,
                    cuts: tile.cuts.clone(),
                    raster_margin_in_device_pixels: tile.raster_margin_in_device_pixels,
                    coverage,
                });
            }
            band_tile_keys_by_band[band_position] = Some(band_tile_keys);
        }
        self.layers.work_counts_of_this_frame.chunks_placed += tile_keying_by_raster_space
            .iter()
            .map(|tile_keying| tile_keying.chunks_placed())
            .sum::<u64>();

        let composited_effects = composited_effects_of_frame(
            stack,
            tree,
            &geometries,
            &tile_keying_by_raster_space,
            composite_offsets,
            &layer_update.spaces_to_frame,
            viewport_rect,
            &self.layers.content_hash_by_composited_node_of_latest_frame,
        );
        let mut stale_rect = composited_effects.stale_rect;
        tile_state.content_hash_by_composited_node = composited_effects.content_hash_by_composited_node;
        self.layers
            .work_counts_of_this_frame
            .tiles_covered_by_opaque_tiles_above += hide_tiles_covered_by_opaque_tiles_above(
            stack_of_frames,
            &mut tile_state,
            &layer_update.compositing_of_layers,
            &composited_effects.read_reaches_in_frame,
            viewport_rect,
            |identity| self.layer_overlay.hides_layer(identity),
        ) as u64;
        for compositing in &layer_update.compositing_of_layers {
            let layer_index = compositing.layer_index;
            let layer = &stack.layers[layer_index.0];
            let cached_layer = self.layers.cached_layer_by_identity.get(&layer.identity);
            if let LayerContent::Media { record_span, reach, .. } = &layer.content {
                let raster_space_index = stack.space_of_layer(layer_index);
                let media_replayed = MediaRecordsReplayedInFrame {
                    content_hash: tile_keying_by_raster_space[raster_space_index.0]
                        .content_hash_of_records_within(*record_span),
                    reach_in_frame: compositing.rect_in_frame(*reach, viewport_rect),
                };
                let media_replayed_in_latest_frame =
                    cached_layer.and_then(|cached_layer| cached_layer.media_replayed_in_latest_frame);
                if !compositing.shown_by_window_server && media_replayed_in_latest_frame != Some(media_replayed) {
                    stale_rect = stale_rect.united(media_replayed.reach_in_frame).united(
                        media_replayed_in_latest_frame.map_or(IntRect::default(), |latest| latest.reach_in_frame),
                    );
                }
                tile_state.media_replayed_by_layer[layer_index.0] = Some(media_replayed);
                continue;
            }
            stale_rect = stale_rect.united(stale_rect_of_painted_layer(
                cached_layer.unwrap_or(&CachedLayer::default()),
                &self.layers.tile_pool,
                tile_state.shown_tiles_of_layer(layer_index),
                compositing,
                viewport_rect,
                tile_state.composited_filter_read_reach_by_layer[layer_index.0],
            ));
        }
        layer_update.tile_state = Some(tile_state);
        layer_update.read_reaches_of_composited_effects_in_frame = composited_effects.read_reaches_in_frame;
        (stale_rect, band_tile_keys_by_band)
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

    fn why_window_server_does_not_show_media_layer(
        &self,
        env: &PresentingEnvironment<'_>,
        layer_index: LayerIndex,
        layer: &StackLayer,
        live_content: &LiveContent,
    ) -> Option<WhyMediaIsReplayed> {
        let LayerContent::Media { content, .. } = &layer.content else {
            return None;
        };
        let placement = match self.layers.window_server_placements_of_media.of_layer(layer_index) {
            Some(Ok(placement)) => placement,
            Some(Err(why)) => return Some(WhyMediaIsReplayed::Placement(*why)),
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

    #[allow(clippy::too_many_arguments)]
    fn update_layer_overlay_with_frame(
        &mut self,
        env: &PresentingEnvironment<'_>,
        frame: &FrameToPaint<'_>,
        stack_of_frames: &StackOfFrames,
        compositing_of_layers: &[LayerCompositingInFrame],
        window_server_shows_layers: bool,
        some_tiles_were_painted_straight: bool,
        live_content: &LiveContent,
    ) {
        if self.layer_overlay.overlay().is_none() {
            return;
        }
        let stack = &stack_of_frames.stack;
        let names_of_layers = stack_of_frames.names_of_layers_for_overlay.get_or_init(|| {
            (0..stack.layers.len())
                .map(|index| Rc::from(stack.name_of_layer(LayerIndex(index), &NodeNamesFromIndices)))
                .collect()
        });
        let why_context_shows_no_media = self.why_window_server_shows_no_media_of_this_context(env);
        let layers = compositing_of_layers
            .iter()
            .map(|compositing| {
                let layer_index = compositing.layer_index;
                let layer = &stack.layers[layer_index.0];
                let content = match &layer.content {
                    LayerContent::Painted { .. } => LayerOfFrameContent::Painted {
                        held_tiles: self
                            .layers
                            .cached_layer_by_identity
                            .get(&compositing.identity)
                            .map(|cached_layer| {
                                pooled_held_tiles(&cached_layer.held_tiles, &self.layers.tile_pool)
                                    .map(|(coord, tile)| HeldTile {
                                        coord,
                                        held_part_of_cell: tile.held_part_of_cell,
                                        is_one_color: tile.surface().is_none(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    LayerContent::Media { reach, .. } => LayerOfFrameContent::Media {
                        reach: *reach,
                        presentation: if window_server_shows_layers && compositing.shown_by_window_server {
                            MediaPresentation::ShownByWindowServer
                        } else {
                            let why = why_context_shows_no_media
                                .or_else(|| {
                                    self.why_window_server_does_not_show_media_layer(
                                        env,
                                        layer_index,
                                        layer,
                                        live_content,
                                    )
                                })
                                .unwrap_or(if some_tiles_were_painted_straight {
                                    WhyMediaIsReplayed::TilesWerePaintedStraightIntoTheFrame
                                } else if !stack_of_frames.composited_bands_around_layer(layer_index).is_empty() {
                                    WhyMediaIsReplayed::InsideAnEffectTheCompositorComposites
                                } else {
                                    WhyMediaIsReplayed::UnderALayerTheFrameHolds
                                });
                            MediaPresentation::Replayed { why: why.to_string() }
                        },
                    },
                };
                LayerOfFrame {
                    identity: compositing.identity,
                    name: names_of_layers[layer_index.0].clone(),
                    space_to_frame: compositing.space_to_frame,
                    composite_clip: compositing.composite_clip,
                    content,
                }
            })
            .collect();
        let overlay_frame = FrameOfLayerOverlay {
            viewport_rect: self.raster_rect(),
            painted_at: frame.frame_inputs.painted_at,
            layers,
            why_list_is_one_layer: stack.why_list_is_one_layer.map(|why| why.to_string()),
            notes_of_layer_stack: stack_of_frames
                .layer_plan
                .as_ref()
                .map_or_else(Vec::new, |layer_plan| layer_plan.note_lines.to_vec()),
            work_counts: self.layers.work_counts_of_this_frame,
        };
        let device_pixel_ratio = self.device_pixel_ratio;
        let shaper = env.host.overlay_label_shaper(env.context_id);
        if let Some(overlay) = self.layer_overlay.overlay_mut() {
            overlay.update_with_frame(overlay_frame, device_pixel_ratio, &shaper);
        }
    }

    /// Takes in what the frame painted of its layers: how it composited them, and which layers left the stack, whose
    /// tiles stay in the pool for a while.
    pub(super) fn commit_layer_update(&mut self, env: &PresentingEnvironment<'_>, layer_update: LayerUpdate) {
        let layers = &mut self.layers;
        layers.take_in_painted_frame(layer_update);
        let identities_in_stack: HashSet<LayerIdentity> = layers
            .compositing_of_latest_frame
            .iter()
            .map(|compositing| compositing.identity)
            .collect();
        layers.cached_layer_by_identity.retain(|identity, cached_layer| {
            let is_in_stack = identities_in_stack.contains(identity);
            if !is_in_stack && let Some(presentation) = &cached_layer.window_server_presentation {
                env.raster
                    .drop_presentable_surface_pool(env.context_id, presentation.presentable_surface_pool);
            }
            is_in_stack
        });
        layers.evict_spare_tiles(env);
    }

    /// Paints the frame from its layers over `damage_rect`, and answers the rect it painted, which grows to the whole
    /// raster where the frame has to hold what the window server was to show above it. Unless `paints_ui_overlays`
    /// is set, the frame leaves out the overlays of the browser and of the layers.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_layered_frame(
        &mut self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
        layer_update: &mut LayerUpdate,
        live_content: &LiveContent,
        paints_ui_overlays: bool,
    ) -> IntRect {
        self.with_stack_of_frames(|this, stack_of_frames| {
            this.paint_layers(
                env,
                surface,
                frame,
                damage_rect,
                layer_update,
                stack_of_frames,
                live_content,
                paints_ui_overlays,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_layers(
        &mut self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        mut damage_rect: IntRect,
        layer_update: &mut LayerUpdate,
        stack_of_frames: &StackOfFrames,
        live_content: &LiveContent,
        paints_ui_overlays: bool,
    ) -> IntRect {
        let stack = stack_of_frames.stack.as_ref();
        env.raster.start_raster_cache_frame(env.context_id);
        // A store can miss the damage of several frames, which joined can reach part of what a composited effect reads.
        damage_rect = layer_update.damage_grown_over_what_composited_effects_read(damage_rect);
        let tile_state = layer_update
            .tile_state
            .as_ref()
            .expect("a layered frame worked out its tiles before it was painted");
        let holder_count_by_content_hash = self.layers.holder_count_by_content_hash(tile_state);
        let mut tile_pool = std::mem::take(&mut self.layers.tile_pool);
        let mut rects_painted_straight = Vec::new();
        for compositing in &layer_update.compositing_of_layers {
            let layer = &stack.layers[compositing.layer_index.0];
            if matches!(layer.content, LayerContent::Media { .. }) {
                self.layers
                    .cached_layer_by_identity
                    .entry(compositing.identity)
                    .or_default();
                continue;
            }
            let mut cached_layer = self
                .layers
                .cached_layer_by_identity
                .remove(&compositing.identity)
                .unwrap_or_default();
            rects_painted_straight.extend(self.raster_shown_tiles_lacking_current_pixels(
                env,
                surface,
                frame,
                stack_of_frames,
                compositing,
                &mut cached_layer,
                &mut tile_pool,
                &holder_count_by_content_hash,
                tile_state,
            ));
            self.layers
                .cached_layer_by_identity
                .insert(compositing.identity, cached_layer);
        }
        self.layers.tile_pool = tile_pool;

        // The overlay goes into the surface the window server shows the overlays from, so it is worked out first,
        // expecting presenting to the window server to succeed.
        let expected_shown_by_window_server =
            layer_update.shown_by_window_server() && rects_painted_straight.is_empty();
        self.update_layer_overlay_with_frame(
            env,
            frame,
            stack_of_frames,
            &layer_update.compositing_of_layers,
            expected_shown_by_window_server,
            !rects_painted_straight.is_empty(),
            live_content,
        );

        if layer_update.shown_by_window_server() {
            let presented_to_window_server = rects_painted_straight.is_empty()
                && self.present_painted_layers_to_window_server(
                    env,
                    stack,
                    &layer_update.compositing_of_layers,
                    surface,
                )
                && self.present_compositor_overlays_to_window_server(env, frame, surface);
            if !presented_to_window_server {
                for compositing in &mut layer_update.compositing_of_layers {
                    compositing.shown_by_window_server = false;
                }
                damage_rect = self.raster_rect();
            }
        }
        if layer_update.shown_by_window_server() != expected_shown_by_window_server {
            self.update_layer_overlay_with_frame(
                env,
                frame,
                stack_of_frames,
                &layer_update.compositing_of_layers,
                layer_update.shown_by_window_server(),
                !rects_painted_straight.is_empty(),
                live_content,
            );
        }

        self.composite_frame_from_layers(
            env,
            surface,
            frame,
            damage_rect,
            layer_update,
            stack_of_frames,
            &rects_painted_straight,
            paints_ui_overlays,
        );
        damage_rect
    }

    #[allow(clippy::too_many_arguments)]
    fn composite_frame_from_layers(
        &self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
        layer_update: &LayerUpdate,
        stack_of_frames: &StackOfFrames,
        rects_painted_straight: &[IntRect],
        paints_ui_overlays: bool,
    ) {
        let raster = env.raster;
        let save_count = raster.save(surface);
        raster.clip_rect(surface, damage_rect);
        if let Some(base_layer_compositing) = layer_update.compositing_of_layers.first() {
            if self.layer_overlay.hides_layer(&base_layer_compositing.identity) {
                raster.clear(surface, frame.surface_clear_color());
            } else if let Some(cached_layer) = self
                .layers
                .cached_layer_by_identity
                .get(&base_layer_compositing.identity)
            {
                // The base layer replaces what is below it, and nothing is below it outside its composite clip. The
                // planner clips no base layer, but a plan from WebContent can.
                if base_layer_compositing.composite_clip.is_some() {
                    raster.clear(surface, frame.surface_clear_color());
                }
                copy_held_tiles_into(
                    raster,
                    surface,
                    &cached_layer.held_tiles,
                    &self.layers.tile_pool,
                    damage_rect,
                    base_layer_compositing,
                );
            }
        }
        self.composite_layers_above_base_of_stack(
            env,
            surface,
            frame,
            damage_rect,
            &stack_of_frames.composited_stack(),
            &layer_update.compositing_of_layers,
            layer_update
                .tile_state
                .as_ref()
                .map_or(&[][..], |tile_state| &tile_state.composited_filter_read_reach_by_layer),
            false,
        );
        if !layer_update.shown_by_window_server() {
            let selection_painting_straight = ReplaySelection {
                allows_lcd_text: layer_update
                    .tile_state
                    .as_ref()
                    .is_some_and(|tile_state| tile_state.frame_allows_lcd_text),
                ..Default::default()
            };
            for &rect_painted_straight in rects_painted_straight {
                let damaged_rect_painted_straight = rect_painted_straight.intersected(damage_rect);
                if damaged_rect_painted_straight.is_empty() {
                    continue;
                }
                let save_count = raster.save(surface);
                raster.clip_rect(surface, damaged_rect_painted_straight);
                raster.clear(surface, frame.surface_clear_color());
                self.apply_raster_transform(raster, surface);
                replay_into(env, surface, frame, selection_painting_straight);
                raster.restore_to_count(surface, save_count);
            }
            self.apply_raster_transform(raster, surface);
            paint_overlays(env, surface, frame.frame_inputs, paints_ui_overlays);
            if paints_ui_overlays {
                self.paint_layer_overlay(env, surface);
            }
        }
        raster.restore_to_count(surface, save_count);
    }

    #[allow(clippy::too_many_arguments)]
    fn raster_shown_tiles_lacking_current_pixels(
        &mut self,
        env: &PresentingEnvironment<'_>,
        like: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        stack_of_frames: &StackOfFrames,
        compositing: &LayerCompositingInFrame,
        cached_layer: &mut CachedLayer,
        tile_pool: &mut TilePool,
        holder_count_by_content_hash: &HashMap<ContentHash, usize>,
        tile_state: &FrameTileState,
    ) -> Vec<IntRect> {
        let stack = stack_of_frames.stack.as_ref();
        let layer_index = compositing.layer_index;
        let raster = env.raster;
        let viewport_rect = tile_state.viewport_rect;
        let raster_space_index = stack.space_of_layer(layer_index);
        let shown = tile_state.shown_tiles_of_layer(layer_index);
        let composited_filter_read_reach = tile_state.composited_filter_read_reach_by_layer[layer_index.0];
        let rect_shown_in_layer_space =
            compositing.rect_shown_in_layer_space(viewport_rect, composited_filter_read_reach);
        let kept_tile_rect = rect_of_kept_tiles(
            raster_space_index,
            rect_shown_in_layer_space,
            viewport_rect,
            composited_filter_read_reach,
        );
        let clear_color = clear_color_of_layer(stack, layer_index, frame.surface_clear_color());
        let allows_lcd_text = layer_allows_lcd_text(stack_of_frames, layer_index, tile_state.frame_allows_lcd_text);
        let painted_at = frame.frame_inputs.painted_at;
        let layer_overlay_records_rasters = self.layer_overlay.overlay().is_some();
        let mut record_raster_in_layer_overlay = |coord: TileCoord, cause: RasterCause, rect: IntRect| {
            if let Some(overlay) = self.layer_overlay.overlay_mut() {
                overlay.record_raster(compositing.identity, coord, cause, rect, painted_at);
            }
        };
        let mut rects_painted_straight = Vec::new();

        let held_tile_count_before = cached_layer.held_tiles.len();
        let cells_shown = cells_shown(shown);
        cached_layer.held_tiles.retain(|coord, content_hash| {
            !coord.cell_rect().intersected(kept_tile_rect).is_empty()
                && tile_pool.get(content_hash).is_some_and(|held_tile| {
                    !held_tile_lies_in_shown_rect_at_unshown_cell(
                        *coord,
                        held_tile,
                        rect_shown_in_layer_space,
                        &cells_shown,
                    )
                })
        });
        let mut held_tiles_changed = cached_layer.held_tiles.len() != held_tile_count_before;

        self.layers.work_counts_of_this_frame.shown_tiles_of_one_color += shown
            .iter()
            .filter(|tile| matches!(tile.coverage, TileCoverage::OneColor(_)))
            .count() as u64;
        let mut tiles_to_raster: Vec<TileToRaster> = Vec::new();
        for (shown_tile_index, tile) in shown.iter().enumerate() {
            let held_content_hash = cached_layer.held_tiles.get(&tile.coord).copied();
            let raster_cause_for_overlay = layer_overlay_records_rasters.then(|| {
                let tile_held_at_cell = held_content_hash.and_then(|held_content_hash| {
                    Some((
                        tile_pool.get(&held_content_hash)?,
                        held_content_hash == tile.content_hash,
                    ))
                });
                raster_cause_of(tile_held_at_cell, tile.part_of_cell_needed, raster, like)
            });
            if held_content_hash != Some(tile.content_hash) {
                cached_layer.held_tiles.insert(tile.coord, tile.content_hash);
                held_tiles_changed = true;
            }
            let pooled_tile_holds_needed_part_drawable_into_frame = |pooled: &PooledTile| {
                pooled.held_part_of_cell.contains_rect(tile.part_of_cell_needed)
                    && pooled
                        .surface()
                        .is_none_or(|surface| raster.surfaces_share_backend(surface, like))
            };
            if tile_pool.get(&tile.content_hash).is_some_and(|pooled| {
                pooled_tile_holds_needed_part_drawable_into_frame(pooled) && pooled.holds_current_pixels
            }) {
                continue;
            }
            if let TileCoverage::OneColor(color) = tile.coverage {
                tile_pool.insert(
                    tile.content_hash,
                    PooledTile {
                        pixels: TilePixels::OneColor(color),
                        held_part_of_cell: tile.part_of_cell_needed,
                        holds_current_pixels: true,
                        last_held_at: env.now,
                    },
                );
                held_tiles_changed = true;
                continue;
            }
            let every_pixel_is_opaque = tile.coverage == TileCoverage::Opaque;
            match tile_pool.get_mut(&tile.content_hash) {
                Some(pooled)
                    if pooled.surface().is_some() && pooled_tile_holds_needed_part_drawable_into_frame(pooled) =>
                {
                    if let TilePixels::Surface {
                        every_pixel_is_opaque: pooled_every_pixel_is_opaque,
                        ..
                    } = &mut pooled.pixels
                    {
                        *pooled_every_pixel_is_opaque = every_pixel_is_opaque;
                    }
                }
                _ => {
                    // The surface of the tile the layer held at the cell takes the new content hash only where it holds
                    // the very part of the cell a new surface would: the GPU antialiases some content, such as rounded
                    // clips, by where in its surface the content lands.
                    let surface_held_by_this_layer_alone = held_content_hash
                        .filter(|held_content_hash| {
                            *held_content_hash != tile.content_hash
                                && holder_count_by_content_hash
                                    .get(held_content_hash)
                                    .copied()
                                    .unwrap_or(0)
                                    <= 1
                                && tile_pool.get(held_content_hash).is_some_and(|held_tile| {
                                    held_tile.held_part_of_cell == tile.part_of_cell_needed
                                        && held_tile
                                            .surface()
                                            .is_some_and(|surface| raster.surfaces_share_backend(surface, like))
                                })
                        })
                        .and_then(|held_content_hash| tile_pool.remove(&held_content_hash))
                        .and_then(|held_tile| match held_tile.pixels {
                            TilePixels::Surface { surface, .. } => Some(surface),
                            TilePixels::OneColor(_) => None,
                        });
                    let surface = surface_held_by_this_layer_alone
                        .or_else(|| create_surface_on_backend_of(raster, size_of(tile.part_of_cell_needed), like));
                    let Some(surface) = surface else {
                        cached_layer.held_tiles.remove(&tile.coord);
                        held_tiles_changed = true;
                        rects_painted_straight.push(compositing.rect_in_frame(tile.part_of_cell_needed, viewport_rect));
                        record_raster_in_layer_overlay(
                            tile.coord,
                            RasterCause::PaintedStraightIntoTheFrame,
                            tile.part_of_cell_needed,
                        );
                        continue;
                    };
                    tile_pool.insert(
                        tile.content_hash,
                        PooledTile {
                            pixels: TilePixels::Surface {
                                surface,
                                every_pixel_is_opaque,
                            },
                            held_part_of_cell: tile.part_of_cell_needed,
                            holds_current_pixels: false,
                            last_held_at: env.now,
                        },
                    );
                }
            }
            tiles_to_raster.push(TileToRaster {
                shown_tile_index,
                raster_cause_for_overlay,
                was_rastered: false,
            });
        }
        if held_tiles_changed || !tiles_to_raster.is_empty() {
            cached_layer.bump_content_version_of_held_tiles();
        }
        if tiles_to_raster.is_empty() {
            return rects_painted_straight;
        }
        self.layers.work_counts_of_this_frame.tiles_rastered += tiles_to_raster.len() as u64;

        let band_index_of_layer = tile_state.shown_tiles_by_layer[layer_index.0]
            .as_ref()
            .expect("a layer with tiles to raster shows tiles")
            .band_index;
        let band_tile_keys = stack_of_frames
            .band_tile_keys_keyed_with_this_stack(band_index_of_layer)
            .expect("a band with shown tiles has tile keys");
        let geometry = band_tile_keys.bound_geometry();
        let clips_left_to_composite =
            (!geometry.clips_left_to_composite.is_empty()).then_some(&geometry.clips_left_to_composite[..]);
        let (raster_scale, raster_translation) = (self.raster_scale, self.raster_translation);
        let composited_effect = stack.nodes[stack.layers[layer_index.0].placement_node_index.0].composited_effect;
        let frame_rect_in_layer_space = frame_rect_in_space_unless_viewport_or_transformed(
            raster_space_index,
            compositing.space_to_frame,
            viewport_rect,
        );
        let replay_tiles = |target: &SurfaceHandle, tiles_replayed: &[&TileShownInFrame]| {
            let record_spans = band_tile_keys
                .tile_keys
                .record_spans_of_tiles(tiles_replayed.iter().map(|tile| {
                    band_tile_keys
                        .tile_keys
                        .tile(tile.coord, layer_index)
                        .expect("a shown tile is keyed")
                }));
            let mut selection = if band_tile_keys.tile_keys.replays_whole_list {
                ReplaySelection::default()
            } else {
                replaying_only(&record_spans, false)
            };
            selection.clips_left_to_composite = clips_left_to_composite;
            selection.composited_effect = composited_effect;
            selection.allows_lcd_text = allows_lcd_text;
            selection.frame_edge_backdrop_blurs_mirror_at = frame_rect_in_layer_space.map(IntRect::to_float);
            selection.raster_root = stack.spaces[raster_space_index.0].raster_root();
            apply_raster_transform(raster, target, raster_scale, raster_translation);
            replay_with_scroll_offsets(env, target, frame, selection, &geometry.raster_scroll_offsets);
        };
        let pooled_surface_and_held_part = |tile_pool: &TilePool, tile: &TileShownInFrame| {
            let pooled = tile_pool
                .get(&tile.content_hash)
                .expect("a tile to raster is in the pool");
            let surface = pooled.surface().expect("a tile to raster has a surface");
            (surface.clone(), pooled.held_part_of_cell)
        };
        let bounds_held_by = |tile_pool: &TilePool, tiles: &[&TileShownInFrame]| {
            tiles
                .iter()
                .filter_map(|tile| tile_pool.get(&tile.content_hash))
                .fold(IntRect::default(), |bounds, pooled| {
                    bounds.united(pooled.held_part_of_cell)
                })
        };

        for batch in batch_tiles_to_raster_by_cuts(shown, &tiles_to_raster) {
            let batch_tiles: Vec<&TileShownInFrame> = batch
                .iter()
                .map(|&position_in_tiles_to_raster| {
                    &shown[tiles_to_raster[position_in_tiles_to_raster].shown_tile_index]
                })
                .collect();
            let largest_raster_margin_if_every_one_is_known = batch_tiles.iter().try_fold(0i32, |largest, tile| {
                Some(largest.max(tile.raster_margin_in_device_pixels?))
            });
            let was_rastered = if largest_raster_margin_if_every_one_is_known == Some(0) && batch_tiles.len() == 1 {
                let (surface, held_part_of_cell) = pooled_surface_and_held_part(tile_pool, batch_tiles[0]);
                let save_count = raster.save(&surface);
                raster.translate(&surface, -held_part_of_cell.x as f32, -held_part_of_cell.y as f32);
                raster.clip_rect(&surface, held_part_of_cell);
                raster.clear(&surface, clear_color);
                replay_tiles(&surface, &batch_tiles);
                raster.restore_to_count(&surface, save_count);
                true
            } else {
                // A backdrop filter reading by an unknown reach needs every shown tile cut alike replayed.
                let tiles_replayed: Vec<&TileShownInFrame> = match largest_raster_margin_if_every_one_is_known {
                    Some(_) => batch_tiles.clone(),
                    None => shown.iter().filter(|tile| tile.cuts == batch_tiles[0].cuts).collect(),
                };
                let raster_margin = largest_raster_margin_if_every_one_is_known.unwrap_or(0);
                let mut scratch_rect_in_space = bounds_held_by(tile_pool, &tiles_replayed).inflated_edges(
                    raster_margin,
                    raster_margin,
                    raster_margin,
                    raster_margin,
                );
                if raster_space_index == RasterSpaceIndex::VIEWPORT {
                    scratch_rect_in_space = scratch_rect_in_space.intersected(viewport_rect);
                }
                scratch_rect_in_space = scratch_rect_in_space.united(bounds_held_by(tile_pool, &batch_tiles));
                let target_tiles: Vec<(SurfaceHandle, IntRect)> = batch_tiles
                    .iter()
                    .map(|tile| pooled_surface_and_held_part(tile_pool, tile))
                    .collect();
                let scratch =
                    self.layers
                        .tile_raster_scratch_surface_of_size(env, size_of(scratch_rect_in_space), like);
                if let Some(scratch) = &scratch {
                    replay_into_scratch_and_copy_into_tiles(
                        raster,
                        scratch,
                        &target_tiles,
                        scratch_rect_in_space,
                        clear_color,
                        &|target| {
                            replay_tiles(target, &tiles_replayed);
                        },
                    );
                }
                scratch.is_some()
            };
            for position_in_tiles_to_raster in batch {
                tiles_to_raster[position_in_tiles_to_raster].was_rastered = was_rastered;
            }
        }

        for tile_to_raster in &tiles_to_raster {
            let tile = &shown[tile_to_raster.shown_tile_index];
            let Some(pooled) = tile_pool.get_mut(&tile.content_hash) else {
                continue;
            };
            pooled.holds_current_pixels = tile_to_raster.was_rastered;
            if !tile_to_raster.was_rastered {
                rects_painted_straight.push(compositing.rect_in_frame(pooled.held_part_of_cell, viewport_rect));
                record_raster_in_layer_overlay(
                    tile.coord,
                    RasterCause::PaintedStraightIntoTheFrame,
                    pooled.held_part_of_cell,
                );
            } else if let Some(cause) = tile_to_raster.raster_cause_for_overlay {
                record_raster_in_layer_overlay(tile.coord, cause, pooled.held_part_of_cell);
            }
        }
        rects_painted_straight
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
        let Some(composited_stack) = &self.layers.stack_of_painted_frame else {
            return;
        };
        self.composite_layers_above_base_of_stack(
            env,
            surface,
            frame,
            damage_rect,
            composited_stack,
            &self.layers.compositing_of_latest_frame,
            &[],
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn composite_layers_above_base_of_stack(
        &self,
        env: &PresentingEnvironment<'_>,
        surface: &SurfaceHandle,
        frame: &FrameToPaint<'_>,
        damage_rect: IntRect,
        composited_stack: &CompositedStack,
        compositing_of_layers: &[LayerCompositingInFrame],
        composited_filter_read_reach_by_layer: &[i32],
        composites_layers_the_window_server_shows: bool,
    ) {
        let stack = &composited_stack.stack;
        if compositing_of_layers
            .iter()
            .skip(1)
            .all(|compositing| compositing.shown_by_window_server != composites_layers_the_window_server_shows)
        {
            return;
        }
        debug_assert!(self.raster_transform() == FloatMatrix4x4::identity());
        let tile_pool = &self.layers.tile_pool;
        composite_inside_effects_of_frame(env, surface, frame, |compositing_session| {
            let mut entered_composited_bands: Vec<BandIndex> = Vec::new();
            for compositing in compositing_of_layers
                .iter()
                .skip(1)
                .filter(|compositing| compositing.shown_by_window_server == composites_layers_the_window_server_shows)
                .filter(|compositing| !self.layer_overlay.hides_layer(&compositing.identity))
            {
                let Some((layer_index, layer)) = composited_stack.layer_composited_as(compositing) else {
                    continue;
                };
                let damage_grown_by_filter_reads = inflated(
                    damage_rect,
                    composited_filter_read_reach_by_layer
                        .get(layer_index.0)
                        .copied()
                        .unwrap_or(0),
                );
                let composited_bands_around_layer = composited_stack.composited_bands_around_layer(layer_index);
                let count_of_bands_still_entered = entered_composited_bands
                    .iter()
                    .zip(composited_bands_around_layer)
                    .take_while(|(entered, around_layer)| entered == around_layer)
                    .count();
                while entered_composited_bands.len() > count_of_bands_still_entered {
                    compositing_session.leave_effect();
                    entered_composited_bands.pop();
                }
                for &band_index in &composited_bands_around_layer[count_of_bands_still_entered..] {
                    let effect = stack.nodes[stack.bands[band_index.0].placement_node_index.0]
                        .composited_effect
                        .expect("a composited band is of a composited node");
                    compositing_session.enter_effect(effect);
                    entered_composited_bands.push(band_index);
                }
                if let LayerContent::Media { record_span, reach, .. } = &layer.content {
                    if !compositing
                        .rect_in_frame(*reach, damage_grown_by_filter_reads)
                        .is_empty()
                    {
                        compositing_session.replay_records(*record_span);
                    }
                    continue;
                }
                let Some(cached_layer) = self.layers.cached_layer_by_identity.get(&compositing.identity) else {
                    continue;
                };
                let clips_of_layer = CompositeClipsOfRaster {
                    rect: compositing.composite_clip,
                    rounded_rects: &compositing.rounded_composite_clips,
                };
                let (layer_to_frame_offset, layer_to_frame_transform) = match compositing.space_to_frame {
                    SpaceToFrame::Offset(offset) => (offset, None),
                    SpaceToFrame::Transform(transform) => (IntPoint::default(), Some(transform)),
                };
                for draw in draws_of_held_tiles_reaching_into(
                    &cached_layer.held_tiles,
                    tile_pool,
                    compositing,
                    damage_grown_by_filter_reads,
                ) {
                    match draw {
                        TileDraw::TileSurface {
                            surface,
                            rect_in_layer_space,
                        } => compositing_session.draw_raster(
                            surface.as_ptr(),
                            rect_of_size(size_of(rect_in_layer_space)),
                            rect_in_layer_space.translated(layer_to_frame_offset.x, layer_to_frame_offset.y),
                            clips_of_layer,
                            layer_to_frame_transform,
                        ),
                        TileDraw::FillOfJoinedOneColorTiles {
                            rect_in_layer_space,
                            color,
                        } if color.alpha() != 0 => {
                            compositing_session.fill_rect(
                                rect_in_layer_space.translated(layer_to_frame_offset.x, layer_to_frame_offset.y),
                                color,
                                clips_of_layer,
                                layer_to_frame_transform,
                            );
                        }
                        TileDraw::FillOfJoinedOneColorTiles { .. } => {}
                    }
                }
            }
            for _ in entered_composited_bands {
                compositing_session.leave_effect();
            }
        });
    }

    fn present_painted_layers_to_window_server(
        &mut self,
        env: &PresentingEnvironment<'_>,
        stack: &LayerStack,
        compositing_of_layers: &[LayerCompositingInFrame],
        like: &SurfaceHandle,
    ) -> bool {
        let raster = env.raster;
        let viewport_rect = rect_of_size(self.viewport_size);
        let layers = &mut self.layers;
        for compositing in compositing_of_layers
            .iter()
            .skip(1)
            .filter(|compositing| compositing.shown_by_window_server)
            .filter(|compositing| {
                matches!(
                    stack.layers[compositing.layer_index.0].content,
                    LayerContent::Painted { .. }
                )
            })
        {
            let Some(cached_layer) = layers.cached_layer_by_identity.get_mut(&compositing.identity) else {
                continue;
            };
            let held_rect_in_frame =
                compositing.rect_in_frame(cached_layer.bounds_of_held_tiles(&layers.tile_pool), viewport_rect);
            let CachedLayer {
                held_tiles,
                content_version_of_held_tiles,
                window_server_presentation,
                ..
            } = cached_layer;
            if held_rect_in_frame.is_empty() {
                if let Some(presentation) = window_server_presentation {
                    presentation.latest_surface = None;
                }
                continue;
            }
            let presentation = window_server_presentation.get_or_insert_with(|| {
                let presentable_surface_pool = layers.next_presentable_surface_pool;
                layers.next_presentable_surface_pool += 1;
                PaintedLayerPresentation {
                    presentable_surface_pool,
                    latest_surface: None,
                    rect_in_frame: IntRect::default(),
                    copied_content_version: 0,
                    copied_with_compositing: None,
                }
            });
            if presentation.latest_surface.is_some()
                && presentation.rect_in_frame == held_rect_in_frame
                && presentation.copied_content_version == *content_version_of_held_tiles
                && presentation.copied_with_compositing.as_ref() == Some(compositing)
            {
                continue;
            }
            let Some(presentable) = raster.next_presentable_surface(
                env.context_id,
                presentation.presentable_surface_pool,
                size_of(held_rect_in_frame),
                like,
            ) else {
                return false;
            };
            let target = raster.painting_surface_of(&presentable);
            raster.clear(&target, Color::TRANSPARENT);
            let save_count = raster.save(&target);
            raster.translate(&target, -held_rect_in_frame.x as f32, -held_rect_in_frame.y as f32);
            copy_held_tiles_into(
                raster,
                &target,
                held_tiles,
                &layers.tile_pool,
                held_rect_in_frame,
                compositing,
            );
            raster.restore_to_count(&target, save_count);
            presentation.latest_surface = Some(presentable);
            presentation.rect_in_frame = held_rect_in_frame;
            presentation.copied_content_version = *content_version_of_held_tiles;
            presentation.copied_with_compositing = Some(compositing.clone());
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
        let mut overlays_rect = frame.frame_inputs.overlays.bounds;
        if self.layer_overlay.latest_frame().is_some() {
            overlays_rect = overlays_rect.united(self.raster_rect());
        }
        self.layers.compositor_overlays_rect = overlays_rect;
        if overlays_rect.is_empty() {
            self.layers.latest_compositor_overlays_surface = None;
            return true;
        }
        let Some(presentable) = raster.next_presentable_surface(
            env.context_id,
            PRESENTABLE_SURFACE_POOL_OF_COMPOSITOR_OVERLAYS,
            size_of(overlays_rect),
            like,
        ) else {
            return false;
        };
        let target = raster.painting_surface_of(&presentable);
        raster.clear(&target, Color::TRANSPARENT);
        let save_count = raster.save(&target);
        raster.translate(&target, -overlays_rect.x as f32, -overlays_rect.y as f32);
        paint_overlays(env, &target, frame.frame_inputs, true);
        self.paint_layer_overlay(env, &target);
        raster.restore_to_count(&target, save_count);
        self.layers.latest_compositor_overlays_surface = Some(presentable);
        true
    }

    pub(super) fn sublayers_shown_by_window_server(&self, live_content: &LiveContent) -> Vec<FfiSublayer> {
        let layers = &self.layers;
        let mut sublayers = Vec::new();
        let Some(composited_stack) = layers
            .stack_of_painted_frame
            .as_ref()
            .filter(|_| layers.window_server_shows_media())
        else {
            return sublayers;
        };
        for compositing in layers
            .compositing_of_latest_frame
            .iter()
            .skip(1)
            .filter(|compositing| compositing.shown_by_window_server)
            .filter(|compositing| !self.layer_overlay.hides_layer(&compositing.identity))
        {
            let Some((layer_index, layer)) = composited_stack.layer_composited_as(compositing) else {
                continue;
            };
            if let LayerContent::Media { content, .. } = &layer.content {
                let Some(Ok(placement)) = layers.window_server_placements_of_media.of_layer(layer_index) else {
                    continue;
                };
                let LayerIdentity::Media { ordinal, .. } = layer.identity else {
                    unreachable!("a media layer has a media identity");
                };
                let (kind, id, sublayer_identity_of_media, has_content) = match content {
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
                    identity: sublayer_identity_of_media
                        | (u64::from(ordinal) << MEDIA_ORDINAL_SHIFT_PAST_CANVAS_AND_VIDEO_IDS),
                    content_kind: kind,
                    media_id: id,
                    presentable_surface: std::ptr::null_mut(),
                    placement: sublayer_placement_of(placement),
                });
                continue;
            }
            let Some(presentation) = layers
                .cached_layer_by_identity
                .get(&compositing.identity)
                .and_then(|cached_layer| cached_layer.window_server_presentation.as_ref())
            else {
                continue;
            };
            let Some(surface) = &presentation.latest_surface else {
                continue;
            };
            sublayers.push(unclipped_untransformed_surface_sublayer(
                PAINTED_LAYER_SUBLAYER_IDENTITY_TAG | u64::from(presentation.presentable_surface_pool),
                surface,
                presentation.rect_in_frame,
            ));
        }
        if let Some(surface) = &layers.latest_compositor_overlays_surface {
            sublayers.push(unclipped_untransformed_surface_sublayer(
                COMPOSITOR_OVERLAY_SUBLAYER_IDENTITY,
                surface,
                layers.compositor_overlays_rect,
            ));
        }
        sublayers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libcompositing_rust::display_list::builder::DisplayListBuilder;
    use libcompositing_rust::display_list::commands::{
        ContextRef, EffectNodeIndex, FillRect, SpatialNodeIndex, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use libcompositing_rust::display_list::storage::DisplayListStorage;
    use libcompositing_rust::visual_context::{SpatialData, TransformData, TransformDataRole};
    use libgfx_rust::{AffineTransform, CompositingAndBlendingOperator, FloatRect};
    use std::sync::Arc;

    const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);

    // How bands matched the tile keys of the latest frame when each was compared with each.
    fn tile_keys_reused_by_comparing_every_band_with_every_band(
        band_tile_keys_of_latest_frame: &mut Vec<BandTileKeysOfFrame>,
        identities_of_bands: &[BandIdentities],
    ) -> Vec<Option<BandTileKeysOfFrame>> {
        let mut reused_by_band: Vec<Option<BandTileKeysOfFrame>> = identities_of_bands.iter().map(|_| None).collect();
        for identity_of in BAND_IDENTITY_KINDS_IN_MATCHING_ORDER {
            for (reused, identities) in reused_by_band.iter_mut().zip(identities_of_bands) {
                if reused.is_some() {
                    continue;
                }
                if let Some(index) = band_tile_keys_of_latest_frame
                    .iter()
                    .position(|band_tile_keys| identity_of(&band_tile_keys.identities) == identity_of(identities))
                {
                    *reused = Some(band_tile_keys_of_latest_frame.swap_remove(index));
                }
            }
        }
        reused_by_band
    }

    fn band_tile_keys_of(identities: BandIdentities) -> BandTileKeysOfFrame {
        BandTileKeysOfFrame {
            identities,
            bound_to_list_and_geometry: None,
            tile_keys: BandTileKeys::default(),
        }
    }

    fn identities_of(band_tile_keys: &[Option<BandTileKeysOfFrame>]) -> Vec<Option<(u64, u64)>> {
        band_tile_keys
            .iter()
            .map(|band_tile_keys| {
                band_tile_keys.as_ref().map(|band_tile_keys| {
                    (
                        band_tile_keys.identities.identity_by_first_record_position,
                        band_tile_keys.identities.identity_by_band_ordinal,
                    )
                })
            })
            .collect()
    }

    #[test]
    fn bands_found_by_identity_match_as_bands_compared_each_with_each() {
        // Few distinct identities of each kind, so that many bands share one.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |range: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % range
        };
        for _ in 0..500 {
            let latest_count = next(24) as usize;
            let band_count = next(24) as usize;
            let mut identities = |count: usize| -> Vec<BandIdentities> {
                (0..count)
                    .map(|_| BandIdentities {
                        identity_by_first_record_position: next(6),
                        identity_by_band_ordinal: next(4),
                    })
                    .collect()
            };
            let latest = identities(latest_count);
            let bands = identities(band_count);
            let mut latest_found_by_identity: Vec<BandTileKeysOfFrame> =
                latest.iter().copied().map(band_tile_keys_of).collect();
            let mut latest_compared_each_with_each: Vec<BandTileKeysOfFrame> =
                latest.iter().copied().map(band_tile_keys_of).collect();
            let found_by_identity = reuse_tile_keys_of_matching_bands(&mut latest_found_by_identity, &bands);
            let compared_each_with_each =
                tile_keys_reused_by_comparing_every_band_with_every_band(&mut latest_compared_each_with_each, &bands);
            assert_eq!(
                identities_of(&found_by_identity),
                identities_of(&compared_each_with_each)
            );
            let left_over =
                |left: Vec<BandTileKeysOfFrame>| identities_of(&left.into_iter().map(Some).collect::<Vec<_>>());
            assert_eq!(
                left_over(latest_found_by_identity),
                left_over(latest_compared_each_with_each)
            );
        }
    }

    fn identities(identity_by_first_record_position: u64, identity_by_band_ordinal: u64) -> BandIdentities {
        BandIdentities {
            identity_by_first_record_position,
            identity_by_band_ordinal,
        }
    }

    #[test]
    fn bands_drawn_with_the_same_stack_take_the_tile_keys_of_their_own_index() {
        let band_tile_keys_by_band = vec![
            Some(band_tile_keys_of(identities(1, 1))),
            None,
            Some(band_tile_keys_of(identities(2, 2))),
        ];
        let (taken, are_of_same_stack) = BandTileKeysOfLatestFrame::OfSameStack(band_tile_keys_by_band)
            .taken_by_bands(|| panic!("the bands of the same stack are not matched by identity"));
        assert_eq!(identities_of(&taken), [Some((1, 1)), None, Some((2, 2))]);
        assert!(are_of_same_stack);
    }

    #[test]
    fn bands_drawn_with_another_stack_take_the_tile_keys_found_by_their_identities() {
        // The first band starts with the records that the second band of the latest frame started with, the second is
        // the band of the node that the first band of the latest frame was, and the third is new.
        let latest = [identities(1, 10), identities(2, 20)];
        let bands = [identities(2, 99), identities(98, 10), identities(97, 97)];
        let band_taking_tile_keys_of_latest_band = [1, 0];
        // A flag of the tile keys tells which tile keys of the latest frame a band takes.
        for marked_latest_band in 0..latest.len() {
            let band_tile_keys_of_latest_frame = latest
                .iter()
                .enumerate()
                .map(|(latest_band, identities)| {
                    let mut band_tile_keys = band_tile_keys_of(*identities);
                    band_tile_keys.tile_keys.replays_whole_list = latest_band == marked_latest_band;
                    Some(band_tile_keys)
                })
                .collect();
            let (taken, are_of_same_stack) = BandTileKeysOfLatestFrame::OfOtherStack(band_tile_keys_of_latest_frame)
                .taken_by_bands(|| bands.to_vec());
            assert!(!are_of_same_stack);
            assert_eq!(
                identities_of(&taken),
                bands
                    .iter()
                    .map(|identities| Some((
                        identities.identity_by_first_record_position,
                        identities.identity_by_band_ordinal
                    )))
                    .collect::<Vec<_>>()
            );
            let bands_taking_marked_tile_keys: Vec<usize> = taken
                .iter()
                .enumerate()
                .filter(|(_, taken)| taken.as_ref().is_some_and(|taken| taken.tile_keys.replays_whole_list))
                .map(|(band, _)| band)
                .collect();
            assert_eq!(
                bands_taking_marked_tile_keys,
                [band_taking_tile_keys_of_latest_band[marked_latest_band]]
            );
        }
    }

    fn tree_of_viewport() -> Arc<VisualContextTree> {
        Arc::new(VisualContextTree::create(TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
            bounds: FloatRect::default(),
        }))
    }

    fn empty_display_list(tree: &Arc<VisualContextTree>) -> Rc<DisplayList> {
        Rc::new(DisplayList::new(
            DisplayListStorage::recorded(&DisplayListBuilder::new().finish(), tree.clone()),
            1,
            tree.structural_epoch,
            None,
            None,
            Default::default(),
        ))
    }

    fn one_layer_of(display_list: &Rc<DisplayList>, viewport_rect: IntRect) -> LayerStack {
        LayerStack::single_layer(viewport_rect, display_list.storage().whole_span(), None)
    }

    fn scaled_twice() -> FloatMatrix4x4 {
        let mut matrix = FloatMatrix4x4::identity();
        matrix.elements[0][0] = 2.0;
        matrix.elements[1][1] = 2.0;
        matrix
    }

    // Answers whether the frame takes a plan into use, and whether it keeps the band tile keys of the frame before, as
    // a frame keeps what frames kept of the stack, after which the frame leaves its own.
    fn take_stack_of_frame(
        layers: &mut ContextLayers,
        layer_plan: Option<&ReceivedLayerPlan>,
        one_layer: LayerStack,
    ) -> (bool, bool) {
        layers.work_counts_of_this_frame = FrameWorkCounts::default();
        layers.take_stack_of_frame(layer_plan, |_| one_layer, FloatMatrix4x4::identity());
        let stack_of_frames = layers.stack_of_frames.as_mut().expect("the frame took a stack");
        let keeps_band_tile_keys = matches!(
            stack_of_frames.band_tile_keys_of_latest_frame,
            BandTileKeysOfLatestFrame::OfSameStack(_)
        );
        stack_of_frames.band_tile_keys_of_latest_frame = BandTileKeysOfLatestFrame::OfSameStack(Vec::new());
        (
            layers.work_counts_of_this_frame.took_plan_into_use,
            keeps_band_tile_keys,
        )
    }

    #[test]
    fn frames_keep_what_they_kept_of_a_stack_only_while_they_draw_with_the_same_plan() {
        let tree = tree_of_viewport();
        let display_list = empty_display_list(&tree);
        let plan = ReceivedLayerPlan {
            viewport_rect: VIEWPORT,
            stack: Rc::new(one_layer_of(&display_list, VIEWPORT)),
            leaves_out_scroll_offset_of_node: Rc::default(),
            note_lines: Rc::default(),
        };
        // A plan received again is another plan, even where it holds what the plan before held.
        let plan_received_again = ReceivedLayerPlan {
            stack: Rc::new((*plan.stack).clone()),
            ..plan.clone()
        };
        let mut layers = ContextLayers::default();
        let one_layer = || one_layer_of(&display_list, VIEWPORT);
        assert_eq!(
            take_stack_of_frame(&mut layers, Some(&plan), one_layer()),
            (true, false)
        );
        assert_eq!(
            take_stack_of_frame(&mut layers, Some(&plan), one_layer()),
            (false, true)
        );
        assert_eq!(
            take_stack_of_frame(&mut layers, Some(&plan_received_again), one_layer()),
            (true, false)
        );
        assert_eq!(take_stack_of_frame(&mut layers, None, one_layer()), (false, false));
        assert_eq!(take_stack_of_frame(&mut layers, None, one_layer()), (false, false));
        assert_eq!(
            take_stack_of_frame(&mut layers, Some(&plan_received_again), one_layer()),
            (true, false)
        );
    }

    #[test]
    fn a_new_stack_keeps_the_geometries_of_spaces_at_the_same_anchors_under_the_same_raster_transform() {
        let tree = tree_of_viewport();
        let display_list = empty_display_list(&tree);
        let identity = FloatMatrix4x4::identity();
        let resized_viewport = IntRect::new(0, 0, 400, 300);
        let mut layers = ContextLayers::default();
        layers.take_stack_of_frame(None, |_| one_layer_of(&display_list, VIEWPORT), identity);
        let stack_of_frames = layers.stack_of_frames.as_mut().expect("the frame took a stack");
        let geometry = stack_of_frames.geometries_of_spaces(&tree, &[]).remove(0);
        stack_of_frames.band_tile_keys_of_latest_frame = BandTileKeysOfLatestFrame::OfSameStack(vec![
            Some(band_tile_keys_of(identities(1, 1))),
            None,
            Some(band_tile_keys_of(identities(2, 2))),
        ]);

        layers.take_stack_of_frame(None, |_| one_layer_of(&display_list, resized_viewport), identity);
        let resized = layers.stack_of_frames.as_mut().expect("the frame took a stack");
        assert!(Rc::ptr_eq(&resized.geometries_of_spaces(&tree, &[])[0], &geometry));
        let BandTileKeysOfLatestFrame::OfOtherStack(band_tile_keys_of_other_stack) = std::mem::replace(
            &mut resized.band_tile_keys_of_latest_frame,
            BandTileKeysOfLatestFrame::OfSameStack(Vec::new()),
        ) else {
            panic!("the band tile keys of a frame drawn with another stack are matched by identity");
        };
        assert_eq!(
            identities_of(&band_tile_keys_of_other_stack),
            [Some((1, 1)), None, Some((2, 2))]
        );

        layers.take_stack_of_frame(None, |_| one_layer_of(&display_list, resized_viewport), scaled_twice());
        let scaled = layers.stack_of_frames.as_ref().expect("the frame took a stack");
        assert!(scaled.geometry_by_space.iter().all(Option::is_none));
    }

    fn layer_composited_through(transform: AffineTransform) -> LayerCompositingInFrame {
        LayerCompositingInFrame {
            layer_index: LayerIndex(1),
            identity: LayerIdentity::Painted {
                node: PlacementNodeIdentity(1),
                band_ordinal_in_node: 0,
                above_barrier: None,
            },
            space_to_frame: SpaceToFrame::Transform(transform),
            composite_clip: None,
            rounded_composite_clips: Rc::new([]),
            shown_by_window_server: false,
        }
    }

    fn one_color_tiles_at(coords: &[TileCoord]) -> (BTreeMap<TileCoord, ContentHash>, TilePool) {
        let held_tiles: BTreeMap<TileCoord, ContentHash> = coords
            .iter()
            .enumerate()
            .map(|(index, coord)| (*coord, index as ContentHash + 1))
            .collect();
        let tile_pool: TilePool = held_tiles
            .iter()
            .map(|(coord, content_hash)| {
                (
                    *content_hash,
                    PooledTile {
                        // A color of its own, so that no two tiles join into one fill.
                        pixels: TilePixels::OneColor(Color::from_rgb(0, 128, *content_hash as u8)),
                        held_part_of_cell: coord.cell_rect(),
                        holds_current_pixels: true,
                        last_held_at: MonotonicTime::default(),
                    },
                )
            })
            .collect();
        (held_tiles, tile_pool)
    }

    fn rects_of_draws(draws: &[TileDraw<'_>]) -> Vec<IntRect> {
        draws
            .iter()
            .map(|draw| match draw {
                TileDraw::TileSurface {
                    rect_in_layer_space, ..
                }
                | TileDraw::FillOfJoinedOneColorTiles {
                    rect_in_layer_space, ..
                } => *rect_in_layer_space,
            })
            .collect()
    }

    // A box that the compositor scales up from nothing shows at a tiny part of its raster scale at first.
    #[test]
    fn a_layer_shown_at_a_tiny_scale_draws_its_held_tiles_without_visiting_every_cell_the_viewport_maps_to() {
        let tiny_scale = 1.0e-5;
        let layer = layer_composited_through(AffineTransform::new(tiny_scale, 0.0, 0.0, tiny_scale, 400.0, 300.0));
        let shown_rect = layer.rect_shown_in_layer_space(VIEWPORT, 0);
        assert!(CellRange::touched_by(shown_rect).cell_count() > 1_000_000_000);
        let held_cells = [TileCoord::new(-1, -1), TileCoord::new(0, 0), TileCoord::new(3, 0)];
        let (held_tiles, tile_pool) = one_color_tiles_at(&held_cells);
        let draws = draws_of_held_tiles_reaching_into(&held_tiles, &tile_pool, &layer, VIEWPORT);
        assert_eq!(rects_of_draws(&draws), held_cells.map(TileCoord::cell_rect));
    }

    #[test]
    fn a_layer_whose_transform_is_not_finite_or_has_no_inverse_shows_nothing() {
        let (held_tiles, tile_pool) = one_color_tiles_at(&[TileCoord::new(0, 0)]);
        for transform in [
            AffineTransform::new(f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0),
            AffineTransform::new(1.0, 0.0, 0.0, 1.0, f32::INFINITY, 0.0),
            AffineTransform::new(0.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            AffineTransform::new(1.0e-30, 0.0, 0.0, 1.0e-30, 0.0, 0.0),
        ] {
            let layer = layer_composited_through(transform);
            assert_eq!(layer.rect_shown_in_layer_space(VIEWPORT, 0), IntRect::default());
            assert_eq!(
                layer.rect_in_frame(TileCoord::new(0, 0).cell_rect(), VIEWPORT),
                IntRect::default()
            );
            assert!(draws_of_held_tiles_reaching_into(&held_tiles, &tile_pool, &layer, VIEWPORT).is_empty());
        }
    }

    struct NoLiveContent;

    impl LiveContentVersions for NoLiveContent {
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

    // A red plane in front of a blue plane that covers the viewport, though the red plane comes first in the list, as
    // `transform-style: preserve-3d` and `translateZ()` make them.
    fn planes_sorted_by_depth() -> (Arc<VisualContextTree>, Rc<DisplayList>) {
        let mut tree = (*tree_of_viewport()).clone();
        let plane = |tree: &mut VisualContextTree,
                     parent: SpatialNodeIndex,
                     sorting_context_root: Option<SpatialNodeIndex>,
                     matrix: FloatMatrix4x4| {
            tree.append_spatial(
                SpatialData::Transform(TransformData {
                    matrix,
                    origin: FloatPoint::default(),
                    sorting_context_root_index: sorting_context_root,
                    flattens_inherited_transform: false,
                    role: TransformDataRole::CssTransform,
                    synthetic_plane: false,
                    establishes_sorting_context: sorting_context_root.is_none(),
                    bounds: VIEWPORT.to_float(),
                }),
                parent,
            )
        };
        let context_root = plane(&mut tree, VISUAL_VIEWPORT_NODE_INDEX, None, FloatMatrix4x4::identity());
        let mut moved_forward = FloatMatrix4x4::identity();
        moved_forward.elements[2][3] = 10.0;
        let red_plane = plane(&mut tree, context_root, Some(context_root), moved_forward);
        let blue_plane = plane(&mut tree, context_root, Some(context_root), FloatMatrix4x4::identity());
        let tree = Arc::new(tree);
        let mut builder = DisplayListBuilder::new();
        for (spatial, color) in [
            (red_plane, Color::from_rgb(255, 0, 0)),
            (blue_plane, Color::from_rgb(0, 0, 255)),
        ] {
            builder.append(
                &FillRect {
                    rect: VIEWPORT,
                    color,
                    compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
                    background_color_animation_effect: EffectNodeIndex::NONE,
                },
                &[],
                ContextRef {
                    spatial,
                    ..ContextRef::default()
                },
            );
        }
        let display_list = Rc::new(DisplayList::new(
            DisplayListStorage::recorded(&builder.finish(), tree.clone()),
            1,
            tree.structural_epoch,
            None,
            None,
            Default::default(),
        ));
        (tree, display_list)
    }

    // A frame is not drawn as planned at a pinch, at a resize, or under the raster transform of a nested context.
    #[test]
    fn the_tiles_of_a_frame_not_drawn_as_planned_show_planes_sorted_by_depth() {
        let (tree, display_list) = planes_sorted_by_depth();
        assert!(tree.may_sort_planes_by_depth());
        let mut layers = ContextLayers::default();
        let stack = layers.one_layer_stack_of_frame(&display_list, &tree, VIEWPORT);
        let list_keying = TileKeyingOfList::new(
            display_list.storage(),
            &tree,
            &stack,
            FrameStateReadByReplay {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContent,
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
        let white = Color::from_rgb(255, 255, 255);
        let mut band_tile_keys = BandTileKeys::default();
        raster_space.bind_band(&mut band_tile_keys, &stack, BandIndex(0));
        raster_space.key_shown_tiles(
            &mut band_tile_keys,
            &stack,
            BandIndex(0),
            |_| LayerRasterSettings {
                raster_space_identity: 0,
                clear_color: white,
                root_group_color: Color::TRANSPARENT,
                viewport_to_raster_transform: FloatMatrix4x4::identity(),
                allows_lcd_text: false,
            },
            VIEWPORT,
            VIEWPORT,
            None,
        );
        // Walked in list order from the opaque clear color, the blue fill would cover what the red plane draws.
        let tile = band_tile_keys
            .tile(TileCoord::new(0, 0), stack.base_layer)
            .expect("the base layer holds a tile at every shown cell");
        assert_eq!(
            raster_space.coverage_of_tile(&band_tile_keys, tile, tile.coord.cell_rect(), white),
            TileCoverage::Unknown
        );
        assert!(band_tile_keys.replays_whole_list);

        // The list and the tree are looked at once for the frames that draw them.
        let obstacles_of_list_and_tree = stack.layering_obstacles;
        layers
            .layering_obstacles_of_frames_not_drawn_as_planned
            .as_mut()
            .expect("the obstacles of the list and the tree are kept")
            .obstacles
            .mask_declared_under_effects_its_mask_is_not_under = true;
        let stack_of_next_frame = layers.one_layer_stack_of_frame(&display_list, &tree, VIEWPORT);
        assert!(
            stack_of_next_frame
                .layering_obstacles
                .mask_declared_under_effects_its_mask_is_not_under
        );
        let another_tree = Arc::new((*tree).clone());
        let stack_of_another_tree = layers.one_layer_stack_of_frame(&display_list, &another_tree, VIEWPORT);
        assert_eq!(stack_of_another_tree.layering_obstacles, obstacles_of_list_and_tree);
    }

    // A spinner that the compositor rotates, whose label a new list makes shorter.
    #[test]
    fn a_layer_moved_by_an_animated_transform_damages_what_it_showed_in_the_frame_before() {
        let tree = tree_of_viewport();
        let display_list = empty_display_list(&tree);
        let short_label = IntRect::new(0, 0, 100, 20);
        let long_label = IntRect::new(0, 0, 300, 20);
        // The one layer of the stack reaches as far as the short label.
        let stack = one_layer_of(&display_list, short_label);
        let composited_at = |y: f32| LayerCompositingInFrame {
            layer_index: LayerIndex(0),
            identity: stack.layers[0].identity,
            space_to_frame: SpaceToFrame::Transform(AffineTransform::new(1.0, 0.0, 0.0, 1.0, 100.0, y)),
            composite_clip: None,
            rounded_composite_clips: Rc::new([]),
            shown_by_window_server: false,
        };
        let (latest, current) = (composited_at(100.0), composited_at(200.0));
        let tile_pool: TilePool = HashMap::from([(
            1,
            PooledTile {
                pixels: TilePixels::OneColor(Color::from_rgb(0, 0, 0)),
                held_part_of_cell: long_label,
                holds_current_pixels: true,
                last_held_at: MonotonicTime::default(),
            },
        )]);
        let cached_layer_by_identity = HashMap::from([(
            latest.identity,
            CachedLayer {
                held_tiles: BTreeMap::from([(TileCoord::new(0, 0), 1)]),
                ..CachedLayer::default()
            },
        )]);
        let layer_update = LayerUpdate {
            tile_state: None,
            composite_offsets: Vec::new(),
            spaces_to_frame: Vec::new(),
            compositing_of_layers: vec![current.clone()],
            read_reaches_of_composited_effects_in_frame: Vec::new(),
        };
        let damage = damage_of_layers_moved_by_animated_transforms(
            &stack,
            std::slice::from_ref(&latest),
            &cached_layer_by_identity,
            &tile_pool,
            &layer_update,
            VIEWPORT,
        )
        .expect("only an animated transform moved the layer");
        assert!(damage.contains_rect(latest.rect_in_frame(long_label, VIEWPORT)));
        assert!(damage.contains_rect(current.rect_in_frame(short_label, VIEWPORT)));
    }

    // A frame can find no store to be painted into once it took its stack.
    #[test]
    fn a_frame_that_is_not_painted_leaves_the_stack_the_frame_on_screen_is_composited_with() {
        let tree = tree_of_viewport();
        let painted_list = empty_display_list(&tree);
        let identity = FloatMatrix4x4::identity();
        let mut layers = ContextLayers::default();
        layers.take_stack_of_frame(None, |_| one_layer_of(&painted_list, VIEWPORT), identity);
        let painted_stack = layers
            .stack_of_frames
            .as_ref()
            .expect("the frame took a stack")
            .stack
            .clone();
        let compositing_of_painted_frame = vec![compositing_of_layer(
            &painted_stack,
            LayerIndex(0),
            &[IntPoint::default()],
            &[SpaceToFrame::Offset(IntPoint::default())],
        )];
        layers.take_in_painted_frame(LayerUpdate {
            tile_state: None,
            composite_offsets: vec![IntPoint::default()],
            spaces_to_frame: vec![SpaceToFrame::Offset(IntPoint::default())],
            compositing_of_layers: compositing_of_painted_frame.clone(),
            read_reaches_of_composited_effects_in_frame: Vec::new(),
        });

        let plan_of_next_list = ReceivedLayerPlan {
            viewport_rect: VIEWPORT,
            stack: Rc::new(one_layer_of(&empty_display_list(&tree), VIEWPORT)),
            leaves_out_scroll_offset_of_node: Rc::default(),
            note_lines: Rc::default(),
        };
        layers.take_stack_of_frame(
            Some(&plan_of_next_list),
            |_| unreachable!("a frame drawn with a plan takes its stack"),
            identity,
        );
        let stack_of_frame_on_screen = layers
            .stack_of_painted_frame
            .as_ref()
            .expect("the frame on screen was drawn with a stack");
        assert!(Rc::ptr_eq(&stack_of_frame_on_screen.stack, &painted_stack));
        assert!(layers.compositing_of_latest_frame == compositing_of_painted_frame);
    }
}
