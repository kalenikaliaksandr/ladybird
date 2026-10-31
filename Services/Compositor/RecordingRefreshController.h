/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/JsonObject.h>
#include <AK/Noncopyable.h>
#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Time.h>
#include <AK/Vector.h>
#include <LibCompositing/Forward.h>
#include <LibCompositing/Scrolling/RecordingRefreshPlanner.h>
#include <LibGfx/Point.h>
#include <LibWebCommon/Page/AsyncScrollNodeStableID.h>

namespace Compositing {

class AsyncScrollTree;

}

namespace Compositor {

// Asks WebContent to record a context's display list again around new rects once the compositor scrolls the context's
// scroll nodes close to the edges of what the list records of them. It looks once per presented frame and keeps one
// request in flight: another replaces it only when what it asks for would no longer hold what shows, or when no answer
// came within a while that doubles with each resend. Nothing asks while the list records the whole document.
class RecordingRefreshController {
    AK_MAKE_NONCOPYABLE(RecordingRefreshController);
    AK_MAKE_NONMOVABLE(RecordingRefreshController);

public:
    RecordingRefreshController() = default;

    struct Request {
        u64 id { 0 };
        Vector<Web::ScrollNodeContentRect> scroll_node_rects;
    };

    struct SmoothScroll {
        Web::AsyncScrollNodeStableID stable_node_id;
        Gfx::FloatPoint offset_after_refresh_latency;
    };

    struct PresentedFrame {
        MonotonicTime now;
        AK::Duration frame_interval;
        Compositing::AsyncScrollTree const& scroll_tree;
        Compositing::AccumulatedVisualContextTree const& visual_context_tree;
        Compositing::ScrollStateSnapshot const& scroll_state_snapshot;
        ReadonlySpan<SmoothScroll> smooth_scrolls;
    };

    Optional<Request> request_for_presented_frame(PresentedFrame const&);
    // The request in flight under a new id, once no answer to it came in time. Only frames tell what to ask for.
    Optional<Request> request_to_resend(MonotonicTime now, AK::Duration frame_interval);
    // A display list WebContent sent names the request it answers: it records each rect the request asks for.
    void note_answered_by_display_list(u64 request_id, MonotonicTime now);
    // WebContent answered the request without a new display list, as the one the compositor has records its rects.
    void note_answered_without_display_list(u64 request_id);
    // A display list that records every scroll node whole holds whatever shows, which answers any request in flight.
    void note_display_list_records_every_scroll_node_whole() { m_request_in_flight.clear(); }
    // The request in flight went to a render clock channel that is gone, along with WebContent's memory of it.
    void forget_request_in_flight();
    Optional<MonotonicTime> resend_due_at(AK::Duration frame_interval) const;
    bool presented_frame_since_request_in_flight_was_sent() const { return m_request_in_flight.has_value() && m_request_in_flight->presented_frame_since_sent; }
    AK::Duration refresh_latency(AK::Duration frame_interval) const { return m_planner.refresh_latency(frame_interval); }

    // Request ids keep increasing, since WebContent drops a request older than one it took.
    void reset_for_testing();
    JsonObject state_for_testing(AK::Duration frame_interval) const;

private:
    struct RequestInFlight {
        u64 id { 0 };
        MonotonicTime sent_at;
        Vector<Web::ScrollNodeContentRect> scroll_node_rects;
        u32 resend_count { 0 };
        bool presented_frame_since_sent { false };
    };

    struct SentRequest {
        u64 id { 0 };
        MonotonicTime sent_at;
    };

    void note_answered(u64 request_id);
    void note_sent(Request const&, MonotonicTime now, u32 resend_count);
    AK::Duration resend_delay(u32 resend_count, AK::Duration frame_interval) const;

    Compositing::RecordingRefreshPlanner m_planner;
    Optional<RequestInFlight> m_request_in_flight;
    // The latest requests sent, oldest first, which a display list may still answer after a later request replaced them.
    Vector<SentRequest> m_recently_sent_requests;
    u64 m_next_request_id { 1 };
    u64 m_newest_answered_request_id { 0 };
    u64 m_requests_sent { 0 };
    u64 m_checkerboarded_frame_count { 0 };
    u64 m_last_checkerboarded_area { 0 };
};

}
