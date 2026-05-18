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
#include <LibCore/EventLoop.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibMedia/VideoFrame.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>
#include <LibWeb/Compositor/ViewportScrollbars.h>
#include <LibWeb/Page/InputEvent.h>
#include <LibWeb/Painting/DisplayListPlayerSkia.h>

namespace Compositor {

static IDAllocator s_connection_ids;

static RefPtr<Gfx::SkiaBackendContext> skia_backend_context_for(Web::DisplayListPlayerType display_list_player_type)
{
    switch (display_list_player_type) {
    case Web::DisplayListPlayerType::SkiaGPUIfAvailable:
        return Gfx::SkiaBackendContext::the_main_thread_context();
    case Web::DisplayListPlayerType::SkiaCPU:
        return nullptr;
    }
    VERIFY_NOT_REACHED();
}

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

bool ConnectionFromWebContent::async_scroll_by(Web::Compositor::PresentationId presentation_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels)
{
    for (auto& entry : web_content_connections()) {
        if (auto* context = entry.value->context_for_presentation(presentation_id))
            return entry.value->async_scroll_by(*context, position, delta_in_device_pixels);
    }

    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring async_scroll_by for unknown presentation {}",
        presentation_id.value());
    return false;
}

bool ConnectionFromWebContent::handle_mouse_event(Web::Compositor::PresentationId presentation_id, Web::MouseEvent const& event)
{
    for (auto& entry : web_content_connections()) {
        if (auto* context = entry.value->context_for_presentation(presentation_id))
            return entry.value->handle_viewport_scrollbar_mouse_event(*context, event);
    }

    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring mouse_event for unknown presentation {}",
        presentation_id.value());
    return false;
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
    auto skia_backend_context = skia_backend_context_for(context.display_list_player_type);
    auto publication = context.backing_store_manager->allocate_backing_stores(
        *allocation,
        skia_backend_context,
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

RasterizeResult ConnectionFromWebContent::rasterize_present(ContextState& context, Gfx::IntRect viewport_rect, Optional<u64> frame_id, bool clear_pending_update_flags)
{
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote raster begin context {} frame {} viewport={}x{} at {},{} pending_updates={}/{}",
        context.context_id.value(), frame_id.value_or(0), viewport_rect.width(), viewport_rect.height(), viewport_rect.x(), viewport_rect.y(),
        context.has_pending_display_list_update, context.has_pending_scroll_state_update);

    if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI
        && context.presents_to_client
        && context.presented_bitmap_id_awaiting_ack.has_value()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Deferring present for context {} until bitmap {} is ready to paint",
            context.context_id.value(), context.presented_bitmap_id_awaiting_ack.value());
        return RasterizeResult::Deferred;
    }

    auto viewport_size = context.viewport_size.value_or(viewport_rect.size());
    auto allocation = allocate_backing_stores_if_needed(context, viewport_size);
    if (allocation.is_error()) {
        dbgln("Skipping present for compositor context {} from WebContent connection {}: {}", context.context_id.value(), m_connection_id.value(), allocation.error());
        if (frame_id.has_value())
            context.completed_frame_id = *frame_id;
        return RasterizeResult::Completed;
    }

    if (!context.display_list || !context.scroll_state_snapshot.has_value() || !context.backing_store_manager->is_valid()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Skipping present for context {}: display_list={}, scroll_state={}, backing_stores={}",
            context.context_id.value(), !!context.display_list, context.scroll_state_snapshot.has_value(), context.backing_store_manager->is_valid());
        if (frame_id.has_value())
            context.completed_frame_id = *frame_id;
        return RasterizeResult::Completed;
    }

    auto& back_store = context.backing_store_manager->back_store();
    Web::Painting::DisplayListPlayerSkia player { skia_backend_context_for(context.display_list_player_type) };
    player.execute(*context.display_list, context.display_list_resource_storage, *context.scroll_state_snapshot, back_store);
    Web::Compositor::paint_viewport_scrollbars(
        back_store,
        context.viewport_scrollbars,
        *context.scroll_state_snapshot,
        context.hovered_viewport_scrollbar_index,
        context.captured_viewport_scrollbar_index);
    Web::Compositor::flush_surface(back_store);

    auto rendered_bitmap_id = context.backing_store_manager->back_bitmap_id();
    context.backing_store_manager->swap();

    if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI && context.presents_to_client) {
        context.presented_bitmap_id_awaiting_ack = rendered_bitmap_id;
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote raster did_paint context {} presentation {} bitmap {}",
            context.context_id.value(), context.presentation_mode.presentation_id.value(), rendered_bitmap_id);
        Compositor::ConnectionFromClient::did_paint(
            context.presentation_mode.presentation_id,
            viewport_rect,
            rendered_bitmap_id);
    } else if (context.presentation_mode.kind == Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface) {
        publish_context_to_compositor_surface(context);
    }

    if (frame_id.has_value())
        context.completed_frame_id = *frame_id;
    if (clear_pending_update_flags) {
        context.has_pending_display_list_update = false;
        context.has_pending_scroll_state_update = false;
    }
    return RasterizeResult::Completed;
}

