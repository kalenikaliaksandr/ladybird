/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The layers a display list is painted in, bottom to top: the stack, what the compositor and tile keying read of it,
//! and its wire form.
//!
//! Content is rastered in the space of the scroll node, sticky box or node whose transform the compositor animates
//! that it moves with, and its raster is composited within the clips it leaves to apply then (its composite clips), and
//! inside the effects the compositor applies to it: content of one space, one set of composite clips and at most one
//! composited effect makes a placement node. The records of a node are cut into bands. Within a band, a canvas, video
//! or nested context that is a layer of its own, and each band of a node nested in it, sits between the band's records
//! as a barrier. A barrier cuts the band's tiles that it can show in, or for an effect that reads what is composited
//! below it, that it reads, at its paint position: there, the band's records painted before the barrier go into the
//! layer below the cut and those painted after it into the layer above the cut. Elsewhere it cuts nothing.

use crate::display_list::commands::{EffectNodeIndex, SpatialNodeIndex};
use crate::display_list::effect_geometry::filter_may_spread_at_some_sample;
pub use crate::display_list::placement::{
    CompositeClip, RasterSpace, RasterSpaceAnchorKind, RasterSpaceIndex, RunGroupIndex, raster_space_anchors,
};
use crate::display_list::placement::{
    EXTENT_LIMIT_IN_DEVICE_PIXELS, RasterSpaceGeometry, enclosing_int_rect_within_extent_limit, is_finite_rect,
    space_and_ancestors_below_viewport, spatial_nodes_that_move_on_their_own,
};
use crate::display_list::reach::Reach;
use crate::display_list::storage::{DisplayListStorage, ListPosition, ListSpan};
use crate::display_list::summary::LiveContent;
use crate::display_list::window_server_placement::{WhyWindowServerCannotShowMedia, WindowServerPlacement};
use crate::visual_context::{EffectNodeData, VisualContextTree, WorldTransforms, device_offset_for_index};
use libgfx_rust::{AffineTransform, CornerRadii, FloatMatrix4x4, FloatPoint, IntPoint, IntRect};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};
use std::rc::Rc;

pub mod planning;
mod wire;

pub use wire::{LayerPlan, UnvalidatedLayerPlan, UnvalidatedLayerStack};

// Twice the extent limit of a plan, which leaves room for the margins that planning adds past the extent of a space.
// The rects of a stack from another process are within it, and so are the offsets a frame composites spaces at, so that
// a rect offset by such an offset still fits in an `IntRect`.
const COORDINATE_LIMIT_IN_DEVICE_PIXELS: i32 = 2 * EXTENT_LIMIT_IN_DEVICE_PIXELS as i32;

pub const MAXIMUM_MEDIA_LAYER_COUNT: usize = 8;

pub(crate) fn media_name(content: LiveContent) -> &'static str {
    match content {
        LiveContent::Canvas(_) => "canvas",
        LiveContent::VideoFrame(_) => "video",
        LiveContent::ImageFrame(_) => "image",
        LiveContent::CompositedContext(_) => "composited context",
    }
}

/// Why the compositor cannot composite an effect, which keeps what it holds from having layers of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyCompositorCannotCompositeEffect {
    FilterMovesWhatItFilters,
    FilterReadsFartherThanTilesAreFollowed,
}

impl std::fmt::Display for WhyCompositorCannotCompositeEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyCompositorCannotCompositeEffect::FilterMovesWhatItFilters => "filter that moves what it filters",
            WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed => {
                "filter that reads farther than tiles are followed"
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyMediaHasNoLayer {
    DrawnInsideAnotherCommand,
    UnderTransformThatIsNot2DAffine,
    DrawsNothingVisible,
    MoreMediaThanLayersFor,
    SortedByDepthInA3DSortingContext,
    InsideAnEffectTheCompositorCannotComposite(WhyCompositorCannotCompositeEffect),
}

impl std::fmt::Display for WhyMediaHasNoLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyMediaHasNoLayer::DrawnInsideAnotherCommand => "drawn inside another command",
            WhyMediaHasNoLayer::UnderTransformThatIsNot2DAffine => "under a transform that is not 2D affine",
            WhyMediaHasNoLayer::DrawsNothingVisible => "draws nothing visible",
            WhyMediaHasNoLayer::MoreMediaThanLayersFor => "past the most media that get layers",
            WhyMediaHasNoLayer::SortedByDepthInA3DSortingContext => "sorted by depth in a 3D sorting context",
            WhyMediaHasNoLayer::InsideAnEffectTheCompositorCannotComposite(why) => {
                return write!(formatter, "inside a {why}");
            }
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
    TheCompositorAnimatesIt,
    ItAppliesOverATransformTheCompositorAnimates,
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
            WhyEffectIsComposited::TheCompositorAnimatesIt => "the compositor animates it",
            WhyEffectIsComposited::ItAppliesOverATransformTheCompositorAnimates => {
                "it applies over a transform the compositor animates"
            }
        })
    }
}

/// Why a list is painted in a single layer that replays all of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyListIsOneLayer {
    EmptyViewport,
    EmptyDisplayList,
    SortingContext3D,
    RootIsolationEffectIsNotPlainGroup,
    MaskDeclaredUnderEffectsItsMaskIsNotUnder,
    RecordsBlendWithWhatOtherLayersHoldBelowThem,
    EffectTheCompositorCannotCompositeIsComposited(WhyCompositorCannotCompositeEffect),
    /// The frame draws the list otherwise than a plan of its layers is made for, as through a raster transform or a
    /// visual viewport transform the plan does not know.
    FrameIsNotDrawnAsPlanned,
}

impl std::fmt::Display for WhyListIsOneLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            WhyListIsOneLayer::EmptyViewport => "empty viewport",
            WhyListIsOneLayer::EmptyDisplayList => "empty display list",
            WhyListIsOneLayer::SortingContext3D => "3D sorting context holding an effect that reads or is composited",
            WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup => "root isolation effect is not a plain group",
            WhyListIsOneLayer::MaskDeclaredUnderEffectsItsMaskIsNotUnder => {
                "mask content declared under effects its mask is not under"
            }
            WhyListIsOneLayer::RecordsBlendWithWhatOtherLayersHoldBelowThem => {
                "records blend with what other layers hold below them"
            }
            WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited(why) => {
                return write!(formatter, "{why} is composited");
            }
            WhyListIsOneLayer::FrameIsNotDrawnAsPlanned => "frame is not drawn as the plan was made for",
        })
    }
}

/// How the lines that describe a stack name the visual context nodes they mention.
pub trait VisualContextNodeNamer {
    fn name_of_spatial_node(&self, index: SpatialNodeIndex) -> String;
    fn name_of_effect_node(&self, index: EffectNodeIndex) -> String;
}

/// Names visual context nodes by their indices in the tree.
pub struct NodeNamesFromIndices;

impl VisualContextNodeNamer for NodeNamesFromIndices {
    fn name_of_spatial_node(&self, index: SpatialNodeIndex) -> String {
        format!("s{}", index.0)
    }

    fn name_of_effect_node(&self, index: EffectNodeIndex) -> String {
        format!("e{}", index.0)
    }
}

