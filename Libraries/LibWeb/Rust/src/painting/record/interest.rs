/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! What a recording records of each scroll node the compositor scrolls. Around the scroll positions, a node aims for
//! what its scrollport shows, cut by what its parent records when it is placed exactly in its parent, grown by a
//! scrollport on each side it can scroll. What the recording published last aimed for is kept while it still holds what
//! the scrollport shows, so that scrolling a little records nothing new. A node culls by its aimed rect snapped to the
//! interest grid and unbounded where it reaches the end of its scroll range, which is also what the display list tells
//! the compositor it records. A committed frame aims for the rect the compositor requested instead, where what it
//! records for that rect holds what the scrollport shows, and also holds what the scrollport shows where a smooth
//! scroll the main thread asked for takes the node. A clock lane's frame or a refresh shows the scroll positions of the
//! committed frame it starts from, which the compositor may have scrolled on from, so it aims for the requested rect as
//! it is, and for what the display list it replaces on screen aimed for of a node the request does not name. A node
//! whose content reaches past the unbounded edges records all of it.
//!
//! What a recording reads of its frame to work this out holds while the frame's visual context tree, geometry and scroll
//! metadata do, so a later recording of such a frame works it out over the compositor's scroll nodes alone. The same
//! keeps telling the host whether the scroll positions the main thread moved its scroll nodes to show what the display
//! list the compositor holds leaves out.

use super::PaintRecorder;
use super::assemble::AssemblyCulling;
use super::cull::CullSpaces;
use super::inputs::InterestInput;
use super::order_tree::{CullReach, ScrollNodeSet};
use super::trace::Observer;
use crate::css::css_pixels::CssPixelPoint;
use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::display_list::commands::{ClipNodeIndex, ContextRef, SpatialNodeIndex};
use crate::painting::ffi::{FfiScrollNodeContentRect, FfiScrollNodeDestination};
use crate::painting::interest_refresh::InterestRequest;
use crate::painting::paint_read::PaintRead;
use crate::painting::visual_context::interest::{
    CullSpace, LeadPerSide, ScrollRange, default_interest_rect, interest_rect_covers, interest_rect_recorded_for,
    view_joined_with_destination, view_to_record_around,
};
use crate::painting::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
use crate::painting::visual_context::{ScrollData, SpatialData, VisualContextTree};
use libgfx_rust::{FloatPoint, IntRect, IntSize};
use std::sync::{Arc, Weak};

/// What a recording records of its document.
#[derive(Debug, Default)]
pub(crate) struct RecordedInterest {
    /// What the display list tells the compositor it records of each scroll node the compositor scrolls. Empty when it
    /// records the whole document.
    pub(crate) scroll_node_rects: Vec<FfiScrollNodeContentRect>,
    // Beside each of `scroll_node_rects`, what the node aimed for, in its content space at scroll offset zero, before
    // snapping and saturation. Empty when the node records none of its content.
    aimed_rects: Vec<IntRect>,
    /// Whether the display list leaves out per-box wheel targets, wheel regions, scroll chaining barriers or nested
    /// context regions outside the rects.
    pub(crate) leaves_out_wheel_routing: bool,
    /// The compositor's request to record around new rects that the recording answers: it records every rect the
    /// request asks for.
    pub(crate) answered_request_id: Option<u64>,
    // What the rects were worked out from, whose scroll nodes lie beside `scroll_node_rects`. None when the recording
    // left nothing out by interest.
    basis: Option<Arc<InterestBasis>>,
    // Whether a testing recording made it: later testing recordings keep only what such a recording aimed for.
    recorded_for_testing: bool,
}

/// What working out a recording's interest reads of its frame: which scroll nodes the compositor scrolls, where each lies
/// and how far it scrolls, and where content lands among them. It holds while the frame's visual context tree,
/// geometry and scroll metadata do, and the device pixel ratio and viewport size the recording records at.
#[derive(Debug)]
struct InterestBasis {
    tree: Weak<VisualContextTree>,
    geometry_epoch: u64,
    device_pixels_per_css_pixel: f64,
    device_viewport_size: IntSize,
    cull_spaces: Arc<CullSpaces>,
    cull_mapping: CullMapping,
    spatial_node_count: usize,
    // The scroll nodes the compositor scrolls, parents first, but for those recorded whole.
    scroll_nodes: Vec<ScrollNodeBasis>,
    // The scroll nodes the compositor scrolls whose content reaches past the unbounded edges, which a recording records
    // whole and tells the compositor nothing of. A node in one cuts nothing by its rect.
    scroll_nodes_recorded_whole: Vec<(SpatialNodeIndex, NodeSlotId)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollNodeBasis {
    scroll_node: SpatialNodeIndex,
    owner: NodeSlotId,
    state_slot: usize,
    // The node as the display list names it to the compositor. Its rect is unused.
    named: FfiScrollNodeContentRect,
    // In device pixels at scroll offset zero.
    scrollport: IntRect,
    range: ScrollRange,
    // The position in the scroll nodes of the one the node is placed exactly in, whose recorded rect cuts what the node
    // records. None for the root, and for a node that a transform, sticky offset, anchor shift or spreading filter moves
    // in its parent.
    placed_exactly_in: Option<usize>,
}

// What decides the cull space of content whose visual context did not change: which scroll nodes the compositor
// scrolls, and which effect nodes may spread content beyond its bounds. Content that moves to other spatial or effect
// nodes records again anyway.
#[derive(Debug, PartialEq)]
struct CullMapping {
    compositor_scroll_nodes: Vec<(SpatialNodeIndex, NodeSlotId)>,
    effects_under_spreading_filters: Vec<usize>,
}

impl ScrollNodeBasis {
    // What the node records around at `scroll_offset`, cut by `parent_recorded`, joined with what it shows where the
    // smooth scrolls among `smooth_scroll_destinations` take it, those near enough.
    fn view_heading_to_smooth_scroll_destinations(
        &self,
        scroll_offset: FloatPoint,
        parent_recorded: Option<IntRect>,
        smooth_scroll_destinations: &[FfiScrollNodeDestination],
    ) -> IntRect {
        let view_at =
            |scroll_offset| view_to_record_around(self.scrollport, scroll_offset, parent_recorded, self.range.axes);
        smooth_scroll_destinations
            .iter()
            .filter(|destination| destination.is_of(&self.named))
            .fold(view_at(scroll_offset), |view, destination| {
                view_joined_with_destination(
                    view,
                    view_at(destination.destination_offset),
                    size_of(self.scrollport),
                    self.range,
                )
            })
    }
}

impl InterestBasis {
    fn holds_for(
        &self,
        tree: &Arc<VisualContextTree>,
        geometry_epoch: u64,
        device_pixels_per_css_pixel: f64,
        device_viewport_size: IntSize,
    ) -> bool {
        self.geometry_epoch == geometry_epoch
            && self.device_pixels_per_css_pixel == device_pixels_per_css_pixel
            && self.device_viewport_size == device_viewport_size
            && self
                .tree
                .upgrade()
                .is_some_and(|basis_tree| Arc::ptr_eq(&basis_tree, tree))
    }

