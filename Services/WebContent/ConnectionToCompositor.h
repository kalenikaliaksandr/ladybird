/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <Compositor/WebContentCompositorClientEndpoint.h>
#include <Compositor/WebContentCompositorServerEndpoint.h>
#include <LibGfx/Size.h>
#include <LibIPC/ConnectionToServer.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace WebContent {

class ConnectionToCompositor final
    : public IPC::ConnectionToServer<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint>
    , public WebContentCompositorClientEndpoint {
    C_OBJECT(ConnectionToCompositor);

public:
    virtual void die() override;

    void create_context(
        Web::Compositor::CompositorContextId,
        u64 page_id,
        Web::Compositor::SerializedPresentationModeKind,
        Web::Compositor::PresentationId,
        Web::Compositor::PresentationCapability,
        Web::Compositor::CompositorContextId target_context_id,
        Web::Painting::CompositorSurfaceId,
        Web::DisplayListPlayerType);
    void destroy_context(Web::Compositor::CompositorContextId);
    void set_presentation_mode(
        Web::Compositor::CompositorContextId,
        Web::Compositor::SerializedPresentationModeKind,
        Web::Compositor::PresentationId,
        Web::Compositor::PresentationCapability,
        Web::Compositor::CompositorContextId target_context_id,
        Web::Painting::CompositorSurfaceId);
    void stop_presenting_to_client(Web::Compositor::CompositorContextId);
    void viewport_size_updated(Web::Compositor::CompositorContextId, Gfx::IntSize, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress);
    void update_display_list(Web::Compositor::CompositorContextId, NonnullRefPtr<Web::Painting::DisplayList> const&, Web::Painting::DisplayListResourceTransaction const&, Web::Painting::ScrollStateSnapshot const&);
    void update_scroll_state(Web::Compositor::CompositorContextId, Web::Painting::ScrollStateSnapshot const&);

private:
    explicit ConnectionToCompositor(NonnullOwnPtr<IPC::Transport>);

    virtual void schedule_rendering_update(u64 page_id) override;
};

}