void ConnectionFromWebContent::rasterize_pending_present(ContextState& context)
{
    if (!context.pending_present.has_value())
        return;

    auto pending_present = *context.pending_present;
    if (rasterize_present(context, pending_present.viewport_rect, pending_present.frame_id, true) == RasterizeResult::Deferred)
        return;
    context.pending_present.clear();
}

void ConnectionFromWebContent::schedule_async_scroll_present(ContextState& context, Gfx::IntRect viewport_rect)
{
    if (viewport_rect.is_empty())
        return;

    context.pending_async_scroll_present = viewport_rect;
    rasterize_pending_async_scroll_present(context);
}

void ConnectionFromWebContent::rasterize_pending_async_scroll_present(ContextState& context)
{
    if (!context.pending_async_scroll_present.has_value())
        return;

    auto viewport_rect = *context.pending_async_scroll_present;
    if (rasterize_present(context, viewport_rect, {}, false) == RasterizeResult::Deferred)
        return;
    context.pending_async_scroll_present.clear();
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
    rasterize_pending_async_scroll_present(*target_context);
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
        rasterize_pending_async_scroll_present(context);
        rasterize_pending_present(context);
        return true;
    }

    return false;
}

ConnectionFromWebContent::ContextState* ConnectionFromWebContent::context_for_presentation(Web::Compositor::PresentationId presentation_id)
{
    for (auto& entry : m_contexts) {
        auto& context = entry.value;
        if (context.presentation_mode.kind != Web::Compositor::SerializedPresentationModeKind::PresentToUI)
            continue;
        if (context.presentation_mode.presentation_id == presentation_id)
            return &context;
    }

    return nullptr;
}

Optional<Gfx::FloatPoint> ConnectionFromWebContent::reapply_pending_async_scroll_offsets(ContextState& context, ReadonlySpan<Web::Compositor::AsyncScrollOffset> pending_scroll_offsets)
{
    Optional<Gfx::FloatPoint> viewport_scroll_offset;
    for (auto const& pending_scroll_offset : pending_scroll_offsets) {
        auto node_id = context.async_scroll_tree.scroll_node_id_for_stable_id(pending_scroll_offset.stable_node_id);
        if (!node_id.has_value())
            continue;
        auto current_scroll_offset = context.async_scroll_tree.scroll_offset_for_node(*node_id, *context.scroll_state_snapshot);
        if (!current_scroll_offset.has_value())
            continue;
        auto reconciled_scroll_offset = context.async_scroll_tree.set_scroll_offset(*node_id, pending_scroll_offset.compositor_scroll_offset, *context.scroll_state_snapshot);
        if (reconciled_scroll_offset.has_value() && context.async_scroll_tree.scroll_node_is_viewport(*node_id))
            viewport_scroll_offset = *reconciled_scroll_offset;
    }
    return viewport_scroll_offset;
}

Web::Compositor::AsyncScrollOperationID ConnectionFromWebContent::next_async_scroll_operation_id(ContextState& context)
{
    return ++context.next_async_scroll_operation_id;
}

void ConnectionFromWebContent::complete_async_scroll_operation(ContextState& context, Optional<Web::Compositor::AsyncScrollOperationID> operation_id)
{
    if (!operation_id.has_value())
        return;

    context.completed_async_scroll_operation_ids.append(*operation_id);
    async_schedule_rendering_update(context.page_id);
}

void ConnectionFromWebContent::store_pending_async_scroll_offsets(
    ContextState& context,
    ReadonlySpan<Web::Compositor::AsyncScrollOffset> scroll_offsets,
    Optional<Web::Compositor::AsyncScrollOperationID> operation_id)
{
    for (auto const& scroll_offset : scroll_offsets)
        Web::Compositor::set_or_append_pending_scroll_offset(context.pending_async_scroll_offsets, scroll_offset);
    if (operation_id.has_value())
        context.completed_async_scroll_operation_ids.append(*operation_id);
    if (!scroll_offsets.is_empty() || operation_id.has_value())
        async_schedule_rendering_update(context.page_id);
}

