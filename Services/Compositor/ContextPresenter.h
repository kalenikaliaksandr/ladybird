/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <AK/Vector.h>
#include <Compositor/BackingStoreManager.h>
#include <Compositor/CanvasSurfaceRegistry.h>
#include <Compositor/CompositedContext.h>
#include <Compositor/CompositorCoreHost.h>
#include <Compositor/DisplayListRasterCache.h>
#include <Compositor/Forward.h>
#include <Compositor/FrameInputs.h>
#include <Compositor/FramePlanner.h>
#include <Compositor/PaintingSurface.h>
#include <Compositor/PresentableSurface.h>
#include <Compositor/RemoteLayer.h>
#include <LibCompositing/DisplayList/AccumulatedVisualContext.h>
#include <LibCompositing/DisplayList/DisplayList.h>
#include <LibCompositing/DisplayList/DisplayListResourceStorage.h>
#include <LibCompositing/Forward.h>
#include <LibCompositing/OverlayFade.h>
#include <LibCompositing/Types.h>
#include <LibCompositing/UIOverlays.h>
#include <LibCore/Forward.h>
#include <LibGfx/Rect.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/Size.h>
#include <LibMediaCodecs/VideoFrame.h>

namespace Gfx {

class SkiaBackendContext;

}

namespace Compositor {

struct PreparedFrame {
    // Null for a frame that changes only what the window server shows above the frame.
    Gfx::PaintingSurface* rendered_surface { nullptr };
    i32 bitmap_id { 0 };
    Gfx::IntRect damage_rect;
    double device_pixel_ratio { 1.0 };
    // What the window server shows above the frame, bottom to top.
    Vector<RemoteSublayer> sublayers;
};

// Rasterizes the scene of a context into its backing stores and keeps track of the frames it presents.
class ContextPresenter {
    AK_MAKE_NONCOPYABLE(ContextPresenter);
    AK_MAKE_NONMOVABLE(ContextPresenter);

public:
    ContextPresenter(OwnedCanvasSurfaces, bool presents_to_client);
    ~ContextPresenter();

    bool has_display_list() const { return m_display_list; }
    void set_display_list(NonnullRefPtr<Compositing::DisplayList const>);
    Compositing::DisplayListResourceStorage& resource_storage() { return m_resource_storage; }
    Compositing::DisplayListResourceStorage const& resource_storage() const { return m_resource_storage; }
    void apply_resource_transaction(Compositing::DisplayListResourceTransaction&&);

    bool presents_to_client() const { return m_presents_to_client; }
    void stop_presenting_to_client() { m_presents_to_client = false; }

    // Whether the window server may show the canvases and videos of the frames above them, instead of the frames
    // holding them.
    void set_window_server_may_show_media(bool may_show_media) { m_window_server_may_show_media = may_show_media; }
    // Otherwise, the frame draws HDR content tone-mapped to SDR.
    void set_window_server_may_show_high_dynamic_range(bool may_show) { m_window_server_may_show_high_dynamic_range = may_show; }
    void did_deliver_frame_without_store() { m_frame_without_store_in_flight = false; }

    Gfx::IntSize viewport_size() const { return m_viewport_size; }
    double device_pixel_ratio() const { return m_device_pixel_ratio; }
    void set_viewport_size(Gfx::IntSize, double device_pixel_ratio, Compositing::WindowResizingInProgress);
    bool window_resize_in_progress() const { return m_window_resize_in_progress == Compositing::WindowResizingInProgress::Yes; }
    bool should_shrink_backing_stores_after_resize() const;
    void schedule_backing_store_shrink(Function<void()>);
    void finish_window_resize();

    Compositing::UIOverlays const& ui_overlays() const { return m_ui_overlays; }
    void set_ui_overlays(Compositing::UIOverlays);
    Optional<Gfx::IntRect> viewport_rect_for_ui_overlay() const;
    bool ui_overlay_fades_need_frame() const { return m_picture_in_picture_controls_fade.is_fading(); }
    Optional<PendingFrame> advance_ui_overlay_fades(MonotonicTime frame_time);