    // Whether producers that `other`, the basis of the published recording, placed land in the same cull spaces.
    fn maps_content_as(&self, other: &InterestBasis) -> bool {
        std::ptr::eq(self, other) || self.cull_mapping == other.cull_mapping
    }

    // Whether working out interest from `other` decides what working it out from this basis does.
    fn decides_like(&self, other: &InterestBasis) -> bool {
        *self.cull_spaces == *other.cull_spaces
            && self.cull_mapping == other.cull_mapping
            && self.spatial_node_count == other.spatial_node_count
            && self.scroll_nodes == other.scroll_nodes
            && self.scroll_nodes_recorded_whole == other.scroll_nodes_recorded_whole
    }
}

impl RecordedInterest {
    /// Whether a display list with this interest tells the compositor what one with `other` tells it. The request it
    /// answers does not count: the frame that presents the list tells the compositor apart from the list.
    pub(crate) fn tells_the_compositor_the_same_as(&self, other: &Self) -> bool {
        self.scroll_node_rects == other.scroll_node_rects
            && self.leaves_out_wheel_routing == other.leaves_out_wheel_routing
    }

    // Whether a frame whose scroll state holds `own_offsets` shows part of a scroll node that neither this recording nor
    // `requested` records, or would show it at one of `smooth_scroll_destinations`. A node is cut by what this recording
    // records of the node it is placed exactly in, as a recording cuts it.
    fn misses_what_scroll_positions_show(
        &self,
        own_offsets: &[CssPixelPoint],
        requested: Option<&InterestRequest>,
        smooth_scroll_destinations: &[FfiScrollNodeDestination],
    ) -> bool {
        let Some(basis) = &self.basis else {
            return false;
        };
        basis.scroll_nodes.iter().enumerate().any(|(position, node)| {
            let own_offset = if node.state_slot == NO_SCROLL_STATE_SLOT {
                Some(CssPixelPoint::default())
            } else {
                own_offsets.get(node.state_slot).copied()
            };
            let Some(own_offset) = own_offset else {
                return true;
            };
            let parent_recorded = node.placed_exactly_in.map(|parent| self.scroll_node_rects[parent].rect);
            let view = node.view_heading_to_smooth_scroll_destinations(
                device_scroll_offset(own_offset, basis.device_pixels_per_css_pixel),
                parent_recorded,
                smooth_scroll_destinations,
            );
            let records = |rect: IntRect| rect.contains_rect(view);
            let requested = requested
                .and_then(|requested| requested.rect_requested_for(&node.named))
                .map(|requested| {
                    interest_rect_recorded_for(
                        requested_rect_joined_with_view_on_unscrollable_axes(requested, view, node.range),
                        node.range,
                    )
                });
            !(view.is_empty() || records(self.scroll_node_rects[position].rect) || requested.is_some_and(records))
        })
    }
    // Of each scroll node that a frame whose scroll state holds `own_offsets` shows part of that this recording leaves
    // out, the default rect around what it shows. A node is cut by what this recording records of the node it is placed
    // exactly in, and a node the scroll state has no slot for is left out.
    fn rects_around_what_scroll_positions_show_that_it_leaves_out(
        &self,
        own_offsets: &[CssPixelPoint],
    ) -> Vec<FfiScrollNodeContentRect> {
        let Some(basis) = &self.basis else {
            return Vec::new();
        };
        basis
            .scroll_nodes
            .iter()
            .enumerate()
            .filter_map(|(position, node)| {
                let own_offset = match node.state_slot {
                    NO_SCROLL_STATE_SLOT => CssPixelPoint::default(),
                    state_slot => *own_offsets.get(state_slot)?,
                };
                let parent_recorded = node.placed_exactly_in.map(|parent| self.scroll_node_rects[parent].rect);
                let view = node.view_heading_to_smooth_scroll_destinations(
                    device_scroll_offset(own_offset, basis.device_pixels_per_css_pixel),
                    parent_recorded,
                    &[],
                );
                (!view.is_empty() && !self.scroll_node_rects[position].rect.contains_rect(view)).then(|| {
                    FfiScrollNodeContentRect {
                        rect: default_interest_rect(view, size_of(node.scrollport), node.range.axes),
                        ..node.named
                    }
                })
            })
            .collect()
    }
}

/// What the display list a compositor context holds records, as the Paint thread presented it last, with the structural
/// epoch of the visual context tree it was recorded against. A frame recorded beside the event loop aims for what it
/// records of the scroll nodes no request names, so that it records no less of them than the list it replaces, and a
/// frame the main thread commits records where its scroll positions show what it leaves out.
#[derive(Clone)]
pub(crate) struct PresentedInterest {
    interest: Arc<RecordedInterest>,
    structural_epoch: u64,
}

impl PresentedInterest {
    pub(crate) fn of(output: &super::RecordingOutput) -> Self {
        Self {
            interest: Arc::clone(&output.recorded_interest),
            structural_epoch: output.recorded_structural_epoch,
        }
    }

    pub(crate) fn interest(&self) -> &RecordedInterest {
        &self.interest
    }

    /// Whether a frame whose visual context tree has `structural_epoch`, and whose scroll state holds `own_offsets`,
    /// shows part of a scroll node that the display list leaves out and `requested`, the compositor's newest request,
    /// does not ask for, or would show it at one of `smooth_scroll_destinations`, where smooth scrolls the main thread
    /// asked for take the nodes. A display list of the whole document leaves nothing out; one of another tree may leave
    /// out anything.
    pub(crate) fn misses_what_scroll_positions_show(
        &self,
        structural_epoch: u64,
        own_offsets: &[CssPixelPoint],
        requested: Option<&InterestRequest>,
        smooth_scroll_destinations: &[FfiScrollNodeDestination],
    ) -> bool {
        if self.interest.basis.is_none() {
            return false;
        }
        self.structural_epoch != structural_epoch
            || self
                .interest
                .misses_what_scroll_positions_show(own_offsets, requested, smooth_scroll_destinations)
    }

