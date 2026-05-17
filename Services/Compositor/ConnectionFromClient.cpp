/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Debug.h>
#include <AK/HashMap.h>
#include <AK/NeverDestroyed.h>
#include <Compositor/ConnectionFromClient.h>
#include <Compositor/ConnectionFromWebContent.h>
#include <LibCore/EventLoop.h>

namespace Compositor {

struct PresentationRegistration {
    Web::Compositor::WebContentConnectionId web_content_connection_id;
    Web::Compositor::PresentationCapability presentation_capability;
    bool is_visible { false };
};

static HashMap<Web::Compositor::PresentationId, PresentationRegistration>& presentation_registrations()
{
    static NeverDestroyed<HashMap<Web::Compositor::PresentationId, PresentationRegistration>> registrations;
    return *registrations;
}

static HashMap<u64, Web::Compositor::PresentationId>& active_presentations()
{
    static NeverDestroyed<HashMap<u64, Web::Compositor::PresentationId>> presentations;
    return *presentations;
}

static void remove_active_presentation(Web::Compositor::PresentationId presentation_id)
{
    active_presentations().remove_all_matching([presentation_id](auto const&, auto active_presentation_id) {
        return active_presentation_id == presentation_id;
    });
}

ConnectionFromClient::ConnectionFromClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionFromClient<CompositorClientEndpoint, CompositorServerEndpoint>(*this, move(transport), 1)
{
}

void ConnectionFromClient::die()
{
    Core::EventLoop::current().quit(0);
}

void ConnectionFromClient::ping()
{
}

Messages::CompositorServer::ConnectWebContentResponse ConnectionFromClient::connect_web_content(IPC::TransportHandle handle)
{
    auto connection_id = ConnectionFromWebContent::connect(move(handle));
    if (connection_id.is_error()) {
        dbgln("Failed to connect WebContent to Compositor: {}", connection_id.error());
        return 0;
    }

    return connection_id.value().value();
}

Messages::CompositorServer::RegisterPresentationResponse ConnectionFromClient::register_presentation(u64 raw_presentation_id, u64 raw_web_content_connection_id, u64 raw_presentation_capability)
{
    auto presentation_id = Web::Compositor::PresentationId { raw_presentation_id };
    auto web_content_connection_id = Web::Compositor::WebContentConnectionId { raw_web_content_connection_id };
    auto presentation_capability = Web::Compositor::PresentationCapability { raw_presentation_capability };

    if (presentation_id.value() == 0) {
        dbgln("Ignoring presentation registration with empty presentation ID");
        return false;
    }

    if (!presentation_capability) {
        dbgln("Ignoring presentation {} registration with empty capability", presentation_id.value());
        return false;
    }

    if (!ConnectionFromWebContent::has_connection(web_content_connection_id)) {
        dbgln("Ignoring presentation {} registration for unknown WebContent connection {}", presentation_id.value(), web_content_connection_id.value());
        return false;
    }

    if (presentation_registrations().contains(presentation_id)) {
        dbgln("Ignoring duplicate presentation {} registration", presentation_id.value());
        return false;
    }

    presentation_registrations().set(presentation_id, PresentationRegistration {
                                                          .web_content_connection_id = web_content_connection_id,
                                                          .presentation_capability = presentation_capability,
                                                      });
    return true;
}

void ConnectionFromClient::unregister_presentation(u64 raw_presentation_id)
{
    auto presentation_id = Web::Compositor::PresentationId { raw_presentation_id };
    if (!presentation_registrations().remove(presentation_id)) {
        dbgln("Ignoring unregister for unknown presentation {}", presentation_id.value());
        return;
    }
    remove_active_presentation(presentation_id);
}

void ConnectionFromClient::set_presentation_visibility(u64 raw_presentation_id, bool is_visible)
{
    auto presentation_id = Web::Compositor::PresentationId { raw_presentation_id };
    auto registration = presentation_registrations().get(presentation_id);
    if (!registration.has_value()) {
        dbgln("Ignoring visibility update for unknown presentation {}", presentation_id.value());
        return;
    }

    registration->is_visible = is_visible;
}

void ConnectionFromClient::set_active_presentation(u64 ui_view_id, Optional<u64> raw_presentation_id)
{
    if (!raw_presentation_id.has_value()) {
        active_presentations().remove(ui_view_id);
        return;
    }

    auto presentation_id = Web::Compositor::PresentationId { *raw_presentation_id };
    if (!presentation_registrations().contains(presentation_id)) {
        dbgln("Ignoring active-presentation update for unknown presentation {}", presentation_id.value());
        return;
    }

    active_presentations().set(ui_view_id, presentation_id);
}

bool ConnectionFromClient::is_registered_presentation(
    Web::Compositor::WebContentConnectionId web_content_connection_id,
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::PresentationCapability presentation_capability)
{
    auto registration = presentation_registrations().get(presentation_id);
    return registration.has_value()
        && registration->web_content_connection_id == web_content_connection_id
        && registration->presentation_capability == presentation_capability;
}

void ConnectionFromClient::unregister_presentations_for_connection(Web::Compositor::WebContentConnectionId web_content_connection_id)
{
    presentation_registrations().remove_all_matching([web_content_connection_id](auto presentation_id, auto const& registration) {
        if (registration.web_content_connection_id != web_content_connection_id)
            return false;
        remove_active_presentation(presentation_id);
        return true;
    });
}

Messages::CompositorServer::AsyncScrollByResponse ConnectionFromClient::async_scroll_by(u64, Gfx::FloatPoint, Gfx::FloatPoint)
{
    return false;
}

Messages::CompositorServer::MouseEventResponse ConnectionFromClient::mouse_event(u64, Web::MouseEvent)
{
    return false;
}

void ConnectionFromClient::ready_to_paint(u64, i32)
{
}

}
