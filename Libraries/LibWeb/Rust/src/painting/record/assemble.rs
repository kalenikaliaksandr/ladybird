/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Assembles a frame from the scopes the published recording walked and the pushed damage. A walked scope whose
//! owner has no damage below it and has not moved is copied whole. Otherwise its plan is walked: each run of items is
//! copied from the published recording, or recorded again from its first item to its last when damage reaches it or
//! an ancestor moved it. Nothing is validated on the way: when assembly starts, the damage set is complete by
//! construction.
//!
//! A recording of a window of the document leaves out the runs and the walked child scopes that lie outside the
//! window, and remembers that it did, so that the recording after it records them once they lie inside.

use super::PaintPhase;
use super::order_tree::{
    InkBound, OutputSize, ProducerKind, RecordedRun, RecordedScope, RecordedSegment, RowPlacement, Segment,
    SegmentedPlan,
};
use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::paint_order_plan::{
    PaintOrderItem, PaintProducer, PaintScope, PaintScopeKind, StackingContextPaintPhase,
};
use crate::painting::record::damage::PaintDamage;
use smallvec::SmallVec;
use std::ops::Range;
use std::sync::Arc;

/// The planner's items for one scope in paint order. Every producer belongs to the scope's
/// owner. An inactive plan is a stacking context that does not paint; its scope records nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScopePlan {
    pub active: bool,
    pub items: SmallVec<[PaintOrderItem; 16]>,
}

/// A box phase in paint order is up to three producers: its hit-test items, the scroll
/// metadata recorded with the background, and its commands. An SVG root paints its
/// background, border and then all of its content as one producer.
pub(crate) fn producer_kinds(producer: PaintProducer) -> SmallVec<[ProducerKind; 5]> {
    use ProducerKind::*;
    match producer {
        PaintProducer::BoxPhase(PaintPhase::Background) => {
            smallvec::smallvec![HitBackground, ScrollMetadata, DrawBackground]
        }
        PaintProducer::BoxPhase(PaintPhase::Border) => smallvec::smallvec![DrawBorder],
        PaintProducer::BoxPhase(PaintPhase::TableCollapsedBorder) => smallvec::smallvec![DrawTableCollapsedBorder],
        PaintProducer::BoxPhase(PaintPhase::Foreground) => smallvec::smallvec![HitForeground, DrawForeground],
        PaintProducer::BoxPhase(PaintPhase::Outline) => smallvec::smallvec![DrawOutline],
        PaintProducer::BoxPhase(PaintPhase::Overlay) => smallvec::smallvec![HitOverlay, DrawOverlay],
        PaintProducer::SvgRoot => smallvec::smallvec![HitBackground, ScrollMetadata, DrawBackground, DrawBorder, Svg],
        PaintProducer::SvgBoxForeground => smallvec::smallvec![Svg],
        PaintProducer::ScopePreamble => smallvec::smallvec![ScopePreamble],
        PaintProducer::ForegroundHitTest => smallvec::smallvec![HitForeground],
        PaintProducer::ForegroundCommands => smallvec::smallvec![DrawForeground],
        PaintProducer::InlinePiece(index) => smallvec::smallvec![InlinePiece(index)],
        PaintProducer::TextFragment(index) => smallvec::smallvec![TextFragment(index)],
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProducerOutcome {
    // Recorded again every frame; the entry and every scope above it stay live.
    pub live: bool,
    // Recorded again when the window gains or loses focus.
    pub reads_window_focus: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeAction {
    // Walked with the published child list.
    Assembled,
    // The plan was rebuilt and matched against the published children.
    Replanned,
    // Recorded without a published counterpart.
    Recorded,
    // The stacking context does not paint.
    Inactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssemblyEvent {
    ScopeBegin(PaintScope, ScopeAction),
    ScopeEnd(PaintScope),
    ScopeCopied(PaintScope),
    ProducerCopied(NodeSlotId, ProducerKind),
    ScopeLeftOut(PaintScope),
    ProducerLeftOut(NodeSlotId, ProducerKind),
}

/// How much of the document a recording holds, against what the published recording holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// Everything, wherever it is.
    WholeDocument,
    /// What shows in a window, the one the published recording was made for: what lay outside it then still does,
    /// unless damage says otherwise.
    PublishedWindow,
    /// What shows in a window the published recording was not made for.
    AnotherWindow,
}

/// The end of the output being assembled: command bytes, hit-test items and blocking wheel
/// regions so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputPosition {
    pub bytes: u32,
    pub hits: u32,
    pub blocking_wheel_event_regions: u32,
}

/// Where the plan of a walked scope is cut at one of its items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemCut {
    /// A child scope that is walked on its own.
    Walked,
    /// An item recorded in a run. Runs are cut along a grid, so an item that knows which row of it it starts in
    /// says so; one that does not stays with the items before it.
    InRun { grid_row: Option<i32> },
}

/// A run never holds more items than this, however little room they take.
const MAX_ITEMS_IN_A_RUN: usize = 512;

/// What assembly needs from the recorder, which owns the output and the published recording.
pub(crate) trait AssemblyHost {
    fn plan_scope(&mut self, scope: PaintScope) -> ScopePlan;

    /// Where the plan of a walked scope is cut at `item`.
    fn cut_at(&mut self, scope: PaintScope, item: PaintOrderItem) -> ItemCut;

    /// Records one producer of `owner` under the contexts it needs.
    fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> ProducerOutcome;

    fn output_position(&self) -> OutputPosition;

    /// Appends the published recording's command bytes and hit-test items in the given ranges.
    fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32);

    /// Ends the chunk of the tape that holds a run recorded whole, so that copying the run copies whole chunks.
    fn end_recorded_run(&mut self);

    /// The damage pushed for a row since the published recording, with the hint that something below it was
    /// damaged.
    fn damage_of_row(&mut self, row: NodeSlotId) -> PaintDamage;

    /// Where a row's boxes are. A row an ancestor moved records again though nothing was pushed for it.
    fn placement_of_row(&mut self, row: NodeSlotId) -> RowPlacement;

    fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool;

    /// What bounds everything `item` of the plan of `scope` paints, or none where no rect in a space of the tree
    /// does. Only a recording of a window asks.
    fn bound_of_item(&mut self, _scope: PaintScope, _item: PaintOrderItem) -> Option<InkBound> {
        None
    }

    /// One bound for what both bound, or none where their spaces have no common one that places both.
    fn united_bound(&mut self, _first: InkBound, _second: InkBound) -> Option<InkBound> {
        None
    }

    /// Whether nothing inside the bound shows in the window of the recording.
    fn is_outside_window(&mut self, _bound: InkBound) -> bool {
        false
    }

    /// Whether everything inside the bound shows in the window of the recording.
    fn is_wholly_inside_window(&mut self, _bound: InkBound) -> bool {
        false
    }

    /// Whether a box cuts off some of what it holds, as a scroller does. What bounds its scopes then bounds what
    /// shows of them, and what they paint may lie outside it.
    fn cuts_off_what_it_holds(&mut self, _owner: NodeSlotId) -> bool {
        false
    }

    fn observe(&mut self, _event: AssemblyEvent) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceCursor {
    bytes: u32,
    hits: u32,
}

impl SourceCursor {
    fn advance(&mut self, output: OutputSize) {
        self.bytes += output.bytes;
        self.hits += output.hits;
    }
}

// Consecutive clean parts are copied with one range append each for commands and items.
#[derive(Clone, Copy)]
struct PendingCopy {
    start: SourceCursor,
    end: SourceCursor,
    blocking_wheel_event_regions: u32,
}

/// A walked scope of the published recording, with where its output starts there.
#[derive(Clone, Copy)]
struct Published<'a> {
    scope: &'a Arc<RecordedScope>,
    cursor: SourceCursor,
}

