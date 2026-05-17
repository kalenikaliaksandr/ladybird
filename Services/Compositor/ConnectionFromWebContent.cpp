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
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>
#include <LibWeb/Painting/DisplayListPlayerSkia.h>

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

void ConnectionFromWebContent::presented_bitmap_ready_to_paint(Web::Compositor::PresentationId presentation_id, i32 bitmap_id)
{
    for (auto& entry : web_content_connections()) {
        if (entry.value->mark_presented_bitmap_ready_to_paint(presentation_id, bitmap_id))
            return;
    }

    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring ready_to_paint for unknown presentation {} bitmap {}",
        presentation_id.value(), bitmap_id);
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

ErrorOr<void> ConnectionFromWebContent::allocate_backing_stores_if_needed(ContextState& context, Gfx::IntSize viewport_size)
{
    auto allocation = context.backing_store_manager->resize_backing_stores_if_needed(
        viewport_size,
        context.is_top_level_traversable,
        context.window_resize_in_progress);
    if (!allocation.has_value())
        return {};

    auto should_publish = context.presents_to_client
        && context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI;
    auto publication = context.backing_store_manager->allocate_backing_stores(
        *allocation,
        {},
        should_publish);
    if (!publication.has_value())
        return {};

    Compositor::ConnectionFromClient::did_allocate_backing_stores(
        context.presentation_mode.presentation_id,
        publication->front_bitmap_id,
        move(publication->front_shared_image),
        publication->back_bitmap_id,
        move(publication->back_shared_image));
    return {};
}

void ConnectionFromWebContent::rasterize_pending_present(ContextState& context)
{
    if (!context.pending_present.has_value())
        return;

    if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI
        && context.presents_to_client
        && context.presented_bitmap_id_awaiting_ack.has_value()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Deferring present for context {} until bitmap {} is ready to paint",
            context.context_id.value(), context.presented_bitmap_id_awaiting_ack.value());
        return;
    }

    auto pending_present = *context.pending_present;
    auto viewport_size = context.viewport_size.value_or(pending_present.viewport_rect.size());
    auto allocation = allocate_backing_stores_if_needed(context, viewport_size);
    if (allocation.is_error()) {
        dbgln("Skipping present for compositor context {} from WebContent connection {}: {}", context.context_id.value(), m_connection_id.value(), allocation.error());
        context.completed_frame_id = pending_present.frame_id;
        context.pending_present.clear();
        return;
    }

    if (!context.display_list || !context.scroll_state_snapshot.has_value() || !context.backing_store_manager->is_valid()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Skipping present for context {}: display_list={}, scroll_state={}, backing_stores={}",
            context.context_id.value(), !!context.display_list, context.scroll_state_snapshot.has_value(), context.backing_store_manager->is_valid());
        context.completed_frame_id = pending_present.frame_id;
        context.pending_present.clear();
        return;
    }

    auto& back_store = context.backing_store_manager->back_store();
    Web::Painting::DisplayListPlayerSkia player;
    player.execute(*context.display_list, context.display_list_resource_storage, *context.scroll_state_snapshot, back_store);

    auto rendered_bitmap_id = context.backing_store_manager->back_bitmap_id();
    context.backing_store_manager->swap();

    if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI && context.presents_to_client) {
        context.presented_bitmap_id_awaiting_ack = rendered_bitmap_id;
        Compositor::ConnectionFromClient::did_paint(
            context.presentation_mode.presentation_id,
            pending_present.viewport_rect,
            rendered_bitmap_id);
    } else if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface) {
        publish_context_to_compositor_surface(context);
    }

    context.completed_frame_id = pending_present.frame_id;
    context.pending_present.clear();
    context.has_pending_display_list_update = false;
    context.has_pending_scroll_state_update = false;
}

void ConnectionFromWebContent::publish_context_to_compositor_surface(ContextState& context)
{
    VERIFY(context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface);

    auto target_context_id = context.presentation_mode.target_context_id;
    auto target_context = m_contexts.get(target_context_id);
    if (!target_context.has_value()) {
        dbgln("Cannot publish compositor context {} to missing target context {} from WebContent connection {}",
            context.context_id.value(), target_context_id.value(), m_connection_id.value());
        return;
    }

    auto& front_store = context.backing_store_manager->front_store();
    target_context->display_list_resource_storage.update_compositor_surface(
        context.presentation_mode.compositor_surface_id,
        front_store.snapshot_into_shared_image());
    rasterize_pending_present(*target_context);
}

