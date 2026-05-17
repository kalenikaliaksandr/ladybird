/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/HashMap.h>
#include <AK/Optional.h>
#include <AK/WeakPtr.h>
#include <Compositor/CompositorClientEndpoint.h>
#include <Compositor/CompositorServerEndpoint.h>
#include <LibGfx/Point.h>
#include <LibIPC/ConnectionToServer.h>
#include <LibIPC/TransportHandle.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Page/InputEvent.h>
#include <LibWebView/Export.h>

namespace WebView {

class WebContentClient;

class WEBVIEW_API CompositorClient final
    : public IPC::ConnectionToServer<CompositorClientEndpoint, CompositorServerEndpoint>
    , public CompositorClientEndpoint {
    C_OBJECT_ABSTRACT(CompositorClient);

public:
    explicit CompositorClient(NonnullOwnPtr<IPC::Transport>);

    Optional<Web::Compositor::WebContentConnectionId> connect_web_content(IPC::TransportHandle);
    bool register_presentation(Web::Compositor::PresentationId, Web::Compositor::WebContentConnectionId, Web::Compositor::PresentationCapability);
    void unregister_presentation(Web::Compositor::PresentationId);
    void set_presentation_visibility(Web::Compositor::PresentationId, bool is_visible);
    void set_active_presentation(u64 ui_view_id, Optional<Web::Compositor::PresentationId>);
    void register_web_content_client(WebContentClient&);
    void unregister_web_content_client(WebContentClient&);
    bool async_scroll_by(Web::Compositor::PresentationId, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels);
    bool mouse_event(Web::Compositor::PresentationId, Web::MouseEvent const&);
    void ready_to_paint(Web::Compositor::PresentationId, i32 bitmap_id);

    Function<void()> on_death;

private:
    virtual void die() override;

    virtual void did_allocate_backing_stores(
        u64, i32, Gfx::SharedImage, i32, Gfx::SharedImage) override;
    virtual void did_paint(u64, Gfx::IntRect, i32) override;

    HashMap<Web::Compositor::PresentationId, WeakPtr<WebContentClient>> m_web_content_clients_by_presentation;
};

}