    /// The rects to record around of the scroll nodes that a frame whose visual context tree has `structural_epoch`,
    /// and whose scroll state holds `own_offsets`, shows part of that the display list leaves out: the default rect
    /// around what each shows. None where the display list was recorded against another tree, whose scroll nodes the
    /// frame's scroll state does not name.
    pub(crate) fn rects_around_what_scroll_positions_show_that_it_leaves_out(
        &self,
        structural_epoch: u64,
        own_offsets: &[CssPixelPoint],
    ) -> Vec<FfiScrollNodeContentRect> {
        if self.structural_epoch != structural_epoch {
            return Vec::new();
        }
        self.interest
            .rects_around_what_scroll_positions_show_that_it_leaves_out(own_offsets)
    }
}

// What the published recording recorded of each scroll node.
enum PublishedScrollNodes<'a> {
    // The published recording worked its interest out from the same basis, so its nodes lie at the same positions.
    AtTheSamePositions(&'a RecordedInterest),
    ByOwner {
        interest: &'a RecordedInterest,
        position_by_owner: FastMap<NodeSlotId, usize>,
    },
}

impl<'a> PublishedScrollNodes<'a> {
    fn new(interest: &'a RecordedInterest, basis: &InterestBasis) -> Self {
        let Some(published_basis) = interest.basis.as_deref() else {
            return Self::ByOwner {
                interest,
                position_by_owner: FastMap::default(),
            };
        };
        if std::ptr::eq(published_basis, basis) {
            return Self::AtTheSamePositions(interest);
        }
        let position_by_owner = published_basis
            .scroll_nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.owner, position))
            .collect();
        Self::ByOwner {
            interest,
            position_by_owner,
        }
    }

    fn interest(&self) -> &'a RecordedInterest {
        match self {
            Self::AtTheSamePositions(interest) | Self::ByOwner { interest, .. } => interest,
        }
    }

    // The position in the published recording of the node at `position` of the current basis, if it recorded the node
    // of the same owner.
    fn position_of(&self, position: usize, node: &ScrollNodeBasis) -> Option<usize> {
        match self {
            Self::AtTheSamePositions(_) => Some(position),
            Self::ByOwner { position_by_owner, .. } => position_by_owner.get(&node.owner).copied(),
        }
    }

    // The rect the published recording culled the node of `owner` by, if it was the scroll node `scroll_node` then too,
    // for a node the current basis records whole.
    fn culled_by_rect_of_node_recorded_whole(
        &self,
        owner: NodeSlotId,
        scroll_node: SpatialNodeIndex,
    ) -> Option<IntRect> {
        let Self::ByOwner {
            interest,
            position_by_owner,
        } = self
        else {
            return None;
        };
        let published_position = *position_by_owner.get(&owner)?;
        let published_node = interest.basis.as_ref()?.scroll_nodes[published_position];
        (published_node.scroll_node == scroll_node).then(|| interest.scroll_node_rects[published_position].rect)
    }

    fn aimed_rect_of(&self, position: usize, node: &ScrollNodeBasis) -> Option<IntRect> {
        let published_position = self.position_of(position, node)?;
        Some(self.interest().aimed_rects[published_position])
    }

    // The rect the published recording culled the node by, if the node was the scroll node of the same owner then too.
    fn culled_by_rect_of(&self, position: usize, node: &ScrollNodeBasis) -> Option<IntRect> {
        let published_position = self.position_of(position, node)?;
        let interest = self.interest();
        let published_node = interest.basis.as_ref()?.scroll_nodes[published_position];
        (published_node.scroll_node == node.scroll_node).then(|| interest.scroll_node_rects[published_position].rect)
    }
}

/// The rect each spatial node's content is culled by.
#[derive(Default)]
pub(crate) struct InterestRects {
    // Indexed by spatial node: none where nothing limits its content. An empty rect culls all of it.
    by_spatial_node: Vec<Option<IntRect>>,
    // The scroll nodes whose rect differs from the one the published recording culled them by, with that rect, or none
    // where the published recording culled nothing of the node by a rect.
    published_rects_of_changed_scroll_nodes: FastMap<SpatialNodeIndex, Option<IntRect>>,
}

impl InterestRects {
    fn new(spatial_node_count: usize) -> Self {
        Self {
            by_spatial_node: vec![None; spatial_node_count],
            published_rects_of_changed_scroll_nodes: FastMap::default(),
        }
    }

    // Notes that the published recording culled `scroll_node` by `published_rect`, and that this one culls it by
    // another rect or by none.
    fn note_changed_scroll_node(
        &mut self,
        changed_scroll_nodes: &mut ScrollNodeSet,
        scroll_node: SpatialNodeIndex,
        published_rect: Option<IntRect>,
    ) {
        changed_scroll_nodes.insert(scroll_node);
        self.published_rects_of_changed_scroll_nodes
            .insert(scroll_node, published_rect);
    }

    // Of the nodes `basis` records whole, notes those the published recording culled by a rect as changed.
    fn note_scroll_nodes_recorded_whole(
        &mut self,
        changed_scroll_nodes: &mut ScrollNodeSet,
        basis: &InterestBasis,
        published_nodes: Option<&PublishedScrollNodes>,
    ) {
        for &(scroll_node, owner) in &basis.scroll_nodes_recorded_whole {
            let published_rect = published_nodes
                .and_then(|published| published.culled_by_rect_of_node_recorded_whole(owner, scroll_node));
            if published_rect.is_some() {
                self.note_changed_scroll_node(changed_scroll_nodes, scroll_node, published_rect);
            }
        }
    }

    fn rect_of(&self, scroll_node: SpatialNodeIndex) -> Option<IntRect> {
        self.by_spatial_node.get(scroll_node.0 as usize).copied().flatten()
    }

    /// Whether the commands landing where `reach` says are all outside the rect of their scroll node.
    pub(crate) fn miss(&self, reach: CullReach) -> bool {
        let Some((scroll_node, bounds)) = reach.bounds_in_only_scroll_node() else {
            return false;
        };
        self.rect_of(scroll_node)
            .is_some_and(|rect| rect.intersected(bounds).is_empty())
    }

    /// The rect the published recording culled `scroll_node` by and the one this recording culls it by, unless one of
    /// them culls nothing of it by a rect.
    pub(crate) fn published_and_current_rects_of(&self, scroll_node: SpatialNodeIndex) -> Option<(IntRect, IntRect)> {
        let current = self.rect_of(scroll_node)?;
        let published = match self.published_rects_of_changed_scroll_nodes.get(&scroll_node) {
            Some(published) => (*published)?,
            None => current,
        };
        Some((published, current))
    }
}

