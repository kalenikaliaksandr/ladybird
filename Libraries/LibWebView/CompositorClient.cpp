/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWebView/CompositorClient.h>

namespace WebView {

CompositorClient::CompositorClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionToServer<CompositorClientEndpoint, CompositorServerEndpoint>(*this, move(transport))
{
}

void CompositorClient::die()
{
}

Optional<Web::Compositor::WebContentConnectionId> CompositorClient::connect_web_content(IPC::TransportHandle handle)
{
    auto response = send_sync_but_allow_failure<Messages::CompositorServer::ConnectWebContent>(move(handle));
    if (!response || response->web_content_connection_id() == 0)
        return {};
    return Web::Compositor::WebContentConnectionId { response->web_content_connection_id() };
}

bool CompositorClient::register_presentation(
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::WebContentConnectionId web_content_connection_id,
    Web::Compositor::PresentationCapability presentation_capability)
{
    auto response = send_sync_but_allow_failure<Messages::CompositorServer::RegisterPresentation>(
        presentation_id.value(),
        web_content_connection_id.value(),
        presentation_capability.value());
    return response && response->accepted();
}

void CompositorClient::unregister_presentation(Web::Compositor::PresentationId presentation_id)
{
    async_unregister_presentation(presentation_id.value());
}

void CompositorClient::set_presentation_visibility(Web::Compositor::PresentationId presentation_id, bool is_visible)
{
    async_set_presentation_visibility(presentation_id.value(), is_visible);
}

void CompositorClient::set_active_presentation(u64 ui_view_id, Optional<Web::Compositor::PresentationId> presentation_id)
{
    Optional<u64> raw_presentation_id;
    if (presentation_id.has_value())
        raw_presentation_id = presentation_id->value();
    async_set_active_presentation(ui_view_id, raw_presentation_id);
}

void CompositorClient::did_allocate_backing_stores(u64, i32, Gfx::SharedImage, i32, Gfx::SharedImage)
{
}

void CompositorClient::did_paint(u64, Gfx::IntRect, i32)
{
}

}
