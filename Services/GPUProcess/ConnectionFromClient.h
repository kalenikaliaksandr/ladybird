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
#include <LibGfx/PaintingSurface.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

class DisplayListPlayerSkia;

}

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

    virtual void register_typeface(u64 typeface_id, Core::AnonymousBuffer font_data, u32 ttc_index) override;
    virtual void register_font(u64 font_id, u64 typeface_id, float point_size) override;
    virtual void register_image(u64 image_id, Gfx::ShareableBitmap bitmap) override;
    virtual void release_typeface(u64 typeface_id) override;
    virtual void release_font(u64 font_id) override;
    virtual void release_image(u64 image_id) override;

    virtual void update_backing_stores(u64 page_id, i32 front_bitmap_id, Gfx::ShareableBitmap front_bitmap, i32 back_bitmap_id, Gfx::ShareableBitmap back_bitmap) override;

    virtual void submit_display_list(u64 page_id, Core::AnonymousBuffer display_list_buffer) override;
    virtual void update_scroll_state(u64 page_id, Core::AnonymousBuffer scroll_state_buffer) override;
    virtual void present_frame(u64 page_id, Gfx::IntRect viewport_rect) override;

    ErrorOr<IPC::TransportHandle> connect_new_client();

    HashMap<u64, NonnullRefPtr<Gfx::Typeface>> m_typefaces;
    HashMap<u64, NonnullRefPtr<Gfx::Font>> m_fonts;
    HashMap<u64, NonnullRefPtr<Gfx::ImmutableBitmap>> m_images;

    Core::AnonymousBuffer m_cached_display_list_buffer;
    RefPtr<Web::Painting::DisplayList> m_cached_display_list;
    Web::Painting::ScrollStateSnapshotByDisplayList m_cached_scroll_states;
    OwnPtr<Web::Painting::DisplayListPlayerSkia> m_skia_player;

    // Backing store state
    i32 m_front_bitmap_id { -1 };
    i32 m_back_bitmap_id { -1 };
    RefPtr<Gfx::PaintingSurface> m_front_surface;
    RefPtr<Gfx::PaintingSurface> m_back_surface;
};

}