/// What a recording records of its document, worked out before it assembles: the rects it culls by, what it leaves
/// out of the published recording's output differently, and what it says it records.
pub(crate) struct ResolvedInterest {
    pub(crate) rects: InterestRects,
    pub(crate) culling: AssemblyCulling,
    recorded: ResolvedRecordedInterest,
}

enum ResolvedRecordedInterest {
    Resolved(RecordedInterest),
    AsSourceRecording(Arc<RecordedInterest>),
}

impl ResolvedInterest {
    fn whole_document() -> Self {
        Self {
            rects: InterestRects::default(),
            culling: AssemblyCulling::LeavesNothingOut,
            recorded: ResolvedRecordedInterest::Resolved(RecordedInterest::default()),
        }
    }

    // Notes the request the recording answers, where what it records holds every rect `requested` asks for.
    fn note_answered_request(&mut self, requested: &InterestRequest) {
        if let ResolvedRecordedInterest::Resolved(recorded) = &mut self.recorded {
            recorded.answered_request_id = requested.compositor_request_answered_by(&recorded.scroll_node_rects);
        }
    }

    /// What the recording records, once assembly told whether it left out wheel routing.
    pub(crate) fn into_recorded_interest(self, leaves_out_wheel_routing: bool) -> Arc<RecordedInterest> {
        match self.recorded {
            ResolvedRecordedInterest::Resolved(recorded) => Arc::new(RecordedInterest {
                leaves_out_wheel_routing: leaves_out_wheel_routing && !recorded.scroll_node_rects.is_empty(),
                ..recorded
            }),
            ResolvedRecordedInterest::AsSourceRecording(recorded) => recorded,
        }
    }
}

// What a recording that may leave content out by interest aims for of each scroll node (see `InterestInput`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Aiming {
    AroundScrollPositions,
    AroundScrollPositionsForTesting,
    AsRequested,
}

