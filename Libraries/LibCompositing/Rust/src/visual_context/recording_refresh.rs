/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! When the compositor asks WebContent to record a display list again, and around which rects. It looks once per frame
//! it presents: how fast each scroll node scrolls, how long a new recording takes to arrive, and from those, whether what
//! the display list records of each node that shows still holds it until a new recording could arrive, and what to ask
//! for of every node that shows.

use super::ffi_types::{FfiDesiredInterestRect, FfiRecordingRefreshPlan, FfiRecordingRefreshScrollNode};
use super::interest::{
    CullSpace, ScrollRange, area_outside, cull_space_of, desired_interest_rect, interest_rect_recorded_for,
    interest_refresh_needed, view_to_record_around,
};
use super::{ClipNodeIndex, ContextRef, EffectNodeIndex, SpatialData, VisualContextTree};
use crate::fast_hash::{FastMap, FastSet};
use libgfx_rust::{FloatPoint, IntSize};
use std::collections::VecDeque;

/// How many of the latest refresh latencies the estimate takes the median of.
const RECENT_REFRESH_LATENCY_SAMPLE_COUNT: usize = 9;

/// How long a recording that the compositor asked for takes to arrive: the median of the latest times from asking to
/// installing the display list that answers, never less than a frame, and two frames before any arrived.
#[derive(Clone, Debug, Default)]
pub struct RefreshLatencyEstimator {
    recent_samples_in_seconds: VecDeque<f64>,
}

impl RefreshLatencyEstimator {
    pub fn note_sample(&mut self, seconds: f64) {
        if !seconds.is_finite() || seconds < 0.0 {
            return;
        }
        if self.recent_samples_in_seconds.len() == RECENT_REFRESH_LATENCY_SAMPLE_COUNT {
            self.recent_samples_in_seconds.pop_front();
        }
        self.recent_samples_in_seconds.push_back(seconds);
    }

    pub fn latency_in_seconds(&self, frame_interval_in_seconds: f64) -> f64 {
        let frame_interval = if frame_interval_in_seconds.is_finite() {
            frame_interval_in_seconds.max(0.0)
        } else {
            0.0
        };
        if self.recent_samples_in_seconds.is_empty() {
            return 2.0 * frame_interval;
        }
        let mut samples: Vec<f64> = self.recent_samples_in_seconds.iter().copied().collect();
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2].max(frame_interval)
    }
}

#[derive(Clone, Copy, Debug)]
struct OffsetSample {
    time_in_seconds: f64,
    offset: (f64, f64),
}

/// How fast a scroll node scrolls, from where the frames the compositor presents show it. A move since the last frame
/// that the motion so far, the move before it and any smooth scroll the compositor runs all leave unexplained by more
/// than the scrollport is a jump, which restarts the estimate: the frames before it tell nothing of where the node goes
/// next. A fast scroll moves further than the scrollport from frame to frame too, but it moves on as it did.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollMotionEstimator {
    last_sample: Option<OffsetSample>,
    // In device pixels per second, positive toward the bottom right.
    velocity: (f64, f64),
    // How far the node moved between the last two frames.
    last_move: (f64, f64),
    jumped_at_last_sample: bool,
    // Whether a request asked for the least the node needs right after a jump, and the node still waits for a request
    // for its full rect.
    owes_request_after_jump: bool,
}

impl ScrollMotionEstimator {
    /// Takes where the node is scrolled to at a frame, and whether a smooth scroll the compositor runs moved it there.
    /// The velocity follows the motion between frames with the refresh latency as its time constant, so that it tells
    /// how fast the node scrolls over about the time it looks ahead.
    pub fn note_offset(
        &mut self,
        time_in_seconds: f64,
        offset: FloatPoint,
        scrollport_size: IntSize,
        latency: f64,
        moved_by_smooth_scroll: bool,
    ) {
        let offset = (f64::from(offset.x), f64::from(offset.y));
        let sample = OffsetSample {
            time_in_seconds,
            offset,
        };
        let Some(last_sample) = self.last_sample.replace(sample) else {
            self.jumped_at_last_sample = false;
            return;
        };
        let moved = (offset.0 - last_sample.offset.0, offset.1 - last_sample.offset.1);
        if !moved.0.is_finite() || !moved.1.is_finite() {
            self.velocity = (0.0, 0.0);
            self.last_move = (0.0, 0.0);
            self.jumped_at_last_sample = false;
            return;
        }
        let elapsed = time_in_seconds - last_sample.time_in_seconds;
        let elapsed_is_known = elapsed.is_finite() && elapsed > 0.0;
        let expected_move = if elapsed_is_known {
            (self.velocity.0 * elapsed, self.velocity.1 * elapsed)
        } else {
            (0.0, 0.0)
        };
        let leaves_unexplained = |moved: f64, expected_move: f64, last_move: f64, scrollport_extent: i32| {
            let scrollport_extent = f64::from(scrollport_extent.max(0));
            (moved - expected_move).abs() > scrollport_extent && (moved - last_move).abs() > scrollport_extent
        };
        self.jumped_at_last_sample = !moved_by_smooth_scroll
            && (leaves_unexplained(moved.0, expected_move.0, self.last_move.0, scrollport_size.width)
                || leaves_unexplained(moved.1, expected_move.1, self.last_move.1, scrollport_size.height));
        self.last_move = moved;
        if self.jumped_at_last_sample {
            self.velocity = (0.0, 0.0);
            return;
        }
        if !elapsed_is_known {
            return;
        }
        let following = if latency > 0.0 {
            1.0 - (-elapsed / latency).exp()
        } else {
            1.0
        };
        self.velocity.0 += following * (moved.0 / elapsed - self.velocity.0);
        self.velocity.1 += following * (moved.1 / elapsed - self.velocity.1);
    }

