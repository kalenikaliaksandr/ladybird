/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::painting::record::trace::Observer;

use super::{PaintPhase, PaintRecorder};
use crate::css::style::fast_hash::FastSet;
use crate::layout::node_data::{NodeKind, NodeSlotId};
use crate::layout::node_facts;
use crate::painting::display_list::commands::ContextRef;
use crate::painting::display_list::device_pixels::DevicePixelConverter;
use crate::painting::display_list::recorder::DisplayListRecorder;
use crate::painting::node_painting;
use crate::painting::paint_order_plan::PaintScope;
use crate::painting::paint_read::{GeometryRead, PaintSource};
use crate::painting::record::RecordingInputs;
use crate::painting::record::assemble::{Assembler, AssemblyCulling, frame_is_unchanged};
use crate::painting::record::frame_inputs::FrameInputs;
use crate::painting::record::inputs::RecordingPurpose;
use crate::painting::record::order_tree::{ChildKey, PaintOrderTree, ProducerKind};
use crate::painting::record::paint::background_resolution::root_background_canvas_rect;
use crate::painting::record::resources::RecordingResourceManifest;
use crate::painting::record::scratch::RecordingScratch;
use crate::painting::record::svg_resources::MaskLayerSet;
use crate::painting::record::trace::{Action, Operation};
use crate::painting::record::vector_images::VectorImageRenderRequest;
use crate::painting::record::{AssembledHitTestItems, PublishedHitTestItems, RecordingOutput, RecordingResult};
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_display_list(
    source: &PaintSource<'_>,
    scratch: &mut RecordingScratch,
    tree: &mut PaintOrderTree,
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    source_recording: Option<Arc<RecordingOutput>>,
    source_items: Option<Arc<PublishedHitTestItems>>,
    trace: bool,
) -> RecordingResult {
    scratch.begin_recording(source.frame().paintable_row_capacity());
    macro_rules! record {
        ($observer:ty) => {
            record_display_list_impl::<$observer>(
                source,
                scratch,
                tree,
                viewport,
                inputs,
                source_recording,
                source_items,
            )
        };
    }
    let result = if trace {
        record!(super::trace::Trace)
    } else {
        record!(super::trace::NoTrace)
    };
    scratch.clear_temporary_caches();
    result
}