impl<O: Observer> PaintRecorder<'_, O> {
    /// What the recording records of its document, given `published`, what the recording published last records. A
    /// recording that may leave producers out by interest, or that verifies their placement, learns its cull spaces
    /// here; any other places none of its producers. A recording that took the compositor's request answers it where
    /// it records every rect the request asks for.
    pub(crate) fn resolve_interest(&mut self, published: Option<&Arc<RecordedInterest>>) -> ResolvedInterest {
        let requested = self.inputs.interest_request().map(Arc::as_ref);
        let mut resolved = self.resolve_interest_around(published, requested);
        if let Some(requested) = requested {
            resolved.note_answered_request(requested);
        }
        resolved
    }

    fn resolve_interest_around(
        &mut self,
        published: Option<&Arc<RecordedInterest>>,
        requested: Option<&InterestRequest>,
    ) -> ResolvedInterest {
        let inputs = self.inputs;
        let leaves_out_by_interest = match inputs.interest {
            InterestInput::WholeDocument => false,
            InterestInput::AsSourceRecording => published.is_some_and(|published| published.basis.is_some()),
            InterestInput::AroundScrollPositions { .. }
            | InterestInput::AroundScrollPositionsForTesting
            | InterestInput::AsRequested { .. } => true,
        };
        if !leaves_out_by_interest {
            if super::cull::verifies_paint_producer_bounds() {
                self.cull_spaces = self.cull_spaces_of_recording().map(Arc::new);
            }
            return ResolvedInterest::whole_document();
        }
        let paint_state = self.paint_state;
        let Some(tree) = paint_state.visual_context_tree.as_ref() else {
            return ResolvedInterest::whole_document();
        };
        let Some(basis) = self.interest_basis(tree, published.map(Arc::as_ref)) else {
            return ResolvedInterest::whole_document();
        };
        self.cull_spaces = Some(Arc::clone(&basis.cull_spaces));
        match (&inputs.interest, published) {
            (InterestInput::AsSourceRecording, Some(published)) => {
                Self::interest_of_source_recording(&basis, published)
            }
            (
                InterestInput::AroundScrollPositions {
                    smooth_scroll_destinations,
                    ..
                },
                _,
            ) => self.interest_around_scroll_positions(
                &basis,
                published.map(Arc::as_ref),
                None,
                Aiming::AroundScrollPositions,
                requested,
                smooth_scroll_destinations,
            ),
            (InterestInput::AroundScrollPositionsForTesting, _) => self.interest_around_scroll_positions(
                &basis,
                published.map(Arc::as_ref),
                None,
                Aiming::AroundScrollPositionsForTesting,
                None,
                &[],
            ),
            (InterestInput::AsRequested { presented, .. }, _) => self.interest_around_scroll_positions(
                &basis,
                published.map(Arc::as_ref),
                presented.as_ref().map(PresentedInterest::interest),
                Aiming::AsRequested,
                requested,
                &[],
            ),
            (InterestInput::WholeDocument | InterestInput::AsSourceRecording, _) => ResolvedInterest::whole_document(),
        }
    }

    // The basis of the published recording's interest while it still holds for the frame, else one worked out afresh.
    fn interest_basis(
        &mut self,
        tree: &Arc<VisualContextTree>,
        published: Option<&RecordedInterest>,
    ) -> Option<Arc<InterestBasis>> {
        let frame = self.source.frame();
        let geometry_epoch = frame.geometry_epoch();
        let device_pixels_per_css_pixel = self.inputs.device_pixels_per_css_pixel;
        let device_viewport_size = self.inputs.uncaptured.device_viewport_size;
        if let Some(basis) = published.and_then(|published| published.basis.as_ref())
            && basis.holds_for(tree, geometry_epoch, device_pixels_per_css_pixel, device_viewport_size)
            && !frame.damage().changes_scroll_metadata()
        {
            // Nothing the culling audit checks would catch a basis that no longer holds, as it places producers by it.
            if super::cull::verifies_interest_culling() {
                let afresh = self.work_out_interest_basis(tree);
                assert!(
                    afresh.as_ref().is_some_and(|afresh| afresh.decides_like(basis)),
                    "The interest basis kept since geometry epoch {geometry_epoch} no longer holds: kept {basis:?}, worked out afresh {afresh:?}"
                );
            }
            return Some(Arc::clone(basis));
        }
        self.work_out_interest_basis(tree).map(Arc::new)
    }

    fn work_out_interest_basis(&mut self, tree: &Arc<VisualContextTree>) -> Option<InterestBasis> {
        let geometry_epoch = self.source.frame().geometry_epoch();
        let device_pixels_per_css_pixel = self.inputs.device_pixels_per_css_pixel;
        let device_viewport_size = self.inputs.uncaptured.device_viewport_size;
        let cull_spaces = Arc::new(self.cull_spaces_of_recording()?);
        let mut position_of_scroll_node = vec![None; tree.spatial_nodes.len()];
        let mut order = Vec::with_capacity(tree.spatial_nodes.len());
        tree.visit_spatial_nodes_parents_first(|index| order.push(index));
        let mut scroll_nodes = Vec::new();
        let mut scroll_nodes_recorded_whole = Vec::new();
        for index in order {
            let scroll_node = SpatialNodeIndex(index as u32);
            let node = &tree.spatial_nodes[index];
            let SpatialData::Scroll(ScrollData {
                owner_paintable: owner,
                state_slot,
                ..
            }) = node.data
            else {
                continue;
            };
            let Some(kind) = cull_spaces.compositor_scroll_node_kind(scroll_node) else {
                continue;
            };
            let scrollport = self.compositor_scrollport_rect(owner);
            let (minimum_offset, maximum_offset) = self.device_scroll_offset_range(owner);
            let range = ScrollRange::new(scrollport, minimum_offset, maximum_offset);
            if range.reaches_past_unbounded_edges {
                scroll_nodes_recorded_whole.push((scroll_node, owner));
                continue;
            }
            // Filters that spread the node's content move it beyond its scrollport, as transforms above it do.
            let placement_in_parent = cull_spaces.cull_space_of(
                tree,
                ContextRef {
                    spatial: node.parent,
                    clip: ClipNodeIndex::NONE,
                    effect: self.data(owner).accumulated_visual_context_for_descendants.effect,
                },
            );
            let placed_exactly_in = match placement_in_parent {
                CullSpace::ScrollNode {
                    scroll_node: parent,
                    placed_exactly: true,
                } => position_of_scroll_node[parent.0 as usize],
                _ => None,
            };
            position_of_scroll_node[index] = Some(scroll_nodes.len());
            scroll_nodes.push(ScrollNodeBasis {
                scroll_node,
                owner,
                state_slot,
                named: FfiScrollNodeContentRect {
                    scrollable_node_id: self.data(owner).node_identity,
                    scroll_node_kind: kind,
                    pseudo_element_type: self.source.node_generated_for(owner).saturating_sub(1),
                    rect: IntRect::default(),
                },
                scrollport,
                range,
                placed_exactly_in,
            });
        }
        Some(InterestBasis {
            tree: Arc::downgrade(tree),
            geometry_epoch,
            device_pixels_per_css_pixel,
            device_viewport_size,
            cull_mapping: cull_mapping_of(tree, &cull_spaces),
            cull_spaces,
            spatial_node_count: tree.spatial_nodes.len(),
            scroll_nodes,
            scroll_nodes_recorded_whole,
        })
    }

    // What the recording records around the scroll positions, or as requested, given `published`, what the recording
    // published last records, and `presented`, what the display list the recording's frame takes the place of on screen
    // records where that is not the published recording, whose aims a node the request does not name keeps.
    fn interest_around_scroll_positions(
        &mut self,
        basis: &Arc<InterestBasis>,
        published: Option<&RecordedInterest>,
        presented: Option<&RecordedInterest>,
        aiming: Aiming,
        requested: Option<&InterestRequest>,
        smooth_scroll_destinations: &[FfiScrollNodeDestination],
    ) -> ResolvedInterest {
        let for_testing = aiming == Aiming::AroundScrollPositionsForTesting;
        let published_nodes = published.map(|published| PublishedScrollNodes::new(published, basis));
        let presented_nodes = presented.map(|presented| PublishedScrollNodes::new(presented, basis));
        // A test never depends on what a recording outside it aimed for.
        let kept_from = presented_nodes
            .as_ref()
            .or(published_nodes.as_ref())
            .filter(|kept_from| {
                kept_from.interest().basis.is_some() && (!for_testing || kept_from.interest().recorded_for_testing)
            });
        let mut rects = InterestRects::new(basis.spatial_node_count);
        let mut recorded = RecordedInterest {
            scroll_node_rects: Vec::with_capacity(basis.scroll_nodes.len()),
            aimed_rects: Vec::with_capacity(basis.scroll_nodes.len()),
            basis: Some(Arc::clone(basis)),
            recorded_for_testing: for_testing,
            ..RecordedInterest::default()
        };
        let mut changed_scroll_nodes = ScrollNodeSet::default();
        for (position, node) in basis.scroll_nodes.iter().enumerate() {
            let scroll_offset = self.device_scroll_offset_of(node.state_slot);
            // The parent culls by what it records, which reaches past what it aims for, so a nested node is cut by that.
            let parent_recorded = node
                .placed_exactly_in
                .map(|parent| recorded.scroll_node_rects[parent].rect);
            let view = node.view_heading_to_smooth_scroll_destinations(
                scroll_offset,
                parent_recorded,
                smooth_scroll_destinations,
            );
            let requested_rect = requested.and_then(|requested| requested.rect_requested_for(&node.named));
            let published_aimed = kept_from.and_then(|published| published.aimed_rect_of(position, node));
            let scrollport_size = size_of(node.scrollport);
            let aimed = match aiming {
                Aiming::AroundScrollPositions | Aiming::AroundScrollPositionsForTesting => {
                    aimed_interest_rect(view, requested_rect, published_aimed, scrollport_size, node.range)
                }
                Aiming::AsRequested => {
                    interest_rect_as_requested(view, requested_rect, published_aimed, scrollport_size, node.range)
                }
            };
            let culled_by = interest_rect_recorded_for(aimed, node.range);
            rects.by_spatial_node[node.scroll_node.0 as usize] = Some(culled_by);
            let published_rect = published_nodes
                .as_ref()
                .and_then(|published| published.culled_by_rect_of(position, node));
            if published_rect != Some(culled_by) {
                rects.note_changed_scroll_node(&mut changed_scroll_nodes, node.scroll_node, published_rect);
            }
            recorded.scroll_node_rects.push(FfiScrollNodeContentRect {
                rect: culled_by,
                ..node.named
            });
            recorded.aimed_rects.push(aimed);
        }
        rects.note_scroll_nodes_recorded_whole(&mut changed_scroll_nodes, basis, published_nodes.as_ref());
        let places_every_producer_again = published
            .and_then(|published| published.basis.as_deref())
            .is_none_or(|published_basis| !basis.maps_content_as(published_basis));
        ResolvedInterest {
            rects,
            culling: AssemblyCulling::ByInterest {
                changed_scroll_nodes,
                places_every_producer_again,
            },
            recorded: ResolvedRecordedInterest::Resolved(recorded),
        }
    }

    // A recording that only rebuilds the hit-test list culls by what the recording it rebuilds culled by. A scroll node
    // that recording did not cull culls nothing.
    fn interest_of_source_recording(basis: &Arc<InterestBasis>, published: &Arc<RecordedInterest>) -> ResolvedInterest {
        let published_nodes = PublishedScrollNodes::new(published, basis);
        let mut rects = InterestRects::new(basis.spatial_node_count);
        let mut changed_scroll_nodes = ScrollNodeSet::default();
        for (position, node) in basis.scroll_nodes.iter().enumerate() {
            match published_nodes.culled_by_rect_of(position, node) {
                Some(rect) => rects.by_spatial_node[node.scroll_node.0 as usize] = Some(rect),
                None => rects.note_changed_scroll_node(&mut changed_scroll_nodes, node.scroll_node, None),
            }
        }
        rects.note_scroll_nodes_recorded_whole(&mut changed_scroll_nodes, basis, Some(&published_nodes));
        let places_every_producer_again = published
            .basis
            .as_deref()
            .is_none_or(|published_basis| !basis.maps_content_as(published_basis));
        ResolvedInterest {
            rects,
            culling: AssemblyCulling::ByInterest {
                changed_scroll_nodes,
                places_every_producer_again,
            },
            recorded: ResolvedRecordedInterest::AsSourceRecording(Arc::clone(published)),
        }
    }

    fn device_scroll_offset_of(&self, state_slot: usize) -> FloatPoint {
        let own_offset = if state_slot == NO_SCROLL_STATE_SLOT {
            CssPixelPoint::default()
        } else {
            self.paint_state.scroll_own_offset(state_slot)
        };
        device_scroll_offset(own_offset, self.inputs.device_pixels_per_css_pixel)
    }
}

