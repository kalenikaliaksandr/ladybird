/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <Compositor/CompositorClientEndpoint.h>
#include <Compositor/CompositorServerEndpoint.h>
#include <LibGfx/Rect.h>
#include <LibGfx/SharedImage.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibIPC/TransportHandle.h>
#include <LibWeb/Compositor/Types.h>

namespace Compositor {

class ConnectionFromClient final
    : public IPC::ConnectionFromClient<CompositorClientEndpoint, CompositorServerEndpoint> {
    C_OBJECT(ConnectionFromClient);

public:
    virtual void die() override;

    static bool is_registered_presentation(Web::Compositor::WebContentConnectionId, Web::Compositor::PresentationId, Web::Compositor::PresentationCapability);
    static void unregister_presentations_for_connection(Web::Compositor::WebContentConnectionId);
    static void did_allocate_backing_stores(Web::Compositor::PresentationId, i32 front_bitmap_id, Gfx::SharedImage front_backing_store, i32 back_bitmap_id, Gfx::SharedImage back_backing_store);
    static void did_paint(Web::Compositor::PresentationId, Gfx::IntRect content_rect, i32 bitmap_id);

private:
    explicit ConnectionFromClient(NonnullOwnPtr<IPC::Transport>);

    virtual void ping() override;
    virtual Messages::CompositorServer::ConnectWebContentResponse connect_web_content(IPC::TransportHandle) override;
    virtual Messages::CompositorServer::RegisterPresentationResponse register_presentation(u64 presentation_id, u64 web_content_connection_id, u64 presentation_capability) override;
    virtual void unregister_presentation(u64 presentation_id) override;
    virtual void set_presentation_visibility(u64 presentation_id, bool is_visible) override;
    virtual void set_active_presentation(u64 ui_view_id, Optional<u64> presentation_id) override;
    virtual Messages::CompositorServer::AsyncScrollByResponse async_scroll_by(u64 page_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels) override;
    virtual Messages::CompositorServer::MouseEventResponse mouse_event(u64 page_id, Web::MouseEvent event) override;
    virtual void ready_to_paint(u64 page_id, i32 bitmap_id) override;
};

}
