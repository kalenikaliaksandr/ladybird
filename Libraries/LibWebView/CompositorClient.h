/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <Compositor/CompositorClientEndpoint.h>
#include <Compositor/CompositorServerEndpoint.h>
#include <LibIPC/ConnectionToServer.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWebView/Export.h>

namespace WebView {

class WEBVIEW_API CompositorClient final
    : public IPC::ConnectionToServer<CompositorClientEndpoint, CompositorServerEndpoint>
    , public CompositorClientEndpoint {
    C_OBJECT_ABSTRACT(CompositorClient);

public:
    explicit CompositorClient(NonnullOwnPtr<IPC::Transport>);

    void register_presentation(Web::Compositor::PresentationId, Web::Compositor::WebContentConnectionId, Web::Compositor::PresentationCapability);
    void unregister_presentation(Web::Compositor::PresentationId);
    void set_presentation_visibility(Web::Compositor::PresentationId, bool is_visible);
    void set_active_presentation(u64 ui_view_id, Optional<Web::Compositor::PresentationId>);

private:
    virtual void die() override;

    virtual void did_allocate_backing_stores(
        u64, i32, Gfx::SharedImage, i32, Gfx::SharedImage) override;
    virtual void did_paint(u64, Gfx::IntRect, i32) override;
};

}