/// Names a placement node by the identity of its space, the clips its rasters are composited within, and the effects
/// they are composited inside, which outlive the tree and the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlacementNodeIdentity(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarrierIdentity {
    /// The `ordinal`th drawing of a canvas, video or nested context in paint order.
    Media { content: LiveContent, ordinal: u32 },
    /// The `band_ordinal_in_node`th band of content of a node, counted over the whole list.
    NestedNodeBand {
        node: PlacementNodeIdentity,
        band_ordinal_in_node: u32,
    },
}

/// Names a layer from one list to the next, so that its tiles outlive both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerIdentity {
    Painted {
        node: PlacementNodeIdentity,
        band_ordinal_in_node: u32,
        above_barrier: Option<BarrierIdentity>,
    },
    Media {
        content: LiveContent,
        ordinal: u32,
    },
}

/// How the frame composites the rasters of a space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpaceToFrame {
    /// Translated by whole device pixels.
    Offset(IntPoint),
    /// Through a transform from the space's raster pixels to the frame's, for a space whose anchor the compositor
    /// animates.
    Transform(AffineTransform),
}

impl Eq for SpaceToFrame {}

// A rect that a transform maps past the extent limit shows nothing there, and is kept within the limit so that the
// arithmetic of the rect does not overflow.
impl SpaceToFrame {
    /// The frame's pixels that `rect_in_space` covers, or the bounds of them.
    pub fn rect_in_frame(self, rect_in_space: IntRect) -> IntRect {
        match self {
            SpaceToFrame::Offset(offset) => rect_in_space.translated(offset.x, offset.y),
            SpaceToFrame::Transform(_) if rect_in_space.is_empty() => IntRect::default(),
            SpaceToFrame::Transform(transform) => {
                enclosing_int_rect_within_extent_limit(transform.map_rect(rect_in_space.to_float()))
            }
        }
    }

    /// The space's pixels that `rect_in_frame` covers, or the bounds of them.
    pub fn rect_in_space(self, rect_in_frame: IntRect) -> IntRect {
        match self {
            SpaceToFrame::Offset(offset) => rect_in_frame.translated(-offset.x, -offset.y),
            SpaceToFrame::Transform(_) if rect_in_frame.is_empty() => IntRect::default(),
            SpaceToFrame::Transform(transform) => transform.inverse().map_or(IntRect::default(), |inverse| {
                enclosing_int_rect_within_extent_limit(inverse.map_rect(rect_in_frame.to_float()))
            }),
        }
    }
}

/// The index of a node in `LayerStack::nodes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlacementNodeIndex(pub usize);

/// The index of a band in `LayerStack::bands`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BandIndex(pub usize);

/// The index of a layer in `LayerStack::layers`, bottom to top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerIndex(pub usize);

/// Content rastered in one space, composited within one set of clips, and inside at most one effect that the
/// compositor applies, which a replay of the list would apply around it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementNode {
    pub identity: PlacementNodeIdentity,
    pub raster_space_index: RasterSpaceIndex,
    /// At most one per space, ordered by space.
    pub composite_clips: Vec<CompositeClip>,
    /// The effect the compositor composites the rasters of the node inside, within those it composites the parent
    /// node inside. The rasters leave it out, with the effects around it and the clips it is entered within.
    pub composited_effect: Option<EffectNodeIndex>,
    pub parent_index: Option<PlacementNodeIndex>,
    /// Where content of the node and of the nodes in it can show, or a composited effect read.
    pub reach_in_parent_node: Reach<IntRect>,
}

/// Where a barrier cuts the tiles of a band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarrierCut {
    pub barrier_first_record_position: ListPosition,
    /// The barrier cuts the tiles of the band's space this reaches.
    pub cut_reach: Reach<IntRect>,
    pub layer_above_cut: LayerIndex,
}

impl BarrierCut {
    pub fn cuts_cell(&self, cell: IntRect) -> bool {
        self.cut_reach.touches(cell)
    }
}

/// One band of a placement node: the records placed in the node while the band lasts, and the barriers that cut them.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementNodeBand {
    pub placement_node_index: PlacementNodeIndex,
    pub band_ordinal_in_node: u32,
    /// Where the first record painted in the band is, of the node or of a node inside it, which names the band from one
    /// list to the next where bands of the node painted before it come or go.
    pub first_record_position: ListPosition,
    pub parent_band_holding_this_band_as_barrier: Option<BandIndex>,
    /// In paint order.
    pub record_spans: Vec<ListSpan>,
    /// In paint order.
    pub cuts: Vec<BarrierCut>,
    pub layer_below_all_cuts: LayerIndex,
}

impl PlacementNodeBand {
    /// The layer the band's record at `record_position` goes into within the tile at `cell`: the one above the last
    /// barrier painted before it that cuts the tile.
    pub fn layer_of_record_in_cell(&self, record_position: ListPosition, cell: IntRect) -> LayerIndex {
        self.cuts
            .iter()
            .take_while(|cut| cut.barrier_first_record_position <= record_position)
            .filter(|cut| cut.cuts_cell(cell))
            .last()
            .map_or(self.layer_below_all_cuts, |cut| cut.layer_above_cut)
    }

    /// The layer of the band above the first `cut_count` of the barriers that cut the tile at `cell`, in paint order,
    /// or none where fewer cut it.
    pub fn layer_above_first_cuts_of_cell(&self, cell: IntRect, cut_count: usize) -> Option<LayerIndex> {
        match cut_count {
            0 => Some(self.layer_below_all_cuts),
            cut_count => self
                .cuts
                .iter()
                .filter(|cut| cut.cuts_cell(cell))
                .nth(cut_count - 1)
                .map(|cut| cut.layer_above_cut),
        }
    }

    /// How many of the barriers that cut the tile at `cell` are below `layer` of the band there, as
    /// `layer_above_first_cuts_of_cell` counts them.
    pub fn count_of_cuts_of_cell_below_layer(&self, cell: IntRect, layer: LayerIndex) -> usize {
        self.cuts
            .iter()
            .filter(|cut| cut.cuts_cell(cell))
            .position(|cut| cut.layer_above_cut == layer)
            .map_or(0, |position_among_cuts_of_cell| position_among_cuts_of_cell + 1)
    }

    /// Which barriers cut the tile at `cell`: tiles that answer alike place every record alike.
    pub fn cuts_of_cell(&self, cell: IntRect) -> BarrierCutsOfCell {
        let indices_of_cuts_of_cell = self
            .cuts
            .iter()
            .enumerate()
            .filter(|(_, cut)| cut.cuts_cell(cell))
            .map(|(cut_index, _)| cut_index);
        if self.cuts.len() <= u64::BITS as usize {
            BarrierCutsOfCell::BitPerCut(indices_of_cuts_of_cell.fold(0, |bits, cut_index| bits | (1u64 << cut_index)))
        } else {
            BarrierCutsOfCell::CutIndices(indices_of_cuts_of_cell.map(|cut_index| cut_index as u32).collect())
        }
    }
}

