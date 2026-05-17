/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <Compositor/WebContentCompositorClientEndpoint.h>
#include <Compositor/WebContentCompositorServerEndpoint.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibIPC/TransportHandle.h>
#include <LibWeb/Compositor/Types.h>

namespace Compositor {

class ConnectionFromWebContent final
    : public IPC::ConnectionFromClient<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint> {
    C_OBJECT(ConnectionFromWebContent);

public:
    static ErrorOr<Web::Compositor::WebContentConnectionId> connect(IPC::TransportHandle);

    virtual void die() override;

private:
    ConnectionFromWebContent(NonnullOwnPtr<IPC::Transport>, Web::Compositor::WebContentConnectionId);

    virtual void create_context(u64 context_id, u64 page_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id, Web::DisplayListPlayerType display_list_player_type) override;
    virtual void destroy_context(u64 context_id) override;
    virtual void set_presentation_mode(u64 context_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id) override;
    virtual void stop_presenting_to_client(u64 context_id) override;
    virtual void viewport_size_updated(u64 context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress window_resize_in_progress) override;

    Web::Compositor::WebContentConnectionId m_connection_id;
};

}
