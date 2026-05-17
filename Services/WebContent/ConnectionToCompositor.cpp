/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <WebContent/ConnectionToCompositor.h>

namespace WebContent {

ConnectionToCompositor::ConnectionToCompositor(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionToServer<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint>(*this, move(transport))
{
}

void ConnectionToCompositor::die()
{
    for (auto& entry : m_screenshot_callbacks)
        entry.value({});
    m_screenshot_callbacks.clear();
}

void ConnectionToCompositor::create_context(
    Web::Compositor::CompositorContextId context_id,
    u64 page_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::PresentationCapability presentation_capability,
    Web::Compositor::CompositorContextId target_context_id,
    Web::Painting::CompositorSurfaceId compositor_surface_id,
    Web::DisplayListPlayerType display_list_player_type)
{
    async_create_context(
        context_id.value(),
        page_id,
        presentation_mode_kind,
        presentation_id.value(),
        presentation_capability.value(),
        target_context_id.value(),
        compositor_surface_id.value(),
        display_list_player_type);
}

void ConnectionToCompositor::destroy_context(Web::Compositor::CompositorContextId context_id)
{
    async_destroy_context(context_id.value());
}

void ConnectionToCompositor::set_presentation_mode(
    Web::Compositor::CompositorContextId context_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::PresentationCapability presentation_capability,
    Web::Compositor::CompositorContextId target_context_id,
    Web::Painting::CompositorSurfaceId compositor_surface_id)
{
    async_set_presentation_mode(
        context_id.value(),
        presentation_mode_kind,
        presentation_id.value(),
        presentation_capability.value(),
        target_context_id.value(),
        compositor_surface_id.value());
}

void ConnectionToCompositor::stop_presenting_to_client(Web::Compositor::CompositorContextId context_id)
{
    async_stop_presenting_to_client(context_id.value());
}

void ConnectionToCompositor::viewport_size_updated(
    Web::Compositor::CompositorContextId context_id,
    Gfx::IntSize viewport_size,
    bool is_top_level_traversable,
    Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    async_viewport_size_updated(context_id.value(), viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void ConnectionToCompositor::update_display_list(
    Web::Compositor::CompositorContextId context_id,
    NonnullRefPtr<Web::Painting::DisplayList> const& display_list,
    Web::Painting::DisplayListResourceTransaction const& resource_transaction,
    Web::Painting::ScrollStateSnapshot const& scroll_state)
{
    async_update_display_list(context_id.value(), display_list, resource_transaction, scroll_state);
}

void ConnectionToCompositor::update_scroll_state(
    Web::Compositor::CompositorContextId context_id,
    Web::Painting::ScrollStateSnapshot const& scroll_state)
{
    async_update_scroll_state(context_id.value(), scroll_state);
}

void ConnectionToCompositor::update_compositor_surface(
    Web::Compositor::CompositorContextId context_id,
    Web::Painting::CompositorSurfaceId surface_id,
    Gfx::SharedImage&& shared_image)
{
    async_update_compositor_surface(context_id.value(), surface_id.value(), move(shared_image));
}

void ConnectionToCompositor::clear_compositor_surface(
    Web::Compositor::CompositorContextId context_id,
    Web::Painting::CompositorSurfaceId surface_id)
{
    async_clear_compositor_surface(context_id.value(), surface_id.value());
}

void ConnectionToCompositor::update_yuv_video_frame(
    Web::Compositor::CompositorContextId context_id,
    Web::Compositor::SerializedVideoFrameUpdate const& video_frame_update)
{
    async_update_yuv_video_frame(context_id.value(), video_frame_update);
}

void ConnectionToCompositor::clear_video_frame(
    Web::Compositor::CompositorContextId context_id,
    Web::Painting::VideoFrameResourceId video_frame_source_id)
{
    async_clear_video_frame(context_id.value(), video_frame_source_id.value());
}

void ConnectionToCompositor::request_screenshot(
    Web::Compositor::CompositorContextId context_id,
    Gfx::IntSize size,
    Function<void(Optional<Gfx::SharedImage>)>&& callback)
{
    auto request_id = m_next_screenshot_request_id++;
    m_screenshot_callbacks.set(request_id, move(callback));
    async_request_screenshot(context_id.value(), request_id, size);
}

u64 ConnectionToCompositor::present_frame(Web::Compositor::CompositorContextId context_id, Gfx::IntRect viewport_rect)
{
    auto response = send_sync_but_allow_failure<Messages::WebContentCompositorServer::PresentFrame>(context_id.value(), viewport_rect);
    if (!response)
        return 0;
    return response->frame_id();
}

void ConnectionToCompositor::wait_for_frame(Web::Compositor::CompositorContextId context_id, u64 frame_id)
{
    (void)send_sync_but_allow_failure<Messages::WebContentCompositorServer::WaitForFrame>(context_id.value(), frame_id);
}

void ConnectionToCompositor::schedule_rendering_update(u64)
{
}

void ConnectionToCompositor::did_finish_screenshot(u64 request_id, Gfx::SharedImage shared_image)
{
    auto callback = m_screenshot_callbacks.take(request_id);
    if (!callback.has_value())
        return;
    callback.release_value()(move(shared_image));
}

void ConnectionToCompositor::did_fail_screenshot(u64 request_id)
{
    auto callback = m_screenshot_callbacks.take(request_id);
    if (!callback.has_value())
        return;
    callback.release_value()({});
}

}
