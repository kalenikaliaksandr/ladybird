/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <WebContent/ConnectionToCompositor.h>

namespace WebContent {

ConnectionToCompositor::ConnectionToCompositor(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionToServer<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint>(*this, move(transport))
{
}

void ConnectionToCompositor::die()
{
}

void ConnectionToCompositor::create_context(
    Web::Compositor::CompositorContextId context_id,
    u64 page_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::PresentationCapability presentation_capability,
    Web::Compositor::CompositorContextId target_context_id,
    Web::Painting::CompositorSurfaceId compositor_surface_id,
    Web::DisplayListPlayerType display_list_player_type)
{
    async_create_context(
        context_id.value(),
        page_id,
        presentation_mode_kind,
        presentation_id.value(),
        presentation_capability.value(),
        target_context_id.value(),
        compositor_surface_id.value(),
        display_list_player_type);
}

void ConnectionToCompositor::destroy_context(Web::Compositor::CompositorContextId context_id)
{
    async_destroy_context(context_id.value());
}

void ConnectionToCompositor::set_presentation_mode(
    Web::Compositor::CompositorContextId context_id,
    Web::Compositor::SerializedPresentationModeKind presentation_mode_kind,
    Web::Compositor::PresentationId presentation_id,
    Web::Compositor::PresentationCapability presentation_capability,
    Web::Compositor::CompositorContextId target_context_id,
    Web::Painting::CompositorSurfaceId compositor_surface_id)
{
    async_set_presentation_mode(
        context_id.value(),
        presentation_mode_kind,
        presentation_id.value(),
        presentation_capability.value(),
        target_context_id.value(),
        compositor_surface_id.value());
}

void ConnectionToCompositor::stop_presenting_to_client(Web::Compositor::CompositorContextId context_id)
{
    async_stop_presenting_to_client(context_id.value());
}

void ConnectionToCompositor::viewport_size_updated(
    Web::Compositor::CompositorContextId context_id,
    Gfx::IntSize viewport_size,
    bool is_top_level_traversable,
    Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    async_viewport_size_updated(context_id.value(), viewport_size, is_top_level_traversable, window_resize_in_progress);
}

void ConnectionToCompositor::schedule_rendering_update(u64)
{
}

}
