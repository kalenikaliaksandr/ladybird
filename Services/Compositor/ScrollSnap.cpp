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

ContextState::UserScrollGesture& ContextState::user_scroll_gesture_for(Web::Compositor::AsyncScrollNodeStableID id, Web::CSSPixelPoint start, Web::Compositor::AsyncScrollInput input, bool scrollbar, MonotonicTime now)
{
    auto entry = m_user_scroll_gestures.find_if([&](auto const& value) { return value.update.stable_node_id == id; });
    auto* gesture = entry == m_user_scroll_gestures.end() ? nullptr : &*entry;
    bool new_gesture = !gesture || gesture->input_ended || gesture->selected_at_end
        || gesture->scrollbar != scrollbar || gesture->input.precision != input.precision
        || (gesture->input.phase == Web::ScrollGesturePhase::Momentum && input.phase != Web::ScrollGesturePhase::Momentum);
    if (new_gesture) {
        UserScrollGesture state;
        state.update.stable_node_id = id;
        state.update.gesture_id = ++m_next_user_scroll_gesture_id;
        state.update.initial_scroll_offset = start;
        state.unsnapped_destination = start;
        if (gesture) {
            // A new gesture takes over the pending scrollend as well as the animation's visual position.
            state.update.did_scroll = gesture->update.did_scroll;
            state.update.snap_destination = gesture->update.snap_destination;
            state.animation_id = gesture->animation_id;
            *gesture = move(state);
        } else {
            m_user_scroll_gestures.append(move(state));
            gesture = &m_user_scroll_gestures.last();
        }
    }
    gesture->input = input;
    gesture->scrollbar = scrollbar;
    gesture->input_ended = scrollbar && !m_viewport_scrollbar_controller.has_captured_scrollbar();
    gesture->settle_deadline = !scrollbar && input.phase == Web::ScrollGesturePhase::None
        ? Optional<MonotonicTime> { now + AK::Duration::from_milliseconds(500) }
        : OptionalNone {};
    return *gesture;
}

void ContextState::track_user_scroll_offsets(Vector<Web::Compositor::AsyncScrollOffset> const& offsets, Web::Compositor::AsyncScrollInput input, bool scrollbar, MonotonicTime now)
{
    auto scale = m_async_scroll_tree.snap_device_pixels_per_css_pixel();
    for (auto const& offset : offsets) {
        auto node_id = m_async_scroll_tree.scroll_node_id_for_stable_id(offset.stable_node_id);
        if (!node_id.has_value() || !m_async_scroll_tree.snap_data_for_node(*node_id))
            continue;
        auto current = css_offset(offset.compositor_scroll_offset, scale);
        auto start = css_offset(offset.compositor_scroll_offset - offset.unadopted_scroll_delta, scale);
        auto& gesture = user_scroll_gesture_for(offset.stable_node_id, start, input, scrollbar, now);
        gesture.selected_at_end = false;
        gesture.animation_id.clear();
        gesture.unsnapped_destination = current;
        gesture.update.did_scroll |= !offset.unadopted_scroll_delta.is_zero();
        m_pending_user_scroll_updates.append(gesture.update);
    }
    update_user_scroll_settle_timer();
}

bool ContextState::consume_selected_momentum_scroll(Web::Compositor::AsyncScrollInput input, MonotonicTime now, Optional<Web::UniqueNodeID> document_id)
{
    if (input.phase != Web::ScrollGesturePhase::Momentum)
        return false;
    for (auto const& gesture : m_user_scroll_gestures) {
        if (!gesture.momentum_selected || gesture.input_ended)
            continue;
        auto node = m_async_scroll_tree.scroll_node_id_for_stable_id(gesture.update.stable_node_id);
        if (!node.has_value() || (document_id.has_value() && node->document_id != *document_id))
            continue;
        note_user_scroll_input(input, now);
        return true;
    }
    return false;
}

