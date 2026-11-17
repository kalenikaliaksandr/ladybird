/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! What a frame repaints of the frame painted before it, and of what each backing store holds.

use super::layers::{LayerUpdate, TileInvalidation};
use super::{ContextPresenting, FrameToPaint, PresentingEnvironment, rect_of_size};
use crate::frame_inputs::FrameInputs;
use crate::host::PendingFrame;
use crate::raster_host::LiveContent;
use libcompositing_rust::display_list::commands::{
    CanvasId, CompositorContextId, ImageFrameResourceId, VideoSinkHandle,
};
use libcompositing_rust::scrolling::DisplayList;
use libgfx_rust::{IntRect, IntSize};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The frame painted last: its display list and inputs, and the version of each live content it drew, which comparing
/// lists cannot see change.
pub struct PaintedFrame {
    pub display_list: Rc<DisplayList>,
    pub frame_inputs: FrameInputs,
    pub viewport_size: IntSize,
    canvas_content_generations: HashMap<u64, u64>,
    video_frame_ids: HashMap<u64, Option<u64>>,
    image_frame_pixel_generations: HashMap<u64, u64>,
    nested_context_frame_generations: HashMap<u64, Option<u64>>,
}

impl PaintedFrame {
    pub fn draws_canvas(&self, canvas_id: u64) -> bool {
        self.canvas_content_generations.contains_key(&canvas_id)
    }
}

/// What each backing store misses of the frames painted since it was last painted into, by its id, which new stores
/// never repeat. The stores painted into least recently are forgotten first, as retired stores are never painted into
/// again, and a store that is forgotten is painted in full.
#[derive(Default)]
pub struct StoreDamage {
    // The store painted into last is last.
    accumulated: Vec<(i32, IntRect)>,
}

// Twice as many as a context ever has at once.
const MAXIMUM_REMEMBERED_STORE_COUNT: usize = 16;

impl StoreDamage {
    pub fn forget_every_store(&mut self) {
        self.accumulated.clear();
    }

    /// What painting a frame into the store repaints: the frame's damage, and what the store missed since it was last
    /// painted, all of it for a store never painted into.
    pub fn take(&mut self, bitmap_id: i32, frame_damage: IntRect, store_rect: IntRect) -> IntRect {
        let mut repaint_rect = store_rect;
        for (id, accumulated) in &mut self.accumulated {
            if *id == bitmap_id {
                repaint_rect = accumulated.united(frame_damage);
            }
            *accumulated = accumulated.united(frame_damage);
        }
        self.accumulated.retain(|(id, _)| *id != bitmap_id);
        self.accumulated.push((bitmap_id, IntRect::default()));
        if self.accumulated.len() > MAXIMUM_REMEMBERED_STORE_COUNT {
            self.accumulated.remove(0);
        }
        repaint_rect
    }
}

// The canvases, video sinks, image frames and nested contexts whose pixels changed since the frame painted last.
#[derive(Default)]
pub struct LiveContentChangesSinceLastPaint {
    pub canvases: Vec<CanvasId>,
    pub video_sinks: Vec<VideoSinkHandle>,
    pub image_frames: Vec<ImageFrameResourceId>,
    pub nested_contexts: Vec<CompositorContextId>,
    pub repaints_everything: bool,
}

pub struct FrameDamage {
    // In the device pixels of the raster.
    pub rect: IntRect,
    pub layers: LayerUpdate,
    pub live_media_changed: bool,
}

impl ContextPresenting {
    /// The live content of the context as it is now, with the canvases that the list and the frame painted last draw.
    pub(super) fn live_content(&self, env: &PresentingEnvironment<'_>, display_list: &DisplayList) -> LiveContent {
        let mut canvas_ids = display_list.storage().summary().canvas_ids.clone();
        if let Some(painted_frame) = &self.painted_frame {
            let canvas_ids_of_list: HashSet<u64> = canvas_ids.iter().copied().collect();
            canvas_ids.extend(
                painted_frame
                    .canvas_content_generations
                    .keys()
                    .filter(|id| !canvas_ids_of_list.contains(id)),
            );
        }
        env.raster.live_content(env.context_id, &canvas_ids)
    }

    pub(super) fn remember_painted_frame(
        &mut self,
        env: &PresentingEnvironment<'_>,
        viewport_rect: IntRect,
        display_list: &Rc<DisplayList>,
        frame_inputs: FrameInputs,
        live_content: &LiveContent,
    ) {
        let canvas_generation = |id: u64| {
            live_content
                .canvases
                .get(&id)
                .map_or(0, |canvas| canvas.content_generation)
        };
        let summary = display_list.storage().summary();
        let canvas_content_generations = summary
            .canvas_ids
            .iter()
            .map(|&id| (id, canvas_generation(id)))
            .collect();
        // The host reports the video sinks that the list draws.
        let video_frame_ids = live_content
            .video_sinks
            .values()
            .map(|sink| (sink.handle, sink.has_frame.then_some(sink.frame_id)))
            .collect();
        let image_frame_pixel_generations = live_content.image_frames.clone();
        let nested_context_frame_generations = summary
            .composited_context_ids
            .iter()
            .map(|&id| (id, env.nested_contexts.frame_generation_of(CompositorContextId(id))))
            .collect();
        self.painted_frame = Some(PaintedFrame {
            display_list: display_list.clone(),
            frame_inputs,
            viewport_size: IntSize {
                width: viewport_rect.width,
                height: viewport_rect.height,
            },
            canvas_content_generations,
            video_frame_ids,
            image_frame_pixel_generations,
            nested_context_frame_generations,
        });
    }

