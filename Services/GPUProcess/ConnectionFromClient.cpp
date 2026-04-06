/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/IDAllocator.h>
#include <GPUProcess/ConnectionFromClient.h>
#include <LibCore/EventLoop.h>
#include <LibCore/System.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/Font/Typeface.h>
#include <LibIPC/TransportHandle.h>

namespace GPUProcess {

static HashMap<int, RefPtr<ConnectionFromClient>> s_connections;
static IDAllocator s_client_ids;

ConnectionFromClient::ConnectionFromClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionFromClient<GPUProcessClientEndpoint, GPUProcessServerEndpoint>(*this, move(transport), s_client_ids.allocate())
{
    s_connections.set(client_id(), *this);
}

void ConnectionFromClient::die()
{
    auto client_id = this->client_id();
    s_connections.remove(client_id);
    s_client_ids.deallocate(client_id);

    if (s_connections.is_empty())
        Core::EventLoop::current().quit(0);
}

Messages::GPUProcessServer::InitTransportResponse ConnectionFromClient::init_transport([[maybe_unused]] int peer_pid)
{
#ifdef AK_OS_WINDOWS
    m_transport->set_peer_pid(peer_pid);
    return Core::System::getpid();
#endif
    VERIFY_NOT_REACHED();
}

ErrorOr<IPC::TransportHandle> ConnectionFromClient::connect_new_client()
{
    auto paired = TRY(IPC::Transport::create_paired());
    auto handle = move(paired.remote_handle);

    auto client = adopt_ref(*new ConnectionFromClient(move(paired.local)));

    return handle;
}

Messages::GPUProcessServer::ConnectNewClientsResponse ConnectionFromClient::connect_new_clients(size_t count)
{
    Vector<IPC::TransportHandle> handles;
    handles.ensure_capacity(count);
    for (size_t i = 0; i < count; ++i) {
        auto handle_or_error = connect_new_client();
        if (handle_or_error.is_error()) {
            dbgln("GPUProcess: Failed to connect new client: {}", handle_or_error.error());
            return Vector<IPC::TransportHandle> {};
        }
        handles.unchecked_append(handle_or_error.release_value());
    }
    return handles;
}

void ConnectionFromClient::register_typeface(u64 typeface_id, Core::AnonymousBuffer font_data, u32 ttc_index)
{
    if (!font_data.is_valid()) {
        dbgln("GPUProcess: Invalid font data for typeface {}", typeface_id);
        return;
    }

    auto typeface_or_error = Gfx::Typeface::try_load_from_externally_owned_memory(
        ReadonlyBytes { font_data.data<u8>(), font_data.size() },
        ttc_index);

    if (typeface_or_error.is_error()) {
        dbgln("GPUProcess: Failed to load typeface {}: {}", typeface_id, typeface_or_error.error());
        return;
    }

    m_typefaces.set(typeface_id, typeface_or_error.release_value());
}

void ConnectionFromClient::register_font(u64 font_id, u64 typeface_id, float point_size)
{
    auto typeface_it = m_typefaces.find(typeface_id);
    if (typeface_it == m_typefaces.end()) {
        dbgln("GPUProcess: Unknown typeface {} for font {}", typeface_id, font_id);
        return;
    }

    // FIXME: Pass font variation settings and shape features
    auto font = typeface_it->value->font(point_size);
    m_fonts.set(font_id, move(font));
}

void ConnectionFromClient::register_image(u64 image_id, Gfx::ShareableBitmap shareable_bitmap)
{
    if (!shareable_bitmap.is_valid()) {
        dbgln("GPUProcess: Invalid bitmap for image {}", image_id);
        return;
    }

    auto bitmap = Gfx::ImmutableBitmap::create(*shareable_bitmap.bitmap());
    m_images.set(image_id, move(bitmap));
}

void ConnectionFromClient::release_typeface(u64 typeface_id)
{
    m_typefaces.remove(typeface_id);
}

void ConnectionFromClient::release_font(u64 font_id)
{
    m_fonts.remove(font_id);
}

void ConnectionFromClient::release_image(u64 image_id)
{
    m_images.remove(image_id);
}

void ConnectionFromClient::submit_display_list(u64 page_id, Core::AnonymousBuffer display_list_buffer)
{
    (void)page_id;

    if (!display_list_buffer.is_valid()) {
        dbgln("GPUProcess: Invalid display list buffer");
        return;
    }

    m_cached_display_list_buffer = move(display_list_buffer);

    // FIXME: Deserialize and pass to rendering thread
}

void ConnectionFromClient::update_scroll_state(u64 page_id, Core::AnonymousBuffer scroll_state_buffer)
{
    // FIXME: Update cached scroll state without replacing display list
    (void)page_id;
    (void)scroll_state_buffer;
}

void ConnectionFromClient::present_frame(u64 page_id, Gfx::IntRect viewport_rect)
{
    // FIXME: Execute display list on rendering thread and send did_paint
    (void)page_id;
    (void)viewport_rect;
}

}
