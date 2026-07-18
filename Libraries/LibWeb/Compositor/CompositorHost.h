/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/OwnPtr.h>
#include <AK/Types.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/Size.h>
#include <LibMedia/Forward.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>
#include <LibWeb/Painting/OffscreenCanvasLink.h>

namespace Web::Compositor {

class CompositorHost;

class WEB_API CompositorContextHandle {
    AK_MAKE_NONCOPYABLE(CompositorContextHandle);
    AK_MAKE_NONMOVABLE(CompositorContextHandle);

public:
    ~CompositorContextHandle();

    CompositorContextId id() const { return m_context_id; }
    void set_parent_context(Optional<CompositorContextId>);
    void stop_presenting_to_client();

    void update_display_list(NonnullRefPtr<Painting::DisplayList>, Painting::AccumulatedVisualContextTree, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&);
    void update_visual_context_tree(Painting::AccumulatedVisualContextTree);
    void update_video_frame(Painting::VideoFrameResourceId, NonnullRefPtr<Media::VideoFrame const>);
    void clear_video_frame(Painting::VideoFrameResourceId);
    void update_scroll_state(Painting::ScrollStateSnapshot&&);
    void invalidate_wheel_event_listener_state(u64 generation);
    AsyncScrollEnqueueResult async_scroll_by(UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels,
        Gfx::IntRect viewport_rect, AsyncScrollOperationTracking = AsyncScrollOperationTracking::No);
    PendingAsyncScrollUpdates take_pending_async_scroll_updates();
    void viewport_size_updated(Gfx::IntSize, WindowResizingInProgress);
    void present_frame(Gfx::IntRect viewport_rect, Gfx::IntRect damage_rect);
    void request_screenshot(NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback);

private:
    friend class CompositorHost;

    CompositorContextHandle(CompositorHost&, CompositorContextId);

    CompositorHost& m_host;
    CompositorContextId m_context_id;
};

class WEB_API CompositorHost {
    AK_MAKE_NONCOPYABLE(CompositorHost);
    AK_MAKE_NONMOVABLE(CompositorHost);

public:
    virtual ~CompositorHost();

    OwnPtr<CompositorContextHandle> create_context(CompositorContextId);

    Painting::Canvas2DCommandStream& canvas_2d_stream() { return *m_canvas_2d_stream; }
    void flush_canvas_2d_stream();
    void discard_canvas_2d_stream();

    virtual RefPtr<WebGL::RemoteWebGLTransport> create_webgl_transport() = 0;
    virtual RefPtr<HTML::RemoteCanvas2DTransport> create_canvas_2d_transport() = 0;

    virtual Optional<Painting::OffscreenCanvasPlaceholderLink> allocate_offscreen_canvas_id() = 0;
    // Reads back a canvas surface by id from the shared registry, including surfaces
    // whose producing context lives on another connection (placeholder canvases).
    // The origin-clean state arrives with the pixels so the reader can never pair
    // a tainted frame with a stale clean flag.
    struct CanvasSurfaceReadback {
        RefPtr<Gfx::Bitmap> bitmap;
        bool origin_clean { true };
    };
    virtual CanvasSurfaceReadback read_back_canvas_surface(Painting::CanvasId, Gfx::IntRect const&) = 0;
    virtual void watch_canvas_surface(Painting::OffscreenCanvasPlaceholderLink) = 0;
    virtual void unwatch_canvas_surface(Painting::CanvasId) = 0;
    virtual void release_offscreen_canvas_id(Painting::CanvasId) = 0;

    // Tells the Compositor no claim will ever arrive for this link (its holder
    // died unclaimed), so the reservation can be garbage collected.
    virtual void decline_offscreen_canvas_claim(Painting::OffscreenCanvasPlaceholderLink) = 0;

    // Commits a logical size to placeholder watchers without presenting a
    // surface (a resize to zero, or to dimensions no bitmap can back).
    virtual void commit_offscreen_canvas_size(Painting::OffscreenCanvasPlaceholderLink, Gfx::IntSize logical_size) = 0;

    virtual void destroy_context(CompositorContextId) = 0;
    virtual void set_parent_context(CompositorContextId, Optional<CompositorContextId>) = 0;
    virtual void stop_presenting_to_client(CompositorContextId) = 0;

    virtual void update_display_list(CompositorContextId, NonnullRefPtr<Painting::DisplayList>, Painting::AccumulatedVisualContextTree, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&) = 0;
    virtual void update_visual_context_tree(CompositorContextId, Painting::AccumulatedVisualContextTree) = 0;
    virtual void update_video_frame(CompositorContextId, Painting::VideoFrameResourceId, NonnullRefPtr<Media::VideoFrame const>) = 0;
    virtual void clear_video_frame(CompositorContextId, Painting::VideoFrameResourceId) = 0;
    virtual void update_scroll_state(CompositorContextId, Painting::ScrollStateSnapshot&&) = 0;
    virtual void invalidate_wheel_event_listener_state(CompositorContextId, u64 generation) = 0;
    virtual AsyncScrollEnqueueResult async_scroll_by(CompositorContextId, UniqueNodeID expected_document_id, Gfx::FloatPoint position,
        Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking)
        = 0;
    virtual PendingAsyncScrollUpdates take_pending_async_scroll_updates(CompositorContextId) = 0;
    virtual void viewport_size_updated(CompositorContextId, Gfx::IntSize, WindowResizingInProgress) = 0;
    virtual void present_frame(CompositorContextId, Gfx::IntRect viewport_rect, Gfx::IntRect damage_rect) = 0;
    virtual void request_screenshot(CompositorContextId, NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback) = 0;

protected:
    CompositorHost();

    // Drains the stream, but only when the message can actually be delivered.
    virtual void send_canvas_2d_stream(Painting::Canvas2DCommandStream&) = 0;

private:
    NonnullRefPtr<Painting::Canvas2DCommandStream> m_canvas_2d_stream;
};

}
