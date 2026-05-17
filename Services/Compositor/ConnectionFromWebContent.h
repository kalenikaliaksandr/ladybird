/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <Compositor/WebContentCompositorClientEndpoint.h>
#include <Compositor/WebContentCompositorServerEndpoint.h>
#include <LibGfx/Size.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibIPC/TransportHandle.h>
#include <LibWeb/Compositor/BackingStoreManager.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Compositor {

class ConnectionFromWebContent final
    : public IPC::ConnectionFromClient<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint> {
    C_OBJECT(ConnectionFromWebContent);

public:
    static ErrorOr<Web::Compositor::WebContentConnectionId> connect(IPC::TransportHandle);
    static bool has_connection(Web::Compositor::WebContentConnectionId);
    static void presented_bitmap_ready_to_paint(Web::Compositor::PresentationId, i32 bitmap_id);

    virtual void die() override;

private:
    struct PresentationModeState {
        Web::Compositor::SerializedPresentationModeKind kind;
        Web::Compositor::PresentationId presentation_id;
        Web::Compositor::PresentationCapability presentation_capability;
        Web::Compositor::CompositorContextId target_context_id;
        Web::Painting::CompositorSurfaceId compositor_surface_id;
    };

    struct ContextState {
        struct PendingPresent {
            u64 frame_id { 0 };
            Gfx::IntRect viewport_rect;
        };

        Web::Compositor::CompositorContextId context_id;
        u64 page_id { 0 };
        PresentationModeState presentation_mode;
        Web::DisplayListPlayerType display_list_player_type;
        Optional<Gfx::IntSize> viewport_size;
        Web::Painting::DisplayListResourceStorage display_list_resource_storage;
        RefPtr<Web::Painting::DisplayList> display_list;
        Optional<Web::Painting::ScrollStateSnapshot> scroll_state_snapshot;
        bool has_pending_display_list_update { false };
        bool has_pending_scroll_state_update { false };
        Optional<PendingPresent> pending_present;
        NonnullOwnPtr<Web::Compositor::BackingStoreManager> backing_store_manager;
        Optional<i32> presented_bitmap_id_awaiting_ack;
        u64 submitted_frame_id { 0 };
        u64 completed_frame_id { 0 };
        bool is_top_level_traversable { false };
        Web::Compositor::WindowResizingInProgress window_resize_in_progress { Web::Compositor::WindowResizingInProgress::No };
        bool presents_to_client { true };
    };

    ConnectionFromWebContent(NonnullOwnPtr<IPC::Transport>, Web::Compositor::WebContentConnectionId);

    Optional<PresentationModeState> presentation_mode_state_from_marshaled(Web::Compositor::SerializedPresentationModeKind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id) const;
    bool validate_presentation_mode(Web::Compositor::CompositorContextId, PresentationModeState const&) const;
    ErrorOr<void> allocate_backing_stores_if_needed(ContextState&, Gfx::IntSize viewport_size);
    void rasterize_pending_present(ContextState&);
    void publish_context_to_compositor_surface(ContextState&);
    bool mark_presented_bitmap_ready_to_paint(Web::Compositor::PresentationId, i32 bitmap_id);

    virtual void create_context(u64 context_id, u64 page_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id, Web::DisplayListPlayerType display_list_player_type) override;
    virtual void destroy_context(u64 context_id) override;
    virtual void set_presentation_mode(u64 context_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id) override;
    virtual void stop_presenting_to_client(u64 context_id) override;
    virtual void viewport_size_updated(u64 context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress window_resize_in_progress) override;
    virtual void update_display_list(u64 context_id, NonnullRefPtr<Web::Painting::DisplayList> display_list, Web::Painting::DisplayListResourceTransaction resource_transaction, Web::Painting::ScrollStateSnapshot scroll_state) override;
    virtual void update_scroll_state(u64 context_id, Web::Painting::ScrollStateSnapshot scroll_state) override;
    virtual void update_compositor_surface(u64 context_id, u64 surface_id, Gfx::SharedImage shared_image) override;
    virtual void clear_compositor_surface(u64 context_id, u64 surface_id) override;
    virtual Messages::WebContentCompositorServer::PresentFrameResponse present_frame(u64 context_id, Gfx::IntRect viewport_rect) override;
    virtual void wait_for_frame(u64 context_id, u64 frame_id) override;

    Web::Compositor::WebContentConnectionId m_connection_id;
    HashMap<Web::Compositor::CompositorContextId, ContextState> m_contexts;
};

}