/// Which barriers of a band cut a tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarrierCutsOfCell {
    /// A bit per barrier, in paint order, of a band with no more barriers than bits.
    BitPerCut(u64),
    /// The indices of the barriers, in paint order, of a band with more.
    CutIndices(Rc<[u32]>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    /// The records of a band that the layer holds in some of its tiles.
    Painted { band_index: BandIndex },
    /// A canvas, video or nested context, replayed or shown by the window server.
    Media {
        content: LiveContent,
        record_span: ListSpan,
        /// In the raster coordinates of the node's space.
        reach: IntRect,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackLayer {
    pub identity: LayerIdentity,
    pub placement_node_index: PlacementNodeIndex,
    /// The band the layer holds the records of, or the one it is a barrier in.
    pub band_index: BandIndex,
    pub content: LayerContent,
    /// Whether any record can go into the layer anywhere; a layer that holds nothing has no tiles.
    pub may_hold_content: bool,
    /// Where what the layer holds can show, in the raster coordinates of its space.
    pub reach: IntRect,
}

/// An effect that the compositor composites the layers of its content inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompositedEffect {
    pub effect: EffectNodeIndex,
    pub placement_node_index: PlacementNodeIndex,
    /// Whether the effect reads what is composited below it: it reads the backdrop of a backdrop filter, or it blends.
    pub reads_what_is_composited_below_it: bool,
    /// Whether its filter moves what it composites past where it is drawn, so that it reads what is drawn inside it
    /// past each of its pixels.
    pub filter_spreads_what_it_composites: bool,
}

impl CompositedEffect {
    /// The effects the compositor composites the nodes of a stack inside, one per composited node in the order of the
    /// nodes, with what the tree tells of how each reads. Each of those effects is a node of `tree`.
    fn of_nodes(nodes: &[PlacementNode], tree: &VisualContextTree) -> Vec<CompositedEffect> {
        if nodes.iter().all(|node| node.composited_effect.is_none()) {
            return Vec::new();
        }
        let backdrop_reader_by_effect_index = backdrop_reader_by_reading_effect_index(tree);
        nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let effect = node.composited_effect?;
                Some(CompositedEffect {
                    effect,
                    placement_node_index: PlacementNodeIndex(index),
                    reads_what_is_composited_below_it: backdrop_reader_by_effect_index[effect.0 as usize].is_some(),
                    filter_spreads_what_it_composites: filter_may_spread_at_some_sample(tree, effect),
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
struct BackdropReader {
    effect_with_backdrop_filter: Option<EffectNodeIndex>,
}

// Per effect node, whether it reads what is below it, and if it reads for a backdrop filter, the effect of the filter.
fn backdrop_reader_by_reading_effect_index(tree: &VisualContextTree) -> Vec<Option<BackdropReader>> {
    let mut backdrop_reader_by_reading_effect_index = vec![None; tree.effect_nodes.len()];
    for (index, effect_node) in tree.effect_nodes.iter().enumerate() {
        let EffectNodeData::Effects(effects) = &effect_node.data else {
            continue;
        };
        let effect = EffectNodeIndex(index as u32);
        if effects.backdrop_filter.is_some() {
            let reading_effect = tree.node_reading_the_backdrop_of(effect);
            backdrop_reader_by_reading_effect_index[reading_effect.0 as usize] = Some(BackdropReader {
                effect_with_backdrop_filter: Some(effect),
            });
        } else if effects.blend_mode != libgfx_rust::CompositingAndBlendingOperator::Normal {
            backdrop_reader_by_reading_effect_index[index] = Some(BackdropReader {
                effect_with_backdrop_filter: None,
            });
        }
    }
    backdrop_reader_by_reading_effect_index
}

/// The layers of a list bottom to top, with the base layer, which is rastered over the surface's clear color and
/// replaces what is below it. Layers below the base layer hold nothing. A stack does not change once it is planned:
/// what a frame works out of it is kept apart from it.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerStack {
    /// The viewport's space, and for a stack of more than one layer, the spaces `RasterSpaces::spaces_at_anchors` makes
    /// at the anchors `raster_space_anchors` gives for the tree, without the extent of each space but the viewport's.
    pub spaces: Vec<RasterSpace>,
    pub nodes: Vec<PlacementNode>,
    pub bands: Vec<PlacementNodeBand>,
    pub layers: Vec<StackLayer>,
    /// In the order of their nodes, which is paint order.
    pub composited_effects: Vec<CompositedEffect>,
    pub base_layer: LayerIndex,
    /// Why the stack is one layer that replays the whole list, if it is.
    pub why_list_is_one_layer: Option<WhyListIsOneLayer>,
    pub layering_obstacles: LayeringObstacles,
    /// Per raster space, whether records its tiles hold move within it as nodes that move on their own, or that the
    /// compositor scrolls without the space following them, do. Canvases, videos and nested contexts with layers of
    /// their own hold none of those records.
    pub tiles_hold_content_moving_within_space_by_space_index: Vec<bool>,
    /// Per raster space whose anchor the compositor animates and that holds content, where that content can show in
    /// the raster coordinates of the space it is in wherever the animation takes it.
    pub swept_reach_in_parent_space_by_space_index: Vec<Option<IntRect>>,
    index_of_layer_with_identity: HashMap<LayerIdentity, LayerIndex>,
    has_layer_at_or_above_base_by_band_index: Vec<bool>,
}

/// Where the window server can show each canvas and video layer of a stack under the values of one frame, or why it
/// cannot, by layer index. A stack is kept while its spaces scroll and its media move with them, so each frame works
/// this out anew.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowServerPlacementsOfMedia {
    placement_by_layer_index: Vec<Option<Result<WindowServerPlacement, WhyWindowServerCannotShowMedia>>>,
}

impl WindowServerPlacementsOfMedia {
    /// Where the window server can show the canvas or video of the layer, or why it cannot. None for a layer that holds
    /// no canvas or video.
    pub fn of_layer(
        &self,
        layer_index: LayerIndex,
    ) -> Option<&Result<WindowServerPlacement, WhyWindowServerCannotShowMedia>> {
        self.placement_by_layer_index.get(layer_index.0)?.as_ref()
    }
}

/// Where the window server can show the canvases and videos of a stack under the values of one frame, and which layers
/// it shows above the frame where it can show every canvas and video it has a placement for, as the dump tells them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowServerPresentationOfMedia {
    pub placements: WindowServerPlacementsOfMedia,
    /// By layer index.
    pub shown_by_window_server_by_layer_index: Vec<bool>,
}

impl WindowServerPresentationOfMedia {
    fn layer_is_shown_by_window_server(&self, layer_index: LayerIndex) -> bool {
        self.shown_by_window_server_by_layer_index
            .get(layer_index.0)
            .copied()
            .unwrap_or(false)
    }
}

fn which_bands_have_layer_at_or_above_base(
    layers: &[StackLayer],
    band_count: usize,
    base_layer: LayerIndex,
) -> Vec<bool> {
    let mut has_layer_at_or_above_base_by_band_index = vec![false; band_count];
    for layer in layers.iter().skip(base_layer.0) {
        if let LayerContent::Painted { band_index } = layer.content {
            has_layer_at_or_above_base_by_band_index[band_index.0] = true;
        }
    }
    has_layer_at_or_above_base_by_band_index
}

fn index_of_layer_with_identity(layers: &[StackLayer]) -> HashMap<LayerIdentity, LayerIndex> {
    let mut index_by_identity = HashMap::with_capacity(layers.len());
    for (layer_index, layer) in layers.iter().enumerate() {
        index_by_identity
            .entry(layer.identity)
            .or_insert(LayerIndex(layer_index));
    }
    index_by_identity
}

fn identity_of_node(
    spaces: &[RasterSpace],
    raster_space_index: RasterSpaceIndex,
    composite_clips: &[CompositeClip],
) -> PlacementNodeIdentity {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x706c_6163_656d_656e).build_hasher();
    hasher.write_u64(spaces[raster_space_index.0].identity);
    for clip in composite_clips {
        hasher.write_u64(spaces[clip.raster_space_index.0].identity);
        for value in [clip.rect.x, clip.rect.y, clip.rect.width, clip.rect.height] {
            hasher.write_i32(value);
        }
        clip.corner_radii.hash(&mut hasher);
    }
    PlacementNodeIdentity(hasher.finish())
}

// An effect index names the effect for as long as the tree keeps the node, which outlives lists and scrolls.
fn identity_of_composited_node(
    parent_identity: PlacementNodeIdentity,
    effect: EffectNodeIndex,
) -> PlacementNodeIdentity {
    let mut hasher = foldhash::fast::FixedState::with_seed(0x636f_6d70_6f73_6974).build_hasher();
    hasher.write_u64(parent_identity.0);
    hasher.write_u32(effect.0);
    PlacementNodeIdentity(hasher.finish())
}

fn node_covers_viewport(nodes: &[PlacementNode], spaces: &[RasterSpace], node_index: PlacementNodeIndex) -> bool {
    let mut node_on_way_out = Some(node_index);
    while let Some(index) = node_on_way_out {
        let node = &nodes[index.0];
        if !node.composite_clips.is_empty()
            || node.composited_effect.is_some()
            || node.reach_in_parent_node != Reach::Anywhere
            || (node.parent_index.is_some()
                && spaces[node.raster_space_index.0].anchor_kind != RasterSpaceAnchorKind::ViewportScroller)
        {
            return false;
        }
        node_on_way_out = node.parent_index;
    }
    true
}

/// What a plan says of a stack of more than one layer, each thing once: the nodes without their identities, and the
/// bands in the order of a walk that enters the first band, and at each cut whose barrier is a nested band, the next
/// band, each band with what only it knows of its layers. `LayerStack::of_layered_plan` works out the rest.
#[derive(Clone, Debug)]
struct LayeredPlan {
    nodes: Vec<NodeOfPlan>,
    bands: Vec<BandOfPlan>,
    tiles_hold_content_moving_within_space_by_space_index: Vec<bool>,
    swept_reach_in_parent_space_by_space_index: Vec<Option<IntRect>>,
}

/// A placement node without its identity, which the stack works out of the node, the node it is in and the spaces.
#[derive(Clone, Debug)]
struct NodeOfPlan {
    raster_space_index: RasterSpaceIndex,
    composite_clips: Vec<CompositeClip>,
    composited_effect: Option<EffectNodeIndex>,
    parent_index: Option<PlacementNodeIndex>,
    reach_in_parent_node: Reach<IntRect>,
}

#[derive(Clone, Debug)]
struct BandOfPlan {
    placement_node_index: PlacementNodeIndex,
    first_record_position: ListPosition,
    /// In paint order.
    record_spans: Vec<ListSpan>,
    layer_below_all_cuts: PaintedLayerOfPlan,
    /// In paint order.
    cuts: Vec<CutOfPlan>,
}

#[derive(Clone, Copy, Debug)]
struct CutOfPlan {
    barrier: BarrierOfPlan,
    layer_above_cut: PaintedLayerOfPlan,
}

#[derive(Clone, Copy, Debug)]
enum BarrierOfPlan {
    /// A canvas, video or nested context that is a layer of its own, the `ordinal`th drawing of its content in paint
    /// order, which cuts the tiles its reach touches.
    Media {
        content: LiveContent,
        ordinal: u32,
        record_span: ListSpan,
        reach: IntRect,
    },
    /// The next band of the walk, which is of a node in the band's node and cuts the tiles that node reaches.
    NestedBand,
}

/// What a band knows of one of its painted layers.
#[derive(Clone, Copy, Debug, Default)]
struct PaintedLayerOfPlan {
    may_hold_content: bool,
    reach: IntRect,
}

// A band the walk of the bands of a plan is in, with the cuts of it that the walk has yet to pass, and the layer above
// the cut whose nested band the walk is in.
struct BandOfWalk {
    band_index: BandIndex,
    cuts_to_pass: std::vec::IntoIter<CutOfPlan>,
    layer_above_nested_band: PaintedLayerOfPlan,
}

fn painted_layer_of_band(
    nodes: &[PlacementNode],
    bands: &[PlacementNodeBand],
    band_index: BandIndex,
    above_barrier: Option<BarrierIdentity>,
    layer: PaintedLayerOfPlan,
) -> StackLayer {
    let band = &bands[band_index.0];
    StackLayer {
        identity: LayerIdentity::Painted {
            node: nodes[band.placement_node_index.0].identity,
            band_ordinal_in_node: band.band_ordinal_in_node,
            above_barrier,
        },
        placement_node_index: band.placement_node_index,
        band_index,
        content: LayerContent::Painted { band_index },
        may_hold_content: layer.may_hold_content,
        reach: layer.reach,
    }
}

impl LayerStack {
    /// One layer that replays the whole list, which is always correct.
    pub fn single_layer(
        viewport_rect: IntRect,
        whole_list_span: ListSpan,
        why_list_is_one_layer: Option<WhyListIsOneLayer>,
    ) -> Self {
        let spaces = vec![RasterSpace::of_viewport(viewport_rect)];
        let root_node = PlacementNode {
            identity: identity_of_node(&spaces, RasterSpaceIndex::VIEWPORT, &[]),
            raster_space_index: RasterSpaceIndex::VIEWPORT,
            composite_clips: Vec::new(),
            composited_effect: None,
            parent_index: None,
            reach_in_parent_node: Reach::Anywhere,
        };
        let layers = vec![StackLayer {
            identity: LayerIdentity::Painted {
                node: root_node.identity,
                band_ordinal_in_node: 0,
                above_barrier: None,
            },
            placement_node_index: PlacementNodeIndex(0),
            band_index: BandIndex(0),
            content: LayerContent::Painted {
                band_index: BandIndex(0),
            },
            may_hold_content: true,
            reach: viewport_rect,
        }];
        Self {
            layers,
            bands: vec![PlacementNodeBand {
                placement_node_index: PlacementNodeIndex(0),
                band_ordinal_in_node: 0,
                first_record_position: whole_list_span.begin,
                parent_band_holding_this_band_as_barrier: None,
                record_spans: vec![whole_list_span],
                cuts: Vec::new(),
                layer_below_all_cuts: LayerIndex(0),
            }],
            nodes: vec![root_node],
            spaces,
            composited_effects: Vec::new(),
            base_layer: LayerIndex(0),
            why_list_is_one_layer,
            layering_obstacles: LayeringObstacles::default(),
            tiles_hold_content_moving_within_space_by_space_index: vec![true],
            swept_reach_in_parent_space_by_space_index: vec![None],
            index_of_layer_with_identity: HashMap::new(),
            has_layer_at_or_above_base_by_band_index: Vec::new(),
        }
        .with_lookups()
    }

    // The lookups that the methods read, built from what the stack holds. Every index the stack holds must be in range.
    fn with_lookups(mut self) -> Self {
        self.index_of_layer_with_identity = index_of_layer_with_identity(&self.layers);
        self.has_layer_at_or_above_base_by_band_index =
            which_bands_have_layer_at_or_above_base(&self.layers, self.bands.len(), self.base_layer);
        self
    }

    /// The stack of a plan of more than one layer, as the planner and the compositor both work it out. The layers come
    /// in the order of the walk of the bands: a band's layer below all cuts, then per cut the layers of its barrier,
    /// which are a canvas, video or nested context or those of the nested band, and the layer above the cut. The
    /// ordinal of a band in its node counts the bands of the node before it in the walk. The base layer is the first
    /// layer that can hold content where that is the layer below all cuts of a band of a node that covers the viewport,
    /// and the first layer otherwise.
    ///
    /// Every index the plan holds must be in range, each node must come after the node it is in, and the walk must
    /// enter each band of the plan once.
    fn of_layered_plan(
        plan: LayeredPlan,
        spaces: Vec<RasterSpace>,
        tree: &VisualContextTree,
        layering_obstacles: LayeringObstacles,
    ) -> Self {
        let mut nodes: Vec<PlacementNode> = Vec::with_capacity(plan.nodes.len());
        for node in plan.nodes {
            let identity = match (node.composited_effect, node.parent_index) {
                (Some(effect), Some(parent_index)) => {
                    identity_of_composited_node(nodes[parent_index.0].identity, effect)
                }
                _ => identity_of_node(&spaces, node.raster_space_index, &node.composite_clips),
            };
            nodes.push(PlacementNode {
                identity,
                raster_space_index: node.raster_space_index,
                composite_clips: node.composite_clips,
                composited_effect: node.composited_effect,
                parent_index: node.parent_index,
                reach_in_parent_node: node.reach_in_parent_node,
            });
        }
        let mut bands: Vec<PlacementNodeBand> = Vec::with_capacity(plan.bands.len());
        let mut layers: Vec<StackLayer> = Vec::new();
        let mut next_band_ordinal_by_node_index = vec![0; nodes.len()];
        let mut bands_of_plan = plan.bands.into_iter();
        let mut walk: Vec<BandOfWalk> = Vec::new();
        let mut enters_next_band = true;
        loop {
            if enters_next_band && let Some(band) = bands_of_plan.next() {
                let band_index = BandIndex(bands.len());
                let band_ordinal_in_node = &mut next_band_ordinal_by_node_index[band.placement_node_index.0];
                bands.push(PlacementNodeBand {
                    placement_node_index: band.placement_node_index,
                    band_ordinal_in_node: *band_ordinal_in_node,
                    first_record_position: band.first_record_position,
                    parent_band_holding_this_band_as_barrier: walk.last().map(|walked| walked.band_index),
                    record_spans: band.record_spans,
                    cuts: Vec::with_capacity(band.cuts.len()),
                    layer_below_all_cuts: LayerIndex(layers.len()),
                });
                *band_ordinal_in_node += 1;
                let layer = painted_layer_of_band(&nodes, &bands, band_index, None, band.layer_below_all_cuts);
                layers.push(layer);
                walk.push(BandOfWalk {
                    band_index,
                    cuts_to_pass: band.cuts.into_iter(),
                    layer_above_nested_band: PaintedLayerOfPlan::default(),
                });
            }
            enters_next_band = false;
            let Some(walked) = walk.last_mut() else {
                break;
            };
            let band_index = walked.band_index;
            match walked.cuts_to_pass.next() {
                Some(CutOfPlan {
                    barrier:
                        BarrierOfPlan::Media {
                            content,
                            ordinal,
                            record_span,
                            reach,
                        },
                    layer_above_cut,
                }) => {
                    layers.push(StackLayer {
                        identity: LayerIdentity::Media { content, ordinal },
                        placement_node_index: bands[band_index.0].placement_node_index,
                        band_index,
                        content: LayerContent::Media {
                            content,
                            record_span,
                            reach,
                        },
                        may_hold_content: true,
                        reach,
                    });
                    let barrier = Some(BarrierIdentity::Media { content, ordinal });
                    let layer = painted_layer_of_band(&nodes, &bands, band_index, barrier, layer_above_cut);
                    layers.push(layer);
                    bands[band_index.0].cuts.push(BarrierCut {
                        barrier_first_record_position: record_span.begin,
                        cut_reach: Reach::Within(reach),
                        layer_above_cut: LayerIndex(layers.len() - 1),
                    });
                }
                Some(CutOfPlan {
                    barrier: BarrierOfPlan::NestedBand,
                    layer_above_cut,
                }) => {
                    walked.layer_above_nested_band = layer_above_cut;
                    enters_next_band = true;
                }
                None => {
                    walk.pop();
                    let Some(walked_around) = walk.last() else {
                        continue;
                    };
                    let nested_band = &bands[band_index.0];
                    let nested_node = &nodes[nested_band.placement_node_index.0];
                    let cut = BarrierCut {
                        barrier_first_record_position: nested_band.first_record_position,
                        cut_reach: nested_node.reach_in_parent_node,
                        layer_above_cut: LayerIndex(layers.len()),
                    };
                    let barrier = Some(BarrierIdentity::NestedNodeBand {
                        node: nested_node.identity,
                        band_ordinal_in_node: nested_band.band_ordinal_in_node,
                    });
                    let (band_index, layer_above_cut) =
                        (walked_around.band_index, walked_around.layer_above_nested_band);
                    let layer = painted_layer_of_band(&nodes, &bands, band_index, barrier, layer_above_cut);
                    layers.push(layer);
                    bands[band_index.0].cuts.push(cut);
                }
            }
        }
        let base_layer = layers
            .iter()
            .position(|layer| layer.may_hold_content)
            .filter(|&index| {
                matches!(
                    layers[index].identity,
                    LayerIdentity::Painted {
                        above_barrier: None,
                        ..
                    }
                ) && node_covers_viewport(&nodes, &spaces, layers[index].placement_node_index)
            })
            .map_or(LayerIndex(0), LayerIndex);
        LayerStack {
            composited_effects: CompositedEffect::of_nodes(&nodes, tree),
            spaces,
            nodes,
            bands,
            layers,
            base_layer,
            why_list_is_one_layer: None,
            layering_obstacles,
            tiles_hold_content_moving_within_space_by_space_index: plan
                .tiles_hold_content_moving_within_space_by_space_index,
            swept_reach_in_parent_space_by_space_index: plan.swept_reach_in_parent_space_by_space_index,
            index_of_layer_with_identity: HashMap::new(),
            has_layer_at_or_above_base_by_band_index: Vec::new(),
        }
        .with_lookups()
    }

    pub fn replays_whole_list(&self) -> bool {
        self.why_list_is_one_layer.is_some()
    }

    /// Whether tiles of the space hold content that moves within it as nodes that move on their own, or that the
    /// compositor scrolls without the space following them, do, so that the tiles read where those nodes are. Those of
    /// a space that holds none read no value the compositor animates either.
    pub fn space_holds_content_that_moves_within_it(&self, raster_space_index: RasterSpaceIndex) -> bool {
        self.replays_whole_list()
            || self
                .tiles_hold_content_moving_within_space_by_space_index
                .get(raster_space_index.0)
                .copied()
                .unwrap_or(true)
    }

    pub fn layer_with_identity(&self, identity: LayerIdentity) -> Option<(LayerIndex, &StackLayer)> {
        let &layer_index = self.index_of_layer_with_identity.get(&identity)?;
        Some((layer_index, &self.layers[layer_index.0]))
    }

    pub fn band_has_layer_at_or_above_base_layer(&self, band_index: BandIndex) -> bool {
        self.has_layer_at_or_above_base_by_band_index[band_index.0]
    }

    pub fn space_of_layer(&self, layer_index: LayerIndex) -> RasterSpaceIndex {
        self.nodes[self.layers[layer_index.0].placement_node_index.0].raster_space_index
    }

    // An offset past the coordinate limit shows nothing of the space, and is kept within the limit so that a rect of the
    // stack offset by it still fits in an `IntRect`.
    fn composite_offset_of_space(
        &self,
        raster_space_index: RasterSpaceIndex,
        scroll_offsets: &[FloatPoint],
    ) -> IntPoint {
        let (x, y) = space_and_ancestors_below_viewport(&self.spaces, raster_space_index).fold(
            (0i64, 0i64),
            |(x_so_far, y_so_far), space_or_ancestor_index| {
                let space_or_ancestor = &self.spaces[space_or_ancestor_index.0];
                let anchor_translation = space_or_ancestor.anchor_translation_on_whole_device_pixels(
                    device_offset_for_index(scroll_offsets, space_or_ancestor.anchor),
                );
                (
                    x_so_far + i64::from(anchor_translation.x),
                    y_so_far + i64::from(anchor_translation.y),
                )
            },
        );
        let limit = i64::from(COORDINATE_LIMIT_IN_DEVICE_PIXELS);
        IntPoint {
            x: x.clamp(-limit, limit) as i32,
            y: y.clamp(-limit, limit) as i32,
        }
    }

    /// Per raster space, by its index, where the frame composites its rasters, or for a space whose anchor the
    /// compositor animates, those of the space it is in.
    pub fn composite_offsets(&self, scroll_offsets: &[FloatPoint]) -> Vec<IntPoint> {
        (0..self.spaces.len())
            .map(|space_index| self.composite_offset_of_space(RasterSpaceIndex(space_index), scroll_offsets))
            .collect()
    }

    /// Per raster space, by its index, how the frame composites its rasters: at `composite_offsets`, or for a space
    /// whose anchor the compositor animates, through the matrix `draw_matrix_in_frame` gives its anchor in the frame.
    pub fn spaces_to_frame(
        &self,
        composite_offsets: &[IntPoint],
        draw_matrix_in_frame: impl Fn(SpatialNodeIndex) -> FloatMatrix4x4,
    ) -> Vec<SpaceToFrame> {
        self.spaces
            .iter()
            .zip(composite_offsets)
            .map(|(space, &composite_offset)| match space.anchor_kind {
                RasterSpaceAnchorKind::AnimatedTransform { raster_scale } => {
                    let anchor_to_frame = draw_matrix_in_frame(space.anchor);
                    if !anchor_to_frame.is_2d_affine() {
                        return SpaceToFrame::Offset(composite_offset);
                    }
                    let raster_to_anchor = 1.0 / raster_scale;
                    SpaceToFrame::Transform(
                        anchor_to_frame
                            .extract_2d_affine()
                            .scaled(raster_to_anchor, raster_to_anchor),
                    )
                }
                _ => SpaceToFrame::Offset(composite_offset),
            })
            .collect()
    }

    /// The clip a layer's raster is composited within, in the viewport, or none for no clip.
    pub fn composite_clip_of_layer_in_viewport(
        &self,
        layer_index: LayerIndex,
        composite_offsets: &[IntPoint],
    ) -> Option<IntRect> {
        self.composite_clip_of_node_in_viewport(self.layers[layer_index.0].placement_node_index, composite_offsets)
    }

    /// The clip the rasters of a node are composited within, in the viewport, or none for no clip.
    pub fn composite_clip_of_node_in_viewport(
        &self,
        node_index: PlacementNodeIndex,
        composite_offsets: &[IntPoint],
    ) -> Option<IntRect> {
        self.nodes[node_index.0]
            .composite_clips
            .iter()
            .fold(None, |intersection_so_far, clip| {
                let clip_rect_in_viewport = clip.rect.translated(
                    composite_offsets[clip.raster_space_index.0].x,
                    composite_offsets[clip.raster_space_index.0].y,
                );
                Some(
                    intersection_so_far.map_or(clip_rect_in_viewport, |intersection_so_far: IntRect| {
                        intersection_so_far.intersected(clip_rect_in_viewport)
                    }),
                )
            })
    }

    /// The rounded composite clips of a node, in the viewport, which the clip of `composite_clip_of_node_in_viewport`
    /// bounds.
    pub fn rounded_composite_clips_of_node_in_viewport(
        &self,
        node_index: PlacementNodeIndex,
        composite_offsets: &[IntPoint],
    ) -> Vec<(IntRect, CornerRadii)> {
        self.nodes[node_index.0]
            .composite_clips
            .iter()
            .filter(|clip| clip.is_rounded())
            .map(|clip| {
                let offset = composite_offsets[clip.raster_space_index.0];
                (clip.rect.translated(offset.x, offset.y), clip.corner_radii)
            })
            .collect()
    }

    /// Whether the layer is rastered in a space whose anchor the compositor animates, which the frame composites
    /// through the anchor's transform.
    pub fn layer_is_in_animated_space(&self, layer_index: LayerIndex) -> bool {
        matches!(
            self.spaces[self.space_of_layer(layer_index).0].anchor_kind,
            RasterSpaceAnchorKind::AnimatedTransform { .. }
        )
    }

    fn layer_is_composited_within_rounded_clips(&self, layer_index: LayerIndex) -> bool {
        self.nodes[self.layers[layer_index.0].placement_node_index.0]
            .composite_clips
            .iter()
            .any(CompositeClip::is_rounded)
    }

    /// The effects the compositor composites, which rasters of their content leave out.
    pub fn effects_the_compositor_composites(&self) -> Vec<EffectNodeIndex> {
        self.composited_effects
            .iter()
            .map(|composited| composited.effect)
            .collect()
    }

    /// The bands the layer is in, outermost first, whose nodes the compositor composites inside their effects.
    pub fn composited_bands_around_layer(&self, layer_index: LayerIndex) -> Vec<BandIndex> {
        let mut composited_bands = Vec::new();
        let mut band_on_way_out = Some(self.layers[layer_index.0].band_index);
        while let Some(band_index) = band_on_way_out {
            let band = &self.bands[band_index.0];
            if self.nodes[band.placement_node_index.0].composited_effect.is_some() {
                composited_bands.push(band_index);
            }
            band_on_way_out = band.parent_band_holding_this_band_as_barrier;
        }
        composited_bands.reverse();
        composited_bands
    }

    // A layer of a space whose anchor the compositor animates is taken to be anywhere the animation can take it, so
    // that which media the window server shows stays put from one sample to the next.
    fn reach_of_layer_in_viewport(&self, layer_index: LayerIndex, composite_offsets: &[IntPoint]) -> IntRect {
        let node = &self.nodes[self.layers[layer_index.0].placement_node_index.0];
        let swept_reach_of_animated_space = self
            .swept_reach_in_parent_space_by_space_index
            .get(node.raster_space_index.0)
            .copied()
            .flatten();
        let reach_in_raster_space = match self.composited_bands_around_layer(layer_index).first() {
            _ if let Some(swept_reach) = swept_reach_of_animated_space => swept_reach,
            Some(&outermost_composited_band) => {
                let outermost_composited_node =
                    &self.nodes[self.bands[outermost_composited_band.0].placement_node_index.0];
                match outermost_composited_node.reach_in_parent_node {
                    Reach::Nothing => return IntRect::default(),
                    Reach::Within(draw_and_read_reach_of_outermost_composited_effect) => {
                        draw_and_read_reach_of_outermost_composited_effect
                    }
                    Reach::Anywhere => return self.spaces[RasterSpaceIndex::VIEWPORT.0].extent,
                }
            }
            None => self.layers[layer_index.0].reach,
        };
        let composite_offset = composite_offsets[node.raster_space_index.0];
        let reach_in_viewport = reach_in_raster_space.translated(composite_offset.x, composite_offset.y);
        match self.composite_clip_of_layer_in_viewport(layer_index, composite_offsets) {
            Some(clip) => reach_in_viewport.intersected(clip),
            None => reach_in_viewport,
        }
    }

    /// Which layers the window server shows above the frame, rather than the frame holding them, of those above the
    /// base layer that can hold content. The frame holds every layer up to the topmost one it must hold: one inside an
    /// effect the compositor composites, or a canvas, video or nested context the window server cannot show by itself,
    /// as `window_server_can_show` says. Below that, the window server still shows a canvas or video it can show where
    /// no layer above it that the frame holds can show over it. A painted layer is the window server's to show only to
    /// stay above a canvas or video that it shows, so the frame also holds the painted layers below the lowest of
    /// those, and every layer where the window server shows no canvas or video.
    pub fn layers_the_window_server_shows(
        &self,
        composite_offsets: &[IntPoint],
        window_server_can_show: impl Fn(LayerIndex) -> bool,
    ) -> Vec<bool> {
        let mut shown_by_window_server = vec![false; self.layers.len()];
        if self.replays_whole_list() {
            return shown_by_window_server;
        }
        let layers_above_base_that_may_hold_content: Vec<LayerIndex> = (self.base_layer.0 + 1..self.layers.len())
            .map(LayerIndex)
            .filter(|&layer| self.layers[layer.0].may_hold_content)
            .collect();
        let is_media = |layer: LayerIndex| matches!(self.layers[layer.0].content, LayerContent::Media { .. });
        let frame_must_hold = |layer: LayerIndex| {
            !self.composited_bands_around_layer(layer).is_empty()
                || self.layer_is_composited_within_rounded_clips(layer)
                || self.layer_is_in_animated_space(layer)
                || (is_media(layer) && !window_server_can_show(layer))
        };
        let position_of_topmost_layer_frame_must_hold = layers_above_base_that_may_hold_content
            .iter()
            .rposition(|&layer| frame_must_hold(layer));
        let count_up_to_topmost_layer_frame_must_hold =
            position_of_topmost_layer_frame_must_hold.map_or(0, |position| position + 1);
        for &layer in &layers_above_base_that_may_hold_content[count_up_to_topmost_layer_frame_must_hold..] {
            shown_by_window_server[layer.0] = true;
        }
        let mut reach_in_viewport_of_held_layers_above = IntRect::default();
        for &layer in layers_above_base_that_may_hold_content[..count_up_to_topmost_layer_frame_must_hold]
            .iter()
            .rev()
        {
            let reach_in_viewport = self.reach_of_layer_in_viewport(layer, composite_offsets);
            if is_media(layer)
                && !frame_must_hold(layer)
                && reach_in_viewport_of_held_layers_above
                    .intersected(reach_in_viewport)
                    .is_empty()
            {
                shown_by_window_server[layer.0] = true;
                continue;
            }
            reach_in_viewport_of_held_layers_above = reach_in_viewport_of_held_layers_above.united(reach_in_viewport);
        }
        let lowest_media_the_window_server_shows = layers_above_base_that_may_hold_content
            .iter()
            .copied()
            .find(|&layer| is_media(layer) && shown_by_window_server[layer.0]);
        for &layer in &layers_above_base_that_may_hold_content {
            if lowest_media_the_window_server_shows.is_none_or(|lowest_media| layer < lowest_media) {
                shown_by_window_server[layer.0] = false;
            }
        }
        shown_by_window_server
    }

    /// Works out where the window server can show the content of each canvas and video layer under `scroll_offsets`,
    /// and which layers it shows where it can show every canvas and video it has a placement for.
    pub fn window_server_presentation_of_media(
        &self,
        list: &DisplayListStorage,
        tree: &VisualContextTree,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
    ) -> WindowServerPresentationOfMedia {
        let placements = self.window_server_placements_of_media(list, tree, scroll_offsets, viewport_rect);
        let composite_offsets = self.composite_offsets(scroll_offsets);
        let shown_by_window_server_by_layer_index = self.layers_the_window_server_shows(&composite_offsets, |layer| {
            matches!(placements.of_layer(layer), Some(Ok(_)))
        });
        WindowServerPresentationOfMedia {
            placements,
            shown_by_window_server_by_layer_index,
        }
    }

    /// Works out where the window server could show each canvas and video of the stack under the frame's values.
    pub fn window_server_placements_of_media(
        &self,
        list: &DisplayListStorage,
        tree: &VisualContextTree,
        scroll_offsets: &[FloatPoint],
        viewport_rect: IntRect,
    ) -> WindowServerPlacementsOfMedia {
        let window_server_may_show_content =
            |content: &LiveContent| !matches!(content, LiveContent::CompositedContext(_));
        let has_media_the_window_server_may_show = self.layers.iter().any(|layer| {
            matches!(&layer.content, LayerContent::Media { content, .. } if window_server_may_show_content(content))
        });
        if !has_media_the_window_server_may_show {
            return WindowServerPlacementsOfMedia::default();
        }
        let world_transforms = WorldTransforms::new(tree, scroll_offsets, Some(FloatMatrix4x4::identity()));
        let placement_by_layer_index = self
            .layers
            .iter()
            .map(|layer| {
                let LayerContent::Media {
                    content, record_span, ..
                } = &layer.content
                else {
                    return None;
                };
                if !window_server_may_show_content(content) {
                    return None;
                }
                let media_record_position = record_span.begin;
                let runs_of_chunk = list.chunk(media_record_position.chunk_index).runs();
                let run_holding_media = &runs_of_chunk
                    [runs_of_chunk.partition_point(|run| run.offset + run.size <= media_record_position.offset)];
                let (header, payload) = list.record_at(media_record_position);
                Some(crate::display_list::window_server_placement::window_server_placement(
                    tree,
                    &world_transforms,
                    viewport_rect,
                    run_holding_media,
                    &header,
                    payload,
                ))
            })
            .collect();
        WindowServerPlacementsOfMedia {
            placement_by_layer_index,
        }
    }

    fn name_of_space(&self, raster_space_index: RasterSpaceIndex, node_namer: &dyn VisualContextNodeNamer) -> String {
        let raster_space = &self.spaces[raster_space_index.0];
        let anchor_kind_name = match raster_space.anchor_kind {
            RasterSpaceAnchorKind::Viewport => return "viewport".into(),
            RasterSpaceAnchorKind::ViewportScroller => return "document".into(),
            RasterSpaceAnchorKind::Scroller => "scroller",
            RasterSpaceAnchorKind::StickyBox { .. } => "sticky",
            RasterSpaceAnchorKind::AnimatedTransform { .. } => "animated",
        };
        format!(
            "{anchor_kind_name}@{} in {}",
            node_namer.name_of_spatial_node(raster_space.anchor),
            self.name_of_space(raster_space.parent_index, node_namer)
        )
    }

    fn name_of_node(&self, node_index: PlacementNodeIndex, node_namer: &dyn VisualContextNodeNamer) -> String {
        let mut name = self.name_of_space(self.nodes[node_index.0].raster_space_index, node_namer);
        let mut node_on_way_out = Some(node_index);
        while let Some(index) = node_on_way_out {
            if let Some(effect) = self.nodes[index.0].composited_effect {
                name.push_str(&format!(" inside {}", node_namer.name_of_effect_node(effect)));
            }
            node_on_way_out = self.nodes[index.0].parent_index;
        }
        name
    }

    fn name_of_barrier(&self, barrier: &BarrierIdentity, node_namer: &dyn VisualContextNodeNamer) -> String {
        match barrier {
            BarrierIdentity::Media { content, ordinal } => {
                let media_layer_identity = LayerIdentity::Media {
                    content: *content,
                    ordinal: *ordinal,
                };
                let (media_layer_index, _) = self
                    .layer_with_identity(media_layer_identity)
                    .expect("a media barrier is a layer of the stack");
                format!("L{}", media_layer_index.0)
            }
            BarrierIdentity::NestedNodeBand {
                node,
                band_ordinal_in_node,
            } => {
                let nested_node_index = self
                    .nodes
                    .iter()
                    .position(|candidate| candidate.identity == *node)
                    .expect("a band barrier names a node of the stack");
                format!(
                    "band {band_ordinal_in_node} of {}",
                    self.name_of_node(PlacementNodeIndex(nested_node_index), node_namer)
                )
            }
        }
    }

    /// What the dump calls a layer: its index, what it holds, the space it is rastered in and the effects the
    /// compositor composites it inside, and what it sits above.
    pub fn name_of_layer(&self, layer_index: LayerIndex, node_namer: &dyn VisualContextNodeNamer) -> String {
        if self.why_list_is_one_layer.is_some() {
            return "[L0] single layer replaying the whole list".into();
        }
        let index = layer_index.0;
        let layer = &self.layers[index];
        let node_name = self.name_of_node(layer.placement_node_index, node_namer);
        match (&layer.identity, &layer.content) {
            (
                LayerIdentity::Painted {
                    band_ordinal_in_node,
                    above_barrier,
                    ..
                },
                _,
            ) => {
                let kind = if layer_index == self.base_layer {
                    "base"
                } else {
                    "painted"
                };
                let mut name = format!("[L{index}] {kind} space={node_name}");
                if *band_ordinal_in_node != 0 {
                    name.push_str(&format!(" band={band_ordinal_in_node}"));
                }
                if let Some(barrier) = above_barrier {
                    name.push_str(&format!(" above {}", self.name_of_barrier(barrier, node_namer)));
                }
                name
            }
            (LayerIdentity::Media { content, ordinal }, _) => {
                let reach = layer.reach;
                format!(
                    "[L{index}] {} #{ordinal} space={node_name} region=[{},{} {}x{}]",
                    media_name(*content),
                    reach.x,
                    reach.y,
                    reach.width,
                    reach.height
                )
            }
        }
    }
}

/// What of a tree and its list keeps the tiles of its layers from replaying only the records that reach them, or keeps
/// the list from being layered, at some sample of the animations of the tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayeringObstacles {
    pub sorts_planes_by_depth: bool,
    pub mask_declared_under_effects_its_mask_is_not_under: bool,
}