    pub fn jumped_at_last_sample(&self) -> bool {
        self.jumped_at_last_sample
    }

    /// How far the node is expected to scroll from `offset` within `latency`, unless it just jumped. A smooth scroll the
    /// compositor runs takes it to `smooth_scroll_offset_after_latency`.
    pub fn expected_motion_within(
        &self,
        latency: f64,
        offset: FloatPoint,
        smooth_scroll_offset_after_latency: Option<FloatPoint>,
    ) -> FloatPoint {
        if let Some(offset_after_latency) = smooth_scroll_offset_after_latency {
            return FloatPoint {
                x: offset_after_latency.x - offset.x,
                y: offset_after_latency.y - offset.y,
            };
        }
        if self.jumped_at_last_sample {
            return FloatPoint::default();
        }
        FloatPoint {
            x: (self.velocity.0 * latency) as f32,
            y: (self.velocity.1 * latency) as f32,
        }
    }
}

// A scroll node as the compositor and WebContent name it, which outlives the display lists that record it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ScrollNodeName {
    scrollable_node_id: i64,
    kind: u8,
    pseudo_element_type: u8,
}

impl ScrollNodeName {
    fn of(node: &FfiRecordingRefreshScrollNode) -> Self {
        Self {
            scrollable_node_id: node.scrollable_node_id,
            kind: node.scroll_node_kind,
            pseudo_element_type: node.pseudo_element_type,
        }
    }
}

/// What the compositor keeps of a context to decide when to ask for a new recording: how each scroll node moves, and
/// how long recordings take to arrive.
#[derive(Default)]
pub struct RecordingRefreshPlanner {
    motions: FastMap<ScrollNodeName, ScrollMotionEstimator>,
    latency: RefreshLatencyEstimator,
    // The nodes the last plan asked for rects of, which a request sent after it names.
    nodes_in_last_plan: Vec<ScrollNodeName>,
}

impl RecordingRefreshPlanner {
    pub fn note_refresh_latency(&mut self, seconds: f64) {
        self.latency.note_sample(seconds);
    }

    pub fn refresh_latency_in_seconds(&self, frame_interval_in_seconds: f64) -> f64 {
        self.latency.latency_in_seconds(frame_interval_in_seconds)
    }