#[allow(clippy::too_many_arguments)]
fn record_display_list_impl<O: Observer>(
    source: &PaintSource<'_>,
    scratch: &mut RecordingScratch,
    tree: &mut PaintOrderTree,
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    source_recording: Option<Arc<RecordingOutput>>,
    source_items: Option<Arc<PublishedHitTestItems>>,
) -> RecordingResult {
    let paint_state = source.frame().paint_state();
    let structural_epoch = paint_state.structural_epoch();
    let frame_inputs = FrameInputs::from_recording_inputs(inputs, paint_state);
    // What the published recording records steers what this one aims for, even where its output cannot be copied.
    let published_interest = source_recording
        .as_ref()
        .map(|frame| Arc::clone(&frame.recorded_interest));
    let root_background_canvas_rect = root_background_canvas_rect(
        source,
        inputs.uncaptured.root_background_source.root_layout_node,
        inputs.css_viewport_rect,
    );
    // The published recording is copied from while every input its producers read is unchanged and
    // no push asked for everything; otherwise this frame records from scratch.
    let source_is_usable = source_recording
        .as_ref()
        .is_some_and(|frame| frame.frame_inputs == frame_inputs)
        && source_items.is_some()
        && !source.frame().damage().covers_everything()
        && !source.frame().damage().scroll_metadata_everywhere();
    let (source_recording, source_items) = if source_is_usable {
        (source_recording, source_items)
    } else {
        (None, None)
    };
    let window_focus_changed = source_recording
        .as_ref()
        .is_some_and(|frame| frame.window_is_focused != inputs.window_is_focused);
    let shared_items = source_items.as_ref().map(|published| Arc::clone(&published.items));
    let mut recorder = PaintRecorder::<O>::new(
        source,
        scratch,
        viewport,
        inputs,
        frame_inputs.records_per_box_wheel_hit_test_targets(),
        source_recording,
        AssembledHitTestItems::new(shared_items.clone(), paint_state.hit_test_item_capacity_hint),
    );
    let mut interest = recorder.resolve_interest(published_interest.as_ref());
    recorder.interest_rects = std::mem::take(&mut interest.rects);
    recorder
        .observer
        .observe(|log| log.damage = Some(source.frame().damage().summary()));
    recorder.record_canvas();
    let prologue_bytes = u32::try_from(recorder.recorder.byte_size()).expect("display list exceeds u32");
    let has_inspector_overlays = inputs.inspector_highlight.is_some()
        || inputs.grid_overlays.is_some()
        || !inputs.flex_overlays.is_empty()
        || inputs.caret_debug_rect.is_some();
    let root_scope = PaintScope::stacking_context(viewport);
    // Nothing was pushed and nothing records every frame: the published tape and items are
    // this frame, which lets the compositor skip its update as well.
    let unchanged_frame = recorder
        .source_recording
        .as_ref()
        .filter(|frame| {
            frame_is_unchanged(
                tree,
                !source.frame().damage().is_empty(),
                window_focus_changed,
                interest.culling,
            ) && !has_inspector_overlays
                && frame.prologue_bytes == prologue_bytes
                && recorder.recorder.bytes() == &frame.display_list.bytes[..prologue_bytes as usize]
        })
        .map(|frame| frame.display_list.clone());
    let (display_list, root_output) = match unchanged_frame {
        Some(display_list) => {
            recorder
                .observer
                .observe(|log| log.leaf(Operation::Scope(root_scope), Action::Copy, false));
            recorder.list.append_every_published_item();
            let root_output = tree.root_entry().output();
            recorder.blocking_wheel_event_region_count = root_output.blocking_wheel_event_regions;
            (display_list, root_output)
        }
        None => {
            let source_prologue_bytes = recorder.source_recording.as_ref().map(|frame| frame.prologue_bytes);
            let root_output = Assembler::new(
                &mut recorder,
                tree,
                source_prologue_bytes,
                inputs.publishes_recording,
                window_focus_changed,
                interest.culling,
            )
            .assemble_root(root_scope);
            if has_inspector_overlays {
                recorder.trace_paint(
                    Operation::Named(None, "inspector-overlays"),
                    crate::painting::record::paint::inspector_overlay::record_inspector_overlays,
                );
            }
            (Arc::new(recorder.recorder.into_builder().finish()), root_output)
        }
    };
    let mut hit_test_list = recorder.list.finish();
    debug_assert!(
        inputs.purpose == RecordingPurpose::Frame
            || shared_items.is_some_and(|items| Arc::ptr_eq(&items, &hit_test_list.items)),
        "a refresh shares the hit-test items of the recording it copies from"
    );
    hit_test_list.generation = paint_state.hit_test_list_generation + 1;
    let output = RecordingOutput {
        recorded_structural_epoch: structural_epoch,
        frame_inputs,
        root_background_canvas_rect,
        prologue_bytes,
        hit_test_list,
        display_list,
        has_blocking_wheel_event_listeners: recorder.blocking_wheel_event_region_count > 0,
        wheel_event_listener_state_generation: inputs.wheel_event_listener_state_generation,
        is_identical_to_published_recording: false,
        window_is_focused: inputs.window_is_focused,
        capture_log: recorder.observer.finish(),
        vector_image_display_lists: Vec::new(),
        recorded_interest: interest.into_recorded_interest(root_output.culled_wheel_routing),
    };
    RecordingResult {
        output,
        resources: recorder.resources,
    }
}