void ConnectionFromWebContent::update_async_scrolling_state_from_display_list(ContextState& context)
{
    VERIFY(context.display_list);
    VERIFY(context.scroll_state_snapshot.has_value());

    auto async_scrolling_state = Web::Compositor::async_scrolling_state_from_display_list(*context.display_list);
    auto async_scrolling_viewport_rect = async_scrolling_state.viewport_rect;
    auto const wheel_event_listener_state_generation = async_scrolling_state.wheel_event_listener_state_generation;
    auto wheel_routing_admission = Web::Compositor::wheel_routing_admission_for(async_scrolling_state);
    if (wheel_event_listener_state_generation < context.wheel_event_listener_state_generation)
        wheel_routing_admission = Web::Compositor::WheelRoutingAdmission::StaleWheelEventListeners;
    else
        context.wheel_event_listener_state_generation = wheel_event_listener_state_generation;

    context.wheel_routing_admission = wheel_routing_admission;
    context.can_accept_async_wheel_events = wheel_routing_admission == Web::Compositor::WheelRoutingAdmission::Accepted;
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote compositor wheel routing admission: {} (scroll_nodes={}, sticky_areas={}, blocking_regions={})",
        Web::Compositor::wheel_routing_admission_to_string(wheel_routing_admission),
        async_scrolling_state.scroll_nodes.size(),
        async_scrolling_state.sticky_areas.size(),
        async_scrolling_state.blocking_wheel_event_regions.size());

    auto pending_async_scroll_offsets = context.pending_async_scroll_offsets;
    auto hovered_scrollbar_identity = Web::Compositor::viewport_scrollbar_identity_at(context.viewport_scrollbars, context.hovered_viewport_scrollbar_index);
    auto captured_scrollbar_identity = Web::Compositor::viewport_scrollbar_identity_at(context.viewport_scrollbars, context.captured_viewport_scrollbar_index);
    context.viewport_scrollbars = move(async_scrolling_state.viewport_scrollbars);
    context.hovered_viewport_scrollbar_index = hovered_scrollbar_identity.has_value()
        ? Web::Compositor::find_viewport_scrollbar_index(context.viewport_scrollbars, *hovered_scrollbar_identity)
        : Optional<size_t> {};
    context.captured_viewport_scrollbar_index = captured_scrollbar_identity.has_value()
        ? Web::Compositor::find_viewport_scrollbar_index(context.viewport_scrollbars, *captured_scrollbar_identity)
        : Optional<size_t> {};
    context.async_scroll_tree.set_state(move(async_scrolling_state));
    if (!pending_async_scroll_offsets.is_empty()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Reapplying {} pending async scroll offset(s) to remote display list update",
            pending_async_scroll_offsets.size());
        if (auto viewport_scroll_offset = reapply_pending_async_scroll_offsets(context, pending_async_scroll_offsets); viewport_scroll_offset.has_value())
            async_scrolling_viewport_rect.set_location(viewport_scroll_offset->to_type<int>());
    }
    context.async_scroll_tree.rebuild_wheel_hit_test_targets(context.display_list, *context.scroll_state_snapshot);
    context.async_scrolling_viewport_rect = async_scrolling_viewport_rect;
    context.has_async_scrolling_state = true;
}

Optional<size_t> ConnectionFromWebContent::hit_test_viewport_scrollbar(ContextState const& context, Gfx::FloatPoint position) const
{
    if (!context.scroll_state_snapshot.has_value())
        return {};

    for (size_t i = 0; i < context.viewport_scrollbars.size(); ++i) {
        auto const& scrollbar = context.viewport_scrollbars[i];
        auto scroll_offset = context.async_scroll_tree.scroll_offset_for_node(scrollbar.scroll_node_id, *context.scroll_state_snapshot);
        if (!scroll_offset.has_value())
            continue;

        if (Web::Compositor::scrollbar_hit_rect(scrollbar, *scroll_offset).to_type<float>().contains(position))
            return i;
    }
    return {};
}

void ConnectionFromWebContent::set_hovered_viewport_scrollbar(ContextState& context, Optional<size_t> scrollbar_index)
{
    if (context.hovered_viewport_scrollbar_index == scrollbar_index)
        return;
    context.hovered_viewport_scrollbar_index = scrollbar_index;
    schedule_async_scroll_present(context, context.async_scrolling_viewport_rect);
}

bool ConnectionFromWebContent::apply_viewport_scrollbar_drag(ContextState& context, size_t scrollbar_index, float primary_position, float thumb_grab_position)
{
    if (!context.display_list || !context.scroll_state_snapshot.has_value())
        return false;
    if (scrollbar_index >= context.viewport_scrollbars.size())
        return false;

    auto const& scrollbar = context.viewport_scrollbars[scrollbar_index];
    auto expanded = context.hovered_viewport_scrollbar_index == scrollbar_index || context.captured_viewport_scrollbar_index == scrollbar_index;
    auto scroll_size = Web::Compositor::scrollbar_scroll_size(scrollbar, expanded);
    if (scroll_size == 0)
        return false;

    auto current_scroll_offset = context.async_scroll_tree.scroll_offset_for_node(scrollbar.scroll_node_id, *context.scroll_state_snapshot);
    if (!current_scroll_offset.has_value())
        return false;

    auto orientation = Web::Compositor::orientation_for_scrollbar(scrollbar);
    auto thumb_rect = expanded ? scrollbar.expanded_thumb_rect : scrollbar.thumb_rect;
    auto min_thumb_position = static_cast<float>(thumb_rect.primary_offset_for_orientation(orientation));
    auto max_thumb_position = min_thumb_position + scrollbar.max_scroll_offset * static_cast<float>(scroll_size);
    auto target_thumb_position = AK::clamp(primary_position - thumb_grab_position, min_thumb_position, max_thumb_position);
    auto target_scroll_offset = (target_thumb_position - min_thumb_position) / static_cast<float>(scroll_size);

    Gfx::FloatPoint delta;
    delta.set_primary_offset_for_orientation(orientation, target_scroll_offset - current_scroll_offset->primary_offset_for_orientation(orientation));
    if (delta.x() == 0 && delta.y() == 0)
        return false;

    auto scroll_offsets = context.async_scroll_tree.apply_scroll_delta(scrollbar.scroll_node_id, delta, *context.scroll_state_snapshot);
    if (scroll_offsets.is_empty())
        return false;
    context.async_scroll_tree.rebuild_wheel_hit_test_targets(context.display_list, *context.scroll_state_snapshot);

    auto scroll_offset = Web::Compositor::viewport_scroll_offset_from(scroll_offsets);
    if (!scroll_offset.has_value())
        return false;

    store_pending_async_scroll_offsets(context, scroll_offsets);
    auto async_scroll_viewport_rect = context.async_scrolling_viewport_rect;
    async_scroll_viewport_rect.set_location(scroll_offset->to_type<int>());
    context.async_scrolling_viewport_rect = async_scroll_viewport_rect;
    schedule_async_scroll_present(context, async_scroll_viewport_rect);
    return true;
}