#[derive(PartialEq, Eq, Hash)]
enum SegmentKey {
    RunStartingAt(PaintOrderItem),
    Walked(PaintScope),
}

/// Whether the published recording can be returned as it is: nothing was pushed, and nothing in it is recorded every
/// frame or read a window focus that changed.
pub(crate) fn frame_is_unchanged(
    published_root: &RecordedScope,
    has_damage: bool,
    window_focus_changed: bool,
    coverage: Coverage,
) -> bool {
    let output = published_root.output;
    let covers_the_same = match coverage {
        Coverage::PublishedWindow => true,
        Coverage::WholeDocument => !published_root.leaves_out_what_lies_outside_the_window,
        Coverage::AnotherWindow => false,
    };
    !has_damage && !output.live && !(window_focus_changed && output.reads_window_focus) && covers_the_same
}

// The damage of a scope's owner that cannot change what the scope records. The scope of a box's backgrounds and
// borders holds none of the foreground, outline and overlay producers of the box, and none of its SVG content.
fn owner_damage_that_misses(kind: PaintScopeKind) -> PaintDamage {
    match kind {
        PaintScopeKind::Descendants(StackingContextPaintPhase::BackgroundAndBorders) => {
            PaintDamage::DRAW_FOREGROUND
                | PaintDamage::HIT_FOREGROUND
                | PaintDamage::DRAW_OUTLINE
                | PaintDamage::DRAW_OVERLAY
                | PaintDamage::HIT_OVERLAY
                | PaintDamage::SVG
        }
        _ => PaintDamage::NONE,
    }
}

pub(crate) struct Assembler<'a, H: AssemblyHost> {
    host: &'a mut H,
    // The published recording was made in the other window focus state.
    window_focus_changed: bool,
    coverage: Coverage,
    pending_copy: Option<PendingCopy>,
}

impl<'a, H: AssemblyHost> Assembler<'a, H> {
    pub(crate) fn new(host: &'a mut H, window_focus_changed: bool, coverage: Coverage) -> Self {
        Self {
            host,
            window_focus_changed,
            coverage,
            pending_copy: None,
        }
    }

    // Copying a part would keep output that is recorded every frame, or that read the window focus before it
    // changed.
    fn records_again(&self, output: OutputSize) -> bool {
        output.live || (self.window_focus_changed && output.reads_window_focus)
    }

    /// Records the frame's root scope after the prologue. `published_root` is the root the published recording
    /// walked, with the byte offset its output starts at there, when that recording can be copied from.
    pub(crate) fn assemble_root(
        mut self,
        root_scope: PaintScope,
        published_root: Option<(&Arc<RecordedScope>, u32)>,
    ) -> Arc<RecordedScope> {
        let published = published_root
            .filter(|(root, _)| root.scope == root_scope)
            .map(|(scope, prologue_bytes)| Published {
                scope,
                cursor: SourceCursor {
                    bytes: prologue_bytes,
                    hits: 0,
                },
            });
        let root_bound = match self.coverage {
            Coverage::WholeDocument => None,
            Coverage::PublishedWindow | Coverage::AnotherWindow => {
                self.host.bound_of_item(root_scope, PaintOrderItem::Scope(root_scope))
            }
        };
        let root_cuts_off_what_it_holds = self.host.cuts_off_what_it_holds(root_scope.owner);
        let is_wholly_inside_window = self.is_wholly_inside_window(root_bound, false, root_cuts_off_what_it_holds);
        let root = self.walk_scope(root_scope, published, is_wholly_inside_window);
        self.flush_pending_copy();
        root
    }

    // Nothing of what a scope paints directly is left out: the recording holds everything, or the scope is in a
    // scope that is wholly inside the window or is bounded by something that is. The scope of a box that cuts off
    // what it holds paints outside what bounds it, so being inside the window says nothing about what it holds.
    fn is_wholly_inside_window(
        &mut self,
        bound: Option<InkBound>,
        is_in_a_scope_wholly_inside_window: bool,
        owner_cuts_off_what_it_holds: bool,
    ) -> bool {
        if self.coverage == Coverage::WholeDocument {
            return true;
        }
        if owner_cuts_off_what_it_holds {
            return false;
        }
        is_in_a_scope_wholly_inside_window || bound.is_some_and(|bound| self.host.is_wholly_inside_window(bound))
    }

    // Whether a clean walked scope of the published recording holds what this recording holds of it: neither left
    // out what this one records, nor recorded what this one leaves out. In another window that is so where the scope
    // is wholly inside both, down to the scopes below it that paint outside what bounds it.
    fn published_scope_covers_the_same(&self, published: &RecordedScope, is_wholly_inside_window: bool) -> bool {
        match self.coverage {
            Coverage::PublishedWindow => true,
            Coverage::WholeDocument => !published.leaves_out_what_lies_outside_the_window,
            Coverage::AnotherWindow => {
                !published.leaves_out_what_lies_outside_the_window
                    && !published.walks_a_scope_of_a_box_that_cuts_off_what_it_holds
                    && is_wholly_inside_window
            }
        }
    }

    fn bound_of_segment(&mut self, scope: PaintScope, plan: &SegmentedPlan, segment: &Segment) -> Option<InkBound> {
        match segment {
            Segment::Walked(item) => self.host.bound_of_item(scope, plan.items[*item as usize]),
            Segment::Run(run) => {
                let mut bound_of_run: Option<InkBound> = None;
                for item in plan.items_of(run) {
                    let bound = self.host.bound_of_item(scope, *item)?;
                    bound_of_run = Some(match bound_of_run {
                        Some(bound_so_far) => self.host.united_bound(bound_so_far, bound)?,
                        None => bound,
                    });
                }
                bound_of_run
            }
        }
    }

    // The events of a segment that was left out, for whoever follows the recording.
    fn observe_left_out(&mut self, scope: PaintScope, plan: &SegmentedPlan, segment: &Segment) {
        match segment {
            Segment::Walked(item) => self.host.observe(AssemblyEvent::ScopeLeftOut(plan.walked_scope(*item))),
            Segment::Run(run) => {
                for item in plan.items_of(run) {
                    match *item {
                        PaintOrderItem::Producer(producer) => {
                            for kind in producer_kinds(producer) {
                                self.host.observe(AssemblyEvent::ProducerLeftOut(scope.owner, kind));
                            }
                        }
                        PaintOrderItem::Scope(child) => self.host.observe(AssemblyEvent::ScopeLeftOut(child)),
                    }
                }
            }
        }
    }

    fn scope_is_clean(&mut self, scope: PaintScope, recorded_placement: RowPlacement) -> bool {
        self.host
            .damage_of_row(scope.owner)
            .without(owner_damage_that_misses(scope.kind))
            .is_empty()
            && self.host.placement_of_row(scope.owner) == recorded_placement
    }

    fn producer_is_damaged(&mut self, owner: NodeSlotId, kind: ProducerKind, damage: PaintDamage) -> bool {
        damage.intersects(kind.damage() | PaintDamage::MOVED)
            || (damage.contains(PaintDamage::DESCENDANT_READERS) && self.host.producer_reads_descendants(owner, kind))
    }

    fn plan_and_cut(&mut self, scope: PaintScope) -> SegmentedPlan {
        let plan = self.host.plan_scope(scope);
        self.cut(scope, plan.active, plan.items.into_vec().into())
    }