Optional<ContextState::ContextUpdateResult> ContextState::scroll_by_with_snapping(Web::Compositor::AsyncScrollNodeID node_id, Gfx::FloatPoint delta, Web::Compositor::AsyncScrollInput input, MonotonicTime now)
{
    bool discrete = input.precision == Web::WheelDeltaPrecision::Discrete;
    bool momentum = input.phase == Web::ScrollGesturePhase::Momentum;
    if (!discrete && !momentum)
        return {};
    auto const* data = m_async_scroll_tree.snap_data_for_node(node_id);
    auto stable_id = m_async_scroll_tree.stable_node_id_for_node(node_id);
    auto offset = m_async_scroll_tree.scroll_offset_for_node(node_id, m_scroll_state_snapshot);
    if (!data || !stable_id.has_value() || !offset.has_value())
        return {};
    auto scale = m_async_scroll_tree.snap_device_pixels_per_css_pixel();
    auto current = css_offset(*offset, scale);
    auto displacement = css_offset(delta, scale);
    auto& gesture = user_scroll_gesture_for(*stable_id, current, input, false, now);
    if (momentum) {
        if (gesture.momentum_has_no_target)
            return {};
        auto estimate = gesture.momentum_estimator.estimate_remaining_displacement(displacement);
        if (!estimate.has_value())
            return {};
        displacement = *estimate;
    }
    auto start = discrete ? gesture.unsnapped_destination : current;
    Web::CSSPixelPoint unsnapped_destination {
        clamp(start.x() + displacement.x(), data->min_scroll_offset.x(), data->max_scroll_offset.x()),
        clamp(start.y() + displacement.y(), data->min_scroll_offset.y(), data->max_scroll_offset.y()),
    };
    Web::Compositor::SnapSelectionStrategy strategy {
        discrete ? Web::Compositor::SnapSelectionStrategy::Type::Direction : Web::Compositor::SnapSelectionStrategy::Type::EndPositionAndDirection,
        start,
        displacement,
    };
    if (discrete)
        strategy.starting_positions_boundary = unsnapped_destination;
    auto destination = Web::Compositor::select_snap_destination(*data, unsnapped_destination, strategy);
    if (!(destination.snapped_x && displacement.x() != 0) && !(destination.snapped_y && displacement.y() != 0)) {
        if (momentum)
            gesture.momentum_has_no_target = true;
        return {};
    }
    gesture.unsnapped_destination = unsnapped_destination;
    gesture.selected_per_scroll = true;
    gesture.momentum_selected = momentum;
    auto resting_position = user_scroll_animation_is_active(gesture) && gesture.update.snap_destination.has_value()
        ? gesture.update.snap_destination->position
        : current;
    Optional<PendingFrame> frame;
    if (destination.position == resting_position) {
        gesture.update.snap_destination = move(destination);
        m_pending_user_scroll_updates.append(gesture.update);
    } else {
        cancel_smooth_scroll_taken_over_by_user_input(node_id);
        frame = animate_user_scroll_to(gesture, move(destination), momentum ? Web::Compositor::ScrollAnimationKind::Momentum : Web::Compositor::ScrollAnimationKind::SmoothScroll, now);
    }
    note_user_scroll_input(input, now);
    return ContextUpdateResult { .accepted = true, .frame_to_present = frame, .should_request_rendering_update = true };
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
        if (m_async_scroll_tree.is_missing_snap_data(*node_id)) {
            auto id = gesture.update.stable_node_id;
            take_over_user_scroll(id, Web::Compositor::UserScrollTakeoverReason::UserInput);
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
                if (!gesture.scrollbar && !gesture.selected_per_scroll) {
                    strategy.displacement = current - gesture.update.initial_scroll_offset;
                    if (gesture.input.precision == Web::WheelDeltaPrecision::Discrete)
                        strategy.type = Web::Compositor::SnapSelectionStrategy::Type::Direction;
                    if (gesture.input.precision == Web::WheelDeltaPrecision::Discrete || gesture.input.phase == Web::ScrollGesturePhase::Momentum)
                        strategy.start_offset = gesture.update.initial_scroll_offset;
                }
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