bool ConnectionFromWebContent::handle_viewport_scrollbar_mouse_event(ContextState& context, Web::MouseEvent const& event)
{
    if (!context.has_async_scrolling_state || !context.scroll_state_snapshot.has_value())
        return false;

    auto position = Gfx::FloatPoint {
        static_cast<float>(event.position.x().value()),
        static_cast<float>(event.position.y().value()),
    };
    auto primary_position_for_scrollbar = [&](Web::Compositor::ViewportScrollbar const& scrollbar) {
        return position.primary_offset_for_orientation(Web::Compositor::orientation_for_scrollbar(scrollbar));
    };

    switch (event.type) {
    case Web::MouseEvent::Type::MouseDown: {
        if (event.button != Web::UIEvents::MouseButton::Primary)
            return false;

        Optional<size_t> scrollbar_index;
        float thumb_grab_position = 0;
        float primary_position = 0;
        for (size_t i = 0; i < context.viewport_scrollbars.size(); ++i) {
            auto const& scrollbar = context.viewport_scrollbars[i];
            auto scroll_offset = context.async_scroll_tree.scroll_offset_for_node(scrollbar.scroll_node_id, *context.scroll_state_snapshot);
            if (!scroll_offset.has_value())
                continue;

            auto expanded = context.hovered_viewport_scrollbar_index == i || context.captured_viewport_scrollbar_index == i;
            auto orientation = Web::Compositor::orientation_for_scrollbar(scrollbar);
            auto thumb_rect = Web::Compositor::translated_thumb_rect(scrollbar, *scroll_offset, expanded);
            primary_position = primary_position_for_scrollbar(scrollbar);
            if (thumb_rect.to_type<float>().contains(position)) {
                thumb_grab_position = primary_position - static_cast<float>(thumb_rect.primary_offset_for_orientation(orientation));
                scrollbar_index = i;
                break;
            }
            if (Web::Compositor::scrollbar_hit_rect(scrollbar, *scroll_offset).to_type<float>().contains(position)) {
                auto gutter_rect = Web::Compositor::scrollbar_gutter_rect(scrollbar, true);
                auto thumb_size = static_cast<float>(thumb_rect.primary_size_for_orientation(orientation));
                auto gutter_start = static_cast<float>(gutter_rect.primary_offset_for_orientation(orientation));
                auto gutter_size = static_cast<float>(gutter_rect.primary_size_for_orientation(orientation));
                auto offset_relative_to_gutter = primary_position - gutter_start;
                thumb_grab_position = max(min(offset_relative_to_gutter, thumb_size / 2), offset_relative_to_gutter - gutter_size + thumb_size);
                scrollbar_index = i;
                break;
            }
        }

        if (!scrollbar_index.has_value())
            return false;

        context.captured_viewport_scrollbar_index = *scrollbar_index;
        context.hovered_viewport_scrollbar_index = *scrollbar_index;
        context.viewport_scrollbar_thumb_grab_position = thumb_grab_position;
        schedule_async_scroll_present(context, context.async_scrolling_viewport_rect);
        apply_viewport_scrollbar_drag(context, *scrollbar_index, primary_position, thumb_grab_position);
        return true;
    }
    case Web::MouseEvent::Type::MouseMove: {
        if (context.captured_viewport_scrollbar_index.has_value()) {
            auto scrollbar_index = *context.captured_viewport_scrollbar_index;
            auto thumb_grab_position = context.viewport_scrollbar_thumb_grab_position;
            if (scrollbar_index >= context.viewport_scrollbars.size()) {
                context.captured_viewport_scrollbar_index.clear();
                return false;
            }

            auto primary_position = primary_position_for_scrollbar(context.viewport_scrollbars[scrollbar_index]);
            apply_viewport_scrollbar_drag(context, scrollbar_index, primary_position, thumb_grab_position);
            return true;
        }

        auto hovered_scrollbar_index = hit_test_viewport_scrollbar(context, position);
        set_hovered_viewport_scrollbar(context, hovered_scrollbar_index);
        return hovered_scrollbar_index.has_value();
    }
    case Web::MouseEvent::Type::MouseUp: {
        if (!context.captured_viewport_scrollbar_index.has_value())
            return false;
        auto scrollbar_index = *context.captured_viewport_scrollbar_index;
        auto thumb_grab_position = context.viewport_scrollbar_thumb_grab_position;
        if (scrollbar_index >= context.viewport_scrollbars.size()) {
            context.captured_viewport_scrollbar_index.clear();
            return false;
        }

        auto primary_position = primary_position_for_scrollbar(context.viewport_scrollbars[scrollbar_index]);
        context.captured_viewport_scrollbar_index.clear();
        schedule_async_scroll_present(context, context.async_scrolling_viewport_rect);
        apply_viewport_scrollbar_drag(context, scrollbar_index, primary_position, thumb_grab_position);
        return true;
    }
    case Web::MouseEvent::Type::MouseLeave: {
        auto has_capture = context.captured_viewport_scrollbar_index.has_value();
        set_hovered_viewport_scrollbar(context, {});
        return has_capture;
    }
    case Web::MouseEvent::Type::MouseWheel:
        return false;
    }
    VERIFY_NOT_REACHED();
}

