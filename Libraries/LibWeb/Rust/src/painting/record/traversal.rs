/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::cache::{CaptureKind, CaptureSite, narrow_record_gen};
use super::cache_compatibility::PaintCacheInputs;
use super::packed::PackedRecording;
use super::program::PaintProgram;
use super::resources::RecordingResourceManifest;
use super::scratch::RecordingScratch;
use super::svg_resources::MaskLayerSet;
use super::trace::{Action, Observer, Operation};
use super::verify::LoggedCapture;
use super::{PaintPhase, PaintRecorder, RecordingInputs, RecordingOutput, RecordingResult};
use crate::layout::node_data::{NodeFlag, NodeKind, NodeSlotId};
use crate::layout::{LayoutNodeArena, node_facts, used_values};
use crate::painting::display_list::builder::CommandRange;
use crate::painting::display_list::commands::{ContextRef, VISUAL_VIEWPORT_NODE_INDEX};
use crate::painting::display_list::device_pixels::DevicePixelConverter;
use crate::painting::display_list::recorder::DisplayListRecorder;
use crate::painting::force_dark::ForceDarkRole;
use crate::painting::hit_test::*;
use crate::painting::node_painting;
use crate::painting::paint_order_plan::PaintProducer;
pub(crate) use crate::painting::paint_order_plan::StackingContextPaintPhase;
use std::rc::Rc;
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_display_list(
    layout_arena: &LayoutNodeArena,
    paint_state: &crate::painting::paint_state::PaintState,
    scratch: &mut RecordingScratch,
    viewport: NodeSlotId,
    inputs: &RecordingInputs<'_>,
    hit_test_list_generation: u64,
    command_cache_source: Option<Rc<RecordingOutput>>,
    item_cache_source: Option<Rc<crate::painting::record::cache::HitTestItemCacheSource>>,
    trace: bool,
) -> RecordingResult {
    scratch.begin_recording(layout_arena.paintable_row_count());
    macro_rules! record {
        ($observer:ty) => {
            record_display_list_impl::<$observer>(
                layout_arena,
                paint_state,
                scratch,
                viewport,
                inputs,
                hit_test_list_generation,
                command_cache_source,
                item_cache_source,
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
    layout_arena: &LayoutNodeArena,
    paint_state: &crate::painting::paint_state::PaintState,
    scratch: &mut RecordingScratch,
    viewport: NodeSlotId,
    inputs: &RecordingInputs<'_>,
    hit_test_list_generation: u64,
    command_cache_source: Option<Rc<RecordingOutput>>,
    item_cache_source: Option<Rc<super::cache::HitTestItemCacheSource>>,
) -> RecordingResult {
    let completed = narrow_record_gen(layout_arena.paint_cache_completed_record_gen());
    let command_cache_source = command_cache_source.filter(|source| {
        source
            .paint_cache
            .as_ref()
            .is_some_and(|cache| cache.record_gen == completed)
    });
    let structural_epoch = paint_state.visual_context.structural_epoch();
    let cache_inputs = PaintCacheInputs::from_recording_inputs(inputs, paint_state);
    let cache_compatibility = command_cache_source.as_ref().map_or_else(Default::default, |source| {
        cache_inputs.compatibility_with(&source.cache_inputs)
    });
    let rows = layout_arena.paintable_rows();
    let update = PaintProgram::compile(
        &rows,
        viewport,
        inputs.should_paint_overlay,
        command_cache_source
            .as_ref()
            .and_then(|source| source.paint_cache.as_ref())
            .map(|cache| (&cache.program, cache.topology_revision)),
    );
    let packed = PackedRecording::new(
        update,
        command_cache_source
            .as_ref()
            .and_then(|source| source.paint_cache.as_ref()),
        layout_arena.paint_topology_revision(),
        layout_arena.paint_geometry_revision(),
    );
    let mut recorder = PaintRecorder {
        layout_arena: &rows,
        paint_state,
        inputs,
        recorder: DisplayListRecorder::new(inputs.force_dark_enabled.then_some(inputs.force_dark_settings)),
        converter: DevicePixelConverter::new(inputs.device_pixels_per_css_pixel),
        svg_resource_walk: None,
        viewport,
        command_cache_source,
        item_cache_source,
        cache_compatibility,
        packed: Some(packed),
        blocking_wheel_event_region_count: 0,
        uncacheable_paint_generation: 0,
        observer: O::default(),
        list: HitTestList {
            item_capacity_hint_from_previous_list: paint_state
                .hit_test_list
                .as_ref()
                .map_or(0, |list| list.items.len()),
            ..Default::default()
        },
        scratch,
        completed_record_gen: completed,
        all_paint_caches_dirty: layout_arena.all_paint_caches_dirty(),
        resources: RecordingResourceManifest::default(),
    };
    let shared = recorder.record_packed_program();
    let cache = recorder.packed.take().unwrap().finish(completed + 1);
    let mut hit_test_list = recorder.list;
    hit_test_list.generation = hit_test_list_generation;
    let recorded = recorder.recorder.into_builder().finish();
    let display_list = shared.unwrap_or_else(|| Arc::new(recorded));
    RecordingResult {
        output: RecordingOutput {
            recorded_structural_epoch: structural_epoch,
            cache_inputs,
            hit_test_list,
            display_list,
            has_blocking_wheel_event_listeners: recorder.blocking_wheel_event_region_count > 0,
            wheel_event_listener_state_generation: inputs.wheel_event_listener_state_generation,
            is_identical_to_cache_source: false,
            paint_cache: inputs.paint_command_cache_read_write.then_some(cache),
            capture_log_for_verification: recorder.observer.finish(),
        },
        resources: recorder.resources,
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    pub(super) fn record_canvas(&mut self) {
        let inputs = self.inputs;
        if let Some(rect) = inputs.canvas_fill_rect {
            self.recorder
                .fill_rect(rect, inputs.uncaptured.canvas_color, ForceDarkRole::Background);
        }
        // .. in the case of embedded documents typically rendered over a transparent canvas
        // (such as provided via an HTML iframe element), if the used color scheme of the element
        // and the used color scheme of the embedded document’s root element do not match,
        // then the UA must use an opaque canvas of the Canvas color appropriate to the
        // embedded document’s used color scheme instead of a transparent canvas.
        if inputs.opaque_canvas {
            self.recorder.fill_rect(
                inputs.bitmap_rect,
                inputs.uncaptured.canvas_color,
                ForceDarkRole::Background,
            );
        }
        self.recorder.fill_rect(
            inputs.bitmap_rect,
            inputs.uncaptured.background_color,
            ForceDarkRole::Background,
        );
    }

    pub(super) fn stacking_context_is_painted(&self, paintable: NodeSlotId) -> bool {
        // https://drafts.csswg.org/css-transforms-1/#transform-function-lists
        // If a transform function causes the current transformation matrix of an object to be
        // non-invertible, the object and its content do not get displayed. Retain content whose
        // transform is animated so the compositor can reveal it without a main-thread repaint.
        if self
            .data(paintable)
            .has_flag(crate::painting::paintable_data::PaintableFlag::HasNonInvertibleCssTransform)
            && self.layout_arena.node_flags_if_live(paintable) & NodeFlag::HasAnimatedOpacityOrTransform as u32 == 0
        {
            return false;
        }
        true
    }

    pub(super) fn prepare_stacking_context(&mut self, paintable: NodeSlotId) -> bool {
        debug_assert!(self.layout_arena.paintable_row_is_populated(paintable));
        if !self.layout_arena.paintable_row_is_populated(paintable) {
            return false;
        }
        if !self.stacking_context_is_painted(paintable) {
            return false;
        }
        let effective_context = self.own_context(paintable);
        self.recorder.set_accumulated_visual_context(effective_context);

        // For elements with SVG filters, emit a transparent FillRect to trigger filter application.
        // This ensures content-generating filters (feFlood, feImage) work even with empty source.
        if let Some(svg_filter_bounds) = self.layout_arena.paintable_side_data(paintable).svg_filter_bounds.get() {
            self.mark_open_captures_unsplicable();
            let device_rect = self
                .converter
                .enclosing_device_rect(crate::css::css_pixels::CssPixelRect::from(svg_filter_bounds));
            self.recorder.fill_rect_transparent(device_rect);
        }

        self.declare_mask_contents(paintable, MaskLayerSet::CssAndSvg);

        true
    }

    pub(super) fn has_inspector_overlays(&self) -> bool {
        self.inputs.inspector_highlight.is_some()
            || self.inputs.grid_overlays.is_some()
            || !self.inputs.flex_overlays.is_empty()
            || self.inputs.caret_debug_rect.is_some()
    }

    pub(super) fn execute_fresh_producer(&mut self, row: NodeSlotId, producer: PaintProducer) {
        match producer {
            PaintProducer::DrawBoxPhase(phase) => self.paint_box_commands(row, phase),
            PaintProducer::HitTestPhase(phase) => self.paint_hit_test_phase(row, phase),
            PaintProducer::ScrollMetadata => self.paint_scroll_metadata(row),
            PaintProducer::ScopePreamble => {
                self.prepare_stacking_context(row);
            }
            PaintProducer::Canvas => self.trace_paint(Operation::Producer(None, "canvas"), |this| this.record_canvas()),
            PaintProducer::InspectorOverlays => {
                if self.has_inspector_overlays() {
                    self.trace_paint(
                        Operation::Producer(None, "inspector-overlays"),
                        super::paint::inspector_overlay::record_inspector_overlays,
                    );
                }
            }
            PaintProducer::Svg(phase) => self.paint_svg(row, phase),
            PaintProducer::SvgBoxForeground => self.paint_svg_box(row, PaintPhase::Foreground),
        }
    }

    fn paint_hit_test_phase(&mut self, row: NodeSlotId, phase: PaintPhase) {
        if self.is_recording_svg_resource_content() {
            return;
        }
        self.recorder
            .set_accumulated_visual_context(self.context_for_phase(row, phase));
        let start = self.list.items.len();
        self.record_hit_test_items(row, phase);
        self.log_hit_test_item_capture_for_verification(
            row,
            CaptureKind::BoxPhase(phase),
            start,
            self.list.items.len() - start,
            false,
        );
        if phase == PaintPhase::Overlay
            && self.list.items.len() != start
            && (self.data(row).own_scroll_node_index != VISUAL_VIEWPORT_NODE_INDEX
                || self.layout_kind(row) == Some(NodeKind::Viewport))
        {
            self.mark_open_captures_unsplicable();
        }
    }

    fn paint_scroll_metadata(&mut self, row: NodeSlotId) {
        if self.is_recording_svg_resource_content() {
            return;
        }
        self.recorder.set_accumulated_visual_context(self.own_context(row));
        self.trace_paint(Operation::Producer(Some(row), "scroll-metadata"), |this| {
            this.record_async_scrolling_metadata(row);
        });
    }

    fn paint_box_commands(&mut self, row: NodeSlotId, phase: PaintPhase) {
        self.recorder
            .set_accumulated_visual_context(self.context_for_phase(row, phase));
        let facts = self.base_paint_facts(row);
        if phase == PaintPhase::Background
            && (facts.has_fixed_background || facts.has_scroll_offset_dependent_background)
        {
            self.mark_open_captures_unsplicable();
        }
        let site = CaptureSite {
            paintable: row,
            kind: CaptureKind::BoxPhase(phase),
        };
        if facts.paint_phase_mask & phase.bit() == 0 {
            self.observer
                .observe(|log| log.leaf(Operation::Capture(site), Action::Skip, true));
            self.recorder.set_accumulated_visual_context(ContextRef::default());
            return;
        }
        let start = self.recorder.byte_size();
        self.trace_paint(Operation::Capture(site), |this| this.paint(row, phase));
        let end = self.recorder.byte_size();
        if !self.is_recording_svg_resource_content() {
            self.log_command_byte_capture_for_verification(
                row,
                site.kind,
                CommandRange {
                    offset: start as u32,
                    size: (end - start) as u32,
                },
                false,
            );
        }
        if phase == PaintPhase::Overlay
            && end != start
            && (self.data(row).own_scroll_node_index != VISUAL_VIEWPORT_NODE_INDEX
                || self.layout_kind(row) == Some(NodeKind::Viewport))
        {
            self.mark_open_captures_unsplicable();
        }
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    pub(crate) fn paint_svg(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        if phase != PaintPhase::Foreground {
            return;
        }
        self.paint_node(paintable, PaintPhase::Background);
        self.paint_node(paintable, PaintPhase::Border);
        self.paint_svg_box(paintable, phase);
    }

    pub(super) fn log_command_byte_capture_for_verification(
        &mut self,
        paintable: NodeSlotId,
        kind: CaptureKind,
        range: CommandRange,
        spliced_from_cache: bool,
    ) {
        self.observer.observe(|log| {
            if spliced_from_cache && matches!(kind, CaptureKind::BoxPhase(_)) {
                log.leaf(
                    Operation::Capture(CaptureSite { paintable, kind }),
                    Action::Reuse,
                    range.size == 0,
                );
            }
            log.command_byte_captures.push(LoggedCapture {
                start: range.offset,
                length: range.size,
                paintable,
                kind,
                spliced_from_cache,
            });
        });
    }

    pub(super) fn log_hit_test_item_capture_for_verification(
        &mut self,
        paintable: NodeSlotId,
        kind: CaptureKind,
        start: usize,
        count: usize,
        spliced_from_cache: bool,
    ) {
        self.observer.observe(|log| {
            if let CaptureKind::BoxPhase(phase) = kind {
                log.leaf(
                    Operation::HitTest(paintable, phase),
                    if spliced_from_cache {
                        Action::Reuse
                    } else {
                        Action::Record
                    },
                    count == 0,
                );
            }
            log.hit_test_item_captures.push(LoggedCapture {
                start: start as u32,
                length: count as u32,
                paintable,
                kind,
                spliced_from_cache,
            });
        });
    }

    pub(super) fn current_absolute_position(&mut self, paintable: NodeSlotId) -> used_values::FfiCssPixelPoint {
        if let Some(position) = self.scratch.absolute_position(paintable) {
            return position;
        }
        let position: used_values::FfiCssPixelPoint =
            crate::painting::paintable_geometry::absolute_position(self.layout_arena, paintable).into();
        self.scratch.set_absolute_position(paintable, position);
        position
    }

    pub(super) fn paint_svg_box(&mut self, svg_box: NodeSlotId, phase: PaintPhase) {
        self.trace_scope(Operation::Producer(Some(svg_box), "svg"), Action::Walk, |this| {
            this.paint_svg_box_impl(svg_box, phase);
        });
    }

    fn paint_svg_box_impl(&mut self, svg_box: NodeSlotId, phase: PaintPhase) {
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
        if let Some(svg_filter_bounds) = self.layout_arena.paintable_side_data(svg_box).svg_filter_bounds.get() {
            self.mark_open_captures_unsplicable();
            let device_rect = self
                .converter
                .enclosing_device_rect(crate::css::css_pixels::CssPixelRect::from(svg_filter_bounds));
            self.recorder.fill_rect_transparent(device_rect);
        }

        if self.declare_mask_contents(svg_box, MaskLayerSet::SvgOnly) {
            return;
        }
        let before = self.list.items.len();
        self.record_hit_test_items(svg_box, phase);
        self.observer.observe(|log| {
            log.leaf(
                Operation::HitTest(svg_box, phase),
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
            self.trace_paint(
                Operation::Capture(CaptureSite {
                    paintable: svg_box,
                    kind: CaptureKind::BoxPhase(PaintPhase::Background),
                }),
                |this| crate::painting::record::paint::paint(this, svg_box, PaintPhase::Background),
            );
        }
        self.trace_paint(
            Operation::Capture(CaptureSite {
                paintable: svg_box,
                kind: CaptureKind::BoxPhase(PaintPhase::Foreground),
            }),
            |this| crate::painting::record::paint::paint(this, svg_box, PaintPhase::Foreground),
        );
        self.svg_paint_descendants(svg_box, phase);
    }

    fn svg_paint_descendants(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        if phase != PaintPhase::Foreground {
            return;
        }
        let mut next_child = crate::painting::paint_order::first_paint_child(self.layout_arena, paintable);
        while let Some(child) = next_child {
            next_child = crate::painting::paint_order::next_paint_sibling(self.layout_arena, child);
            // A child that establishes a stacking context is painted by that context.
            if self.has_stacking_context(child) {
                continue;
            }
            self.paint_svg_box(child, phase);
        }
    }

    pub(super) fn for_descendants_context(&self, paintable: NodeSlotId) -> ContextRef {
        if let Some(walk) = &self.svg_resource_walk {
            return walk.enclosing_context;
        }
        self.data(paintable).accumulated_visual_context_for_descendants
    }

    pub(super) fn context_for_phase(&self, paintable: NodeSlotId, phase: PaintPhase) -> ContextRef {
        // Text fragments are content of the block container (or of a self-painting inline box).
        // They need the descendants' visual context, not the element's own visual context.
        let foreground_paints_descendant_content = node_painting::has_lines(self.layout_arena, paintable)
            || node_painting::is_inline(self.layout_arena, paintable);
        if foreground_paints_descendant_content && phase == PaintPhase::Foreground {
            self.for_descendants_context(paintable)
        } else {
            self.own_context(paintable)
        }
    }

    pub(crate) fn paint_node(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        // Opaque SVG/resource units can still invoke complete box phases locally.
        if matches!(
            phase,
            PaintPhase::Background | PaintPhase::Foreground | PaintPhase::Overlay
        ) {
            self.paint_hit_test_phase(paintable, phase);
        }
        if phase == PaintPhase::Background {
            self.paint_scroll_metadata(paintable);
        }
        self.paint_box_commands(paintable, phase);
    }

    pub(super) fn append_spliced_hit_test_item(&mut self, spliced: &HitTestItem) {
        let mut item = spliced.clone();
        if !item.block_container.is_invalid() {
            item.block_container_margin_rect = self.containing_block_margin_rect(item.block_container);
        }
        if item.kind == HitTestItemKind::Box {
            (item.caret_line_index, item.caret_line_rect) = self.containing_line_of_box(item.paintable);
        }
        self.list.append(item);
    }

    fn paint(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        crate::painting::record::paint::paint(self, paintable, phase);
    }
}