// A scroll node's scroll offset in device pixels, from its own offset in the scroll state, which is kept negated: a node
// scrolled down has a negative own offset.
fn device_scroll_offset(own_offset: CssPixelPoint, device_pixels_per_css_pixel: f64) -> FloatPoint {
    let scale = device_pixels_per_css_pixel as f32;
    FloatPoint {
        x: -own_offset.x.to_float() * scale,
        y: -own_offset.y.to_float() * scale,
    }
}

fn size_of(rect: IntRect) -> IntSize {
    IntSize {
        width: rect.width,
        height: rect.height,
    }
}

// What a node aims to record around `view`: the rect the compositor requested while what the node records for it holds
// the view with room to scroll, else the rect the published recording aimed for while that holds the view, else the
// default rect around the view.
fn aimed_interest_rect(
    view: IntRect,
    requested: Option<IntRect>,
    published_aimed: Option<IntRect>,
    scrollport_size: IntSize,
    range: ScrollRange,
) -> IntRect {
    let holds_view =
        |rect: IntRect| interest_rect_covers(rect, view, scrollport_size, LeadPerSide::default(), range.axes);
    if let Some(requested) = requested
        .map(|requested| requested_rect_joined_with_view_on_unscrollable_axes(requested, view, range))
        .filter(|requested| holds_view(interest_rect_recorded_for(*requested, range)))
    {
        return requested;
    }
    if view.is_empty() {
        return IntRect::default();
    }
    published_aimed
        .filter(|aimed| holds_view(*aimed))
        .unwrap_or_else(|| default_interest_rect(view, scrollport_size, range.axes))
}

// What a node of a frame recorded beside the event loop aims to record: the rect the compositor requested, whatever
// `view`, which the scroll position of the committed frame the frame starts from shows, else the rect the display list
// the frame replaces aimed for, else the default rect around the view. An empty requested rect asks for nothing.
fn interest_rect_as_requested(
    view: IntRect,
    requested: Option<IntRect>,
    published_aimed: Option<IntRect>,
    scrollport_size: IntSize,
    range: ScrollRange,
) -> IntRect {
    if let Some(requested) = requested.filter(|requested| !requested.is_empty()) {
        return requested_rect_joined_with_view_on_unscrollable_axes(requested, view, range);
    }
    if let Some(published_aimed) = published_aimed {
        return published_aimed;
    }
    if view.is_empty() {
        return IntRect::default();
    }
    default_interest_rect(view, scrollport_size, range.axes)
}

// `requested`, joined with `view` on each axis `range` does not scroll, where a node records all of its view. The
// compositor cuts such an axis by where it takes the node to lie in its parent, which it tells without the filters that
// spread the node's content, so it may ask for less of that axis than the node records.
fn requested_rect_joined_with_view_on_unscrollable_axes(
    requested: IntRect,
    view: IntRect,
    range: ScrollRange,
) -> IntRect {
    if view.is_empty() {
        return requested;
    }
    let (left, right) = if range.axes.horizontal {
        (requested.x, requested.right())
    } else {
        (requested.x.min(view.x), requested.right().max(view.right()))
    };
    let (top, bottom) = if range.axes.vertical {
        (requested.y, requested.bottom())
    } else {
        (requested.y.min(view.y), requested.bottom().max(view.bottom()))
    };
    IntRect::new(left, top, right - left, bottom - top)
}

