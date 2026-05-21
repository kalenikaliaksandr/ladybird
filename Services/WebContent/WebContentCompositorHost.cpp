/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/HashMap.h>
#include <AK/NeverDestroyed.h>
#include <LibGfx/PaintingSurface.h>
#include <LibMedia/VideoFrame.h>
#include <WebContent/CompositorConnection.h>
#include <WebContent/WebContentCompositorHost.h>

namespace WebContent {

static RefPtr<CompositorConnection>& compositor_process_connection()
{
    static NeverDestroyed<RefPtr<CompositorConnection>> connection;
    return *connection;
}

static HashMap<u64, Web::Compositor::CompositorContextId>& page_compositor_context_ids()
{
    static NeverDestroyed<HashMap<u64, Web::Compositor::CompositorContextId>> context_ids;
    return *context_ids;
}

static Function<void(Web::Compositor::CompositorContextId)>& compositor_context_destruction_handler()
{
    static NeverDestroyed<Function<void(Web::Compositor::CompositorContextId)>> handler;
    return *handler;
}

static CompositorConnection& compositor_process_connection_or_die()
{
    auto connection = compositor_process_connection();
    VERIFY(connection);
    VERIFY(connection->is_open());
    return *connection;
}

void set_compositor_process_connection(RefPtr<CompositorConnection> connection)
{
    compositor_process_connection() = move(connection);
}

void set_compositor_context_destruction_handler(Function<void(Web::Compositor::CompositorContextId)> handler)
{
    compositor_context_destruction_handler() = move(handler);
}

bool request_compositor_process_cursor_change(u64 page_id, Gfx::Cursor const& cursor)
{
    auto connection = compositor_process_connection();
    if (!connection || !connection->is_open())
        return false;

    auto context_id = page_compositor_context_ids().get(page_id);
    if (!context_id.has_value())
        return false;

    connection->did_request_cursor_change(*context_id, cursor);
    return true;
}

class WebContentCompositorHost final : public Web::Compositor::CompositorHost {
public:
    virtual void start(Web::DisplayListPlayerType) override
    {
    }

private:
    virtual void register_context(Web::Compositor::CompositorContextId context_id, Optional<u64> page_id, Web::Compositor::PagePresentationRegistration page_presentation_registration) override
    {
        if (page_presentation_registration != Web::Compositor::PagePresentationRegistration::Yes)
            return;

        VERIFY(page_id.has_value());
        page_compositor_context_ids().set(*page_id, context_id);
    }

    virtual void destroy_context(Web::Compositor::CompositorContextId context_id) override
    {
        page_compositor_context_ids().remove_all_matching([&](auto const&, auto value) {
            return value == context_id;
        });

        VERIFY(compositor_context_destruction_handler());
        compositor_context_destruction_handler()(context_id);
    }

    virtual void stop_presenting_to_client(Web::Compositor::CompositorContextId context_id) override
    {
        compositor_process_connection_or_die().stop_presenting_to_client(context_id);
    }

    virtual void set_presentation_mode(Web::Compositor::CompositorContextId context_id, Web::Compositor::PresentationMode mode) override
    {
        compositor_process_connection_or_die().set_presentation_mode(context_id, mode);
    }

    virtual void update_display_list(Web::Compositor::CompositorContextId context_id, NonnullRefPtr<Web::Painting::DisplayList> display_list, Web::Painting::DisplayListResourceTransaction&& resource_transaction, Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot) override
    {
        compositor_process_connection_or_die().update_display_list(context_id, display_list, resource_transaction, scroll_state_snapshot);
    }

    virtual void update_video_frame(Web::Compositor::CompositorContextId context_id, Web::Painting::VideoFrameResourceId frame_id, NonnullRefPtr<Media::VideoFrame const> frame) override
    {
        compositor_process_connection_or_die().update_video_frame(context_id, frame_id, frame);
    }

    virtual void clear_video_frame(Web::Compositor::CompositorContextId context_id, Web::Painting::VideoFrameResourceId frame_id) override
    {
        compositor_process_connection_or_die().clear_video_frame(context_id, frame_id);
    }

    virtual void update_compositor_surface(Web::Compositor::CompositorContextId context_id, Web::Painting::CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image) override
    {
        compositor_process_connection_or_die().update_compositor_surface(context_id, surface_id, shared_image);
    }

    virtual void clear_compositor_surface(Web::Compositor::CompositorContextId context_id, Web::Painting::CompositorSurfaceId surface_id) override
    {
        compositor_process_connection_or_die().clear_compositor_surface(context_id, surface_id);
    }

    virtual void update_scroll_state(Web::Compositor::CompositorContextId context_id, Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot) override
    {
        compositor_process_connection_or_die().update_scroll_state(context_id, scroll_state_snapshot);
    }

    virtual void invalidate_wheel_event_listener_state(Web::Compositor::CompositorContextId context_id, u64 generation) override
    {
        compositor_process_connection_or_die().invalidate_wheel_event_listener_state(context_id, generation);
    }

    virtual Web::Compositor::AsyncScrollEnqueueResult async_scroll_by(Web::Compositor::CompositorContextId context_id, Web::UniqueNodeID expected_document_id, Gfx::FloatPoint position,
        Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, Web::Compositor::AsyncScrollOperationTracking operation_tracking) override
    {
        return compositor_process_connection_or_die().async_scroll_by(context_id, expected_document_id, position, delta_in_device_pixels, viewport_rect, operation_tracking);
    }

    virtual bool should_defer_main_thread_present_for_async_scroll(Web::Compositor::CompositorContextId context_id) const override
    {
        return compositor_process_connection_or_die().should_defer_main_thread_present_for_async_scroll(context_id);
    }

    virtual Web::Compositor::PendingAsyncScrollUpdates take_pending_async_scroll_updates(Web::Compositor::CompositorContextId context_id) override
    {
        return compositor_process_connection_or_die().take_pending_async_scroll_updates(context_id);
    }

    virtual void viewport_size_updated(Web::Compositor::CompositorContextId context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress window_resize_in_progress) override
    {
        compositor_process_connection_or_die().viewport_size_updated(context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
    }

    virtual void present_frame(Web::Compositor::CompositorContextId context_id, Gfx::IntRect viewport_rect) override
    {
        compositor_process_connection_or_die().present_frame(context_id, viewport_rect);
    }

    virtual void request_screenshot(Web::Compositor::CompositorContextId context_id, NonnullRefPtr<Gfx::PaintingSurface> target_surface, Function<void()>&& callback) override
    {
        compositor_process_connection_or_die().request_screenshot(context_id, move(target_surface), move(callback));
    }
};

NonnullOwnPtr<Web::Compositor::CompositorHost> create_web_content_compositor_host()
{
    return make<WebContentCompositorHost>();
}

}
