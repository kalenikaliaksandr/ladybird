/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Debug.h>
#include <LibWebView/CompositorClient.h>
#include <LibWebView/WebContentClient.h>

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
    m_web_content_clients_by_presentation.remove(presentation_id);
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

void CompositorClient::register_web_content_client(WebContentClient& web_content_client)
{
    m_web_content_clients_by_presentation.set(web_content_client.presentation_id(), web_content_client.make_weak_ptr<WebContentClient>());
}

void CompositorClient::unregister_web_content_client(WebContentClient& web_content_client)
{
    m_web_content_clients_by_presentation.remove(web_content_client.presentation_id());
}

bool CompositorClient::async_scroll_by(Web::Compositor::PresentationId presentation_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels)
{
    auto response = send_sync_but_allow_failure<Messages::CompositorServer::AsyncScrollBy>(
        presentation_id.value(),
        position,
        delta_in_device_pixels);
    return response && response->handled();
}

bool CompositorClient::mouse_event(Web::Compositor::PresentationId presentation_id, Web::MouseEvent const& event)
{
    auto response = send_sync_but_allow_failure<Messages::CompositorServer::MouseEvent>(
        presentation_id.value(),
        event.clone_without_browser_data());
    return response && response->handled();
}

void CompositorClient::ready_to_paint(Web::Compositor::PresentationId presentation_id, i32 bitmap_id)
{
    async_ready_to_paint(presentation_id.value(), bitmap_id);
}

void CompositorClient::did_allocate_backing_stores(u64 raw_presentation_id, i32 front_bitmap_id, Gfx::SharedImage front_backing_store, i32 back_bitmap_id, Gfx::SharedImage back_backing_store)
{
    auto presentation_id = Web::Compositor::PresentationId { raw_presentation_id };
    auto web_content_client = m_web_content_clients_by_presentation.find(presentation_id);
    if (web_content_client == m_web_content_clients_by_presentation.end()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] UI dropping backing stores for unknown presentation {}",
            presentation_id.value());
        return;
    }

    auto client = web_content_client->value.strong_ref();
    if (!client) {
        m_web_content_clients_by_presentation.remove(presentation_id);
        return;
    }

    client->did_present_backing_stores_for_presentation(presentation_id, front_bitmap_id, move(front_backing_store), back_bitmap_id, move(back_backing_store));
}

void CompositorClient::did_paint(u64 raw_presentation_id, Gfx::IntRect content_rect, i32 bitmap_id)
{
    auto presentation_id = Web::Compositor::PresentationId { raw_presentation_id };
    auto web_content_client = m_web_content_clients_by_presentation.find(presentation_id);
    if (web_content_client == m_web_content_clients_by_presentation.end()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] UI dropping did_paint for unknown presentation {} bitmap {}",
            presentation_id.value(), bitmap_id);
        ready_to_paint(presentation_id, bitmap_id);
        return;
    }

    auto client = web_content_client->value.strong_ref();
    if (!client) {
        m_web_content_clients_by_presentation.remove(presentation_id);
        ready_to_paint(presentation_id, bitmap_id);
        return;
    }

    client->did_present_bitmap_for_presentation(presentation_id, content_rect, bitmap_id);
}

}