    fn cut(&mut self, scope: PaintScope, active: bool, items: Arc<[PaintOrderItem]>) -> SegmentedPlan {
        let mut segments = Vec::new();
        let mut run_start = 0;
        let mut grid_row_of_run = None;
        for (index, item) in items.iter().enumerate() {
            let cut = self.host.cut_at(scope, *item);
            let starts_another_run = match cut {
                ItemCut::Walked => true,
                ItemCut::InRun { grid_row } => {
                    let leaves_grid_row =
                        grid_row.is_some() && grid_row_of_run.is_some() && grid_row != grid_row_of_run;
                    leaves_grid_row || index - run_start == MAX_ITEMS_IN_A_RUN
                }
            };
            if starts_another_run {
                if run_start < index {
                    segments.push(Segment::Run(run_start as u32..index as u32));
                }
                run_start = index;
                grid_row_of_run = None;
            }
            match cut {
                ItemCut::Walked => {
                    segments.push(Segment::Walked(index as u32));
                    run_start = index + 1;
                }
                ItemCut::InRun { grid_row } => grid_row_of_run = grid_row_of_run.or(grid_row),
            }
        }
        if run_start < items.len() {
            segments.push(Segment::Run(run_start as u32..items.len() as u32));
        }
        SegmentedPlan {
            active,
            items,
            segments,
        }
    }

    /// Walks the plan of `scope`, which damage reaches or the published recording did not walk, or which holds
    /// something the window of the published recording left out. A scope that is wholly inside the window of this
    /// recording leaves nothing out, so nothing asks what bounds its segments.
    fn walk_scope(
        &mut self,
        scope: PaintScope,
        published: Option<Published<'_>>,
        is_wholly_inside_window: bool,
    ) -> Arc<RecordedScope> {
        let owner_damage = self.host.damage_of_row(scope.owner);
        let owner_placement = self.host.placement_of_row(scope.owner);
        let order_damage = match scope.kind {
            PaintScopeKind::PaintedAsStackingContext => {
                PaintDamage::ORDER | PaintDamage::CONTEXT_ORDER | PaintDamage::ELIGIBILITY
            }
            PaintScopeKind::Descendants(_) => PaintDamage::ORDER,
        };
        let owner_moved = published.is_some_and(|published| published.scope.owner_placement != owner_placement);
        let published_plan = published
            .filter(|_| !owner_damage.intersects(order_damage))
            .map(|published| &published.scope.plan);
        let items_were_kept = published_plan.is_some();
        // Nothing was pushed for the owner or anything below it, and nothing moved it: every item is where it was.
        let cuts_were_kept = items_were_kept && owner_damage.is_empty() && !owner_moved;
        let plan = match published_plan {
            Some(published_plan) if cuts_were_kept => Arc::clone(published_plan),
            Some(published_plan) => Arc::new(self.cut(scope, published_plan.active, Arc::clone(&published_plan.items))),
            None => Arc::new(self.plan_and_cut(scope)),
        };
        let action = match (published.is_some(), items_were_kept, plan.active) {
            (_, _, false) => ScopeAction::Inactive,
            (true, true, true) => ScopeAction::Assembled,
            (true, false, true) => ScopeAction::Replanned,
            (false, _, true) => ScopeAction::Recorded,
        };
        self.host.observe(AssemblyEvent::ScopeBegin(scope, action));
        let mut recorded = RecordedScope {
            scope,
            plan: Arc::clone(&plan),
            segments: Vec::with_capacity(plan.segments.len()),
            output: OutputSize::default(),
            owner_placement,
            segment_bounds: None,
            leaves_out_what_lies_outside_the_window: false,
            walks_a_scope_of_a_box_that_cuts_off_what_it_holds: false,
        };
        if plan.active {
            if !is_wholly_inside_window {
                // A scope whose cuts were kept holds every item where it was, so what bounded its segments does.
                let kept_bounds = published
                    .filter(|_| cuts_were_kept)
                    .and_then(|published| published.scope.segment_bounds.clone());
                recorded.segment_bounds = Some(kept_bounds.unwrap_or_else(|| {
                    plan.segments
                        .iter()
                        .map(|segment| self.bound_of_segment(scope, &plan, segment))
                        .collect()
                }));
            }
            let segment_bounds = recorded.segment_bounds.clone();
            let published_segments = published.map(|published| PublishedSegments::new(published, cuts_were_kept));
            for (index, segment) in plan.segments.iter().enumerate() {
                let bound = segment_bounds.as_ref().and_then(|bounds| bounds[index]);
                if bound.is_some_and(|bound| self.host.is_outside_window(bound)) {
                    self.observe_left_out(scope, &plan, segment);
                    recorded.leaves_out_what_lies_outside_the_window = true;
                    recorded.segments.push(RecordedSegment::Outside);
                    continue;
                }
                let published_segment = published_segments
                    .as_ref()
                    .and_then(|segments| segments.matching(index, segment, &plan));
                let assembled = match segment {
                    Segment::Run(run) => RecordedSegment::Run(self.assemble_run(
                        scope,
                        plan.items_of(run),
                        published_segment,
                        owner_damage,
                        owner_moved,
                    )),
                    Segment::Walked(item) => {
                        let child = plan.walked_scope(*item);
                        let child_cuts_off_what_it_holds = self.host.cuts_off_what_it_holds(child.owner);
                        let child_is_wholly_inside_window =
                            self.is_wholly_inside_window(bound, is_wholly_inside_window, child_cuts_off_what_it_holds);
                        let assembled_child =
                            self.assemble_walked_child(child, published_segment, child_is_wholly_inside_window);
                        recorded.walks_a_scope_of_a_box_that_cuts_off_what_it_holds |= child_cuts_off_what_it_holds
                            || assembled_child.walks_a_scope_of_a_box_that_cuts_off_what_it_holds;
                        RecordedSegment::Walked(assembled_child)
                    }
                };
                recorded.output.add(assembled.output());
                recorded.leaves_out_what_lies_outside_the_window |= assembled.leaves_out_what_lies_outside_the_window();
                recorded.segments.push(assembled);
            }
        }
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        Arc::new(recorded)
    }

    fn assemble_walked_child(
        &mut self,
        child: PaintScope,
        published: Option<(&RecordedSegment, SourceCursor)>,
        is_wholly_inside_window: bool,
    ) -> Arc<RecordedScope> {
        let published = published.and_then(|(segment, cursor)| match segment {
            RecordedSegment::Walked(scope) => Some(Published { scope, cursor }),
            RecordedSegment::Run(_) | RecordedSegment::Outside => None,
        });
        if let Some(published) = published
            && !self.records_again(published.scope.output)
            && self.published_scope_covers_the_same(published.scope, is_wholly_inside_window)
            && self.scope_is_clean(child, published.scope.owner_placement)
        {
            self.copy(published.cursor, published.scope.output);
            self.host.observe(AssemblyEvent::ScopeCopied(child));
            return Arc::clone(published.scope);
        }
        self.walk_scope(child, published, is_wholly_inside_window)
    }

