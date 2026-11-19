/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! A plan of the layers of a list as it goes from WebContent to the compositor: what of its stack the compositor cannot
//! work out of the list and the tree, the notes the overlay of layers shows, and the checks that a plan from another
//! process passes before anything reads it.

use super::{
    BandIndex, BarrierCut, BarrierIdentity, COORDINATE_LIMIT_IN_DEVICE_PIXELS, CompositedEffect, LayerContent,
    LayerIdentity, LayerIndex, LayerStack, LayeringObstacles, MAXIMUM_MEDIA_LAYER_COUNT, PlacementNode,
    PlacementNodeBand, PlacementNodeIdentity, PlacementNodeIndex, StackLayer, WhyCompositorCannotCompositeEffect,
    WhyListIsOneLayer, anchors_of_layer_plans,
};
use crate::display_list::commands::{
    CanvasId, CompositorContextId, EffectNodeIndex, ImageFrameResourceId, VideoSinkHandle,
};
use crate::display_list::placement::{
    CompositeClip, MAXIMUM_RASTER_SPACE_COUNT, RasterSpaceAnchorKind, RasterSpaceIndex, RasterSpaces,
};
use crate::display_list::reach::Reach;
use crate::display_list::storage::{ChunkIndexInList, DisplayListStorage, ListPosition, ListSpan};
use crate::display_list::summary::{LiveContent, live_content_of};
use crate::visual_context::VisualContextTree;
use libgfx_rust::IntRect;
use libipc_rust::wire::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};
use std::collections::{HashMap, HashSet};

// Caps on a received stack, which bound what decoding allocates. The values per space, and the composite clips of a node,
// which are one per space, are capped at the most spaces a plan makes. The other caps are far more than a plan holds: it
// holds nodes, bands, layers and spans as many as the runs of a list ask for.
const MAXIMUM_NODE_COUNT_OF_RECEIVED_STACK: usize = 1 << 16;
const MAXIMUM_BAND_COUNT_OF_RECEIVED_STACK: usize = 1 << 16;
const MAXIMUM_LAYER_COUNT_OF_RECEIVED_STACK: usize = 1 << 17;
const MAXIMUM_CUT_COUNT_OF_RECEIVED_BAND: usize = 1 << 16;
const MAXIMUM_SPAN_COUNT_OF_RECEIVED_BAND: usize = 1 << 20;
// The most note lines a plan holds, and the most bytes a line holds. A plan holds only the first lines, each cut to that
// length, so that no plan exceeds the caps.
const MAXIMUM_NOTE_LINE_COUNT_OF_PLAN: usize = 1 << 10;
const MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN: usize = 1 << 10;

/// The note lines a plan holds of `lines`: as many of the first lines as a plan holds, each cut at a character boundary
/// to the length a plan holds.
pub(super) fn note_lines_a_plan_holds(lines: impl Iterator<Item = String>) -> Vec<String> {
    lines
        .take(MAXIMUM_NOTE_LINE_COUNT_OF_PLAN)
        .map(|mut line| {
            line.truncate(line.floor_char_boundary(MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN));
            line
        })
        .collect()
}

/// A stack decoded from another process: why it is one layer that replays the whole list, or what of a stack of more
/// layers the compositor cannot work out of the list and the tree. Nothing reads it until `LayerStack::validated`
/// checks it and works out the rest.
#[derive(Debug)]
pub struct UnvalidatedLayerStack(StackOnWire);

#[derive(Debug)]
enum StackOnWire {
    OneLayer(WhyListIsOneLayer),
    Layered {
        nodes: Vec<PlacementNode>,
        bands: Vec<PlacementNodeBand>,
        layers: Vec<StackLayer>,
        base_layer: LayerIndex,
        tiles_hold_content_moving_within_space_by_space_index: Vec<bool>,
        swept_reach_in_parent_space_by_space_index: Vec<Option<IntRect>>,
    },
}

/// A plan of the layers of a list as WebContent sends it to the compositor: the stack, the display list it was planned
/// for, the viewport, in device pixels, it was planned within, and what the overlay of layers tells of the stack past
/// its layers.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerPlan {
    pub display_list_id: u64,
    pub viewport_rect: IntRect,
    pub stack: LayerStack,
    pub note_lines: Vec<String>,
}

/// A plan of layers decoded from another process, whose stack nothing reads until it is checked.
#[derive(Debug)]
pub struct UnvalidatedLayerPlan {
    pub display_list_id: u64,
    pub viewport_rect: IntRect,
    pub stack: UnvalidatedLayerStack,
    note_lines: Vec<String>,
}

impl UnvalidatedLayerPlan {
    /// The plan, once its stack passes `LayerStack::validated` for `list` under `tree` within the viewport the plan
    /// names. The receiver makes sure that `list` is the list that `display_list_id` names. The note lines go unchecked:
    /// decoding bounds their count and length, and only the overlay of layers shows them.
    pub fn validated(self, list: &DisplayListStorage, tree: &VisualContextTree) -> Result<LayerPlan, &'static str> {
        Ok(LayerPlan {
            display_list_id: self.display_list_id,
            viewport_rect: self.viewport_rect,
            stack: LayerStack::validated(self.stack, list, tree, self.viewport_rect)?,
            note_lines: self.note_lines,
        })
    }
}

macro_rules! impl_codec_of_index_going_as_u32 {
    ($($type:ident),*) => {
        $(
            impl Encode for $type {
                fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
                    encoder.encode(&u32::try_from(self.0).map_err(|_| EncodeError::SizeTooLarge(self.0))?)
                }
            }

            impl Decode for $type {
                const MIN_ENCODED_SIZE: usize = size_of::<u32>();

                fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                    Ok(Self(decoder.decode::<u32>()? as usize))
                }
            }
        )*
    };
}

impl_codec_of_index_going_as_u32!(RasterSpaceIndex, PlacementNodeIndex, BandIndex, LayerIndex);

impl Encode for PlacementNodeIdentity {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.0)
    }
}

impl Decode for PlacementNodeIdentity {
    const MIN_ENCODED_SIZE: usize = size_of::<u64>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self(decoder.decode()?))
    }
}

impl Encode for ListPosition {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.chunk_index.0)?;
        encoder.encode(&self.offset)
    }
}

impl Decode for ListPosition {
    const MIN_ENCODED_SIZE: usize = 2 * size_of::<u32>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self::new(ChunkIndexInList(decoder.decode()?), decoder.decode()?))
    }
}

impl Encode for ListSpan {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.begin)?;
        encoder.encode(&self.end)
    }
}

impl Decode for ListSpan {
    const MIN_ENCODED_SIZE: usize = 2 * ListPosition::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            begin: decoder.decode()?,
            end: decoder.decode()?,
        })
    }
}

const REACH_KIND_NOTHING: u8 = 0;
const REACH_KIND_WITHIN: u8 = 1;
const REACH_KIND_ANYWHERE: u8 = 2;

impl Encode for Reach<IntRect> {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        match self {
            Reach::Nothing => encoder.encode(&REACH_KIND_NOTHING),
            Reach::Within(rect) => {
                encoder.encode(&REACH_KIND_WITHIN)?;
                encoder.encode(rect)
            }
            Reach::Anywhere => encoder.encode(&REACH_KIND_ANYWHERE),
        }
    }
}

impl Decode for Reach<IntRect> {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            REACH_KIND_NOTHING => Reach::Nothing,
            REACH_KIND_WITHIN => Reach::Within(decoder.decode()?),
            REACH_KIND_ANYWHERE => Reach::Anywhere,
            _ => return Err(DecodeError::InvalidValue("Reach")),
        })
    }
}

const LIVE_CONTENT_KIND_CANVAS: u8 = 0;
const LIVE_CONTENT_KIND_VIDEO_FRAME: u8 = 1;
const LIVE_CONTENT_KIND_IMAGE_FRAME: u8 = 2;
const LIVE_CONTENT_KIND_COMPOSITED_CONTEXT: u8 = 3;

impl Encode for LiveContent {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        let (kind, id) = match self {
            LiveContent::Canvas(id) => (LIVE_CONTENT_KIND_CANVAS, id.0),
            LiveContent::VideoFrame(id) => (LIVE_CONTENT_KIND_VIDEO_FRAME, id.0),
            LiveContent::ImageFrame(id) => (LIVE_CONTENT_KIND_IMAGE_FRAME, id.0),
            LiveContent::CompositedContext(id) => (LIVE_CONTENT_KIND_COMPOSITED_CONTEXT, id.0),
        };
        encoder.encode(&kind)?;
        encoder.encode(&id)
    }
}

impl Decode for LiveContent {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>() + size_of::<u64>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let kind = decoder.decode::<u8>()?;
        let id = decoder.decode::<u64>()?;
        Ok(match kind {
            LIVE_CONTENT_KIND_CANVAS => LiveContent::Canvas(CanvasId(id)),
            LIVE_CONTENT_KIND_VIDEO_FRAME => LiveContent::VideoFrame(VideoSinkHandle(id)),
            LIVE_CONTENT_KIND_IMAGE_FRAME => LiveContent::ImageFrame(ImageFrameResourceId(id)),
            LIVE_CONTENT_KIND_COMPOSITED_CONTEXT => LiveContent::CompositedContext(CompositorContextId(id)),
            _ => return Err(DecodeError::InvalidValue("LiveContent")),
        })
    }
}

impl Encode for CompositeClip {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.raster_space_index)?;
        encoder.encode(&self.rect)?;
        encoder.encode(&self.corner_radii)
    }
}

impl Decode for CompositeClip {
    const MIN_ENCODED_SIZE: usize =
        RasterSpaceIndex::MIN_ENCODED_SIZE + IntRect::MIN_ENCODED_SIZE + libgfx_rust::CornerRadii::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            raster_space_index: decoder.decode()?,
            rect: decoder.decode()?,
            corner_radii: decoder.decode()?,
        })
    }
}

impl Encode for PlacementNode {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.identity)?;
        encoder.encode(&self.raster_space_index)?;
        encoder.encode(&self.composite_clips)?;
        encoder.encode(&self.composited_effect)?;
        encoder.encode(&self.parent_index)?;
        encoder.encode(&self.reach_in_parent_node)
    }
}

impl Decode for PlacementNode {
    const MIN_ENCODED_SIZE: usize = PlacementNodeIdentity::MIN_ENCODED_SIZE
        + RasterSpaceIndex::MIN_ENCODED_SIZE
        + Vec::<CompositeClip>::MIN_ENCODED_SIZE
        + Option::<EffectNodeIndex>::MIN_ENCODED_SIZE
        + Option::<PlacementNodeIndex>::MIN_ENCODED_SIZE
        + Reach::<IntRect>::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            identity: decoder.decode()?,
            raster_space_index: decoder.decode()?,
            composite_clips: decoder.decode_with_max_size(MAXIMUM_RASTER_SPACE_COUNT)?,
            composited_effect: decoder.decode()?,
            parent_index: decoder.decode()?,
            reach_in_parent_node: decoder.decode()?,
        })
    }
}

impl Encode for BarrierCut {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.barrier_first_record_position)?;
        encoder.encode(&self.cut_reach)?;
        encoder.encode(&self.layer_above_cut)
    }
}

impl Decode for BarrierCut {
    const MIN_ENCODED_SIZE: usize =
        ListPosition::MIN_ENCODED_SIZE + Reach::<IntRect>::MIN_ENCODED_SIZE + LayerIndex::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            barrier_first_record_position: decoder.decode()?,
            cut_reach: decoder.decode()?,
            layer_above_cut: decoder.decode()?,
        })
    }
}

impl Encode for PlacementNodeBand {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.placement_node_index)?;
        encoder.encode(&self.band_ordinal_in_node)?;
        encoder.encode(&self.first_record_position)?;
        encoder.encode(&self.parent_band_holding_this_band_as_barrier)?;
        encoder.encode(&self.record_spans)?;
        encoder.encode(&self.cuts)?;
        encoder.encode(&self.layer_below_all_cuts)
    }
}

impl Decode for PlacementNodeBand {
    const MIN_ENCODED_SIZE: usize = PlacementNodeIndex::MIN_ENCODED_SIZE
        + size_of::<u32>()
        + ListPosition::MIN_ENCODED_SIZE
        + Option::<BandIndex>::MIN_ENCODED_SIZE
        + Vec::<ListSpan>::MIN_ENCODED_SIZE
        + Vec::<BarrierCut>::MIN_ENCODED_SIZE
        + LayerIndex::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            placement_node_index: decoder.decode()?,
            band_ordinal_in_node: decoder.decode()?,
            first_record_position: decoder.decode()?,
            parent_band_holding_this_band_as_barrier: decoder.decode()?,
            record_spans: decoder.decode_with_max_size(MAXIMUM_SPAN_COUNT_OF_RECEIVED_BAND)?,
            cuts: decoder.decode_with_max_size(MAXIMUM_CUT_COUNT_OF_RECEIVED_BAND)?,
            layer_below_all_cuts: decoder.decode()?,
        })
    }
}

const BARRIER_KIND_MEDIA: u8 = 0;
const BARRIER_KIND_NESTED_NODE_BAND: u8 = 1;

impl Encode for BarrierIdentity {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        match self {
            BarrierIdentity::Media { content, ordinal } => {
                encoder.encode(&BARRIER_KIND_MEDIA)?;
                encoder.encode(content)?;
                encoder.encode(ordinal)
            }
            BarrierIdentity::NestedNodeBand {
                node,
                band_ordinal_in_node,
            } => {
                encoder.encode(&BARRIER_KIND_NESTED_NODE_BAND)?;
                encoder.encode(node)?;
                encoder.encode(band_ordinal_in_node)
            }
        }
    }
}

impl Decode for BarrierIdentity {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>() + PlacementNodeIdentity::MIN_ENCODED_SIZE + size_of::<u32>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            BARRIER_KIND_MEDIA => BarrierIdentity::Media {
                content: decoder.decode()?,
                ordinal: decoder.decode()?,
            },
            BARRIER_KIND_NESTED_NODE_BAND => BarrierIdentity::NestedNodeBand {
                node: decoder.decode()?,
                band_ordinal_in_node: decoder.decode()?,
            },
            _ => return Err(DecodeError::InvalidValue("BarrierIdentity")),
        })
    }
}

