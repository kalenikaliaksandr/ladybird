/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub mod ffi;
mod media_activity;
#[cfg(test)]
mod tests;

use crate::frame_inputs::FrameInputs;
use libcompositing_rust::display_list::commands::{CanvasId, ImageFrameResourceId, VideoSinkResourceId};
use libcompositing_rust::display_list::damage::{DisplayListDamage, LayerPlans, LiveContentChanges};
use libcompositing_rust::display_list::layers::{LayerKind, LayerPlan, LayerPlanInputs, build_layer_plan};
use libcompositing_rust::display_list::storage::DisplayListStorage;
use libcompositing_rust::display_list::summary::LiveContent;
use libcompositing_rust::display_list::window_server_placement::WindowServerPlacement;
use libcompositing_rust::time::{Duration, MonotonicTime};
use libgfx_rust::{IntRect, IntSize};
pub use media_activity::{MediaActivity, MediaUpdate, TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasSample {
    pub id: CanvasId,
    pub content_generation: u64,
    pub has_window_server_surface: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSample {
    pub id: VideoSinkResourceId,
    pub frame_id: Option<u64>,
    pub frame_has_window_server_surface: bool,
    pub frame_holds_high_dynamic_range: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFrameSample {
    pub id: ImageFrameResourceId,
    pub pixel_generation: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveMediaSample {
    pub canvases: Vec<CanvasSample>,
    pub videos: Vec<VideoSample>,
    pub image_frames: Vec<ImageFrameSample>,
}

impl LiveMediaSample {
    fn canvas(&self, id: CanvasId) -> Option<&CanvasSample> {
        self.canvases.iter().find(|canvas| canvas.id == id)
    }

    fn video(&self, id: VideoSinkResourceId) -> Option<&VideoSample> {
        self.videos.iter().find(|video| video.id == id)
    }

    fn video_frame_id(&self, id: VideoSinkResourceId) -> Option<u64> {
        self.video(id).and_then(|video| video.frame_id)
    }

    fn image_frame_pixel_generation(&self, id: ImageFrameResourceId) -> Option<u64> {
        self.image_frames
            .iter()
            .find(|image_frame| image_frame.id == id)
            .map(|image_frame| image_frame.pixel_generation)
    }

    fn kept_for_list(mut self, display_list: &DisplayListStorage) -> Self {
        let canvases_drawn_by_list = &display_list.summary().canvas_ids;
        self.canvases
            .retain(|canvas| canvases_drawn_by_list.contains(&canvas.id.0));
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameRequest {
    pub viewport_size: IntSize,
    pub forced_damage_rect: IntRect,
    pub now: MonotonicTime,
    pub presents_to_client: bool,
    pub window_server_may_show_media: bool,
    pub window_server_may_show_high_dynamic_range: bool,
}

pub struct LastFrame<'a> {
    pub frame_inputs: &'a FrameInputs,
    pub display_list: &'a DisplayListStorage,
    pub viewport_size: IntSize,
    pub is_same_display_list: bool,
    pub has_other_surface_clear_color: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotRepaint {
    Whole,
    Within(IntRect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotBlend {
    Source,
    SourceOver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStep {
    RasterizeSlot {
        slot: usize,
        rect: IntRect,
        covers_viewport: bool,
        repaint: SlotRepaint,
    },
    ReleaseSlot {
        slot: usize,
    },
    DrawSlotIntoStore {
        slot: usize,
        blend: SlotBlend,
    },
    ReplaySlotRecordsIntoStore {
        slot: usize,
        rect: IntRect,
    },
    PaintCompositorOverlaysIntoStore,
    PresentSlotToWindowServer {
        slot: usize,
        rect: IntRect,
    },
    WithdrawSlotFromWindowServer {
        slot: usize,
    },
    PresentCompositorOverlaysToWindowServer,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePlan {
    pub damage_rect: IntRect,
    pub paints_from_raster_slots: bool,
    pub raster_slot_count: usize,
    pub only_window_server_content_changed: bool,
    pub steps: Vec<FrameStep>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SublayerContent {
    Canvas(CanvasId),
    Video(VideoSinkResourceId),
    RasterSlot(usize),
    CompositorOverlays,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sublayer {
    pub identity: u64,
    pub content: SublayerContent,
    pub placement: Option<WindowServerPlacement>,
}

const CANVAS_SUBLAYER_IDENTITY_TAG: u64 = 1 << 60;
const VIDEO_SUBLAYER_IDENTITY_TAG: u64 = 2 << 60;
const RASTER_SLOT_SUBLAYER_IDENTITY_TAG: u64 = 3 << 60;
const COMPOSITOR_OVERLAYS_SUBLAYER_IDENTITY: u64 = 4 << 60;
const MEDIA_DRAWING_INDEX_SHIFT: u32 = 44;

enum LayerPlanChoice {
    None,
    Keep,
    New(LayerPlan),
}

struct PlannedFrame {
    layer_plan: LayerPlanChoice,
    active_media: Vec<LiveContent>,
    shown_by_window_server: bool,
    repaints_everything: bool,
    sample: LiveMediaSample,
}

#[derive(Default)]
struct LiveContentChangesSinceLastFrame {
    canvases: Vec<CanvasId>,
    video_sinks: Vec<VideoSinkResourceId>,
    image_frames: Vec<ImageFrameResourceId>,
    media_updates: Vec<MediaUpdate>,
    repaints_everything: bool,
}

#[derive(Default)]
pub struct FramePlanner {
    media_activity: MediaActivity,
    layer_plan: Option<LayerPlan>,
    layer_plan_active_media: Vec<LiveContent>,
    last_frame_sample: LiveMediaSample,
    window_server_shows_media: bool,
    repaint_everything_after_dropping_layers: bool,
    planned_frame: Option<PlannedFrame>,
    frame_plan: FramePlan,
    sublayers: Vec<Sublayer>,
}

impl FramePlanner {
    pub fn plan(
        &mut self,
        request: &FrameRequest,
        frame_inputs: &FrameInputs,
        display_list: &DisplayListStorage,
        last_frame: Option<&LastFrame<'_>>,
        sample: LiveMediaSample,
    ) -> &FramePlan {
        let viewport_rect = IntRect::new(0, 0, request.viewport_size.width, request.viewport_size.height);
        let forced_damage_rect = request.forced_damage_rect.intersected(viewport_rect);
        let changes =
            self.live_content_changes_since_last_frame(last_frame, request.viewport_size, display_list, &sample);
        self.media_activity.note_updates(&changes.media_updates, request.now);

        let (layer_plan_choice, active_media) =
            self.layer_plan_choice_for(request, frame_inputs, display_list, last_frame, &sample, viewport_rect);
        let layer_plan = match &layer_plan_choice {
            LayerPlanChoice::None => None,
            LayerPlanChoice::Keep => self.layer_plan.as_ref(),
            LayerPlanChoice::New(layer_plan) => Some(layer_plan),
        };
        let shown_by_window_server =
            layer_plan.is_some_and(|layer_plan| window_server_can_show_media_layers_of(layer_plan, &sample, request));
        let last_layer_plan_holds_last_frame = self.layer_plan.as_ref().filter(|_| last_frame.is_some());
        let keeps_layer_rasters = match (layer_plan, last_layer_plan_holds_last_frame) {
            (Some(layer_plan), Some(last_layer_plan)) => layer_plan.has_same_layers_as(last_layer_plan),
            _ => false,
        };

        let damage = match last_frame {
            Some(last_frame) if !changes.repaints_everything && !forced_damage_rect.contains_rect(viewport_rect) => {
                let offsets_of_records_composited_as_media_layers: Vec<u32> = layer_plan
                    .map(|layer_plan| layer_plan.media_record_offsets().collect())
                    .unwrap_or_default();
                let layer_plans = match (layer_plan, last_layer_plan_holds_last_frame) {
                    (Some(layer_plan), Some(last_layer_plan)) if keeps_layer_rasters => Some(LayerPlans {
                        old: last_layer_plan,
                        new: layer_plan,
                    }),
                    _ => None,
                };
                let mut damage = frame_inputs.damage_since(
                    display_list,
                    last_frame.frame_inputs,
                    last_frame.display_list,
                    viewport_rect,
                    LiveContentChanges {
                        canvases: &changes.canvases,
                        video_sinks: &changes.video_sinks,
                        image_frames: &changes.image_frames,
                        forced_damage_rect,
                        offsets_of_records_composited_as_media_layers: &offsets_of_records_composited_as_media_layers,
                    },
                    layer_plans,
                );
                damage.rect = damage.rect.map(|rect| rect.intersected(viewport_rect));
                damage
            }
            _ => DisplayListDamage {
                rect: None,
                layer_rects: Vec::new(),
            },
        };
        let painted_damage = damage.rect.unwrap_or(viewport_rect).intersected(viewport_rect);

        let layer_count = layer_plan.map_or(0, |layer_plan| layer_plan.layers.len());
        let mut fully_repainted_layers = vec![false; layer_count];
        let mut damage_of_layers = Vec::new();
        let mut repaints_everything = false;
        match layer_plan {
            None => {
                repaints_everything =
                    last_layer_plan_holds_last_frame.is_some() || self.repaint_everything_after_dropping_layers;
            }
            Some(_) if !keeps_layer_rasters => {
                fully_repainted_layers.fill(true);
                repaints_everything = true;
            }
            Some(_) => {
                for (layer_index, fully_repainted) in fully_repainted_layers.iter_mut().enumerate() {
                    let layer_damage = if damage.layer_rects.is_empty() {
                        Some(painted_damage)
                    } else {
                        damage.layer_rects[layer_index]
                    };
                    *fully_repainted = layer_damage.is_none();
                    damage_of_layers.push(layer_damage.unwrap_or(viewport_rect).intersected(viewport_rect));
                }
            }
        }

        if last_frame.is_some() && shown_by_window_server != self.window_server_shows_media {
            repaints_everything = true;
            fully_repainted_layers.fill(true);
        }

        let live_media_changed = !changes.canvases.is_empty() || !changes.video_sinks.is_empty();
        let damage_rect = if repaints_everything {
            viewport_rect
        } else {
            match layer_plan.filter(|_| !shown_by_window_server) {
                Some(layer_plan) => painted_damage.united(
                    rect_of_media_layers_showing(layer_plan, &changes.canvases, &changes.video_sinks)
                        .intersected(viewport_rect),
                ),
                None => painted_damage,
            }
        };

        let steps = layer_plan.map_or_else(Vec::new, |layer_plan| {
            frame_steps(
                layer_plan,
                &fully_repainted_layers,
                &damage_of_layers,
                shown_by_window_server,
            )
        });
        self.frame_plan = FramePlan {
            damage_rect,
            paints_from_raster_slots: layer_plan.is_some(),
            raster_slot_count: layer_count,
            only_window_server_content_changed: shown_by_window_server && live_media_changed,
            steps,
        };
        self.planned_frame = Some(PlannedFrame {
            layer_plan: layer_plan_choice,
            active_media,
            shown_by_window_server,
            repaints_everything,
            sample: sample.kept_for_list(display_list),
        });
        &self.frame_plan
    }

    pub fn frame_plan(&self) -> &FramePlan {
        &self.frame_plan
    }

    pub fn frame_layer_plan(&self) -> Option<&LayerPlan> {
        match &self.planned_frame.as_ref()?.layer_plan {
            LayerPlanChoice::None => None,
            LayerPlanChoice::Keep => self.layer_plan.as_ref(),
            LayerPlanChoice::New(layer_plan) => Some(layer_plan),
        }
    }

    pub fn committed_layer_plan(&self) -> Option<&LayerPlan> {
        self.layer_plan.as_ref()
    }

    pub fn did_fail_to_allocate_raster_slot(&mut self) -> bool {
        let planned_frame = self
            .planned_frame
            .as_mut()
            .expect("a raster slot is allocated for a planned frame");
        let needs_whole_store = planned_frame.shown_by_window_server;
        planned_frame.layer_plan = LayerPlanChoice::None;
        planned_frame.shown_by_window_server = false;
        self.drop_layers();
        self.repaint_everything_after_dropping_layers = true;
        needs_whole_store
    }

    pub fn commit(&mut self) -> Option<Duration> {
        let planned_frame = self
            .planned_frame
            .take()
            .expect("a frame is planned before it is committed");
        let has_layer_plan = !matches!(planned_frame.layer_plan, LayerPlanChoice::None);
        self.window_server_shows_media = has_layer_plan && planned_frame.shown_by_window_server;
        if planned_frame.repaints_everything {
            self.repaint_everything_after_dropping_layers = false;
        }
        self.last_frame_sample = planned_frame.sample;
        if !has_layer_plan {
            self.drop_layers();
            return None;
        }
        if let LayerPlanChoice::New(layer_plan) = planned_frame.layer_plan {
            self.layer_plan = Some(layer_plan);
        }
        self.layer_plan_active_media = planned_frame.active_media;
        self.sublayers = self.sublayers_shown_by_window_server();
        Some(TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE)
    }

    pub fn release_idle_layers(&mut self) -> bool {
        // While the window server shows the media, the layers cost nothing, and the frame on screen still needs
        // their plan, as for a snapshot. The next frame without a plan puts the media back into the store.
        if self.layer_plan.is_none() || self.window_server_shows_media {
            return false;
        }
        self.drop_layers();
        self.repaint_everything_after_dropping_layers = true;
        true
    }

    pub fn sublayers(&self) -> &[Sublayer] {
        &self.sublayers
    }

    pub fn steps_compositing_presented_frame_into_store(&self) -> Vec<FrameStep> {
        let Some(layer_plan) = self.layer_plan.as_ref().filter(|_| self.window_server_shows_media) else {
            return Vec::new();
        };
        let mut steps = store_steps_above_bottom_slot(layer_plan);
        steps.push(FrameStep::PaintCompositorOverlaysIntoStore);
        steps
    }

    pub fn last_frame_drew_canvas(&self, id: CanvasId) -> bool {
        self.last_frame_sample.canvas(id).is_some()
    }

    fn drop_layers(&mut self) {
        self.layer_plan = None;
        self.layer_plan_active_media.clear();
        self.sublayers.clear();
    }

    fn live_content_changes_since_last_frame(
        &self,
        last_frame: Option<&LastFrame<'_>>,
        viewport_size: IntSize,
        display_list: &DisplayListStorage,
        sample: &LiveMediaSample,
    ) -> LiveContentChangesSinceLastFrame {
        let mut changes = LiveContentChangesSinceLastFrame::default();
        let Some(last_frame) = last_frame else {
            changes.repaints_everything = true;
            return changes;
        };
        if last_frame.viewport_size != viewport_size || last_frame.has_other_surface_clear_color {
            changes.repaints_everything = true;
        }

        for last_canvas in &self.last_frame_sample.canvases {
            let content_generation = sample
                .canvas(last_canvas.id)
                .map_or(0, |canvas| canvas.content_generation);
            if content_generation == last_canvas.content_generation {
                continue;
            }
            changes.canvases.push(last_canvas.id);
            changes.media_updates.push(MediaUpdate {
                media: LiveContent::Canvas(last_canvas.id),
                content_version: content_generation,
            });
        }

        let summary = display_list.summary();
        for last_video in &self.last_frame_sample.videos {
            let frame_id = sample.video_frame_id(last_video.id);
            if frame_id == last_video.frame_id {
                continue;
            }
            changes.media_updates.push(MediaUpdate {
                media: LiveContent::VideoFrame(last_video.id),
                content_version: frame_id.unwrap_or(0),
            });
            if !summary.video_sink_ids.contains(&last_video.id.0) {
                changes.repaints_everything = true;
            }
            changes.video_sinks.push(last_video.id);
        }

        for image_frame in &sample.image_frames {
            if self.last_frame_sample.image_frame_pixel_generation(image_frame.id) == Some(image_frame.pixel_generation)
            {
                continue;
            }
            if !summary.image_frame_ids.contains(&image_frame.id.0) {
                changes.repaints_everything = true;
            }
            changes.image_frames.push(image_frame.id);
        }
        changes
    }

    fn layer_plan_choice_for(
        &self,
        request: &FrameRequest,
        frame_inputs: &FrameInputs,
        display_list: &DisplayListStorage,
        last_frame: Option<&LastFrame<'_>>,
        sample: &LiveMediaSample,
        viewport_rect: IntRect,
    ) -> (LayerPlanChoice, Vec<LiveContent>) {
        if !request.presents_to_client {
            return (LayerPlanChoice::None, Vec::new());
        }
        let mut active_media = self.media_activity.active_media(request.now);
        if request.window_server_may_show_media && request.window_server_may_show_high_dynamic_range {
            for &video_sink_id in &display_list.summary().video_sink_ids {
                let video_sink_id = VideoSinkResourceId(video_sink_id);
                let media = LiveContent::VideoFrame(video_sink_id);
                if active_media.contains(&media) {
                    continue;
                }
                let holds_high_dynamic_range = sample
                    .video(video_sink_id)
                    .is_some_and(|video| video.frame_id.is_some() && video.frame_holds_high_dynamic_range);
                if holds_high_dynamic_range {
                    active_media.push(media);
                }
            }
        }
        if active_media.is_empty() {
            return (LayerPlanChoice::None, active_media);
        }

        let layer_plan_holds_for_frame = self.layer_plan.is_some()
            && last_frame.is_some_and(|last_frame| {
                last_frame.is_same_display_list
                    && last_frame.viewport_size == request.viewport_size
                    && frame_inputs_place_content_alike(frame_inputs, last_frame.frame_inputs)
            })
            && self.layer_plan_active_media == active_media;
        if layer_plan_holds_for_frame {
            return (LayerPlanChoice::Keep, active_media);
        }

        let tree = frame_inputs.visual_context_tree.as_arc();
        let layer_plan = build_layer_plan(&LayerPlanInputs {
            tape: display_list.bytes(),
            command_runs: display_list.command_runs(),
            summary: display_list.summary(),
            effect_clips: display_list.effect_clip_plan(tree),
            tree,
            scroll_offsets: frame_inputs.presented_scroll_state_snapshot.device_offsets(),
            viewport_rect,
            active_media: &active_media,
        });
        match layer_plan {
            Ok(layer_plan) => (LayerPlanChoice::New(layer_plan), active_media),
            Err(_) => (LayerPlanChoice::None, active_media),
        }
    }

    fn sublayers_shown_by_window_server(&self) -> Vec<Sublayer> {
        let Some(layer_plan) = self.layer_plan.as_ref().filter(|_| self.window_server_shows_media) else {
            return Vec::new();
        };
        let mut sublayers = Vec::new();
        let mut drawings_of_media: Vec<(u64, u64)> = Vec::new();
        for (slot, layer) in layer_plan.layers.iter().enumerate().skip(1) {
            let LayerKind::Media { content } = layer.kind else {
                sublayers.push(Sublayer {
                    identity: RASTER_SLOT_SUBLAYER_IDENTITY_TAG | slot as u64,
                    content: SublayerContent::RasterSlot(slot),
                    placement: None,
                });
                continue;
            };
            let Some(Ok(placement)) = &layer.window_server_placement else {
                continue;
            };
            let (identity, content) = match content {
                LiveContent::Canvas(id) => (CANVAS_SUBLAYER_IDENTITY_TAG | id.0, SublayerContent::Canvas(id)),
                LiveContent::VideoFrame(id) => (VIDEO_SUBLAYER_IDENTITY_TAG | id.0, SublayerContent::Video(id)),
                LiveContent::ImageFrame(_) => continue,
            };
            let earlier_drawings = match drawings_of_media
                .iter_mut()
                .find(|(media_identity, _)| *media_identity == identity)
            {
                Some((_, drawings)) => {
                    *drawings += 1;
                    *drawings - 1
                }
                None => {
                    drawings_of_media.push((identity, 1));
                    0
                }
            };
            sublayers.push(Sublayer {
                identity: identity | (earlier_drawings << MEDIA_DRAWING_INDEX_SHIFT),
                content,
                placement: Some(*placement),
            });
        }
        sublayers.push(Sublayer {
            identity: COMPOSITOR_OVERLAYS_SUBLAYER_IDENTITY,
            content: SublayerContent::CompositorOverlays,
            placement: None,
        });
        sublayers
    }
}

fn frame_inputs_place_content_alike(a: &FrameInputs, b: &FrameInputs) -> bool {
    Arc::ptr_eq(a.visual_context_tree.as_arc(), b.visual_context_tree.as_arc())
        && a.presented_scroll_state_snapshot.device_offsets() == b.presented_scroll_state_snapshot.device_offsets()
}

fn window_server_can_show_media_layers_of(
    layer_plan: &LayerPlan,
    sample: &LiveMediaSample,
    request: &FrameRequest,
) -> bool {
    if !request.window_server_may_show_media {
        return false;
    }
    layer_plan.layers.iter().all(|layer| {
        let LayerKind::Media { content } = layer.kind else {
            return true;
        };
        if !matches!(layer.window_server_placement, Some(Ok(_))) {
            return false;
        }
        match content {
            LiveContent::Canvas(id) => sample.canvas(id).is_some_and(|canvas| canvas.has_window_server_surface),
            LiveContent::VideoFrame(id) => sample.video(id).is_some_and(|video| {
                video.frame_id.is_some()
                    && video.frame_has_window_server_surface
                    && (request.window_server_may_show_high_dynamic_range || !video.frame_holds_high_dynamic_range)
            }),
            LiveContent::ImageFrame(_) => false,
        }
    })
}

fn rect_of_media_layers_showing(
    layer_plan: &LayerPlan,
    canvases: &[CanvasId],
    video_sinks: &[VideoSinkResourceId],
) -> IntRect {
    layer_plan
        .layers
        .iter()
        .filter(|layer| match layer.kind {
            LayerKind::Media {
                content: LiveContent::Canvas(id),
            } => canvases.contains(&id),
            LayerKind::Media {
                content: LiveContent::VideoFrame(id),
            } => video_sinks.contains(&id),
            _ => false,
        })
        .fold(IntRect::default(), |rect, layer| rect.united(layer.rect))
}

fn frame_steps(
    layer_plan: &LayerPlan,
    fully_repainted_layers: &[bool],
    damage_of_layers: &[IntRect],
    shown_by_window_server: bool,
) -> Vec<FrameStep> {
    let mut steps = Vec::new();
    for (slot, layer) in layer_plan.layers.iter().enumerate() {
        if matches!(layer.kind, LayerKind::Media { .. }) {
            steps.push(FrameStep::ReleaseSlot { slot });
            continue;
        }
        steps.push(FrameStep::RasterizeSlot {
            slot,
            rect: layer.rect,
            covers_viewport: layer.kind == LayerKind::Base,
            repaint: if fully_repainted_layers[slot] {
                SlotRepaint::Whole
            } else {
                SlotRepaint::Within(damage_of_layers[slot])
            },
        });
    }

    steps.push(FrameStep::DrawSlotIntoStore {
        slot: 0,
        blend: SlotBlend::Source,
    });
    if !shown_by_window_server {
        steps.extend(store_steps_above_bottom_slot(layer_plan));
        steps.push(FrameStep::PaintCompositorOverlaysIntoStore);
        return steps;
    }

    for (slot, layer) in layer_plan.layers.iter().enumerate() {
        if layer.kind == LayerKind::Painted && !layer.rect.is_empty() {
            steps.push(FrameStep::PresentSlotToWindowServer { slot, rect: layer.rect });
        } else {
            steps.push(FrameStep::WithdrawSlotFromWindowServer { slot });
        }
    }
    steps.push(FrameStep::PresentCompositorOverlaysToWindowServer);
    steps
}

fn store_steps_above_bottom_slot(layer_plan: &LayerPlan) -> Vec<FrameStep> {
    layer_plan
        .layers
        .iter()
        .enumerate()
        .skip(1)
        .map(|(slot, layer)| match layer.kind {
            LayerKind::Media { .. } => FrameStep::ReplaySlotRecordsIntoStore { slot, rect: layer.rect },
            _ => FrameStep::DrawSlotIntoStore {
                slot,
                blend: SlotBlend::SourceOver,
            },
        })
        .collect()
}
