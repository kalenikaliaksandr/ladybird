/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <AK/Span.h>
#include <AK/Vector.h>
#include <Compositor/WebContentCompositorClientEndpoint.h>
#include <Compositor/WebContentCompositorServerEndpoint.h>
#include <LibGfx/Point.h>
#include <LibGfx/Size.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibIPC/TransportHandle.h>
#include <LibWeb/Compositor/AsyncScrollTree.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Compositor/BackingStoreManager.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Page/InputEvent.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Compositor {

enum class RasterizeResult {
    Completed,
    Deferred,
};

class ConnectionFromWebContent final
    : public IPC::ConnectionFromClient<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint> {
    C_OBJECT(ConnectionFromWebContent);

public:
    static ErrorOr<Web::Compositor::WebContentConnectionId> connect(IPC::TransportHandle);
    static bool has_connection(Web::Compositor::WebContentConnectionId);
    static bool async_scroll_by(Web::Compositor::PresentationId, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels);
    static bool handle_mouse_event(Web::Compositor::PresentationId, Web::MouseEvent const&);
    static void presented_bitmap_ready_to_paint(Web::Compositor::PresentationId, i32 bitmap_id);

    virtual void die() override;

private:
    enum class VideoFrameUpdateResult {
        Applied,
        Consumed,
        MissingSource,
    };

    struct PresentationModeState {
        Web::Compositor::SerializedPresentationModeKind kind;
        Web::Compositor::PresentationId presentation_id;
        Web::Compositor::PresentationCapability presentation_capability;
        Web::Compositor::CompositorContextId target_context_id;
        Web::Painting::CompositorSurfaceId compositor_surface_id;
    };

    struct ContextState {
        struct PendingPresent {
            u64 frame_id { 0 };
            Gfx::IntRect viewport_rect;
        };

        Web::Compositor::CompositorContextId context_id;
        u64 page_id { 0 };
        PresentationModeState presentation_mode;
        Web::DisplayListPlayerType display_list_player_type;
        Optional<Gfx::IntSize> viewport_size;
        Web::Painting::DisplayListResourceStorage display_list_resource_storage;
        RefPtr<Web::Painting::DisplayList> display_list;
        Optional<Web::Painting::ScrollStateSnapshot> scroll_state_snapshot;
        Web::Compositor::AsyncScrollTree async_scroll_tree;
        Vector<Web::Compositor::ViewportScrollbar> viewport_scrollbars;
        Optional<size_t> hovered_viewport_scrollbar_index;
        Optional<size_t> captured_viewport_scrollbar_index;
        float viewport_scrollbar_thumb_grab_position { 0 };
        Vector<Web::Compositor::AsyncScrollOffset> pending_async_scroll_offsets;
        Vector<Web::Compositor::AsyncScrollOperationID> completed_async_scroll_operation_ids;
        Web::Compositor::AsyncScrollOperationID next_async_scroll_operation_id { 0 };
        Gfx::IntRect async_scrolling_viewport_rect;
        bool has_async_scrolling_state { false };
        bool can_accept_async_wheel_events { false };
        u64 wheel_event_listener_state_generation { 0 };
        Web::Compositor::WheelRoutingAdmission wheel_routing_admission { Web::Compositor::WheelRoutingAdmission::NoAsyncScrollingState };
        HashMap<Web::Painting::VideoFrameResourceId, u64> video_frame_sequence_ids;
        HashMap<Web::Painting::VideoFrameResourceId, Web::Compositor::SerializedVideoFrameUpdate> pending_video_frame_updates;
        bool has_pending_display_list_update { false };
        bool has_pending_scroll_state_update { false };
        Optional<PendingPresent> pending_present;
        Optional<Gfx::IntRect> pending_async_scroll_present;
        NonnullOwnPtr<Web::Compositor::BackingStoreManager> backing_store_manager;
        Optional<i32> presented_bitmap_id_awaiting_ack;
        u64 submitted_frame_id { 0 };
        u64 completed_frame_id { 0 };
        bool is_top_level_traversable { false };
        Web::Compositor::WindowResizingInProgress window_resize_in_progress { Web::Compositor::WindowResizingInProgress::No };
        bool presents_to_client { true };
    };

    ConnectionFromWebContent(NonnullOwnPtr<IPC::Transport>, Web::Compositor::WebContentConnectionId);

    Optional<PresentationModeState> presentation_mode_state_from_marshaled(Web::Compositor::SerializedPresentationModeKind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id) const;
    bool validate_presentation_mode(Web::Compositor::CompositorContextId, PresentationModeState const&) const;
    ErrorOr<void> allocate_backing_stores_if_needed(ContextState&, Gfx::IntSize viewport_size);
    RasterizeResult rasterize_present(ContextState&, Gfx::IntRect viewport_rect, Optional<u64> frame_id, bool clear_pending_update_flags);
    void rasterize_pending_present(ContextState&);
    void schedule_async_scroll_present(ContextState&, Gfx::IntRect viewport_rect);
    void rasterize_pending_async_scroll_present(ContextState&);
    void publish_context_to_compositor_surface(ContextState&);
    bool mark_presented_bitmap_ready_to_paint(Web::Compositor::PresentationId, i32 bitmap_id);
    ContextState* context_for_presentation(Web::Compositor::PresentationId);
    Optional<Gfx::FloatPoint> reapply_pending_async_scroll_offsets(ContextState&, ReadonlySpan<Web::Compositor::AsyncScrollOffset>);
    Web::Compositor::AsyncScrollOperationID next_async_scroll_operation_id(ContextState&);
    void complete_async_scroll_operation(ContextState&, Optional<Web::Compositor::AsyncScrollOperationID>);
    void store_pending_async_scroll_offsets(ContextState&, ReadonlySpan<Web::Compositor::AsyncScrollOffset>, Optional<Web::Compositor::AsyncScrollOperationID> = {});
    void update_async_scrolling_state_from_display_list(ContextState&);
    Optional<size_t> hit_test_viewport_scrollbar(ContextState const&, Gfx::FloatPoint position) const;
    void set_hovered_viewport_scrollbar(ContextState&, Optional<size_t> scrollbar_index);
    bool apply_viewport_scrollbar_drag(ContextState&, size_t scrollbar_index, float primary_position, float thumb_grab_position);
    bool handle_viewport_scrollbar_mouse_event(ContextState&, Web::MouseEvent const&);
    bool apply_async_scroll_to_target(ContextState&, Web::Compositor::AsyncScrollNodeID scroll_target, Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, Optional<Web::Compositor::AsyncScrollOperationID> = {});
    void defer_async_scroll_to_target(ContextState&, Web::Compositor::AsyncScrollNodeID scroll_target, Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, Optional<Web::Compositor::AsyncScrollOperationID> = {});
    Messages::WebContentCompositorServer::EnqueueAsyncScrollByResponse enqueue_async_scroll_by(ContextState&, Web::UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, bool track_operation);
    bool async_scroll_by(ContextState&, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels);
    VideoFrameUpdateResult apply_video_frame_update(ContextState&, Web::Compositor::SerializedVideoFrameUpdate const&);
    void apply_pending_video_frame_updates(ContextState&);

    virtual void create_context(u64 context_id, u64 page_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id, Web::DisplayListPlayerType display_list_player_type) override;
    virtual void destroy_context(u64 context_id) override;
    virtual void set_presentation_mode(u64 context_id, Web::Compositor::SerializedPresentationModeKind presentation_mode_kind, u64 presentation_id, u64 presentation_capability, u64 target_context_id, u64 compositor_surface_id) override;
    virtual void stop_presenting_to_client(u64 context_id) override;
    virtual void viewport_size_updated(u64 context_id, Gfx::IntSize viewport_size, bool is_top_level_traversable, Web::Compositor::WindowResizingInProgress window_resize_in_progress) override;
    virtual void update_display_list(u64 context_id, NonnullRefPtr<Web::Painting::DisplayList> display_list, Web::Painting::DisplayListResourceTransaction resource_transaction, Web::Painting::ScrollStateSnapshot scroll_state) override;
    virtual void update_scroll_state(u64 context_id, Web::Painting::ScrollStateSnapshot scroll_state) override;
    virtual void invalidate_wheel_event_listener_state(u64 context_id, u64 generation) override;
    virtual Messages::WebContentCompositorServer::EnqueueAsyncScrollByResponse enqueue_async_scroll_by(u64 context_id, Web::UniqueNodeID expected_document_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels, Gfx::IntRect viewport_rect, bool track_operation) override;
    virtual Messages::WebContentCompositorServer::ShouldDeferAsyncScrollOffsetAdoptionResponse should_defer_async_scroll_offset_adoption(u64 context_id) override;
    virtual Messages::WebContentCompositorServer::ShouldDeferMainThreadPresentForAsyncScrollResponse should_defer_main_thread_present_for_async_scroll(u64 context_id) override;
    virtual Messages::WebContentCompositorServer::TakePendingAsyncScrollUpdatesResponse take_pending_async_scroll_updates(u64 context_id) override;
    virtual void update_compositor_surface(u64 context_id, u64 surface_id, Gfx::SharedImage shared_image) override;
    virtual void clear_compositor_surface(u64 context_id, u64 surface_id) override;
    virtual void update_yuv_video_frame(u64 context_id, Web::Compositor::SerializedVideoFrameUpdate video_frame_update) override;
    virtual void clear_video_frame(u64 context_id, u64 video_frame_source_id) override;
    virtual void request_screenshot(u64 context_id, u64 request_id, Gfx::IntSize size) override;
    virtual Messages::WebContentCompositorServer::PresentFrameResponse present_frame(u64 context_id, Gfx::IntRect viewport_rect) override;
    virtual void wait_for_frame(u64 context_id, u64 frame_id) override;

    Web::Compositor::WebContentConnectionId m_connection_id;
    HashMap<Web::Compositor::CompositorContextId, ContextState> m_contexts;
};

}