const LAYER_KIND_PAINTED: u8 = 0;
const LAYER_KIND_MEDIA: u8 = 1;

impl Encode for LayerIdentity {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        match self {
            LayerIdentity::Painted {
                node,
                band_ordinal_in_node,
                above_barrier,
            } => {
                encoder.encode(&LAYER_KIND_PAINTED)?;
                encoder.encode(node)?;
                encoder.encode(band_ordinal_in_node)?;
                encoder.encode(above_barrier)
            }
            LayerIdentity::Media { content, ordinal } => {
                encoder.encode(&LAYER_KIND_MEDIA)?;
                encoder.encode(content)?;
                encoder.encode(ordinal)
            }
        }
    }
}

impl Decode for LayerIdentity {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>() + PlacementNodeIdentity::MIN_ENCODED_SIZE + size_of::<u32>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            LAYER_KIND_PAINTED => LayerIdentity::Painted {
                node: decoder.decode()?,
                band_ordinal_in_node: decoder.decode()?,
                above_barrier: decoder.decode()?,
            },
            LAYER_KIND_MEDIA => LayerIdentity::Media {
                content: decoder.decode()?,
                ordinal: decoder.decode()?,
            },
            _ => return Err(DecodeError::InvalidValue("LayerIdentity")),
        })
    }
}

impl Encode for LayerContent {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        match self {
            LayerContent::Painted { band_index } => {
                encoder.encode(&LAYER_KIND_PAINTED)?;
                encoder.encode(band_index)
            }
            LayerContent::Media {
                content,
                record_span,
                reach,
            } => {
                encoder.encode(&LAYER_KIND_MEDIA)?;
                encoder.encode(content)?;
                encoder.encode(record_span)?;
                encoder.encode(reach)
            }
        }
    }
}

impl Decode for LayerContent {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>() + BandIndex::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            LAYER_KIND_PAINTED => LayerContent::Painted {
                band_index: decoder.decode()?,
            },
            LAYER_KIND_MEDIA => LayerContent::Media {
                content: decoder.decode()?,
                record_span: decoder.decode()?,
                reach: decoder.decode()?,
            },
            _ => return Err(DecodeError::InvalidValue("LayerContent")),
        })
    }
}

impl Encode for StackLayer {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.identity)?;
        encoder.encode(&self.placement_node_index)?;
        encoder.encode(&self.band_index)?;
        encoder.encode(&self.content)?;
        encoder.encode(&self.may_hold_content)?;
        encoder.encode(&self.reach)
    }
}

impl Decode for StackLayer {
    const MIN_ENCODED_SIZE: usize = LayerIdentity::MIN_ENCODED_SIZE
        + PlacementNodeIndex::MIN_ENCODED_SIZE
        + BandIndex::MIN_ENCODED_SIZE
        + LayerContent::MIN_ENCODED_SIZE
        + bool::MIN_ENCODED_SIZE
        + IntRect::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            identity: decoder.decode()?,
            placement_node_index: decoder.decode()?,
            band_index: decoder.decode()?,
            content: decoder.decode()?,
            may_hold_content: decoder.decode()?,
            reach: decoder.decode()?,
        })
    }
}

impl Encode for WhyCompositorCannotCompositeEffect {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        let kind: u8 = match self {
            WhyCompositorCannotCompositeEffect::FilterMovesWhatItFilters => 0,
            WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed => 1,
        };
        encoder.encode(&kind)
    }
}

impl Decode for WhyCompositorCannotCompositeEffect {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            0 => WhyCompositorCannotCompositeEffect::FilterMovesWhatItFilters,
            1 => WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed,
            _ => return Err(DecodeError::InvalidValue("WhyCompositorCannotCompositeEffect")),
        })
    }
}

impl Encode for WhyListIsOneLayer {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        let kind: u8 = match self {
            WhyListIsOneLayer::EmptyViewport => 0,
            WhyListIsOneLayer::EmptyDisplayList => 1,
            WhyListIsOneLayer::SortingContext3D => 2,
            WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup => 3,
            WhyListIsOneLayer::MaskDeclaredUnderEffectsItsMaskIsNotUnder => 4,
            WhyListIsOneLayer::RecordsBlendWithWhatOtherLayersHoldBelowThem => 5,
            WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited(_) => 6,
            WhyListIsOneLayer::FrameIsNotDrawnAsPlanned => 7,
        };
        encoder.encode(&kind)?;
        if let WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited(why) = self {
            encoder.encode(why)?;
        }
        Ok(())
    }
}

impl Decode for WhyListIsOneLayer {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>();

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match decoder.decode::<u8>()? {
            0 => WhyListIsOneLayer::EmptyViewport,
            1 => WhyListIsOneLayer::EmptyDisplayList,
            2 => WhyListIsOneLayer::SortingContext3D,
            3 => WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup,
            4 => WhyListIsOneLayer::MaskDeclaredUnderEffectsItsMaskIsNotUnder,
            5 => WhyListIsOneLayer::RecordsBlendWithWhatOtherLayersHoldBelowThem,
            6 => WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited(decoder.decode()?),
            7 => WhyListIsOneLayer::FrameIsNotDrawnAsPlanned,
            _ => return Err(DecodeError::InvalidValue("WhyListIsOneLayer")),
        })
    }
}

const PLAN_KIND_ONE_LAYER: u8 = 0;
const PLAN_KIND_LAYERED: u8 = 1;

// A stack of one layer that replays the whole list goes as why it is one layer. A stack of more layers goes without
// what the compositor works out of the list and the tree: its spaces, its composited effects and its layering
// obstacles.
impl Encode for LayerStack {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        if let Some(why_list_is_one_layer) = self.why_list_is_one_layer {
            encoder.encode(&PLAN_KIND_ONE_LAYER)?;
            return encoder.encode(&why_list_is_one_layer);
        }
        encoder.encode(&PLAN_KIND_LAYERED)?;
        encoder.encode(&self.nodes)?;
        encoder.encode(&self.bands)?;
        encoder.encode(&self.layers)?;
        encoder.encode(&self.base_layer)?;
        encoder.encode(&self.tiles_hold_content_moving_within_space_by_space_index)?;
        encoder.encode(&self.swept_reach_in_parent_space_by_space_index)
    }
}

impl Decode for UnvalidatedLayerStack {
    const MIN_ENCODED_SIZE: usize = size_of::<u8>() + WhyListIsOneLayer::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self(match decoder.decode::<u8>()? {
            PLAN_KIND_ONE_LAYER => StackOnWire::OneLayer(decoder.decode()?),
            PLAN_KIND_LAYERED => StackOnWire::Layered {
                nodes: decoder.decode_with_max_size(MAXIMUM_NODE_COUNT_OF_RECEIVED_STACK)?,
                bands: decoder.decode_with_max_size(MAXIMUM_BAND_COUNT_OF_RECEIVED_STACK)?,
                layers: decoder.decode_with_max_size(MAXIMUM_LAYER_COUNT_OF_RECEIVED_STACK)?,
                base_layer: decoder.decode()?,
                tiles_hold_content_moving_within_space_by_space_index: decoder
                    .decode_with_max_size(MAXIMUM_RASTER_SPACE_COUNT)?,
                swept_reach_in_parent_space_by_space_index: decoder.decode_with_max_size(MAXIMUM_RASTER_SPACE_COUNT)?,
            },
            _ => return Err(DecodeError::InvalidValue("UnvalidatedLayerStack")),
        }))
    }
}

fn decode_note_lines(decoder: &mut Decoder<'_>) -> Result<Vec<String>, DecodeError> {
    let count = decoder.decode_size()?;
    if count > MAXIMUM_NOTE_LINE_COUNT_OF_PLAN || count > decoder.remaining_bytes() / String::MIN_ENCODED_SIZE {
        return Err(DecodeError::SizeTooLarge(count));
    }
    (0..count)
        .map(|_| decoder.decode_with_max_size(MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN))
        .collect()
}

impl Encode for LayerPlan {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.display_list_id)?;
        encoder.encode(&self.viewport_rect)?;
        encoder.encode(&self.stack)?;
        encoder.encode(&self.note_lines)
    }
}

impl Decode for UnvalidatedLayerPlan {
    const MIN_ENCODED_SIZE: usize = size_of::<u64>()
        + IntRect::MIN_ENCODED_SIZE
        + UnvalidatedLayerStack::MIN_ENCODED_SIZE
        + Vec::<String>::MIN_ENCODED_SIZE;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            display_list_id: decoder.decode()?,
            viewport_rect: decoder.decode()?,
            stack: decoder.decode()?,
            note_lines: decode_note_lines(decoder)?,
        })
    }
}

fn rect_is_within_coordinate_limit(rect: IntRect) -> bool {
    let limit = i64::from(COORDINATE_LIMIT_IN_DEVICE_PIXELS);
    let (x, y, width, height) = (
        i64::from(rect.x),
        i64::from(rect.y),
        i64::from(rect.width),
        i64::from(rect.height),
    );
    width >= 0 && height >= 0 && x >= -limit && y >= -limit && x + width <= limit && y + height <= limit
}

fn reach_is_within_coordinate_limit(reach: Reach<IntRect>) -> bool {
    match reach {
        Reach::Within(rect) => rect_is_within_coordinate_limit(rect),
        Reach::Nothing | Reach::Anywhere => true,
    }
}

fn position_starts_run(list: &DisplayListStorage, position: ListPosition) -> bool {
    let Some(chunk) = list.chunks().get(position.chunk_index.0 as usize) else {
        return false;
    };
    let runs = chunk.runs();
    let index = runs.partition_point(|run| run.offset < position.offset);
    runs.get(index).is_some_and(|run| run.offset == position.offset)
}

fn position_ends_run(list: &DisplayListStorage, position: ListPosition) -> bool {
    let Some(chunk) = list.chunks().get(position.chunk_index.0 as usize) else {
        return false;
    };
    let runs = chunk.runs();
    let index = runs.partition_point(|run| run.offset.saturating_add(run.size) < position.offset);
    runs.get(index)
        .is_some_and(|run| run.offset.checked_add(run.size) == Some(position.offset))
}

fn span_is_of_whole_runs(list: &DisplayListStorage, span: ListSpan) -> bool {
    span.begin <= span.end && position_starts_run(list, span.begin) && position_ends_run(list, span.end)
}

// The span of a canvas, video or nested context with a layer is one run of the list, which starts with a record that
// draws that content.
fn span_is_run_drawing_content(list: &DisplayListStorage, span: ListSpan, content: LiveContent) -> bool {
    if matches!(content, LiveContent::ImageFrame(_)) || !position_starts_run(list, span.begin) {
        return false;
    }
    let chunk = list.chunk(span.begin.chunk_index);
    let runs = chunk.runs();
    let run = runs[runs.partition_point(|run| run.offset < span.begin.offset)];
    if run.size == 0 || span.end != ListPosition::new(span.begin.chunk_index, run.offset.saturating_add(run.size)) {
        return false;
    }
    let (header, payload) = chunk.record_at(run.offset);
    live_content_of(header.command_type, payload) == Some(content)
}

// Where a walk of the effect tree enters and leaves each effect node, so that one effect is inside another exactly where
// the walk enters and leaves it between where it enters and leaves the other. None for a node the walk does not reach.
fn walk_steps_of_effect_nodes(tree: &VisualContextTree) -> Vec<Option<(usize, usize)>> {
    let effect_count = tree.effect_nodes.len();
    let mut children_by_effect_index: Vec<Vec<usize>> = vec![Vec::new(); effect_count];
    let mut roots = Vec::new();
    for (index, node) in tree.effect_nodes.iter().enumerate() {
        if node.parent.is_none() {
            roots.push(index);
        } else if let Some(children) = children_by_effect_index.get_mut(node.parent.0 as usize) {
            children.push(index);
        }
    }
    let mut steps_by_effect_index: Vec<Option<(usize, usize)>> = vec![None; effect_count];
    let mut step = 0;
    let mut walk: Vec<(usize, usize)> = Vec::new();
    for root in roots {
        walk.push((root, 0));
        steps_by_effect_index[root] = Some((step, step));
        step += 1;
        while let Some((effect, next_child_position)) = walk.last_mut() {
            let effect = *effect;
            match children_by_effect_index[effect].get(*next_child_position) {
                Some(&child) => {
                    *next_child_position += 1;
                    steps_by_effect_index[child] = Some((step, step));
                    step += 1;
                    walk.push((child, 0));
                }
                None => {
                    if let Some((_, left_at)) = &mut steps_by_effect_index[effect] {
                        *left_at = step;
                    }
                    step += 1;
                    walk.pop();
                }
            }
        }
    }
    steps_by_effect_index
}

fn effect_is_strictly_inside(
    walk_steps_by_effect_index: &[Option<(usize, usize)>],
    inner: EffectNodeIndex,
    outer: EffectNodeIndex,
) -> bool {
    let steps_of = |effect: EffectNodeIndex| walk_steps_by_effect_index.get(effect.0 as usize).copied().flatten();
    match (steps_of(inner), steps_of(outer)) {
        (Some((inner_entered_at, inner_left_at)), Some((outer_entered_at, outer_left_at))) => {
            outer_entered_at < inner_entered_at && inner_left_at < outer_left_at
        }
        _ => false,
    }
}

fn check_rects_and_coordinates(stack: &LayerStack) -> Result<(), &'static str> {
    let within = rect_is_within_coordinate_limit;
    let reach_within = reach_is_within_coordinate_limit;
    let radius_limit = i64::from(COORDINATE_LIMIT_IN_DEVICE_PIXELS);
    let radius_is_within = |radius: i32| i64::from(radius) >= 0 && i64::from(radius) <= radius_limit;
    let nodes_are_within = stack.nodes.iter().all(|node| {
        reach_within(node.reach_in_parent_node)
            && node.composite_clips.iter().all(|clip| {
                let radii = clip.corner_radii;
                within(clip.rect)
                    && [radii.top_left, radii.top_right, radii.bottom_right, radii.bottom_left]
                        .iter()
                        .all(|corner| {
                            radius_is_within(corner.horizontal_radius) && radius_is_within(corner.vertical_radius)
                        })
            })
    });
    let bands_are_within = stack
        .bands
        .iter()
        .all(|band| band.cuts.iter().all(|cut| reach_within(cut.cut_reach)));
    let layers_are_within = stack.layers.iter().all(|layer| {
        within(layer.reach)
            && match layer.content {
                LayerContent::Media { reach, .. } => within(reach),
                LayerContent::Painted { .. } => true,
            }
    });
    let swept_reaches_are_within = stack
        .swept_reach_in_parent_space_by_space_index
        .iter()
        .flatten()
        .all(|swept_reach| within(*swept_reach));
    if !nodes_are_within || !bands_are_within || !layers_are_within || !swept_reaches_are_within {
        return Err("a rect or a coordinate of the stack lies past the limit a plan keeps to");
    }
    Ok(())
}

