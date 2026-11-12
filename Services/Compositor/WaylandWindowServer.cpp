/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/BackingStoreManager.h>
#include <Compositor/PaintingSurface.h>
#include <Compositor/SkiaBackendContext.h>
#include <Compositor/WaylandWindowServer.h>
#include <LibGfx/SharedImage.h>
#include <LibGfx/VulkanImage.h>

namespace Compositor {

ConnectionToWaylandPresenter::ConnectionToWaylandPresenter(NonnullOwnPtr<IPC::Transport> transport, WaylandWindowServer& window_server)
    : IPC::ConnectionToServer<WaylandPresenterClientEndpoint, WaylandPresenterServerEndpoint>(*this, move(transport))
    , m_window_server(window_server)
{
}

void ConnectionToWaylandPresenter::die()
{
    if (m_window_server)
        m_window_server->presenter_disconnected();
}

void ConnectionToWaylandPresenter::did_release_buffer(u64 buffer_id)
{
    if (m_window_server)
        m_window_server->presenter_did_release_buffer(buffer_id);
}

WaylandPresenterBuffer::WaylandPresenterBuffer(WaylandWindowServer& window_server, u64 buffer_id)
    : m_window_server(window_server)
    , m_buffer_id(buffer_id)
{
}

WaylandPresenterBuffer::~WaylandPresenterBuffer()
{
    if (m_window_server)
        m_window_server->forget_buffer(*this);
}

NonnullRefPtr<WaylandWindowServer> WaylandWindowServer::create(NonnullOwnPtr<IPC::Transport> transport)
{
    auto window_server = adopt_ref(*new WaylandWindowServer);
    window_server->m_presenter = adopt_ref(*new ConnectionToWaylandPresenter(move(transport), window_server));
    return window_server;
}

WaylandWindowServer::~WaylandWindowServer()
{
    on_surface_let_go = nullptr;
    if (!exchange(m_presenter_is_gone, true))
        m_presenter->shutdown();
}

OwnPtr<RemoteLayer> WaylandWindowServer::try_create_remote_layer(u64 remote_layer_id)
{
    if (!presenter())
        return nullptr;
    return make<WaylandRemoteLayer>(*this, remote_layer_id);
}

ErrorOr<NonnullRefPtr<PresentableSurface>> WaylandWindowServer::try_create_presentable_surface(Gfx::IntSize size, RefPtr<Gfx::SkiaBackendContext> const& skia_backend_context)
{
    if (!presenter())
        return Error::from_string_literal("The Wayland presenter is gone");
    if (!skia_backend_context)
        return Error::from_string_literal("The Wayland presenter shows frames only from GPU memory");

    auto image = TRY(BackingStoreManager::create_vulkan_image_the_presentation_can_import(size, *skia_backend_context));
    auto dmabuf = Gfx::duplicate_linux_dmabuf_handle(*image);
    auto buffer_id = m_next_buffer_id++;
    auto buffer = make<WaylandPresenterBuffer>(*this, buffer_id);
    m_buffers.set(buffer_id, buffer.ptr());
    presenter()->async_register_buffer(buffer_id, move(dmabuf));

    auto painting_surface = Gfx::PaintingSurface::create_from_vkimage(*skia_backend_context, move(image), Gfx::PaintingSurface::Origin::TopLeft);
    return PresentableSurface::create(move(painting_surface), Gfx::PaintingSurface::Origin::TopLeft, move(buffer));
}

void WaylandWindowServer::presenter_did_release_buffer(u64 buffer_id)
{
    auto buffer = m_buffers.get(buffer_id);
    if (!buffer.has_value())
        return;
    (*buffer)->did_release();
    if (on_surface_let_go)
        on_surface_let_go();
}

void WaylandWindowServer::presenter_disconnected()
{
    if (exchange(m_presenter_is_gone, true))
        return;
    for (auto& [_, buffer] : m_buffers) {
        while (buffer->may_be_read_by_window_server())
            buffer->did_release();
    }
    if (on_surface_let_go)
        on_surface_let_go();
}

void WaylandWindowServer::forget_buffer(WaylandPresenterBuffer& buffer)
{
    m_buffers.remove(buffer.buffer_id());
    if (auto* presenter = this->presenter())
        presenter->async_forget_buffer(buffer.buffer_id());
}

WaylandRemoteLayer::WaylandRemoteLayer(WaylandWindowServer& window_server, u64 layer_id)
    : m_window_server(window_server)
    , m_layer_id(layer_id)
{
    window_server.presenter()->async_create_layer(layer_id);
}

WaylandRemoteLayer::~WaylandRemoteLayer()
{
    if (m_window_server && m_window_server->presenter())
        m_window_server->presenter()->async_destroy_layer(m_layer_id);
}

Compositing::RemoteLayerHostHandle WaylandRemoteLayer::host_handle() const
{
    return { .kind = Compositing::RemoteLayerHostKind::WaylandPresenterLayer, .id = m_layer_id };
}

void WaylandRemoteLayer::show_frame(Frame frame)
{
    if (!frame.store.has_value() || !m_window_server || !m_window_server->presenter())
        return;
    auto& store = frame.store->surface();
    auto* buffer = static_cast<WaylandPresenterBuffer*>(store.window_server_surface());
    VERIFY(buffer);
    buffer->did_hand_to_presenter();
    store.did_hand_to_window_server();
    m_window_server->presenter()->async_show_frame(m_layer_id, buffer->buffer_id(), frame.painted_size, frame.device_pixel_ratio, frame.fit_within);
}

void WaylandRemoteLayer::place_frame(Optional<Gfx::IntSize> fit_within)
{
    if (m_window_server && m_window_server->presenter())
        m_window_server->presenter()->async_place_frame(m_layer_id, fit_within);
}

}