    fn live_content_changes_since_last_paint(
        &self,
        env: &PresentingEnvironment<'_>,
        viewport_size: IntSize,
        display_list: &DisplayList,
        live_content: &LiveContent,
    ) -> LiveContentChangesSinceLastPaint {
        let mut changes = LiveContentChangesSinceLastPaint::default();
        let Some(painted_frame) = &self.painted_frame else {
            changes.repaints_everything = true;
            return changes;
        };
        if painted_frame.viewport_size != viewport_size
            || painted_frame.display_list.surface_clear_color() != display_list.surface_clear_color()
        {
            changes.repaints_everything = true;
        }

        let canvas_ids_of_list: HashSet<u64> = display_list.storage().summary().canvas_ids.iter().copied().collect();
        for (canvas_id, content_generation) in &painted_frame.canvas_content_generations {
            let current_content_generation = live_content
                .canvases
                .get(canvas_id)
                .map_or(0, |canvas| canvas.content_generation);
            if current_content_generation == *content_generation {
                continue;
            }
            // A canvas that no command of this list draws has no command in this list to damage.
            if !canvas_ids_of_list.contains(canvas_id) {
                changes.repaints_everything = true;
            }
            changes.canvases.push(CanvasId(*canvas_id));
        }
        let summary = display_list.storage().summary();
        let image_frame_ids_of_list: HashSet<u64> = summary.image_frame_ids.iter().copied().collect();
        let video_sink_handles_of_list: HashSet<u64> = summary.video_sink_handles.iter().copied().collect();
        for (video_sink_handle, frame_id) in &painted_frame.video_frame_ids {
            let current_frame_id = live_content
                .video_sinks
                .get(video_sink_handle)
                .and_then(|sink| sink.has_frame.then_some(sink.frame_id));
            if current_frame_id == *frame_id {
                continue;
            }
            // A video that no command of this list draws has no command in this list to damage.
            if !video_sink_handles_of_list.contains(video_sink_handle) {
                changes.repaints_everything = true;
            }
            changes.video_sinks.push(VideoSinkHandle(*video_sink_handle));
        }
        for (&image_frame_id, &pixel_generation) in &live_content.image_frames {
            if painted_frame.image_frame_pixel_generations.get(&image_frame_id) == Some(&pixel_generation) {
                continue;
            }
            // An image that no command of this list draws, such as one that a filter of the visual context tree
            // draws, has no command in this list to damage.
            if !image_frame_ids_of_list.contains(&image_frame_id) {
                changes.repaints_everything = true;
            }
            changes.image_frames.push(ImageFrameResourceId(image_frame_id));
        }
        let nested_context_ids_of_list: HashSet<u64> = summary.composited_context_ids.iter().copied().collect();
        for (&nested_context_id, &frame_generation) in &painted_frame.nested_context_frame_generations {
            let nested_context = CompositorContextId(nested_context_id);
            if env.nested_contexts.frame_generation_of(nested_context) == frame_generation {
                continue;
            }
            if !nested_context_ids_of_list.contains(&nested_context_id) {
                changes.repaints_everything = true;
            }
            changes.nested_contexts.push(nested_context);
        }
        changes
    }

    pub(super) fn frame_damage_for(
        &mut self,
        env: &PresentingEnvironment<'_>,
        pending_frame: &PendingFrame,
        frame: &FrameToPaint<'_>,
        live_content: &LiveContent,
    ) -> FrameDamage {
        let viewport_size = IntSize {
            width: pending_frame.viewport_rect.width,
            height: pending_frame.viewport_rect.height,
        };
        let viewport_rect = rect_of_size(viewport_size);
        let raster_rect = self.raster_rect();
        let forced_damage_rect = pending_frame.forced_damage_rect.intersected(viewport_rect);
        let changes = self.live_content_changes_since_last_paint(env, viewport_size, frame.display_list, live_content);
        let mut layer_update = self.layer_update_for(env, frame, raster_rect, live_content);
        let live_media_changed = !changes.canvases.is_empty() || !changes.video_sinks.is_empty();

        let scrolled_compositor_scrollbars = self.painted_frame.as_ref().map_or(IntRect::default(), |painted_frame| {
            frame
                .frame_inputs
                .gutters_of_compositor_painted_scrollbars_that_scrolled_since(&painted_frame.frame_inputs)
        });
        let forced_damage_rect =
            self.raster_rect_of(forced_damage_rect.united(scrolled_compositor_scrollbars.intersected(viewport_rect)));
        let invalidates_every_tile = changes.repaints_everything || forced_damage_rect.contains_rect(raster_rect);
        layer_update.tile_invalidation = if invalidates_every_tile {
            TileInvalidation::Everywhere
        } else if !forced_damage_rect.is_empty() {
            TileInvalidation::WithinViewportRect(forced_damage_rect)
        } else {
            TileInvalidation::Nothing
        };
        // Layers that come or go, or move as their spaces scroll, repaint all of the frame. So does a switch of who
        // shows the layers above the base layer, as the frame holds them only while the window server does not.
        let composites_like_last_frame = self.painted_frame.is_some()
            && self.layers.compositing_of_latest_frame() == layer_update.compositing.as_slice();
        let window_server_switched = layer_update.shown_by_window_server != self.layers.window_server_shows_media();
        let repaints_everything = invalidates_every_tile || !composites_like_last_frame || window_server_switched;
        let stale_rect = self.prepare_tile_pass(env, frame, live_content, &mut layer_update, raster_rect);
        FrameDamage {
            rect: if repaints_everything {
                raster_rect
            } else {
                forced_damage_rect.united(stale_rect)
            },
            layers: layer_update,
            live_media_changed,
        }
    }
}