/// Whether a recording of `source` for the document's `viewport` with `inputs`, which copies from `published` and
/// assembles in `tree`, would draw an SVG image that `drawn` says no recording drew. Only a producer `published` left out
/// that the recording does not can draw one, so the producers `tree` lists as left out that paint for a box whose facts
/// hold an SVG image, its own and the inline pieces another box paints for it, are placed, and those the recording's
/// interest no longer leaves out record aside. The tree is left as it was.
pub(crate) fn would_draw_vector_image_not_drawn(
    source: &PaintSource<'_>,
    scratch: &mut RecordingScratch,
    tree: &PaintOrderTree,
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    published: Option<&Arc<RecordingOutput>>,
    drawn: impl Fn(&VectorImageRenderRequest) -> bool,
) -> bool {
    let rows: Vec<NodeSlotId> = source.frame().rows_with_vector_images().collect();
    if rows.is_empty() {
        return false;
    }
    scratch.begin_recording(source.frame().paintable_row_capacity());
    let records_per_box_wheel_hit_test_targets =
        FrameInputs::from_recording_inputs(inputs, source.frame().paint_state())
            .records_per_box_wheel_hit_test_targets();
    let mut recorder = PaintRecorder::<super::trace::NoTrace>::new(
        source,
        scratch,
        viewport,
        inputs,
        records_per_box_wheel_hit_test_targets,
        None,
        AssembledHitTestItems::new(None, 0),
    );
    let interest = recorder.resolve_interest(published.map(|published| &published.recorded_interest));
    let leaves_out_by_interest = matches!(interest.culling, AssemblyCulling::ByInterest { .. });
    recorder.interest_rects = interest.rects;
    for row in rows {
        let owner_of_inline_pieces = recorder.owner_of_inline_pieces_painted_for(row);
        for owner in std::iter::once(row).chain(owner_of_inline_pieces) {
            for scope in tree.scopes_owned_by(owner) {
                for entry in tree.children(scope).iter() {
                    let ChildKey::Producer(kind) = entry.key() else {
                        continue;
                    };
                    if !entry.output().culled {
                        continue;
                    }
                    let paints_for_row = match kind {
                        _ if owner == row => true,
                        ProducerKind::InlinePiece(index) => recorder.inline_piece_node(owner, index) == row,
                        _ => false,
                    };
                    if !paints_for_row {
                        continue;
                    }
                    let reach = recorder.cull_reach_of_producer(owner, kind);
                    if reach.is_nothing() || (leaves_out_by_interest && recorder.interest_rects.miss(reach)) {
                        continue;
                    }
                    recorder.record_commands_of_producer(owner, kind);
                }
            }
        }
    }
    let draws_vector_image_not_drawn = recorder
        .resources
        .vector_image_render_requests
        .iter()
        .any(|request| !drawn(request));
    drop(recorder);
    scratch.clear_temporary_caches();
    draws_vector_image_not_drawn
}