    fn assemble_run(
        &mut self,
        scope: PaintScope,
        items: &[PaintOrderItem],
        published: Option<(&RecordedSegment, SourceCursor)>,
        owner_damage: PaintDamage,
        owner_moved: bool,
    ) -> RecordedRun {
        let published = published.and_then(|(segment, cursor)| match segment {
            RecordedSegment::Run(run) => Some((run, cursor)),
            RecordedSegment::Walked(_) | RecordedSegment::Outside => None,
        });
        if let Some((run, cursor)) = published
            && !owner_moved
            && !self.records_again(run.output)
            && self.run_is_clean(scope, items, run, owner_damage)
        {
            self.copy(cursor, run.output);
            for item in items {
                match *item {
                    PaintOrderItem::Producer(producer) => {
                        for kind in producer_kinds(producer) {
                            self.host.observe(AssemblyEvent::ProducerCopied(scope.owner, kind));
                        }
                    }
                    PaintOrderItem::Scope(child) => self.host.observe(AssemblyEvent::ScopeCopied(child)),
                }
            }
            return RecordedRun {
                output: run.output,
                child_scope_placements: run.child_scope_placements.clone(),
            };
        }
        self.flush_pending_copy();
        let mut output = OutputSize::default();
        let mut child_scope_placements = Vec::new();
        for item in items {
            match *item {
                PaintOrderItem::Producer(producer) => {
                    for kind in producer_kinds(producer) {
                        output.add(self.record_producer(scope.owner, kind));
                    }
                }
                PaintOrderItem::Scope(child) => {
                    child_scope_placements.push(self.host.placement_of_row(child.owner));
                    output.add(self.record_whole_scope(child));
                }
            }
        }
        self.host.end_recorded_run();
        RecordedRun {
            output,
            child_scope_placements: child_scope_placements.into_boxed_slice(),
        }
    }

    fn run_is_clean(
        &mut self,
        scope: PaintScope,
        items: &[PaintOrderItem],
        published: &RecordedRun,
        owner_damage: PaintDamage,
    ) -> bool {
        let mut child_scope_placements = published.child_scope_placements.iter();
        items.iter().all(|item| match *item {
            PaintOrderItem::Producer(producer) => !producer_kinds(producer)
                .into_iter()
                .any(|kind| self.producer_is_damaged(scope.owner, kind, owner_damage)),
            PaintOrderItem::Scope(child) => {
                let recorded_placement = child_scope_placements
                    .next()
                    .expect("a run keeps the origin of each of its child scopes");
                self.scope_is_clean(child, *recorded_placement)
            }
        })
    }

    /// Records a scope with everything below it, as part of a run.
    fn record_whole_scope(&mut self, scope: PaintScope) -> OutputSize {
        let plan = self.host.plan_scope(scope);
        let mut total = OutputSize::default();
        if !plan.active {
            self.host
                .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Inactive));
            self.host.observe(AssemblyEvent::ScopeEnd(scope));
            return total;
        }
        self.host
            .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Recorded));
        for item in plan.items {
            match item {
                PaintOrderItem::Producer(producer) => {
                    for kind in producer_kinds(producer) {
                        total.add(self.record_producer(scope.owner, kind));
                    }
                }
                PaintOrderItem::Scope(child_scope) => total.add(self.record_whole_scope(child_scope)),
            }
        }
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        total
    }

    fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> OutputSize {
        debug_assert!(self.pending_copy.is_none());
        let before = self.host.output_position();
        let outcome = self.host.record_producer(owner, kind);
        let after = self.host.output_position();
        OutputSize {
            bytes: after.bytes - before.bytes,
            hits: after.hits - before.hits,
            blocking_wheel_event_regions: after.blocking_wheel_event_regions - before.blocking_wheel_event_regions,
            live: outcome.live,
            reads_window_focus: outcome.reads_window_focus,
        }
    }

    fn copy(&mut self, cursor: SourceCursor, output: OutputSize) {
        let mut end = cursor;
        end.advance(output);
        match &mut self.pending_copy {
            Some(pending) if pending.end == cursor => {
                pending.end = end;
                pending.blocking_wheel_event_regions += output.blocking_wheel_event_regions;
            }
            _ => {
                self.flush_pending_copy();
                self.pending_copy = Some(PendingCopy {
                    start: cursor,
                    end,
                    blocking_wheel_event_regions: output.blocking_wheel_event_regions,
                });
            }
        }
    }

    fn flush_pending_copy(&mut self) {
        let Some(pending) = self.pending_copy.take() else {
            return;
        };
        self.host.copy_published(
            pending.start.bytes..pending.end.bytes,
            pending.start.hits..pending.end.hits,
            pending.blocking_wheel_event_regions,
        );
    }
}

/// The segments a walked scope had in the published recording, each with where its output starts there. A plan
/// whose cuts were kept has the same segments as then. One that was cut again is matched by what its segments begin
/// with: a run is the published one when it lists the same items from the same first item.
struct PublishedSegments<'a> {
    published: Published<'a>,
    cursors: Vec<SourceCursor>,
    index_by_key: Option<FastMap<SegmentKey, usize>>,
}

impl<'a> PublishedSegments<'a> {
    fn new(published: Published<'a>, cuts_were_kept: bool) -> Self {
        let scope = published.scope;
        let mut cursor = published.cursor;
        let cursors = scope
            .segments
            .iter()
            .map(|segment| {
                let start = cursor;
                cursor.advance(segment.output());
                start
            })
            .collect();
        let index_by_key = (!cuts_were_kept).then(|| {
            scope
                .plan
                .segments
                .iter()
                .enumerate()
                .map(|(index, segment)| (Self::key(&scope.plan, segment), index))
                .collect()
        });
        Self {
            published,
            cursors,
            index_by_key,
        }
    }

    fn key(plan: &SegmentedPlan, segment: &Segment) -> SegmentKey {
        match segment {
            Segment::Run(run) => SegmentKey::RunStartingAt(plan.items[run.start as usize]),
            Segment::Walked(item) => SegmentKey::Walked(plan.walked_scope(*item)),
        }
    }

