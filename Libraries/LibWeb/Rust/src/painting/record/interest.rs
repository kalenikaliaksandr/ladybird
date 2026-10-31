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
//! records for that rect holds what the scrollport shows.

use super::PaintRecorder;
use super::assemble::AssemblyCulling;
use super::cull::CullSpaces;
use super::inputs::InterestInput;
use super::order_tree::{CullReach, ScrollNodeSet};
use super::trace::Observer;
use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::display_list::commands::{ClipNodeIndex, ContextRef, SpatialNodeIndex};
use crate::painting::ffi::FfiScrollNodeContentRect;
use crate::painting::interest_refresh::InterestRequest;
use crate::painting::paint_read::PaintRead;
use crate::painting::visual_context::interest::{
    CullSpace, LeadPerSide, ScrollRange, default_interest_rect, interest_rect_covers, interest_rect_recorded_for,
    view_to_record_around,
};
use crate::painting::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
use crate::painting::visual_context::{ScrollData, SpatialData, VisualContextTree};
use libgfx_rust::{FloatPoint, IntRect, IntSize};
use std::sync::Arc;

/// What a recording records of its document.
#[derive(Debug, Default)]
pub(crate) struct RecordedInterest {
    /// What the display list tells the compositor it records of each scroll node the compositor scrolls. Empty when it
    /// records the whole document.
    pub(crate) scroll_node_rects: Vec<FfiScrollNodeContentRect>,
    // Beside each of `scroll_node_rects`, what a later recording reads of the node.
    scroll_nodes: Vec<ScrollNodeInterest>,
    /// Whether the display list leaves out per-box wheel targets, wheel regions, scroll chaining barriers or nested
    /// context regions outside the rects.
    pub(crate) leaves_out_wheel_routing: bool,
    /// The compositor's request to record around new rects that the recording answers: it records every rect the
    /// request asks for.
    pub(crate) answered_request_id: Option<u64>,
    // Where the recording's producers landed, for a later recording to tell whether they land there still. None when
    // the recording left nothing out by interest.
    cull_mapping: Option<CullMapping>,
    // Whether a testing recording made it: later testing recordings keep only what such a recording aimed for.
    recorded_for_testing: bool,
}

#[derive(Clone, Copy, Debug)]
struct ScrollNodeInterest {
    scroll_node: SpatialNodeIndex,
    owner: NodeSlotId,
    // In the node's content space at scroll offset zero, before snapping and saturation. Empty when the node records
    // none of its content.
    aimed: IntRect,
}

// What decides the cull space of content whose visual context did not change: which scroll nodes the compositor
// scrolls, and which effect nodes may spread content beyond its bounds. Content that moves to other spatial or effect
// nodes records again anyway.
#[derive(Debug, PartialEq)]
struct CullMapping {
    compositor_scroll_nodes: Vec<(SpatialNodeIndex, NodeSlotId)>,
    effects_under_spreading_filters: Vec<usize>,
}

impl RecordedInterest {
    /// Whether a display list with this interest tells the compositor what one with `other` tells it. The request it
    /// answers does not count: the frame that presents the list tells the compositor apart from the list.
    pub(crate) fn tells_the_compositor_the_same_as(&self, other: &Self) -> bool {
        self.scroll_node_rects == other.scroll_node_rects
            && self.leaves_out_wheel_routing == other.leaves_out_wheel_routing
    }
}

// What a recording recorded of each scroll node, found by the node's owner.
struct ScrollNodesByOwner<'a> {
    interest: &'a RecordedInterest,
    index_by_owner: FastMap<NodeSlotId, usize>,
}

