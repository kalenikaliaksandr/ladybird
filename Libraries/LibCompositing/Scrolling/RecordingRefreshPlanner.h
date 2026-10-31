/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Time.h>
#include <AK/Vector.h>
#include <LibCompositing/DisplayList/ContextRef.h>
#include <LibCompositing/Export.h>
#include <LibCompositing/Forward.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibWebCommon/Page/AsyncScrollNodeStableID.h>

namespace Compositing {

// A scroll node of the display list a compositor shows, as a frame it presents shows the node. Offsets and rects are in
// device pixels, rects of the node's content at scroll offset zero.
struct RecordingRefreshScrollNode {
    Web::AsyncScrollNodeStableID stable_node_id;
    SpatialNodeIndex scroll_node_index;
    Gfx::IntRect scrollport_rect;
    Gfx::FloatPoint scroll_offset;
    Gfx::FloatPoint min_scroll_offset;
    Gfx::FloatPoint max_scroll_offset;
    // Empty when none of the node shows.
    Optional<Gfx::IntRect> visible_content_rect;
    // Empty when the display list records all of the node's content.
    Optional<Gfx::IntRect> recorded_content_rect;
    // Where a smooth scroll the compositor runs takes the node by the time a recording asked for now could arrive.
    Optional<Gfx::FloatPoint> smooth_scroll_offset_after_refresh_latency;
    // What the request the compositor awaits an answer to asks for of the node, if it names the node.
    Optional<Gfx::IntRect> in_flight_rect;
};

struct RecordingRefreshPlan {
    // What the display list records of some scroll node no longer holds what shows of it until a new recording could
    // arrive.
    bool some_scroll_node_needs_refresh { false };
    // A scroll node that asked for the least it needs right after a jump waits to ask for its full rect.
    bool some_scroll_node_owes_request_after_jump { false };
    // What the request in flight asks for holds what shows of every node that needs a refresh.
    bool in_flight_request_holds { true };
    // How many device pixels of the content that shows lie outside what the display list records, over every node.
    u64 checkerboarded_area { 0 };
    // What to ask WebContent to record of each scroll node that shows.
    Vector<Web::ScrollNodeContentRect> desired_rects;
};

// What a compositor keeps of a context to decide when to ask WebContent to record the context's display list again,
// and around which rects: how each scroll node moves, and how long recordings take to arrive.
class COMPOSITING_API RecordingRefreshPlanner {
    AK_MAKE_NONCOPYABLE(RecordingRefreshPlanner);
    AK_MAKE_NONMOVABLE(RecordingRefreshPlanner);

public:
    RecordingRefreshPlanner();
    ~RecordingRefreshPlanner();

    // Looks at the scroll nodes of the display list whose visual context tree is given, as a frame presented now shows
    // them. Meant to be called once per presented frame, which is what the scroll nodes' motion is told from.
    RecordingRefreshPlan plan(AccumulatedVisualContextTree const&, MonotonicTime now, AK::Duration frame_interval, ReadonlySpan<RecordingRefreshScrollNode>);
    void note_request_for_last_plan_sent();
    void note_display_list_answered_request_after(AK::Duration);
    AK::Duration refresh_latency(AK::Duration frame_interval) const;
    void forget_how_scroll_nodes_moved_and_recordings_arrived();

private:
    void* m_rust_planner { nullptr };
};

}
