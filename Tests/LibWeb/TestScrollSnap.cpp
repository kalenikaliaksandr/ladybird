/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/Compositor/ScrollSnap.h>

using namespace Web;
using namespace Web::Compositor;

static SnapPositionCandidate candidate(int offset, i64 id, bool always_stop = false)
{
    return {
        .offset = offset,
        .area = { UniqueNodeID { id }, {} },
        .covering_ranges = {},
        .cross_axis_visible_range_start = -1000,
        .cross_axis_visible_range_end = 1000,
        .always_stop = always_stop,
    };
}

static SnapContainerData vertical_container()
{
    return {
        .axes = { false, true },
        .strictness = CSS::ScrollSnapStrictness::Mandatory,
        .snapport_size = { 200, 200 },
        .min_scroll_offset = {},
        .max_scroll_offset = { 400, 400 },
        .candidates = { {}, { candidate(0, 1), candidate(200, 2), candidate(400, 3) } },
    };
}

TEST_CASE(mandatory_selects_nearest_position_and_reports_the_area)
{
    auto result = select_snap_destination(vertical_container(), { 0, 120 });
    EXPECT_EQ(result.position, CSSPixelPoint(0, 200));
    EXPECT(result.snapped_y);
    EXPECT(result.evaluated_y);
    EXPECT(!result.evaluated_x);
    EXPECT_EQ(result.snapped_areas.y.size(), 1u);
    EXPECT_EQ(result.snapped_areas.y[0].element_id, UniqueNodeID { 2 });
}

TEST_CASE(proximity_only_snaps_within_one_third_of_the_snapport)
{
    auto data = vertical_container();
    data.strictness = CSS::ScrollSnapStrictness::Proximity;
    auto distant = select_snap_destination(data, { 0, 80 });
    EXPECT_EQ(distant.position, CSSPixelPoint(0, 80));
    EXPECT(!distant.snapped_y);
    auto nearby = select_snap_destination(data, { 0, 150 });
    EXPECT_EQ(nearby.position, CSSPixelPoint(0, 200));
    EXPECT(nearby.snapped_y);
}

TEST_CASE(direction_and_end_position_choose_different_candidates)
{
    auto data = vertical_container();
    SnapSelectionStrategy directional { SnapSelectionStrategy::Type::Direction, CSSPixelPoint {}, { 0, 250 }, CSSPixelPoint { 0, 250 } };
    EXPECT_EQ(select_snap_destination(data, { 0, 250 }, directional).position, CSSPixelPoint(0, 400));
    SnapSelectionStrategy relative { SnapSelectionStrategy::Type::EndPositionAndDirection, CSSPixelPoint {}, { 0, 250 } };
    EXPECT_EQ(select_snap_destination(data, { 0, 250 }, relative).position, CSSPixelPoint(0, 200));
}

TEST_CASE(stop_always_prevents_passing_a_candidate)
{
    auto data = vertical_container();
    data.candidates.y_candidates[1].always_stop = true;
    SnapSelectionStrategy strategy { SnapSelectionStrategy::Type::EndPositionAndDirection, CSSPixelPoint {}, { 0, 390 } };
    EXPECT_EQ(select_snap_destination(data, { 0, 390 }, strategy).position, CSSPixelPoint(0, 200));
}

TEST_CASE(mandatory_falls_back_when_no_candidate_is_ahead)
{
    SnapSelectionStrategy strategy { SnapSelectionStrategy::Type::Direction, CSSPixelPoint { 0, 400 }, { 0, 10 }, CSSPixelPoint { 0, 410 } };
    EXPECT_EQ(select_snap_destination(vertical_container(), { 0, 400 }, strategy).position, CSSPixelPoint(0, 400));
}

TEST_CASE(relative_scroll_preserves_the_axis_it_did_not_move)
{
    auto data = vertical_container();
    data.axes.x = true;
    data.candidates.x_candidates = { candidate(0, 1), candidate(200, 2) };
    SnapSelectionStrategy strategy { SnapSelectionStrategy::Type::EndPositionAndDirection, CSSPixelPoint { 80, 0 }, { 0, 120 } };
    auto result = select_snap_destination(data, { 80, 120 }, strategy);
    EXPECT_EQ(result.position, CSSPixelPoint(80, 200));
    EXPECT(!result.evaluated_x);
}

TEST_CASE(oversized_area_preserves_positions_in_its_covering_range)
{
    auto data = vertical_container();
    auto area = candidate(0, 1);
    area.covering_ranges.append({ 100, 400 });
    data.candidates.y_candidates = { area };
    auto result = select_snap_destination(data, { 0, 350 });
    EXPECT_EQ(result.position, CSSPixelPoint(0, 350));
    EXPECT(result.snapped_y);
}

TEST_CASE(candidates_outside_the_cross_axis_snapport_are_ineligible)
{
    auto data = vertical_container();
    data.candidates.y_candidates[1].cross_axis_visible_range_start = 100;
    EXPECT_EQ(select_snap_destination(data, { 0, 190 }).position, CSSPixelPoint(0, 0));
}

TEST_CASE(empty_candidates_leave_the_destination_unchanged)
{
    auto data = vertical_container();
    data.candidates.y_candidates.clear();
    auto result = select_snap_destination(data, { 0, 120 });
    EXPECT_EQ(result.position, CSSPixelPoint(0, 120));
    EXPECT(!result.snapped_y);
}

TEST_CASE(momentum_estimation_waits_for_decay_and_can_be_reset)
{
    MomentumFlingEstimator estimator;
    EXPECT(!estimator.estimate_remaining_displacement({ 0, 100 }).has_value());
    auto estimate = estimator.estimate_remaining_displacement({ 0, 80 });
    EXPECT(estimate.has_value());
    EXPECT_EQ(*estimate, CSSPixelPoint(0, 400));
    estimator.reset();
    EXPECT(!estimator.estimate_remaining_displacement({ 0, 80 }).has_value());
    EXPECT(!estimator.estimate_remaining_displacement({ 0, 100 }).has_value());
}