fn cull_mapping_of(tree: &VisualContextTree, cull_spaces: &CullSpaces) -> CullMapping {
    let compositor_scroll_nodes = tree
        .spatial_nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| match node.data {
            SpatialData::Scroll(ScrollData { owner_paintable, .. })
                if cull_spaces.is_scrolled_by_compositor(SpatialNodeIndex(index as u32)) =>
            {
                Some((SpatialNodeIndex(index as u32), owner_paintable))
            }
            _ => None,
        })
        .collect();
    let effects_under_spreading_filters = cull_spaces
        .effects_under_spreading_filters()
        .iter()
        .enumerate()
        .filter_map(|(index, under_spreading_filter)| under_spreading_filter.then_some(index))
        .collect();
    CullMapping {
        compositor_scroll_nodes,
        effects_under_spreading_filters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::css_pixels::CssPixels;
    use crate::painting::display_list::commands::CompositorScrollNodeKind;
    use crate::painting::visual_context::interest::{ScrollableAxes, UNBOUNDED_INTEREST_EDGE};

    const UNBOUNDED: i32 = UNBOUNDED_INTEREST_EDGE;
    const SCROLLPORT: IntRect = IntRect::new(0, 0, 1000, 800);
    const SCROLLPORT_SIZE: IntSize = IntSize {
        width: 1000,
        height: 800,
    };
    const VERTICAL: ScrollableAxes = ScrollableAxes {
        horizontal: false,
        vertical: true,
    };

    fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> IntRect {
        IntRect::new(left, top, right - left, bottom - top)
    }

    // A root that scrolls 20000 px of content vertically.
    fn root_range() -> ScrollRange {
        ScrollRange::new(
            SCROLLPORT,
            FloatPoint { x: 0.0, y: 0.0 },
            FloatPoint { x: 0.0, y: 19200.0 },
        )
    }

    fn view_at(scroll_offset_y: f32) -> IntRect {
        view_to_record_around(
            SCROLLPORT,
            FloatPoint {
                x: 0.0,
                y: scroll_offset_y,
            },
            None,
            VERTICAL,
        )
    }

    #[test]
    fn a_frame_aims_for_the_requested_rect_while_it_holds_what_the_frame_shows() {
        let view = view_at(5000.0);
        let requested = from_edges(-UNBOUNDED, 4000, UNBOUNDED, 7000);
        assert_eq!(
            aimed_interest_rect(view, Some(requested), None, SCROLLPORT_SIZE, root_range()),
            requested
        );

        // A rect that leaves less than half a scrollport above what the frame shows is not aimed for.
        let behind_the_frame = from_edges(-UNBOUNDED, 5200, UNBOUNDED, 7000);
        assert_eq!(
            aimed_interest_rect(view, Some(behind_the_frame), None, SCROLLPORT_SIZE, root_range()),
            from_edges(-UNBOUNDED, 4200, UNBOUNDED, 6600)
        );
        let published_aimed = from_edges(-UNBOUNDED, 4500, UNBOUNDED, 6300);
        assert_eq!(
            aimed_interest_rect(
                view,
                Some(behind_the_frame),
                Some(published_aimed),
                SCROLLPORT_SIZE,
                root_range()
            ),
            published_aimed
        );
    }

    #[test]
    fn a_frame_takes_a_requested_rect_narrower_than_its_view_where_the_node_cannot_scroll() {
        // The root's view spans its whole width, which it cannot scroll, while the compositor asked for its scrollport's.
        let view = view_at(5000.0);
        let requested = from_edges(0, 4000, 1000, 7000);
        let aimed = aimed_interest_rect(view, Some(requested), None, SCROLLPORT_SIZE, root_range());
        assert_eq!(aimed, from_edges(-UNBOUNDED, 4000, UNBOUNDED, 7000));
        assert!(interest_rect_recorded_for(aimed, root_range()).contains_rect(requested));
    }

    #[test]
    fn a_requested_rect_that_ends_with_the_content_holds_the_end_of_the_scroll_range() {
        // The rect ends where the content does, so what the frame records for it is unbounded below.
        let requested = from_edges(-UNBOUNDED, 18000, UNBOUNDED, 20000);
        assert_eq!(
            aimed_interest_rect(view_at(19200.0), Some(requested), None, SCROLLPORT_SIZE, root_range()),
            requested
        );
        assert_eq!(
            interest_rect_recorded_for(requested, root_range()),
            from_edges(-UNBOUNDED, 17920, UNBOUNDED, UNBOUNDED)
        );
    }

    #[test]
    fn a_node_its_parent_cuts_away_records_only_what_the_compositor_requests() {
        let cut_away = view_to_record_around(
            SCROLLPORT,
            FloatPoint { x: 0.0, y: 0.0 },
            Some(from_edges(0, 4096, 1024, 8192)),
            VERTICAL,
        );
        assert!(cut_away.is_empty());
        assert!(aimed_interest_rect(cut_away, None, None, SCROLLPORT_SIZE, root_range()).is_empty());
        let requested = from_edges(0, 0, 1000, 1600);
        assert_eq!(
            aimed_interest_rect(cut_away, Some(requested), None, SCROLLPORT_SIZE, root_range()),
            requested
        );
    }

    #[test]
    fn a_clock_lane_frame_aims_for_the_requested_rect_wherever_its_scroll_position_is() {
        // The lane's frame shows the top, while the compositor scrolled far below and asked for a rect there.
        let view = view_at(0.0);
        let requested = from_edges(0, 9000, 1000, 11000);
        let published_aimed = from_edges(-UNBOUNDED, -800, UNBOUNDED, 1600);
        let aimed = interest_rect_as_requested(
            view,
            Some(requested),
            Some(published_aimed),
            SCROLLPORT_SIZE,
            root_range(),
        );
        assert_eq!(aimed, from_edges(-UNBOUNDED, 9000, UNBOUNDED, 11000));
        assert!(interest_rect_recorded_for(aimed, root_range()).contains_rect(requested));

        // A node the request does not name, or asks nothing of, keeps what the compositor holds, though that no longer
        // holds the view.
        let behind_the_view = view_at(5000.0);
        for requested in [None, Some(IntRect::default())] {
            assert_eq!(
                interest_rect_as_requested(
                    behind_the_view,
                    requested,
                    Some(published_aimed),
                    SCROLLPORT_SIZE,
                    root_range()
                ),
                published_aimed
            );
        }
        // A node the published recording did not record aims around its view.
        assert_eq!(
            interest_rect_as_requested(behind_the_view, None, None, SCROLLPORT_SIZE, root_range()),
            default_interest_rect(behind_the_view, SCROLLPORT_SIZE, VERTICAL)
        );
    }

    const ROOT: i64 = 1;
    const NESTED: i64 = 2;

    fn named(scrollable_node_id: i64) -> FfiScrollNodeContentRect {
        FfiScrollNodeContentRect {
            scrollable_node_id,
            scroll_node_kind: CompositorScrollNodeKind::Element,
            pseudo_element_type: 0,
            rect: IntRect::default(),
        }
    }

    fn own_offset_at(scroll_offset_y: i64) -> CssPixelPoint {
        CssPixelPoint::new(CssPixels::from_integer(0), CssPixels::from_integer(-scroll_offset_y))
    }

    fn destination_of(scrollable_node_id: i64, scroll_offset_y: f32) -> FfiScrollNodeDestination {
        FfiScrollNodeDestination {
            scrollable_node_id,
            scroll_node_kind: CompositorScrollNodeKind::Element,
            pseudo_element_type: 0,
            destination_offset: FloatPoint {
                x: 0.0,
                y: scroll_offset_y,
            },
        }
    }

    // What a recording around the top records of a root that scrolls 20000 px of content, and of a scroller placed
    // exactly in it, 4000 px down, which the root's rect cuts away.
    fn recorded_around_the_top() -> RecordedInterest {
        let nested_scrollport = IntRect::new(0, 4000, 300, 200);
        let basis = InterestBasis {
            tree: Weak::new(),
            geometry_epoch: 0,
            device_pixels_per_css_pixel: 1.0,
            device_viewport_size: SCROLLPORT_SIZE,
            cull_spaces: Arc::new(CullSpaces::default()),
            cull_mapping: CullMapping {
                compositor_scroll_nodes: Vec::new(),
                effects_under_spreading_filters: Vec::new(),
            },
            spatial_node_count: 3,
            scroll_nodes_recorded_whole: Vec::new(),
            scroll_nodes: vec![
                ScrollNodeBasis {
                    scroll_node: SpatialNodeIndex(1),
                    owner: NodeSlotId::new(1, 1),
                    state_slot: 0,
                    named: named(ROOT),
                    scrollport: SCROLLPORT,
                    range: root_range(),
                    placed_exactly_in: None,
                },
                ScrollNodeBasis {
                    scroll_node: SpatialNodeIndex(2),
                    owner: NodeSlotId::new(2, 1),
                    state_slot: 1,
                    named: named(NESTED),
                    scrollport: nested_scrollport,
                    range: ScrollRange::new(
                        nested_scrollport,
                        FloatPoint { x: 0.0, y: 0.0 },
                        FloatPoint { x: 0.0, y: 1800.0 },
                    ),
                    placed_exactly_in: Some(0),
                },
            ],
        };
        RecordedInterest {
            scroll_node_rects: vec![
                FfiScrollNodeContentRect {
                    rect: from_edges(-UNBOUNDED, -UNBOUNDED, UNBOUNDED, 2048),
                    ..named(ROOT)
                },
                named(NESTED),
            ],
            aimed_rects: vec![from_edges(-UNBOUNDED, -800, UNBOUNDED, 1600), IntRect::default()],
            basis: Some(Arc::new(basis)),
            ..RecordedInterest::default()
        }
    }

    #[test]
    fn a_main_thread_scroll_needs_a_recording_once_what_shows_leaves_what_is_recorded() {
        let recorded = recorded_around_the_top();
        let misses_at = |root_scroll_offset_y: i64, requested: Option<&InterestRequest>| {
            recorded.misses_what_scroll_positions_show(
                &[own_offset_at(root_scroll_offset_y), own_offset_at(0)],
                requested,
                &[],
            )
        };
        assert!(!misses_at(0, None));
        // What shows needs no room to scroll beyond it: the compositor asks for a recording before it scrolls further.
        assert!(!misses_at(1248, None));
        assert!(misses_at(1249, None));
        // A request whose recording would record what shows is serviced anyway.
        let requested = InterestRequest::new_for_testing(&[FfiScrollNodeContentRect {
            rect: from_edges(-UNBOUNDED, 1000, UNBOUNDED, 3000),
            ..named(ROOT)
        }]);
        assert!(!misses_at(2200, Some(&requested)));
        assert!(misses_at(2300, Some(&requested)));
        // The scroller the root's rect cuts away shows nothing, wherever it is scrolled to.
        assert!(!recorded.misses_what_scroll_positions_show(&[own_offset_at(0), own_offset_at(1800)], None, &[]));
        // A scroll state that has no slot for a node is of another tree.
        assert!(recorded.misses_what_scroll_positions_show(&[own_offset_at(0)], None, &[]));
        assert!(!RecordedInterest::default().misses_what_scroll_positions_show(&[], None, &[]));
    }

    #[test]
    fn a_frame_that_keeps_the_list_asks_for_what_its_scroll_positions_show_that_the_list_leaves_out() {
        let presented = PresentedInterest {
            interest: Arc::new(recorded_around_the_top()),
            structural_epoch: 1,
        };
        let asks_for_at = |structural_epoch: u64, root_scroll_offset_y: i64| {
            presented.rects_around_what_scroll_positions_show_that_it_leaves_out(
                structural_epoch,
                &[own_offset_at(root_scroll_offset_y), own_offset_at(0)],
            )
        };
        assert!(asks_for_at(1, 0).is_empty());
        // The root asks for the default rect around what shows, and the scroller the root's rect cuts away shows nothing.
        assert_eq!(
            asks_for_at(1, 5000),
            vec![FfiScrollNodeContentRect {
                rect: from_edges(-UNBOUNDED, 4200, UNBOUNDED, 6600),
                ..named(ROOT)
            }]
        );
        // The scroll state of another tree names other scroll nodes.
        assert!(asks_for_at(2, 5000).is_empty());
    }

    #[test]
    fn a_smooth_scroll_s_destination_near_enough_is_recorded_before_the_scroll_gets_there() {
        let recorded = recorded_around_the_top();
        let at_the_top = [own_offset_at(0), own_offset_at(0)];
        let misses_heading_to = |destination: FfiScrollNodeDestination| {
            recorded.misses_what_scroll_positions_show(&at_the_top, None, &[destination])
        };
        assert!(misses_heading_to(destination_of(ROOT, 1600.0)));
        assert!(!misses_heading_to(destination_of(ROOT, 1200.0)));
        // The compositor asks for a recording around a destination this far as the scroll nears it.
        assert!(!misses_heading_to(destination_of(ROOT, 19200.0)));
        assert!(!misses_heading_to(destination_of(NESTED + 1, 1600.0)));

        // A recording aims around both the view and the destination.
        let root = recorded.basis.as_ref().unwrap().scroll_nodes[0];
        let view = root.view_heading_to_smooth_scroll_destinations(
            FloatPoint { x: 0.0, y: 5000.0 },
            None,
            &[destination_of(ROOT, 5800.0)],
        );
        assert_eq!(view, from_edges(-UNBOUNDED, 5000, UNBOUNDED, 6600));
        assert_eq!(
            aimed_interest_rect(
                view,
                None,
                Some(from_edges(-UNBOUNDED, 4200, UNBOUNDED, 6600)),
                SCROLLPORT_SIZE,
                root_range()
            ),
            from_edges(-UNBOUNDED, 4200, UNBOUNDED, 7400)
        );
    }
}