bool ConnectionFromWebContent::mark_presented_bitmap_ready_to_paint(Web::Compositor::PresentationId presentation_id, i32 bitmap_id)
{
    for (auto& entry : m_contexts) {
        auto& context = entry.value;
        if (context.presentation_mode.kind != Web::Compositor::SerializedPresentationModeKind::PresentToUI)
            continue;
        if (context.presentation_mode.presentation_id != presentation_id)
            continue;

        if (context.presented_bitmap_id_awaiting_ack != bitmap_id) {
            dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring stale ready_to_paint for presentation {} bitmap {} while awaiting bitmap {}",
                presentation_id.value(), bitmap_id, context.presented_bitmap_id_awaiting_ack.value_or(-1));
            return true;
        }

        context.presented_bitmap_id_awaiting_ack.clear();
        rasterize_pending_present(context);
        return true;
    }

    return false;
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
                                   .display_list_resource_storage = {},
                                   .display_list = {},
                                   .scroll_state_snapshot = {},
                                   .has_pending_display_list_update = false,
                                   .has_pending_scroll_state_update = false,
                                   .pending_present = {},
                                   .backing_store_manager = make<Web::Compositor::BackingStoreManager>(),
                                   .presented_bitmap_id_awaiting_ack = {},
                                   .submitted_frame_id = 0,
                                   .completed_frame_id = 0,
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

    if (auto allocation = allocate_backing_stores_if_needed(*context, viewport_size); allocation.is_error())
        dbgln("Failed to allocate backing stores for compositor context {} from WebContent connection {}: {}", context_id.value(), m_connection_id.value(), allocation.error());
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::update_display_list(
    u64 raw_context_id,
    NonnullRefPtr<Web::Painting::DisplayList> display_list,
    Web::Painting::DisplayListResourceTransaction resource_transaction,
    Web::Painting::ScrollStateSnapshot scroll_state)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring display list update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->display_list_resource_storage.apply_transaction(move(resource_transaction));

    context->display_list = move(display_list);
    context->scroll_state_snapshot = move(scroll_state);
    context->has_pending_display_list_update = true;
    context->has_pending_scroll_state_update = true;
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::update_scroll_state(u64 raw_context_id, Web::Painting::ScrollStateSnapshot scroll_state)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring scroll state update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->scroll_state_snapshot = move(scroll_state);
    context->has_pending_scroll_state_update = true;
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::update_compositor_surface(u64 raw_context_id, u64 raw_surface_id, Gfx::SharedImage shared_image)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring compositor surface update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto surface_id = Web::Painting::CompositorSurfaceId { raw_surface_id };
    if (surface_id.value() == 0) {
        dbgln("Ignoring compositor surface update with empty surface ID for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->display_list_resource_storage.update_compositor_surface(surface_id, move(shared_image));
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::clear_compositor_surface(u64 raw_context_id, u64 raw_surface_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring compositor surface clear for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto surface_id = Web::Painting::CompositorSurfaceId { raw_surface_id };
    if (surface_id.value() == 0) {
        dbgln("Ignoring compositor surface clear with empty surface ID for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->display_list_resource_storage.clear_compositor_surface(surface_id);
    rasterize_pending_present(*context);
}

Messages::WebContentCompositorServer::PresentFrameResponse ConnectionFromWebContent::present_frame(u64 raw_context_id, Gfx::IntRect viewport_rect)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring present for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return 0;
    }

    auto frame_id = ++context->submitted_frame_id;
    context->pending_present = ContextState::PendingPresent {
        .frame_id = frame_id,
        .viewport_rect = viewport_rect,
    };

    rasterize_pending_present(*context);
    return frame_id;
}

void ConnectionFromWebContent::wait_for_frame(u64 raw_context_id, u64 frame_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring frame wait for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    if (frame_id > context->submitted_frame_id) {
        dbgln("Ignoring wait for unknown frame {} on compositor context {} from WebContent connection {}", frame_id, context_id.value(), m_connection_id.value());
        return;
    }
}

}
