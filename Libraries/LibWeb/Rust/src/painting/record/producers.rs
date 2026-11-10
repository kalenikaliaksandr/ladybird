/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The producers of a frame: each records one kind of output for one row under the contexts it
//! needs, starting and ending at a closed group boundary with no ambient state, so its bytes and
//! hit-test items can be copied verbatim by a later frame. The recorder implements the assembly
//! host with them.

use super::assemble::{AssemblyEvent, AssemblyHost, ItemCut, OutputPosition, ProducerOutcome, ScopeAction, ScopePlan};
use super::order_tree::{InkBound, ProducerKind, RowPlacement};
use super::trace::{Action, Observer, Operation};
use super::{PaintPhase, PaintRecorder};
use crate::css::css_pixels::{CssPixelRect, CssPixels};
use crate::layout::node_data::NodeSlotId;
use crate::painting::display_list::commands::ContextRef;
use crate::painting::force_dark::ForceDarkRole;
use crate::painting::paint_order_plan::{PaintOrderItem, PaintProducer, PaintScope, PaintScopePlan};
use crate::painting::paint_read::{GeometryRead, PaintRead};
use crate::painting::record::damage::PaintDamage;
use crate::painting::record::svg_resources::MaskLayerSet;
use crate::painting::{node_painting, paintable_geometry};
use libgfx_rust::Color;
use smallvec::SmallVec;
use std::ops::Range;

/// The height in CSS pixels of a row of the grid the plans of walked scopes are cut along. A child scope taller
/// than a row is walked on its own, and a run ends where the next item starts in another row, so a run records about
/// a row of the document.
const GRID_ROW_HEIGHT_IN_CSS_PIXELS: i64 = 512;

fn draw_phase(kind: ProducerKind) -> Option<PaintPhase> {
    match kind {
        ProducerKind::DrawBackground => Some(PaintPhase::Background),
        ProducerKind::DrawBorder => Some(PaintPhase::Border),
        ProducerKind::DrawTableCollapsedBorder => Some(PaintPhase::TableCollapsedBorder),
        ProducerKind::DrawForeground => Some(PaintPhase::Foreground),
        ProducerKind::DrawOutline => Some(PaintPhase::Outline),
        ProducerKind::DrawOverlay => Some(PaintPhase::Overlay),
        _ => None,
    }
}

fn draw_kind(phase: PaintPhase) -> ProducerKind {
    match phase {
        PaintPhase::Background => ProducerKind::DrawBackground,
        PaintPhase::Border => ProducerKind::DrawBorder,
        PaintPhase::TableCollapsedBorder => ProducerKind::DrawTableCollapsedBorder,
        PaintPhase::Foreground => ProducerKind::DrawForeground,
        PaintPhase::Outline => ProducerKind::DrawOutline,
        PaintPhase::Overlay => ProducerKind::DrawOverlay,
    }
}