bool ConnectionFromWebContent::apply_async_scroll_to_target(
    ContextState& context,
    Web::Compositor::AsyncScrollNodeID scroll_target,
    Gfx::FloatPoint delta_in_device_pixels,
    Gfx::IntRect viewport_rect,
    Optional<Web::Compositor::AsyncScrollOperationID> operation_id)
{
    auto async_scroll_viewport_rect = viewport_rect;
    auto scroll_offsets = context.async_scroll_tree.apply_scroll_delta(scroll_target, delta_in_device_pixels, *context.scroll_state_snapshot);
    if (scroll_offsets.is_empty()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Dropping remote async scroll: scroll tree consumed no delta");
        complete_async_scroll_operation(context, operation_id);
        return false;
    }

    context.async_scroll_tree.rebuild_wheel_hit_test_targets(context.display_list, *context.scroll_state_snapshot);
    if (auto viewport_scroll_offset = Web::Compositor::viewport_scroll_offset_from(scroll_offsets); viewport_scroll_offset.has_value())
        async_scroll_viewport_rect.set_location(viewport_scroll_offset->to_type<int>());
    store_pending_async_scroll_offsets(context, scroll_offsets, operation_id);
    context.async_scrolling_viewport_rect = async_scroll_viewport_rect;
    schedule_async_scroll_present(context, async_scroll_viewport_rect);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Stored {} remote pending async scroll offset(s)",
        scroll_offsets.size());
    return true;
}

void ConnectionFromWebContent::defer_async_scroll_to_target(
    ContextState& context,
    Web::Compositor::AsyncScrollNodeID scroll_target,
    Gfx::FloatPoint delta_in_device_pixels,
    Gfx::IntRect viewport_rect,
    Optional<Web::Compositor::AsyncScrollOperationID> operation_id)
{
    // Mirror the threaded compositor path: acknowledge the sync IPC first, then
    // apply the accepted async scroll and publish any resulting paint/update.
    Core::deferred_invoke([connection_id = m_connection_id, context_id = context.context_id, scroll_target, delta_in_device_pixels, viewport_rect, operation_id] {
        auto connection = web_content_connections().get(connection_id);
        if (!connection.has_value())
            return;

        auto* connection_ptr = *connection;
        auto context = connection_ptr->m_contexts.get(context_id);
        if (!context.has_value())
            return;

        connection_ptr->apply_async_scroll_to_target(*context, scroll_target, delta_in_device_pixels, viewport_rect, operation_id);
    });
}

