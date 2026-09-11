/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/AnyOf.h>
#include <LibCore/Timer.h>
#include <LibWeb/Compositor/ScrollSnap.h>
#include <Services/Compositor/ContextState.h>

namespace Compositor {

static Web::CSSPixelPoint css_offset(Gfx::FloatPoint offset, double scale)
{
    return { Web::CSSPixels::nearest_value_for(offset.x() / scale), Web::CSSPixels::nearest_value_for(offset.y() / scale) };
}

bool ContextState::user_scroll_animation_is_active(UserScrollGesture const& gesture) const
{
    return gesture.animation_id.has_value() && any_of(m_smooth_scroll_animations, [&](auto const& animation) { return animation.operation_id == *gesture.animation_id; });
}

void ContextState::update_user_scroll_settle_timer()
{
    Optional<MonotonicTime> next_deadline;
    auto now = MonotonicTime::now();
    for (auto const& gesture : m_user_scroll_gestures) {
        auto deadline = gesture.settle_deadline;
        if (gesture.input_ended) {
            if (user_scroll_animation_is_active(gesture))
                continue;
            deadline = now;
        }
        if (deadline.has_value() && (!next_deadline.has_value() || *deadline < *next_deadline))
            next_deadline = deadline;
    }
    if (!next_deadline.has_value()) {
        if (m_user_scroll_settle_timer)
            m_user_scroll_settle_timer->stop();
        return;
    }
    if (!m_user_scroll_settle_timer) {
        m_user_scroll_settle_timer = Core::Timer::create_single_shot(500, [this] {
            if (m_schedule_user_scroll_settlement) {
                m_schedule_user_scroll_settlement();
            } else if (auto frame = process_user_scroll_deadlines(MonotonicTime::now()); frame.has_value()) {
                queue_present_frame(*frame);
            }
        });
    }
    m_user_scroll_settle_timer->restart(max(1, static_cast<int>((*next_deadline - now).to_milliseconds())));
}

void ContextState::note_user_scroll_input(Web::Compositor::AsyncScrollInput input, MonotonicTime now)
{
    for (auto& gesture : m_user_scroll_gestures) {
        if (gesture.scrollbar)
            continue;
        gesture.input_ended = input.phase == Web::ScrollGesturePhase::Ended;
        gesture.settle_deadline = input.phase == Web::ScrollGesturePhase::None
            ? Optional<MonotonicTime> { now + AK::Duration::from_milliseconds(500) }
            : OptionalNone {};
    }
    update_user_scroll_settle_timer();
}

void ContextState::track_user_scroll_offsets(Vector<Web::Compositor::AsyncScrollOffset> const& offsets, Web::Compositor::AsyncScrollInput input, bool scrollbar, MonotonicTime now)
{
    // Discrete and momentum input continue to use WebContent until their per-scroll selection is enabled.
    if (!scrollbar && (input.precision == Web::WheelDeltaPrecision::Discrete || input.phase == Web::ScrollGesturePhase::Momentum))
        return;
    auto scale = m_async_scroll_tree.snap_device_pixels_per_css_pixel();
    for (auto const& offset : offsets) {
        auto node_id = m_async_scroll_tree.scroll_node_id_for_stable_id(offset.stable_node_id);
        if (!node_id.has_value() || !m_async_scroll_tree.snap_data_for_node(*node_id))
            continue;
        auto current = css_offset(offset.compositor_scroll_offset, scale);
        auto start = css_offset(offset.compositor_scroll_offset - offset.unadopted_scroll_delta, scale);
        auto entry = m_user_scroll_gestures.find_if([&](auto const& value) { return value.update.stable_node_id == offset.stable_node_id; });
        auto* gesture = entry == m_user_scroll_gestures.end() ? nullptr : &*entry;
        if (!gesture) {
            UserScrollGesture state;
            state.update.stable_node_id = offset.stable_node_id;
            state.update.gesture_id = ++m_next_user_scroll_gesture_id;
            state.update.initial_scroll_offset = start;
            state.unsnapped_destination = start;
            state.scrollbar = scrollbar;
            m_user_scroll_gestures.append(move(state));
            gesture = &m_user_scroll_gestures.last();
        } else if (gesture->input_ended) {
            gesture->update.gesture_id = ++m_next_user_scroll_gesture_id;
            gesture->update.initial_scroll_offset = start;
            gesture->update.snap_destination.clear();
        }
        gesture->input = input;
        gesture->scrollbar = scrollbar;
        gesture->input_ended = scrollbar && !m_viewport_scrollbar_controller.has_captured_scrollbar();
        gesture->selected_at_end = false;
        gesture->animation_id.clear();
        gesture->unsnapped_destination = current;
        gesture->update.did_scroll |= !offset.unadopted_scroll_delta.is_zero();
        gesture->settle_deadline = !scrollbar && input.phase == Web::ScrollGesturePhase::None
            ? Optional<MonotonicTime> { now + AK::Duration::from_milliseconds(500) }
            : OptionalNone {};
        m_pending_user_scroll_updates.append(gesture->update);
    }
    update_user_scroll_settle_timer();
}

Optional<ContextState::PendingFrame> ContextState::animate_user_scroll_to(UserScrollGesture& gesture, Web::Compositor::SnapDestination destination, Web::Compositor::ScrollAnimationKind kind, MonotonicTime now)
{
    auto scale = m_async_scroll_tree.snap_device_pixels_per_css_pixel();
    auto node_id = m_async_scroll_tree.scroll_node_id_for_stable_id(gesture.update.stable_node_id);
    if (!node_id.has_value())
        return {};
    auto current = m_async_scroll_tree.scroll_offset_for_node(*node_id, m_scroll_state_snapshot);
    if (!current.has_value())
        return {};
    gesture.update.snap_destination = move(destination);
    m_pending_user_scroll_updates.append(gesture.update);
    auto target = gesture.update.snap_destination->position.to_type<float>().scaled(static_cast<float>(scale));
    if (css_offset(*current, scale) == gesture.update.snap_destination->position)
        return {};
    auto result = smooth_scroll_to(gesture.update.stable_node_id, target, *current, m_async_scrolling_viewport_rect, scale, kind);
    if (result.enqueue_result.accepted) {
        gesture.animation_id = result.enqueue_result.operation_id;
        for (auto& animation : m_smooth_scroll_animations) {
            if (gesture.animation_id.has_value() && animation.operation_id == *gesture.animation_id)
                animation.started_at = now;
        }
    }
    return result.frame_to_present;
}

Optional<ContextState::PendingFrame> ContextState::process_user_scroll_deadlines(MonotonicTime now)
{
    Optional<PendingFrame> frame;
    for (size_t index = 0; index < m_user_scroll_gestures.size();) {
        auto& gesture = m_user_scroll_gestures[index];
        auto node_id = m_async_scroll_tree.scroll_node_id_for_stable_id(gesture.update.stable_node_id);
        if (!node_id.has_value()) {
            auto id = gesture.update.stable_node_id;
            take_over_user_scroll(id, Web::Compositor::UserScrollTakeoverReason::Programmatic);
            continue;
        }
        if (gesture.settle_deadline.has_value() && now >= *gesture.settle_deadline)
            gesture.input_ended = true;
        if (!gesture.input_ended || user_scroll_animation_is_active(gesture)) {
            ++index;
            continue;
        }
        gesture.animation_id.clear();
        if (!gesture.selected_at_end) {
            gesture.selected_at_end = true;
            auto const* data = m_async_scroll_tree.snap_data_for_node(*node_id);
            auto offset = m_async_scroll_tree.scroll_offset_for_node(*node_id, m_scroll_state_snapshot);
            if (data && offset.has_value()) {
                auto current = css_offset(*offset, m_async_scroll_tree.snap_device_pixels_per_css_pixel());
                Web::Compositor::SnapSelectionStrategy strategy;
                if (!gesture.scrollbar && !gesture.selected_per_scroll)
                    strategy.displacement = current - gesture.update.initial_scroll_offset;
                auto destination = Web::Compositor::select_snap_destination(*data, current, strategy);
                if (auto animation_frame = animate_user_scroll_to(gesture, move(destination), Web::Compositor::ScrollAnimationKind::SmoothScroll, now); animation_frame.has_value()) {
                    frame = animation_frame;
                    ++index;
                    continue;
                }
            }
        }
        auto update = gesture.update;
        update.status = Web::Compositor::UserScrollStatus::Settled;
        m_pending_user_scroll_updates.append(move(update));
        m_user_scroll_gestures.remove(index);
    }
    update_user_scroll_settle_timer();
    if (!m_pending_user_scroll_updates.is_empty())
        request_rendering_update();
    return frame;
}

}