pub(crate) fn hit_phase(kind: ProducerKind) -> Option<PaintPhase> {
    match kind {
        ProducerKind::HitBackground => Some(PaintPhase::Background),
        ProducerKind::HitForeground => Some(PaintPhase::Foreground),
        ProducerKind::HitOverlay => Some(PaintPhase::Overlay),
        _ => None,
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    pub(crate) fn record_canvas(&mut self) -> Color {
        let mut root_group_color = Color::TRANSPARENT;
        self.trace_paint(Operation::Named(None, "canvas"), |this| {
            let inputs = this.inputs;
            let canvas_color = this
                .recorder
                .resolve_color(inputs.uncaptured.canvas_color, ForceDarkRole::Background);
            if let Some(rect) = inputs.canvas_fill_rect {
                this.recorder
                    .fill_rect(rect, inputs.uncaptured.canvas_color, ForceDarkRole::Background);
                root_group_color = canvas_color;
            }
            // .. in the case of embedded documents typically rendered over a transparent canvas
            // (such as provided via an HTML iframe element), if the used color scheme of the element
            // and the used color scheme of the embedded document’s root element do not match,
            // then the UA must use an opaque canvas of the Canvas color appropriate to the
            // embedded document’s used color scheme instead of a transparent canvas.
            if inputs.opaque_canvas {
                this.recorder.fill_rect(
                    inputs.bitmap_rect,
                    inputs.uncaptured.canvas_color,
                    ForceDarkRole::Background,
                );
                root_group_color = canvas_color;
            }
            this.recorder.fill_rect(
                inputs.bitmap_rect,
                inputs.uncaptured.background_color,
                ForceDarkRole::Background,
            );
            root_group_color = root_group_color.blend(
                this.recorder
                    .resolve_color(inputs.uncaptured.background_color, ForceDarkRole::Background),
            );
        });
        root_group_color
    }

    // The transparent fill that triggers a content-generating SVG filter, and the mask
    // contents of a stacking context, recorded before its content.
    fn record_scope_preamble(&mut self, owner: NodeSlotId) {
        let context = self.own_context(owner);
        self.recorder.set_accumulated_visual_context(context);
        if let Some(svg_filter_bounds) = self.source.committed_side_data(owner).svg_filter_bounds {
            let device_rect = self
                .converter
                .enclosing_device_rect(CssPixelRect::from(svg_filter_bounds));
            self.recorder.fill_rect_transparent(device_rect);
        }
        self.declare_mask_contents(owner, MaskLayerSet::CssAndSvg);
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    fn record_hit_test_phase(&mut self, owner: NodeSlotId, phase: PaintPhase) {
        let context = self.context_for_phase(owner, phase);
        self.recorder.set_accumulated_visual_context(context);
        self.record_hit_test_items(owner, phase);
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    fn record_scroll_metadata(&mut self, owner: NodeSlotId) {
        let context = self.own_context(owner);
        self.recorder.set_accumulated_visual_context(context);
        self.record_async_scrolling_metadata(owner);
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    fn record_box_commands(&mut self, owner: NodeSlotId, phase: PaintPhase) {
        let facts = self.base_paint_facts(owner);
        // Scrolling repaints without pushing damage, so output that depends on the scroll
        // offset records every frame.
        if facts.has_fixed_background
            || (facts.has_scroll_offset_dependent_background && phase == PaintPhase::Background)
        {
            self.mark_live_producer();
        }
        let context = self.context_for_phase(owner, phase);
        self.recorder.set_accumulated_visual_context(context);
        self.paint(owner, phase);
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    fn record_svg_box_foreground(&mut self, owner: NodeSlotId) {
        self.paint_svg_box_impl(owner);
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }

    fn producer_is_skipped(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        draw_phase(kind).is_some_and(|phase| self.base_paint_facts(owner).paint_phase_mask & phase.bit() == 0)
    }

    /// The commands of a box phase inside a resource walk, which records into the referencing
    /// producer rather than into an entry of its own.
    pub(crate) fn record_box_phase_inside_resource(&mut self, owner: NodeSlotId, phase: PaintPhase) {
        debug_assert!(self.is_recording_svg_resource_content());
        let kind = draw_kind(phase);
        if self.producer_is_skipped(owner, kind) {
            self.observer
                .observe(|log| log.leaf(Operation::Producer(owner, kind), Action::Skip, true));
            return;
        }
        let context = self.context_for_phase(owner, phase);
        self.recorder.set_accumulated_visual_context(context);
        self.trace_paint(Operation::Producer(owner, kind), |this| this.paint(owner, phase));
    }
}

impl<O: Observer> AssemblyHost for PaintRecorder<'_, O> {
    fn plan_scope(&mut self, scope: PaintScope) -> ScopePlan {
        let plan = PaintScopePlan::build(self.source, scope, self.inputs.should_paint_overlay);
        let active = !plan.establishes_stacking_context
            || crate::painting::hit_test::walk::stacking_context_paints(self.source, scope.owner);
        ScopePlan {
            active,
            items: if active { plan.items } else { SmallVec::new() },
        }
    }

    fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> ProducerOutcome {
        debug_assert!(
            self.recorder.is_producer_boundary(),
            "a producer starts at a closed group boundary without ambient state"
        );
        if self.producer_is_skipped(owner, kind) {
            self.observer
                .observe(|log| log.leaf(Operation::Producer(owner, kind), Action::Skip, true));
            return ProducerOutcome::default();
        }
        self.live_producer = false;
        self.producer_reads_window_focus.set(false);
        let start = self.output_position();
        self.observer
            .observe(|log| log.begin(Operation::Producer(owner, kind), Action::Record));
        match kind {
            ProducerKind::ScopePreamble => self.record_scope_preamble(owner),
            ProducerKind::ScrollMetadata => self.record_scroll_metadata(owner),
            ProducerKind::Svg => self.record_svg_box_foreground(owner),
            ProducerKind::InlinePiece(index) | ProducerKind::TextFragment(index) => {
                let root = if crate::painting::node_painting::has_lines(self.source, owner) {
                    owner
                } else {
                    self.data(owner).containing_block
                };
                if matches!(kind, ProducerKind::InlinePiece(_)) {
                    super::paint::inline_box::paint_piece(self, root, index);
                } else {
                    let context = self.context_for_phase(owner, PaintPhase::Foreground);
                    self.recorder.set_accumulated_visual_context(context);
                    super::paint::text::paint_fragments(self, root, &[index]);
                    self.recorder.set_accumulated_visual_context(ContextRef::default());
                }
            }
            _ => {
                if let Some(phase) = hit_phase(kind) {
                    self.record_hit_test_phase(owner, phase);
                } else if let Some(phase) = draw_phase(kind) {
                    self.record_box_commands(owner, phase);
                }
            }
        }
        let end = self.output_position();
        if self.ink_bounds_audit.enabled {
            self.verify_producer_ink(owner, kind, start.bytes as usize..end.bytes as usize);
            self.verify_producer_against_open_scopes(
                owner,
                kind,
                start.bytes as usize..end.bytes as usize,
                start.hits as usize..end.hits as usize,
            );
        }
        self.observer
            .observe(|log| log.end(end.bytes == start.bytes && end.hits == start.hits));
        debug_assert!(
            self.recorder.is_producer_boundary(),
            "a producer ends at a closed group boundary without ambient state"
        );
        ProducerOutcome {
            live: self.live_producer,
            reads_window_focus: self.producer_reads_window_focus.get(),
        }
    }

    fn output_position(&self) -> OutputPosition {
        OutputPosition {
            bytes: u32::try_from(self.recorder.byte_size()).expect("display list exceeds u32"),
            hits: u32::try_from(self.list.items.len()).expect("hit-test list exceeds u32"),
            blocking_wheel_event_regions: self.blocking_wheel_event_region_count,
        }
    }

    fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32) {
        let frame = self
            .source_recording
            .as_ref()
            .expect("clean output is copied from a published recording");
        let items = self
            .source_items
            .as_ref()
            .expect("clean output is copied from a published recording");
        self.recorder.append_cached_commands(&frame.display_list, bytes);
        if !hits.is_empty() {
            self.list
                .append_copies_of(&items.items[hits.start as usize..hits.end as usize]);
        }
        self.blocking_wheel_event_region_count += blocking_wheel_event_regions;
    }

    fn end_recorded_run(&mut self) {
        self.recorder.end_chunk();
    }

    fn damage_of_row(&mut self, row: NodeSlotId) -> PaintDamage {
        self.source.paint_damage_of_row(row)
    }

    fn placement_of_row(&mut self, row: NodeSlotId) -> RowPlacement {
        use crate::painting::paintable_data::PaintableFlag;
        use std::hash::Hasher;
        // A fragmented inline is placed by the pieces its line root holds, and the offset of its own fragment is
        // the one layout left there last.
        let placed_with = if node_painting::is_fragmented_inline(self.source, row) {
            self.source.inline_pieces_root(row).unwrap_or(row)
        } else {
            row
        };
        let mut later_fragments = 0;
        if self
            .source
            .paintable_data(placed_with)
            .has_flag(PaintableFlag::HasLaterBoxFragments)
        {
            let mut digest = crate::fast_hash::fast_hasher();
            for fragment in paintable_geometry::absolute_border_box_rects_of_fragments(self.source, placed_with)
                .iter()
                .skip(1)
            {
                for coordinate in [fragment.x, fragment.y, fragment.width, fragment.height] {
                    digest.write_i32(coordinate.raw_value());
                }
            }
            later_fragments = digest.finish();
        }
        RowPlacement {
            origin: paintable_geometry::absolute_rect(self.source, placed_with).location(),
            later_fragments,
        }
    }

    fn cut_at(&mut self, scope: PaintScope, item: PaintOrderItem) -> ItemCut {
        let rows = self.source;
        let grid_row_height = CssPixels::from_integer(if self.inputs.records_in_finest_runs {
            1
        } else {
            GRID_ROW_HEIGHT_IN_CSS_PIXELS
        });
        let grid_row_at = |block_start: CssPixels| {
            Some(i64::from(block_start.raw_value()).div_euclid(i64::from(grid_row_height.raw_value())) as i32)
        };
        let line_root = || {
            if node_painting::has_lines(rows, scope.owner) {
                scope.owner
            } else {
                rows.paintable_data(scope.owner).containing_block
            }
        };
        match item {
            PaintOrderItem::Scope(child) => {
                // The pieces of an inline box are spread over its line root's lines: it stays with what is next
                // to it in the plan.
                if node_painting::is_fragmented_inline(rows, child.owner) {
                    return ItemCut::InRun { grid_row: None };
                }
                if crate::painting::style_queries::is_scroll_container(rows, child.owner) {
                    return ItemCut::Walked;
                }
                match rows.absolute_ink_overflow_rect(child.owner) {
                    Some(ink) if ink.height <= grid_row_height => ItemCut::InRun {
                        grid_row: grid_row_at(ink.y),
                    },
                    _ => ItemCut::Walked,
                }
            }
            PaintOrderItem::Producer(PaintProducer::TextFragment(index)) => {
                let side = rows.committed_side_data(line_root());
                let fragment = &side.fragments()[index as usize];
                ItemCut::InRun {
                    grid_row: grid_row_at(crate::painting::text_fragment::absolute_rect(rows, fragment).y),
                }
            }
            PaintOrderItem::Producer(PaintProducer::InlinePiece(index)) => {
                let root = line_root();
                let positions = self.line_root_positions(root);
                let side = rows.committed_side_data(root);
                let piece = &side.inline_box_pieces()[index as usize];
                let line = positions.of_line(side.lines(), piece.line_index as usize);
                ItemCut::InRun {
                    grid_row: grid_row_at(piece.border_box_rect.y + line.y),
                }
            }
            PaintOrderItem::Producer(_) => ItemCut::InRun { grid_row: None },
        }
    }

    // A producer of the scope's owner is not bounded: the owner of a walked scope is in the window when the scope
    // is, and its own producers mostly cover it.
    fn bound_of_item(&mut self, _scope: PaintScope, item: PaintOrderItem) -> Option<InkBound> {
        use crate::painting::ink_overflow::{ScopeInkBound, scope_ink_bound};
        let PaintOrderItem::Scope(child) = item else {
            return None;
        };
        let ScopeInkBound::Rect(bound) = scope_ink_bound(self.source, child) else {
            return None;
        };
        // The bound says where a transform below the child has put what it moves, and nothing asks for a recording
        // when the transform puts it elsewhere.
        if self
            .source
            .stored_ink_overflow(child.owner)
            .holds_what_a_transform_moves
        {
            return None;
        }
        let slack = super::ink_bounds::COMPARISON_SLACK_IN_DEVICE_PIXELS;
        Some(InkBound {
            rect: self
                .converter
                .enclosing_device_rect(bound)
                .inflated_edges(slack, slack, slack, slack)
                .to_float(),
            space: self.own_context(child.owner).spatial,
        })
    }

    fn united_bound(&mut self, first: InkBound, second: InkBound) -> Option<InkBound> {
        self.window_spaces.as_mut()?.united(first, second)
    }

    fn is_outside_window(&mut self, bound: InkBound) -> bool {
        self.window_spaces
            .as_mut()
            .is_some_and(|spaces| spaces.is_outside(bound))
    }

    fn is_wholly_inside_window(&mut self, bound: InkBound) -> bool {
        self.window_spaces
            .as_mut()
            .is_some_and(|spaces| spaces.is_wholly_inside(bound))
    }

    // Any box whose overflow is not visible, though not every one of them clips: saying so of a box that does not
    // only has its scopes ask what bounds their segments.
    fn cuts_off_what_it_holds(&mut self, owner: NodeSlotId) -> bool {
        use crate::css::css_enums::overflow;
        self.source
            .node_style_if_live(owner)
            .is_some_and(|style| style.overflow_x() != overflow::VISIBLE || style.overflow_y() != overflow::VISIBLE)
    }

    // SVG content is recorded by its root.
    fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        match kind {
            ProducerKind::Svg => true,
            ProducerKind::ScopePreamble => self.paints_svg_mask_or_clip_resource_subtree(owner),
            ProducerKind::InlinePiece(_) | ProducerKind::TextFragment(_) => true,
            _ => false,
        }
    }

    fn observe(&mut self, event: AssemblyEvent) {
        if self.ink_bounds_audit.enabled {
            self.audit_scope_event(event);
        }
        if !O::ENABLED {
            return;
        }
        self.observer.observe(|log| match event {
            AssemblyEvent::ScopeBegin(scope, action) => log.begin(
                Operation::Scope(scope),
                match action {
                    ScopeAction::Assembled => Action::Assemble,
                    ScopeAction::Replanned => Action::Replan,
                    ScopeAction::Recorded => Action::Record,
                    ScopeAction::Inactive => Action::Skip,
                },
            ),
            AssemblyEvent::ScopeEnd(_) => log.end(false),
            AssemblyEvent::ScopeCopied(scope) => log.leaf(Operation::Scope(scope), Action::Copy, false),
            AssemblyEvent::ProducerCopied(owner, kind) => {
                log.leaf(Operation::Producer(owner, kind), Action::Copy, false);
            }
            AssemblyEvent::ScopeLeftOut(scope) => log.leaf(Operation::Scope(scope), Action::LeaveOut, false),
            AssemblyEvent::ProducerLeftOut(owner, kind) => {
                log.leaf(Operation::Producer(owner, kind), Action::LeaveOut, false);
            }
        });
    }
}
