/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/NonnullOwnPtr.h>
#include <LibGfx/CanvasCommandList.h>
#include <LibGfx/PaintingSurface.h>
#include <LibMedia/VideoFrame.h>
#include <LibWeb/Compositor/CompositorHost.h>
#include <LibWeb/Compositor/RemoteCanvasTransport.h>
#include <WebContent/CompositorConnection.h>
#include <WebContent/ConnectionFromClient.h>
#include <WebContent/WebContentCompositorHost.h>

namespace WebContent {

// Holds a strong reference to the connection it was created against, so a 2D canvas
// context keeps a stable channel even if the process later reconnects to a new
// compositor (the context's backing storage is lost in that case, exactly like a
// compositor crash).
class WebContentRemoteCanvasTransport final : public Web::Compositor::RemoteCanvasTransport {
public:
    explicit WebContentRemoteCanvasTransport(NonnullRefPtr<CompositorConnection> connection)
        : m_connection(move(connection))
    {
    }

private:
    virtual bool create_context(Web::Painting::CanvasContextId canvas_context_id, Gfx::IntSize size, bool alpha) override
    {
        Vector<String> ignored_supported_extensions;
        return m_connection->create_canvas_context(canvas_context_id, { .type = Web::Compositor::CanvasContextType::Context2D, .size = size, .alpha = alpha }, ignored_supported_extensions);
    }

    virtual void destroy_context(Web::Painting::CanvasContextId canvas_context_id) override
    {
        m_connection->destroy_canvas_context(canvas_context_id);
    }

    virtual void update_commands(Web::Painting::CanvasContextId canvas_context_id, Gfx::CanvasCommandList const& commands) override
    {
        m_connection->update_canvas_commands(canvas_context_id, commands);
    }

    virtual void prepare_canvas_surface(Web::Painting::CanvasContextId canvas_context_id, Web::Compositor::CompositorContextId target_context_id, Web::Painting::CanvasId canvas_id) override
    {
        m_connection->prepare_canvas_surface(canvas_context_id, target_context_id, canvas_id, /* preserve_drawing_buffer= */ true);
    }

    virtual RefPtr<Gfx::Bitmap> read_back_pixels(Web::Painting::CanvasContextId canvas_context_id, Gfx::IntRect const& rect) override
    {
        auto shareable_bitmap = m_connection->get_canvas_pixels(canvas_context_id, rect);
        if (!shareable_bitmap.is_valid())
            return nullptr;
        return shareable_bitmap.bitmap();
    }

    NonnullRefPtr<CompositorConnection> m_connection;
};

class WebContentCompositorHost final : public Web::Compositor::CompositorHost {
public:
    explicit WebContentCompositorHost(ConnectionFromClient& client)
        : m_client(client)
    {
    }

private:
    virtual RefPtr<Web::Compositor::RemoteCanvasTransport> create_canvas_transport() override
    {
        if (auto* connection = compositor_connection())
            return adopt_ref(*new WebContentRemoteCanvasTransport(*connection));
        return nullptr;
    }

    virtual void destroy_context(Web::Compositor::CompositorContextId context_id) override
    {
        if (auto* connection = compositor_connection())
            connection->destroy_context(context_id);
        m_client.did_destroy_compositor_context(context_id);
    }

    virtual void set_presentation_mode(Web::Compositor::CompositorContextId context_id, Web::Compositor::PresentationMode mode) override
    {
        if (auto* connection = compositor_connection())
            connection->set_presentation_mode(context_id, mode);
    }

    virtual void update_display_list(Web::Compositor::CompositorContextId context_id, NonnullRefPtr<Web::Painting::DisplayList> display_list, Web::Painting::AccumulatedVisualContextTree visual_context_tree, Web::Painting::DisplayListResourceTransaction&& resource_transaction, Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot) override
    {
        if (auto* connection = compositor_connection())
            connection->update_display_list(context_id, display_list, visual_context_tree, resource_transaction, scroll_state_snapshot);
    }

