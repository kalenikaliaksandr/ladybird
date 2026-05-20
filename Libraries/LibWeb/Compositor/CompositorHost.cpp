/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/Timer.h>
#include <LibWeb/Compositor/CompositorHost.h>
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Compositor {

CompositorContextHandle::CompositorContextHandle(CompositorHost& host, CompositorContextId context_id)
    : m_host(host)
    , m_context_id(context_id)
{
    m_backing_store_shrink_timer = Core::Timer::create_single_shot(3000, [this] {
        enqueue_viewport_size_updated(m_last_viewport_size, m_last_viewport_size_is_top_level_traversable, WindowResizingInProgress::No);
    });
}

CompositorContextHandle::~CompositorContextHandle()
{
    destroy();
}

void CompositorContextHandle::destroy()
{
    if (m_destroyed)
        return;
    m_destroyed = true;
    cancel_backing_store_shrink_timer();
    m_host.destroy_context(m_context_id);
}

void CompositorContextHandle::stop_presenting_to_client()
{
    if (!m_destroyed)
        m_host.stop_presenting_to_client(m_context_id);
}

void CompositorContextHandle::set_presentation_mode(PresentationMode mode)
{
    if (!m_destroyed)
        m_host.set_presentation_mode(m_context_id, move(mode));
}

void CompositorContextHandle::update_display_list(NonnullRefPtr<Painting::DisplayList> display_list, Painting::DisplayListResourceTransaction&& resource_transaction, Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    if (m_destroyed)
        return;
    m_host.update_display_list(m_context_id, move(display_list), move(resource_transaction), move(scroll_state_snapshot));
}

void CompositorContextHandle::update_video_frame(Painting::VideoFrameResourceId frame_id, NonnullRefPtr<Media::VideoFrame const> frame)
{
    if (!m_destroyed)
        m_host.update_video_frame(m_context_id, frame_id, move(frame));
}

void CompositorContextHandle::clear_video_frame(Painting::VideoFrameResourceId frame_id)
{
    if (!m_destroyed)
        m_host.clear_video_frame(m_context_id, frame_id);
}

void CompositorContextHandle::update_compositor_surface(Painting::CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image)
{
    if (!m_destroyed)
        m_host.update_compositor_surface(m_context_id, surface_id, move(shared_image));
}

void CompositorContextHandle::clear_compositor_surface(Painting::CompositorSurfaceId surface_id)
{
    if (!m_destroyed)
        m_host.clear_compositor_surface(m_context_id, surface_id);
}

void CompositorContextHandle::update_scroll_state(Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    if (!m_destroyed)
        m_host.update_scroll_state(m_context_id, move(scroll_state_snapshot));
}

void CompositorContextHandle::invalidate_wheel_event_listener_state(u64 generation)
{
    if (!m_destroyed)
        m_host.invalidate_wheel_event_listener_state(m_context_id, generation);
}

AsyncScrollEnqueueResult CompositorContextHandle::async_scroll_by(UniqueNodeID expected_document_id, Gfx::FloatPoint position,
    Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking operation_tracking)
{
    if (m_destroyed)
        return {};
    return m_host.async_scroll_by(m_context_id, expected_document_id, position, delta_in_device_pixels, viewport_rect, operation_tracking);
}

bool CompositorContextHandle::should_defer_async_scroll_offset_adoption() const
{
    if (m_destroyed)
        return false;
    return m_host.should_defer_async_scroll_offset_adoption(m_context_id);
}

bool CompositorContextHandle::should_defer_main_thread_present_for_async_scroll() const
{
    if (m_destroyed)
        return false;
    return m_host.should_defer_main_thread_present_for_async_scroll(m_context_id);
}

PendingAsyncScrollUpdates CompositorContextHandle::take_pending_async_scroll_updates()
{
    if (m_destroyed)
        return {};
    return m_host.take_pending_async_scroll_updates(m_context_id);
}

void CompositorContextHandle::viewport_size_updated(Gfx::IntSize viewport_size, bool is_top_level_traversable, WindowResizingInProgress window_resize_in_progress)
{
    if (m_destroyed)
        return;
    m_last_viewport_size = viewport_size;
    m_last_viewport_size_is_top_level_traversable = is_top_level_traversable;
    if (window_resize_in_progress == WindowResizingInProgress::Yes && m_backing_store_shrink_timer)
        m_backing_store_shrink_timer->restart();
    enqueue_viewport_size_updated(viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void CompositorContextHandle::present_frame(Gfx::IntRect viewport_rect)
{
    if (!m_destroyed)
        m_host.present_frame(m_context_id, viewport_rect);
}

void CompositorContextHandle::request_screenshot(Gfx::SharedImage&& target, Function<void()>&& callback)
{
    if (m_destroyed) {
        callback();
        return;
    }
    m_host.request_screenshot(m_context_id, move(target), move(callback));
}

void CompositorContextHandle::enqueue_viewport_size_updated(Gfx::IntSize viewport_size, bool is_top_level_traversable, WindowResizingInProgress window_resize_in_progress)
{
    if (!m_destroyed)
        m_host.viewport_size_updated(m_context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void CompositorContextHandle::cancel_backing_store_shrink_timer()
{
    if (!m_backing_store_shrink_timer)
        return;
    m_backing_store_shrink_timer->on_timeout = {};
    m_backing_store_shrink_timer->stop();
    m_backing_store_shrink_timer.clear();
}

CompositorHost::~CompositorHost() = default;

void CompositorHost::start(DisplayListPlayerType display_list_player_type)
{
    start_impl(display_list_player_type);
}

OwnPtr<CompositorContextHandle> CompositorHost::create_context(Optional<u64> page_id, PagePresentationRegistration page_presentation_registration)
{
    auto context_id = create_context_impl(page_id, page_presentation_registration);
    return adopt_own(*new CompositorContextHandle(*this, context_id));
}

void CompositorHost::destroy_context(CompositorContextId context_id)
{
    destroy_context_impl(context_id);
}

void CompositorHost::stop_presenting_to_client(CompositorContextId context_id)
{
    stop_presenting_to_client_impl(context_id);
}

void CompositorHost::set_presentation_mode(CompositorContextId context_id, PresentationMode mode)
{
    set_presentation_mode_impl(context_id, move(mode));
}

void CompositorHost::update_display_list(CompositorContextId context_id, NonnullRefPtr<Painting::DisplayList> display_list, Painting::DisplayListResourceTransaction&& resource_transaction, Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    update_display_list_impl(context_id, move(display_list), move(resource_transaction), move(scroll_state_snapshot));
}

void CompositorHost::update_video_frame(CompositorContextId context_id, Painting::VideoFrameResourceId frame_id, NonnullRefPtr<Media::VideoFrame const> frame)
{
    update_video_frame_impl(context_id, frame_id, move(frame));
}

void CompositorHost::clear_video_frame(CompositorContextId context_id, Painting::VideoFrameResourceId frame_id)
{
    clear_video_frame_impl(context_id, frame_id);
}

void CompositorHost::update_compositor_surface(CompositorContextId context_id, Painting::CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image)
{
    update_compositor_surface_impl(context_id, surface_id, move(shared_image));
}

void CompositorHost::clear_compositor_surface(CompositorContextId context_id, Painting::CompositorSurfaceId surface_id)
{
    clear_compositor_surface_impl(context_id, surface_id);
}

void CompositorHost::update_scroll_state(CompositorContextId context_id, Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    update_scroll_state_impl(context_id, move(scroll_state_snapshot));
}

void CompositorHost::invalidate_wheel_event_listener_state(CompositorContextId context_id, u64 generation)
{
    invalidate_wheel_event_listener_state_impl(context_id, generation);
}

AsyncScrollEnqueueResult CompositorHost::async_scroll_by(CompositorContextId context_id, UniqueNodeID expected_document_id, Gfx::FloatPoint position,
    Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking operation_tracking)
{
    return async_scroll_by_impl(context_id, expected_document_id, position, delta_in_device_pixels, viewport_rect, operation_tracking);
}

bool CompositorHost::should_defer_async_scroll_offset_adoption(CompositorContextId context_id) const
{
    return should_defer_async_scroll_offset_adoption_impl(context_id);
}

bool CompositorHost::should_defer_main_thread_present_for_async_scroll(CompositorContextId context_id) const
{
    return should_defer_main_thread_present_for_async_scroll_impl(context_id);
}

PendingAsyncScrollUpdates CompositorHost::take_pending_async_scroll_updates(CompositorContextId context_id)
{
    return take_pending_async_scroll_updates_impl(context_id);
}

void CompositorHost::viewport_size_updated(CompositorContextId context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, WindowResizingInProgress window_resize_in_progress)
{
    viewport_size_updated_impl(context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void CompositorHost::present_frame(CompositorContextId context_id, Gfx::IntRect viewport_rect)
{
    present_frame_impl(context_id, viewport_rect);
}

void CompositorHost::request_screenshot(CompositorContextId context_id, Gfx::SharedImage&& target, Function<void()>&& callback)
{
    request_screenshot_impl(context_id, move(target), move(callback));
}

}
