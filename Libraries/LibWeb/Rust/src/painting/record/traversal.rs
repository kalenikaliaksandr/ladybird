/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::painting::record::trace::Observer;

use super::{PaintPhase, PaintRecorder};
use crate::layout::node_data::{NodeKind, NodeSlotId};
use crate::layout::node_facts;
use crate::painting::display_list::commands::ContextRef;
use crate::painting::display_list::device_pixels::DevicePixelConverter;
use crate::painting::display_list::recorder::DisplayListRecorder;
use crate::painting::hit_test::HitTestList;
use crate::painting::node_painting;
use crate::painting::paint_order_plan::PaintScope;
use crate::painting::paint_read::{GeometryRead, PaintSource};
use crate::painting::record::RecordingInputs;
use crate::painting::record::assemble::{Assembler, frame_is_unchanged};
use crate::painting::record::frame_inputs::FrameInputs;
use crate::painting::record::order_tree::ProducerKind;
use crate::painting::record::paint::background_resolution::root_background_canvas_rect;
use crate::painting::record::resources::RecordingResourceManifest;
use crate::painting::record::scratch::RecordingScratch;
use crate::painting::record::svg_resources::MaskLayerSet;
use crate::painting::record::trace::{Action, Operation};
use crate::painting::record::{PublishedHitTestItems, RecordingOutput, RecordingResult};
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_display_list(
    source: &PaintSource<'_>,
    scratch: &mut RecordingScratch,
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    source_recording: Option<Arc<RecordingOutput>>,
    source_items: Option<Arc<PublishedHitTestItems>>,
    trace: bool,
) -> RecordingResult {
    scratch.begin_recording(source.frame().paintable_row_capacity());
    macro_rules! record {
        ($observer:ty) => {
            record_display_list_impl::<$observer>(source, scratch, viewport, inputs, source_recording, source_items)
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
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    source_recording: Option<Arc<RecordingOutput>>,
    source_items: Option<Arc<PublishedHitTestItems>>,
) -> RecordingResult {
    let paint_state = source.frame().paint_state();
    let structural_epoch = paint_state.structural_epoch();
    let frame_inputs = FrameInputs::from_recording_inputs(inputs, paint_state);
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
    let force_dark_settings = inputs.force_dark_enabled.then_some(inputs.force_dark_settings);
    let mut recorder = PaintRecorder {
        source,
        paint_state,
        inputs,
        recorder: DisplayListRecorder::new(force_dark_settings),
        converter: DevicePixelConverter::new(inputs.device_pixels_per_css_pixel),
        svg_resource_walk: None,
        viewport,
        ink_bounds_audit: super::ink_bounds::InkBoundsAudit::new(),
        source_recording,
        source_items,
        live_producer: false,
        producer_reads_window_focus: std::cell::Cell::new(false),
        line_root_positions: None,
        blocking_wheel_event_region_count: 0,
        records_per_box_wheel_hit_test_targets: frame_inputs.records_per_box_wheel_hit_test_targets(),
        observer: O::default(),
        list: HitTestList {
            item_capacity_hint_from_previous_list: paint_state.hit_test_item_capacity_hint,
            ..HitTestList::default()
        },
        scratch,
        resources: RecordingResourceManifest::default(),
    };
    recorder
        .observer
        .observe(|log| log.damage = Some(source.frame().damage().summary()));
    let root_group_color = recorder.record_canvas();
    recorder.recorder.end_chunk();
    let prologue_bytes = u32::try_from(recorder.recorder.byte_size()).expect("display list exceeds u32");
    let has_inspector_overlays = inputs.inspector_highlight.is_some()
        || inputs.grid_overlays.is_some()
        || !inputs.flex_overlays.is_empty()
        || inputs.caret_debug_rect.is_some();
    let root_scope = PaintScope::stacking_context(viewport);
    // The scopes the published recording walked, where it can be copied from.
    let published_root = recorder
        .source_recording
        .as_ref()
        .and_then(|frame| Some((frame.recorded_root.clone()?, frame.prologue_bytes)));
    // Nothing was pushed and nothing records every frame: the published tape and items are
    // this frame, which lets the compositor skip its update as well.
    let unchanged_frame = recorder
        .source_recording
        .as_ref()
        .zip(recorder.source_items.as_ref())
        .zip(published_root.as_ref())
        .filter(|((frame, _), (root, _))| {
            frame_is_unchanged(root, !source.frame().damage().is_empty(), window_focus_changed)
                && !has_inspector_overlays
                && frame.prologue_bytes == prologue_bytes
                && recorder.recorder.bytes() == &frame.display_list.bytes[..prologue_bytes as usize]
        })
        .map(|((frame, items), (root, _))| (frame.display_list.clone(), items.items.clone(), Arc::clone(root)));
    let copies_from_published_recording = published_root.is_some() && unchanged_frame.is_none();
    let (display_list, recorded_root) = match unchanged_frame {
        Some((display_list, items, root)) => {
            recorder
                .observer
                .observe(|log| log.leaf(Operation::Scope(root_scope), Action::Copy, false));
            recorder.list.items = items;
            recorder.blocking_wheel_event_region_count = root.output.blocking_wheel_event_regions;
            (display_list, root)
        }
        None => {
            let recorded_root = Assembler::new(&mut recorder, window_focus_changed).assemble_root(
                root_scope,
                published_root
                    .as_ref()
                    .map(|(root, prologue_bytes)| (root, *prologue_bytes)),
            );
            if has_inspector_overlays {
                recorder.trace_paint(
                    Operation::Named(None, "inspector-overlays"),
                    crate::painting::record::paint::inspector_overlay::record_inspector_overlays,
                );
            }
            (Arc::new(recorder.recorder.into_builder().finish()), recorded_root)
        }
    };
    recorder.ink_bounds_audit.report();
    let mut hit_test_list = recorder.list;
    hit_test_list.generation = paint_state.hit_test_list_generation + 1;
    hit_test_list.recorded_structural_epoch = structural_epoch;
    let output = RecordingOutput {
        recorded_structural_epoch: structural_epoch,
        frame_inputs,
        root_background_canvas_rect,
        prologue_bytes,
        root_group_color,
        hit_test_list,
        display_list,
        recorded_root: Some(recorded_root),
        has_blocking_wheel_event_listeners: recorder.blocking_wheel_event_region_count > 0,
        wheel_event_listener_state_generation: inputs.wheel_event_listener_state_generation,
        is_identical_to_published_recording: false,
        window_is_focused: inputs.window_is_focused,
        capture_log: recorder.observer.finish(),
        vector_image_display_lists: Vec::new(),
    };
    let resources = recorder.resources;
    if copies_from_published_recording && copied_output_is_verified() {
        let from_scratch =
            record_display_list_impl::<super::trace::NoTrace>(source, scratch, viewport, inputs, None, None).output;
        require_same_recording(&output, &from_scratch);
    }
    RecordingResult { output, resources }
}

fn copied_output_is_verified() -> bool {
    static VERIFIED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERIFIED.get_or_init(|| cfg!(debug_assertions) || std::env::var_os("LIBWEB_VERIFY_PAINT_REUSE").is_some())
}

/// Requires a recording that copied clean output from the published one to be the recording the same frame gives
/// when everything is recorded: a difference is output that was copied though what it shows changed.
fn require_same_recording(assembled: &RecordingOutput, from_scratch: &RecordingOutput) {
    use crate::painting::display_list::builder::{RecordedDisplayList, for_each_command};
    use crate::painting::display_list::chunks::{chunk_id, chunks_of};
    use crate::painting::display_list::commands::{DisplayListCommandType, PaintNestedDisplayList};
    // A command that paints an SVG image is recorded with a placeholder for the image's display list, which
    // publication replaces with the list the host rendered. A copied command names that list already.
    let without_nested_display_list_ids = |bytes: &[u8]| {
        let mut bytes = bytes.to_vec();
        let id_field = std::mem::offset_of!(PaintNestedDisplayList, display_list_id);
        let mut id_offsets = Vec::new();
        crate::painting::display_list::nested_records::for_each_command_including_nested(
            &bytes,
            &mut |command_type, payload_offset, _| {
                if command_type == DisplayListCommandType::PaintNestedDisplayList {
                    id_offsets.push(payload_offset + id_field);
                }
            },
        );
        for offset in id_offsets {
            bytes[offset..offset + std::mem::size_of::<u64>()].fill(0);
        }
        bytes
    };
    let (assembled_list, scratch_list) = (&assembled.display_list, &from_scratch.display_list);
    let assembled_bytes = without_nested_display_list_ids(&assembled_list.bytes);
    let scratch_bytes = without_nested_display_list_ids(&scratch_list.bytes);
    if assembled_bytes != scratch_bytes {
        let first_differing_byte = assembled_bytes
            .iter()
            .zip(&scratch_bytes)
            .position(|(copied, recorded)| copied != recorded)
            .unwrap_or(assembled_bytes.len().min(scratch_bytes.len()));
        let command_at = |bytes: &[u8]| {
            let mut found = None;
            for_each_command(bytes, |header, offset, _| {
                if offset <= first_differing_byte {
                    found = Some((offset, header.command_type.name(), header.bounding_rect));
                }
            });
            found
        };
        panic!(
            "A recording copied output that recording from scratch replaces: {} bytes against {}, differing from byte {first_differing_byte}, in {:?} where recording from scratch has {:?}",
            assembled_list.bytes.len(),
            scratch_list.bytes.len(),
            command_at(&assembled_list.bytes),
            command_at(&scratch_list.bytes)
        );
    }
    assert!(
        assembled_list.command_runs == scratch_list.command_runs,
        "A recording copied command runs that recording from scratch replaces"
    );
    let chunk_ends = |list: &RecordedDisplayList| -> Vec<(u32, u32)> {
        list.chunks
            .iter()
            .map(|chunk| (chunk.byte_end, chunk.run_end))
            .collect()
    };
    assert!(
        chunk_ends(assembled_list) == chunk_ends(scratch_list),
        "A recording cut its tape into other chunks than recording from scratch does"
    );
    for (chunk, bytes, runs) in chunks_of(
        &assembled_list.bytes,
        &assembled_list.command_runs,
        &assembled_list.chunks,
    ) {
        let chunk_start = chunk.byte_end - bytes.len() as u32;
        assert!(
            chunk.id == chunk_id(bytes, runs, chunk_start),
            "A recording copied a chunk under a name that is not the name of what it holds"
        );
    }
    assert!(
        assembled.hit_test_list.items == from_scratch.hit_test_list.items,
        "A recording copied hit-test items that recording from scratch replaces"
    );
    assert!(
        assembled.has_blocking_wheel_event_listeners == from_scratch.has_blocking_wheel_event_listeners,
        "A recording copied wheel regions that recording from scratch replaces"
    );
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
        let before = self.list.items.len();
        self.record_hit_test_items(svg_box, PaintPhase::Foreground);
        self.observer.observe(|log| {
            log.leaf(
                Operation::Producer(svg_box, ProducerKind::HitForeground),
                Action::Record,
                self.list.items.len() == before,
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
