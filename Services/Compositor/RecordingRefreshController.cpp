/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/JsonObject.h>
#include <Compositor/RecordingRefreshController.h>
#include <LibCompositing/Scrolling/AsyncScrollTree.h>
#include <LibCore/Environment.h>

namespace Compositor {

// A request goes out again once no answer came within this many refresh latencies, which doubles with each resend up
// to this many times.
static constexpr i64 resend_after_refresh_latencies = 4;
static constexpr u32 most_resend_delay_doublings = 4;
// Enough for the requests that one recording can take to answer while the compositor keeps replacing them.
static constexpr size_t recently_sent_request_count = 16;

static bool interest_refresh_logging_enabled()
{
    static bool enabled = [] {
        auto value = Core::Environment::get("LADYBIRD_DISPLAY_LIST_TIMING"sv);
        return value.has_value() && !value->is_empty() && *value != "0"sv;
    }();
    return enabled;
}

Optional<RecordingRefreshController::Request> RecordingRefreshController::request_for_presented_frame(PresentedFrame const& frame)
{
    if (m_request_in_flight.has_value())
        m_request_in_flight->presented_frame_since_sent = true;
    auto const& scroll_tree = frame.scroll_tree;
    auto scroll_nodes = scroll_tree.scroll_nodes();
    auto visibilities = scroll_tree.scroll_node_visibilities();
    if (visibilities.size() != scroll_nodes.size())
        return {};

    auto rect_in_flight_for = [&](Web::AsyncScrollNodeStableID stable_node_id) -> Optional<Gfx::IntRect> {
        if (!m_request_in_flight.has_value())
            return {};
        for (auto const& requested : m_request_in_flight->scroll_node_rects) {
            if (requested.scroll_node == stable_node_id)
                return requested.rect;
        }
        return {};
    };
    auto smooth_scroll_offset_after_refresh_latency_of = [&](Web::AsyncScrollNodeStableID stable_node_id) -> Optional<Gfx::FloatPoint> {
        for (auto const& smooth_scroll : frame.smooth_scrolls) {
            if (smooth_scroll.stable_node_id == stable_node_id)
                return smooth_scroll.offset_after_refresh_latency;
        }
        return {};
    };

    // Only the scroll nodes of the list's own document carry what the list records of them.
    auto document_id = scroll_tree.document_id();
    Vector<Compositing::RecordingRefreshScrollNode> nodes;
    nodes.ensure_capacity(scroll_nodes.size());
    for (size_t position = 0; position < scroll_nodes.size(); ++position) {
        auto const& node = scroll_nodes[position];
        if (node.node_id.document_id != document_id)
            continue;
        nodes.unchecked_append({
            .stable_node_id = node.stable_node_id,
            .scroll_node_index = node.node_id.scroll_node_index,
            .scrollport_rect = node.scrollport_rect,
            .scroll_offset = scroll_tree.scroll_offset_for_node(node.node_id, frame.scroll_state_snapshot).value_or({}),
            .min_scroll_offset = node.min_scroll_offset,
            .max_scroll_offset = node.max_scroll_offset,
            .visible_content_rect = visibilities[position].map([](auto const& visibility) { return visibility.in_content; }),
            .recorded_content_rect = node.recorded_content_rect,
            .smooth_scroll_offset_after_refresh_latency = smooth_scroll_offset_after_refresh_latency_of(node.stable_node_id),
            .in_flight_rect = rect_in_flight_for(node.stable_node_id),
        });
    }

    auto plan = m_planner.plan(frame.visual_context_tree, frame.now, frame.frame_interval, nodes);
    if (plan.checkerboarded_area > 0) {
        ++m_checkerboarded_frame_count;
        m_last_checkerboarded_area = plan.checkerboarded_area;
    }

    auto needs_request = plan.some_scroll_node_needs_refresh
        || (!m_request_in_flight.has_value() && plan.some_scroll_node_owes_request_after_jump);
    if (!needs_request) {
        // What the display list records holds what shows, whether or not it answered the request in flight.
        m_request_in_flight.clear();
        return {};
    }
    u32 resend_count = 0;
    if (m_request_in_flight.has_value() && plan.in_flight_request_holds) {
        if (frame.now < m_request_in_flight->sent_at + resend_delay(m_request_in_flight->resend_count, frame.frame_interval))
            return {};
        resend_count = m_request_in_flight->resend_count + 1;
    }

    Request request { .id = m_next_request_id++, .scroll_node_rects = move(plan.desired_rects) };
    note_sent(request, frame.now, resend_count);
    m_planner.note_request_for_last_plan_sent();
    if (interest_refresh_logging_enabled())
        dbgln("INTEREST_REFRESH request={} scroll_nodes={} resends={} latency={} µs checkerboarded={}", request.id, request.scroll_node_rects.size(), resend_count, m_planner.refresh_latency(frame.frame_interval).to_microseconds(), plan.checkerboarded_area);
    return request;
}

Optional<RecordingRefreshController::Request> RecordingRefreshController::request_to_resend(MonotonicTime now, AK::Duration frame_interval)
{
    if (!m_request_in_flight.has_value() || now < m_request_in_flight->sent_at + resend_delay(m_request_in_flight->resend_count, frame_interval))
        return {};
    Request request { .id = m_next_request_id++, .scroll_node_rects = m_request_in_flight->scroll_node_rects };
    auto resend_count = m_request_in_flight->resend_count + 1;
    note_sent(request, now, resend_count);
    if (interest_refresh_logging_enabled())
        dbgln("INTEREST_REFRESH request={} scroll_nodes={} resends={} latency={} µs without a frame", request.id, request.scroll_node_rects.size(), resend_count, m_planner.refresh_latency(frame_interval).to_microseconds());
    return request;
}

void RecordingRefreshController::note_sent(Request const& request, MonotonicTime now, u32 resend_count)
{
    m_request_in_flight = RequestInFlight { .id = request.id, .sent_at = now, .scroll_node_rects = request.scroll_node_rects, .resend_count = resend_count };
    if (m_recently_sent_requests.size() == recently_sent_request_count)
        m_recently_sent_requests.take_first();
    m_recently_sent_requests.append({ .id = request.id, .sent_at = now });
    ++m_requests_sent;
}

void RecordingRefreshController::note_answered_by_display_list(u64 request_id, MonotonicTime now)
{
    for (auto const& sent_request : m_recently_sent_requests) {
        if (sent_request.id != request_id)
            continue;
        auto latency = now - sent_request.sent_at;
        m_planner.note_display_list_answered_request_after(latency);
        if (interest_refresh_logging_enabled())
            dbgln("INTEREST_REFRESH answered={} latency={} µs checkerboarded_frames={}", request_id, latency.to_microseconds(), m_checkerboarded_frame_count);
        break;
    }
    note_answered(request_id);
}

void RecordingRefreshController::note_answered_without_display_list(u64 request_id)
{
    note_answered(request_id);
}

void RecordingRefreshController::note_answered(u64 request_id)
{
    // WebContent cannot answer a request that was never sent.
    if (request_id == 0 || request_id >= m_next_request_id)
        return;
    m_newest_answered_request_id = max(m_newest_answered_request_id, request_id);
    // WebContent takes requests in the order they were sent, so none sent before this one is answered later.
    m_recently_sent_requests.remove_all_matching([&](auto const& sent_request) { return sent_request.id <= request_id; });
    if (m_request_in_flight.has_value() && request_id >= m_request_in_flight->id)
        m_request_in_flight.clear();
}

void RecordingRefreshController::forget_request_in_flight()
{
    m_request_in_flight.clear();
    m_recently_sent_requests.clear();
}

AK::Duration RecordingRefreshController::resend_delay(u32 resend_count, AK::Duration frame_interval) const
{
    auto latency = m_planner.refresh_latency(frame_interval);
    return AK::Duration::from_nanoseconds(latency.to_nanoseconds() * (resend_after_refresh_latencies << min(resend_count, most_resend_delay_doublings)));
}

Optional<MonotonicTime> RecordingRefreshController::resend_due_at(AK::Duration frame_interval) const
{
    if (!m_request_in_flight.has_value())
        return {};
    return m_request_in_flight->sent_at + resend_delay(m_request_in_flight->resend_count, frame_interval);
}

void RecordingRefreshController::reset_for_testing()
{
    m_planner.forget_how_scroll_nodes_moved_and_recordings_arrived();
    m_request_in_flight.clear();
    m_recently_sent_requests.clear();
    m_requests_sent = 0;
    m_checkerboarded_frame_count = 0;
    m_last_checkerboarded_area = 0;
}

JsonObject RecordingRefreshController::state_for_testing(AK::Duration frame_interval) const
{
    JsonObject state;
    state.set("requestsSent"sv, m_requests_sent);
    state.set("inFlightRequestId"sv, m_request_in_flight.has_value() ? JsonValue { m_request_in_flight->id } : JsonValue {});
    state.set("lastAnsweredRequestId"sv, m_newest_answered_request_id);
    state.set("latencyMilliseconds"sv, m_planner.refresh_latency(frame_interval).to_seconds_f64() * 1000);
    state.set("checkerboardedFrames"sv, m_checkerboarded_frame_count);
    state.set("lastCheckerboardedArea"sv, m_last_checkerboarded_area);
    return state;
}

}
