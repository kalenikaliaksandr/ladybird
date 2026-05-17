/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullRefPtr.h>
#include <LibWeb/Compositor/PageCompositor.h>
#include <WebContent/ConnectionToCompositor.h>

namespace WebContent {

class RemotePageCompositor final : public Web::Compositor::PageCompositor {
public:
    RemotePageCompositor(ConnectionToCompositor&, u64 page_id, PresentationMode);
    virtual ~RemotePageCompositor() override;

    virtual Web::Compositor::CompositorContextId context_id() const override { return m_context_id; }
    virtual void start(Web::DisplayListPlayerType) override;
    virtual void stop_presenting_to_client() override;
    virtual void set_presentation_mode(PresentationMode) override;

    virtual void update_display_list(NonnullRefPtr<Web::Painting::DisplayList>, Web::Painting::DisplayListResourceTransaction&&, Web::Painting::ScrollStateSnapshot&&) override;
    virtual void update_compositor_surface(Web::Painting::CompositorSurfaceId, Gfx::SharedImage&&) override;
    virtual void clear_compositor_surface(Web::Painting::CompositorSurfaceId) override;
    virtual void update_scroll_state(Web::Painting::ScrollStateSnapshot&&) override;
    virtual void invalidate_wheel_event_listener_state(u64 generation) override;
    virtual AsyncScrollEnqueueResult async_scroll_by(Web::UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking) override;
    virtual bool should_defer_async_scroll_offset_adoption() const override;
    virtual bool should_defer_main_thread_present_for_async_scroll() const override;
    virtual PendingAsyncScrollUpdates take_pending_async_scroll_updates() override;
    virtual void viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress) override;
    virtual u64 present_frame(Gfx::IntRect) override;
    virtual void wait_for_frame(u64 frame_id) override;
    virtual void request_screenshot(NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback) override;

private:
    struct SerializedPresentationMode {
        Web::Compositor::SerializedPresentationModeKind kind;
        Web::Compositor::PresentationId presentation_id;
        Web::Compositor::PresentationCapability presentation_capability;
        Web::Compositor::CompositorContextId target_context_id;
        Web::Painting::CompositorSurfaceId compositor_surface_id;
    };

    SerializedPresentationMode serialized_presentation_mode() const;
    void send_create_context();

    NonnullRefPtr<ConnectionToCompositor> m_connection;
    u64 m_page_id { 0 };
    Web::Compositor::CompositorContextId m_context_id;
    PresentationMode m_presentation_mode;
    Web::DisplayListPlayerType m_display_list_player_type { Web::DisplayListPlayerType::SkiaCPU };
    bool m_started { false };
    u64 m_next_frame_id { 1 };
};

}
