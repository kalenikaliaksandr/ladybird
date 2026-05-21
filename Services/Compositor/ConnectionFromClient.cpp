/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/ConnectionFromClient.h>
#include <Compositor/ConnectionFromWebContent.h>
#include <LibCore/EventLoop.h>
#include <LibIPC/Transport.h>

namespace Compositor {

ConnectionFromClient::ConnectionFromClient(NonnullOwnPtr<IPC::Transport> transport, RefPtr<Gfx::SkiaBackendContext> skia_backend_context, bool async_scrolling_enabled)
    : IPC::ConnectionFromClient<CompositorControlClientEndpoint, CompositorControlServerEndpoint>(*this, move(transport), 1)
    , m_compositor_state(CompositorState::create(move(skia_backend_context), async_scrolling_enabled))
{
    m_compositor_state->set_client(*this);
}

void ConnectionFromClient::die()
{
    for (auto& connection : m_web_content_connections)
        connection->notify_compositor_lost();
    Core::EventLoop::current().quit(0);
}

void ConnectionFromClient::did_allocate_backing_stores(Web::Compositor::CompositorContextId context_id, i32 front_bitmap_id, Gfx::SharedImage&& front_backing_store, i32 back_bitmap_id, Gfx::SharedImage&& back_backing_store)
{
    async_did_allocate_backing_stores(context_id, front_bitmap_id, move(front_backing_store), back_bitmap_id, move(back_backing_store));
}

void ConnectionFromClient::did_present_frame(Web::Compositor::CompositorContextId context_id, Gfx::IntRect content_rect, i32 bitmap_id)
{
    async_did_present_frame(context_id, content_rect, bitmap_id);
}

void ConnectionFromClient::did_request_cursor_change(Web::Compositor::CompositorContextId context_id, Gfx::Cursor const& cursor)
{
    async_did_request_cursor_change(context_id, cursor);
}

Messages::CompositorControlServer::ConnectWebContentResponse ConnectionFromClient::connect_web_content()
{
    auto paired_transport = MUST(IPC::Transport::create_paired());
    auto connection = ConnectionFromWebContent::construct(move(paired_transport.local), m_compositor_state);
    connection->set_on_death([this](ConnectionFromWebContent& dead) {
        m_web_content_connections.remove_first_matching([&](auto& entry) { return entry.ptr() == &dead; });
    });
    m_web_content_connections.append(move(connection));
    return move(paired_transport.remote_handle);
}

void ConnectionFromClient::create_context(Web::Compositor::CompositorContextId context_id, Optional<u64> page_id, Web::Compositor::PagePresentationRegistration page_presentation_registration)
{
    m_compositor_state->create_context(context_id, page_id, page_presentation_registration);
}

void ConnectionFromClient::destroy_context(Web::Compositor::CompositorContextId context_id)
{
    m_compositor_state->destroy_context(context_id);
}

void ConnectionFromClient::viewport_size_updated(Web::Compositor::CompositorContextId context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    m_compositor_state->viewport_size_updated(context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void ConnectionFromClient::device_pixels_per_css_pixel_updated(Web::Compositor::CompositorContextId context_id, double device_pixels_per_css_pixel)
{
    m_compositor_state->device_pixels_per_css_pixel_updated(context_id, device_pixels_per_css_pixel);
}

void ConnectionFromClient::system_visibility_state_updated(Web::Compositor::CompositorContextId context_id, Web::HTML::VisibilityState visibility_state)
{
    m_compositor_state->system_visibility_state_updated(context_id, visibility_state);
}

void ConnectionFromClient::window_occlusion_state_updated(Web::Compositor::CompositorContextId context_id, bool is_occluded)
{
    m_compositor_state->window_occlusion_state_updated(context_id, is_occluded);
}

Messages::CompositorControlServer::MouseEventResponse ConnectionFromClient::mouse_event(Web::Compositor::CompositorContextId context_id, Web::MouseEvent event)
{
    return m_compositor_state->mouse_event(context_id, event);
}

void ConnectionFromClient::forward_mouse_event(Web::Compositor::CompositorContextId context_id, Web::MouseEvent event)
{
    m_compositor_state->forward_mouse_event(context_id, event);
}

Messages::CompositorControlServer::AsyncScrollByResponse ConnectionFromClient::async_scroll_by(Web::Compositor::CompositorContextId context_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels)
{
    return m_compositor_state->async_scroll_by(context_id, position, delta_in_device_pixels);
}

void ConnectionFromClient::presented_bitmap_ready_to_paint(Web::Compositor::CompositorContextId context_id, i32 bitmap_id)
{
    m_compositor_state->presented_bitmap_ready_to_paint(context_id, bitmap_id);
}

}
