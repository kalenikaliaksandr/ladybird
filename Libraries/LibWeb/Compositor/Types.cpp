/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Compositor/Types.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollInput const& input)
{
    TRY(encoder.encode(input.precision));
    TRY(encoder.encode(input.phase));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollInput> decode(Decoder& decoder)
{
    Web::Compositor::AsyncScrollInput input;
    input.precision = TRY(decoder.decode<Web::WheelDeltaPrecision>());
    input.phase = TRY(decoder.decode<Web::ScrollGesturePhase>());
    if (input.precision > Web::WheelDeltaPrecision::Precise || input.phase > Web::ScrollGesturePhase::Ended)
        return Error::from_string_literal("Invalid asynchronous scroll input");
    return input;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::UserScrollUpdate const& update)
{
    TRY(encoder.encode(update.stable_node_id));
    TRY(encoder.encode(update.gesture_id));
    TRY(encoder.encode(update.status));
    TRY(encoder.encode(update.initial_scroll_offset.x().raw_value()));
    TRY(encoder.encode(update.initial_scroll_offset.y().raw_value()));
    TRY(encoder.encode(update.snap_destination));
    TRY(encoder.encode(update.did_scroll));
    return {};
}

template<>
ErrorOr<Web::Compositor::UserScrollUpdate> decode(Decoder& decoder)
{
    Web::Compositor::UserScrollUpdate update;
    update.stable_node_id = TRY(decoder.decode<Web::Compositor::AsyncScrollNodeStableID>());
    update.gesture_id = TRY(decoder.decode<u64>());
    update.status = TRY(decoder.decode<Web::Compositor::UserScrollStatus>());
    if (update.status > Web::Compositor::UserScrollStatus::ReplacedByProgrammaticScroll)
        return Error::from_string_literal("Invalid compositor user scroll status");
    update.initial_scroll_offset = { Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())), Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())) };
    update.snap_destination = TRY(decoder.decode<Optional<Web::Compositor::SnapDestination>>());
    update.did_scroll = TRY(decoder.decode<bool>());
    return update;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollNodeStableID const& stable_node_id)
{
    TRY(encoder.encode(stable_node_id.node_id));
    TRY(encoder.encode(stable_node_id.kind));
    TRY(encoder.encode(stable_node_id.pseudo_element_type));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollNodeStableID> decode(Decoder& decoder)
{
    return Web::Compositor::AsyncScrollNodeStableID {
        .node_id = TRY(decoder.decode<Web::UniqueNodeID>()),
        .kind = TRY(decoder.decode<Web::Compositor::AsyncScrollNodeKind>()),
        .pseudo_element_type = TRY(decoder.decode<u8>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollOffset const& offset)
{
    TRY(encoder.encode(offset.stable_node_id));
    TRY(encoder.encode(offset.compositor_scroll_offset));
    TRY(encoder.encode(offset.unadopted_scroll_delta));
    TRY(encoder.encode(offset.is_visual_viewport_pan));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollOffset> decode(Decoder& decoder)
{
    return Web::Compositor::AsyncScrollOffset {
        .stable_node_id = TRY(decoder.decode<Web::Compositor::AsyncScrollNodeStableID>()),
        .compositor_scroll_offset = TRY(decoder.decode<Gfx::FloatPoint>()),
        .unadopted_scroll_delta = TRY(decoder.decode<Gfx::FloatPoint>()),
        .is_visual_viewport_pan = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::PendingAsyncScrollUpdates const& updates)
{
    TRY(encoder.encode(updates.sequence));
    TRY(encoder.encode(updates.scroll_offsets));
    TRY(encoder.encode(updates.completed_operation_ids));
    TRY(encoder.encode(updates.operation_ids_taken_over_by_user_input));
    TRY(encoder.encode(updates.user_scroll_updates));
    TRY(encoder.encode(updates.user_scroll_gesture_in_progress));
    TRY(encoder.encode(updates.user_scroll_gesture_ended));
    return {};
}

template<>
ErrorOr<Web::Compositor::PendingAsyncScrollUpdates> decode(Decoder& decoder)
{
    return Web::Compositor::PendingAsyncScrollUpdates {
        .sequence = TRY(decoder.decode<u64>()),
        .scroll_offsets = TRY(decoder.decode<Vector<Web::Compositor::AsyncScrollOffset>>()),
        .completed_operation_ids = TRY(decoder.decode<Vector<Web::Compositor::AsyncScrollOperationID>>()),
        .operation_ids_taken_over_by_user_input = TRY(decoder.decode<Vector<Web::Compositor::AsyncScrollOperationID>>()),
        .user_scroll_updates = TRY(decoder.decode<Vector<Web::Compositor::UserScrollUpdate>>()),
        .user_scroll_gesture_in_progress = TRY(decoder.decode<bool>()),
        .user_scroll_gesture_ended = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollEnqueueResult const& result)
{
    TRY(encoder.encode(result.accepted));
    TRY(encoder.encode(result.operation_id));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollEnqueueResult> decode(Decoder& decoder)
{
    return Web::Compositor::AsyncScrollEnqueueResult {
        .accepted = TRY(decoder.decode<bool>()),
        .operation_id = TRY(decoder.decode<Optional<Web::Compositor::AsyncScrollOperationID>>()),
    };
}

}

namespace Web::Compositor {

void merge_async_scroll_updates(PendingAsyncScrollUpdates& pending, PendingAsyncScrollUpdates&& updates)
{
    // Whether a gesture is in progress is a state, not an event: the newest publication decides it.
    bool const is_newest = updates.sequence >= pending.sequence;
    pending.sequence = max(pending.sequence, updates.sequence);
    for (auto const& scroll_offset : updates.scroll_offsets) {
        auto existing = pending.scroll_offsets.find_if([&](auto const& existing) { return existing.stable_node_id == scroll_offset.stable_node_id && existing.is_visual_viewport_pan == scroll_offset.is_visual_viewport_pan; });
        if (existing != pending.scroll_offsets.end()) {
            existing->compositor_scroll_offset = scroll_offset.compositor_scroll_offset;
            existing->unadopted_scroll_delta.translate_by(scroll_offset.unadopted_scroll_delta);
        } else {
            pending.scroll_offsets.append(scroll_offset);
        }
    }
    pending.completed_operation_ids.extend(move(updates.completed_operation_ids));
    pending.operation_ids_taken_over_by_user_input.extend(move(updates.operation_ids_taken_over_by_user_input));
    pending.user_scroll_updates.extend(move(updates.user_scroll_updates));
    if (is_newest)
        pending.user_scroll_gesture_in_progress = updates.user_scroll_gesture_in_progress;
    pending.user_scroll_gesture_ended |= updates.user_scroll_gesture_ended;
}

}