    virtual void update_visual_context_tree(Web::Compositor::CompositorContextId context_id, Web::Painting::AccumulatedVisualContextTree visual_context_tree) override
    {
        if (auto* connection = compositor_connection())
            connection->update_visual_context_tree(context_id, visual_context_tree);
    }

    virtual void update_video_frame(Web::Compositor::CompositorContextId context_id, Web::Painting::VideoFrameResourceId frame_id, NonnullRefPtr<Media::VideoFrame const> frame) override
    {
        if (auto* connection = compositor_connection())
            connection->update_video_frame(context_id, frame_id, frame);
    }

    virtual void clear_video_frame(Web::Compositor::CompositorContextId context_id, Web::Painting::VideoFrameResourceId frame_id) override
    {
        if (auto* connection = compositor_connection())
            connection->clear_video_frame(context_id, frame_id);
    }

    virtual void update_compositor_surface(Web::Compositor::CompositorContextId context_id, Web::Painting::CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image) override
    {
        if (auto* connection = compositor_connection())
            connection->update_compositor_surface(context_id, surface_id, shared_image);
    }

    virtual void clear_compositor_surface(Web::Compositor::CompositorContextId context_id, Web::Painting::CompositorSurfaceId surface_id) override
    {
        if (auto* connection = compositor_connection())
            connection->clear_compositor_surface(context_id, surface_id);
    }

    virtual void update_scroll_state(Web::Compositor::CompositorContextId context_id, Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot) override
    {
        if (auto* connection = compositor_connection())
            connection->update_scroll_state(context_id, scroll_state_snapshot);
    }

    virtual void invalidate_wheel_event_listener_state(Web::Compositor::CompositorContextId context_id, u64 generation) override
    {
        if (auto* connection = compositor_connection())
            connection->invalidate_wheel_event_listener_state(context_id, generation);
    }

    virtual Web::Compositor::AsyncScrollEnqueueResult async_scroll_by(Web::Compositor::CompositorContextId context_id, Web::UniqueNodeID expected_document_id, Gfx::FloatPoint position,
        Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, Web::Compositor::AsyncScrollOperationTracking operation_tracking) override
    {
        if (auto* connection = compositor_connection())
            return connection->async_scroll_by(context_id, expected_document_id, position, delta_in_device_pixels, viewport_rect, operation_tracking);
        return {};
    }

    virtual bool should_defer_main_thread_present_for_async_scroll(Web::Compositor::CompositorContextId context_id) const override
    {
        if (auto* connection = compositor_connection())
            return connection->should_defer_main_thread_present_for_async_scroll(context_id);
        return false;
    }

    virtual Web::Compositor::PendingAsyncScrollUpdates take_pending_async_scroll_updates(Web::Compositor::CompositorContextId context_id) override
    {
        if (auto* connection = compositor_connection())
            return connection->take_pending_async_scroll_updates(context_id);
        return {};
    }

    virtual void viewport_size_updated(Web::Compositor::CompositorContextId context_id, Gfx::IntSize viewport_size, Web::Compositor::WindowResizingInProgress window_resize_in_progress) override
    {
        if (auto* connection = compositor_connection())
            connection->viewport_size_updated(context_id, viewport_size, window_resize_in_progress);
    }

    virtual void present_frame(Web::Compositor::CompositorContextId context_id, Gfx::IntRect viewport_rect) override
    {
        if (auto* connection = compositor_connection())
            connection->present_frame(context_id, viewport_rect);
    }

    virtual void request_screenshot(Web::Compositor::CompositorContextId context_id, NonnullRefPtr<Gfx::PaintingSurface> target_surface, Function<void()>&& callback) override
    {
        if (auto* connection = compositor_connection()) {
            connection->request_screenshot(context_id, move(target_surface), move(callback));
            return;
        }
        if (callback)
            callback();
    }

    CompositorConnection* compositor_connection() const
    {
        return m_client.compositor_process_connection();
    }

    ConnectionFromClient& m_client;
};

NonnullOwnPtr<Web::Compositor::CompositorHost> create_web_content_compositor_host(ConnectionFromClient& client)
{
    return make<WebContentCompositorHost>(client);
}

}
