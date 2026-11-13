/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::{
    CanvasSample, FramePlanner, FrameRequest, FrameStep, ImageFrameSample, LastFrame, LiveMediaSample, SlotBlend,
    SlotRepaint, Sublayer, SublayerContent, VideoSample,
};
use crate::frame_inputs::FrameInputs;
use libcompositing_rust::display_list::commands::{CanvasId, ImageFrameResourceId, VideoSinkResourceId};
use libcompositing_rust::display_list::layers::LayerPlan;
use libcompositing_rust::display_list::storage::storage_from_handle;
use libcompositing_rust::display_list::window_server_placement::WindowServerPlacement;
use libcompositing_rust::host::RecordSpan;
use libcompositing_rust::time::MonotonicTime;
use libgfx_rust::{FloatRect, IntRect, IntSize, ScalingMode};
use std::ffi::c_void;

pub struct ContextFramePlanner {
    planner: FramePlanner,
    steps: Vec<FfiFrameStep>,
    steps_compositing_presented_frame: Vec<FfiFrameStep>,
    sublayers: Vec<FfiSublayer>,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFrameRequest {
    pub viewport_size: IntSize,
    pub forced_damage_rect: IntRect,
    pub now_nanoseconds: i64,
    pub presents_to_client: bool,
    pub window_server_may_show_media: bool,
    pub window_server_may_show_high_dynamic_range: bool,
}

#[repr(C)]
pub struct FfiLastFrame<'a> {
    pub frame_inputs: &'a FrameInputs,
    pub display_list_storage: *const c_void,
    pub viewport_size: IntSize,
    pub is_same_display_list: bool,
    pub has_other_surface_clear_color: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiCanvasSample {
    pub id: CanvasId,
    pub content_generation: u64,
    pub has_window_server_surface: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiVideoSample {
    pub id: VideoSinkResourceId,
    pub has_frame: bool,
    pub frame_id: u64,
    pub frame_has_window_server_surface: bool,
    pub frame_holds_high_dynamic_range: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiImageFrameSample {
    pub id: ImageFrameResourceId,
    pub pixel_generation: u64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiLiveMediaSample {
    pub canvases: *const FfiCanvasSample,
    pub canvas_count: usize,
    pub videos: *const FfiVideoSample,
    pub video_count: usize,
    pub image_frames: *const FfiImageFrameSample,
    pub image_frame_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiFrameStepKind {
    RasterizeSlot,
    ReleaseSlot,
    DrawSlotIntoStore,
    ReplaySlotRecordsIntoStore,
    PaintCompositorOverlaysIntoStore,
    PresentSlotToWindowServer,
    WithdrawSlotFromWindowServer,
    PresentCompositorOverlaysToWindowServer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiSlotBlend {
    Source,
    SourceOver,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFrameStep {
    pub kind: FfiFrameStepKind,
    pub slot: usize,
    pub rect: IntRect,
    pub covers_viewport: bool,
    pub repaints_whole_slot: bool,
    pub repaint_rect: IntRect,
    pub blend: FfiSlotBlend,
    pub record_spans: *const RecordSpan,
    pub record_span_count: usize,
    pub skips_root_isolation_layer: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFramePlan {
    pub damage_rect: IntRect,
    pub paints_from_raster_slots: bool,
    pub raster_slot_count: usize,
    pub only_window_server_content_changed: bool,
    pub steps: *const FfiFrameStep,
    pub step_count: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiCommittedFrame {
    pub keeps_raster_slots: bool,
    pub release_idle_raster_slots_after_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiSublayerContentKind {
    Canvas,
    Video,
    RasterSlot,
    CompositorOverlays,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiWindowServerPlacement {
    pub content_rect: IntRect,
    pub content_to_viewport: [f32; 6],
    pub clip_rect: FloatRect,
    pub has_rounded_clip: bool,
    pub rounded_clip_rect: FloatRect,
    pub rounded_clip_radius: f32,
    pub rounds_top_left: bool,
    pub rounds_top_right: bool,
    pub rounds_bottom_right: bool,
    pub rounds_bottom_left: bool,
    pub scaling_mode: ScalingMode,
}

impl FfiWindowServerPlacement {
    fn of(placement: &WindowServerPlacement) -> Self {
        let rounded_clip = placement.rounded_clip.unwrap_or(
            libcompositing_rust::display_list::window_server_placement::RoundedClip {
                rect: FloatRect::default(),
                radius: 0.0,
                rounded_corners: Default::default(),
            },
        );
        Self {
            content_rect: placement.content_rect,
            content_to_viewport: placement.content_to_viewport.values,
            clip_rect: placement.clip_rect,
            has_rounded_clip: placement.rounded_clip.is_some(),
            rounded_clip_rect: rounded_clip.rect,
            rounded_clip_radius: rounded_clip.radius,
            rounds_top_left: rounded_clip.rounded_corners.top_left,
            rounds_top_right: rounded_clip.rounded_corners.top_right,
            rounds_bottom_right: rounded_clip.rounded_corners.bottom_right,
            rounds_bottom_left: rounded_clip.rounded_corners.bottom_left,
            scaling_mode: placement.scaling_mode,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiSublayer {
    pub identity: u64,
    pub content_kind: FfiSublayerContentKind,
    pub media_id: u64,
    pub raster_slot: usize,
    pub placement: FfiWindowServerPlacement,
}

impl FfiSublayer {
    fn of(sublayer: &Sublayer) -> Self {
        let (content_kind, media_id, raster_slot) = match sublayer.content {
            SublayerContent::Canvas(id) => (FfiSublayerContentKind::Canvas, id.0, 0),
            SublayerContent::Video(id) => (FfiSublayerContentKind::Video, id.0, 0),
            SublayerContent::RasterSlot(slot) => (FfiSublayerContentKind::RasterSlot, 0, slot),
            SublayerContent::CompositorOverlays => (FfiSublayerContentKind::CompositorOverlays, 0, 0),
        };
        Self {
            identity: sublayer.identity,
            content_kind,
            media_id,
            raster_slot,
            placement: sublayer
                .placement
                .as_ref()
                .map(FfiWindowServerPlacement::of)
                .unwrap_or_default(),
        }
    }
}

fn ffi_step(step: &FrameStep, layer_plan: &LayerPlan) -> FfiFrameStep {
    let mut ffi_step = FfiFrameStep {
        kind: FfiFrameStepKind::ReleaseSlot,
        slot: 0,
        rect: IntRect::default(),
        covers_viewport: false,
        repaints_whole_slot: false,
        repaint_rect: IntRect::default(),
        blend: FfiSlotBlend::Source,
        record_spans: std::ptr::null(),
        record_span_count: 0,
        skips_root_isolation_layer: false,
    };
    let records_of_slot = |slot: usize| {
        let record_spans = &layer_plan.layers[slot].record_spans;
        (record_spans.as_ptr(), record_spans.len())
    };
    match *step {
        FrameStep::RasterizeSlot {
            slot,
            rect,
            covers_viewport,
            repaint,
        } => {
            ffi_step.kind = FfiFrameStepKind::RasterizeSlot;
            ffi_step.slot = slot;
            ffi_step.rect = rect;
            ffi_step.covers_viewport = covers_viewport;
            match repaint {
                SlotRepaint::Whole => ffi_step.repaints_whole_slot = true,
                SlotRepaint::Within(repaint_rect) => ffi_step.repaint_rect = repaint_rect,
            }
            (ffi_step.record_spans, ffi_step.record_span_count) = records_of_slot(slot);
        }
        FrameStep::ReleaseSlot { slot } => {
            ffi_step.kind = FfiFrameStepKind::ReleaseSlot;
            ffi_step.slot = slot;
        }
        FrameStep::DrawSlotIntoStore { slot, blend } => {
            ffi_step.kind = FfiFrameStepKind::DrawSlotIntoStore;
            ffi_step.slot = slot;
            ffi_step.blend = match blend {
                SlotBlend::Source => FfiSlotBlend::Source,
                SlotBlend::SourceOver => FfiSlotBlend::SourceOver,
            };
        }
        FrameStep::ReplaySlotRecordsIntoStore { slot, rect } => {
            ffi_step.kind = FfiFrameStepKind::ReplaySlotRecordsIntoStore;
            ffi_step.slot = slot;
            ffi_step.rect = rect;
            ffi_step.skips_root_isolation_layer = true;
            (ffi_step.record_spans, ffi_step.record_span_count) = records_of_slot(slot);
        }
        FrameStep::PaintCompositorOverlaysIntoStore => {
            ffi_step.kind = FfiFrameStepKind::PaintCompositorOverlaysIntoStore;
        }
        FrameStep::PresentSlotToWindowServer { slot, rect } => {
            ffi_step.kind = FfiFrameStepKind::PresentSlotToWindowServer;
            ffi_step.slot = slot;
            ffi_step.rect = rect;
        }
        FrameStep::WithdrawSlotFromWindowServer { slot } => {
            ffi_step.kind = FfiFrameStepKind::WithdrawSlotFromWindowServer;
            ffi_step.slot = slot;
        }
        FrameStep::PresentCompositorOverlaysToWindowServer => {
            ffi_step.kind = FfiFrameStepKind::PresentCompositorOverlaysToWindowServer;
        }
    }
    ffi_step
}

fn ffi_steps(steps: &[FrameStep], layer_plan: Option<&LayerPlan>) -> Vec<FfiFrameStep> {
    match layer_plan {
        Some(layer_plan) => steps.iter().map(|step| ffi_step(step, layer_plan)).collect(),
        None => Vec::new(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_create() -> *mut ContextFramePlanner {
    Box::into_raw(Box::new(ContextFramePlanner {
        planner: FramePlanner::default(),
        steps: Vec::new(),
        steps_compositing_presented_frame: Vec::new(),
        sublayers: Vec::new(),
    }))
}

/// # Safety
///
/// `planner` must be a planner `compositor_frame_planner_create` made that nothing uses afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compositor_frame_planner_destroy(planner: *mut ContextFramePlanner) {
    // SAFETY: The caller hands over the planner.
    drop(unsafe { Box::from_raw(planner) });
}

/// Decides what the frame repaints and from which raster slots, and writes that through `frame_plan`. The steps of the
/// plan stay valid until the next call to the planner.
///
/// # Safety
///
/// `display_list_storage` and the storage of `last_frame` must be live storage handles of lists recorded against the
/// trees of their frame inputs, and the sample must address the stated number of values for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compositor_frame_planner_plan(
    planner: &mut ContextFramePlanner,
    request: &FfiFrameRequest,
    frame_inputs: &FrameInputs,
    display_list_storage: *const c_void,
    last_frame: Option<&FfiLastFrame<'_>>,
    sample: &FfiLiveMediaSample,
    frame_plan: &mut FfiFramePlan,
) {
    // SAFETY: The caller guarantees live storage handles and valid slices for the call.
    let (display_list, last_frame, canvases, videos, image_frames) = unsafe {
        (
            storage_from_handle(display_list_storage),
            last_frame.map(|last_frame| LastFrame {
                frame_inputs: last_frame.frame_inputs,
                display_list: storage_from_handle(last_frame.display_list_storage),
                viewport_size: last_frame.viewport_size,
                is_same_display_list: last_frame.is_same_display_list,
                has_other_surface_clear_color: last_frame.has_other_surface_clear_color,
            }),
            ak::slice_from_raw_parts(sample.canvases, sample.canvas_count),
            ak::slice_from_raw_parts(sample.videos, sample.video_count),
            ak::slice_from_raw_parts(sample.image_frames, sample.image_frame_count),
        )
    };
    let sample = LiveMediaSample {
        canvases: canvases
            .iter()
            .map(|canvas| CanvasSample {
                id: canvas.id,
                content_generation: canvas.content_generation,
                has_window_server_surface: canvas.has_window_server_surface,
            })
            .collect(),
        videos: videos
            .iter()
            .map(|video| VideoSample {
                id: video.id,
                frame_id: video.has_frame.then_some(video.frame_id),
                frame_has_window_server_surface: video.frame_has_window_server_surface,
                frame_holds_high_dynamic_range: video.frame_holds_high_dynamic_range,
            })
            .collect(),
        image_frames: image_frames
            .iter()
            .map(|image_frame| ImageFrameSample {
                id: image_frame.id,
                pixel_generation: image_frame.pixel_generation,
            })
            .collect(),
    };
    let request = FrameRequest {
        viewport_size: request.viewport_size,
        forced_damage_rect: request.forced_damage_rect,
        now: MonotonicTime::from_nanoseconds(request.now_nanoseconds),
        presents_to_client: request.presents_to_client,
        window_server_may_show_media: request.window_server_may_show_media,
        window_server_may_show_high_dynamic_range: request.window_server_may_show_high_dynamic_range,
    };
    planner
        .planner
        .plan(&request, frame_inputs, display_list, last_frame.as_ref(), sample);
    let plan = planner.planner.frame_plan();
    planner.steps = ffi_steps(&plan.steps, planner.planner.frame_layer_plan());
    *frame_plan = FfiFramePlan {
        damage_rect: plan.damage_rect,
        paints_from_raster_slots: plan.paints_from_raster_slots,
        raster_slot_count: plan.raster_slot_count,
        only_window_server_content_changed: plan.only_window_server_content_changed,
        steps: planner.steps.as_ptr(),
        step_count: planner.steps.len(),
    };
}

/// Gives up the raster slots of the planned frame, which then paints without them. Answers whether the store then has
/// to be painted whole.
#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_did_fail_to_allocate_raster_slot(planner: &mut ContextFramePlanner) -> bool {
    planner.steps.clear();
    planner.planner.did_fail_to_allocate_raster_slot()
}

#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_commit(planner: &mut ContextFramePlanner) -> FfiCommittedFrame {
    planner.steps.clear();
    let release_idle_raster_slots_after = planner.planner.commit();
    planner.sublayers = planner.planner.sublayers().iter().map(FfiSublayer::of).collect();
    FfiCommittedFrame {
        keeps_raster_slots: release_idle_raster_slots_after.is_some(),
        release_idle_raster_slots_after_nanoseconds: release_idle_raster_slots_after
            .map_or(0, |duration| duration.nanoseconds()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_release_idle_raster_slots(planner: &mut ContextFramePlanner) -> bool {
    let released = planner.planner.release_idle_layers();
    if released {
        planner.sublayers.clear();
    }
    released
}

#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_sublayers(
    planner: &ContextFramePlanner,
    count: &mut usize,
) -> *const FfiSublayer {
    *count = planner.sublayers.len();
    planner.sublayers.as_ptr()
}

/// The steps that composite what the window server shows above the presented frame into a copy of its store. They
/// stay valid until the next call to the planner.
#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_steps_compositing_presented_frame_into_store(
    planner: &mut ContextFramePlanner,
    count: &mut usize,
) -> *const FfiFrameStep {
    let steps = planner.planner.steps_compositing_presented_frame_into_store();
    planner.steps_compositing_presented_frame = ffi_steps(&steps, planner.planner.committed_layer_plan());
    *count = planner.steps_compositing_presented_frame.len();
    planner.steps_compositing_presented_frame.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn compositor_frame_planner_last_frame_drew_canvas(planner: &ContextFramePlanner, id: CanvasId) -> bool {
    planner.planner.last_frame_drew_canvas(id)
}