impl<'a> ScrollNodesByOwner<'a> {
    fn new(interest: &'a RecordedInterest) -> Self {
        let index_by_owner = interest
            .scroll_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.owner, index))
            .collect();
        Self {
            interest,
            index_by_owner,
        }
    }

    fn aimed_rect_of(&self, owner: NodeSlotId) -> Option<IntRect> {
        let index = *self.index_by_owner.get(&owner)?;
        Some(self.interest.scroll_nodes[index].aimed)
    }

    // The rect the recording culled the scroll node by, if it was the scroll node of `owner` then too.
    fn culled_by_rect_of(&self, scroll_node: SpatialNodeIndex, owner: NodeSlotId) -> Option<IntRect> {
        let index = *self.index_by_owner.get(&owner)?;
        (self.interest.scroll_nodes[index].scroll_node == scroll_node)
            .then(|| self.interest.scroll_node_rects[index].rect)
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

// The geometry of a scroll node the compositor scrolls, in device pixels.
struct ScrollNodeGeometry {
    scrollport: IntRect,
    scroll_offset: FloatPoint,
    range: ScrollRange,
}

impl<O: Observer> PaintRecorder<'_, O> {
    /// What the recording records of its document, given `published`, what the recording published last records. A
    /// recording that may leave producers out by interest, or that verifies their placement, learns its cull spaces
    /// here; any other places none of its producers. A recording that took the compositor's request answers it where
    /// it records every rect the request asks for.
    pub(crate) fn resolve_interest(&mut self, published: Option<&Arc<RecordedInterest>>) -> ResolvedInterest {
        let inputs = self.inputs;
        let requested = match &inputs.interest {
            InterestInput::AroundScrollPositions { requested } => requested.as_deref(),
            _ => None,
        };
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
            InterestInput::AsSourceRecording => published.is_some_and(|published| published.cull_mapping.is_some()),
            InterestInput::AroundScrollPositions { .. } | InterestInput::AroundScrollPositionsForTesting => true,
        };
        if leaves_out_by_interest || super::cull::verifies_paint_producer_bounds() {
            self.cull_spaces = self.cull_spaces_of_recording();
        }
        if !leaves_out_by_interest {
            return ResolvedInterest::whole_document();
        }
        let paint_state = self.paint_state;
        let Some(tree) = paint_state.visual_context_tree.as_deref() else {
            return ResolvedInterest::whole_document();
        };
        let Some(cull_spaces) = self.cull_spaces.take() else {
            return ResolvedInterest::whole_document();
        };
        let resolved = match (&inputs.interest, published) {
            (InterestInput::AsSourceRecording, Some(published)) => {
                self.interest_of_source_recording(tree, &cull_spaces, published)
            }
            (InterestInput::AroundScrollPositions { .. }, _) => {
                self.interest_around_scroll_positions(tree, &cull_spaces, published.map(Arc::as_ref), false, requested)
            }
            (InterestInput::AroundScrollPositionsForTesting, _) => {
                self.interest_around_scroll_positions(tree, &cull_spaces, published.map(Arc::as_ref), true, None)
            }
            (InterestInput::WholeDocument | InterestInput::AsSourceRecording, _) => ResolvedInterest::whole_document(),
        };
        self.cull_spaces = Some(cull_spaces);
        resolved
    }

    fn interest_around_scroll_positions(
        &mut self,
        tree: &VisualContextTree,
        cull_spaces: &CullSpaces,
        published: Option<&RecordedInterest>,
        for_testing: bool,
        requested: Option<&InterestRequest>,
    ) -> ResolvedInterest {
        let published_nodes = published.map(ScrollNodesByOwner::new);
        // A test never depends on what a recording outside it aimed for.
        let kept_from = published_nodes.as_ref().filter(|published| {
            published.interest.cull_mapping.is_some() && (!for_testing || published.interest.recorded_for_testing)
        });
        let mut order = Vec::with_capacity(tree.spatial_nodes.len());
        tree.visit_spatial_nodes_parents_first(|index| order.push(index));
        let mut rects = InterestRects {
            by_spatial_node: vec![None; tree.spatial_nodes.len()],
            published_rects_of_changed_scroll_nodes: FastMap::default(),
        };
        let mut recorded = RecordedInterest {
            recorded_for_testing: for_testing,
            ..RecordedInterest::default()
        };
        let mut changed_scroll_nodes = ScrollNodeSet::default();
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
            let geometry = self.scroll_node_geometry(owner, state_slot);
            // Filters that spread the node's content move it beyond its scrollport, as transforms above it do.
            let placement_in_parent = cull_spaces.cull_space_of(
                tree,
                ContextRef {
                    spatial: node.parent,
                    clip: ClipNodeIndex::NONE,
                    effect: self.data(owner).accumulated_visual_context_for_descendants.effect,
                },
            );
            // The parent culls by what it records, which reaches past what it aims for, so a nested node is cut by that.
            let parent_recorded = match placement_in_parent {
                CullSpace::ScrollNode {
                    scroll_node: parent,
                    placed_exactly: true,
                } => rects.rect_of(parent),
                _ => None,
            };
            let view = view_to_record_around(
                geometry.scrollport,
                geometry.scroll_offset,
                parent_recorded,
                geometry.range.axes,
            );
            let scrollport_size = IntSize {
                width: geometry.scrollport.width,
                height: geometry.scrollport.height,
            };
            let named = FfiScrollNodeContentRect {
                scrollable_node_id: self.data(owner).node_identity,
                scroll_node_kind: kind,
                pseudo_element_type: self.source.node_generated_for(owner).saturating_sub(1),
                rect: IntRect::default(),
            };
            let aimed = aimed_interest_rect(
                view,
                requested.and_then(|requested| requested.rect_requested_for(&named)),
                kept_from.and_then(|published| published.aimed_rect_of(owner)),
                scrollport_size,
                geometry.range,
            );
            let culled_by = interest_rect_recorded_for(aimed, geometry.range);
            rects.by_spatial_node[index] = Some(culled_by);
            let published_rect = published_nodes
                .as_ref()
                .and_then(|published| published.culled_by_rect_of(scroll_node, owner));
            if published_rect != Some(culled_by) {
                changed_scroll_nodes.insert(scroll_node);
                rects
                    .published_rects_of_changed_scroll_nodes
                    .insert(scroll_node, published_rect);
            }
            recorded.scroll_node_rects.push(FfiScrollNodeContentRect {
                rect: culled_by,
                ..named
            });
            recorded.scroll_nodes.push(ScrollNodeInterest {
                scroll_node,
                owner,
                aimed,
            });
        }
        let cull_mapping = cull_mapping_of(tree, cull_spaces);
        let places_every_producer_again =
            published.and_then(|published| published.cull_mapping.as_ref()) != Some(&cull_mapping);
        recorded.cull_mapping = Some(cull_mapping);
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
    fn interest_of_source_recording(
        &self,
        tree: &VisualContextTree,
        cull_spaces: &CullSpaces,
        published: &Arc<RecordedInterest>,
    ) -> ResolvedInterest {
        let published_nodes = ScrollNodesByOwner::new(published);
        let mut rects = InterestRects {
            by_spatial_node: vec![None; tree.spatial_nodes.len()],
            published_rects_of_changed_scroll_nodes: FastMap::default(),
        };
        let mut changed_scroll_nodes = ScrollNodeSet::default();
        for (index, node) in tree.spatial_nodes.iter().enumerate() {
            let scroll_node = SpatialNodeIndex(index as u32);
            let SpatialData::Scroll(ScrollData {
                owner_paintable: owner, ..
            }) = node.data
            else {
                continue;
            };
            if !cull_spaces.is_scrolled_by_compositor(scroll_node) {
                continue;
            }
            match published_nodes.culled_by_rect_of(scroll_node, owner) {
                Some(rect) => rects.by_spatial_node[index] = Some(rect),
                None => {
                    changed_scroll_nodes.insert(scroll_node);
                    rects.published_rects_of_changed_scroll_nodes.insert(scroll_node, None);
                }
            }
        }
        let places_every_producer_again = published.cull_mapping.as_ref() != Some(&cull_mapping_of(tree, cull_spaces));
        ResolvedInterest {
            rects,
            culling: AssemblyCulling::ByInterest {
                changed_scroll_nodes,
                places_every_producer_again,
            },
            recorded: ResolvedRecordedInterest::AsSourceRecording(Arc::clone(published)),
        }
    }

    fn scroll_node_geometry(&mut self, owner: NodeSlotId, state_slot: usize) -> ScrollNodeGeometry {
        let scrollport = self.compositor_scrollport_rect(owner);
        let (minimum_offset, maximum_offset) = self.device_scroll_offset_range(owner);
        let own_offset = if state_slot == NO_SCROLL_STATE_SLOT {
            Default::default()
        } else {
            self.paint_state.scroll_own_offset(state_slot)
        };
        // Own offsets are kept negated: a node scrolled down has a negative own offset.
        let scale = self.inputs.device_pixels_per_css_pixel as f32;
        let scroll_offset = FloatPoint {
            x: -own_offset.x.to_float() * scale,
            y: -own_offset.y.to_float() * scale,
        };
        ScrollNodeGeometry {
            scrollport,
            scroll_offset,
            range: ScrollRange::new(scrollport, minimum_offset, maximum_offset),
        }
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
}
