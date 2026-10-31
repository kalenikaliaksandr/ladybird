/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The producers of a frame: each records one kind of output for one row under the contexts it
//! needs, starting and ending at a closed group boundary with no ambient state, so its bytes and
//! hit-test items can be copied verbatim by a later frame. The recorder implements the assembly
//! host with them.

use super::assemble::{AssemblyEvent, AssemblyHost, OutputPosition, ProducerOutcome, ScopeAction, ScopePlan};
use super::cull::ProducerPlacement;
use super::order_tree::{CullReach, ProducerKind};
use super::trace::{Action, Observer, Operation};
use super::{PaintPhase, PaintRecorder};
use crate::css::css_pixels::CssPixelRect;
use crate::layout::node_data::{NodeFlag, NodeSlotId};
use crate::painting::display_list::commands::{ContextRef, SpatialNodeIndex};
use crate::painting::force_dark::ForceDarkRole;
use crate::painting::paint_order_plan::{PaintScope, PaintScopePlan};
use crate::painting::paint_read::{GeometryRead, PaintRead};
use crate::painting::paintable_data::PaintableFlag;
use crate::painting::record::damage::PaintDamage;
use crate::painting::record::svg_resources::MaskLayerSet;
use crate::painting::scroll_snap;
use libgfx_rust::IntRect;
use smallvec::SmallVec;
use std::ops::Range;

