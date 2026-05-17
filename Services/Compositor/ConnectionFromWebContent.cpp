/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Debug.h>
#include <AK/HashMap.h>
#include <AK/IDAllocator.h>
#include <AK/NeverDestroyed.h>
#include <AK/NonnullRefPtr.h>
#include <AK/StdLibExtras.h>
#include <Compositor/ConnectionFromClient.h>
#include <Compositor/ConnectionFromWebContent.h>

namespace Compositor {

static IDAllocator s_connection_ids;

static HashMap<Web::Compositor::WebContentConnectionId, NonnullRefPtr<ConnectionFromWebContent>>& web_content_connections()
{
    static NeverDestroyed<HashMap<Web::Compositor::WebContentConnectionId, NonnullRefPtr<ConnectionFromWebContent>>> connections;
    return *connections;
}

ErrorOr<Web::Compositor::WebContentConnectionId> ConnectionFromWebContent::connect(IPC::TransportHandle handle)
{
    auto raw_connection_id = s_connection_ids.allocate();
    auto connection_id = Web::Compositor::WebContentConnectionId { static_cast<u64>(raw_connection_id) };

    auto transport = TRY(handle.create_transport());
    auto connection = ConnectionFromWebContent::construct(move(transport), connection_id);
    web_content_connections().set(connection_id, connection);

    return connection_id;
}

bool ConnectionFromWebContent::has_connection(Web::Compositor::WebContentConnectionId connection_id)
{
    return web_content_connections().contains(connection_id);
}

ConnectionFromWebContent::ConnectionFromWebContent(NonnullOwnPtr<IPC::Transport> transport, Web::Compositor::WebContentConnectionId connection_id)
    : IPC::ConnectionFromClient<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint>(*this, move(transport), static_cast<int>(connection_id.value()))
    , m_connection_id(connection_id)
{
}

void ConnectionFromWebContent::die()
{
    m_contexts.clear();
    Compositor::ConnectionFromClient::unregister_presentations_for_connection(m_connection_id);
    web_content_connections().remove(m_connection_id);
    s_connection_ids.deallocate(static_cast<int>(m_connection_id.value()));
}

Optional<ConnectionFromWebContent::PresentationModeState> ConnectionFromWebContent::presentation_mode_state_from_marshaled(
    Web::Compositor::SerializedPresentationModeKind kind,
    u64 presentation_id,
    u64 presentation_capability,
    u64 target_context_id,
    u64 compositor_surface_id)
    const
{
    switch (kind) {
    case Web::Compositor::SerializedPresentationModeKind::PresentToUI:
    case Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface:
        return PresentationModeState {
            .kind = kind,
            .presentation_id = Web::Compositor::PresentationId { presentation_id },
            .presentation_capability = Web::Compositor::PresentationCapability { presentation_capability },
            .target_context_id = Web::Compositor::CompositorContextId { target_context_id },
            .compositor_surface_id = Web::Painting::CompositorSurfaceId { compositor_surface_id },
        };
    }

    dbgln("Rejecting unknown presentation mode kind {} from WebContent connection {}", to_underlying(kind), m_connection_id.value());
    return {};
}

bool ConnectionFromWebContent::validate_presentation_mode(Web::Compositor::CompositorContextId context_id, PresentationModeState const& presentation_mode) const
{
    switch (presentation_mode.kind) {
    case Web::Compositor::SerializedPresentationModeKind::PresentToUI:
        if (presentation_mode.target_context_id.value() != 0 || presentation_mode.compositor_surface_id.value() != 0) {
            dbgln("Rejecting PresentToUI mode with compositor-surface target for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
            return false;
        }
        if (!Compositor::ConnectionFromClient::is_registered_presentation(m_connection_id, presentation_mode.presentation_id, presentation_mode.presentation_capability)) {
            dbgln("Rejecting unregistered PresentToUI mode for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
            return false;
        }
        return true;
    case Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface:
        if (presentation_mode.presentation_id.value() != 0 || presentation_mode.presentation_capability.value() != 0) {
            dbgln("Rejecting surface-publish mode with UI presentation data for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
            return false;
        }
        if (presentation_mode.target_context_id.value() == 0 || presentation_mode.compositor_surface_id.value() == 0) {
            dbgln("Rejecting surface-publish mode with empty target for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
            return false;
        }
        if (presentation_mode.target_context_id == context_id) {
            dbgln("Rejecting surface-publish mode targeting itself for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
            return false;
        }
        if (!m_contexts.contains(presentation_mode.target_context_id)) {
            dbgln("Rejecting surface-publish mode targeting missing context {} for context {} from WebContent connection {}", presentation_mode.target_context_id.value(), context_id.value(), m_connection_id.value());
            return false;
        }
        return true;
    }

    VERIFY_NOT_REACHED();
}

void ConnectionFromWebContent::create_context(
    u64 raw_context_id,
    u64 page_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    u64 presentation_id,
    u64 presentation_capability,
    u64 target_context_id,
    u64 compositor_surface_id,
    Web::DisplayListPlayerType display_list_player_type)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    if (context_id.value() == 0) {
        dbgln("Ignoring compositor context with empty ID from WebContent connection {}", m_connection_id.value());
        return;
    }

    if (m_contexts.contains(context_id)) {
        dbgln("Ignoring duplicate compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto presentation_mode = presentation_mode_state_from_marshaled(presentation_mode_kind, presentation_id, presentation_capability, target_context_id, compositor_surface_id);
    if (!presentation_mode.has_value() || !validate_presentation_mode(context_id, *presentation_mode))
        return;

    m_contexts.set(context_id, ContextState {
                                   .context_id = context_id,
                                   .page_id = page_id,
                                   .presentation_mode = *presentation_mode,
                                   .display_list_player_type = display_list_player_type,
                                   .viewport_size = {},
                                   .is_top_level_traversable = false,
                                   .window_resize_in_progress = Web::Compositor::WindowResizingInProgress::No,
                                   .presents_to_client = presentation_mode->kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI,
                               });
}

void ConnectionFromWebContent::destroy_context(u64 raw_context_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    if (!m_contexts.remove(context_id))
        dbgln("Ignoring destroy for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
}

void ConnectionFromWebContent::set_presentation_mode(
    u64 raw_context_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    u64 presentation_id,
    u64 presentation_capability,
    u64 target_context_id,
    u64 compositor_surface_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring presentation mode update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto presentation_mode = presentation_mode_state_from_marshaled(presentation_mode_kind, presentation_id, presentation_capability, target_context_id, compositor_surface_id);
    if (!presentation_mode.has_value() || !validate_presentation_mode(context_id, *presentation_mode))
        return;

    context->presentation_mode = *presentation_mode;
    context->presents_to_client = presentation_mode->kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI;
}

void ConnectionFromWebContent::stop_presenting_to_client(u64 raw_context_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring stop-presenting for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->presents_to_client = false;
}

void ConnectionFromWebContent::viewport_size_updated(
    u64 raw_context_id,
    Gfx::IntSize viewport_size,
    bool is_top_level_traversable,
    Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring viewport update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->viewport_size = viewport_size;
    context->is_top_level_traversable = is_top_level_traversable;
    context->window_resize_in_progress = window_resize_in_progress;
}

}