fn check_values_per_space(stack: &LayerStack) -> Result<(), &'static str> {
    if stack.tiles_hold_content_moving_within_space_by_space_index.len() != stack.spaces.len()
        || stack.swept_reach_in_parent_space_by_space_index.len() != stack.spaces.len()
    {
        return Err("a value the stack holds per space is not there for every space");
    }
    let only_animated_spaces_are_swept = stack
        .spaces
        .iter()
        .zip(&stack.swept_reach_in_parent_space_by_space_index)
        .all(|(space, swept_reach)| {
            swept_reach.is_none() || matches!(space.anchor_kind, RasterSpaceAnchorKind::AnimatedTransform { .. })
        });
    if !only_animated_spaces_are_swept {
        return Err("a space whose anchor the compositor does not animate has a swept reach");
    }
    Ok(())
}

fn check_nodes(stack: &LayerStack, list: &DisplayListStorage, tree: &VisualContextTree) -> Result<(), &'static str> {
    let mut identities_of_composited_nodes = HashSet::new();
    let mut effects_of_composited_nodes = HashSet::new();
    for (index, node) in stack.nodes.iter().enumerate() {
        if node.raster_space_index.0 >= stack.spaces.len() {
            return Err("a node is in a space the stack does not hold");
        }
        if node.parent_index.is_some_and(|parent_index| parent_index.0 >= index) {
            return Err("a node is not after the node it is in");
        }
        // A node that is not composited is in the node of the space its space is in, and a composited node is in a node
        // of its own space. So nodes nest no deeper than spaces, plus composited effects, which nest as the tree nests
        // them.
        let parent = node.parent_index.map(|parent_index| &stack.nodes[parent_index.0]);
        let is_in_node_a_plan_puts_it_in = match (node.composited_effect, parent) {
            (None, None) => node.raster_space_index == RasterSpaceIndex::VIEWPORT,
            (None, Some(parent)) => {
                node.raster_space_index != RasterSpaceIndex::VIEWPORT
                    && parent.composited_effect.is_none()
                    && parent.raster_space_index == stack.spaces[node.raster_space_index.0].parent_index
            }
            (Some(_), Some(parent)) => parent.raster_space_index == node.raster_space_index,
            (Some(_), None) => return Err("a composited node is in no other node"),
        };
        if !is_in_node_a_plan_puts_it_in {
            return Err("a node is not in the node of its space or of the space its space is in");
        }
        let clips_are_one_per_space_in_order = node
            .composite_clips
            .iter()
            .all(|clip| clip.raster_space_index.0 < stack.spaces.len())
            && node
                .composite_clips
                .windows(2)
                .all(|pair| pair[0].raster_space_index < pair[1].raster_space_index);
        if !clips_are_one_per_space_in_order {
            return Err("the composite clips of a node are not one per space of the stack, in order");
        }
        let Some(effect) = node.composited_effect else {
            continue;
        };
        let effect_takes_part_in_replay = tree.effect_is_live(effect)
            && list
                .effect_clip_plan(tree)
                .output_clip_if_participating(effect)
                .is_some();
        if !effect_takes_part_in_replay {
            return Err("a node is composited inside an effect that a replay of the list does not enter");
        }
        if !identities_of_composited_nodes.insert(node.identity) {
            return Err("two composited nodes share an identity");
        }
        if !effects_of_composited_nodes.insert(effect) {
            return Err("two nodes are composited inside one effect");
        }
    }
    Ok(())
}

fn check_bands(stack: &LayerStack, list: &DisplayListStorage) -> Result<(), &'static str> {
    if stack.bands.is_empty() {
        return Err("the stack holds no band");
    }
    for (index, band) in stack.bands.iter().enumerate() {
        let Some(node) = stack.nodes.get(band.placement_node_index.0) else {
            return Err("a band is of a node the stack does not hold");
        };
        let band_is_inside_band_of_parent_node =
            match (band.parent_band_holding_this_band_as_barrier, node.parent_index) {
                (None, None) => true,
                (Some(parent_band_index), Some(parent_node_index)) => {
                    parent_band_index.0 < index
                        && stack.bands[parent_band_index.0].placement_node_index == parent_node_index
                }
                _ => false,
            };
        if !band_is_inside_band_of_parent_node {
            return Err("a band is not after a band of the node its node is in, which holds it as a barrier");
        }
        // A plan opens each band but the first for a stretch of records that goes into that band or into a band inside
        // it. The first band holds nothing where no record can show.
        if index != 0 && band.record_spans.is_empty() && band.cuts.is_empty() {
            return Err("a band other than the first holds no span and no cut");
        }
        let layers_are_in_range = band.layer_below_all_cuts.0 < stack.layers.len()
            && band.cuts.iter().all(|cut| cut.layer_above_cut.0 < stack.layers.len());
        if !layers_are_in_range {
            return Err("a band names a layer the stack does not hold");
        }
        if !position_starts_run(list, band.first_record_position) {
            return Err("a band starts where no run of the list starts");
        }
        let spans_are_of_whole_runs_in_paint_order =
            band.record_spans.iter().all(|span| span_is_of_whole_runs(list, *span))
                && band.record_spans.windows(2).all(|pair| pair[0].end <= pair[1].begin);
        if !spans_are_of_whole_runs_in_paint_order {
            return Err("the spans of a band are not of whole runs of the list, in paint order");
        }
        let cuts_are_at_runs_in_paint_order = band
            .cuts
            .iter()
            .all(|cut| position_starts_run(list, cut.barrier_first_record_position))
            && band
                .cuts
                .windows(2)
                .all(|pair| pair[0].barrier_first_record_position <= pair[1].barrier_first_record_position);
        if !cuts_are_at_runs_in_paint_order {
            return Err("the cuts of a band are not at runs of the list, in paint order");
        }
    }
    Ok(())
}

fn check_layers(stack: &LayerStack, list: &DisplayListStorage) -> Result<(), &'static str> {
    let mut band_naming_layer_by_layer_index: Vec<Option<BandIndex>> = vec![None; stack.layers.len()];
    for (index, band) in stack.bands.iter().enumerate() {
        let layers_of_band =
            std::iter::once(band.layer_below_all_cuts).chain(band.cuts.iter().map(|cut| cut.layer_above_cut));
        for layer_index in layers_of_band {
            if band_naming_layer_by_layer_index[layer_index.0]
                .replace(BandIndex(index))
                .is_some()
            {
                return Err("a layer is named more than once by the bands");
            }
        }
    }
    let mut layer_identities = HashSet::with_capacity(stack.layers.len());
    let mut media_layer_index_by_identity: HashMap<(LiveContent, u32), LayerIndex> = HashMap::new();
    for (index, layer) in stack.layers.iter().enumerate() {
        if layer.placement_node_index.0 >= stack.nodes.len() || layer.band_index.0 >= stack.bands.len() {
            return Err("a layer is of a node or a band the stack does not hold");
        }
        let band = &stack.bands[layer.band_index.0];
        if layer.placement_node_index != band.placement_node_index {
            return Err("a layer is of another node than its band");
        }
        if !layer_identities.insert(layer.identity) {
            return Err("two layers share an identity");
        }
        match (&layer.identity, &layer.content) {
            (
                LayerIdentity::Painted {
                    node,
                    band_ordinal_in_node,
                    above_barrier,
                },
                LayerContent::Painted { band_index },
            ) => {
                if *band_index != layer.band_index || band_naming_layer_by_layer_index[index] != Some(layer.band_index)
                {
                    return Err("a painted layer is not a layer of its band");
                }
                if *node != stack.nodes[layer.placement_node_index.0].identity
                    || *band_ordinal_in_node != band.band_ordinal_in_node
                {
                    return Err("a painted layer is not named after its band");
                }
                let is_below_all_cuts = band.layer_below_all_cuts == LayerIndex(index);
                if is_below_all_cuts != above_barrier.is_none() {
                    return Err("a painted layer sits above a barrier other than where its band is cut");
                }
            }
            (
                LayerIdentity::Media { content, ordinal },
                LayerContent::Media {
                    content: drawn_content,
                    record_span,
                    ..
                },
            ) => {
                if content != drawn_content || band_naming_layer_by_layer_index[index].is_some() {
                    return Err("a media layer is not a barrier of its band");
                }
                if !span_is_run_drawing_content(list, *record_span, *content) {
                    return Err("a media layer is not a run of the list that draws its content");
                }
                media_layer_index_by_identity.insert((*content, *ordinal), LayerIndex(index));
            }
            _ => return Err("the identity and the content of a layer are of different kinds"),
        }
    }
    if media_layer_index_by_identity.len() > MAXIMUM_MEDIA_LAYER_COUNT {
        return Err("the stack holds more canvas, video and nested context layers than a plan makes");
    }
    // The walk of the bands checks the barriers that are bands.
    let mut media_layer_is_barrier_by_layer_index = vec![false; stack.layers.len()];
    for (index, band) in stack.bands.iter().enumerate() {
        for cut in &band.cuts {
            let LayerIdentity::Painted {
                above_barrier: Some(barrier),
                ..
            } = stack.layers[cut.layer_above_cut.0].identity
            else {
                return Err("a layer above a cut sits above no barrier");
            };
            if let BarrierIdentity::Media { content, ordinal } = barrier {
                let Some(&media_layer_index) = media_layer_index_by_identity.get(&(content, ordinal)) else {
                    return Err("a barrier names a canvas, video or nested context the stack has no layer for");
                };
                if stack.layers[media_layer_index.0].band_index != BandIndex(index)
                    || std::mem::replace(&mut media_layer_is_barrier_by_layer_index[media_layer_index.0], true)
                {
                    return Err("a media layer is not a barrier once in its band");
                }
            }
        }
    }
    let every_media_layer_is_a_barrier = media_layer_index_by_identity
        .values()
        .all(|media_layer_index| media_layer_is_barrier_by_layer_index[media_layer_index.0]);
    if !every_media_layer_is_a_barrier {
        return Err("a media layer is not a barrier in its band");
    }
    Ok(())
}

// A plan puts each stretch of records into one band, or for a canvas, video or nested context with a layer, into that
// layer, and a stretch holds at least one run. So no record is in two spans, and a stack holds no more spans than the
// list holds runs.
fn check_no_record_is_held_twice(stack: &LayerStack) -> Result<(), &'static str> {
    let spans_of_media_layers = stack.layers.iter().filter_map(|layer| match layer.content {
        LayerContent::Media { record_span, .. } => Some(record_span),
        LayerContent::Painted { .. } => None,
    });
    let mut spans: Vec<ListSpan> = stack
        .bands
        .iter()
        .flat_map(|band| band.record_spans.iter().copied())
        .chain(spans_of_media_layers)
        .collect();
    if spans.iter().any(|span| span.begin >= span.end) {
        return Err("a span of the stack holds no record");
    }
    spans.sort_unstable_by_key(|span| span.begin);
    if spans.windows(2).any(|pair| pair[1].begin < pair[0].end) {
        return Err("two spans of the stack hold the same record");
    }
    Ok(())
}

// Bands and layers come in the order a plan makes them: the order of a walk from the first band that enters each other
// band from the one cut of the band it is in whose barrier names it. A band comes right before the bands it holds. Its
// layer below all cuts comes first, then for each cut the layers of the barrier and the layer above the cut. So the walk
// enters each band once, and the layers inside a band are next to each other.
fn check_bands_and_layers_are_in_walk_order(stack: &LayerStack) -> Result<(), &'static str> {
    const NOT_IN_WALK_ORDER: &str = "the bands and layers of the stack are not in the order of a walk of the bands";
    // Each band has a name of its own, as the identity of its layer below all cuts is unique.
    let band_index_by_name: HashMap<(PlacementNodeIdentity, u32), usize> = stack
        .bands
        .iter()
        .enumerate()
        .map(|(index, band)| {
            let node_identity = stack.nodes[band.placement_node_index.0].identity;
            ((node_identity, band.band_ordinal_in_node), index)
        })
        .collect();
    let mut next_band_index = 0;
    let mut next_layer_index = 0;
    // The bands the walk is in, outermost first, each with how many of its cuts the walk has passed.
    let mut bands_entered: Vec<(usize, usize)> = Vec::new();
    let mut band_to_enter = Some(0);
    loop {
        if let Some(band_index) = band_to_enter.take() {
            if band_index != next_band_index || stack.bands[band_index].layer_below_all_cuts.0 != next_layer_index {
                return Err(NOT_IN_WALK_ORDER);
            }
            next_band_index += 1;
            next_layer_index += 1;
            bands_entered.push((band_index, 0));
        }
        let Some(&(band_index, cuts_passed)) = bands_entered.last() else {
            break;
        };
        let Some(cut) = stack.bands[band_index].cuts.get(cuts_passed) else {
            bands_entered.pop();
            // The walk leaves the barrier of a cut of the band it is in, so the layer above that cut comes next.
            if let Some((parent_band_index, parent_cuts_passed)) = bands_entered.last_mut() {
                let parent_cut = &stack.bands[*parent_band_index].cuts[*parent_cuts_passed];
                if parent_cut.layer_above_cut.0 != next_layer_index {
                    return Err(NOT_IN_WALK_ORDER);
                }
                next_layer_index += 1;
                *parent_cuts_passed += 1;
            }
            continue;
        };
        let LayerIdentity::Painted {
            above_barrier: Some(barrier),
            ..
        } = stack.layers[cut.layer_above_cut.0].identity
        else {
            return Err("a layer above a cut sits above no barrier");
        };
        match barrier {
            BarrierIdentity::Media { content, ordinal } => {
                let media_layer_is_next = stack
                    .layers
                    .get(next_layer_index)
                    .is_some_and(|layer| layer.identity == LayerIdentity::Media { content, ordinal });
                if !media_layer_is_next || cut.layer_above_cut.0 != next_layer_index + 1 {
                    return Err(NOT_IN_WALK_ORDER);
                }
                next_layer_index += 2;
                if let Some((_, cuts_passed)) = bands_entered.last_mut() {
                    *cuts_passed += 1;
                }
            }
            BarrierIdentity::NestedNodeBand {
                node,
                band_ordinal_in_node,
            } => {
                let Some(&nested_band_index) = band_index_by_name.get(&(node, band_ordinal_in_node)) else {
                    return Err("a barrier names a band the stack does not hold");
                };
                if stack.bands[nested_band_index].parent_band_holding_this_band_as_barrier
                    != Some(BandIndex(band_index))
                {
                    return Err("a barrier names a band that is not in the band of the barrier");
                }
                band_to_enter = Some(nested_band_index);
            }
        }
    }
    if next_band_index != stack.bands.len() || next_layer_index != stack.layers.len() {
        return Err("the walk of the bands does not reach every band and layer of the stack");
    }
    Ok(())
}