    Optional<BackingStoreManager::Publication> resize_backing_stores_if_needed(RefPtr<Gfx::SkiaBackendContext> const&, BackingStoreManager::GpuSharing, BackingStoreManager::PublicationMode);
    void invalidate_backing_stores();
    Optional<BackingStoreManager::Publication> add_backing_store_for_pending_frame_if_needed(RefPtr<Gfx::SkiaBackendContext> const&);
    bool has_surplus_backing_stores() const { return m_backing_store_manager.has_surplus_backing_stores(); }
    Vector<i32> retire_idle_surplus_backing_stores() { return m_backing_store_manager.retire_idle_surplus_backing_stores(); }
    void schedule_surplus_backing_store_retirement(Function<void()>);
    bool acknowledge_presented_bitmap(i32 bitmap_id);
    void acknowledge_all_presented_bitmaps() { m_backing_store_manager.release_all_presented_buffers(); }
    void did_finish_gpu_present(i32 bitmap_id, Gfx::IntSize frame_size);
    RefPtr<Gfx::PaintingSurface> latest_rendered_surface() const { return m_latest_rendered_surface; }
    Gfx::SharedImageBuffer const* shared_image_buffer_of_latest_rendered_store() const { return m_backing_store_manager.shared_image_buffer_of_latest_rendered_store(); }
    BackingStoreManager::PublicationMode backing_store_publication_mode() const { return m_backing_store_manager.publication_mode(); }

    bool update_composited_raster_transform(Gfx::FloatRect destination_rect, Gfx::FloatMatrix4x4 const& canvas_transform);
    CompositedContextSurface composited_surface() const;

    void queue_present_frame(PendingFrame);
    void mark_pending_present_frame_scheduled();
    void unschedule_pending_present_frame() { m_pending_present_frame_scheduled = false; }
    bool pending_present_frame_scheduled() const { return m_pending_present_frame_scheduled; }
    bool can_schedule_pending_present_frame_if_unblocked() const;
    Optional<PendingFrame> take_pending_present_frame_if_unblocked();
    bool needs_rasterization() const;
    Optional<Gfx::IntRect> frame_rect_to_repaint() const;
    Optional<Gfx::IntRect> self_present_rect() const;
    bool draws_canvas(Compositing::CanvasId) const;

    // The inputs are empty for a context that has no display list yet.
    Optional<PreparedFrame> prepare_frame(DisplayListPlayerSkia&, PendingFrame, Optional<FrameInputs>&&, CompositedContextResolver const*);
    void did_submit_prepared_frame(Gfx::IntRect);
    bool present_synchronously(DisplayListPlayerSkia&, Optional<FrameInputs>&&, CompositedContextResolver const*);
    void paint_screenshot(DisplayListPlayerSkia&, Gfx::ShareableBitmap&, FrameInputs const&, CompositedContextResolver const*);
    // The frame on screen, read back from the store that holds it, and scaled down to fit within the size given.
    Optional<Gfx::ShareableBitmap> snapshot_of_presented_frame(DisplayListPlayerSkia&, Optional<Gfx::IntSize> maximum_size);

private:
    struct RasterizedFrame {
        NonnullRefPtr<Compositing::DisplayList const> display_list;
        FrameInputs frame_inputs;
        Gfx::IntSize viewport_size;
    };

    enum class PaintUIOverlay : u8 {
        No,
        Yes,
    };

    struct RasterSlot {
        RefPtr<Gfx::PaintingSurface> surface;
        Gfx::IntRect rect;
        // Changes whenever anything is drawn into the surface.
        u64 content_version { 0 };
    };

    // The surface that the window server shows a raster slot from.
    struct SlotPresentation {
        PresentableSurfacePool surfaces;
        RefPtr<PresentableSurface> latest_surface;
        Gfx::IntRect rect;
        u64 content_version { 0 };
    };

