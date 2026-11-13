/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/AnyOf.h>
#include <AK/Math.h>
#include <Compositor/CanvasSurfaceRegistry.h>
#include <Compositor/ContextPresenter.h>
#include <Compositor/DisplayListPlayerSkia.h>
#include <Compositor/PausedDebuggerOverlay.h>
#include <Compositor/PictureInPictureControls.h>
#include <Compositor/SkiaUtils.h>
#include <LibCompositing/PictureInPictureControls.h>
#include <LibCore/Timer.h>
#include <LibGfx/Bitmap.h>
#include <LibMedia/Sinks/VideoSink.h>
#include <LibMediaCodecs/VideoFrame.h>
#include <LibMediaCodecs/VideoSurface.h>
#include <core/SkCanvas.h>
#include <core/SkImage.h>
#include <math.h>

namespace Compositor {

static Gfx::FloatSize bounded_composited_raster_scale(Gfx::FloatSize scale, Gfx::IntSize viewport_size)
{
    if (viewport_size.is_empty())
        return { 1, 1 };

    scale = { clamp(scale.width(), 0.25f, 5.0f), clamp(scale.height(), 0.25f, 5.0f) };
    // Reserve a pixel on each axis for fractional placement. The two backing stores together use at most
    // 128 MiB, even when an embedding transform magnifies a large iframe.
    constexpr float maximum_dimension = 16384;
    constexpr float maximum_pixels = 16 * 1024 * 1024;
    scale.set_width(min(scale.width(), (maximum_dimension - 1) / viewport_size.width()));
    scale.set_height(min(scale.height(), (maximum_dimension - 1) / viewport_size.height()));
    auto pixel_count = (viewport_size.width() * scale.width()) * (viewport_size.height() * scale.height());
    constexpr float maximum_content_pixels = maximum_pixels - 2 * maximum_dimension - 1;
    if (pixel_count > maximum_content_pixels)
        scale.scale_by(sqrtf(maximum_content_pixels / pixel_count));
    return scale;
}

static bool video_frame_holds_high_dynamic_range(Media::VideoFrame const& frame)
{
    auto transfer_characteristics = frame.cicp().transfer_characteristics();
    return transfer_characteristics == Media::TransferCharacteristics::SMPTE2084 || transfer_characteristics == Media::TransferCharacteristics::HLG;
}

ContextPresenter::ContextPresenter(OwnedCanvasSurfaces canvas_surfaces, bool presents_to_client)
    : m_canvas_surfaces(canvas_surfaces)
    , m_presents_to_client(presents_to_client)
{
}

ContextPresenter::~ContextPresenter()
{
    stop_backing_store_shrink_timer();
    stop_surplus_backing_store_retirement_timer();
    stop_idle_raster_slot_release_timer();
}

void ContextPresenter::set_display_list(NonnullRefPtr<Compositing::DisplayList const> display_list)
{
    m_display_list = move(display_list);
}

void ContextPresenter::apply_resource_transaction(Compositing::DisplayListResourceTransaction&& resource_transaction)
{
    m_raster_cache.evict(m_resource_storage.apply_transaction(move(resource_transaction)));
}

void ContextPresenter::set_viewport_size(Gfx::IntSize viewport_size, double device_pixel_ratio, Compositing::WindowResizingInProgress window_resize_in_progress)
{
    m_viewport_size = viewport_size;
    m_device_pixel_ratio = device_pixel_ratio;
    m_window_resize_in_progress = window_resize_in_progress;
}

bool ContextPresenter::should_shrink_backing_stores_after_resize() const
{
    return m_window_resize_in_progress == Compositing::WindowResizingInProgress::Yes;
}

void ContextPresenter::schedule_backing_store_shrink(Function<void()> on_timeout)
{
    if (!m_backing_store_shrink_timer)
        m_backing_store_shrink_timer = Core::Timer::create_single_shot(AK::Duration::from_milliseconds(3000), move(on_timeout));
    m_backing_store_shrink_timer->restart();
}

void ContextPresenter::finish_window_resize()
{
    m_window_resize_in_progress = Compositing::WindowResizingInProgress::No;
}

void ContextPresenter::set_ui_overlays(Compositing::UIOverlays overlays)
{
    m_ui_overlays = move(overlays);

    auto const& picture_in_picture_controls = m_ui_overlays.picture_in_picture_controls;
    auto shows_picture_in_picture_controls = picture_in_picture_controls.has_value() && picture_in_picture_controls->shown;
    m_picture_in_picture_controls_fade.fade_to(shows_picture_in_picture_controls ? 1 : 0, MonotonicTime::now());
}

Optional<PendingFrame> ContextPresenter::advance_ui_overlay_fades(MonotonicTime frame_time)
{
    if (!m_picture_in_picture_controls_fade.advance_to(frame_time))
        return {};

    auto viewport_rect = viewport_rect_for_ui_overlay();
    if (!viewport_rect.has_value())
        return {};
    auto geometry = Compositing::picture_in_picture_controls_geometry(m_viewport_size, m_device_pixel_ratio);
    return PendingFrame { *viewport_rect, Compositing::overlay_buttons_bounds(geometry.buttons()) };
}

Optional<Gfx::IntRect> ContextPresenter::viewport_rect_for_ui_overlay() const
{
    if (m_viewport_size.is_empty())
        return {};

    auto viewport_rect = m_pending_present_frame.has_value()
        ? m_pending_present_frame->viewport_rect
        : m_presented_frame.value_or(Gfx::IntRect {});
    viewport_rect.set_size(m_viewport_size);
    return viewport_rect;
}

Optional<BackingStoreManager::Publication> ContextPresenter::resize_backing_stores_if_needed(RefPtr<Gfx::SkiaBackendContext> const& skia_backend_context, BackingStoreManager::GpuSharing gpu_sharing, BackingStoreManager::PublicationMode publication_mode)
{
    if (m_backing_store_manager.is_rendering())
        return {};
    if (!presents_to_client()) {
        // A resize can arrive before the parent's next replay supplies the new embedding transform.
        // Bound the existing embedding's scale for the new viewport before allocating backing stores. Bounding the
        // already bounded scale instead can round it differently, which would discard the raster on every replay.
        auto scale = bounded_composited_raster_scale(m_unbounded_raster_scale_of_embedding, m_viewport_size);
        if (m_raster_scale != scale) {
            m_raster_scale = scale;
            m_latest_rendered_surface = nullptr;
            m_last_rasterized_frame.clear();
        }
    }
    if (!presents_to_client())
        publication_mode = BackingStoreManager::PublicationMode::Unpublished;
    auto allocation = m_backing_store_manager.resize_backing_stores_if_needed(raster_size(), m_window_resize_in_progress, publication_mode);
    if (!allocation.has_value())
        return {};
    m_latest_rendered_surface = nullptr;
    m_last_rasterized_frame.clear();
    return m_backing_store_manager.allocate_backing_stores(*allocation, skia_backend_context, publication_mode, gpu_sharing);
}

void ContextPresenter::invalidate_backing_stores()
{
    m_backing_store_manager.invalidate();
    m_frame_without_store_in_flight = false;
}

Optional<BackingStoreManager::Publication> ContextPresenter::add_backing_store_for_pending_frame_if_needed(RefPtr<Gfx::SkiaBackendContext> const& skia_backend_context)
{
    if (!presents_to_client() || !m_pending_present_frame.has_value() || !can_render_frame())
        return {};
    return m_backing_store_manager.add_backing_store_if_window_server_still_reads_every_released_store(skia_backend_context);
}

void ContextPresenter::schedule_surplus_backing_store_retirement(Function<void()> on_timeout)
{
    if (!m_surplus_backing_store_retirement_timer)
        m_surplus_backing_store_retirement_timer = Core::Timer::create_single_shot(AK::Duration::from_milliseconds(3000), move(on_timeout));
    if (!m_surplus_backing_store_retirement_timer->is_active())
        m_surplus_backing_store_retirement_timer->start();
}

bool ContextPresenter::acknowledge_presented_bitmap(i32 bitmap_id)
{
    return m_backing_store_manager.release_buffer(bitmap_id);
}

void ContextPresenter::did_finish_gpu_present(i32 bitmap_id, Gfx::IntSize frame_size)
{
    m_backing_store_manager.complete_rendering(bitmap_id);
    m_latest_rendered_surface = m_backing_store_manager.latest_rendered_surface();
    m_size_of_latest_rendered_frame = frame_size;
}

bool ContextPresenter::update_composited_raster_transform(Gfx::FloatRect destination_rect, Gfx::FloatMatrix4x4 const& transform)
{
    if (presents_to_client() || m_backing_store_manager.is_rendering() || m_viewport_size.is_empty() || destination_rect.is_empty())
        return false;

    // Display list coordinates already include device scale and page zoom. Only the embedding transform and
    // the viewport-to-destination mapping belong here; neither changes the child's layout or scroll geometry.
    auto has_perspective = transform[3, 0] != 0 || transform[3, 1] != 0 || transform[3, 3] != 1;
    float scale_x = 1;
    float scale_y = 1;
    if (!has_perspective) {
        scale_x = AK::hypot<float>(transform[0, 0], transform[1, 0]) * destination_rect.width() / m_viewport_size.width();
        scale_y = AK::hypot<float>(transform[0, 1], transform[1, 1]) * destination_rect.height() / m_viewport_size.height();
    }
    if (!isfinite(scale_x) || !isfinite(scale_y))
        return false;
    m_unbounded_raster_scale_of_embedding = { scale_x, scale_y };

    // Perspective keeps the native resolution until we can choose a scale from the visible projected region.
    auto scale = bounded_composited_raster_scale(m_unbounded_raster_scale_of_embedding, m_viewport_size);

    Gfx::FloatPoint translation;
    if (!has_perspective && transform[0, 1] == 0 && transform[1, 0] == 0 && scale.width() == scale_x && scale.height() == scale_y) {
        auto x = transform[0, 0] * destination_rect.x() + transform[0, 3];
        auto y = transform[1, 1] * destination_rect.y() + transform[1, 3];
        if (!isfinite(x) || !isfinite(y))
            return false;
        // Mirrored placements need the opposite fractional phase so the remaining image transform still
        // lands on integer device pixels.
        if (transform[0, 0] < 0)
            x = -x;
        if (transform[1, 1] < 0)
            y = -y;
        translation = { x - floorf(x), y - floorf(y) };
    }

    if (m_raster_scale == scale && m_raster_translation == translation)
        return false;
    m_raster_scale = scale;
    m_raster_translation = translation;
    m_latest_rendered_surface = nullptr;
    m_last_rasterized_frame.clear();
    return true;
}

CompositedContextSurface ContextPresenter::composited_surface() const
{
    if (!m_latest_rendered_surface || !m_last_rasterized_frame.has_value())
        return {};
    auto size = m_last_rasterized_frame->viewport_size.to_type<float>();
    size.scale_by(m_raster_scale.width(), m_raster_scale.height());
    return { m_latest_rendered_surface, { m_raster_translation, size } };
}

void ContextPresenter::queue_present_frame(PendingFrame pending_frame)
{
    if (!m_pending_present_frame.has_value()) {
        m_pending_present_frame = pending_frame;
        return;
    }

    if (m_pending_present_frame->viewport_rect.size() != pending_frame.viewport_rect.size()) {
        m_pending_present_frame = PendingFrame::repainting_everything(pending_frame.viewport_rect);
        return;
    }

    m_pending_present_frame->viewport_rect = pending_frame.viewport_rect;
    m_pending_present_frame->forced_damage_rect.unite(pending_frame.forced_damage_rect);
}

void ContextPresenter::mark_pending_present_frame_scheduled()
{
    m_pending_present_frame_scheduled = true;
}

bool ContextPresenter::can_schedule_pending_present_frame_if_unblocked() const
{
    if (is_present_blocked())
        return false;
    if (!m_pending_present_frame.has_value())
        return false;
    if (m_pending_present_frame_scheduled)
        return false;
    return true;
}

Optional<PendingFrame> ContextPresenter::take_pending_present_frame_if_unblocked()
{
    if (is_present_blocked())
        return {};
    m_pending_present_frame_scheduled = false;
    if (!m_pending_present_frame.has_value())
        return {};
    return m_pending_present_frame.release_value();
}

bool ContextPresenter::needs_rasterization() const
{
    return m_pending_present_frame.has_value()
        || (!m_latest_rendered_surface && m_presented_frame.has_value());
}

Optional<Gfx::IntRect> ContextPresenter::frame_rect_to_repaint() const
{
    if (m_pending_present_frame.has_value())
        return m_pending_present_frame->viewport_rect;
    return m_presented_frame;
}

Optional<Gfx::IntRect> ContextPresenter::self_present_rect() const
{
    if (m_presented_frame.has_value())
        return m_presented_frame;
    if (!m_viewport_size.is_empty())
        return Gfx::IntRect { {}, m_viewport_size };
    return {};
}

bool ContextPresenter::draws_canvas(Compositing::CanvasId canvas_id) const
{
    return m_last_rasterized_frame.has_value() && m_frame_planner.last_frame_drew_canvas(canvas_id);
}

Optional<PreparedFrame> ContextPresenter::prepare_frame(DisplayListPlayerSkia& display_list_player, PendingFrame pending_frame, Optional<FrameInputs>&& frame_inputs, CompositedContextResolver const* composited_context_resolver)
{
    if (is_present_blocked()) {
        queue_present_frame(pending_frame);
        return {};
    }

    if (!can_render_frame()) {
        m_presented_frame = pending_frame.viewport_rect;
        return {};
    }
    VERIFY(frame_inputs.has_value());

    auto plan = plan_frame(pending_frame, *frame_inputs);
    auto damage_rect = plan.damage_rect;
    // NB: A changed content rect must still reach the client even if the pixels are identical.
    if (damage_rect.is_empty() && m_presented_frame == pending_frame.viewport_rect) {
        auto only_window_server_content_changed = plan.only_window_server_content_changed;
        commit_frame_plan();
        remember_rasterized_frame(pending_frame.viewport_rect.size(), frame_inputs.release_value());
        if (!only_window_server_content_changed)
            return {};
        m_frame_without_store_in_flight = true;
        return PreparedFrame {
            .rendered_surface = nullptr,
            .bitmap_id = -1,
            .damage_rect = {},
            .device_pixel_ratio = m_device_pixel_ratio,
            .sublayers = sublayers_shown_by_window_server(),
        };
    }
    auto render_target = m_backing_store_manager.acquire_render_target(raster_damage_rect(damage_rect));
    if (!render_target.has_value()) {
        queue_present_frame(pending_frame);
        return {};
    }
    auto& back_store = render_target->surface;
    auto painted_rect = paint_frame(display_list_player, back_store, *frame_inputs, composited_context_resolver, render_target->damage_rect, plan);
    if (painted_rect != render_target->damage_rect)
        damage_rect = { {}, pending_frame.viewport_rect.size() };
    commit_frame_plan();
    remember_rasterized_frame(pending_frame.viewport_rect.size(), frame_inputs.release_value());

    return PreparedFrame {
        .rendered_surface = &back_store,
        .bitmap_id = render_target->bitmap_id,
        .damage_rect = damage_rect,
        .device_pixel_ratio = m_device_pixel_ratio,
        .sublayers = sublayers_shown_by_window_server(),
    };
}

void ContextPresenter::did_submit_prepared_frame(Gfx::IntRect viewport_rect)
{
    m_presented_frame = viewport_rect;
}

bool ContextPresenter::present_synchronously(DisplayListPlayerSkia& display_list_player, Optional<FrameInputs>&& frame_inputs, CompositedContextResolver const* composited_context_resolver)
{
    if (!can_render_frame())
        return false;
    VERIFY(frame_inputs.has_value());
    // Don't race an async present already in flight for this context; its own completion will update the output.
    if (is_present_blocked())
        return false;

    Optional<PendingFrame> pending_frame = m_pending_present_frame;
    if (!pending_frame.has_value() && m_presented_frame.has_value())
        pending_frame = PendingFrame::repainting_everything(*m_presented_frame);
    if (!pending_frame.has_value())
        return false;

    auto plan = plan_frame(*pending_frame, *frame_inputs);
    auto render_target = m_backing_store_manager.acquire_render_target(raster_damage_rect(plan.damage_rect));
    if (!render_target.has_value())
        return false;
    auto& back_store = render_target->surface;
    (void)paint_frame(display_list_player, back_store, *frame_inputs, composited_context_resolver, render_target->damage_rect, plan);
    commit_frame_plan();
    remember_rasterized_frame(pending_frame->viewport_rect.size(), frame_inputs.release_value());
    display_list_player.flush(back_store);
    m_backing_store_manager.complete_rendering(render_target->bitmap_id);
    m_latest_rendered_surface = m_backing_store_manager.latest_rendered_surface();
    m_size_of_latest_rendered_frame = pending_frame->viewport_rect.size();
    m_presented_frame = pending_frame->viewport_rect;
    m_pending_present_frame.clear();
    m_pending_present_frame_scheduled = false;
    return true;
}

void ContextPresenter::paint_screenshot(DisplayListPlayerSkia& display_list_player, Gfx::ShareableBitmap& target_bitmap, FrameInputs const& frame_inputs, CompositedContextResolver const* composited_context_resolver)
{
    auto target_surface = Gfx::PaintingSurface::wrap_bitmap(*target_bitmap.bitmap());
    paint_current_display_list(display_list_player, *target_surface, frame_inputs, composited_context_resolver, {}, PaintUIOverlay::No, false);
    display_list_player.flush(*target_surface);
}

static Gfx::IntSize size_fitting_within(Gfx::IntSize size, Gfx::IntSize maximum_size)
{
    if (size.width() <= maximum_size.width() && size.height() <= maximum_size.height())
        return size;
    auto scale = min(static_cast<double>(maximum_size.width()) / size.width(), static_cast<double>(maximum_size.height()) / size.height());
    return { max(1, static_cast<int>(round(size.width() * scale))), max(1, static_cast<int>(round(size.height() * scale))) };
}

Optional<Gfx::ShareableBitmap> ContextPresenter::snapshot_of_presented_frame(DisplayListPlayerSkia& display_list_player, Optional<Gfx::IntSize> maximum_size)
{
    if (!m_latest_rendered_surface)
        return {};
    NonnullRefPtr<Gfx::PaintingSurface> source_surface = *m_latest_rendered_surface;
    // The store then holds only what is under the canvases, videos and overlays that the window server shows above it.
    auto steps_compositing_window_server_content = m_frame_planner.steps_compositing_presented_frame_into_store();
    if (!steps_compositing_window_server_content.is_empty() && m_last_rasterized_frame.has_value()) {
        auto composited_or_error = Gfx::PaintingSurface::try_create_with_size(source_surface->size(), Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, source_surface->skia_backend_context());
        if (composited_or_error.is_error())
            return {};
        auto composited = composited_or_error.release_value();
        auto store_image = source_surface->sk_image_snapshot<sk_sp<SkImage>>();
        SkPaint paint;
        paint.setBlendMode(SkBlendMode::kSrc);
        composited->canvas().drawImage(store_image.get(), 0, 0, SkSamplingOptions {}, &paint);
        auto const& frame_inputs = m_last_rasterized_frame->frame_inputs;
        run_store_steps(display_list_player, *composited, frame_inputs, nullptr, steps_compositing_window_server_content, composited->rect());
        source_surface = composited;
    }
    auto const& source = *source_surface;
    Gfx::IntSize frame_size { min(m_size_of_latest_rendered_frame.width(), source.size().width()), min(m_size_of_latest_rendered_frame.height(), source.size().height()) };
    if (frame_size.is_empty())
        return {};
    auto snapshot_size = frame_size;
    if (maximum_size.has_value() && !maximum_size->is_empty())
        snapshot_size = size_fitting_within(frame_size, *maximum_size);

    auto bitmap_or_error = Gfx::Bitmap::create_shareable(Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, snapshot_size);
    if (bitmap_or_error.is_error())
        return {};
    auto bitmap = bitmap_or_error.release_value();

    if (snapshot_size == frame_size) {
        source.read_into_bitmap(*bitmap);
        return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
    }

    // A linear filter reads only the pixels around each one it writes, so a frame scaled down to a fraction of its size
    // in one step would skip most of them. Halving it first lets every pixel count.
    auto image = source.sk_image_snapshot<sk_sp<SkImage>>();
    auto image_size = frame_size;
    SkSamplingOptions const sampling { SkFilterMode::kLinear };
    auto draw_scaled = [&](Gfx::IntSize size) -> RefPtr<Gfx::PaintingSurface> {
        auto surface_or_error = Gfx::PaintingSurface::try_create_with_size(size, Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, source.skia_backend_context());
        if (surface_or_error.is_error())
            return nullptr;
        auto surface = surface_or_error.release_value();
        surface->canvas().drawImageRect(image.get(), SkRect::MakeIWH(image_size.width(), image_size.height()), SkRect::MakeIWH(size.width(), size.height()), sampling, nullptr, SkCanvas::kStrict_SrcRectConstraint);
        return surface;
    };
    while (image_size.width() / 2 >= snapshot_size.width() && image_size.height() / 2 >= snapshot_size.height()) {
        Gfx::IntSize half_size { image_size.width() / 2, image_size.height() / 2 };
        auto half = draw_scaled(half_size);
        if (!half)
            return {};
        image = half->sk_image_snapshot<sk_sp<SkImage>>();
        image_size = half_size;
    }
    auto snapshot_surface = draw_scaled(snapshot_size);
    if (!snapshot_surface)
        return {};
    snapshot_surface->read_into_bitmap(*bitmap);
    return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

void ContextPresenter::stop_backing_store_shrink_timer()
{
    if (!m_backing_store_shrink_timer)
        return;
    m_backing_store_shrink_timer->on_timeout = {};
    m_backing_store_shrink_timer->stop();
}

void ContextPresenter::stop_surplus_backing_store_retirement_timer()
{
    if (!m_surplus_backing_store_retirement_timer)
        return;
    m_surplus_backing_store_retirement_timer->on_timeout = {};
    m_surplus_backing_store_retirement_timer->stop();
}

void ContextPresenter::stop_idle_raster_slot_release_timer()
{
    if (!m_idle_raster_slot_release_timer)
        return;
    m_idle_raster_slot_release_timer->on_timeout = {};
    m_idle_raster_slot_release_timer->stop();
}

bool ContextPresenter::is_present_blocked() const
{
    return m_backing_store_manager.is_rendering()
        || m_frame_without_store_in_flight
        || !m_backing_store_manager.has_available_buffer();
}

bool ContextPresenter::can_render_frame() const
{
    return m_display_list && m_backing_store_manager.is_valid();
}

Gfx::IntSize ContextPresenter::raster_size() const
{
    if (m_viewport_size.is_empty())
        return {};
    if (presents_to_client())
        return m_viewport_size;
    return {
        static_cast<int>(ceilf(m_viewport_size.width() * m_raster_scale.width() + m_raster_translation.x())),
        static_cast<int>(ceilf(m_viewport_size.height() * m_raster_scale.height() + m_raster_translation.y())),
    };
}

Gfx::IntRect ContextPresenter::raster_damage_rect(Gfx::IntRect damage_rect) const
{
    if (damage_rect.is_empty())
        return {};
    if (damage_rect.contains(Gfx::IntRect { {}, m_viewport_size }))
        return { {}, raster_size() };
    auto rect = damage_rect.to_type<float>();
    rect.scale_by(m_raster_scale.width(), m_raster_scale.height());
    rect.translate_by(m_raster_translation);
    return Gfx::enclosing_int_rect(rect).intersected(Gfx::IntRect { {}, raster_size() });
}

FramePlan ContextPresenter::plan_frame(PendingFrame const& pending_frame, FrameInputs const& frame_inputs)
{
    Optional<LastFrame> last_frame;
    if (m_last_rasterized_frame.has_value()) {
        auto const& rasterized_frame = *m_last_rasterized_frame;
        last_frame = LastFrame {
            .frame_inputs = &rasterized_frame.frame_inputs,
            .display_list = rasterized_frame.display_list.ptr(),
            .viewport_size = rasterized_frame.viewport_size,
            .is_same_display_list = rasterized_frame.display_list.ptr() == m_display_list.ptr(),
            .has_other_surface_clear_color = rasterized_frame.display_list->surface_clear_color() != m_display_list->surface_clear_color(),
        };
    }
    FrameRequest request {
        .viewport_size = pending_frame.viewport_rect.size(),
        .forced_damage_rect = pending_frame.forced_damage_rect,
        .now = MonotonicTime::now(),
        .presents_to_client = presents_to_client(),
        .window_server_may_show_media = m_window_server_may_show_media,
        .window_server_may_show_high_dynamic_range = m_window_server_may_show_high_dynamic_range,
    };
    return m_frame_planner.plan(request, frame_inputs, *m_display_list, last_frame.has_value() ? &last_frame.value() : nullptr, sample_live_media());
}

LiveMediaSample ContextPresenter::sample_live_media()
{
    LiveMediaSample sample;
    auto sample_canvas = [&](u64 id) {
        Compositing::CanvasId canvas_id { id };
        if (any_of(sample.canvases, [&](auto const& canvas) { return canvas.id == canvas_id; }))
            return;
        bool has_window_server_surface = false;
#ifdef AK_OS_MACOS
        auto surface = m_canvas_surfaces.presentable_canvas_surface(canvas_id);
        has_window_server_surface = surface && surface->iosurface();
#endif
        sample.canvases.append({
            .id = canvas_id,
            .content_generation = m_canvas_surfaces.canvas_content_generation(canvas_id),
            .has_window_server_surface = has_window_server_surface,
        });
    };
    for (auto id : m_display_list->referenced_resource_ids().canvases)
        sample_canvas(id);
    if (m_last_rasterized_frame.has_value()) {
        for (auto id : m_last_rasterized_frame->display_list->referenced_resource_ids().canvases)
            sample_canvas(id);
    }

    for (auto const& [id, handle] : m_resource_storage.video_sink_handles()) {
        Compositing::VideoSinkResourceId video_sink_id { id };
        auto frame = current_video_frame(video_sink_id);
        bool frame_has_window_server_surface = false;
#ifdef AK_OS_MACOS
        // A frame that a decoder left in memory has no surface that the window server could read.
        frame_has_window_server_surface = frame && frame->surface();
#endif
        sample.videos.append({
            .id = video_sink_id,
            .has_frame = frame != nullptr,
            .frame_id = frame ? frame->id() : 0,
            .frame_has_window_server_surface = frame_has_window_server_surface,
            .frame_holds_high_dynamic_range = frame && video_frame_holds_high_dynamic_range(*frame),
        });
    }

    for (auto const& [id, pixel_generation] : m_resource_storage.image_frame_pixel_generations())
        sample.image_frames.append({ .id = Compositing::ImageFrameResourceId { id }, .pixel_generation = pixel_generation });
    return sample;
}

void ContextPresenter::commit_frame_plan()
{
    auto committed_frame = m_frame_planner.commit();
    if (!committed_frame.keeps_raster_slots) {
        release_raster_slots();
        return;
    }
    if (!m_idle_raster_slot_release_timer) {
        m_idle_raster_slot_release_timer = Core::Timer::create_single_shot(committed_frame.release_idle_raster_slots_after, [this] {
            if (m_frame_planner.release_idle_raster_slots())
                release_raster_slots();
        });
    }
    m_idle_raster_slot_release_timer->restart();
}

void ContextPresenter::release_raster_slots()
{
    m_raster_slots.clear();
    m_slot_presentations.clear();
    if (m_idle_raster_slot_release_timer)
        m_idle_raster_slot_release_timer->stop();
}

void ContextPresenter::remember_rasterized_frame(Gfx::IntSize viewport_size, FrameInputs&& frame_inputs)
{
    m_last_rasterized_frame = RasterizedFrame {
        .display_list = NonnullRefPtr { *m_display_list },
        .frame_inputs = move(frame_inputs),
        .viewport_size = viewport_size,
    };
}

RefPtr<Media::VideoFrame> ContextPresenter::current_video_frame(Compositing::VideoSinkResourceId video_sink_id)
{
    auto sink = m_resource_storage.video_sink(video_sink_id);
    if (!sink)
        return {};
    return sink->current_frame();
}

void ContextPresenter::paint_compositor_overlays(DisplayListPlayerSkia& display_list_player, Gfx::PaintingSurface& target_surface, FrameInputs const& frame_inputs, PaintUIOverlay paint_ui_overlay)
{
    display_list_player.paint_scrollbars_painted_by_compositor(target_surface, frame_inputs.scrollbars());
    if (paint_ui_overlay == PaintUIOverlay::No)
        return;

    if (m_ui_overlays.paused_debugger.has_value())
        paint_paused_debugger_overlay(target_surface, m_viewport_size, m_device_pixel_ratio, *m_ui_overlays.paused_debugger);
    if (auto opacity = m_picture_in_picture_controls_fade.opacity(); m_ui_overlays.picture_in_picture_controls.has_value() && opacity > 0)
        paint_picture_in_picture_controls(target_surface, m_viewport_size, m_device_pixel_ratio, *m_ui_overlays.picture_in_picture_controls, opacity);
}

Gfx::IntRect ContextPresenter::paint_frame(DisplayListPlayerSkia& display_list_player, Gfx::PaintingSurface& surface, FrameInputs const& frame_inputs, CompositedContextResolver const* composited_context_resolver, Gfx::IntRect damage_rect, FramePlan const& plan)
{
    if (plan.paints_from_raster_slots) {
        if (paint_from_raster_slots(display_list_player, surface, frame_inputs, composited_context_resolver, damage_rect, plan))
            return damage_rect;
        // The sublayers that the window server was to show go away with this frame, so the store has to hold all
        // that they showed.
        if (m_frame_planner.did_fail_to_allocate_raster_slot())
            damage_rect = { {}, raster_size() };
        release_raster_slots();
    }
    paint_current_display_list(display_list_player, surface, frame_inputs, composited_context_resolver, damage_rect);
    return damage_rect;
}

bool ContextPresenter::paint_from_raster_slots(DisplayListPlayerSkia& display_list_player, Gfx::PaintingSurface& surface, FrameInputs const& frame_inputs, CompositedContextResolver const* composited_context_resolver, Gfx::IntRect damage_rect, FramePlan const& plan)
{
    VERIFY(m_display_list);
    auto skia_backend_context = surface.skia_backend_context();
    auto surface_clear_color = Gfx::to_skia_color(m_display_list->surface_clear_color().value_or(Gfx::Color::Transparent));
    m_raster_cache.drop_clip_paths_not_used_since_last_frame();

    m_raster_slots.resize(plan.raster_slot_count);
    for (auto const& step : plan.steps) {
        if (step.kind == FrameStepKind::ReleaseSlot) {
            m_raster_slots[step.slot] = {};
            continue;
        }
        if (step.kind != FrameStepKind::RasterizeSlot)
            continue;
        auto& raster_slot = m_raster_slots[step.slot];
        auto rect = step.covers_viewport ? Gfx::IntRect { {}, m_viewport_size } : step.rect;
        if (rect.is_empty()) {
            raster_slot = {};
            continue;
        }
        auto repaints_whole_slot = step.repaints_whole_slot;
        auto surface_fits = step.covers_viewport ? raster_slot.rect == rect : raster_slot.rect.contains(rect);
        if (!raster_slot.surface || !surface_fits || raster_slot.surface->skia_backend_context() != skia_backend_context) {
            auto new_surface = Gfx::PaintingSurface::try_create_with_size(rect.size(), Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, skia_backend_context);
            if (new_surface.is_error())
                return false;
            raster_slot = { new_surface.release_value(), rect };
            repaints_whole_slot = true;
        }
        auto repaint_rect = repaints_whole_slot ? raster_slot.rect : step.repaint_rect.intersected(raster_slot.rect);
        if (repaint_rect.is_empty())
            continue;
        auto& slot_canvas = raster_slot.surface->canvas();
        slot_canvas.save();
        slot_canvas.translate(-raster_slot.rect.x(), -raster_slot.rect.y());
        slot_canvas.clipIRect(SkIRect::MakeXYWH(repaint_rect.x(), repaint_rect.y(), repaint_rect.width(), repaint_rect.height()));
        slot_canvas.clear(step.covers_viewport ? surface_clear_color : SK_ColorTRANSPARENT);
        display_list_player.execute(*m_display_list, frame_inputs.visual_context_tree(), m_resource_storage, m_raster_cache, &frame_inputs, *raster_slot.surface, &m_canvas_surfaces, composited_context_resolver, replaying_only(record_spans_of(step), step.skips_root_isolation_layer));
        slot_canvas.restore();
        raster_slot.content_version = ++m_last_raster_slot_content_version;
    }

    auto& canvas = surface.canvas();
    auto save_count = canvas.save();
    canvas.clipIRect(SkIRect::MakeXYWH(damage_rect.x(), damage_rect.y(), damage_rect.width(), damage_rect.height()));
    run_store_steps(display_list_player, surface, frame_inputs, composited_context_resolver, plan.steps, damage_rect);
    canvas.restoreToCount(save_count);

    auto presents_to_window_server = any_of(plan.steps, [](auto const& step) {
        return step.kind == FrameStepKind::PresentSlotToWindowServer || step.kind == FrameStepKind::WithdrawSlotFromWindowServer;
    });
    if (presents_to_window_server)
        m_slot_presentations.resize(plan.raster_slot_count);
    for (auto const& step : plan.steps) {
        switch (step.kind) {
        case FrameStepKind::PresentSlotToWindowServer:
            if (!present_slot_to_window_server(step, skia_backend_context))
                return false;
            break;
        case FrameStepKind::WithdrawSlotFromWindowServer:
            m_slot_presentations[step.slot].latest_surface = nullptr;
            break;
        case FrameStepKind::PresentCompositorOverlaysToWindowServer:
            if (!present_compositor_overlays_to_window_server(display_list_player, frame_inputs, skia_backend_context))
                return false;
            break;
        default:
            break;
        }
    }
    return true;
}

void ContextPresenter::run_store_steps(DisplayListPlayerSkia& display_list_player, Gfx::PaintingSurface& surface, FrameInputs const& frame_inputs, CompositedContextResolver const* composited_context_resolver, ReadonlySpan<FrameStep> steps, Gfx::IntRect damage_rect)
{
    auto& canvas = surface.canvas();
    for (auto const& step : steps) {
        switch (step.kind) {
        case FrameStepKind::DrawSlotIntoStore: {
            if (step.slot >= m_raster_slots.size())
                break;
            auto const& raster_slot = m_raster_slots[step.slot];
            if (!raster_slot.surface || !raster_slot.rect.intersects(damage_rect))
                break;
            auto image = raster_slot.surface->sk_image_snapshot<sk_sp<SkImage>>();
            SkPaint paint;
            paint.setBlendMode(step.blend == SlotBlend::Source ? SkBlendMode::kSrc : SkBlendMode::kSrcOver);
            canvas.drawImage(image.get(), raster_slot.rect.x(), raster_slot.rect.y(), SkSamplingOptions {}, &paint);
            break;
        }
        case FrameStepKind::ReplaySlotRecordsIntoStore:
            if (step.rect.intersects(damage_rect))
                display_list_player.execute(*m_display_list, frame_inputs.visual_context_tree(), m_resource_storage, m_raster_cache, &frame_inputs, surface, &m_canvas_surfaces, composited_context_resolver, replaying_only(record_spans_of(step), step.skips_root_isolation_layer));
            break;
        case FrameStepKind::PaintCompositorOverlaysIntoStore:
            paint_compositor_overlays(display_list_player, surface, frame_inputs, PaintUIOverlay::Yes);
            break;
        default:
            break;
        }
    }
}

// Each raster slot that the window server shows goes to it as its own surface, which is copied from the slot when the
// slot changes. The window server may still show the surface of the frame before, so the copy goes into another one.
bool ContextPresenter::present_slot_to_window_server(FrameStep const& step, RefPtr<Gfx::SkiaBackendContext> const& skia_backend_context)
{
    auto& presentation = m_slot_presentations[step.slot];
    auto const* raster_slot = step.slot < m_raster_slots.size() ? &m_raster_slots[step.slot] : nullptr;
    if (!raster_slot || !raster_slot->surface) {
        presentation.latest_surface = nullptr;
        return true;
    }
    if (presentation.latest_surface && presentation.rect == step.rect && presentation.content_version == raster_slot->content_version)
        return true;
    auto surface_or_error = presentation.surfaces.surface_for_next_content(step.rect.size(), skia_backend_context);
    if (surface_or_error.is_error())
        return false;
    auto surface = surface_or_error.release_value();
    auto image = raster_slot->surface->sk_image_snapshot<sk_sp<SkImage>>();
    auto source_rect = step.rect.translated(-raster_slot->rect.location());
    SkPaint paint;
    paint.setBlendMode(SkBlendMode::kSrc);
    surface->painting_surface().canvas().drawImageRect(image.get(), Gfx::to_skia_rect(source_rect), SkRect::MakeIWH(step.rect.width(), step.rect.height()), SkSamplingOptions {}, &paint, SkCanvas::kStrict_SrcRectConstraint);
    presentation.latest_surface = move(surface);
    presentation.rect = step.rect;
    presentation.content_version = raster_slot->content_version;
    return true;
}

Gfx::IntRect ContextPresenter::compositor_overlay_bounds(FrameInputs const& frame_inputs) const
{
    Gfx::IntRect viewport_rect { {}, m_viewport_size };
    if (m_ui_overlays.paused_debugger.has_value())
        return viewport_rect;
    if (m_ui_overlays.picture_in_picture_controls.has_value() && m_picture_in_picture_controls_fade.opacity() > 0)
        return viewport_rect;
    Gfx::IntRect bounds;
    for (auto const& scrollbar : frame_inputs.scrollbars()) {
        if (!scrollbar.is_painted_by_compositor || scrollbar.opacity <= 0)
            continue;
        // The thumb's outline is drawn around the gutter.
        bounds.unite(Gfx::enclosing_int_rect(scrollbar.gutter_rect).united(scrollbar.thin_and_expanded_gutter_rect).inflated(2, 2));
    }
    return bounds.intersected(viewport_rect);
}

bool ContextPresenter::present_compositor_overlays_to_window_server(DisplayListPlayerSkia& display_list_player, FrameInputs const& frame_inputs, RefPtr<Gfx::SkiaBackendContext> const& skia_backend_context)
{
    auto bounds = compositor_overlay_bounds(frame_inputs);
    m_compositor_overlay_rect = bounds;
    if (bounds.is_empty()) {
        m_latest_compositor_overlay_surface = nullptr;
        return true;
    }
    auto surface_or_error = m_compositor_overlay_surfaces.surface_for_next_content(bounds.size(), skia_backend_context);
    if (surface_or_error.is_error())
        return false;
    auto surface = surface_or_error.release_value();
    auto& canvas = surface->painting_surface().canvas();
    canvas.clear(SK_ColorTRANSPARENT);
    auto save_count = canvas.save();
    canvas.translate(-bounds.x(), -bounds.y());
    paint_compositor_overlays(display_list_player, surface->painting_surface(), frame_inputs, PaintUIOverlay::Yes);
    canvas.restoreToCount(save_count);
    m_latest_compositor_overlay_surface = move(surface);
    return true;
}

static RemoteSublayer sublayer_placed_as_described(SublayerDescription const& description, Variant<NonnullRefPtr<Media::VideoFrame>, PresentableSurface::Lease> content)
{
    auto const& placement = description.placement;
    auto const* transform = placement.content_to_viewport;
    Optional<RemoteSublayer::RoundedClip> rounded_clip;
    if (placement.has_rounded_clip) {
        rounded_clip = RemoteSublayer::RoundedClip {
            .rect = placement.rounded_clip_rect,
            .radius = placement.rounded_clip_radius,
            .rounds_top_left = placement.rounds_top_left,
            .rounds_top_right = placement.rounds_top_right,
            .rounds_bottom_right = placement.rounds_bottom_right,
            .rounds_bottom_left = placement.rounds_bottom_left,
        };
    }
    return {
        .identity = description.identity,
        .content = move(content),
        .content_rect = placement.content_rect,
        .content_to_frame = Gfx::AffineTransform { transform[0], transform[1], transform[2], transform[3], transform[4], transform[5] },
        .clip_rect = placement.clip_rect,
        .rounded_clip = rounded_clip,
        .scaling_mode = placement.scaling_mode,
    };
}

static RemoteSublayer sublayer_showing_whole_surface(u64 identity, PresentableSurface::Lease content, Gfx::IntRect rect)
{
    return {
        .identity = identity,
        .content = move(content),
        .content_rect = rect,
        .content_to_frame = {},
        .clip_rect = rect.to_type<float>(),
        .rounded_clip = {},
        .scaling_mode = Gfx::ScalingMode::NearestNeighbor,
    };
}

Vector<RemoteSublayer> ContextPresenter::sublayers_shown_by_window_server()
{
    Vector<RemoteSublayer> sublayers;
    for (auto const& description : m_frame_planner.sublayers()) {
        switch (description.content_kind) {
        case SublayerContentKind::Canvas:
            if (auto surface = m_canvas_surfaces.presentable_canvas_surface(Compositing::CanvasId { description.media_id }))
                sublayers.append(sublayer_placed_as_described(description, surface->lease()));
            break;
        case SublayerContentKind::Video:
            if (auto frame = current_video_frame(Compositing::VideoSinkResourceId { description.media_id }); frame && frame->surface())
                sublayers.append(sublayer_placed_as_described(description, frame.release_nonnull()));
            break;
        case SublayerContentKind::RasterSlot:
            if (description.raster_slot < m_slot_presentations.size() && m_slot_presentations[description.raster_slot].latest_surface) {
                auto const& presentation = m_slot_presentations[description.raster_slot];
                sublayers.append(sublayer_showing_whole_surface(description.identity, presentation.latest_surface->lease(), presentation.rect));
            }
            break;
        case SublayerContentKind::CompositorOverlays:
            if (m_latest_compositor_overlay_surface)
                sublayers.append(sublayer_showing_whole_surface(description.identity, m_latest_compositor_overlay_surface->lease(), m_compositor_overlay_rect));
            break;
        }
    }
    return sublayers;
}

void ContextPresenter::paint_current_display_list(DisplayListPlayerSkia& display_list_player, Gfx::PaintingSurface& surface, FrameInputs const& frame_inputs, CompositedContextResolver const* composited_context_resolver, Optional<Gfx::IntRect> damage_rect, PaintUIOverlay paint_ui_overlay, bool apply_raster_transform)
{
    VERIFY(m_display_list);
    auto surface_clear_color = Gfx::to_skia_color(m_display_list->surface_clear_color().value_or(Gfx::Color::Transparent));
    auto paint_display_list = [&](Gfx::PaintingSurface& target_surface) {
        display_list_player.execute(
            *m_display_list,
            frame_inputs.visual_context_tree(),
            m_resource_storage,
            m_raster_cache,
            &frame_inputs,
            target_surface,
            &m_canvas_surfaces,
            composited_context_resolver);
        paint_compositor_overlays(display_list_player, target_surface, frame_inputs, paint_ui_overlay);
    };

    if (damage_rect.has_value() && !damage_rect->is_empty() && presents_to_client() && damage_rect->size() != surface.size() && surface.skia_backend_context()) {
        if (!m_damage_surface || m_damage_surface->size() != damage_rect->size()) {
            m_damage_surface = Gfx::PaintingSurface::create_with_size(
                damage_rect->size(), Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, surface.skia_backend_context());
        }

        auto& damage_canvas = m_damage_surface->canvas();
        damage_canvas.clear(surface_clear_color);
        damage_canvas.save();
        damage_canvas.translate(-damage_rect->x(), -damage_rect->y());
        paint_display_list(*m_damage_surface);
        damage_canvas.restore();

        auto image = m_damage_surface->sk_image_snapshot<sk_sp<SkImage>>();
        SkPaint paint;
        paint.setBlendMode(SkBlendMode::kSrc);
        surface.canvas().drawImageRect(
            image.get(),
            SkRect::MakeWH(damage_rect->width(), damage_rect->height()),
            SkRect::MakeXYWH(damage_rect->x(), damage_rect->y(), damage_rect->width(), damage_rect->height()),
            SkSamplingOptions {},
            &paint,
            SkCanvas::kStrict_SrcRectConstraint);
        return;
    }

    auto& canvas = surface.canvas();
    auto save_count = canvas.save();
    if (damage_rect.has_value()) {
        canvas.clipIRect(SkIRect::MakeXYWH(damage_rect->x(), damage_rect->y(), damage_rect->width(), damage_rect->height()));
        canvas.clear(surface_clear_color);
    }
    if (apply_raster_transform) {
        canvas.translate(m_raster_translation.x(), m_raster_translation.y());
        canvas.scale(m_raster_scale.width(), m_raster_scale.height());
    }
    paint_display_list(surface);
    canvas.restoreToCount(save_count);
}

}
