/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <GPUProcess/Forward.h>
#include <GPUProcess/GPUProcessClientEndpoint.h>
#include <GPUProcess/GPUProcessServerEndpoint.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/Typeface.h>
#include <LibGfx/ImmutableBitmap.h>
#include <LibIPC/ConnectionFromClient.h>

namespace GPUProcess {

class ConnectionFromClient final
    : public IPC::ConnectionFromClient<GPUProcessClientEndpoint, GPUProcessServerEndpoint> {
    C_OBJECT(ConnectionFromClient);

public:
    ~ConnectionFromClient() override = default;

    virtual void die() override;

private:
    explicit ConnectionFromClient(NonnullOwnPtr<IPC::Transport>);

    virtual Messages::GPUProcessServer::InitTransportResponse init_transport(int peer_pid) override;
    virtual Messages::GPUProcessServer::ConnectNewClientsResponse connect_new_clients(size_t count) override;

    // Resource registration
    virtual void register_typeface(u64 typeface_id, Core::AnonymousBuffer font_data, u32 ttc_index) override;
    virtual void register_font(u64 font_id, u64 typeface_id, float point_size) override;
    virtual void register_image(u64 image_id, Gfx::ShareableBitmap bitmap) override;
    virtual void release_typeface(u64 typeface_id) override;
    virtual void release_font(u64 font_id) override;
    virtual void release_image(u64 image_id) override;

    // Display list and presentation
    virtual void submit_display_list(u64 page_id, Core::AnonymousBuffer display_list_buffer) override;
    virtual void update_scroll_state(u64 page_id, Core::AnonymousBuffer scroll_state_buffer) override;
    virtual void present_frame(u64 page_id, Gfx::IntRect viewport_rect) override;

    ErrorOr<IPC::TransportHandle> connect_new_client();

    HashMap<u64, NonnullRefPtr<Gfx::Typeface>> m_typefaces;
    HashMap<u64, NonnullRefPtr<Gfx::Font>> m_fonts;
    HashMap<u64, NonnullRefPtr<Gfx::ImmutableBitmap>> m_images;

    // Per-page cached display list buffer
    Core::AnonymousBuffer m_cached_display_list_buffer;
};

}