impl LayeringObstacles {
    pub fn of(list: &DisplayListStorage, tree: &VisualContextTree) -> Self {
        Self {
            sorts_planes_by_depth: tree.may_sort_planes_by_depth(),
            mask_declared_under_effects_its_mask_is_not_under: !mask_declarations_are_on_effect_chains_of_their_masks(
                list, tree,
            ),
        }
    }

    /// Whether every tile replays every record of its band, as which records can show in a tile cannot be told.
    pub fn every_tile_replays_every_record(&self) -> bool {
        self.mask_declared_under_effects_its_mask_is_not_under
    }
}

fn effect_is_or_is_inside(tree: &VisualContextTree, mut effect: EffectNodeIndex, ancestor: EffectNodeIndex) -> bool {
    while !effect.is_none() {
        if effect == ancestor {
            return true;
        }
        effect = tree.effect_nodes[effect.0 as usize].parent;
    }
    false
}

// A tile replays the declaration of a mask's content along with what the mask holds, and so opens the effects the
// declaration is under there, which draw nothing only when they are the mask's own or those around it. A list from
// another process can name a mask that the tree does not hold. No effect is inside such a mask.
fn mask_declarations_are_on_effect_chains_of_their_masks(list: &DisplayListStorage, tree: &VisualContextTree) -> bool {
    list.summary().mask_declarations.iter().all(|declaration| {
        let mask_effect = declaration.mask_effect;
        let declaration_effect = list.run(declaration.record.run_index as usize).context.effect;
        let tree_holds_mask_effect = (mask_effect.0 as usize) < tree.effect_nodes.len();
        declaration_effect.is_none()
            || effect_is_or_is_inside(tree, declaration_effect, mask_effect)
            || (tree_holds_mask_effect && effect_is_or_is_inside(tree, mask_effect, declaration_effect))
    })
}