Messages::WebContentCompositorServer::EnqueueAsyncScrollByResponse ConnectionFromWebContent::enqueue_async_scroll_by(
    ContextState& context,
    Web::UniqueNodeID expected_document_id,
    Gfx::FloatPoint position,
    Gfx::FloatPoint delta_in_device_pixels,
    Gfx::IntRect viewport_rect,
    bool track_operation)
{
    if (!context.can_accept_async_wheel_events) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll enqueue: compositor cannot accept async wheel events ({})",
            Web::Compositor::wheel_routing_admission_to_string(context.wheel_routing_admission));
        return { false, {} };
    }
    if (!context.display_list || !context.scroll_state_snapshot.has_value())
        return { false, {} };

    auto scroll_target = context.async_scroll_tree.hit_test_scroll_node_for_wheel(position, delta_in_device_pixels);
    if (scroll_target.blocked_by_main_thread_region) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll enqueue: main-thread wheel region at {},{} device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return { false, {} };
    }
    if (scroll_target.blocked_by_wheel_event_region) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll enqueue: blocking wheel event region at {},{} device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return { false, {} };
    }
    if (!scroll_target.node_id.has_value()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll enqueue: no wheel target at {},{} for device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return { false, {} };
    }
    if (scroll_target.node_id->document_id != expected_document_id) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll enqueue: stale wheel target at {},{} for current document",
            position.x(), position.y());
        return { false, {} };
    }

    Optional<Web::Compositor::AsyncScrollOperationID> operation_id;
    if (track_operation)
        operation_id = next_async_scroll_operation_id(context);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote compositor accepted main-thread async scroll enqueue at {},{} device delta {},{} for scroll node {} viewport={}x{} at {},{}",
        position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y(), scroll_target.node_id->scroll_frame_index.value(), viewport_rect.width(), viewport_rect.height(), viewport_rect.x(), viewport_rect.y());
    defer_async_scroll_to_target(context, *scroll_target.node_id, delta_in_device_pixels, viewport_rect, operation_id);
    return { true, operation_id };
}

bool ConnectionFromWebContent::async_scroll_by(ContextState& context, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels)
{
    if (!context.can_accept_async_wheel_events) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll: compositor cannot accept async wheel events ({})",
            Web::Compositor::wheel_routing_admission_to_string(context.wheel_routing_admission));
        return false;
    }
    if (!context.display_list || !context.scroll_state_snapshot.has_value())
        return false;

    auto scroll_target = context.async_scroll_tree.hit_test_scroll_node_for_wheel(position, delta_in_device_pixels);
    if (scroll_target.blocked_by_main_thread_region) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll: main-thread wheel region at {},{} device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return false;
    }
    if (scroll_target.blocked_by_wheel_event_region) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll: blocking wheel event region at {},{} device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return false;
    }
    if (!scroll_target.node_id.has_value()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Rejecting remote async scroll: no wheel target at {},{} for device delta {},{}",
            position.x(), position.y(), delta_in_device_pixels.x(), delta_in_device_pixels.y());
        return false;
    }

    defer_async_scroll_to_target(context, *scroll_target.node_id, delta_in_device_pixels, context.async_scrolling_viewport_rect);
    return true;
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
                                   .async_scroll_tree = {},
                                   .viewport_scrollbars = {},
                                   .hovered_viewport_scrollbar_index = {},
                                   .captured_viewport_scrollbar_index = {},
                                   .viewport_scrollbar_thumb_grab_position = 0,
                                   .pending_async_scroll_offsets = {},
                                   .completed_async_scroll_operation_ids = {},
                                   .next_async_scroll_operation_id = 0,
                                   .async_scrolling_viewport_rect = {},
                                   .has_async_scrolling_state = false,
                                   .can_accept_async_wheel_events = false,
                                   .wheel_event_listener_state_generation = 0,
                                   .wheel_routing_admission = Web::Compositor::WheelRoutingAdmission::NoAsyncScrollingState,
                                   .video_frame_sequence_ids = {},
                                   .pending_video_frame_updates = {},
                                   .has_pending_display_list_update = false,
                                   .has_pending_scroll_state_update = false,
                                   .pending_present = {},
                                   .pending_async_scroll_present = {},
                                   .backing_store_manager = make<Web::Compositor::BackingStoreManager>(),
                                   .presented_bitmap_id_awaiting_ack = {},
                                   .submitted_frame_id = 0,
                                   .completed_frame_id = 0,
                                   .is_top_level_traversable = false,
                                   .window_resize_in_progress = Web::Compositor::WindowResizingInProgress::No,
                                   .presents_to_client = presentation_mode->kind == Web::Compositor::SerializedPresentationModeKind::PresentToUI,
                               });
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Accepted remote context {} from WebContent connection {} page {} mode {}",
        context_id.value(), m_connection_id.value(), page_id, to_underlying(presentation_mode->kind));
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

ConnectionFromWebContent::VideoFrameUpdateResult ConnectionFromWebContent::apply_video_frame_update(
    ContextState& context,
    Web::Compositor::SerializedVideoFrameUpdate const& serialized_video_frame_update)
{
    auto video_frame_source_id = serialized_video_frame_update.video_frame_source_id;
    if (!context.display_list_resource_storage.contains_video_frame_source(video_frame_source_id))
        return VideoFrameUpdateResult::MissingSource;

    auto last_frame_sequence_id = context.video_frame_sequence_ids.get(video_frame_source_id).value_or(0);
    if (serialized_video_frame_update.frame_sequence_id <= last_frame_sequence_id) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring stale video frame update {} for source {} in context {}",
            serialized_video_frame_update.frame_sequence_id, video_frame_source_id.value(), context.context_id.value());
        return VideoFrameUpdateResult::Consumed;
    }

    auto video_frame = Web::Compositor::deserialize_video_frame_update(serialized_video_frame_update);
    if (video_frame.is_error()) {
        dbgln("Rejecting video frame update for source {} in compositor context {} from WebContent connection {}: {}",
            video_frame_source_id.value(), context.context_id.value(), m_connection_id.value(), video_frame.error());
        return VideoFrameUpdateResult::Consumed;
    }

    context.video_frame_sequence_ids.set(video_frame_source_id, serialized_video_frame_update.frame_sequence_id);
    context.display_list_resource_storage.video_frame_source(video_frame_source_id).update(video_frame.release_value());
    return VideoFrameUpdateResult::Applied;
}