    /// Looks at the scroll nodes of the display list as a frame at `time_in_seconds` shows them, and writes what to ask
    /// for of each node that shows into `desired_rects`, one per node. `tree` is the display list's visual context tree.
    pub fn plan(
        &mut self,
        tree: &VisualContextTree,
        time_in_seconds: f64,
        frame_interval_in_seconds: f64,
        nodes: &[FfiRecordingRefreshScrollNode],
        desired_rects: &mut [FfiDesiredInterestRect],
    ) -> FfiRecordingRefreshPlan {
        assert_eq!(nodes.len(), desired_rects.len());
        let latency = self.latency.latency_in_seconds(frame_interval_in_seconds);
        let names_in_list: FastSet<ScrollNodeName> = nodes.iter().map(ScrollNodeName::of).collect();
        self.motions.retain(|name, _| names_in_list.contains(name));
        self.nodes_in_last_plan.clear();
        desired_rects.fill(FfiDesiredInterestRect::default());

        let mut position_of_scroll_node = vec![None; tree.spatial_nodes.len()];
        for (position, node) in nodes.iter().enumerate() {
            let index = node.scroll_node_index.0 as usize;
            if tree.spatial_is_live(node.scroll_node_index)
                && matches!(tree.spatial_nodes[index].data, SpatialData::Scroll(_))
            {
                position_of_scroll_node[index] = Some(position);
            }
        }
        let mut positions_parents_first = Vec::with_capacity(nodes.len());
        tree.visit_spatial_nodes_parents_first(|index| {
            if let Some(position) = position_of_scroll_node[index] {
                positions_parents_first.push(position);
            }
        });

        let mut plan = FfiRecordingRefreshPlan {
            in_flight_request_holds: true,
            ..FfiRecordingRefreshPlan::default()
        };
        // What a recording of each node's desired rect records, which cuts the nodes placed exactly in it.
        let mut recorded_of_desired = vec![None; nodes.len()];
        for position in positions_parents_first {
            let node = &nodes[position];
            let name = ScrollNodeName::of(node);
            let scrollport_size = IntSize {
                width: node.scrollport.width,
                height: node.scrollport.height,
            };
            let motion_estimator = self.motions.entry(name).or_default();
            motion_estimator.note_offset(
                time_in_seconds,
                node.scroll_offset,
                scrollport_size,
                latency,
                node.has_smooth_scroll,
            );
            if !node.shows {
                continue;
            }

            let range = ScrollRange::new(node.scrollport, node.minimum_scroll_offset, node.maximum_scroll_offset);
            // WebContent records all of such a node, and cuts nothing in it by its rect.
            if range.reaches_past_unbounded_edges {
                continue;
            }
            let placement_in_parent = cull_space_of(
                tree,
                ContextRef {
                    spatial: tree.spatial_nodes[node.scroll_node_index.0 as usize].parent,
                    clip: ClipNodeIndex::NONE,
                    effect: EffectNodeIndex::NONE,
                },
                |index| position_of_scroll_node[index.0 as usize].is_some(),
                &[],
            );
            let parent_recorded = match placement_in_parent {
                CullSpace::ScrollNode {
                    scroll_node,
                    placed_exactly: true,
                } => position_of_scroll_node[scroll_node.0 as usize].and_then(|parent| recorded_of_desired[parent]),
                _ => None,
            };
            let view = view_to_record_around(node.scrollport, node.scroll_offset, parent_recorded, range.axes);
            let smooth_scroll_offset_after_latency = node
                .has_smooth_scroll
                .then_some(node.smooth_scroll_offset_after_refresh_latency);
            let after_jump = motion_estimator.jumped_at_last_sample();
            let motion = motion_estimator.expected_motion_within(
                latency,
                node.scroll_offset,
                smooth_scroll_offset_after_latency,
            );
            let desired = desired_interest_rect(
                view,
                node.visible_content_rect,
                scrollport_size,
                range,
                motion,
                after_jump,
            );
            // WebContent would record nothing of a node asked for nothing, which answers no request.
            if desired.is_empty() {
                continue;
            }
            recorded_of_desired[position] = Some(interest_rect_recorded_for(desired, range));
            desired_rects[position] = FfiDesiredInterestRect {
                shows: true,
                rect: desired,
            };
            self.nodes_in_last_plan.push(name);

            if !node.records_part {
                continue;
            }
            let needed = |recorded| {
                interest_refresh_needed(
                    recorded,
                    node.visible_content_rect,
                    desired,
                    scrollport_size,
                    range.axes,
                    motion,
                )
            };
            let needs_refresh = needed(node.recorded_content_rect);
            plan.some_node_needs_refresh |= needs_refresh;
            plan.some_node_owes_request_after_jump |= motion_estimator.owes_request_after_jump
                && !after_jump
                && !node.recorded_content_rect.contains_rect(desired);
            if needs_refresh
                && !(node.in_flight_request_names_node
                    && !needed(interest_rect_recorded_for(node.in_flight_rect, range)))
            {
                plan.in_flight_request_holds = false;
            }
            plan.checkerboarded_area = plan
                .checkerboarded_area
                .saturating_add(area_outside(node.visible_content_rect, node.recorded_content_rect));
        }
        plan
    }