impl<'a, O: Observer> PaintRecorder<'a, O> {
    /// A recorder of `source` for the document's `viewport` with `inputs`, which copies clean output from
    /// `source_recording` and assembles its hit-test items in `list`.
    fn new(
        source: &'a PaintSource<'a>,
        scratch: &'a mut RecordingScratch,
        viewport: NodeSlotId,
        inputs: &'a RecordingInputs,
        records_per_box_wheel_hit_test_targets: bool,
        source_recording: Option<Arc<RecordingOutput>>,
        list: AssembledHitTestItems,
    ) -> Self {
        let paint_state = source.frame().paint_state();
        let force_dark_settings = inputs.force_dark_enabled.then_some(inputs.force_dark_settings);
        Self {
            source,
            paint_state,
            inputs,
            recorder: DisplayListRecorder::new(force_dark_settings),
            converter: DevicePixelConverter::new(inputs.device_pixels_per_css_pixel),
            svg_resource_walk: None,
            viewport,
            source_recording,
            live_producer: false,
            producer_reads_window_focus: std::cell::Cell::new(false),
            line_root_positions: None,
            moved_expansion: FastSet::default(),
            blocking_wheel_event_region_count: 0,
            records_per_box_wheel_hit_test_targets,
            observer: O::default(),
            list,
            scratch,
            resources: RecordingResourceManifest::default(),
            cull_spaces: None,
            last_placement: None,
            interest_rects: Default::default(),
        }
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    // SVG content below an SVG root is recorded by the root's producer; every box inside
    // shows up nested under it.
    fn paint_svg_box(&mut self, svg_box: NodeSlotId) {
        self.trace_scope(Operation::Named(Some(svg_box), "svg"), Action::Record, |this| {
            this.paint_svg_box_impl(svg_box);
        });
    }

    pub(crate) fn paint_svg_box_impl(&mut self, svg_box: NodeSlotId) {
        if self.is_recording_svg_resource_content() {
            let parent_to_enclosing_space = self
                .recorder
                .ambient_inline_transform()
                .unwrap_or_else(libgfx_rust::AffineTransform::identity);
            self.paint_svg_box_inside_resource(svg_box, parent_to_enclosing_space, true);
            return;
        }
        let context = self.own_context(svg_box);
        self.recorder.set_accumulated_visual_context(context);

        // For elements with SVG filters, emit a transparent FillRect to trigger filter application.
        // This ensures content-generating filters (feFlood, feImage) work even with empty source.
        if let Some(svg_filter_bounds) = self.source.committed_side_data(svg_box).svg_filter_bounds {
            let device_rect = self
                .converter
                .enclosing_device_rect(crate::css::css_pixels::CssPixelRect::from(svg_filter_bounds));
            self.recorder.fill_rect_transparent(device_rect);
        }

        if self.declare_mask_contents(svg_box, MaskLayerSet::SvgOnly) {
            return;
        }
        let before = self.list.len();
        self.record_hit_test_items(svg_box, PaintPhase::Foreground);
        self.observer.observe(|log| {
            log.leaf(
                Operation::Producer(svg_box, ProducerKind::HitForeground),
                Action::Record,
                self.list.len() == before,
            );
        });
        if self.layout_kind(svg_box) == Some(NodeKind::SVGForeignObjectBox) {
            self.record_foreign_object_descendant_hit_test_items(svg_box);
        }
        let kind = self.layout_kind(svg_box);
        if kind != Some(NodeKind::SVGSVGBox)
            && !kind.is_some_and(node_painting::is_svg)
            && kind.is_some_and(node_facts::kind_is_replaced_box)
        {
            self.trace_paint(Operation::Producer(svg_box, ProducerKind::DrawBackground), |this| {
                crate::painting::record::paint::paint(this, svg_box, PaintPhase::Background);
            });
        }
        self.trace_paint(Operation::Producer(svg_box, ProducerKind::DrawForeground), |this| {
            crate::painting::record::paint::paint(this, svg_box, PaintPhase::Foreground);
        });
        self.svg_paint_descendants(svg_box);
    }

    fn svg_paint_descendants(&mut self, paintable: NodeSlotId) {
        let mut next_child = crate::painting::paint_order::first_paint_child(self.source, paintable);
        while let Some(child) = next_child {
            next_child = crate::painting::paint_order::next_paint_sibling(self.source, child);
            // A child that establishes a stacking context is painted by that context.
            if self.has_stacking_context(child) {
                continue;
            }
            self.paint_svg_box(child);
        }
    }

    pub(crate) fn for_descendants_context(&self, paintable: NodeSlotId) -> ContextRef {
        if let Some(walk) = &self.svg_resource_walk {
            return walk.enclosing_context;
        }
        self.data(paintable).accumulated_visual_context_for_descendants
    }

    pub(crate) fn context_for_phase(&self, paintable: NodeSlotId, phase: PaintPhase) -> ContextRef {
        // Text fragments are content of the block container (or of a self-painting inline box).
        // They need the descendants' visual context, not the element's own visual context.
        let foreground_paints_descendant_content = node_painting::has_lines(self.source, paintable)
            || node_painting::is_fragmented_inline(self.source, paintable);
        if foreground_paints_descendant_content && phase == PaintPhase::Foreground {
            self.for_descendants_context(paintable)
        } else {
            self.own_context(paintable)
        }
    }

    pub(crate) fn paint(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        crate::painting::record::paint::paint(self, paintable, phase);
    }
}