pub(super) fn draw_phase(kind: ProducerKind) -> Option<PaintPhase> {
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

fn hit_phase(kind: ProducerKind) -> Option<PaintPhase> {
    match kind {
        ProducerKind::HitBackground => Some(PaintPhase::Background),
        ProducerKind::HitForeground => Some(PaintPhase::Foreground),
        ProducerKind::HitOverlay => Some(PaintPhase::Overlay),
        _ => None,
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    pub(crate) fn record_canvas(&mut self) {
        self.trace_paint(Operation::Named(None, "canvas"), |this| {
            let inputs = this.inputs;
            if let Some(rect) = inputs.canvas_fill_rect {
                this.recorder
                    .fill_rect(rect, inputs.uncaptured.canvas_color, ForceDarkRole::Background);
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
            }
            this.recorder.fill_rect(
                inputs.bitmap_rect,
                inputs.uncaptured.background_color,
                ForceDarkRole::Background,
            );
        });
    }

    // https://drafts.csswg.org/css-transforms-1/#transform-function-lists
    // If a transform function causes the current transformation matrix of an object to be
    // non-invertible, the object and its content do not get displayed. Content whose transform
    // is animated is retained so the compositor can reveal it without a main-thread repaint.
    fn stacking_context_paints(&self, owner: NodeSlotId) -> bool {
        !(self.data(owner).has_flag(PaintableFlag::HasNonInvertibleCssTransform)
            && self.source.node_flags_if_live(owner) & NodeFlag::HasAnimatedOpacityOrTransform as u32 == 0)
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

    /// The box whose lines hold the inline box piece or text fragment that a producer of `owner` paints.
    pub(super) fn line_root_of_inline_producer(&self, owner: NodeSlotId) -> NodeSlotId {
        if crate::painting::node_painting::has_lines(self.source, owner) {
            owner
        } else {
            self.data(owner).containing_block
        }
    }

    pub(super) fn producer_is_skipped(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        draw_phase(kind).is_some_and(|phase| self.base_paint_facts(owner).paint_phase_mask & phase.bit() == 0)
    }

    // What a producer records, beside the bookkeeping around it.
    fn record_producer_output(&mut self, owner: NodeSlotId, kind: ProducerKind) {
        match kind {
            ProducerKind::ScopePreamble => self.record_scope_preamble(owner),
            ProducerKind::ScrollMetadata => self.record_scroll_metadata(owner),
            ProducerKind::Svg => self.record_svg_box_foreground(owner),
            ProducerKind::InlinePiece(index) | ProducerKind::TextFragment(index) => {
                let root = self.line_root_of_inline_producer(owner);
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
        let active = !plan.establishes_stacking_context || self.stacking_context_paints(scope.owner);
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
        let placement = self.placement_of_producer(owner, kind);
        self.live_producer = false;
        self.producer_reads_window_focus.set(false);
        let start = self.output_position();
        self.observer
            .observe(|log| log.begin(Operation::Producer(owner, kind), Action::Record));
        self.record_producer_output(owner, kind);
        let end = self.output_position();
        self.observer
            .observe(|log| log.end(end.bytes == start.bytes && end.hits == start.hits));
        debug_assert!(
            self.recorder.is_producer_boundary(),
            "a producer ends at a closed group boundary without ambient state"
        );
        let outcome = ProducerOutcome {
            live: self.live_producer,
            reads_window_focus: self.producer_reads_window_focus.get(),
            cull_reach: placement.map_or(CullReach::UNPLACED, ProducerPlacement::cull_reach),
        };
        if let Some(placement) = placement
            && super::cull::verifies_paint_producer_bounds()
        {
            self.verify_recorded_producer_placement(owner, kind, placement, start.bytes as usize..end.bytes as usize);
        }
        outcome
    }

    fn producer_cull_reach(&mut self, owner: NodeSlotId, kind: ProducerKind) -> CullReach {
        self.cull_reach_of_producer(owner, kind)
    }

    fn interest_misses(&self, reach: CullReach) -> bool {
        self.interest_rects.miss(reach)
    }

    fn published_and_current_interest_rects_of(&self, scroll_node: SpatialNodeIndex) -> Option<(IntRect, IntRect)> {
        self.interest_rects.published_and_current_rects_of(scroll_node)
    }

    // The commands go to records of their own, which are dropped. So do the resources they register, which the manifest
    // would otherwise hand on although the list never draws them (the host renders every SVG image it asks for), and
    // the pattern tiles they record, whose cached records name those resources.
    fn record_producer_leaving_out_commands(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        debug_assert!(
            self.recorder.is_producer_boundary(),
            "a producer starts at a closed group boundary without ambient state"
        );
        self.observer
            .observe(|log| log.begin(Operation::Producer(owner, kind), Action::Cull));
        let routes_wheels = self.output_may_hold_hit_test_items_or_wheel_routing(owner, kind) && {
            let resources = std::mem::take(&mut self.resources);
            let pattern_tile_records = std::mem::take(&mut self.scratch.pattern_tile_records);
            let detached = self.recorder.begin_detached_records();
            self.record_producer_output(owner, kind);
            let records = self.recorder.finish_detached_records(detached);
            self.scratch.pattern_tile_records = pattern_tile_records;
            self.resources = resources;
            super::cull::routes_wheels(&records)
        };
        self.observer.observe(|log| log.end(false));
        routes_wheels
    }

    fn published_output_routes_wheels(&mut self, owner: NodeSlotId, kind: ProducerKind, bytes: Range<u32>) -> bool {
        self.published_commands_route_wheels(owner, kind, bytes)
    }

    fn output_position(&self) -> OutputPosition {
        OutputPosition {
            bytes: u32::try_from(self.recorder.byte_size()).expect("display list exceeds u32"),
            hits: u32::try_from(self.list.len()).expect("hit-test list exceeds u32"),
            blocking_wheel_event_regions: self.blocking_wheel_event_region_count,
        }
    }

    fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32) {
        let frame = self
            .source_recording
            .as_ref()
            .expect("clean output is copied from a published recording");
        self.recorder.append_cached_commands(&frame.display_list, bytes);
        self.list.append_published(hits.start as usize..hits.end as usize);
        self.blocking_wheel_event_region_count += blocking_wheel_event_regions;
    }

    fn damaged_rows(&mut self) -> Vec<NodeSlotId> {
        let mut rows: Vec<NodeSlotId> = self.source.damaged_paint_rows().collect();
        // A moved row moves its whole layout subtree, whose rows were not pushed themselves. A
        // moved row inside that subtree is listed already and expands its own subtree, so the
        // walk stops there, and a row expanded from another moved ancestor stops it as well.
        let mut expanded = Vec::new();
        for row in &rows {
            if !self.source.paint_damage_of_row(*row).contains(PaintDamage::MOVED) {
                continue;
            }
            let root = *row;
            self.source
                .for_each_node_in_layout_subtree_in_pre_order_with_pruning(root, |node| {
                    if node == root {
                        return true;
                    }
                    if self.source.paint_damage_of_row(node).contains(PaintDamage::MOVED) {
                        return false;
                    }
                    if !self.source.paintable_row_is_populated(node) {
                        return true;
                    }
                    let newly_expanded = self.moved_expansion.insert(node);
                    if newly_expanded {
                        expanded.push(node);
                    }
                    newly_expanded
                });
        }
        rows.extend(expanded);
        rows
    }

    fn effective_damage(&mut self, row: NodeSlotId) -> PaintDamage {
        let mut damage = self.source.paint_damage_of_row(row);
        if self.moved_expansion.contains(&row) {
            damage |= PaintDamage::ALL_PRODUCERS | PaintDamage::MOVED;
        }
        damage
    }

    // SVG content is recorded by its root, and a snap container's scroll metadata lists the
    // snap areas below it.
    fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        match kind {
            ProducerKind::Svg => true,
            ProducerKind::ScopePreamble => self.paints_svg_mask_or_clip_resource_subtree(owner),
            ProducerKind::ScrollMetadata => scroll_snap::snap_container_geometry(self.source, owner).is_some(),
            ProducerKind::InlinePiece(_) | ProducerKind::TextFragment(_) => true,
            _ => false,
        }
    }

    fn observe(&mut self, event: AssemblyEvent) {
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
            AssemblyEvent::ProducerCulled(owner, kind) => {
                log.leaf(Operation::Producer(owner, kind), Action::Cull, false);
            }
        });
    }
}
