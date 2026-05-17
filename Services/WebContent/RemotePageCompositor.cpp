/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <WebContent/RemotePageCompositor.h>

namespace WebContent {

RemotePageCompositor::RemotePageCompositor(ConnectionToCompositor& connection, u64 page_id, PresentationMode presentation_mode)
    : m_connection(connection)
    , m_page_id(page_id)
    , m_context_id(Web::Compositor::allocate_compositor_context_id())
    , m_presentation_mode(move(presentation_mode))
{
}

RemotePageCompositor::~RemotePageCompositor()
{
    if (m_started && m_connection->is_open())
        m_connection->destroy_context(m_context_id);
}

RemotePageCompositor::SerializedPresentationMode RemotePageCompositor::serialized_presentation_mode() const
{
    return m_presentation_mode.visit(
        [](PresentToUI const& present_to_ui) -> SerializedPresentationMode {
            return {
                .kind = Web::Compositor::SerializedPresentationModeKind::PresentToUI,
                .presentation_id = present_to_ui.presentation_id,
                .presentation_capability = present_to_ui.presentation_capability,
                .target_context_id = {},
                .compositor_surface_id = {},
            };
        },
        [](PublishToCompositorSurface const& publish_to_compositor_surface) -> SerializedPresentationMode {
            return {
                .kind = Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface,
                .presentation_id = {},
                .presentation_capability = {},
                .target_context_id = publish_to_compositor_surface.target_context_id,
                .compositor_surface_id = publish_to_compositor_surface.surface_id,
            };
        });
}

void RemotePageCompositor::send_create_context()
{
    auto presentation_mode = serialized_presentation_mode();
    m_connection->create_context(
        m_context_id,
        m_page_id,
        presentation_mode.kind,
        presentation_mode.presentation_id,
        presentation_mode.presentation_capability,
        presentation_mode.target_context_id,
        presentation_mode.compositor_surface_id,
        m_display_list_player_type);
}

void RemotePageCompositor::start(Web::DisplayListPlayerType display_list_player_type)
{
    if (m_started)
        return;
    m_display_list_player_type = display_list_player_type;
    m_started = true;
    send_create_context();
}

void RemotePageCompositor::stop_presenting_to_client()
{
    if (m_started && m_connection->is_open())
        m_connection->stop_presenting_to_client(m_context_id);
}

void RemotePageCompositor::set_presentation_mode(PresentationMode presentation_mode)
{
    m_presentation_mode = move(presentation_mode);
    if (!m_started || !m_connection->is_open())
        return;

    auto serialized_mode = serialized_presentation_mode();
    m_connection->set_presentation_mode(
        m_context_id,
        serialized_mode.kind,
        serialized_mode.presentation_id,
        serialized_mode.presentation_capability,
        serialized_mode.target_context_id,
        serialized_mode.compositor_surface_id);
}

void RemotePageCompositor::update_display_list(NonnullRefPtr<Web::Painting::DisplayList>, Web::Painting::DisplayListResourceTransaction&&, Web::Painting::ScrollStateSnapshot&&)
{
}

void RemotePageCompositor::update_compositor_surface(Web::Painting::CompositorSurfaceId, Gfx::SharedImage&&)
{
}

void RemotePageCompositor::clear_compositor_surface(Web::Painting::CompositorSurfaceId)
{
}

void RemotePageCompositor::update_scroll_state(Web::Painting::ScrollStateSnapshot&&)
{
}

void RemotePageCompositor::invalidate_wheel_event_listener_state(u64)
{
}

RemotePageCompositor::AsyncScrollEnqueueResult RemotePageCompositor::async_scroll_by(Web::UniqueNodeID, Gfx::FloatPoint, Gfx::FloatPoint, Gfx::IntRect, AsyncScrollOperationTracking)
{
    return {};
}

bool RemotePageCompositor::should_defer_async_scroll_offset_adoption() const
{
    return false;
}

bool RemotePageCompositor::should_defer_main_thread_present_for_async_scroll() const
{
    return false;
}

RemotePageCompositor::PendingAsyncScrollUpdates RemotePageCompositor::take_pending_async_scroll_updates()
{
    return {};
}

void RemotePageCompositor::viewport_size_updated(
    Gfx::IntSize viewport_size,
    bool is_top_level_traversable,
    Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    if (m_started && m_connection->is_open())
        m_connection->viewport_size_updated(m_context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
}

u64 RemotePageCompositor::present_frame(Gfx::IntRect)
{
    return m_next_frame_id++;
}

void RemotePageCompositor::wait_for_frame(u64)
{
}

void RemotePageCompositor::request_screenshot(NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback)
{
    callback();
}

}