void ConnectionFromWebContent::apply_pending_video_frame_updates(ContextState& context)
{
    Vector<Web::Painting::VideoFrameResourceId> source_ids_to_apply;
    for (auto const& pending_update : context.pending_video_frame_updates) {
        if (context.display_list_resource_storage.contains_video_frame_source(pending_update.key))
            source_ids_to_apply.append(pending_update.key);
    }

    for (auto video_frame_source_id : source_ids_to_apply) {
        auto pending_video_frame_update = context.pending_video_frame_updates.take(video_frame_source_id);
        VERIFY(pending_video_frame_update.has_value());
        auto serialized_video_frame_update = pending_video_frame_update.release_value();
        auto frame_sequence_id = serialized_video_frame_update.frame_sequence_id;
        auto result = apply_video_frame_update(context, serialized_video_frame_update);
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Applied queued video frame update {} for source {} in context {} with result {}",
            frame_sequence_id, video_frame_source_id.value(), context.context_id.value(), to_underlying(result));
    }
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

    for (auto video_frame_source_id : resource_transaction.video_frame_source_ids_to_remove) {
        context->pending_video_frame_updates.remove(video_frame_source_id);
        context->video_frame_sequence_ids.remove(video_frame_source_id);
    }
    context->display_list_resource_storage.apply_transaction(move(resource_transaction));
    apply_pending_video_frame_updates(*context);

    context->display_list = move(display_list);
    context->scroll_state_snapshot = move(scroll_state);
    update_async_scrolling_state_from_display_list(*context);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Accepted remote display list for context {} from WebContent connection {}",
        context_id.value(), m_connection_id.value());
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
    if (context->has_async_scrolling_state && context->display_list) {
        Optional<Gfx::FloatPoint> reconciled_viewport_scroll_offset;
        reconciled_viewport_scroll_offset = reapply_pending_async_scroll_offsets(*context, context->pending_async_scroll_offsets);
        context->async_scroll_tree.rebuild_wheel_hit_test_targets(context->display_list, *context->scroll_state_snapshot);
        if (reconciled_viewport_scroll_offset.has_value()) {
            auto reconciled_viewport_rect = context->async_scrolling_viewport_rect;
            reconciled_viewport_rect.set_location(reconciled_viewport_scroll_offset->to_type<int>());
            context->async_scrolling_viewport_rect = reconciled_viewport_rect;
        }
    }
    context->has_pending_scroll_state_update = true;
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::invalidate_wheel_event_listener_state(u64 raw_context_id, u64 generation)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring wheel listener invalidation for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->wheel_event_listener_state_generation = max(context->wheel_event_listener_state_generation, generation);
    context->wheel_routing_admission = Web::Compositor::WheelRoutingAdmission::StaleWheelEventListeners;
    context->can_accept_async_wheel_events = false;
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Invalidated remote compositor wheel listener state for context {} (generation={})",
        context_id.value(), generation);
}

Messages::WebContentCompositorServer::EnqueueAsyncScrollByResponse ConnectionFromWebContent::enqueue_async_scroll_by(
    u64 raw_context_id,
    Web::UniqueNodeID expected_document_id,
    Gfx::FloatPoint position,
    Gfx::FloatPoint delta_in_device_pixels,
    Gfx::IntRect viewport_rect,
    bool track_operation)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring async scroll enqueue for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return { false, {} };
    }

    return enqueue_async_scroll_by(*context, expected_document_id, position, delta_in_device_pixels, viewport_rect, track_operation);
}

Messages::WebContentCompositorServer::ShouldDeferAsyncScrollOffsetAdoptionResponse ConnectionFromWebContent::should_defer_async_scroll_offset_adoption(u64 raw_context_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value())
        return false;

    // The remote compositor currently rasterizes synchronously on its IPC event
    // loop, so WebContent never races a partially applied async scroll update.
    return false;
}

Messages::WebContentCompositorServer::ShouldDeferMainThreadPresentForAsyncScrollResponse ConnectionFromWebContent::should_defer_main_thread_present_for_async_scroll(u64 raw_context_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value())
        return false;

    auto should_defer = !context->pending_async_scroll_offsets.is_empty()
        && (context->pending_async_scroll_present.has_value() || context->presented_bitmap_id_awaiting_ack.has_value());
    return should_defer;
}

