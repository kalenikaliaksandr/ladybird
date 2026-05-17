/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/SharedImage.h>
#include <LibGfx/Size.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Compositor {

enum class WindowResizingInProgress : u8 {
    No,
    Yes,
};

class WEB_API PageCompositor {
public:
    virtual ~PageCompositor() = default;

    enum class PagePresentationRegistration {
        No,
        Yes,
    };

    struct PendingAsyncScrollUpdates {
        Vector<AsyncScrollOffset> scroll_offsets;
        Vector<AsyncScrollOperationID> completed_operation_ids;
    };

    struct AsyncScrollEnqueueResult {
        bool accepted { false };
        Optional<AsyncScrollOperationID> operation_id;
    };

    enum class AsyncScrollOperationTracking {
        No,
        Yes,
    };

    struct PresentToUI {
    };

    struct PublishToCompositorSurface {
        PageCompositor* target { nullptr };
        Painting::CompositorSurfaceId surface_id;
    };

    using PresentationMode = Variant<PresentToUI, PublishToCompositorSurface>;

    virtual void start(DisplayListPlayerType) = 0;
    virtual void stop_presenting_to_client() = 0;
    virtual void set_presentation_mode(PresentationMode) = 0;

    virtual void update_display_list(NonnullRefPtr<Painting::DisplayList>, Painting::DisplayListResourceTransaction&&, Painting::ScrollStateSnapshot&&) = 0;
    virtual void update_compositor_surface(Painting::CompositorSurfaceId, Gfx::SharedImage&&) = 0;
    virtual void clear_compositor_surface(Painting::CompositorSurfaceId) = 0;
    virtual void update_scroll_state(Painting::ScrollStateSnapshot&&) = 0;
    virtual void invalidate_wheel_event_listener_state(u64 generation) = 0;
    virtual AsyncScrollEnqueueResult async_scroll_by(UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels,
        Gfx::IntRect viewport_rect, AsyncScrollOperationTracking = AsyncScrollOperationTracking::No)
        = 0;
    virtual bool should_defer_async_scroll_offset_adoption() const = 0;
    virtual bool should_defer_main_thread_present_for_async_scroll() const = 0;
    virtual PendingAsyncScrollUpdates take_pending_async_scroll_updates() = 0;
    virtual void viewport_size_updated(Gfx::IntSize, bool is_top_level_traversable, WindowResizingInProgress) = 0;
    virtual u64 present_frame(Gfx::IntRect) = 0;
    virtual void wait_for_frame(u64 frame_id) = 0;
    virtual void request_screenshot(NonnullRefPtr<Gfx::PaintingSurface>, Function<void()>&& callback) = 0;
};

}