    fn matching(
        &self,
        index: usize,
        segment: &Segment,
        plan: &SegmentedPlan,
    ) -> Option<(&'a RecordedSegment, SourceCursor)> {
        let scope = self.published.scope;
        let published_index = match &self.index_by_key {
            None => index,
            Some(index_by_key) => {
                let published_index = *index_by_key.get(&Self::key(plan, segment))?;
                if let (Segment::Run(run), Segment::Run(published_run)) =
                    (segment, &scope.plan.segments[published_index])
                    && plan.items_of(run) != scope.plan.items_of(published_run)
                {
                    return None;
                }
                published_index
            }
        };
        Some((scope.segments.get(published_index)?, self.cursors[published_index]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::css_pixels::{CssPixelPoint, CssPixelRect, CssPixels};
    use crate::painting::border_radii::BorderRadii;
    use crate::painting::display_list::builder::RecordedDisplayList;
    use crate::painting::display_list::commands::ContextRef;
    use crate::painting::display_list::recorder::DisplayListRecorder;
    use crate::painting::force_dark::ForceDarkRole;
    use crate::painting::hit_test::{HitTestItem, HitTestItemKind, HitTestList};
    use libgfx_rust::{Color, IntRect};
    use std::collections::{HashMap, HashSet};

    fn row(index: u32) -> NodeSlotId {
        NodeSlotId::new(index, 1)
    }

    // Tells the output of one producer kind from another's.
    fn code_of(kind: ProducerKind) -> u8 {
        match kind {
            ProducerKind::DrawBackground => 0,
            ProducerKind::DrawForeground => 1,
            ProducerKind::HitForeground => 2,
            _ => 3,
        }
    }

    fn context(owner: NodeSlotId) -> PaintScope {
        PaintScope::stacking_context(owner)
    }

    fn backgrounds(owner: NodeSlotId) -> PaintScope {
        PaintScope {
            owner,
            kind: PaintScopeKind::Descendants(StackingContextPaintPhase::BackgroundAndBorders),
        }
    }

    fn foreground(owner: NodeSlotId) -> PaintScope {
        PaintScope {
            owner,
            kind: PaintScopeKind::Descendants(StackingContextPaintPhase::Foreground),
        }
    }

    fn phase(phase: PaintPhase) -> PaintOrderItem {
        PaintOrderItem::Producer(PaintProducer::BoxPhase(phase))
    }

    fn child(scope: PaintScope) -> PaintOrderItem {
        PaintOrderItem::Scope(scope)
    }

    #[derive(Clone, Copy)]
    struct Content {
        rects: u32,
        hits: u32,
        version: u8,
        live: bool,
        reads_window_focus: bool,
        blocking: u32,
    }

    fn content(rects: u32, hits: u32) -> Content {
        Content {
            rects,
            hits,
            version: 1,
            live: false,
            reads_window_focus: false,
            blocking: 0,
        }
    }

    struct Frame {
        display_list: RecordedDisplayList,
        hit_test_items: Vec<HitTestItem>,
        prologue_bytes: u32,
        root: Arc<RecordedScope>,
        window: Option<(i32, i32)>,
    }

    struct Output {
        recorder: DisplayListRecorder,
        hit_test_list: HitTestList,
        blocking_wheel_event_region_count: u32,
    }

    fn new_output() -> Output {
        Output {
            recorder: DisplayListRecorder::new(None),
            hit_test_list: HitTestList::default(),
            blocking_wheel_event_region_count: 0,
        }
    }

    struct TestHost {
        plans: HashMap<PaintScope, ScopePlan>,
        contents: HashMap<(NodeSlotId, ProducerKind), Content>,
        damage: HashMap<NodeSlotId, PaintDamage>,
        origins: HashMap<NodeSlotId, i64>,
        walked: HashSet<PaintScope>,
        grid_rows: HashMap<NodeSlotId, i32>,
        descendant_readers: HashSet<(NodeSlotId, ProducerKind)>,
        recorded: Vec<(NodeSlotId, ProducerKind)>,
        events: Vec<AssemblyEvent>,
        output: Output,
        source: Option<Frame>,
        // The rows a recording holds what shows in, from the first of them to the last, or none for a recording
        // that holds everything. A row is as tall as a row of the grid.
        window: Option<(i32, i32)>,
        // The grid rows a child scope's owner paints in, from the first to the last.
        rows_painted_in: HashMap<NodeSlotId, (i32, i32)>,
        cutting_off_what_they_hold: HashSet<NodeSlotId>,
        bounds_asked: usize,
    }

    impl Default for TestHost {
        fn default() -> Self {
            Self {
                plans: HashMap::new(),
                contents: HashMap::new(),
                damage: HashMap::new(),
                origins: HashMap::new(),
                walked: HashSet::new(),
                grid_rows: HashMap::new(),
                descendant_readers: HashSet::new(),
                recorded: Vec::new(),
                events: Vec::new(),
                output: new_output(),
                source: None,
                window: None,
                rows_painted_in: HashMap::new(),
                cutting_off_what_they_hold: HashSet::new(),
                bounds_asked: 0,
            }
        }
    }

    const ROW_HEIGHT: f32 = 512.0;

    fn rect_of_rows(first: i32, last: i32) -> libgfx_rust::FloatRect {
        libgfx_rust::FloatRect::new(
            0.0,
            first as f32 * ROW_HEIGHT,
            100.0,
            (last - first + 1) as f32 * ROW_HEIGHT,
        )
    }

    impl TestHost {
        fn plan(&mut self, scope: PaintScope, active: bool, items: &[PaintOrderItem]) {
            self.plans.insert(
                scope,
                ScopePlan {
                    active,
                    items: items.iter().copied().collect(),
                },
            );
        }

        fn produce(&mut self, owner: NodeSlotId, kind: ProducerKind, content: Content) {
            self.contents.insert((owner, kind), content);
        }

        fn push(&mut self, owner: NodeSlotId, damage: PaintDamage) {
            *self.damage.entry(owner).or_default() |= damage;
        }

        fn recorded_rows(&self) -> Vec<u32> {
            let mut rows: Vec<u32> = self.recorded.iter().map(|(owner, _)| owner.slot_index()).collect();
            rows.dedup();
            rows
        }

        fn copied_scopes(&self) -> Vec<PaintScope> {
            self.events
                .iter()
                .filter_map(|event| match event {
                    AssemblyEvent::ScopeCopied(scope) => Some(*scope),
                    _ => None,
                })
                .collect()
        }

        fn left_out_scopes(&self) -> Vec<PaintScope> {
            self.events
                .iter()
                .filter_map(|event| match event {
                    AssemblyEvent::ScopeLeftOut(scope) => Some(*scope),
                    _ => None,
                })
                .collect()
        }

        fn rows_recorded(&self) -> std::collections::BTreeSet<u32> {
            self.recorded.iter().map(|(owner, _)| owner.slot_index()).collect()
        }

        fn action_of(&self, scope: PaintScope) -> Option<ScopeAction> {
            self.events.iter().find_map(|event| match event {
                AssemblyEvent::ScopeBegin(begun, action) if *begun == scope => Some(*action),
                _ => None,
            })
        }
    }

    impl AssemblyHost for TestHost {
        fn plan_scope(&mut self, scope: PaintScope) -> ScopePlan {
            self.plans.get(&scope).cloned().unwrap_or_default()
        }

        fn cut_at(&mut self, _scope: PaintScope, item: PaintOrderItem) -> ItemCut {
            match item {
                PaintOrderItem::Scope(child) if self.walked.contains(&child) => ItemCut::Walked,
                PaintOrderItem::Scope(child) => ItemCut::InRun {
                    grid_row: self.grid_rows.get(&child.owner).copied(),
                },
                PaintOrderItem::Producer(_) => ItemCut::InRun { grid_row: None },
            }
        }

        fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> ProducerOutcome {
            let Some(content) = self.contents.get(&(owner, kind)).copied() else {
                return ProducerOutcome::default();
            };
            self.recorded.push((owner, kind));
            let origin = self.origins.get(&owner).copied().unwrap_or_default() as i32;
            let output = &mut self.output;
            for index in 0..content.rects {
                output.recorder.fill_rect(
                    IntRect {
                        x: owner.slot_index() as i32 + origin,
                        y: i32::from(code_of(kind)) * 100 + index as i32,
                        width: 10,
                        height: i32::from(content.version),
                    },
                    Color::from_rgb(content.version, code_of(kind), 7),
                    ForceDarkRole::Background,
                );
            }
            for index in 0..content.hits {
                output.hit_test_list.append(HitTestItem {
                    kind: HitTestItemKind::Box,
                    paintable: owner,
                    hit_node: owner,
                    chrome_widget_kind: 0,
                    text_fragment_index: None,
                    caret_node: NodeSlotId::INVALID,
                    caret_offset: 0,
                    rect: CssPixelRect::default(),
                    caret_rect: CssPixelRect::default(),
                    caret_line_index: Some(index as usize + content.version as usize * 100),
                    block_container: NodeSlotId::INVALID,
                    context: ContextRef::default(),
                    border_radii: BorderRadii::default(),
                    path: None,
                    winding_rule: libgfx_rust::WindingRule::Nonzero,
                    svg_hit_areas: 0,
                    svg_stroke: None,
                    writing_mode: 0,
                    inline_axis_is_reverse: false,
                    block_axis_is_reverse: false,
                    containing_block: NodeSlotId::INVALID,
                    can_produce_caret_position: false,
                });
            }
            output.blocking_wheel_event_region_count += content.blocking;
            ProducerOutcome {
                live: content.live,
                reads_window_focus: content.reads_window_focus,
            }
        }

        fn output_position(&self) -> OutputPosition {
            OutputPosition {
                bytes: self.output.recorder.byte_size() as u32,
                hits: self.output.hit_test_list.items.len() as u32,
                blocking_wheel_event_regions: self.output.blocking_wheel_event_region_count,
            }
        }

        fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32) {
            let source = self.source.as_ref().expect("a published recording to copy from");
            self.output.recorder.append_cached_commands(&source.display_list, bytes);
            if !hits.is_empty() {
                self.output
                    .hit_test_list
                    .append_copies_of(&source.hit_test_items[hits.start as usize..hits.end as usize]);
            }
            self.output.blocking_wheel_event_region_count += blocking_wheel_event_regions;
        }

        fn end_recorded_run(&mut self) {
            self.output.recorder.end_chunk();
        }

        fn bound_of_item(&mut self, _scope: PaintScope, item: PaintOrderItem) -> Option<InkBound> {
            self.bounds_asked += 1;
            let PaintOrderItem::Scope(child) = item else {
                return None;
            };
            let (first, last) = *self.rows_painted_in.get(&child.owner)?;
            Some(InkBound {
                rect: rect_of_rows(first, last),
                space: crate::painting::display_list::commands::SpatialNodeIndex(0),
            })
        }

        fn united_bound(&mut self, first: InkBound, second: InkBound) -> Option<InkBound> {
            Some(InkBound {
                rect: first.rect.united(second.rect),
                space: first.space,
            })
        }

        fn is_outside_window(&mut self, bound: InkBound) -> bool {
            let (first, last) = self.window.expect("only a recording of a window asks");
            bound.rect.intersected(rect_of_rows(first, last)).is_empty()
        }

        fn is_wholly_inside_window(&mut self, bound: InkBound) -> bool {
            let (first, last) = self.window.expect("only a recording of a window asks");
            rect_of_rows(first, last).contains_rect(bound.rect)
        }

        fn cuts_off_what_it_holds(&mut self, owner: NodeSlotId) -> bool {
            self.cutting_off_what_they_hold.contains(&owner)
        }

        fn damage_of_row(&mut self, row: NodeSlotId) -> PaintDamage {
            self.damage.get(&row).copied().unwrap_or_default()
        }

        fn placement_of_row(&mut self, row: NodeSlotId) -> RowPlacement {
            RowPlacement {
                origin: CssPixelPoint::new(
                    CssPixels::from_integer(self.origins.get(&row).copied().unwrap_or_default()),
                    CssPixels::default(),
                ),
                later_fragments: 0,
            }
        }

        fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
            self.descendant_readers.contains(&(owner, kind))
        }

        fn observe(&mut self, event: AssemblyEvent) {
            self.events.push(event);
        }
    }

    // Records the canvas prologue and assembles the viewport context from the host's source frame.
    fn record_frame_with_focus_change(host: &mut TestHost, window_focus_changed: bool) -> Frame {
        host.output = new_output();
        host.recorded.clear();
        host.events.clear();
        host.output.recorder.fill_rect(
            IntRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            Color::from_rgb(255, 255, 255),
            ForceDarkRole::Background,
        );
        host.output.recorder.end_chunk();
        let prologue_bytes = host.output.recorder.byte_size() as u32;
        let published = host
            .source
            .as_ref()
            .map(|frame| (Arc::clone(&frame.root), frame.prologue_bytes));
        let window = host.window;
        let coverage = match (window, host.source.as_ref().map(|frame| frame.window)) {
            (None, _) => Coverage::WholeDocument,
            (Some(window), Some(Some(published_window))) if window == published_window => Coverage::PublishedWindow,
            (Some(_), _) => Coverage::AnotherWindow,
        };
        let root = Assembler::new(host, window_focus_changed, coverage).assemble_root(
            context(row(0)),
            published.as_ref().map(|(root, prologue_bytes)| (root, *prologue_bytes)),
        );
        let output = std::mem::replace(&mut host.output, new_output());
        let hit_test_items = Arc::try_unwrap(output.hit_test_list.items).expect("items are unshared");
        Frame {
            display_list: output.recorder.into_builder().finish(),
            hit_test_items,
            prologue_bytes,
            root,
            window,
        }
    }

    fn record_frame(host: &mut TestHost) -> Frame {
        record_frame_with_focus_change(host, false)
    }

    // Publishes `frame`: the next frame copies from it, and the damage it painted is consumed.
    fn publish(host: &mut TestHost, frame: Frame) {
        host.source = Some(frame);
        host.damage.clear();
    }

    fn assert_matches_from_scratch(host: &mut TestHost, assembled: &Frame) {
        let recorded = std::mem::take(&mut host.recorded);
        let events = std::mem::take(&mut host.events);
        let source = host.source.take();
        let fresh = record_frame(host);
        host.source = source;
        host.recorded = recorded;
        host.events = events;
        assert_eq!(
            assembled.display_list.bytes, fresh.display_list.bytes,
            "command bytes differ"
        );
        assert_eq!(assembled.display_list.command_runs, fresh.display_list.command_runs);
        assert_eq!(assembled.display_list.chunks, fresh.display_list.chunks);
        // Comparing whole items would compare their paths through the host's path equality.
        let item_identities = |items: &[HitTestItem]| -> Vec<(NodeSlotId, Option<usize>)> {
            items
                .iter()
                .map(|item| (item.paintable, item.caret_line_index))
                .collect()
        };
        assert_eq!(
            item_identities(&assembled.hit_test_items),
            item_identities(&fresh.hit_test_items)
        );
        assert_eq!(assembled.root.output, fresh.root.output);
    }

    // A viewport context (row 0) that draws a background and lists the backgrounds and then the foregrounds of
    // six cards (rows 1..=6), two to a grid row. A card draws a background, and a foreground with hit-test items.
    fn cards_host() -> TestHost {
        let mut host = TestHost::default();
        let cards = 1..=6;
        let mut items = vec![phase(PaintPhase::Background)];
        items.extend(cards.clone().map(|card| child(backgrounds(row(card)))));
        items.extend(cards.clone().map(|card| child(foreground(row(card)))));
        host.plan(context(row(0)), true, &items);
        host.produce(row(0), ProducerKind::DrawBackground, content(1, 0));
        for card in cards {
            let grid_row = (card as i32 - 1) / 2;
            host.grid_rows.insert(row(card), grid_row);
            host.rows_painted_in.insert(row(card), (grid_row, grid_row));
            host.plan(backgrounds(row(card)), true, &[phase(PaintPhase::Background)]);
            host.plan(foreground(row(card)), true, &[phase(PaintPhase::Foreground)]);
            host.produce(row(card), ProducerKind::DrawBackground, content(1, 0));
            host.produce(row(card), ProducerKind::HitForeground, content(0, 2));
            host.produce(row(card), ProducerKind::DrawForeground, content(card, 0));
        }
        host
    }

    #[test]
    fn a_plan_is_cut_where_its_items_leave_a_grid_row_and_around_walked_children() {
        let mut host = cards_host();
        host.walked.insert(foreground(row(4)));

        let frame = record_frame(&mut host);

        // The background and the backgrounds of the cards of a grid row, three times, and then the foregrounds,
        // where the grid starts over and the walked card stands alone.
        assert_eq!(
            frame.root.plan.segments,
            vec![
                Segment::Run(0..3),
                Segment::Run(3..5),
                Segment::Run(5..7),
                Segment::Run(7..9),
                Segment::Run(9..10),
                Segment::Walked(10),
                Segment::Run(11..13),
            ]
        );
        assert_eq!(frame.root.output.hits, 12);
        assert_eq!(
            frame.root.output.bytes as usize,
            frame.display_list.bytes.len() - frame.prologue_bytes as usize
        );
        assert!(frame_is_unchanged(&frame.root, false, false, Coverage::WholeDocument));
        assert!(!frame_is_unchanged(&frame.root, true, false, Coverage::WholeDocument));
    }

    #[test]
    fn a_run_never_holds_more_items_than_the_cap() {
        let mut host = TestHost::default();
        let items: Vec<_> = (1..=MAX_ITEMS_IN_A_RUN as u32 + 2)
            .map(|card| child(foreground(row(card))))
            .collect();
        host.plan(context(row(0)), true, &items);

        let frame = record_frame(&mut host);

        let cap = MAX_ITEMS_IN_A_RUN as u32;
        assert_eq!(
            frame.root.plan.segments,
            vec![Segment::Run(0..cap), Segment::Run(cap..cap + 2)]
        );
    }

    #[test]
    fn damage_records_the_run_it_reaches_and_the_runs_beside_it_are_copied() {
        let mut host = cards_host();
        let first = record_frame(&mut host);
        publish(&mut host, first);
        host.contents
            .get_mut(&(row(3), ProducerKind::DrawForeground))
            .unwrap()
            .version = 2;
        host.push(row(3), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);

        let frame = record_frame(&mut host);

        // Only the foregrounds of the second grid row record again: foreground damage misses the run of the
        // card's background.
        assert_eq!(host.recorded_rows(), vec![3, 4]);
        assert_eq!(host.action_of(context(row(0))), Some(ScopeAction::Assembled));
        assert!(host.copied_scopes().contains(&backgrounds(row(3))));
        assert!(host.copied_scopes().contains(&foreground(row(2))));
        assert!(!host.copied_scopes().contains(&foreground(row(4))));
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn a_walked_child_is_copied_whole_until_damage_reaches_below_it() {
        let mut host = cards_host();
        host.walked.insert(foreground(row(4)));
        host.plan(
            foreground(row(4)),
            true,
            &[phase(PaintPhase::Foreground), child(foreground(row(7)))],
        );
        host.plan(foreground(row(7)), true, &[phase(PaintPhase::Foreground)]);
        host.produce(row(7), ProducerKind::DrawForeground, content(2, 0));
        let first = record_frame(&mut host);
        publish(&mut host, first);

        // Damage beside the walked child leaves it copied as one piece, with the node the published recording
        // kept for it.
        host.push(row(1), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_frame(&mut host);
        assert!(host.copied_scopes().contains(&foreground(row(4))));
        assert_eq!(host.action_of(foreground(row(4))), None);
        let walked_child_of = |frame: &Frame| {
            frame
                .root
                .segments
                .iter()
                .find_map(|segment| match segment {
                    RecordedSegment::Walked(scope) => Some(Arc::clone(scope)),
                    RecordedSegment::Run(_) | RecordedSegment::Outside => None,
                })
                .unwrap()
        };
        assert!(Arc::ptr_eq(
            &walked_child_of(&second),
            &walked_child_of(host.source.as_ref().unwrap())
        ));
        assert_matches_from_scratch(&mut host, &second);
        publish(&mut host, second);

        // Damage below it has it walked, and the run the damage reaches recorded. The hint that something below the
        // card was damaged does not say in which of its scopes, so the run with the card's backgrounds records too.
        host.contents
            .get_mut(&(row(7), ProducerKind::DrawForeground))
            .unwrap()
            .version = 2;
        host.push(row(7), PaintDamage::DRAW_FOREGROUND);
        host.push(row(4), PaintDamage::DESCENDANT_READERS);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let third = record_frame(&mut host);
        assert_eq!(host.action_of(foreground(row(4))), Some(ScopeAction::Assembled));
        assert_eq!(host.recorded_rows(), vec![3, 4, 7]);
        assert_matches_from_scratch(&mut host, &third);
    }

    #[test]
    fn a_child_scope_an_ancestor_moved_records_again_though_nothing_was_pushed_for_it() {
        let mut host = cards_host();
        let first = record_frame(&mut host);
        publish(&mut host, first);
        host.origins.insert(row(5), 40);

        let frame = record_frame(&mut host);

        assert_eq!(host.recorded_rows(), vec![5, 6, 5, 6]);
        assert!(host.copied_scopes().contains(&foreground(row(4))));
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn a_plan_that_gains_an_item_still_copies_the_runs_it_did_not_touch() {
        let mut host = cards_host();
        let first = record_frame(&mut host);
        publish(&mut host, first);
        let mut items = host.plans[&context(row(0))].items.to_vec();
        items.push(child(foreground(row(7))));
        host.plan(context(row(0)), true, &items);
        host.grid_rows.insert(row(7), 3);
        host.plan(foreground(row(7)), true, &[phase(PaintPhase::Foreground)]);
        host.produce(row(7), ProducerKind::DrawForeground, content(1, 0));
        host.push(row(0), PaintDamage::ORDER);

        let frame = record_frame(&mut host);

        assert_eq!(host.action_of(context(row(0))), Some(ScopeAction::Replanned));
        assert_eq!(host.recorded_rows(), vec![7]);
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn output_recorded_every_frame_or_read_from_the_window_focus_is_not_copied() {
        let mut host = cards_host();
        host.contents
            .get_mut(&(row(1), ProducerKind::DrawForeground))
            .unwrap()
            .live = true;
        host.contents
            .get_mut(&(row(5), ProducerKind::DrawForeground))
            .unwrap()
            .reads_window_focus = true;
        let first = record_frame(&mut host);
        assert!(!frame_is_unchanged(&first.root, false, false, Coverage::WholeDocument));
        publish(&mut host, first);

        let quiet = record_frame(&mut host);
        assert_eq!(host.recorded_rows(), vec![1, 2]);
        assert_matches_from_scratch(&mut host, &quiet);
        publish(&mut host, quiet);

        let refocused = record_frame_with_focus_change(&mut host, true);
        assert_eq!(host.recorded_rows(), vec![1, 2, 5, 6]);
        assert_matches_from_scratch(&mut host, &refocused);
    }

    #[test]
    fn a_stacking_context_that_stops_painting_records_nothing() {
        let mut host = cards_host();
        let first = record_frame(&mut host);
        publish(&mut host, first);
        let items = host.plans[&context(row(0))].items.to_vec();
        host.plan(context(row(0)), false, &items);
        host.push(row(0), PaintDamage::ELIGIBILITY);

        let frame = record_frame(&mut host);

        assert_eq!(host.action_of(context(row(0))), Some(ScopeAction::Inactive));
        assert!(host.recorded.is_empty());
        assert_eq!(frame.root.output, OutputSize::default());
        assert_eq!(frame.display_list.bytes.len(), frame.prologue_bytes as usize);
    }

    #[test]
    fn a_recording_of_a_window_leaves_out_the_runs_and_walked_children_outside_it() {
        let mut host = cards_host();
        host.walked.insert(foreground(row(4)));
        host.window = Some((2, 2));

        let frame = record_frame(&mut host);

        // The run with the viewport's own background is not bounded, so the backgrounds of the first two cards
        // are recorded with it.
        assert_eq!(
            host.left_out_scopes(),
            vec![
                backgrounds(row(3)),
                backgrounds(row(4)),
                foreground(row(1)),
                foreground(row(2)),
                foreground(row(3)),
                foreground(row(4)),
            ]
        );
        assert_eq!(
            host.rows_recorded().into_iter().collect::<Vec<_>>(),
            vec![0, 1, 2, 5, 6]
        );
        assert!(matches!(frame.root.segments[1], RecordedSegment::Outside));
        assert!(frame.root.leaves_out_what_lies_outside_the_window);
        assert_eq!(
            frame.root.output.bytes as usize,
            frame.display_list.bytes.len() - frame.prologue_bytes as usize
        );
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn a_window_that_moved_records_what_came_into_it_and_drops_what_left_it() {
        let mut host = cards_host();
        host.window = Some((0, 0));
        let first = record_frame(&mut host);
        assert_eq!(host.rows_recorded().into_iter().collect::<Vec<_>>(), vec![0, 1, 2]);
        publish(&mut host, first);

        host.window = Some((1, 2));
        host.recorded.clear();
        host.events.clear();
        let second = record_frame(&mut host);
        assert_eq!(host.rows_recorded().into_iter().collect::<Vec<_>>(), vec![3, 4, 5, 6]);
        assert_eq!(host.left_out_scopes(), vec![foreground(row(1)), foreground(row(2))]);
        assert!(host.copied_scopes().contains(&backgrounds(row(1))));
        assert_matches_from_scratch(&mut host, &second);
        publish(&mut host, second);

        // With the window where it was, nothing is recorded, and nothing asks what bounds the segments again but
        // for the viewport's scope itself.
        host.recorded.clear();
        host.bounds_asked = 0;
        let third = record_frame(&mut host);
        assert!(host.recorded.is_empty());
        assert_eq!(host.bounds_asked, 1);
        assert!(frame_is_unchanged(&third.root, false, false, Coverage::PublishedWindow));
        assert!(!frame_is_unchanged(&third.root, false, false, Coverage::AnotherWindow));
        assert!(!frame_is_unchanged(&third.root, false, false, Coverage::WholeDocument));
        assert_matches_from_scratch(&mut host, &third);
    }

    #[test]
    fn a_recording_of_the_whole_document_records_what_a_window_left_out() {
        let mut host = cards_host();
        host.window = Some((0, 0));
        let first = record_frame(&mut host);
        publish(&mut host, first);

        host.window = None;
        host.recorded.clear();
        host.events.clear();
        let second = record_frame(&mut host);
        assert_eq!(host.rows_recorded().into_iter().collect::<Vec<_>>(), vec![3, 4, 5, 6]);
        assert!(host.left_out_scopes().is_empty());
        assert!(!second.root.leaves_out_what_lies_outside_the_window);
        assert!(frame_is_unchanged(&second.root, false, false, Coverage::WholeDocument));
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn a_clean_walked_child_is_copied_whole_where_neither_window_leaves_any_of_it_out() {
        let mut host = cards_host();
        // The fourth card's foreground is walked, and paints two cards of its own in the second and fourth rows.
        host.walked.insert(foreground(row(4)));
        host.rows_painted_in.insert(row(4), (1, 3));
        host.plan(
            foreground(row(4)),
            true,
            &[
                phase(PaintPhase::Foreground),
                child(foreground(row(7))),
                child(foreground(row(8))),
            ],
        );
        for (card, grid_row) in [(7, 1), (8, 3)] {
            host.grid_rows.insert(row(card), grid_row);
            host.rows_painted_in.insert(row(card), (grid_row, grid_row));
            host.plan(foreground(row(card)), true, &[phase(PaintPhase::Foreground)]);
            host.produce(row(card), ProducerKind::DrawForeground, content(1, 0));
        }
        let record_in_window = |host: &mut TestHost, window: (i32, i32)| {
            host.window = Some(window);
            host.recorded.clear();
            host.events.clear();
            let frame = record_frame(host);
            assert_matches_from_scratch(host, &frame);
            frame
        };

        // The window cuts the walked card: the card in the fourth row is left out.
        let first = record_in_window(&mut host, (0, 1));
        assert!(host.rows_recorded().contains(&7));
        assert!(host.left_out_scopes().contains(&foreground(row(8))));
        publish(&mut host, first);

        // The window holds all of the walked card now, but the recording it would be copied from left some of it
        // out, so it is walked and what was left out is recorded.
        let second = record_in_window(&mut host, (0, 3));
        assert_eq!(host.action_of(foreground(row(4))), Some(ScopeAction::Assembled));
        assert_eq!(host.rows_recorded().into_iter().collect::<Vec<_>>(), vec![5, 6, 8]);
        publish(&mut host, second);

        // Both windows hold all of it: it is copied whole.
        let third = record_in_window(&mut host, (0, 5));
        assert!(host.copied_scopes().contains(&foreground(row(4))));
        assert_eq!(host.action_of(foreground(row(4))), None);
        assert!(host.recorded.is_empty());
        publish(&mut host, third);

        // The window cuts it again: it is walked, and the card in the fourth row is dropped.
        record_in_window(&mut host, (0, 1));
        assert_eq!(host.action_of(foreground(row(4))), Some(ScopeAction::Assembled));
        assert!(host.left_out_scopes().contains(&foreground(row(8))));
        assert!(host.recorded.is_empty());
    }

    #[test]
    fn what_a_box_cuts_off_is_left_out_though_the_box_is_wholly_inside_the_window() {
        let mut host = cards_host();
        // The fourth card is in the second row, and scrolls two cards of its own, one of them far down.
        host.walked.insert(foreground(row(4)));
        host.cutting_off_what_they_hold.insert(row(4));
        host.plan(
            foreground(row(4)),
            true,
            &[
                phase(PaintPhase::Foreground),
                child(foreground(row(7))),
                child(foreground(row(8))),
            ],
        );
        for (card, grid_row) in [(7, 1), (8, 9)] {
            host.grid_rows.insert(row(card), grid_row);
            host.rows_painted_in.insert(row(card), (grid_row, grid_row));
            host.plan(foreground(row(card)), true, &[phase(PaintPhase::Foreground)]);
            host.produce(row(card), ProducerKind::DrawForeground, content(1, 0));
        }
        host.window = Some((0, 2));

        let frame = record_frame(&mut host);

        assert_eq!(host.left_out_scopes(), vec![foreground(row(8))]);
        assert!(host.rows_recorded().contains(&7));
        assert!(frame.root.leaves_out_what_lies_outside_the_window);
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn a_scope_that_holds_what_a_box_cuts_off_is_not_copied_whole_into_another_window() {
        let mut host = cards_host();
        // The fourth card's foreground is walked and holds a walked scope of a box that scrolls two cards, one of
        // them far down.
        host.walked.insert(foreground(row(4)));
        host.walked.insert(foreground(row(9)));
        host.cutting_off_what_they_hold.insert(row(9));
        host.plan(
            foreground(row(4)),
            true,
            &[phase(PaintPhase::Foreground), child(foreground(row(9)))],
        );
        host.grid_rows.insert(row(9), 1);
        host.rows_painted_in.insert(row(9), (1, 1));
        host.plan(
            foreground(row(9)),
            true,
            &[
                phase(PaintPhase::Foreground),
                child(foreground(row(7))),
                child(foreground(row(8))),
            ],
        );
        for (card, grid_row) in [(7, 1), (8, 9)] {
            host.grid_rows.insert(row(card), grid_row);
            host.rows_painted_in.insert(row(card), (grid_row, grid_row));
            host.plan(foreground(row(card)), true, &[phase(PaintPhase::Foreground)]);
            host.produce(row(card), ProducerKind::DrawForeground, content(1, 0));
        }

        // A recording of the whole document holds the far card.
        let whole = record_frame(&mut host);
        assert!(host.rows_recorded().contains(&8));
        publish(&mut host, whole);

        // The fourth card is wholly inside the window, and nothing of it was left out, but what the box in it cuts
        // off reaches outside: the card is walked and the far card is dropped.
        host.window = Some((0, 2));
        host.recorded.clear();
        host.events.clear();
        let windowed = record_frame(&mut host);
        assert_eq!(host.action_of(foreground(row(4))), Some(ScopeAction::Assembled));
        assert_eq!(host.left_out_scopes(), vec![foreground(row(8))]);
        assert!(host.recorded.is_empty());
        assert_matches_from_scratch(&mut host, &windowed);
    }
}
