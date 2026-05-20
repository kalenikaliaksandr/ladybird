/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/OwnPtr.h>
#include <AK/RefPtr.h>
#include <AK/Types.h>
#include <LibCore/Forward.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/SharedImage.h>
#include <LibGfx/Size.h>
#include <LibMedia/Forward.h>
#include <LibWeb/Compositor/CompositorThread.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Compositor {

class WEB_API CompositorContextHandle {
    AK_MAKE_NONCOPYABLE(CompositorContextHandle);
    AK_MAKE_NONMOVABLE(CompositorContextHandle);

public:
    ~CompositorContextHandle();

    CompositorContextId id() const { return m_context_id; }
    void destroy();
    void stop_presenting_to_client();
    void set_presentation_mode(PresentationMode);

    void update_display_list(NonnullRefPtr<Painting::DisplayList>, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&);
    void update_video_frame(Painting::VideoFrameResourceId, NonnullRefPtr<Media::VideoFrame const>);
    void clear_video_frame(Painting::VideoFrameResourceId);
    void update_compositor_surface(Painting::CompositorSurfaceId, Gfx::SharedImage&&);
    void clear_compositor_surface(Painting::CompositorSurfaceId);
    void update_scroll_state(Painting::ScrollStateSnapshot&&);
    void invalidate_wheel_event_listener_state(u64 generation);
    AsyncScrollEnqueueResult async_scroll_by(UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels,
        Gfx::IntRect viewport_rect, AsyncScrollOperationTracking = AsyncScrollOperationTracking::No);
    bool should_defer_async_scroll_offset_adoption() const;
    bool should_defer_main_thread_present_for_async_scroll() const;
    PendingAsyncScrollUpdates take_pending_async_scroll_updates();
    void viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress);
    void present_frame(Gfx::IntRect);
    void request_screenshot(Gfx::SharedImage&&, Function<void()>&& callback);

private:
    friend class CompositorHost;

    CompositorContextHandle(CompositorHost&, CompositorContextId);

    void enqueue_viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress);
    void cancel_backing_store_shrink_timer();

    CompositorHost& m_host;
    CompositorContextId m_context_id;
    RefPtr<Core::Timer> m_backing_store_shrink_timer;
    Gfx::IntSize m_last_viewport_size;
    bool m_last_viewport_size_is_top_level_traversable { false };
    bool m_destroyed { false };
};

class WEB_API CompositorHost {
    AK_MAKE_NONCOPYABLE(CompositorHost);
    AK_MAKE_NONMOVABLE(CompositorHost);

public:
    static NonnullOwnPtr<CompositorHost> create();
    virtual ~CompositorHost();

    void start(DisplayListPlayerType);
    OwnPtr<CompositorContextHandle> create_context(Optional<u64> page_id, PagePresentationRegistration);

    void destroy_context(CompositorContextId);
    void stop_presenting_to_client(CompositorContextId);
    void set_presentation_mode(CompositorContextId, PresentationMode);

    void update_display_list(CompositorContextId, NonnullRefPtr<Painting::DisplayList>, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&);
    void update_video_frame(CompositorContextId, Painting::VideoFrameResourceId, NonnullRefPtr<Media::VideoFrame const>);
    void clear_video_frame(CompositorContextId, Painting::VideoFrameResourceId);
    void update_compositor_surface(CompositorContextId, Painting::CompositorSurfaceId, Gfx::SharedImage&&);
    void clear_compositor_surface(CompositorContextId, Painting::CompositorSurfaceId);
    void update_scroll_state(CompositorContextId, Painting::ScrollStateSnapshot&&);
    void invalidate_wheel_event_listener_state(CompositorContextId, u64 generation);
    AsyncScrollEnqueueResult async_scroll_by(CompositorContextId, UniqueNodeID expected_document_id, Gfx::FloatPoint position,
        Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking);
    bool should_defer_async_scroll_offset_adoption(CompositorContextId) const;
    bool should_defer_main_thread_present_for_async_scroll(CompositorContextId) const;
    PendingAsyncScrollUpdates take_pending_async_scroll_updates(CompositorContextId);
    void viewport_size_updated(CompositorContextId, Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress);
    void present_frame(CompositorContextId, Gfx::IntRect);
    void request_screenshot(CompositorContextId, Gfx::SharedImage&&, Function<void()>&& callback);

protected:
    CompositorHost() = default;

private:
    virtual void start_impl(DisplayListPlayerType) = 0;
    virtual CompositorContextId create_context_impl(Optional<u64> page_id, PagePresentationRegistration) = 0;

    virtual void destroy_context_impl(CompositorContextId) = 0;
    virtual void stop_presenting_to_client_impl(CompositorContextId) = 0;
    virtual void set_presentation_mode_impl(CompositorContextId, PresentationMode) = 0;

    virtual void update_display_list_impl(CompositorContextId, NonnullRefPtr<Painting::DisplayList>, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&) = 0;
    virtual void update_video_frame_impl(CompositorContextId, Painting::VideoFrameResourceId, NonnullRefPtr<Media::VideoFrame const>) = 0;
    virtual void clear_video_frame_impl(CompositorContextId, Painting::VideoFrameResourceId) = 0;
    virtual void update_compositor_surface_impl(CompositorContextId, Painting::CompositorSurfaceId, Gfx::SharedImage&&) = 0;
    virtual void clear_compositor_surface_impl(CompositorContextId, Painting::CompositorSurfaceId) = 0;
    virtual void update_scroll_state_impl(CompositorContextId, Painting::ScrollStateSnapshot&&) = 0;
    virtual void invalidate_wheel_event_listener_state_impl(CompositorContextId, u64 generation) = 0;
    virtual AsyncScrollEnqueueResult async_scroll_by_impl(CompositorContextId, UniqueNodeID expected_document_id, Gfx::FloatPoint position,
        Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, AsyncScrollOperationTracking)
        = 0;
    virtual bool should_defer_async_scroll_offset_adoption_impl(CompositorContextId) const = 0;
    virtual bool should_defer_main_thread_present_for_async_scroll_impl(CompositorContextId) const = 0;
    virtual PendingAsyncScrollUpdates take_pending_async_scroll_updates_impl(CompositorContextId) = 0;
    virtual void viewport_size_updated_impl(CompositorContextId, Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress) = 0;
    virtual void present_frame_impl(CompositorContextId, Gfx::IntRect) = 0;
    virtual void request_screenshot_impl(CompositorContextId, Gfx::SharedImage&&, Function<void()>&& callback) = 0;
};

}