    bool is_present_blocked() const;
    bool can_render_frame() const;
    // Returns the rect it painted, which grows to the whole frame when the frame has to hold media that the window
    // server was to show.
    [[nodiscard]] Gfx::IntRect paint_frame(DisplayListPlayerSkia&, Gfx::PaintingSurface&, FrameInputs const&, CompositedContextResolver const*, Gfx::IntRect damage_rect, FramePlan const&);
    void paint_current_display_list(DisplayListPlayerSkia&, Gfx::PaintingSurface&, FrameInputs const&, CompositedContextResolver const*, Optional<Gfx::IntRect> damage_rect = {}, PaintUIOverlay = PaintUIOverlay::Yes, bool apply_raster_transform = true);
    bool paint_from_raster_slots(DisplayListPlayerSkia&, Gfx::PaintingSurface&, FrameInputs const&, CompositedContextResolver const*, Gfx::IntRect damage_rect, FramePlan const&);
    void run_store_steps(DisplayListPlayerSkia&, Gfx::PaintingSurface&, FrameInputs const&, CompositedContextResolver const*, ReadonlySpan<FrameStep>, Gfx::IntRect damage_rect);
    bool present_slot_to_window_server(FrameStep const&, RefPtr<Gfx::SkiaBackendContext> const&);
    bool present_compositor_overlays_to_window_server(DisplayListPlayerSkia&, FrameInputs const&, RefPtr<Gfx::SkiaBackendContext> const&);
    Gfx::IntRect compositor_overlay_bounds(FrameInputs const&) const;
    Vector<RemoteSublayer> sublayers_shown_by_window_server();
    RefPtr<Media::VideoFrame> current_video_frame(Compositing::VideoSinkResourceId);
    void paint_compositor_overlays(DisplayListPlayerSkia&, Gfx::PaintingSurface&, FrameInputs const&, PaintUIOverlay);
    Gfx::IntSize raster_size() const;
    Gfx::IntRect raster_damage_rect(Gfx::IntRect) const;
    FramePlan plan_frame(PendingFrame const&, FrameInputs const&);
    LiveMediaSample sample_live_media();
    void commit_frame_plan();
    void release_raster_slots();
    void remember_rasterized_frame(Gfx::IntSize viewport_size, FrameInputs&&);
    void stop_backing_store_shrink_timer();
    void stop_surplus_backing_store_retirement_timer();
    void stop_idle_raster_slot_release_timer();

    RefPtr<Compositing::DisplayList const> m_display_list;
    Compositing::DisplayListResourceStorage m_resource_storage;
    DisplayListRasterCache m_raster_cache;
    OwnedCanvasSurfaces m_canvas_surfaces;
    bool m_presents_to_client { false };

    Gfx::IntSize m_viewport_size;
    double m_device_pixel_ratio { 1.0 };
    Compositing::WindowResizingInProgress m_window_resize_in_progress { Compositing::WindowResizingInProgress::No };
    Compositing::UIOverlays m_ui_overlays;
    Compositing::OverlayFade m_picture_in_picture_controls_fade;

    BackingStoreManager m_backing_store_manager;
    RefPtr<Gfx::PaintingSurface> m_latest_rendered_surface;
    // The frame that the latest rendered surface holds, which the presented frame runs ahead of while its GPU work is
    // in flight.
    Gfx::IntSize m_size_of_latest_rendered_frame;
    RefPtr<Gfx::PaintingSurface> m_damage_surface;
    Optional<RasterizedFrame> m_last_rasterized_frame;
    Gfx::FloatSize m_unbounded_raster_scale_of_embedding { 1, 1 };
    Gfx::FloatSize m_raster_scale { 1, 1 };
    Gfx::FloatPoint m_raster_translation;
    RefPtr<Core::Timer> m_backing_store_shrink_timer;
    RefPtr<Core::Timer> m_surplus_backing_store_retirement_timer;

    FramePlanner m_frame_planner;
    Vector<RasterSlot> m_raster_slots;
    u64 m_last_raster_slot_content_version { 0 };

    bool m_window_server_may_show_media { false };
    bool m_window_server_may_show_high_dynamic_range { true };
    bool m_frame_without_store_in_flight { false };
    Vector<SlotPresentation> m_slot_presentations;
    PresentableSurfacePool m_compositor_overlay_surfaces;
    RefPtr<PresentableSurface> m_latest_compositor_overlay_surface;
    Gfx::IntRect m_compositor_overlay_rect;
    RefPtr<Core::Timer> m_idle_raster_slot_release_timer;

    Optional<PendingFrame> m_pending_present_frame;
    bool m_pending_present_frame_scheduled { false };
    Optional<Gfx::IntRect> m_presented_frame;
};

}