    /// A node that asked for the least it needs right after a jump asks for its full rect once nothing is in flight.
    pub fn note_request_sent(&mut self) {
        for name in &self.nodes_in_last_plan {
            if let Some(motion_estimator) = self.motions.get_mut(name) {
                motion_estimator.owes_request_after_jump = motion_estimator.jumped_at_last_sample;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_context::interest::{
        LeadPerSide, ScrollableAxes, UNBOUNDED_INTEREST_EDGE, default_interest_rect, interest_rect_covers,
        snap_to_interest_grid,
    };
    use crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
    use crate::visual_context::{ScrollData, TransformData, TransformDataRole, VISUAL_VIEWPORT_NODE_INDEX};
    use libgfx_rust::{FloatMatrix4x4, IntRect};

    const FRAME: f64 = 1.0 / 60.0;
    const UNBOUNDED: i32 = UNBOUNDED_INTEREST_EDGE;
    const VERTICAL: ScrollableAxes = ScrollableAxes {
        horizontal: false,
        vertical: true,
    };
    const HORIZONTAL: ScrollableAxes = ScrollableAxes {
        horizontal: true,
        vertical: false,
    };
    const BOTH: ScrollableAxes = ScrollableAxes {
        horizontal: true,
        vertical: true,
    };

    fn size(width: i32, height: i32) -> IntSize {
        IntSize { width, height }
    }

    fn point(x: f32, y: f32) -> FloatPoint {
        FloatPoint { x, y }
    }

    fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> IntRect {
        IntRect::new(left, top, right - left, bottom - top)
    }

    fn range_of(scrollport: IntRect, minimum: FloatPoint, maximum: FloatPoint) -> ScrollRange {
        ScrollRange::new(scrollport, minimum, maximum)
    }

    #[test]
    fn the_refresh_latency_is_the_median_of_recent_samples_and_at_least_a_frame() {
        let mut latency = RefreshLatencyEstimator::default();
        assert_eq!(latency.latency_in_seconds(FRAME), 2.0 * FRAME);

        latency.note_sample(0.001);
        assert_eq!(latency.latency_in_seconds(FRAME), FRAME);

        for sample in [0.050, 0.040, 0.300, 0.045] {
            latency.note_sample(sample);
        }
        assert_eq!(latency.latency_in_seconds(FRAME), 0.045);

        latency.note_sample(f64::NAN);
        latency.note_sample(-1.0);
        assert_eq!(latency.latency_in_seconds(FRAME), 0.045);

        // Only the latest samples count.
        for _ in 0..RECENT_REFRESH_LATENCY_SAMPLE_COUNT {
            latency.note_sample(0.120);
        }
        assert_eq!(latency.latency_in_seconds(FRAME), 0.120);
    }

    #[test]
    fn a_steady_scroll_is_expected_to_carry_on_and_a_still_node_to_stay() {
        let scrollport_size = size(1000, 800);
        let latency = 0.1;
        let mut motion = ScrollMotionEstimator::default();
        // 1200 device pixels per second downward, 20 px per frame.
        for frame in 0..120 {
            motion.note_offset(
                frame as f64 * FRAME,
                point(0.0, 20.0 * frame as f32),
                scrollport_size,
                latency,
                false,
            );
            assert!(!motion.jumped_at_last_sample());
        }
        let expected = motion.expected_motion_within(latency, point(0.0, 2380.0), None);
        assert!((expected.y - 120.0).abs() < 1.0, "{expected:?}");
        assert!(expected.x.abs() < 0.001);

        // Once it stops, the expected motion fades.
        for frame in 120..240 {
            motion.note_offset(
                frame as f64 * FRAME,
                point(0.0, 2380.0),
                scrollport_size,
                latency,
                false,
            );
        }
        let expected = motion.expected_motion_within(latency, point(0.0, 2380.0), None);
        assert!(expected.y.abs() < 1.0, "{expected:?}");

        // After an idle spell, the first frame shows the node has stood still all along.
        motion.note_offset(100.0, point(0.0, 2400.0), scrollport_size, latency, false);
        let expected = motion.expected_motion_within(latency, point(0.0, 2400.0), None);
        assert!(expected.y.abs() < 1.0, "{expected:?}");
    }

    #[test]
    fn a_move_past_the_scrollport_is_a_jump_that_restarts_the_estimate() {
        let scrollport_size = size(1000, 800);
        let latency = 0.1;
        let mut motion = ScrollMotionEstimator::default();
        motion.note_offset(0.0, point(0.0, 0.0), scrollport_size, latency, false);
        motion.note_offset(FRAME, point(0.0, 400.0), scrollport_size, latency, false);
        assert!(!motion.jumped_at_last_sample());
        assert!(motion.expected_motion_within(latency, point(0.0, 400.0), None).y > 0.0);

        motion.note_offset(2.0 * FRAME, point(0.0, 9000.0), scrollport_size, latency, false);
        assert!(motion.jumped_at_last_sample());
        assert_eq!(
            motion.expected_motion_within(latency, point(0.0, 9000.0), None),
            FloatPoint::default()
        );

        motion.note_offset(3.0 * FRAME, point(0.0, 9000.0), scrollport_size, latency, false);
        assert!(!motion.jumped_at_last_sample());
        assert_eq!(
            motion.expected_motion_within(latency, point(0.0, 9000.0), None),
            FloatPoint::default()
        );

        // A second jump right after a stop is a jump too.
        motion.note_offset(4.0 * FRAME, point(0.0, 100.0), scrollport_size, latency, false);
        assert!(motion.jumped_at_last_sample());
    }

    #[test]
    fn a_scroll_faster_than_a_scrollport_per_frame_is_no_jump_once_it_moves_on_as_it_did() {
        let scrollport_size = size(1000, 800);
        let latency = FRAME;
        let mut motion = ScrollMotionEstimator::default();
        motion.note_offset(0.0, point(0.0, 0.0), scrollport_size, latency, false);
        // From standing still, the first frame of a fling that moves 1200 px per frame cannot be told from a jump.
        motion.note_offset(FRAME, point(0.0, 1200.0), scrollport_size, latency, false);
        assert!(motion.jumped_at_last_sample());
        for frame in 2..10 {
            motion.note_offset(
                frame as f64 * FRAME,
                point(0.0, 1200.0 * frame as f32),
                scrollport_size,
                latency,
                false,
            );
            assert!(!motion.jumped_at_last_sample(), "frame {frame}");
        }
        let expected = motion.expected_motion_within(latency, point(0.0, 10800.0), None);
        assert!(expected.y > 1000.0, "{expected:?}");

        // A smooth scroll the compositor runs explains any move, its first one included.
        let mut smooth = ScrollMotionEstimator::default();
        smooth.note_offset(0.0, point(0.0, 0.0), scrollport_size, latency, true);
        smooth.note_offset(FRAME, point(0.0, 2000.0), scrollport_size, latency, true);
        assert!(!smooth.jumped_at_last_sample());
    }

    #[test]
    fn a_smooth_scroll_is_expected_to_take_the_node_where_it_goes_within_the_latency() {
        let scrollport_size = size(1000, 800);
        let latency = 0.05;
        let mut motion = ScrollMotionEstimator::default();
        motion.note_offset(0.0, point(0.0, 1000.0), scrollport_size, latency, true);
        motion.note_offset(FRAME, point(0.0, 1100.0), scrollport_size, latency, true);
        assert_eq!(
            motion.expected_motion_within(latency, point(0.0, 1100.0), Some(point(0.0, 1900.0))),
            point(0.0, 800.0)
        );
        assert_ne!(
            motion.expected_motion_within(latency, point(0.0, 1100.0), None),
            point(0.0, 800.0)
        );
    }

    #[test]
    fn a_still_node_asks_for_a_scrollport_past_each_side_it_can_scroll() {
        let scrollport = IntRect::new(0, 0, 1000, 800);
        let scrollport_size = size(1000, 800);
        let range = range_of(scrollport, point(0.0, 0.0), point(0.0, 20000.0));
        let view = view_to_record_around(scrollport, point(0.0, 5000.0), None, VERTICAL);
        let visible = IntRect::new(0, 5000, 1000, 800);
        let desired = desired_interest_rect(view, visible, scrollport_size, range, FloatPoint::default(), false);
        assert_eq!(desired, default_interest_rect(view, scrollport_size, VERTICAL));
        assert_eq!(desired, from_edges(-UNBOUNDED, 4200, UNBOUNDED, 6600));

        let right_after_a_jump = desired_interest_rect(view, visible, scrollport_size, range, point(0.0, 3000.0), true);
        assert_eq!(right_after_a_jump, from_edges(-UNBOUNDED, 4600, UNBOUNDED, 6200));
        assert!(interest_rect_covers(
            interest_rect_recorded_for(right_after_a_jump, range),
            visible,
            scrollport_size,
            LeadPerSide::default(),
            VERTICAL
        ));
    }

    #[test]
    fn a_moving_node_asks_for_more_toward_where_it_heads_within_the_area_cap() {
        let scrollport = IntRect::new(0, 0, 1000, 800);
        let scrollport_size = size(1000, 800);
        let range = range_of(scrollport, point(0.0, 0.0), point(0.0, 40000.0));
        let view = view_to_record_around(scrollport, point(0.0, 5000.0), None, VERTICAL);
        let visible = IntRect::new(0, 5000, 1000, 800);

        let heading_down = desired_interest_rect(view, visible, scrollport_size, range, point(0.0, 500.0), false);
        assert_eq!(heading_down, from_edges(-UNBOUNDED, 4200, UNBOUNDED, 7100));
        let heading_up = desired_interest_rect(view, visible, scrollport_size, range, point(0.0, -500.0), false);
        assert_eq!(heading_up, from_edges(-UNBOUNDED, 3700, UNBOUNDED, 6600));

        // A stationary rect is 2400 px tall here, so the lead may add another 2400 px at most.
        let flung_down = desired_interest_rect(view, visible, scrollport_size, range, point(0.0, 10000.0), false);
        assert_eq!(flung_down, from_edges(-UNBOUNDED, 4200, UNBOUNDED, 9000));

        // Content past the end of the scroll range counts for nothing, so a lead toward the end is not cut.
        let near_the_end = view_to_record_around(scrollport, point(0.0, 38000.0), None, VERTICAL);
        let visible_near_the_end = IntRect::new(0, 38000, 1000, 800);
        let toward_the_end = desired_interest_rect(
            near_the_end,
            visible_near_the_end,
            scrollport_size,
            range,
            point(0.0, 10000.0),
            false,
        );
        assert_eq!(toward_the_end, from_edges(-UNBOUNDED, 37200, UNBOUNDED, 49600));

        // On both axes, the cap shares the area between the leads.
        let scroller = IntRect::new(0, 0, 400, 300);
        let both_ways = range_of(scroller, point(0.0, 0.0), point(10000.0, 10000.0));
        let view_both_ways = view_to_record_around(scroller, point(4000.0, 4000.0), None, BOTH);
        let visible_both_ways = IntRect::new(4000, 4000, 400, 300);
        let diagonal = desired_interest_rect(
            view_both_ways,
            visible_both_ways,
            size(400, 300),
            both_ways,
            point(4000.0, 3000.0),
            false,
        );
        let area = |rect: IntRect| i64::from(rect.width) * i64::from(rect.height);
        assert!(area(diagonal) <= 2 * 1200 * 900);
        assert!(area(diagonal) > 2 * 1200 * 900 * 99 / 100);
        assert!(diagonal.contains_rect(from_edges(3600, 3700, 4800, 4600)));
    }

    #[test]
    fn what_a_recording_of_the_desired_rect_records_holds_until_the_node_scrolls_on() {
        let scrollport_sizes = [size(1, 1), size(300, 200), size(511, 513), size(1000, 800)];
        let every_axes = [
            ScrollableAxes {
                horizontal: false,
                vertical: false,
            },
            HORIZONTAL,
            VERTICAL,
            BOTH,
        ];
        let motions = [
            point(0.0, 0.0),
            point(0.0, 150.0),
            point(-700.0, 0.0),
            point(3000.0, -9000.0),
            point(f32::INFINITY, f32::NAN),
        ];
        let scroll_ranges = [(0, 0), (0, 5000), (-3000, 0)];
        for scrollport_size in scrollport_sizes {
            for axes in every_axes {
                for (minimum_offset, maximum_offset) in scroll_ranges {
                    let scrollport = IntRect::new(-700, 37, scrollport_size.width, scrollport_size.height);
                    let range_on = |scrollable: bool| {
                        if scrollable {
                            (minimum_offset, maximum_offset)
                        } else {
                            (0, 0)
                        }
                    };
                    let (minimum_x, maximum_x) = range_on(axes.horizontal);
                    let (minimum_y, maximum_y) = range_on(axes.vertical);
                    let range = range_of(
                        scrollport,
                        point(minimum_x as f32, minimum_y as f32),
                        point(maximum_x as f32, maximum_y as f32),
                    );
                    for step in 0..=8 {
                        let offset_x = minimum_x + (maximum_x - minimum_x) * step / 8;
                        let offset_y = minimum_y + (maximum_y - minimum_y) * step / 8;
                        let offset = point(offset_x as f32, offset_y as f32);
                        let visible = IntRect::new(
                            scrollport.x + offset_x,
                            scrollport.y + offset_y,
                            scrollport.width,
                            scrollport.height,
                        );
                        let view = view_to_record_around(scrollport, offset, Some(scrollport), range.axes);
                        for motion in motions {
                            for after_jump in [false, true] {
                                let desired =
                                    desired_interest_rect(view, visible, scrollport_size, range, motion, after_jump);
                                assert!(desired.contains_rect(visible));
                                let recorded = interest_rect_recorded_for(desired, range);
                                assert!(
                                    !interest_refresh_needed(
                                        recorded,
                                        visible,
                                        desired,
                                        scrollport_size,
                                        range.axes,
                                        motion
                                    ),
                                    "{recorded:?} from {desired:?} needs a refresh for {visible:?} ({scrollport_size:?}, {range:?}, {motion:?})"
                                );
                                // WebContent takes the rect: what it records for it holds what the node shows.
                                assert!(interest_rect_covers(
                                    recorded,
                                    view,
                                    scrollport_size,
                                    LeadPerSide::default(),
                                    range.axes
                                ));
                                if motion == point(0.0, 0.0) && !after_jump {
                                    assert!(interest_rect_covers(
                                        snap_to_interest_grid(desired),
                                        visible,
                                        scrollport_size,
                                        LeadPerSide::default(),
                                        range.axes
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_recording_needs_a_refresh_once_scrolling_carries_the_node_close_to_its_edge() {
        let scrollport = IntRect::new(0, 0, 1000, 800);
        let scrollport_size = size(1000, 800);
        let range = range_of(scrollport, point(0.0, 0.0), point(0.0, 20000.0));
        let recorded = from_edges(-UNBOUNDED, 4096, UNBOUNDED, 6656);
        let needed_at = |offset: i32, motion: FloatPoint| {
            let view = view_to_record_around(scrollport, point(0.0, offset as f32), None, VERTICAL);
            let visible = IntRect::new(0, offset, 1000, 800);
            let desired = desired_interest_rect(view, visible, scrollport_size, range, motion, false);
            interest_refresh_needed(recorded, visible, desired, scrollport_size, VERTICAL, motion)
        };
        assert!(!needed_at(5000, FloatPoint::default()));
        assert!(!needed_at(5400, FloatPoint::default()));
        assert!(needed_at(5600, FloatPoint::default()));
        // 856 px are left below what shows, which holds half a scrollport and a lead of 456 px.
        assert!(!needed_at(5000, point(0.0, 400.0)));
        assert!(needed_at(5000, point(0.0, 500.0)));
        assert!(!needed_at(5000, point(0.0, -400.0)));
        assert!(needed_at(4300, point(0.0, -400.0)));
    }

    fn identity_transform() -> TransformData {
        TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
        }
    }

    fn scroll() -> SpatialData {
        SpatialData::Scroll(ScrollData {
            state_slot: NO_SCROLL_STATE_SLOT,
            owner_paintable: NodeSlotId::INVALID,
            registry_parent_node: VISUAL_VIEWPORT_NODE_INDEX,
        })
    }

    struct Page {
        tree: VisualContextTree,
        root: FfiRecordingRefreshScrollNode,
        nested: FfiRecordingRefreshScrollNode,
    }

    // A root scrolled to 5000 of 20000 px, with a box 200 px into what it shows that scrolls 600 px sideways, placed in
    // the root through a transform when `nested_is_transformed`.
    fn page(nested_is_transformed: bool) -> Page {
        let mut tree = VisualContextTree::create(identity_transform());
        let root_index = tree.append_spatial(scroll(), VISUAL_VIEWPORT_NODE_INDEX);
        let nested_parent = if nested_is_transformed {
            tree.append_spatial(SpatialData::Transform(identity_transform()), root_index)
        } else {
            root_index
        };
        let nested_index = tree.append_spatial(scroll(), nested_parent);
        let root_scrollport = IntRect::new(0, 0, 1000, 800);
        let root = FfiRecordingRefreshScrollNode {
            scrollable_node_id: 1,
            scroll_node_kind: 0,
            scroll_node_index: root_index,
            scrollport: root_scrollport,
            scroll_offset: point(0.0, 5000.0),
            maximum_scroll_offset: point(0.0, 20000.0),
            shows: true,
            visible_content_rect: IntRect::new(0, 5000, 1000, 800),
            records_part: true,
            recorded_content_rect: from_edges(-UNBOUNDED, 4096, UNBOUNDED, 6656),
            ..FfiRecordingRefreshScrollNode::default()
        };
        let nested_scrollport = IntRect::new(100, 5200, 400, 300);
        let nested = FfiRecordingRefreshScrollNode {
            scrollable_node_id: 2,
            scroll_node_kind: 1,
            scroll_node_index: nested_index,
            scrollport: nested_scrollport,
            maximum_scroll_offset: point(600.0, 0.0),
            shows: true,
            visible_content_rect: nested_scrollport,
            records_part: true,
            recorded_content_rect: from_edges(-UNBOUNDED, 4096, 1024, 6656),
            ..FfiRecordingRefreshScrollNode::default()
        };
        Page { tree, root, nested }
    }

    fn plan_of(
        planner: &mut RecordingRefreshPlanner,
        page: &Page,
        time: f64,
    ) -> (FfiRecordingRefreshPlan, [FfiDesiredInterestRect; 2]) {
        let mut desired_rects = [FfiDesiredInterestRect::default(); 2];
        let plan = planner.plan(&page.tree, time, FRAME, &[page.root, page.nested], &mut desired_rects);
        (plan, desired_rects)
    }

    #[test]
    fn a_nested_node_placed_exactly_is_cut_by_what_the_root_asks_for() {
        let mut planner = RecordingRefreshPlanner::default();
        let placed_exactly = page(false);
        let (plan, desired_rects) = plan_of(&mut planner, &placed_exactly, 0.0);
        assert!(!plan.some_node_needs_refresh);
        assert_eq!(plan.checkerboarded_area, 0);
        assert!(desired_rects.iter().all(|desired| desired.shows));
        let root_range = range_of(placed_exactly.root.scrollport, point(0.0, 0.0), point(0.0, 20000.0));
        let root_recorded = interest_rect_recorded_for(desired_rects[0].rect, root_range);
        // The same rule WebContent cuts the box by when it takes the request.
        let view = view_to_record_around(
            placed_exactly.nested.scrollport,
            placed_exactly.nested.scroll_offset,
            Some(root_recorded),
            HORIZONTAL,
        );
        assert_eq!(view, from_edges(100, 5200, 500, 5500));
        assert_eq!(desired_rects[1].rect, from_edges(-300, 5200, 900, 5500));

        let transformed = page(true);
        let (_, desired_rects) = plan_of(&mut planner, &transformed, FRAME);
        assert_eq!(desired_rects[1].rect, from_edges(-300, -UNBOUNDED, 900, UNBOUNDED));
    }

    #[test]
    fn a_node_whose_content_reaches_past_the_unbounded_edges_is_asked_for_nothing_and_cuts_nothing() {
        let mut planner = RecordingRefreshPlanner::default();
        let mut page = page(false);
        // WebContent records all of a root 20000000 px tall, so the list names no rect of it.
        page.root.maximum_scroll_offset = point(0.0, 20_000_000.0);
        page.root.records_part = false;
        let (plan, desired_rects) = plan_of(&mut planner, &page, 0.0);
        assert!(!plan.some_node_needs_refresh);
        assert!(!desired_rects[0].shows);
        // The box in it is cut by nothing on the axis it cannot scroll, as WebContent cuts it.
        assert!(desired_rects[1].shows);
        assert_eq!(desired_rects[1].rect, from_edges(-300, -UNBOUNDED, 900, UNBOUNDED));
    }

    #[test]
    fn a_plan_asks_for_a_refresh_near_the_edge_and_waits_for_a_request_that_holds() {
        let mut planner = RecordingRefreshPlanner::default();
        let mut page = page(false);
        let (plan, _) = plan_of(&mut planner, &page, 0.0);
        assert!(!plan.some_node_needs_refresh);

        page.root.scroll_offset = point(0.0, 5600.0);
        page.root.visible_content_rect = IntRect::new(0, 5600, 1000, 800);
        page.nested.shows = false;
        let (plan, desired_rects) = plan_of(&mut planner, &page, FRAME);
        assert!(plan.some_node_needs_refresh);
        assert!(!plan.in_flight_request_holds);
        assert!(!desired_rects[1].shows);

        page.root.in_flight_request_names_node = true;
        page.root.in_flight_rect = desired_rects[0].rect;
        let (plan, _) = plan_of(&mut planner, &page, 2.0 * FRAME);
        assert!(plan.some_node_needs_refresh);
        assert!(plan.in_flight_request_holds);

        // Content that shows outside what the list records counts as checkerboarded.
        page.root.scroll_offset = point(0.0, 6200.0);
        page.root.visible_content_rect = IntRect::new(0, 6200, 1000, 800);
        let (plan, _) = plan_of(&mut planner, &page, 3.0 * FRAME);
        assert_eq!(plan.checkerboarded_area, 1000 * (7000 - 6656));
    }

    #[test]
    fn after_a_jump_a_node_asks_for_the_least_it_needs_and_then_for_its_full_rect() {
        let mut planner = RecordingRefreshPlanner::default();
        let mut page = page(false);
        page.nested.shows = false;
        let (plan, _) = plan_of(&mut planner, &page, 0.0);
        assert!(!plan.some_node_needs_refresh);

        page.root.scroll_offset = point(0.0, 15000.0);
        page.root.visible_content_rect = IntRect::new(0, 15000, 1000, 800);
        let (plan, desired_rects) = plan_of(&mut planner, &page, FRAME);
        assert!(plan.some_node_needs_refresh);
        assert_eq!(desired_rects[0].rect, from_edges(-UNBOUNDED, 14600, UNBOUNDED, 16200));
        planner.note_request_sent();

        // The answer records what it asked for, and nothing is in flight any more.
        let root_range = range_of(page.root.scrollport, point(0.0, 0.0), point(0.0, 20000.0));
        page.root.recorded_content_rect = interest_rect_recorded_for(desired_rects[0].rect, root_range);
        let (plan, desired_rects) = plan_of(&mut planner, &page, 2.0 * FRAME);
        assert!(!plan.some_node_needs_refresh);
        assert!(plan.some_node_owes_request_after_jump);
        assert_eq!(desired_rects[0].rect, from_edges(-UNBOUNDED, 14200, UNBOUNDED, 16600));
        planner.note_request_sent();

        page.root.recorded_content_rect = interest_rect_recorded_for(desired_rects[0].rect, root_range);
        let (plan, _) = plan_of(&mut planner, &page, 3.0 * FRAME);
        assert!(!plan.some_node_needs_refresh);
        assert!(!plan.some_node_owes_request_after_jump);
    }
}