// A frame enters the effects of the nodes around a layer outermost first, each from the one around it.
fn check_composited_effects_nest_as_in_tree(stack: &LayerStack, tree: &VisualContextTree) -> Result<(), &'static str> {
    if stack.nodes.iter().all(|node| node.composited_effect.is_none()) {
        return Ok(());
    }
    let walk_steps_by_effect_index = walk_steps_of_effect_nodes(tree);
    let mut innermost_effect_around_by_node_index: Vec<Option<EffectNodeIndex>> = Vec::with_capacity(stack.nodes.len());
    for node in &stack.nodes {
        let innermost_effect_around = node.parent_index.and_then(|parent_index| {
            stack.nodes[parent_index.0]
                .composited_effect
                .or(innermost_effect_around_by_node_index[parent_index.0])
        });
        if let (Some(effect), Some(effect_around)) = (node.composited_effect, innermost_effect_around)
            && !effect_is_strictly_inside(&walk_steps_by_effect_index, effect, effect_around)
        {
            return Err("the composited effects around a node are not nested as the tree nests them");
        }
        innermost_effect_around_by_node_index.push(innermost_effect_around);
    }
    Ok(())
}

fn check_base_layer(stack: &LayerStack) -> Result<(), &'static str> {
    if !stack
        .bands
        .iter()
        .any(|band| band.layer_below_all_cuts == stack.base_layer)
    {
        return Err("the base layer is not the layer below the cuts of a band");
    }
    let space = &stack.spaces[stack.space_of_layer(stack.base_layer).0];
    if matches!(space.anchor_kind, RasterSpaceAnchorKind::AnimatedTransform { .. }) {
        return Err("the base layer is in a space whose anchor the compositor animates");
    }
    Ok(())
}

impl LayerStack {
    /// The stack that another process planned for `list` under `tree` within `viewport_rect`, once what came on the
    /// wire passes the checks that let every consumer read the stack without a panic, an index out of range, an
    /// overflow, or work without a bound, and the rest of the stack is worked out as the planner works it out. The
    /// checks also build the lookups of the stack.
    ///
    /// What is worked out needs no check, as it comes from the functions the planner uses: a stack of one layer is the
    /// one `single_layer` makes for the whole list; the spaces of a stack of more layers are those
    /// `RasterSpaces::spaces_at_anchors` makes at the anchors `raster_space_anchors` gives for the tree, and its
    /// composited effects are the effects of its composited nodes, with what the tree tells of how each reads; and the
    /// layering obstacles of either are those of the list and the tree.
    ///
    /// A stack passes where the viewport lies within twice the extent limit of a plan. A stack of more layers also
    /// passes only where every rect and coordinate it holds lies within that limit; it holds a value per space for each
    /// space, and only spaces whose anchor the compositor animates have a swept reach; nodes are after the nodes they
    /// are in, with composite clips one per space; a node that is not composited is in the node of the space its space
    /// is in, or in no node if it is in the viewport's space, and a composited node is in a node of its own space;
    /// bands are of nodes of the stack, after the bands that hold them, each band but the first holds a span or a cut,
    /// and spans and cuts are at runs of the list in paint order; no span is empty and no record is in two spans; each
    /// layer is named once, by its band, as a painted layer or as a canvas, video or nested context that is a run of
    /// the list drawing it and a barrier once in its band, and the stack holds no more of those than a plan makes;
    /// bands and layers come in the order of a walk of the bands that enters each band from the one cut of the band it
    /// is in whose barrier names it; identities of layers and of composited nodes are unique; nodes are composited
    /// inside different effects that a replay of the list enters, nested as the tree nests them; and the base layer is
    /// the layer below the cuts of a band, outside the spaces the compositor animates.
    ///
    /// So a stack holds no more spans than the list holds runs, no more cuts than bands and media layers, and no more
    /// composited nodes than the tree holds effects. Its nodes nest no deeper than its spaces nest plus the effects of
    /// the tree nest, and the layers inside a band are next to each other, so a frame that composites the layers in
    /// order enters each composited band once.
    ///
    /// The checks leave out what can only make pixels wrong: whether reaches are large enough for what records draw,
    /// which layers can hold content, whether every record is in a span, whether composite clips are the clips content
    /// needs, whether the identities of nodes and layers are those a plan gives them, which effects must be composited,
    /// which spaces hold content that moves within them, and where the window server could show media. The list and
    /// the tree pass their own checks first.
    pub fn validated(
        unvalidated: UnvalidatedLayerStack,
        list: &DisplayListStorage,
        tree: &VisualContextTree,
        viewport_rect: IntRect,
    ) -> Result<LayerStack, &'static str> {
        if !rect_is_within_coordinate_limit(viewport_rect) {
            return Err("the viewport of the plan lies past the limit a plan keeps to");
        }
        let stack = match unvalidated.0 {
            StackOnWire::OneLayer(why_list_is_one_layer) => {
                let mut stack = LayerStack::single_layer(viewport_rect, list.whole_span(), Some(why_list_is_one_layer));
                stack.layering_obstacles = LayeringObstacles::of(list, tree);
                return Ok(stack);
            }
            StackOnWire::Layered {
                nodes,
                bands,
                layers,
                base_layer,
                tiles_hold_content_moving_within_space_by_space_index,
                swept_reach_in_parent_space_by_space_index,
            } => LayerStack {
                spaces: RasterSpaces::spaces_at_anchors(tree, &anchors_of_layer_plans(tree), viewport_rect),
                nodes,
                bands,
                layers,
                composited_effects: Vec::new(),
                base_layer,
                why_list_is_one_layer: None,
                layering_obstacles: LayeringObstacles::default(),
                tiles_hold_content_moving_within_space_by_space_index,
                swept_reach_in_parent_space_by_space_index,
                index_of_layer_with_identity: HashMap::new(),
                has_layer_at_or_above_base_by_band_index: Vec::new(),
            },
        };
        check_rects_and_coordinates(&stack)?;
        check_values_per_space(&stack)?;
        check_nodes(&stack, list, tree)?;
        check_bands(&stack, list)?;
        check_layers(&stack, list)?;
        check_no_record_is_held_twice(&stack)?;
        check_bands_and_layers_are_in_walk_order(&stack)?;
        check_composited_effects_nest_as_in_tree(&stack, tree)?;
        check_base_layer(&stack)?;
        Ok(LayerStack {
            composited_effects: CompositedEffect::of_nodes(&stack.nodes, tree),
            layering_obstacles: LayeringObstacles::of(list, tree),
            ..stack
        }
        .with_lookups())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{
        ClipNodeIndex, ContextRef, DisplayListCommand, DrawCanvas, FillRect, SpatialNodeIndex,
        VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::display_list::layers::planning::{LayerStackInputs, PlannedLayers, plan_layer_stack};
    use crate::display_list::layers::{NodeNamesFromIndices, WindowServerPresentationOfMedia};
    use crate::display_list::placement::{RasterSpace, RasterSpaceGeometry};
    use crate::display_list::storage::ListChunk;
    use crate::display_list::tile_keys::{
        BandTileKeys, FrameStateReadByReplay, LayerRasterSettings, LiveContentVersions, TileKeyingOfList,
        TileKeyingOfRasterSpace,
    };
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
    use crate::visual_context::{
        CompositorScrollNodeRecord, CompositorScrollTable, EffectNodeData, EffectsData, ScrollData, SpatialData,
        StickyData, TransformData, TransformDataRole,
    };
    use libgfx_rust::{
        Color, CompositingAndBlendingOperator, FloatMatrix4x4, FloatPoint, FloatRect, FloatSize, ScalingMode,
    };
    use std::sync::Arc;

    const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);

    struct Page {
        tree: VisualContextTree,
        list: Arc<DisplayListStorage>,
    }

    impl Page {
        fn planned(&self) -> PlannedLayers {
            plan_layer_stack(&LayerStackInputs {
                list: &self.list,
                tree: &self.tree,
                scroll_offsets: &[],
                viewport_rect: VIEWPORT,
            })
        }

