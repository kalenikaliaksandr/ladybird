/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/FramePlanner.h>

namespace Compositor {

FramePlanner::FramePlanner()
    : m_planner(RustFFI::compositor_frame_planner_create())
{
}

FramePlanner::~FramePlanner()
{
    RustFFI::compositor_frame_planner_destroy(m_planner);
}

FramePlan FramePlanner::plan(FrameRequest const& request, FrameInputs const& frame_inputs, Compositing::DisplayList const& display_list, LastFrame const* last_frame, LiveMediaSample const& sample)
{
    VERIFY(display_list.compatible_visual_context_tree_structural_epoch() == frame_inputs.visual_context_tree().structural_epoch());
    RustFFI::FfiFrameRequest ffi_request {
        .viewport_size = request.viewport_size,
        .forced_damage_rect = request.forced_damage_rect,
        .now_nanoseconds = request.now.nanoseconds(),
        .presents_to_client = request.presents_to_client,
        .window_server_may_show_media = request.window_server_may_show_media,
        .window_server_may_show_high_dynamic_range = request.window_server_may_show_high_dynamic_range,
    };
    Optional<RustFFI::FfiLastFrame> ffi_last_frame;
    if (last_frame) {
        ffi_last_frame = RustFFI::FfiLastFrame {
            .frame_inputs = &last_frame->frame_inputs->rust_handle(),
            .display_list_storage = last_frame->display_list->rust_handle(),
            .viewport_size = last_frame->viewport_size,
            .is_same_display_list = last_frame->is_same_display_list,
            .has_other_surface_clear_color = last_frame->has_other_surface_clear_color,
        };
    }
    RustFFI::FfiLiveMediaSample ffi_sample {
        .canvases = sample.canvases.data(),
        .canvas_count = sample.canvases.size(),
        .videos = sample.videos.data(),
        .video_count = sample.videos.size(),
        .image_frames = sample.image_frames.data(),
        .image_frame_count = sample.image_frames.size(),
    };
    RustFFI::FfiFramePlan plan {};
    RustFFI::compositor_frame_planner_plan(m_planner, &ffi_request, &frame_inputs.rust_handle(), display_list.rust_handle(), ffi_last_frame.has_value() ? &ffi_last_frame.value() : nullptr, &ffi_sample, &plan);
    return {
        .damage_rect = plan.damage_rect,
        .paints_from_raster_slots = plan.paints_from_raster_slots,
        .raster_slot_count = plan.raster_slot_count,
        .only_window_server_content_changed = plan.only_window_server_content_changed,
        .steps = { plan.steps, plan.step_count },
    };
}

bool FramePlanner::did_fail_to_allocate_raster_slot()
{
    return RustFFI::compositor_frame_planner_did_fail_to_allocate_raster_slot(m_planner);
}

CommittedFrame FramePlanner::commit()
{
    auto committed_frame = RustFFI::compositor_frame_planner_commit(m_planner);
    return {
        .keeps_raster_slots = committed_frame.keeps_raster_slots,
        .release_idle_raster_slots_after = AK::Duration::from_nanoseconds(committed_frame.release_idle_raster_slots_after_nanoseconds),
    };
}

bool FramePlanner::release_idle_raster_slots()
{
    return RustFFI::compositor_frame_planner_release_idle_raster_slots(m_planner);
}

ReadonlySpan<SublayerDescription> FramePlanner::sublayers() const
{
    size_t count = 0;
    auto const* sublayers = RustFFI::compositor_frame_planner_sublayers(m_planner, &count);
    return { sublayers, count };
}

ReadonlySpan<FrameStep> FramePlanner::steps_compositing_presented_frame_into_store()
{
    size_t count = 0;
    auto const* steps = RustFFI::compositor_frame_planner_steps_compositing_presented_frame_into_store(m_planner, &count);
    return { steps, count };
}

bool FramePlanner::last_frame_drew_canvas(Compositing::CanvasId canvas_id) const
{
    return RustFFI::compositor_frame_planner_last_frame_drew_canvas(m_planner, canvas_id);
}

}
