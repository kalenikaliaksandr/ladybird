/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCompositing/DisplayList/AccumulatedVisualContext.h>
#include <LibCompositing/RustFFI.h>
#include <LibCompositing/Scrolling/RecordingRefreshPlanner.h>

namespace Compositing {

RecordingRefreshPlanner::RecordingRefreshPlanner()
    : m_rust_planner(RustFFI::recording_refresh_planner_create())
{
}

RecordingRefreshPlanner::~RecordingRefreshPlanner()
{
    RustFFI::recording_refresh_planner_destroy(m_rust_planner);
}

RecordingRefreshPlan RecordingRefreshPlanner::plan(AccumulatedVisualContextTree const& visual_context_tree, MonotonicTime now, AK::Duration frame_interval, ReadonlySpan<RecordingRefreshScrollNode> scroll_nodes)
{
    Vector<RustFFI::FfiRecordingRefreshScrollNode> nodes;
    nodes.ensure_capacity(scroll_nodes.size());
    for (auto const& scroll_node : scroll_nodes) {
        nodes.unchecked_append({
            .scrollable_node_id = scroll_node.stable_node_id.node_id.value(),
            .scroll_node_kind = to_underlying(scroll_node.stable_node_id.kind),
            .pseudo_element_type = scroll_node.stable_node_id.pseudo_element_type,
            .scroll_node_index = scroll_node.scroll_node_index,
            .scrollport = scroll_node.scrollport_rect,
            .scroll_offset = scroll_node.scroll_offset,
            .minimum_scroll_offset = scroll_node.min_scroll_offset,
            .maximum_scroll_offset = scroll_node.max_scroll_offset,
            .shows = scroll_node.visible_content_rect.has_value(),
            .visible_content_rect = scroll_node.visible_content_rect.value_or({}),
            .records_part = scroll_node.recorded_content_rect.has_value(),
            .recorded_content_rect = scroll_node.recorded_content_rect.value_or({}),
            .has_smooth_scroll = scroll_node.smooth_scroll_offset_after_refresh_latency.has_value(),
            .smooth_scroll_offset_after_refresh_latency = scroll_node.smooth_scroll_offset_after_refresh_latency.value_or({}),
            .in_flight_request_names_node = scroll_node.in_flight_rect.has_value(),
            .in_flight_rect = scroll_node.in_flight_rect.value_or({}),
        });
    }
    Vector<RustFFI::FfiDesiredInterestRect> desired_rects;
    desired_rects.resize(nodes.size());
    auto rust_plan = RustFFI::recording_refresh_planner_plan(m_rust_planner, visual_context_tree.rust_handle(), now.to_seconds_f64(), frame_interval.to_seconds_f64(), nodes.data(), desired_rects.data(), nodes.size());

    RecordingRefreshPlan plan {
        .some_scroll_node_needs_refresh = rust_plan.some_node_needs_refresh,
        .some_scroll_node_owes_request_after_jump = rust_plan.some_node_owes_request_after_jump,
        .in_flight_request_holds = rust_plan.in_flight_request_holds,
        .checkerboarded_area = rust_plan.checkerboarded_area,
        .desired_rects = {},
    };
    for (size_t i = 0; i < scroll_nodes.size(); ++i) {
        if (desired_rects[i].shows)
            plan.desired_rects.append({ .scroll_node = scroll_nodes[i].stable_node_id, .rect = desired_rects[i].rect });
    }
    return plan;
}

void RecordingRefreshPlanner::note_request_for_last_plan_sent()
{
    RustFFI::recording_refresh_planner_note_request_sent(m_rust_planner);
}

void RecordingRefreshPlanner::note_display_list_answered_request_after(AK::Duration latency)
{
    RustFFI::recording_refresh_planner_note_refresh_latency(m_rust_planner, latency.to_seconds_f64());
}

AK::Duration RecordingRefreshPlanner::refresh_latency(AK::Duration frame_interval) const
{
    return AK::Duration::from_seconds_f64(RustFFI::recording_refresh_planner_refresh_latency(m_rust_planner, frame_interval.to_seconds_f64()));
}

void RecordingRefreshPlanner::forget_how_scroll_nodes_moved_and_recordings_arrived()
{
    RustFFI::recording_refresh_planner_destroy(m_rust_planner);
    m_rust_planner = RustFFI::recording_refresh_planner_create();
}

}
