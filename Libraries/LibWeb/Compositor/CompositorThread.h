/*
 * Copyright (c) 2025-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Types.h>
#include <LibCore/Forward.h>
#include <LibGfx/SharedImage.h>
#include <LibGfx/Size.h>
#include <LibSync/ConditionVariable.h>
#include <LibThreading/Forward.h>
#include <LibWeb/Compositor/PageCompositor.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Page/InputEvent.h>

namespace Web::Compositor {

class CompositorEngine;

class WEB_API CompositorThread final : public PageCompositor {
    AK_MAKE_NONCOPYABLE(CompositorThread);
    AK_MAKE_NONMOVABLE(CompositorThread);
    friend class CompositorEngine;

public:
    using PagePresentationRegistration = PageCompositor::PagePresentationRegistration;
    using PendingAsyncScrollUpdates = PageCompositor::PendingAsyncScrollUpdates;
    using AsyncScrollEnqueueResult = PageCompositor::AsyncScrollEnqueueResult;
    using AsyncScrollOperationTracking = PageCompositor::AsyncScrollOperationTracking;
    using PresentToUI = PageCompositor::PresentToUI;
    using PublishToCompositorSurface = PageCompositor::PublishToCompositorSurface;
    using PresentationMode = PageCompositor::PresentationMode;

    using BackingStorePresentationCallback = Function<void(u64 page_id, i32 front_bitmap_id, Gfx::SharedImage, i32 back_bitmap_id, Gfx::SharedImage)>;
    using FramePresentationCallback = Function<void(u64 page_id, Gfx::IntRect const&, i32 bitmap_id)>;

    CompositorThread(u64 page_id, PagePresentationRegistration);
    virtual ~CompositorThread() override;

    static void set_frame_presentation_callbacks(NonnullRefPtr<Core::WeakEventLoopReference>, BackingStorePresentationCallback, FramePresentationCallback);
    static void clear_frame_presentation_callbacks();
    static void presented_bitmap_ready_to_paint(u64 page_id, i32 bitmap_id);
    static bool async_scroll_by(u64 page_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels);
    static bool handle_mouse_event(u64 page_id, MouseEvent const&);

    virtual CompositorContextId context_id() const override { return m_context_id; }
    virtual void start(DisplayListPlayerType) override;
    virtual void stop_presenting_to_client() override;
    virtual void set_presentation_mode(PresentationMode) override;

    virtual void update_display_list(NonnullRefPtr<Painting::DisplayList>, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&) override;
    virtual void update_compositor_surface(Painting::CompositorSurfaceId, Gfx::SharedImage&&) override;
    virtual void clear_compositor_surface(Painting::CompositorSurfaceId) override;
    virtual void update_scroll_state(Painting::ScrollStateSnapshot&&) override;
    virtual void invalidate_wheel_event_listener_state(u64 generation) override;
    virtual AsyncScrollEnqueueResult async_scroll_by(UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels,
        Gfx::IntRect viewport_rect, AsyncScrollOperationTracking = AsyncScrollOperationTracking::No) override;
    virtual bool should_defer_async_scroll_offset_adoption() const override;
    virtual bool should_defer_main_thread_present_for_async_scroll() const override;
    virtual PendingAsyncScrollUpdates take_pending_async_scroll_updates() override;
    virtual void viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress) override;
    virtual u64 present_frame(Gfx::IntRect) override;
    virtual void wait_for_frame(u64 frame_id) override;
    virtual void request_screenshot(NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback) override;

private:
    void enqueue_viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress);

    CompositorContextId m_context_id { allocate_compositor_context_id() };
    NonnullRefPtr<CompositorEngine> m_engine;
    RefPtr<Threading::Thread> m_thread;
    RefPtr<Core::Timer> m_backing_store_shrink_timer;
    Gfx::IntSize m_last_viewport_size;
    bool m_last_viewport_size_is_top_level_traversable { false };

    static void register_page_compositor(u64 page_id, NonnullRefPtr<CompositorEngine>);
    static void unregister_page_compositor(u64 page_id, CompositorEngine&);
    static void register_context_compositor(CompositorContextId, NonnullRefPtr<CompositorEngine>);
    static void unregister_context_compositor(CompositorContextId, CompositorEngine&);
    static bool update_compositor_surface_for_context(CompositorContextId, Painting::CompositorSurfaceId, Gfx::SharedImage&&);
    static bool present_backing_stores_to_client(u64 page_id, i32 front_bitmap_id, Gfx::SharedImage&&, i32 back_bitmap_id, Gfx::SharedImage&&);
    static bool present_frame_to_client(u64 page_id, Gfx::IntRect const&, i32 bitmap_id);
};

}