/// Where a backdrop filter reads, in the raster coordinates of the space: its region, and how far its filter reaches
/// past it.
pub fn backdrop_filter_read_reach(
    tree: &VisualContextTree,
    geometry: &RasterSpaceGeometry,
    effect: EffectNodeIndex,
) -> Reach<IntRect> {
    let effect_node = &tree.effect_nodes[effect.0 as usize];
    let EffectNodeData::Effects(effects) = &effect_node.data else {
        return Reach::Anywhere;
    };
    let Some(backdrop) = &effects.backdrop_filter else {
        return Reach::Anywhere;
    };
    let effect_to_device = geometry.world.draw_matrix(effect_node.spatial);
    let Some(read_reach_in_device_pixels) =
        crate::display_list::tile_keys::filter_read_reach_in_device_pixels(&backdrop.filter, &effect_to_device)
    else {
        return Reach::Anywhere;
    };
    let backdrop_region = geometry
        .world
        .map_rect_to_root(effect_node.spatial, backdrop.region.to_float());
    if !is_finite_rect(backdrop_region) {
        return Reach::Anywhere;
    }
    Reach::Within(enclosing_int_rect_within_extent_limit(backdrop_region).inflated_edges(
        read_reach_in_device_pixels,
        read_reach_in_device_pixels,
        read_reach_in_device_pixels,
        read_reach_in_device_pixels,
    ))
}

fn anchors_of_layer_plans(tree: &VisualContextTree) -> Vec<SpatialNodeIndex> {
    raster_space_anchors(tree, &spatial_nodes_that_move_on_their_own(tree))
}