Messages::WebContentCompositorServer::TakePendingAsyncScrollUpdatesResponse ConnectionFromWebContent::take_pending_async_scroll_updates(u64 raw_context_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value())
        return { {}, {} };

    Vector<Web::Compositor::AsyncScrollOffset> scroll_offsets;
    Vector<Web::Compositor::AsyncScrollOperationID> completed_operation_ids;
    AK::swap(scroll_offsets, context->pending_async_scroll_offsets);
    AK::swap(completed_operation_ids, context->completed_async_scroll_operation_ids);
    return { move(scroll_offsets), move(completed_operation_ids) };
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

void ConnectionFromWebContent::update_yuv_video_frame(
    u64 raw_context_id,
    Web::Compositor::SerializedVideoFrameUpdate serialized_video_frame_update)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring video frame update for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto video_frame_source_id = serialized_video_frame_update.video_frame_source_id;
    if (video_frame_source_id.value() == 0) {
        dbgln("Ignoring video frame update with empty source ID for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto result = apply_video_frame_update(*context, serialized_video_frame_update);
    if (result == VideoFrameUpdateResult::MissingSource) {
        auto existing_update = context->pending_video_frame_updates.get(video_frame_source_id);
        if (!existing_update.has_value() || serialized_video_frame_update.frame_sequence_id > existing_update->frame_sequence_id) {
            dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Queuing video frame update {} for missing source {} in context {}",
                serialized_video_frame_update.frame_sequence_id, video_frame_source_id.value(), context_id.value());
            context->pending_video_frame_updates.set(video_frame_source_id, move(serialized_video_frame_update));
        } else {
            dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Ignoring stale pending video frame update {} for source {} in context {}",
                serialized_video_frame_update.frame_sequence_id, video_frame_source_id.value(), context_id.value());
        }
        return;
    }

    if (result == VideoFrameUpdateResult::Applied)
        rasterize_pending_present(*context);
}

void ConnectionFromWebContent::clear_video_frame(u64 raw_context_id, u64 raw_video_frame_source_id)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    if (!context.has_value()) {
        dbgln("Ignoring video frame clear for missing compositor context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    auto video_frame_source_id = Web::Painting::VideoFrameResourceId { raw_video_frame_source_id };
    if (video_frame_source_id.value() == 0) {
        dbgln("Ignoring video frame clear with empty source ID for context {} from WebContent connection {}", context_id.value(), m_connection_id.value());
        return;
    }

    context->pending_video_frame_updates.remove(video_frame_source_id);
    if (!context->display_list_resource_storage.contains_video_frame_source(video_frame_source_id)) {
        dbgln("Ignoring video frame clear for missing source {} in compositor context {} from WebContent connection {}",
            video_frame_source_id.value(), context_id.value(), m_connection_id.value());
        return;
    }

    context->display_list_resource_storage.video_frame_source(video_frame_source_id).clear();
    rasterize_pending_present(*context);
}

void ConnectionFromWebContent::request_screenshot(u64 raw_context_id, u64 request_id, Gfx::IntSize size)
{
    auto context_id = Web::Compositor::CompositorContextId { raw_context_id };
    auto context = m_contexts.get(context_id);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote screenshot request {} for context {} size={}x{}",
        request_id, context_id.value(), size.width(), size.height());
    if (!context.has_value()) {
        dbgln("Failing screenshot {} for missing compositor context {} from WebContent connection {}", request_id, context_id.value(), m_connection_id.value());
        async_did_fail_screenshot(request_id);
        return;
    }

    if (size.is_empty()) {
        dbgln("Failing screenshot {} for invalid size {}x{} from WebContent connection {}", request_id, size.width(), size.height(), m_connection_id.value());
        async_did_fail_screenshot(request_id);
        return;
    }

    if (!context->display_list || !context->scroll_state_snapshot.has_value()) {
        dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Failing screenshot {} for context {}: display_list={}, scroll_state={}",
            request_id, context_id.value(), !!context->display_list, context->scroll_state_snapshot.has_value());
        async_did_fail_screenshot(request_id);
        return;
    }

    auto surface = Gfx::PaintingSurface::create_with_size(size, Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied);
    Web::Painting::DisplayListPlayerSkia player { skia_backend_context_for(context->display_list_player_type) };
    player.execute(*context->display_list, context->display_list_resource_storage, *context->scroll_state_snapshot, surface);
    Web::Compositor::paint_viewport_scrollbars(
        surface,
        context->viewport_scrollbars,
        *context->scroll_state_snapshot,
        context->hovered_viewport_scrollbar_index,
        context->captured_viewport_scrollbar_index);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote screenshot request {} finished for context {}",
        request_id, context_id.value());
    async_did_finish_screenshot(request_id, surface->snapshot_into_shared_image());
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
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] Remote present request for context {} frame {} viewport={}x{} at {},{}",
        context_id.value(), frame_id, viewport_rect.width(), viewport_rect.height(), viewport_rect.x(), viewport_rect.y());
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