        fn validated(&self, stack: LayerStack) -> Result<LayerStack, &'static str> {
            LayerStack::validated(on_wire(&stack), &self.list, &self.tree, VIEWPORT)
        }
    }

    // The stack as decoding its wire form gives it.
    fn on_wire(stack: &LayerStack) -> UnvalidatedLayerStack {
        UnvalidatedLayerStack(match stack.why_list_is_one_layer {
            Some(why_list_is_one_layer) => StackOnWire::OneLayer(why_list_is_one_layer),
            None => StackOnWire::Layered {
                nodes: stack.nodes.clone(),
                bands: stack.bands.clone(),
                layers: stack.layers.clone(),
                base_layer: stack.base_layer,
                tiles_hold_content_moving_within_space_by_space_index: stack
                    .tiles_hold_content_moving_within_space_by_space_index
                    .clone(),
                swept_reach_in_parent_space_by_space_index: stack.swept_reach_in_parent_space_by_space_index.clone(),
            },
        })
    }

    fn effects(opacity: f32) -> EffectNodeData {
        EffectNodeData::Effects(EffectsData {
            isolation: false,
            opacity,
            blend_mode: CompositingAndBlendingOperator::Normal,
            filter: None,
            backdrop_filter: None,
        })
    }

    fn transform(bounds: FloatRect) -> TransformData {
        TransformData {
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
        }
    }

    fn tree_with_root_isolation_effect(root_opacity: f32) -> (VisualContextTree, ContextRef) {
        let mut tree = VisualContextTree::create(transform(FloatRect::default()));
        let root = tree.append_effect(
            effects(root_opacity),
            EffectNodeIndex::NONE,
            VISUAL_VIEWPORT_NODE_INDEX,
            ClipNodeIndex::NONE,
        );
        tree.root_isolation_effect = Some(root);
        let viewport = ContextRef {
            effect: root,
            ..ContextRef::default()
        };
        (tree, viewport)
    }

    fn scroll_node(tree: &mut VisualContextTree, parent: SpatialNodeIndex) -> SpatialNodeIndex {
        tree.append_spatial(
            SpatialData::Scroll(ScrollData {
                state_slot: NO_SCROLL_STATE_SLOT,
                owner_paintable: NodeSlotId::INVALID,
            }),
            parent,
        )
    }

    fn scroll_record(
        node: SpatialNodeIndex,
        parent: SpatialNodeIndex,
        scrollport_rect: IntRect,
        scroll_height: f32,
        is_viewport: bool,
    ) -> CompositorScrollNodeRecord {
        CompositorScrollNodeRecord {
            scroll_node_index: node,
            parent_scroll_node_index: parent,
            scrollport_rect,
            max_scroll_offset: FloatPoint {
                x: 0.0,
                y: scroll_height,
            },
            is_viewport,
            can_be_wheel_scrolled_vertically: true,
            ..CompositorScrollNodeRecord::default()
        }
    }

    // A tree whose document scrolls in the viewport, with the contexts of the viewport and of the document.
    fn tree_with_document_scroller(nested_scrollers: &[IntRect]) -> (VisualContextTree, ContextRef, Vec<ContextRef>) {
        let (mut tree, viewport) = tree_with_root_isolation_effect(1.0);
        let document_node = scroll_node(&mut tree, VISUAL_VIEWPORT_NODE_INDEX);
        let mut records = vec![scroll_record(
            document_node,
            VISUAL_VIEWPORT_NODE_INDEX,
            VIEWPORT,
            2000.0,
            true,
        )];
        let mut contexts = vec![ContextRef {
            spatial: document_node,
            ..viewport
        }];
        for &scrollport_rect in nested_scrollers {
            let nested_node = scroll_node(&mut tree, document_node);
            records.push(scroll_record(nested_node, document_node, scrollport_rect, 500.0, false));
            contexts.push(ContextRef {
                spatial: nested_node,
                ..viewport
            });
        }
        assert!(tree.set_compositor_scroll_table(CompositorScrollTable {
            nodes: records.into(),
            ..CompositorScrollTable::default()
        }));
        (tree, viewport, contexts)
    }

    fn fill(rect: IntRect) -> FillRect {
        FillRect {
            rect,
            color: Color::from_rgb(0, 128, 0),
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    fn canvas(rect: IntRect, id: u64) -> DrawCanvas {
        DrawCanvas {
            dst_rect: rect,
            canvas_id: CanvasId(id),
            scaling_mode: ScalingMode::NearestNeighbor,
        }
    }

    #[derive(Default)]
    struct ListOfOneChunk {
        builder: DisplayListBuilder,
    }

    impl ListOfOneChunk {
        fn with<C: DisplayListCommand>(mut self, command: C, context: ContextRef) -> Self {
            self.builder.append(&command, &[], context);
            self
        }

        fn storage(&self) -> Arc<DisplayListStorage> {
            DisplayListStorage::of_builder(&self.builder)
        }
    }

    fn chunk_of<C: DisplayListCommand>(command: C, context: ContextRef) -> Arc<ListChunk> {
        let mut builder = DisplayListBuilder::new();
        builder.append(&command, &[], context);
        Arc::new(ListChunk::copied(builder.bytes(), builder.command_runs(), 0))
    }

    fn opacity_animation(effect: EffectNodeIndex) -> crate::visual_animation::VisualAnimation {
        use crate::visual_animation::{VisualAnimation, VisualAnimationKeyframe, VisualAnimationValue};
        let keyframe = |offset: f64, opacity: f32| VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: VisualAnimationValue::Opacity(opacity),
        };
        VisualAnimation {
            target_kind: crate::host::FfiVisualAnimationTargetKind::Opacity,
            node_indices: vec![effect.0],
            timing: crate::animation_timing::AnimationEffectTiming {
                iteration_duration: crate::animation_timing::TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            keyframes: vec![keyframe(0.0, 1.0), keyframe(1.0, 0.0)],
            ..VisualAnimation::default()
        }
    }

    fn spinning_animation(node: SpatialNodeIndex) -> crate::visual_animation::VisualAnimation {
        use crate::host::FfiVisualAnimationTransformOperationKind;
        use crate::visual_animation::{
            VisualAnimation, VisualAnimationKeyframe, VisualAnimationTransformOperation, VisualAnimationValue,
        };
        let keyframe = |offset: f64, degrees: f32| VisualAnimationKeyframe {
            offset,
            easing: crate::easing::Easing::default(),
            value: VisualAnimationValue::Transform(vec![
                VisualAnimationTransformOperation::new(FfiVisualAnimationTransformOperationKind::Rotate, &[degrees])
                    .unwrap(),
            ]),
        };
        VisualAnimation {
            target_kind: crate::host::FfiVisualAnimationTargetKind::Transform,
            node_indices: vec![node.0],
            timing: crate::animation_timing::AnimationEffectTiming {
                iteration_duration: crate::animation_timing::TimeValue::milliseconds(1000.0),
                ..VisualAnimation::default().timing
            },
            keyframes: vec![keyframe(0.0, 0.0), keyframe(1.0, 360.0)],
            ..VisualAnimation::default()
        }
    }

    // A long page of fills in the document, each in a chunk of its own, under a header fixed to the viewport.
    fn plain_page() -> Page {
        let (tree, viewport, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let mut chunks = vec![chunk_of(fill(IntRect::new(0, 0, 800, 2600)), document)];
        for line in 0..40 {
            chunks.push(chunk_of(fill(IntRect::new(10, 60 + line * 60, 700, 20)), document));
        }
        chunks.push(chunk_of(fill(IntRect::new(0, 0, 800, 50)), viewport));
        Page {
            tree,
            list: DisplayListStorage::of_chunks(chunks).unwrap(),
        }
    }

    fn page_with_two_canvases() -> Page {
        let (tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(100, 100, 200, 100), 1), document)
            .with(fill(IntRect::new(150, 150, 200, 100)), document)
            .with(canvas(IntRect::new(100, 400, 200, 100), 2), document)
            .with(fill(IntRect::new(150, 450, 200, 100)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn page_with_canvas_in_nested_scroller() -> Page {
        let (tree, _, contexts) = tree_with_document_scroller(&[IntRect::new(100, 100, 300, 200)]);
        let (document, nested) = (contexts[0], contexts[1]);
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(100, 100, 300, 700)), nested)
            .with(canvas(IntRect::new(120, 150, 100, 100), 1), nested)
            .with(fill(IntRect::new(150, 200, 100, 100)), nested)
            .with(fill(IntRect::new(0, 900, 800, 100)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn page_with_sticky_header() -> Page {
        let (mut tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let mut sticky_data =
            StickyData::unconstrained(document.spatial, None, NO_SCROLL_STATE_SLOT, NodeSlotId::INVALID);
        sticky_data.position_relative_to_scroller = FloatPoint { x: 0.0, y: 100.0 };
        sticky_data.border_box_size = FloatSize {
            width: 800.0,
            height: 50.0,
        };
        sticky_data.scrollport_size = FloatSize {
            width: 800.0,
            height: 600.0,
        };
        sticky_data.containing_block_region = FloatRect::new(0.0, 0.0, 800.0, 2000.0);
        sticky_data.needs_parent_offset_adjustment = true;
        sticky_data.inset_top = Some(0.0);
        let sticky_node = tree.append_spatial(SpatialData::Sticky(sticky_data), document.spatial);
        let sticky = ContextRef {
            spatial: sticky_node,
            ..document
        };
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(0, 100, 800, 50)), sticky)
            .with(fill(IntRect::new(0, 300, 800, 50)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    // An element whose opacity the compositor animates holds another whose opacity it animates.
    fn page_with_nested_composited_effects() -> Page {
        let (mut tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let outer = tree.append_effect_node_under(document, effects(1.0));
        let inner = tree.append_effect_node_under(outer, effects(1.0));
        tree.set_visual_animations(vec![opacity_animation(outer.effect), opacity_animation(inner.effect)]);
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(100, 100, 300, 300)), outer)
            .with(fill(IntRect::new(150, 150, 100, 100)), inner)
            .with(fill(IntRect::new(0, 500, 800, 50)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn page_with_spinner() -> Page {
        let (mut tree, viewport) = tree_with_root_isolation_effect(1.0);
        let spinner_node = tree.append_spatial(
            SpatialData::Transform(transform(FloatRect::new(50.0, 50.0, 40.0, 40.0))),
            VISUAL_VIEWPORT_NODE_INDEX,
        );
        tree.set_visual_animations(vec![spinning_animation(spinner_node)]);
        let spinner = ContextRef {
            spatial: spinner_node,
            ..viewport
        };
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner)
            .with(fill(IntRect::new(0, 100, 100, 100)), viewport);
        Page {
            tree,
            list: list.storage(),
        }
    }

    // The background is painted below the root group, which a spinner holds all of, so the compositor composites the
    // root isolation effect over the spinner's transform.
    fn page_with_spinner_holding_the_root_group() -> Page {
        let (mut tree, viewport) = tree_with_root_isolation_effect(1.0);
        let spinner_node = tree.append_spatial(
            SpatialData::Transform(transform(FloatRect::new(50.0, 50.0, 40.0, 40.0))),
            VISUAL_VIEWPORT_NODE_INDEX,
        );
        tree.set_visual_animations(vec![spinning_animation(spinner_node)]);
        let spinner = ContextRef {
            spatial: spinner_node,
            ..viewport
        };
        let list = ListOfOneChunk::default()
            .with(fill(VIEWPORT), ContextRef::default())
            .with(fill(IntRect::new(50, 50, 40, 40)), spinner);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn page_under_translucent_root() -> Page {
        let (tree, viewport) = tree_with_root_isolation_effect(0.5);
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 600)), viewport)
            .with(canvas(IntRect::new(100, 100, 200, 100), 1), viewport);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn page_without_records() -> Page {
        let (tree, _) = tree_with_root_isolation_effect(1.0);
        Page {
            tree,
            list: DisplayListStorage::of_chunks(Vec::new()).unwrap(),
        }
    }

    // A record that draws nothing, which no band holds, so that the only band of the stack holds nothing.
    fn page_whose_records_draw_nothing() -> Page {
        let (tree, viewport) = tree_with_root_isolation_effect(1.0);
        let list = ListOfOneChunk::default().with(fill(IntRect::new(10, 10, 0, 0)), viewport);
        Page {
            tree,
            list: list.storage(),
        }
    }

    // More scrollers in the document than get spaces.
    const NESTED_SCROLLER_COUNT_OF_CROWDED_PAGE: i32 = 80;

    fn page_with_many_nested_scrollers() -> Page {
        let scrollport_rects: Vec<IntRect> = (0..NESTED_SCROLLER_COUNT_OF_CROWDED_PAGE)
            .map(|index| IntRect::new(10, 100 + index * 220, 300, 200))
            .collect();
        let (tree, _, contexts) = tree_with_document_scroller(&scrollport_rects);
        let mut list = ListOfOneChunk::default().with(fill(IntRect::new(0, 0, 800, 20000)), contexts[0]);
        for (scrollport_rect, context) in scrollport_rects.iter().zip(&contexts[1..]) {
            list = list.with(fill(*scrollport_rect), *context);
        }
        Page {
            tree,
            list: list.storage(),
        }
    }

    // One canvas more than get layers, the last after a fill.
    fn page_with_nine_canvases() -> Page {
        let (tree, viewport) = tree_with_root_isolation_effect(1.0);
        let mut list = ListOfOneChunk::default().with(fill(VIEWPORT), viewport);
        for index in 0..9 {
            list = list
                .with(fill(IntRect::new(10 + index * 80, 10, 60, 60)), viewport)
                .with(
                    canvas(IntRect::new(10 + index * 80, 100, 60, 60), index as u64 + 1),
                    viewport,
                );
        }
        Page {
            tree,
            list: list.storage(),
        }
    }

    // Two elements side by side whose opacity the compositor animates.
    fn page_with_two_fading_siblings() -> Page {
        let (mut tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let left = tree.append_effect_node_under(document, effects(1.0));
        let right = tree.append_effect_node_under(document, effects(1.0));
        tree.set_visual_animations(vec![opacity_animation(left.effect), opacity_animation(right.effect)]);
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(100, 100, 200, 200)), left)
            .with(fill(IntRect::new(400, 100, 200, 200)), right)
            .with(fill(IntRect::new(0, 500, 800, 50)), document);
        Page {
            tree,
            list: list.storage(),
        }
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
                    filter: Arc::new(libgfx_rust::filter::Filter::blur(4.0, 4.0, None).serialize()),
                    region,
                    corner_radii: libgfx_rust::CornerRadii::default(),
                }),
            }),
        )
    }

    // A backdrop filter over the document alone, which reads only its own layer, and one over an earlier canvas, which
    // the compositor composites.
    fn page_with_frosted_glass() -> Page {
        let (mut tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let over_document = frosted_glass(&mut tree, document, IntRect::new(400, 100, 200, 100));
        let over_canvas = frosted_glass(&mut tree, document, IntRect::new(100, 100, 200, 100));
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(fill(IntRect::new(400, 100, 200, 100)), over_document)
            .with(canvas(IntRect::new(100, 100, 200, 100), 1), document)
            .with(fill(IntRect::new(100, 100, 200, 100)), over_canvas)
            .with(fill(IntRect::new(0, 500, 800, 50)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    // A spinner in a group scaled up eighty times, whose space a plan rasters at that scale.
    fn page_with_spinner_in_large_scale() -> Page {
        let (mut tree, viewport) = tree_with_root_isolation_effect(1.0);
        let mut scaling = transform(FloatRect::new(0.0, 0.0, 10.0, 10.0));
        scaling.matrix = libgfx_rust::scale_matrix(80.0, 80.0, 1.0);
        let scaled_node = tree.append_spatial(SpatialData::Transform(scaling), VISUAL_VIEWPORT_NODE_INDEX);
        let spinner_node = tree.append_spatial(
            SpatialData::Transform(transform(FloatRect::new(1.0, 1.0, 4.0, 4.0))),
            scaled_node,
        );
        tree.set_visual_animations(vec![spinning_animation(spinner_node)]);
        let spinner = ContextRef {
            spatial: spinner_node,
            ..viewport
        };
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 100, 100)), viewport)
            .with(fill(IntRect::new(1, 1, 4, 4)), spinner)
            .with(fill(IntRect::new(0, 100, 100, 100)), viewport);
        Page {
            tree,
            list: list.storage(),
        }
    }

    // More translucent groups nested in each other than 1024, around a canvas, so that the compositor composites each.
    const NESTED_GROUP_COUNT_OF_DEEP_PAGE: usize = 1100;

    fn page_with_canvas_in_deeply_nested_groups() -> Page {
        let (mut tree, _, contexts) = tree_with_document_scroller(&[]);
        let document = contexts[0];
        let mut group = document;
        for _ in 0..NESTED_GROUP_COUNT_OF_DEEP_PAGE {
            group = tree.append_effect_node_under(group, effects(0.5));
        }
        let list = ListOfOneChunk::default()
            .with(fill(IntRect::new(0, 0, 800, 2600)), document)
            .with(canvas(IntRect::new(100, 100, 200, 100), 1), group)
            .with(fill(IntRect::new(0, 500, 800, 50)), document);
        Page {
            tree,
            list: list.storage(),
        }
    }

    fn every_page() -> Vec<(&'static str, Page)> {
        vec![
            ("plain page", plain_page()),
            ("two canvases", page_with_two_canvases()),
            ("canvas in nested scroller", page_with_canvas_in_nested_scroller()),
            ("sticky header", page_with_sticky_header()),
            ("nested composited effects", page_with_nested_composited_effects()),
            ("spinner", page_with_spinner()),
            (
                "spinner holding the root group",
                page_with_spinner_holding_the_root_group(),
            ),
            ("translucent root", page_under_translucent_root()),
            ("no records", page_without_records()),
            ("records that draw nothing", page_whose_records_draw_nothing()),
            ("frosted glass", page_with_frosted_glass()),
        ]
    }

    // The plan WebContent sends of what it planned.
    fn plan_sent_of(planned: &PlannedLayers) -> LayerPlan {
        LayerPlan {
            display_list_id: 42,
            viewport_rect: VIEWPORT,
            stack: planned.stack.clone(),
            note_lines: planned.note_lines_for_layers_overlay(),
        }
    }

    fn encoded(plan: &LayerPlan) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.encode(plan).expect("a planned stack encodes");
        encoder.finish().0
    }

    fn decoded_and_validated_plan(page: &Page, bytes: &[u8]) -> Result<LayerPlan, &'static str> {
        let mut decoder = Decoder::new(bytes, []);
        let plan: UnvalidatedLayerPlan = decoder.decode().map_err(|_| "the plan does not decode")?;
        if !decoder.is_at_end() {
            return Err("bytes follow the plan");
        }
        plan.validated(&page.list, &page.tree)
    }

    fn kinds_of_anchors(stack: &LayerStack) -> Vec<&'static str> {
        stack
            .spaces
            .iter()
            .map(|space| match space.anchor_kind {
                RasterSpaceAnchorKind::Viewport => "viewport",
                RasterSpaceAnchorKind::ViewportScroller => "document",
                RasterSpaceAnchorKind::Scroller => "scroller",
                RasterSpaceAnchorKind::StickyBox { .. } => "sticky",
                RasterSpaceAnchorKind::AnimatedTransform { .. } => "animated",
            })
            .collect()
    }

    fn media_layer_count(stack: &LayerStack) -> usize {
        stack
            .layers
            .iter()
            .filter(|layer| matches!(layer.content, LayerContent::Media { .. }))
            .count()
    }

    #[test]
    fn the_pages_plan_the_stacks_the_tests_are_about() {
        let stack_of = |page: Page| page.planned().stack;
        let plain = stack_of(plain_page());
        assert_eq!(kinds_of_anchors(&plain), ["viewport", "document"]);
        assert!(plain.why_list_is_one_layer.is_none());
        assert_eq!(media_layer_count(&stack_of(page_with_two_canvases())), 2);
        let nested = stack_of(page_with_canvas_in_nested_scroller());
        assert_eq!(kinds_of_anchors(&nested), ["viewport", "document", "scroller"]);
        assert_eq!(media_layer_count(&nested), 1);
        assert_eq!(
            kinds_of_anchors(&stack_of(page_with_sticky_header())),
            ["viewport", "document", "sticky"]
        );
        let composited = stack_of(page_with_nested_composited_effects());
        assert_eq!(composited.composited_effects.len(), 2);
        assert_eq!(
            kinds_of_anchors(&stack_of(page_with_spinner())),
            ["viewport", "animated"]
        );
        let root_page = page_with_spinner_holding_the_root_group();
        let root_effect = root_page.tree.root_isolation_effect;
        let composited_effects_of_root_page: Vec<EffectNodeIndex> = stack_of(root_page)
            .composited_effects
            .iter()
            .map(|composited| composited.effect)
            .collect();
        assert_eq!(composited_effects_of_root_page, Vec::from_iter(root_effect));
        assert_eq!(
            stack_of(page_under_translucent_root()).why_list_is_one_layer,
            Some(WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup)
        );
        assert_eq!(
            stack_of(page_without_records()).why_list_is_one_layer,
            Some(WhyListIsOneLayer::EmptyDisplayList)
        );
        let nothing_drawn = stack_of(page_whose_records_draw_nothing());
        assert!(nothing_drawn.why_list_is_one_layer.is_none());
        assert_eq!(nothing_drawn.bands.len(), 1);
        assert!(nothing_drawn.bands[0].record_spans.is_empty() && nothing_drawn.bands[0].cuts.is_empty());
        let crowded = stack_of(page_with_many_nested_scrollers());
        assert_eq!(kinds_of_anchors(&crowded)[2..], ["scroller"; 6]);
        let nested_scrollers_without_spaces = NESTED_SCROLLER_COUNT_OF_CROWDED_PAGE as usize - 6;
        assert!(crowded.spaces.len() + nested_scrollers_without_spaces > MAXIMUM_RASTER_SPACE_COUNT);
        let nine_canvases = stack_of(page_with_nine_canvases());
        assert_eq!(nine_canvases.bands.len(), 1);
        assert_eq!(media_layer_count(&nine_canvases), MAXIMUM_MEDIA_LAYER_COUNT);
        let siblings = stack_of(page_with_two_fading_siblings());
        let parents_of_composited_nodes: Vec<Option<PlacementNodeIndex>> = siblings
            .composited_effects
            .iter()
            .map(|composited| siblings.nodes[composited.placement_node_index.0].parent_index)
            .collect();
        assert_eq!(parents_of_composited_nodes.len(), 2);
        assert_eq!(parents_of_composited_nodes[0], parents_of_composited_nodes[1]);
        let frosted = stack_of(page_with_frosted_glass());
        let reads_of_composited_effects: Vec<bool> = frosted
            .composited_effects
            .iter()
            .map(|composited| composited.reads_what_is_composited_below_it)
            .collect();
        assert_eq!(reads_of_composited_effects, [true]);
        // The spinner is rastered at the scale of its group, far past four times the scale of the viewport.
        let large_scale = stack_of(page_with_spinner_in_large_scale());
        assert!(matches!(
            large_scale.spaces[1..],
            [RasterSpace {
                anchor_kind: RasterSpaceAnchorKind::AnimatedTransform { raster_scale },
                ..
            }] if raster_scale > 64.0
        ));
    }

    #[test]
    fn a_planned_stack_round_trips_through_its_wire_form_and_its_checks() {
        let pages = every_page()
            .into_iter()
            .chain([("spinner in large scale", page_with_spinner_in_large_scale())]);
        for (name, page) in pages {
            let sent = plan_sent_of(&page.planned());
            let plan = decoded_and_validated_plan(&page, &encoded(&sent))
                .unwrap_or_else(|why| panic!("the plan of the {name} is rejected: {why}"));
            assert_eq!(plan, sent, "the plan of the {name} comes back otherwise");
        }
    }

    #[test]
    fn the_compositor_works_out_what_a_plan_on_the_wire_leaves_out() {
        for page in [
            page_with_sticky_header(),
            page_with_frosted_glass(),
            page_under_translucent_root(),
        ] {
            let planned = page.planned();
            let mut stack = planned.stack.clone();
            for space in &mut stack.spaces {
                space.identity ^= 1;
                space.extent = IntRect::new(1, 2, 3, 4);
            }
            for composited in &mut stack.composited_effects {
                composited.reads_what_is_composited_below_it ^= true;
                composited.filter_spreads_what_it_composites ^= true;
            }
            stack.layering_obstacles.sorts_planes_by_depth ^= true;
            assert_eq!(page.validated(stack), Ok(planned.stack));
        }
    }

    #[test]
    fn a_stack_made_for_a_frame_not_drawn_as_planned_round_trips() {
        let page = page_with_two_canvases();
        let stack = LayerStack::single_layer(
            VIEWPORT,
            page.list.whole_span(),
            Some(WhyListIsOneLayer::FrameIsNotDrawnAsPlanned),
        );
        assert_eq!(page.validated(stack.clone()), Ok(stack));
    }

    #[test]
    fn a_plan_with_nodes_nested_deeper_than_1024_round_trips() {
        let page = page_with_canvas_in_deeply_nested_groups();
        let planned = page.planned();
        let stack = &planned.stack;
        let depth_of_deepest_node = (0..stack.nodes.len())
            .map(|node_index| {
                std::iter::successors(Some(PlacementNodeIndex(node_index)), |&index| {
                    stack.nodes[index.0].parent_index
                })
                .count()
            })
            .max()
            .unwrap_or(0);
        assert!(depth_of_deepest_node > NESTED_GROUP_COUNT_OF_DEEP_PAGE);
        let sent = plan_sent_of(&planned);
        assert_eq!(decoded_and_validated_plan(&page, &encoded(&sent)), Ok(sent));
    }

    #[test]
    fn a_plan_for_a_viewport_past_the_coordinate_limit_is_refused() {
        let viewport_past_limit = IntRect::new(0, 0, COORDINATE_LIMIT_IN_DEVICE_PIXELS + 1, 600);
        for page in [plain_page(), page_under_translucent_root()] {
            let stack = page.planned().stack;
            assert!(LayerStack::validated(on_wire(&stack), &page.list, &page.tree, VIEWPORT).is_ok());
            assert!(LayerStack::validated(on_wire(&stack), &page.list, &page.tree, viewport_past_limit).is_err());
        }
    }

    // The overlay of layers showed these lines of the plans of these pages before the plan carried them as text.
    #[test]
    fn the_note_lines_of_a_plan_are_the_lines_the_overlay_of_layers_showed() {
        let lines_of = |page: Page| page.planned().note_lines_for_layers_overlay();
        assert_eq!(
            lines_of(page_with_nested_composited_effects()),
            [
                "composited e1 space=document: the compositor animates it",
                "composited e2 space=document inside e1: the compositor animates it",
            ]
        );
        assert_eq!(
            lines_of(page_with_spinner_holding_the_root_group()),
            ["composited e0 space=animated@s1 in viewport: it applies over a transform the compositor animates"]
        );
        assert_eq!(
            lines_of(page_with_nine_canvases()),
            ["no layer for canvas: past the most media that get layers"]
        );
        assert_eq!(
            lines_of(page_with_two_fading_siblings()),
            [
                "composited e1 space=document: the compositor animates it",
                "composited e2 space=document: the compositor animates it",
            ]
        );
        assert_eq!(
            lines_of(page_with_frosted_glass()),
            [
                "composited e2 space=document reads=[73,73 254x154]: it reads what its own layer does not hold",
                "e1 is not composited: it reads only its own layer",
            ]
        );
        assert!(lines_of(page_under_translucent_root()).is_empty());
        assert!(lines_of(plain_page()).is_empty());
    }

    #[test]
    fn note_lines_past_what_a_plan_holds_are_cut() {
        // The last character that fits is cut in two at the length a line holds, so the line ends before it.
        let long_line = format!("a{}", "é".repeat(MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN));
        let lines = note_lines_a_plan_holds(std::iter::repeat_n(long_line, MAXIMUM_NOTE_LINE_COUNT_OF_PLAN + 1));
        assert_eq!(lines.len(), MAXIMUM_NOTE_LINE_COUNT_OF_PLAN);
        assert!(
            lines
                .iter()
                .all(|line| line.len() == MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN - 1)
        );
        let page = plain_page();
        let sent = LayerPlan {
            note_lines: lines,
            ..plan_sent_of(&page.planned())
        };
        assert_eq!(decoded_and_validated_plan(&page, &encoded(&sent)), Ok(sent));
    }

    #[test]
    fn every_reason_and_kind_round_trips() {
        fn round_trip<T: Encode + Decode + PartialEq + std::fmt::Debug>(value: T) {
            let mut encoder = Encoder::new();
            encoder.encode(&value).unwrap();
            let bytes = encoder.finish().0;
            let mut decoder = Decoder::new(&bytes, []);
            assert_eq!(decoder.decode::<T>().unwrap(), value);
            assert!(decoder.is_at_end());
        }
        for why in [
            WhyCompositorCannotCompositeEffect::FilterMovesWhatItFilters,
            WhyCompositorCannotCompositeEffect::FilterReadsFartherThanTilesAreFollowed,
        ] {
            round_trip(why);
            round_trip(WhyListIsOneLayer::EffectTheCompositorCannotCompositeIsComposited(why));
        }
        for why in [
            WhyListIsOneLayer::EmptyViewport,
            WhyListIsOneLayer::EmptyDisplayList,
            WhyListIsOneLayer::SortingContext3D,
            WhyListIsOneLayer::RootIsolationEffectIsNotPlainGroup,
            WhyListIsOneLayer::MaskDeclaredUnderEffectsItsMaskIsNotUnder,
            WhyListIsOneLayer::RecordsBlendWithWhatOtherLayersHoldBelowThem,
            WhyListIsOneLayer::FrameIsNotDrawnAsPlanned,
        ] {
            round_trip(why);
        }
        for content in [
            LiveContent::Canvas(CanvasId(1)),
            LiveContent::VideoFrame(VideoSinkHandle(2)),
            LiveContent::ImageFrame(ImageFrameResourceId(3)),
            LiveContent::CompositedContext(CompositorContextId(4)),
        ] {
            round_trip(content);
        }
        for reach in [Reach::Nothing, Reach::Within(IntRect::new(1, 2, 3, 4)), Reach::Anywhere] {
            round_trip(reach);
        }
    }

    #[test]
    fn a_count_or_a_length_past_its_maximum_is_refused_before_anything_is_allocated() {
        let mut encoder = Encoder::new();
        encoder.encode(&PLAN_KIND_LAYERED).unwrap();
        encoder.encode_size(MAXIMUM_NODE_COUNT_OF_RECEIVED_STACK + 1).unwrap();
        let bytes = encoder.finish().0;
        assert!(matches!(
            Decoder::new(&bytes, []).decode::<UnvalidatedLayerStack>(),
            Err(DecodeError::SizeTooLarge(_))
        ));
        let page = page_with_two_canvases();
        let plan_with_note_lines = |note_lines: Vec<String>| {
            encoded(&LayerPlan {
                note_lines,
                ..plan_sent_of(&page.planned())
            })
        };
        let too_many_lines = vec![String::new(); MAXIMUM_NOTE_LINE_COUNT_OF_PLAN + 1];
        let too_long_line = vec!["x".repeat(MAXIMUM_NOTE_LINE_LENGTH_OF_PLAN + 1)];
        for bytes in [
            plan_with_note_lines(too_many_lines),
            plan_with_note_lines(too_long_line),
        ] {
            assert!(matches!(
                Decoder::new(&bytes, []).decode::<UnvalidatedLayerPlan>(),
                Err(DecodeError::SizeTooLarge(_))
            ));
        }
        // A count of many spans that the bytes cannot hold is refused too.
        let mut bytes = encoded(&plan_sent_of(&page.planned()));
        bytes.truncate(bytes.len() / 2);
        assert!(decoded_and_validated_plan(&page, &bytes).is_err());
    }

    fn media_layer_index(stack: &LayerStack) -> usize {
        stack
            .layers
            .iter()
            .position(|layer| matches!(layer.content, LayerContent::Media { .. }))
            .expect("the stack has a media layer")
    }

    fn band_with_cuts(stack: &LayerStack) -> usize {
        stack
            .bands
            .iter()
            .position(|band| band.cuts.len() >= 2)
            .expect("the stack has a band with two cuts")
    }

    fn band_with_spans(stack: &LayerStack) -> usize {
        stack
            .bands
            .iter()
            .position(|band| !band.record_spans.is_empty())
            .expect("the stack has a band with spans")
    }

    fn band_with_parent(stack: &LayerStack) -> usize {
        stack
            .bands
            .iter()
            .position(|band| band.parent_band_holding_this_band_as_barrier.is_some())
            .expect("the stack has a nested band")
    }

    fn composited_node_index(stack: &LayerStack) -> usize {
        stack
            .nodes
            .iter()
            .position(|node| node.composited_effect.is_some())
            .expect("the stack has a composited node")
    }

    fn layer_above_a_cut(stack: &LayerStack) -> usize {
        stack
            .bands
            .iter()
            .find_map(|band| band.cuts.first())
            .expect("the stack has a cut")
            .layer_above_cut
            .0
    }

    fn band_of_node(stack: &LayerStack, node: PlacementNodeIndex) -> usize {
        stack
            .bands
            .iter()
            .position(|band| band.placement_node_index == node)
            .expect("the node has a band")
    }

    fn layer_above_nested_band(stack: &LayerStack) -> usize {
        stack
            .layers
            .iter()
            .position(|layer| {
                matches!(
                    layer.identity,
                    LayerIdentity::Painted {
                        above_barrier: Some(BarrierIdentity::NestedNodeBand { .. }),
                        ..
                    }
                )
            })
            .expect("a layer sits above a nested band")
    }

    // Gives the last canvas of the page with nine canvases a layer as a plan gives a canvas one, past the most a plan
    // gives.
    fn give_last_of_nine_canvases_a_layer(stack: &mut LayerStack) {
        let first_media = media_layer_index(stack);
        let LayerContent::Media {
            record_span: span_of_first_canvas,
            reach,
            ..
        } = stack.layers[first_media].content
        else {
            unreachable!()
        };
        let band_index = stack.layers[first_media].band_index;
        let band = &mut stack.bands[band_index.0];
        let span_holding_last_canvas = band.record_spans.last_mut().expect("the band holds the last canvas");
        let canvas_size = span_of_first_canvas.end.offset - span_of_first_canvas.begin.offset;
        let span_of_last_canvas = ListSpan {
            begin: ListPosition::new(
                span_holding_last_canvas.end.chunk_index,
                span_holding_last_canvas.end.offset - canvas_size,
            ),
            end: span_holding_last_canvas.end,
        };
        span_holding_last_canvas.end = span_of_last_canvas.begin;
        let (node_index, band_ordinal_in_node) = (band.placement_node_index, band.band_ordinal_in_node);
        let content = LiveContent::Canvas(CanvasId(9));
        stack.layers.push(StackLayer {
            identity: LayerIdentity::Media { content, ordinal: 0 },
            placement_node_index: node_index,
            band_index,
            content: LayerContent::Media {
                content,
                record_span: span_of_last_canvas,
                reach,
            },
            may_hold_content: true,
            reach,
        });
        stack.layers.push(StackLayer {
            identity: LayerIdentity::Painted {
                node: stack.nodes[node_index.0].identity,
                band_ordinal_in_node,
                above_barrier: Some(BarrierIdentity::Media { content, ordinal: 0 }),
            },
            placement_node_index: node_index,
            band_index,
            content: LayerContent::Painted { band_index },
            may_hold_content: false,
            reach: IntRect::default(),
        });
        let layer_above_cut = LayerIndex(stack.layers.len() - 1);
        stack.bands[band_index.0].cuts.push(BarrierCut {
            barrier_first_record_position: span_of_last_canvas.begin,
            cut_reach: Reach::Within(reach),
            layer_above_cut,
        });
    }

    // Moves the records of the band of the fixed header of the plain page into a band of a node of the viewport's space
    // that no barrier names.
    fn move_header_into_band_no_barrier_names(stack: &mut LayerStack) {
        let header_span = stack.bands[0]
            .record_spans
            .pop()
            .expect("the first band holds the header");
        let node_identity = PlacementNodeIdentity(77);
        stack.nodes.push(PlacementNode {
            identity: node_identity,
            raster_space_index: RasterSpaceIndex::VIEWPORT,
            composite_clips: vec![CompositeClip {
                raster_space_index: RasterSpaceIndex::VIEWPORT,
                rect: IntRect::new(0, 0, 800, 50),
                corner_radii: libgfx_rust::CornerRadii::default(),
            }],
            composited_effect: None,
            parent_index: None,
            reach_in_parent_node: Reach::Anywhere,
        });
        let node_index = PlacementNodeIndex(stack.nodes.len() - 1);
        let band_index = BandIndex(stack.bands.len());
        stack.bands.push(PlacementNodeBand {
            placement_node_index: node_index,
            band_ordinal_in_node: 0,
            first_record_position: header_span.begin,
            parent_band_holding_this_band_as_barrier: None,
            record_spans: vec![header_span],
            cuts: Vec::new(),
            layer_below_all_cuts: LayerIndex(stack.layers.len()),
        });
        stack.layers.push(StackLayer {
            identity: LayerIdentity::Painted {
                node: node_identity,
                band_ordinal_in_node: 0,
                above_barrier: None,
            },
            placement_node_index: node_index,
            band_index,
            content: LayerContent::Painted { band_index },
            may_hold_content: true,
            reach: IntRect::new(0, 0, 800, 50),
        });
    }

    type Mutation = fn(&mut LayerStack);

    // The need a mutation breaks, the page whose stack it mutates, and the mutation.
    type Rejection = (&'static str, fn() -> Page, Mutation);

    fn rejections() -> Vec<Rejection> {
        vec![
            (
                "1: layer below all cuts out of range",
                page_with_two_canvases,
                |stack| stack.bands[0].layer_below_all_cuts = LayerIndex(stack.layers.len()),
            ),
            ("1: layer above a cut out of range", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts[0].layer_above_cut = LayerIndex(stack.layers.len() + 7);
            }),
            ("2: node of a layer out of range", page_with_two_canvases, |stack| {
                stack.layers[0].placement_node_index = PlacementNodeIndex(stack.nodes.len());
            }),
            ("2: node of a band out of range", page_with_two_canvases, |stack| {
                stack.bands[0].placement_node_index = PlacementNodeIndex(stack.nodes.len());
            }),
            ("3: band of a layer out of range", page_with_two_canvases, |stack| {
                stack.layers[0].band_index = BandIndex(stack.bands.len());
            }),
            (
                "3: band a painted layer holds out of range",
                page_with_two_canvases,
                |stack| {
                    stack.layers[0].content = LayerContent::Painted {
                        band_index: BandIndex(stack.bands.len()),
                    }
                },
            ),
            ("3: parent band out of range", page_with_two_canvases, |stack| {
                let band = band_with_parent(stack);
                stack.bands[band].parent_band_holding_this_band_as_barrier = Some(BandIndex(usize::MAX));
            }),
            (
                "3: band that holds itself as a barrier",
                page_with_two_canvases,
                |stack| {
                    let band = band_with_parent(stack);
                    stack.bands[band].parent_band_holding_this_band_as_barrier = Some(BandIndex(band));
                },
            ),
            ("4: space of a node out of range", page_with_two_canvases, |stack| {
                stack.nodes[0].raster_space_index = RasterSpaceIndex(stack.spaces.len());
            }),
            (
                "4: space of a composite clip out of range",
                page_with_two_canvases,
                |stack| {
                    stack.nodes[0].composite_clips.push(CompositeClip {
                        raster_space_index: RasterSpaceIndex(stack.spaces.len()),
                        rect: IntRect::new(0, 0, 10, 10),
                        corner_radii: libgfx_rust::CornerRadii::default(),
                    });
                },
            ),
            ("6: node in itself", page_with_two_canvases, |stack| {
                stack.nodes[1].parent_index = Some(PlacementNodeIndex(1));
            }),
            (
                "6: composited node in no node",
                page_with_nested_composited_effects,
                |stack| {
                    let node = composited_node_index(stack);
                    stack.nodes[node].parent_index = None;
                },
            ),
            (
                "7: composited effect out of range",
                page_with_nested_composited_effects,
                |stack| {
                    let node = composited_node_index(stack);
                    stack.nodes[node].composited_effect = Some(EffectNodeIndex(9999));
                },
            ),
            (
                "9: composited effects nested otherwise than in the tree",
                page_with_nested_composited_effects,
                |stack| {
                    let (first, second) = (stack.composited_effects[0], stack.composited_effects[1]);
                    stack.nodes[first.placement_node_index.0].composited_effect = Some(second.effect);
                    stack.nodes[second.placement_node_index.0].composited_effect = Some(first.effect);
                },
            ),
            (
                "10: media layer at a run that draws a fill",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let LayerContent::Media { record_span, .. } = &mut stack.layers[media].content else {
                        unreachable!()
                    };
                    *record_span = ListSpan {
                        begin: ListPosition::default(),
                        end: record_span.end,
                    };
                },
            ),
            (
                "10: media layer that names another canvas",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let LayerContent::Media { content, .. } = &mut stack.layers[media].content else {
                        unreachable!()
                    };
                    *content = LiveContent::Canvas(CanvasId(77));
                    stack.layers[media].identity = LayerIdentity::Media {
                        content: LiveContent::Canvas(CanvasId(77)),
                        ordinal: 0,
                    };
                },
            ),
            ("10: media layer of an image", page_with_two_canvases, |stack| {
                let media = media_layer_index(stack);
                let image = LiveContent::ImageFrame(ImageFrameResourceId(1));
                let LayerContent::Media { content, .. } = &mut stack.layers[media].content else {
                    unreachable!()
                };
                *content = image;
                stack.layers[media].identity = LayerIdentity::Media {
                    content: image,
                    ordinal: 0,
                };
            }),
            ("11: span that starts inside a run", page_with_two_canvases, |stack| {
                let band = band_with_spans(stack);
                stack.bands[band].record_spans[0].begin.offset += 1;
            }),
            ("11: span in a chunk the list does not hold", plain_page, |stack| {
                let band = band_with_spans(stack);
                stack.bands[band].record_spans[0].end.chunk_index = ChunkIndexInList(999);
            }),
            ("11: span that ends before it begins", plain_page, |stack| {
                let band = band_with_spans(stack);
                let span = &mut stack.bands[band].record_spans[0];
                std::mem::swap(&mut span.begin, &mut span.end);
            }),
            ("11: band that starts inside a run", page_with_two_canvases, |stack| {
                stack.bands[0].first_record_position.offset += 4;
            }),
            ("11: cut that starts inside a run", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts[0].barrier_first_record_position.offset += 4;
            }),
            ("12: spans out of paint order", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].record_spans.reverse();
            }),
            ("12: cuts out of paint order", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts.reverse();
            }),
            ("13: layer named by two cuts", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts[1].layer_above_cut = stack.bands[band].cuts[0].layer_above_cut;
            }),
            ("13: layer of no band", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts[1].layer_above_cut = stack.bands[band].layer_below_all_cuts;
            }),
            (
                "13: media layer of another band than its barrier",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let other_band = (stack.layers[media].band_index.0 + 1) % stack.bands.len();
                    stack.layers[media].band_index = BandIndex(other_band);
                    stack.layers[media].placement_node_index = stack.bands[other_band].placement_node_index;
                },
            ),
            (
                "13: painted layer of another node than its band",
                page_with_two_canvases,
                |stack| stack.layers[0].placement_node_index = PlacementNodeIndex(stack.nodes.len() - 1),
            ),
            ("14: media layer above a cut", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                let media = media_layer_index(stack);
                stack.bands[band].cuts[0].layer_above_cut = LayerIndex(media);
            }),
            (
                "15: media layer with a painted identity",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    stack.layers[media].identity = LayerIdentity::Painted {
                        node: PlacementNodeIdentity(5),
                        band_ordinal_in_node: 9,
                        above_barrier: None,
                    }
                },
            ),
            (
                "15: painted layer with media content",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let content = stack.layers[media].content.clone();
                    let above = layer_above_a_cut(stack);
                    stack.layers[above].content = content;
                },
            ),
            (
                "16: barrier that names no media layer",
                page_with_two_canvases,
                |stack| {
                    let above = layer_above_a_cut(stack);
                    let LayerIdentity::Painted { above_barrier, .. } = &mut stack.layers[above].identity else {
                        unreachable!()
                    };
                    *above_barrier = Some(BarrierIdentity::Media {
                        content: LiveContent::Canvas(CanvasId(1)),
                        ordinal: 99,
                    });
                },
            ),
            (
                "16: barrier that names no node",
                page_with_canvas_in_nested_scroller,
                |stack| {
                    let nested_band_barrier = stack
                        .layers
                        .iter()
                        .position(|layer| {
                            matches!(
                                layer.identity,
                                LayerIdentity::Painted {
                                    above_barrier: Some(BarrierIdentity::NestedNodeBand { .. }),
                                    ..
                                }
                            )
                        })
                        .expect("a layer sits above a nested band");
                    let LayerIdentity::Painted { above_barrier, .. } = &mut stack.layers[nested_band_barrier].identity
                    else {
                        unreachable!()
                    };
                    *above_barrier = Some(BarrierIdentity::NestedNodeBand {
                        node: PlacementNodeIdentity(12345),
                        band_ordinal_in_node: 0,
                    });
                },
            ),
            ("17: two layers of one identity", page_with_two_canvases, |stack| {
                let above = layer_above_a_cut(stack);
                stack.layers[above].identity = stack.layers[0].identity;
            }),
            (
                "17: two composited nodes of one identity",
                page_with_nested_composited_effects,
                |stack| {
                    let first = stack.composited_effects[0].placement_node_index.0;
                    let second = stack.composited_effects[1].placement_node_index.0;
                    stack.nodes[second].identity = stack.nodes[first].identity;
                },
            ),
            ("18: base layer out of range", page_with_two_canvases, |stack| {
                stack.base_layer = LayerIndex(stack.layers.len());
            }),
            ("18: base layer above a cut", page_with_two_canvases, |stack| {
                stack.base_layer = LayerIndex(layer_above_a_cut(stack));
            }),
            (
                "18: base layer in a space the compositor animates",
                page_with_spinner,
                |stack| {
                    let band_of_spinner = stack
                        .bands
                        .iter()
                        .position(|band| stack.nodes[band.placement_node_index.0].raster_space_index.0 == 1)
                        .expect("the spinner has a band");
                    stack.base_layer = stack.bands[band_of_spinner].layer_below_all_cuts;
                },
            ),
            ("19: reach of a layer past the limit", page_with_two_canvases, |stack| {
                stack.layers[0].reach = IntRect::new(i32::MAX - 10, 0, 100, 100);
            }),
            ("19: rect of negative size", page_with_two_canvases, |stack| {
                stack.layers[0].reach = IntRect::new(0, 0, -10, 100);
            }),
            ("19: reach of a node past the limit", page_with_two_canvases, |stack| {
                stack.nodes[0].reach_in_parent_node = Reach::Within(IntRect::new(i32::MIN, i32::MIN, 10, 10));
            }),
            ("19: reach of a cut past the limit", page_with_two_canvases, |stack| {
                let band = band_with_cuts(stack);
                stack.bands[band].cuts[0].cut_reach = Reach::Within(IntRect::new(0, 0, i32::MAX, 1));
            }),
            (
                "19: negative radius of a composite clip",
                page_with_two_canvases,
                |stack| {
                    stack.nodes[1].composite_clips.push(CompositeClip {
                        raster_space_index: RasterSpaceIndex(0),
                        rect: IntRect::new(0, 0, 10, 10),
                        corner_radii: libgfx_rust::CornerRadii::uniform(-5),
                    });
                },
            ),
            ("19: swept reach past the limit", page_with_spinner, |stack| {
                stack.swept_reach_in_parent_space_by_space_index[1] = Some(IntRect::new(1 << 30, 0, 1, 1));
            }),
            ("20: a value per space missing", page_with_two_canvases, |stack| {
                stack.tiles_hold_content_moving_within_space_by_space_index.pop();
            }),
            (
                "20: a swept reach per space too many",
                page_with_two_canvases,
                |stack| stack.swept_reach_in_parent_space_by_space_index.push(None),
            ),
            (
                "20: a swept reach of a space the compositor does not animate",
                page_with_two_canvases,
                |stack| stack.swept_reach_in_parent_space_by_space_index[1] = Some(IntRect::new(0, 0, 1, 1)),
            ),
            ("22: record held by two bands", plain_page, |stack| {
                let span_of_document = stack.bands[1].record_spans[0];
                stack.bands[0].record_spans.insert(0, span_of_document);
            }),
            (
                "22: record held by a band and by a media layer",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let LayerContent::Media { record_span, .. } = stack.layers[media].content else {
                        unreachable!()
                    };
                    let band = &mut stack.bands[stack.layers[media].band_index.0];
                    let position = band.record_spans.partition_point(|span| span.end <= record_span.begin);
                    band.record_spans.insert(position, record_span);
                },
            ),
            ("22: span that holds no record", page_with_two_canvases, |stack| {
                let band = band_with_spans(stack);
                let end_of_first_span = stack.bands[band].record_spans[0].end;
                stack.bands[band].record_spans.insert(
                    1,
                    ListSpan {
                        begin: end_of_first_span,
                        end: end_of_first_span,
                    },
                );
            }),
            (
                "23: band other than the first that holds nothing",
                plain_page,
                |stack| {
                    stack.bands[1].record_spans.clear();
                },
            ),
            (
                "24: more media layers than a plan makes",
                page_with_nine_canvases,
                give_last_of_nine_canvases_a_layer,
            ),
            (
                "26: two nodes composited inside one effect",
                page_with_two_fading_siblings,
                |stack| {
                    let first_effect = stack.composited_effects[0].effect;
                    let second_node = stack.composited_effects[1].placement_node_index;
                    stack.nodes[second_node.0].composited_effect = Some(first_effect);
                },
            ),
            ("27: node in a node of its own space", plain_page, |stack| {
                stack.nodes[1].raster_space_index = RasterSpaceIndex::VIEWPORT;
            }),
            (
                "27: composited node in a node of another space",
                page_with_nested_composited_effects,
                |stack| {
                    let node = composited_node_index(stack);
                    stack.nodes[node].raster_space_index = RasterSpaceIndex::VIEWPORT;
                },
            ),
            (
                "28: media layer after the layer above its cut",
                page_with_two_canvases,
                |stack| {
                    let media = media_layer_index(stack);
                    let band = stack.layers[media].band_index.0;
                    let cut = stack.bands[band]
                        .cuts
                        .iter()
                        .position(|cut| cut.layer_above_cut == LayerIndex(media + 1))
                        .expect("the layer above the cut of a media layer is next to it");
                    stack.layers.swap(media, media + 1);
                    stack.bands[band].cuts[cut].layer_above_cut = LayerIndex(media);
                },
            ),
            (
                "28: bands in a band out of the order of their cuts",
                page_with_two_fading_siblings,
                |stack| {
                    let first = band_of_node(stack, stack.composited_effects[0].placement_node_index);
                    let second = band_of_node(stack, stack.composited_effects[1].placement_node_index);
                    let swapped = |band: BandIndex| match band.0 {
                        band_index if band_index == first => BandIndex(second),
                        band_index if band_index == second => BandIndex(first),
                        _ => band,
                    };
                    stack.bands.swap(first, second);
                    for layer in &mut stack.layers {
                        layer.band_index = swapped(layer.band_index);
                        if let LayerContent::Painted { band_index } = &mut layer.content {
                            *band_index = swapped(*band_index);
                        }
                    }
                },
            ),
            (
                "28: barrier that names no band",
                page_with_two_fading_siblings,
                |stack| {
                    let above = layer_above_nested_band(stack);
                    let LayerIdentity::Painted {
                        above_barrier:
                            Some(BarrierIdentity::NestedNodeBand {
                                band_ordinal_in_node, ..
                            }),
                        ..
                    } = &mut stack.layers[above].identity
                    else {
                        unreachable!()
                    };
                    *band_ordinal_in_node = 99;
                },
            ),
            (
                "28: band that no barrier names",
                plain_page,
                move_header_into_band_no_barrier_names,
            ),
        ]
    }

    #[test]
    fn a_stack_that_a_consumer_cannot_read_safely_is_rejected() {
        for (need, page, mutate) in rejections() {
            let page = page();
            let mut stack = page.planned().stack;
            assert!(
                page.validated(stack.clone()).is_ok(),
                "{need}: the planned stack passes"
            );
            mutate(&mut stack);
            assert!(page.validated(stack).is_err(), "{need}: the mutated stack is rejected");
        }
    }

    #[test]
    fn a_plan_whose_bands_each_repeat_the_whole_list_is_refused() {
        for page in [plain_page(), page_with_canvas_in_nested_scroller()] {
            let planned = page.planned();
            let mut stack = planned.stack.clone();
            assert!(stack.bands.len() >= 2);
            for band in &mut stack.bands {
                band.record_spans = vec![page.list.whole_span()];
            }
            assert_eq!(
                page.validated(stack.clone()),
                Err("two spans of the stack hold the same record")
            );
            let sent = LayerPlan {
                stack,
                ..plan_sent_of(&planned)
            };
            assert!(decoded_and_validated_plan(&page, &encoded(&sent)).is_err());
        }
    }

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

    fn raster_settings_of_layer(_: LayerIndex) -> LayerRasterSettings {
        LayerRasterSettings {
            raster_space_identity: 0,
            clear_color: Color::TRANSPARENT,
            root_group_color: Color::TRANSPARENT,
            viewport_to_raster_transform: FloatMatrix4x4::identity(),
            allows_lcd_text: false,
        }
    }

    // Reads the stack as the compositor reads it for a frame over the whole viewport, and as the dump reads it.
    fn read_as_consumers_do(page: &Page, frame_tree: &VisualContextTree, planned: &PlannedLayers, stack: &LayerStack) {
        let list = &page.list;
        let scroll_offsets = [FloatPoint { x: 0.0, y: 300.0 }; 4];
        let composite_offsets = stack.composite_offsets(&scroll_offsets);
        let spaces_to_frame = stack.spaces_to_frame(&composite_offsets, |_| {
            libgfx_rust::affine_to_matrix(libgfx_rust::AffineTransform::new(0.5, 0.5, -0.5, 0.5, 10.0, 20.0))
        });
        for layer_index in (0..stack.layers.len()).map(LayerIndex) {
            stack.composited_bands_around_layer(layer_index);
            stack.composite_clip_of_layer_in_viewport(layer_index, &composite_offsets);
            stack.rounded_composite_clips_of_node_in_viewport(
                stack.layers[layer_index.0].placement_node_index,
                &composite_offsets,
            );
            stack.layer_is_in_animated_space(layer_index);
            stack.name_of_layer(layer_index, &NodeNamesFromIndices);
            let space_to_frame = spaces_to_frame[stack.space_of_layer(layer_index).0];
            space_to_frame.rect_in_frame(stack.layers[layer_index.0].reach);
            space_to_frame.rect_in_space(VIEWPORT);
        }
        for band_index in (0..stack.bands.len()).map(BandIndex) {
            stack.band_has_layer_at_or_above_base_layer(band_index);
        }
        stack.layers_the_window_server_shows(&composite_offsets, |_| true);
        let presentation = stack.window_server_presentation_of_media(list, frame_tree, &scroll_offsets, VIEWPORT);
        let planned_with_stack = PlannedLayers {
            stack: stack.clone(),
            notes: planned.notes.clone(),
            placement: planned.placement.clone(),
        };
        planned_with_stack.layer_lines(&presentation, &NodeNamesFromIndices, |layer| {
            Some(format!("{}", layer.0))
        });
        planned_with_stack.layer_lines(
            &WindowServerPresentationOfMedia::default(),
            &NodeNamesFromIndices,
            |_| None,
        );
        for run_index in 0..list.run_count() {
            planned_with_stack.layers_of_record(list.start_of_run(run_index));
        }
        planned_with_stack.note_lines_for_layers_overlay();
        for layer_index in 0..stack.layers.len() {
            stack.layer_with_identity(stack.layers[layer_index].identity);
            stack.space_holds_content_that_moves_within_it(stack.space_of_layer(LayerIndex(layer_index)));
        }

        let tile_keying_of_list = TileKeyingOfList::new(
            list,
            frame_tree,
            stack,
            FrameStateReadByReplay {
                scrollbars: &[],
                time_ns: 0,
                live_content: &NoLiveContentChanges,
            },
        );
        let raster_spaces = RasterSpaces::of_spaces(frame_tree, stack.spaces.clone());
        let geometries: Vec<RasterSpaceGeometry> = (0..stack.spaces.len())
            .map(|space_index| {
                RasterSpaceGeometry::new(
                    frame_tree,
                    &raster_spaces,
                    RasterSpaceIndex(space_index),
                    &scroll_offsets,
                    VIEWPORT,
                    FloatMatrix4x4::identity(),
                )
            })
            .collect();
        let tile_keying_by_space: Vec<TileKeyingOfRasterSpace<'_>> = geometries
            .iter()
            .map(|geometry| TileKeyingOfRasterSpace::new(&tile_keying_of_list, geometry))
            .collect();
        for (band_position, band) in stack.bands.iter().enumerate() {
            let band_index = BandIndex(band_position);
            if !stack.band_has_layer_at_or_above_base_layer(band_index) {
                continue;
            }
            let space_index = stack.nodes[band.placement_node_index.0].raster_space_index;
            let tile_keying = &tile_keying_by_space[space_index.0];
            let shown_in_viewport = stack
                .composite_clip_of_layer_in_viewport(band.layer_below_all_cuts, &composite_offsets)
                .map_or(VIEWPORT, |clip| VIEWPORT.intersected(clip));
            let shown_rect = spaces_to_frame[space_index.0].rect_in_space(shown_in_viewport);
            let mut band_tile_keys = BandTileKeys::default();
            for _ in 0..2 {
                tile_keying.bind_band(&mut band_tile_keys, stack, band_index);
                tile_keying.key_shown_tiles(
                    &mut band_tile_keys,
                    stack,
                    band_index,
                    raster_settings_of_layer,
                    shown_rect,
                    shown_rect,
                    Some(VIEWPORT),
                );
            }
            for tile in band_tile_keys.tiles_within(shown_rect) {
                assert!(tile.layer_index.0 < stack.layers.len());
                tile_keying.coverage_of_tile(&band_tile_keys, tile, tile.coord.cell_rect(), Color::TRANSPARENT);
                band_tile_keys.record_spans_of_tiles([tile]);
            }
        }
        for composited in &stack.composited_effects {
            let space_index = stack.nodes[composited.placement_node_index.0].raster_space_index;
            tile_keying_by_space[space_index.0].content_hash_of_composited_effect(composited.effect);
            crate::display_list::layers::backdrop_filter_read_reach(
                frame_tree,
                &geometries[space_index.0],
                composited.effect,
            );
        }
        for (layer_index, layer) in stack.layers.iter().enumerate() {
            if let LayerContent::Media { record_span, .. } = layer.content {
                tile_keying_by_space[stack.space_of_layer(LayerIndex(layer_index)).0]
                    .content_hash_of_records_within(record_span);
            }
        }
    }

    // A small generator of numbers that are far from random but alike from one run to the next.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    const BOUNDARY_VALUES: [u32; 12] = [
        0,
        1,
        2,
        3,
        u32::MAX,
        i32::MIN as u32,
        i32::MAX as u32,
        0x7fc0_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7149_f2ca,
        0x0080_0000,
    ];

    fn mutated(bytes: &[u8], generator: &mut Xorshift) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        for _ in 0..=generator.below(3) {
            if bytes.is_empty() {
                break;
            }
            match generator.below(5) {
                0 => {
                    let position = generator.below(bytes.len());
                    bytes[position] ^= 1 << generator.below(8);
                }
                1 if bytes.len() >= 4 => {
                    let position = generator.below(bytes.len() - 3);
                    let value = BOUNDARY_VALUES[generator.below(BOUNDARY_VALUES.len())];
                    bytes[position..position + 4].copy_from_slice(&value.to_ne_bytes());
                }
                2 => {
                    let (first, second) = (generator.below(bytes.len()), generator.below(bytes.len()));
                    bytes.swap(first, second);
                }
                3 => {
                    let position = generator.below(bytes.len());
                    bytes[position] = generator.next() as u8;
                }
                _ => bytes.truncate(generator.below(bytes.len())),
            }
        }
        bytes
    }

    #[test]
    fn a_mutated_plan_that_passes_its_checks_is_read_without_a_panic() {
        let mut generator = Xorshift(0x9e37_79b9_7f4a_7c15);
        let mut validated_count = 0;
        for (name, page) in every_page() {
            let planned = page.planned();
            let frame_tree = page.tree.with_visual_animation_samples(250_000_000);
            read_as_consumers_do(&page, &frame_tree, &planned, &planned.stack);
            let bytes = encoded(&plan_sent_of(&planned));
            for trial in 0..3000 {
                let mutated_bytes = mutated(&bytes, &mut generator);
                let Ok(plan) = decoded_and_validated_plan(&page, &mutated_bytes) else {
                    continue;
                };
                validated_count += 1;
                let reading = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_as_consumers_do(&page, &frame_tree, &planned, &plan.stack);
                }));
                assert!(reading.is_ok(), "trial {trial} of the {name} panics: {:?}", plan.stack);
            }
        }
        assert!(validated_count > 0, "some mutated plans pass their checks");
    }
}
