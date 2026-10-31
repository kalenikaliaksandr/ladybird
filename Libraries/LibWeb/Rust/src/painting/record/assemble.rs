/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Assembles a frame from the retained paint-order tree and the pushed damage. Scopes on a
//! damaged path are walked, clean children are copied from the published recording by range, and
//! damaged producers are recorded again under explicit contexts. Nothing is validated on the
//! way: when assembly starts, the damage set is complete by construction.
//!
//! A frame that rewrites the tree installs the cull summary of every scope it walks or records
//! with its child list, and a scope it copies keeps the one it has.
//!
//! A frame recorded around the scroll positions leaves out every producer whose commands the
//! interest rect of their scroll node misses, as an entry without bytes that keeps its hit-test
//! items and blocking wheel regions. A clean scope is walked only where an interest rect it reaches
//! changed across what it draws, and a producer left out records once it is not left out any more.

use super::PaintPhase;
use super::order_tree::{
    ChildEntry, ChildKey, CullReach, OutputSize, PaintOrderTree, ProducerKind, ScopeId, ScrollNodeSet,
};
use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::display_list::commands::SpatialNodeIndex;
use crate::painting::paint_order_plan::{PaintOrderItem, PaintProducer, PaintScope, PaintScopeKind};
use crate::painting::record::damage::PaintDamage;
use libgfx_rust::IntRect;
use smallvec::SmallVec;
use std::ops::Range;

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
fn producer_kinds(producer: PaintProducer) -> SmallVec<[ProducerKind; 5]> {
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
    // Where its commands land if a recording around the scroll positions may leave them out.
    pub cull_reach: CullReach,
}

/// The reach of a scope's own producers while assembly goes through them. Only a producer that
/// records says where it draws, so a copied producer whose output may be left out keeps the reach
/// that the scope's own producers had.
#[derive(Default)]
struct OwnProducersReach {
    recorded: CullReach,
    copies_cullable_output: bool,
}

impl OwnProducersReach {
    fn note_recorded(&mut self, (entry, reach): (ChildEntry, CullReach)) -> ChildEntry {
        self.recorded = self.recorded.united(reach);
        entry
    }

    fn note_copied(&mut self, entry: ChildEntry) -> ChildEntry {
        self.copies_cullable_output |= entry.has_cullable_output();
        entry
    }

    fn finish(self, published: CullReach) -> CullReach {
        if self.copies_cullable_output {
            self.recorded.united(published)
        } else {
            self.recorded
        }
    }
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
    // Left out without recording anything.
    ProducerCulled(NodeSlotId, ProducerKind),
}

/// What a frame leaves out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssemblyCulling {
    /// Nothing: producers the published recording left out record again.
    LeavesNothingOut,
    /// Every producer whose commands the interest rect of their scroll node misses, as the host tells.
    ByInterest {
        /// The scroll nodes whose interest rect differs from the published recording's.
        changed_scroll_nodes: ScrollNodeSet,
        /// Set when the published recording placed its producers in other scroll nodes, or left out nothing by
        /// interest: every producer is placed again.
        places_every_producer_again: bool,
    },
}

impl AssemblyCulling {
    // Whether the frame leaves out what the published recording, whose root has `root_output`, left out.
    fn keeps_published_cull_state(self, root_output: OutputSize) -> bool {
        match self {
            Self::LeavesNothingOut => !root_output.culled,
            Self::ByInterest {
                changed_scroll_nodes,
                places_every_producer_again,
            } => changed_scroll_nodes.is_empty() && !places_every_producer_again,
        }
    }
}

/// The end of the output being assembled: command bytes, hit-test items and blocking wheel
/// regions so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputPosition {
    pub bytes: u32,
    pub hits: u32,
    pub blocking_wheel_event_regions: u32,
}

/// What assembly needs from the recorder, which owns the output and the published recording.
pub(crate) trait AssemblyHost {
    fn plan_scope(&mut self, scope: PaintScope) -> ScopePlan;

    /// Records one producer of `owner` under the contexts it needs.
    fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> ProducerOutcome;

    /// Where the commands of a producer about to be assembled land, if a frame recorded around the scroll positions may
    /// leave them out.
    fn producer_cull_reach(&mut self, owner: NodeSlotId, kind: ProducerKind) -> CullReach;

    /// Whether the interest rect of the scroll node that commands landing where `reach` says land in misses them.
    fn interest_misses(&self, reach: CullReach) -> bool;

    /// The interest rect the published recording left out by in `scroll_node` and the one this frame leaves out by,
    /// unless one of them leaves out nothing there by a rect.
    fn published_and_current_interest_rects_of(&self, scroll_node: SpatialNodeIndex) -> Option<(IntRect, IntRect)>;

    /// Leaves a producer out: records the hit-test items and counts the blocking wheel regions it would record, without
    /// its commands, and tells whether those would have routed wheels.
    fn record_producer_leaving_out_commands(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool;

    /// Whether the published commands of a producer in `bytes` route wheels.
    fn published_output_routes_wheels(&mut self, owner: NodeSlotId, kind: ProducerKind, bytes: Range<u32>) -> bool;

    fn output_position(&self) -> OutputPosition;

    /// Appends the published recording's command bytes and hit-test items in the given ranges.
    fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32);

    /// Every damaged row, including the rows of subtrees whose root moved.
    fn damaged_rows(&mut self) -> Vec<NodeSlotId>;

    /// The damage of a row with a moved ancestor's subtree expansion folded in.
    fn effective_damage(&mut self, row: NodeSlotId) -> PaintDamage;

    fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool;

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

// Consecutive clean children are copied with one range append each for commands and items.
#[derive(Clone, Copy)]
struct PendingCopy {
    start: SourceCursor,
    end: SourceCursor,
    blocking_wheel_event_regions: u32,
}

/// Whether the published recording can be returned as it is: nothing was pushed, nothing in it is
/// recorded every frame or read a window focus that changed, nothing is left out differently, and
/// there is a published tree at all.
pub(crate) fn frame_is_unchanged(
    tree: &PaintOrderTree,
    has_damage: bool,
    window_focus_changed: bool,
    culling: AssemblyCulling,
) -> bool {
    let root = tree.root_entry();
    !tree.root().is_none()
        && !has_damage
        && !root.is_live()
        && !(window_focus_changed && root.reads_window_focus())
        && culling.keeps_published_cull_state(root.output())
}

// The published output of a clean producer: its entry, and where its output starts in the published recording.
#[derive(Clone, Copy)]
struct PublishedProducer {
    entry: ChildEntry,
    cursor: SourceCursor,
}

pub(crate) struct Assembler<'a, H: AssemblyHost> {
    host: &'a mut H,
    tree: &'a mut PaintOrderTree,
    // The byte offset of the published root scope, when a published recording can be copied from.
    source_prologue_bytes: Option<u32>,
    // Off for a frame that publishes nothing: it still marks damaged paths, but the tree's
    // structure keeps describing the published recording.
    rewrites_tree: bool,
    // The published recording was made in the other window focus state.
    window_focus_changed: bool,
    culling: AssemblyCulling,
    pending_copy: Option<PendingCopy>,
}

impl<'a, H: AssemblyHost> Assembler<'a, H> {
    pub(crate) fn new(
        host: &'a mut H,
        tree: &'a mut PaintOrderTree,
        source_prologue_bytes: Option<u32>,
        rewrites_tree: bool,
        window_focus_changed: bool,
        culling: AssemblyCulling,
    ) -> Self {
        Self {
            host,
            tree,
            source_prologue_bytes,
            rewrites_tree,
            window_focus_changed,
            culling,
            pending_copy: None,
        }
    }

    // Copying an entry would keep output that is recorded every frame, or that read the window
    // focus before it changed.
    fn records_again(&self, entry: &ChildEntry) -> bool {
        entry.is_live() || (self.window_focus_changed && entry.reads_window_focus())
    }

    /// Records the frame's root scope after the prologue and returns its output. A frame that
    /// rewrites the tree leaves the assembled structure there as pending changes for the caller
    /// to publish or discard.
    pub(crate) fn assemble_root(mut self, root_scope: PaintScope) -> OutputSize {
        self.tree.begin_recording();
        // Damage only steers what a frame copies; a frame without a published recording to copy
        // from records everything and never consults it.
        if self.source_prologue_bytes.is_some() {
            for row in self.host.damaged_rows() {
                for scope in self.tree.scopes_owned_by(row) {
                    self.tree.mark_path_to_root_damaged(scope);
                }
            }
        }
        let published_root = self.tree.root();
        let output = match self.source_prologue_bytes {
            Some(prologue_bytes) if !published_root.is_none() && self.tree.scope_of(published_root) == root_scope => {
                let cursor = SourceCursor {
                    bytes: prologue_bytes,
                    hits: 0,
                };
                let entry = self.assemble_scope(published_root, cursor);
                self.flush_pending_copy();
                if self.rewrites_tree {
                    self.tree.finish_recording(published_root, entry);
                }
                entry.output()
            }
            _ if self.rewrites_tree => {
                if !published_root.is_none() {
                    self.tree.detach_scope(published_root);
                }
                let root = self.tree.allocate_scope(root_scope, ScopeId::NONE);
                let entry = self.record_scope_fresh(root);
                self.tree.finish_recording(root, entry);
                entry.output()
            }
            _ => self.record_scope_outside_tree(root_scope),
        };
        debug_assert!(self.pending_copy.is_none());
        if self.rewrites_tree && super::cull::verifies_interest_culling() {
            self.audit_culling(output);
        }
        output
    }

    fn assemble_scope(&mut self, id: ScopeId, cursor: SourceCursor) -> ChildEntry {
        let scope = self.tree.scope_of(id);
        let damage = self.host.effective_damage(scope.owner);
        let order_damage = match scope.kind {
            PaintScopeKind::PaintedAsStackingContext => {
                PaintDamage::ORDER | PaintDamage::CONTEXT_ORDER | PaintDamage::ELIGIBILITY
            }
            PaintScopeKind::Descendants(_) => PaintDamage::ORDER,
        };
        if damage.intersects(order_damage) {
            return self.rebuild_scope(id, cursor, damage);
        }
        self.host
            .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Assembled));
        let published: SmallVec<[ChildEntry; 16]> = self.tree.children(id).iter().copied().collect();
        let published_own_producers = self.tree.cull_summary(id).own_producers;
        let mut entries: SmallVec<[ChildEntry; 16]> = SmallVec::with_capacity(published.len());
        let mut total = OutputSize::default();
        let mut own_producers = OwnProducersReach::default();
        let mut child_cursor = cursor;
        for entry in published {
            let published_output = entry.output();
            let assembled = match entry.key() {
                ChildKey::Scope(child) => self.assemble_or_copy_child_scope(child, entry, child_cursor),
                ChildKey::Producer(kind) => {
                    let clean =
                        (!self.producer_is_damaged(scope.owner, kind, damage, &entry)).then_some(PublishedProducer {
                            entry,
                            cursor: child_cursor,
                        });
                    self.assemble_producer(scope.owner, kind, clean, published_own_producers, &mut own_producers)
                }
            };
            child_cursor.advance(published_output);
            total.add(assembled.output());
            entries.push(assembled);
        }
        if self.rewrites_tree {
            let own_producers = own_producers.finish(published_own_producers);
            self.tree.set_children(id, &entries, own_producers);
        }
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        ChildEntry::scope(id, total)
    }

    /// Whether a published entry with `output`, whose commands land where `reach` says, may be left out differently
    /// now.
    fn may_cull_differently(&self, reach: CullReach, output: OutputSize) -> bool {
        match self.culling {
            AssemblyCulling::LeavesNothingOut => output.culled,
            AssemblyCulling::ByInterest {
                changed_scroll_nodes,
                places_every_producer_again,
            } => {
                places_every_producer_again
                    || (output.has_cullable_output
                        && reach.touches(changed_scroll_nodes)
                        && !self.rect_changes_cut_alike(reach, changed_scroll_nodes))
            }
        }
    }

    // Whether, of the scroll nodes whose interest rect changed, the commands landing where `reach` says land in one
    // alone, within bounds that its published and current rect cut alike: those leave out every command they left out
    // and nothing more.
    fn rect_changes_cut_alike(&self, reach: CullReach, changed_scroll_nodes: ScrollNodeSet) -> bool {
        let Some((scroll_node, bounds)) = reach.bounds_in_only_one_of(changed_scroll_nodes) else {
            return false;
        };
        self.host
            .published_and_current_interest_rects_of(scroll_node)
            .is_some_and(|(published, current)| bounds.intersected(published) == bounds.intersected(current))
    }

    // A clean child scope is copied unless something in it records again or is left out differently.
    fn assemble_or_copy_child_scope(&mut self, child: ScopeId, entry: ChildEntry, cursor: SourceCursor) -> ChildEntry {
        let walks_child = self.tree.is_on_damaged_path(child)
            || self.records_again(&entry)
            || self.may_cull_differently(self.tree.cull_summary(child).subtree, entry.output());
        if walks_child {
            return self.assemble_scope(child, cursor);
        }
        self.copy(cursor, entry.output());
        let child_scope = self.tree.scope_of(child);
        self.host.observe(AssemblyEvent::ScopeCopied(child_scope));
        entry
    }

    fn producer_is_damaged(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        damage: PaintDamage,
        entry: &ChildEntry,
    ) -> bool {
        damage.intersects(kind.damage())
            || self.records_again(entry)
            || (damage.contains(PaintDamage::DESCENDANT_READERS) && self.host.producer_reads_descendants(owner, kind))
    }

    // The plan is rebuilt; published children it still lists are copied or walked from their
    // old positions, the rest are recorded, and published children it no longer lists retire.
    fn rebuild_scope(&mut self, id: ScopeId, cursor: SourceCursor, damage: PaintDamage) -> ChildEntry {
        let scope = self.tree.scope_of(id);
        let plan = self.host.plan_scope(scope);
        let published: SmallVec<[ChildEntry; 16]> = self.tree.children(id).iter().copied().collect();
        let published_own_producers = self.tree.cull_summary(id).own_producers;
        let mut published_cursors: SmallVec<[SourceCursor; 16]> = SmallVec::with_capacity(published.len());
        let mut child_cursor = cursor;
        for entry in &published {
            published_cursors.push(child_cursor);
            child_cursor.advance(entry.output());
        }
        let mut matched: SmallVec<[bool; 16]> = SmallVec::from_elem(false, published.len());
        // A stacking context with thousands of hoisted children is replanned when one of them
        // appears or disappears; matching by key must not scan the list per item.
        let published_index_by_key: Option<FastMap<ChildKey, usize>> = (published.len() > 16).then(|| {
            published
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.key(), index))
                .collect()
        });
        let published_index_of = |key: ChildKey| match &published_index_by_key {
            Some(index_by_key) => index_by_key.get(&key).copied(),
            None => published.iter().position(|entry| entry.key() == key),
        };
        if !plan.active {
            self.host
                .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Inactive));
            self.install_replanned_children(id, &published, &matched, &[], CullReach::NOTHING);
            self.host.observe(AssemblyEvent::ScopeEnd(scope));
            return ChildEntry::scope(id, OutputSize::default());
        }
        self.host
            .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Replanned));
        let mut entries: SmallVec<[ChildEntry; 16]> = SmallVec::with_capacity(plan.items.len());
        let mut total = OutputSize::default();
        let mut own_producers = OwnProducersReach::default();
        for item in plan.items {
            match item {
                PaintOrderItem::Producer(producer) => {
                    for kind in producer_kinds(producer) {
                        let published_index = published_index_of(ChildKey::Producer(kind));
                        if let Some(index) = published_index {
                            matched[index] = true;
                        }
                        let clean = published_index
                            .filter(|index| !self.producer_is_damaged(scope.owner, kind, damage, &published[*index]))
                            .map(|index| PublishedProducer {
                                entry: published[index],
                                cursor: published_cursors[index],
                            });
                        let assembled = self.assemble_producer(
                            scope.owner,
                            kind,
                            clean,
                            published_own_producers,
                            &mut own_producers,
                        );
                        total.add(assembled.output());
                        entries.push(assembled);
                    }
                }
                PaintOrderItem::Scope(child_scope) => {
                    let found = self
                        .tree
                        .published_scope(child_scope, id)
                        .and_then(|child| published_index_of(ChildKey::Scope(child)).map(|index| (child, index)));
                    let assembled = match found {
                        Some((child, index)) => {
                            matched[index] = true;
                            self.assemble_or_copy_child_scope(child, published[index], published_cursors[index])
                        }
                        None if self.rewrites_tree => {
                            let child = self.tree.allocate_scope(child_scope, id);
                            self.record_scope_fresh(child)
                        }
                        None => {
                            total.add(self.record_scope_outside_tree(child_scope));
                            continue;
                        }
                    };
                    total.add(assembled.output());
                    entries.push(assembled);
                }
            }
        }
        let own_producers = own_producers.finish(published_own_producers);
        self.install_replanned_children(id, &published, &matched, &entries, own_producers);
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        ChildEntry::scope(id, total)
    }

    fn install_replanned_children(
        &mut self,
        id: ScopeId,
        published: &[ChildEntry],
        matched: &[bool],
        entries: &[ChildEntry],
        own_producers: CullReach,
    ) {
        if !self.rewrites_tree {
            return;
        }
        for (entry, matched) in published.iter().zip(matched) {
            if let (ChildKey::Scope(child), false) = (entry.key(), *matched) {
                self.tree.detach_scope(child);
            }
        }
        self.tree.set_children(id, entries, own_producers);
    }

    fn record_scope_fresh(&mut self, id: ScopeId) -> ChildEntry {
        debug_assert!(self.rewrites_tree);
        let scope = self.tree.scope_of(id);
        let plan = self.host.plan_scope(scope);
        if !plan.active {
            self.host
                .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Inactive));
            self.tree.set_children(id, &[], CullReach::NOTHING);
            self.host.observe(AssemblyEvent::ScopeEnd(scope));
            return ChildEntry::scope(id, OutputSize::default());
        }
        self.host
            .observe(AssemblyEvent::ScopeBegin(scope, ScopeAction::Recorded));
        let mut entries: SmallVec<[ChildEntry; 16]> = SmallVec::with_capacity(plan.items.len());
        let mut total = OutputSize::default();
        let mut own_producers = OwnProducersReach::default();
        for item in plan.items {
            match item {
                PaintOrderItem::Producer(producer) => {
                    for kind in producer_kinds(producer) {
                        let recorded =
                            self.assemble_producer(scope.owner, kind, None, CullReach::NOTHING, &mut own_producers);
                        total.add(recorded.output());
                        entries.push(recorded);
                    }
                }
                PaintOrderItem::Scope(child_scope) => {
                    let child = self.tree.allocate_scope(child_scope, id);
                    let recorded = self.record_scope_fresh(child);
                    total.add(recorded.output());
                    entries.push(recorded);
                }
            }
        }
        self.tree.set_children(id, &entries, own_producers.recorded);
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        ChildEntry::scope(id, total)
    }

    fn record_scope_outside_tree(&mut self, scope: PaintScope) -> OutputSize {
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
                        let assembled = self.assemble_producer(
                            scope.owner,
                            kind,
                            None,
                            CullReach::NOTHING,
                            &mut OwnProducersReach::default(),
                        );
                        total.add(assembled.output());
                    }
                }
                PaintOrderItem::Scope(child_scope) => total.add(self.record_scope_outside_tree(child_scope)),
            }
        }
        self.host.observe(AssemblyEvent::ScopeEnd(scope));
        total
    }

    /// Assembles one producer of `owner`, given its published output while that is `clean`: copies the output while
    /// the producer is left out as it was, leaves the producer out where the interest rect of its scroll node misses
    /// it, and otherwise copies clean output or records. Output the published recording left out is never copied as
    /// it is once the producer is not left out any more. `published_own_producers` is the published reach of the
    /// scope's own producers.
    fn assemble_producer(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        clean: Option<PublishedProducer>,
        published_own_producers: CullReach,
        own_producers: &mut OwnProducersReach,
    ) -> ChildEntry {
        let Some(PublishedProducer { entry, cursor }) = clean else {
            let reach = self.cull_reach(owner, kind);
            if self.interest_misses(reach) {
                return own_producers.note_recorded(self.leave_out_without_published_output(owner, kind, reach));
            }
            return own_producers.note_recorded(self.record_producer(owner, kind));
        };
        let output = entry.output();
        if !self.may_cull_differently(published_own_producers, output) {
            self.copy(cursor, output);
            self.observe_copied_producer(owner, kind, output);
            return own_producers.note_copied(entry);
        }
        // Without commands there is nothing to leave out, wherever the producer would draw.
        if !output.culled && output.bytes == 0 {
            self.copy(cursor, output);
            self.observe_copied_producer(owner, kind, output);
            return own_producers.note_recorded((entry, CullReach::NOTHING));
        }
        let reach = self.cull_reach(owner, kind);
        if self.interest_misses(reach) {
            let culled = if output.culled {
                output
            } else {
                let published_bytes = cursor.bytes..cursor.bytes + output.bytes;
                let routes_wheels = self.host.published_output_routes_wheels(owner, kind, published_bytes);
                culled_output(output.hits, output.blocking_wheel_event_regions, routes_wheels)
            };
            if culled.hits != 0 || culled.blocking_wheel_event_regions != 0 {
                self.copy(cursor, culled);
            }
            self.host.observe(AssemblyEvent::ProducerCulled(owner, kind));
            return own_producers.note_recorded((ChildEntry::producer(kind, culled), reach));
        }
        if output.culled {
            return own_producers.note_recorded(self.record_producer(owner, kind));
        }
        self.copy(cursor, output);
        self.observe_copied_producer(owner, kind, output);
        let copied = OutputSize {
            has_cullable_output: !reach.is_nothing(),
            ..output
        };
        own_producers.note_recorded((ChildEntry::producer(kind, copied), reach))
    }

    fn observe_copied_producer(&mut self, owner: NodeSlotId, kind: ProducerKind, output: OutputSize) {
        self.host.observe(if output.culled {
            AssemblyEvent::ProducerCulled(owner, kind)
        } else {
            AssemblyEvent::ProducerCopied(owner, kind)
        });
    }

    fn cull_reach(&mut self, owner: NodeSlotId, kind: ProducerKind) -> CullReach {
        match self.culling {
            AssemblyCulling::LeavesNothingOut => CullReach::NOTHING,
            AssemblyCulling::ByInterest { .. } => self.host.producer_cull_reach(owner, kind),
        }
    }

    fn interest_misses(&self, reach: CullReach) -> bool {
        matches!(self.culling, AssemblyCulling::ByInterest { .. })
            && !reach.is_nothing()
            && self.host.interest_misses(reach)
    }

    // Leaves out a producer that has no published output to take its hit-test items and blocking wheel regions from.
    fn leave_out_without_published_output(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        reach: CullReach,
    ) -> (ChildEntry, CullReach) {
        self.flush_pending_copy();
        let before = self.host.output_position();
        let routes_wheels = self.host.record_producer_leaving_out_commands(owner, kind);
        let after = self.host.output_position();
        debug_assert_eq!(before.bytes, after.bytes, "a producer left out recorded commands");
        let culled = culled_output(
            after.hits - before.hits,
            after.blocking_wheel_event_regions - before.blocking_wheel_event_regions,
            routes_wheels,
        );
        (ChildEntry::producer(kind, culled), reach)
    }

    // Returns the producer's entry with the reach of its output that a recording around the scroll
    // positions may leave out.
    fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> (ChildEntry, CullReach) {
        self.flush_pending_copy();
        let before = self.host.output_position();
        let outcome = self.host.record_producer(owner, kind);
        let after = self.host.output_position();
        let bytes = after.bytes - before.bytes;
        // Without commands there is nothing to leave out, wherever the producer would draw.
        let cull_reach = if bytes == 0 {
            CullReach::NOTHING
        } else {
            outcome.cull_reach
        };
        let output = OutputSize {
            bytes,
            hits: after.hits - before.hits,
            blocking_wheel_event_regions: after.blocking_wheel_event_regions - before.blocking_wheel_event_regions,
            live: outcome.live,
            reads_window_focus: outcome.reads_window_focus,
            culled: false,
            has_cullable_output: !cull_reach.is_nothing(),
            culled_wheel_routing: false,
        };
        (ChildEntry::producer(kind, output), cull_reach)
    }

    /// Panics unless the tree lists every scope and producer the plans list, its root counts the hit-test items the
    /// frame holds, and every producer is left out exactly where the interest rect of its scroll node misses it, placed
    /// afresh rather than through the scope summaries. A producer recorded without commands has nothing to leave out.
    fn audit_culling(&mut self, root_output: OutputSize) {
        assert_eq!(
            root_output.hits,
            self.host.output_position().hits,
            "the frame holds other hit-test items than its tree counts"
        );
        if self.culling == AssemblyCulling::LeavesNothingOut {
            assert!(
                !root_output.culled,
                "a frame that leaves nothing out left producers out"
            );
            return;
        }
        #[derive(Debug, PartialEq)]
        enum Listed {
            Producer(ProducerKind),
            Scope(PaintScope),
        }
        let mut stack = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            let scope = self.tree.scope_of(id);
            let plan = self.host.plan_scope(scope);
            // A stacking context that does not paint lists nothing.
            let planned_items = if plan.active { plan.items } else { SmallVec::new() };
            let planned: Vec<Listed> = planned_items
                .into_iter()
                .flat_map(|item| -> SmallVec<[Listed; 5]> {
                    match item {
                        PaintOrderItem::Producer(producer) => {
                            producer_kinds(producer).into_iter().map(Listed::Producer).collect()
                        }
                        PaintOrderItem::Scope(child_scope) => smallvec::smallvec![Listed::Scope(child_scope)],
                    }
                })
                .collect();
            let children = self.tree.children(id).to_vec();
            let listed: Vec<Listed> = children
                .iter()
                .map(|entry| match entry.key() {
                    ChildKey::Scope(child) => Listed::Scope(self.tree.scope_of(child)),
                    ChildKey::Producer(kind) => Listed::Producer(kind),
                })
                .collect();
            assert_eq!(
                listed, planned,
                "the tree lists other children of {scope:?} than its plan"
            );
            for entry in children {
                let kind = match entry.key() {
                    ChildKey::Scope(child) => {
                        stack.push(child);
                        continue;
                    }
                    ChildKey::Producer(kind) => kind,
                };
                let reach = self.host.producer_cull_reach(scope.owner, kind);
                let missed = !reach.is_nothing() && self.host.interest_misses(reach);
                let output = entry.output();
                assert!(
                    output.culled == missed || (!output.culled && output.bytes == 0),
                    "{kind:?} of {:?} is {} while the interest rect of {reach:?} {} it",
                    scope.owner,
                    if output.culled { "left out" } else { "recorded" },
                    if missed { "misses" } else { "reaches" }
                );
            }
        }
    }

    fn copy(&mut self, cursor: SourceCursor, output: OutputSize) {
        debug_assert!(
            self.source_prologue_bytes.is_some(),
            "clean output is copied from a published recording"
        );
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

// What a producer left out contributes: no bytes, but the hit-test items and blocking wheel regions it records still
// count, since hit testing and the tree stay whole-document.
fn culled_output(hits: u32, blocking_wheel_event_regions: u32, routes_wheels: bool) -> OutputSize {
    OutputSize {
        bytes: 0,
        hits,
        blocking_wheel_event_regions,
        live: false,
        reads_window_focus: false,
        culled: true,
        has_cullable_output: true,
        culled_wheel_routing: routes_wheels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::css_pixels::CssPixelRect;
    use crate::painting::border_radii::BorderRadii;
    use crate::painting::display_list::builder::RecordedDisplayList;
    use crate::painting::display_list::commands::{ContextRef, SpatialNodeIndex};
    use crate::painting::display_list::recorder::DisplayListRecorder;
    use crate::painting::force_dark::ForceDarkRole;
    use crate::painting::hit_test::{HitTestItem, HitTestItemKind};
    use crate::painting::paint_order_plan::StackingContextPaintPhase;
    use crate::painting::record::AssembledHitTestItems;
    use crate::painting::record::order_tree::ScopeCullSummary;
    use libgfx_rust::{Color, IntRect};
    use std::collections::{HashMap, HashSet};

    fn row(index: u32) -> NodeSlotId {
        NodeSlotId::new(index, 1)
    }

    fn context(owner: NodeSlotId) -> PaintScope {
        PaintScope::stacking_context(owner)
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

    fn svg() -> PaintOrderItem {
        PaintOrderItem::Producer(PaintProducer::SvgBoxForeground)
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
        blocking: u32,
        cull_reach: CullReach,
        routes_wheels: bool,
    }

    fn content(rects: u32, hits: u32) -> Content {
        Content {
            rects,
            hits,
            version: 1,
            live: false,
            blocking: 0,
            cull_reach: CullReach::NOTHING,
            routes_wheels: false,
        }
    }

    const ROOT_SCROLL_NODE: SpatialNodeIndex = SpatialNodeIndex(1);
    const NESTED_SCROLL_NODE: SpatialNodeIndex = SpatialNodeIndex(2);

    struct Frame {
        display_list: RecordedDisplayList,
        hit_test_items: std::sync::Arc<Vec<HitTestItem>>,
        prologue_bytes: u32,
        root_output: OutputSize,
    }

    struct Output {
        recorder: DisplayListRecorder,
        hit_test_items: AssembledHitTestItems,
        blocking_wheel_event_region_count: u32,
    }

    fn new_output(published_hit_test_items: Option<std::sync::Arc<Vec<HitTestItem>>>) -> Output {
        Output {
            recorder: DisplayListRecorder::new(None),
            hit_test_items: AssembledHitTestItems::new(published_hit_test_items, 0),
            blocking_wheel_event_region_count: 0,
        }
    }

    struct TestHost {
        plans: HashMap<PaintScope, ScopePlan>,
        contents: HashMap<(NodeSlotId, ProducerKind), Content>,
        damage: HashMap<NodeSlotId, PaintDamage>,
        moved_subtrees: HashSet<NodeSlotId>,
        descendant_readers: HashSet<(NodeSlotId, ProducerKind)>,
        recorded: Vec<(NodeSlotId, ProducerKind)>,
        left_out: Vec<(NodeSlotId, ProducerKind)>,
        events: Vec<AssemblyEvent>,
        output: Output,
        source: Option<Frame>,
        culling: AssemblyCulling,
        // The interest rect of each scroll node that has one.
        interest_rects: HashMap<SpatialNodeIndex, IntRect>,
        // The interest rects the published recording left out by.
        published_interest_rects: HashMap<SpatialNodeIndex, IntRect>,
    }

    impl Default for TestHost {
        fn default() -> Self {
            Self {
                plans: HashMap::new(),
                contents: HashMap::new(),
                damage: HashMap::new(),
                moved_subtrees: HashSet::new(),
                descendant_readers: HashSet::new(),
                recorded: Vec::new(),
                left_out: Vec::new(),
                events: Vec::new(),
                output: new_output(None),
                source: None,
                culling: AssemblyCulling::LeavesNothingOut,
                interest_rects: HashMap::new(),
                published_interest_rects: HashMap::new(),
            }
        }
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

        fn clear_damage(&mut self) {
            self.damage.clear();
            self.moved_subtrees.clear();
            self.recorded.clear();
            self.left_out.clear();
            self.events.clear();
        }

        fn record_hits_and_blocking_regions(&mut self, owner: NodeSlotId, content: Content) {
            let output = &mut self.output;
            for index in 0..content.hits {
                output.hit_test_items.append(HitTestItem {
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
        }

        fn recorded_kinds_of(&self, owner: NodeSlotId) -> Vec<ProducerKind> {
            self.recorded
                .iter()
                .filter(|(recorded_owner, _)| *recorded_owner == owner)
                .map(|(_, kind)| *kind)
                .collect()
        }
    }

    impl AssemblyHost for TestHost {
        fn plan_scope(&mut self, scope: PaintScope) -> ScopePlan {
            self.plans.get(&scope).cloned().unwrap_or_default()
        }

        fn record_producer(&mut self, owner: NodeSlotId, kind: ProducerKind) -> ProducerOutcome {
            let Some(content) = self.contents.get(&(owner, kind)).copied() else {
                return ProducerOutcome::default();
            };
            self.recorded.push((owner, kind));
            for index in 0..content.rects {
                self.output.recorder.fill_rect(
                    IntRect {
                        x: owner.slot_index() as i32,
                        y: kind.code() as i32 * 100 + index as i32,
                        width: 10,
                        height: i32::from(content.version),
                    },
                    Color::from_rgb(content.version, kind.code() as u8, 7),
                    ForceDarkRole::Background,
                );
            }
            self.record_hits_and_blocking_regions(owner, content);
            ProducerOutcome {
                live: content.live,
                cull_reach: content.cull_reach,
                ..Default::default()
            }
        }

        fn producer_cull_reach(&mut self, owner: NodeSlotId, kind: ProducerKind) -> CullReach {
            self.contents
                .get(&(owner, kind))
                .map_or(CullReach::NOTHING, |content| content.cull_reach)
        }

        fn interest_misses(&self, reach: CullReach) -> bool {
            reach
                .bounds_in_only_scroll_node()
                .and_then(|(scroll_node, bounds)| {
                    let interest_rect = self.interest_rects.get(&scroll_node)?;
                    Some(interest_rect.intersected(bounds).is_empty())
                })
                .unwrap_or(false)
        }

        fn published_and_current_interest_rects_of(&self, scroll_node: SpatialNodeIndex) -> Option<(IntRect, IntRect)> {
            Some((
                *self.published_interest_rects.get(&scroll_node)?,
                *self.interest_rects.get(&scroll_node)?,
            ))
        }

        fn record_producer_leaving_out_commands(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
            let Some(content) = self.contents.get(&(owner, kind)).copied() else {
                return false;
            };
            self.left_out.push((owner, kind));
            self.record_hits_and_blocking_regions(owner, content);
            content.routes_wheels && content.rects != 0
        }

        fn published_output_routes_wheels(&mut self, owner: NodeSlotId, kind: ProducerKind, bytes: Range<u32>) -> bool {
            !bytes.is_empty() && self.contents[&(owner, kind)].routes_wheels
        }

        fn output_position(&self) -> OutputPosition {
            OutputPosition {
                bytes: self.output.recorder.byte_size() as u32,
                hits: self.output.hit_test_items.len() as u32,
                blocking_wheel_event_regions: self.output.blocking_wheel_event_region_count,
            }
        }

        fn copy_published(&mut self, bytes: Range<u32>, hits: Range<u32>, blocking_wheel_event_regions: u32) {
            let source = self.source.as_ref().expect("a published recording to copy from");
            self.output.recorder.append_cached_commands(&source.display_list, bytes);
            self.output
                .hit_test_items
                .append_published(hits.start as usize..hits.end as usize);
            self.output.blocking_wheel_event_region_count += blocking_wheel_event_regions;
        }

        fn damaged_rows(&mut self) -> Vec<NodeSlotId> {
            let mut rows: Vec<NodeSlotId> = self.damage.keys().copied().collect();
            rows.extend(self.moved_subtrees.iter().copied());
            rows.sort_unstable_by_key(|row| row.slot_index());
            rows.dedup();
            rows
        }

        fn effective_damage(&mut self, row: NodeSlotId) -> PaintDamage {
            let mut damage = self.damage.get(&row).copied().unwrap_or_default();
            if self.moved_subtrees.contains(&row) {
                damage |= PaintDamage::ALL_PRODUCERS | PaintDamage::MOVED;
            }
            damage
        }

        fn producer_reads_descendants(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
            self.descendant_readers.contains(&(owner, kind))
        }

        fn observe(&mut self, event: AssemblyEvent) {
            self.events.push(event);
        }
    }

    // Records the canvas prologue and assembles the viewport context from the host's source
    // frame.
    fn record_frame(host: &mut TestHost, tree: &mut PaintOrderTree) -> Frame {
        record_frame_rewriting_tree(host, tree, true)
    }

    fn record_frame_rewriting_tree(host: &mut TestHost, tree: &mut PaintOrderTree, rewrites_tree: bool) -> Frame {
        host.output = new_output(host.source.as_ref().map(|frame| frame.hit_test_items.clone()));
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
        let prologue_bytes = host.output.recorder.byte_size() as u32;
        let source_prologue_bytes = host.source.as_ref().map(|frame| frame.prologue_bytes);
        let culling = host.culling;
        let root_output = Assembler::new(host, tree, source_prologue_bytes, rewrites_tree, false, culling)
            .assemble_root(context(row(0)));
        let output = std::mem::replace(&mut host.output, new_output(None));
        Frame {
            display_list: output.recorder.into_builder().finish(),
            hit_test_items: output.hit_test_items.finish().items,
            prologue_bytes,
            root_output,
        }
    }

    // Assembles the next frame from the source frame and keeps it as the source in turn.
    fn record_next_frame(host: &mut TestHost, tree: &mut PaintOrderTree) -> Frame {
        let frame = record_frame(host, tree);
        let previous = host.source.replace(Frame {
            display_list: frame.display_list.clone(),
            hit_test_items: frame.hit_test_items.clone(),
            prologue_bytes: frame.prologue_bytes,
            root_output: frame.root_output,
        });
        assert!(previous.is_some(), "a next frame copies from a first frame");
        frame
    }

    // Returns the tree of the recording from scratch.
    fn assert_matches_from_scratch(host: &mut TestHost, assembled: &Frame) -> PaintOrderTree {
        let recorded = std::mem::take(&mut host.recorded);
        let left_out = std::mem::take(&mut host.left_out);
        let events = std::mem::take(&mut host.events);
        let source = host.source.take();
        let mut fresh_tree = PaintOrderTree::default();
        let fresh = record_frame(host, &mut fresh_tree);
        host.source = source;
        host.recorded = recorded;
        host.left_out = left_out;
        host.events = events;
        assert_eq!(
            assembled.display_list.bytes, fresh.display_list.bytes,
            "command bytes differ"
        );
        assert_eq!(assembled.display_list.command_runs, fresh.display_list.command_runs);
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
        // A producer without commands may be left out or recorded empty, as the frame assembled it.
        let comparable = |output: OutputSize| match host.culling {
            AssemblyCulling::LeavesNothingOut => output,
            AssemblyCulling::ByInterest { .. } => OutputSize {
                culled: false,
                has_cullable_output: false,
                ..output
            },
        };
        assert_eq!(comparable(assembled.root_output), comparable(fresh.root_output));
        fresh_tree
    }

    // Every scope of an assembled tree reaches at least as far as the same scope recorded from scratch.
    fn assert_cull_summaries_cover(assembled_tree: &PaintOrderTree, fresh_tree: &PaintOrderTree) {
        let assembled = assembled_tree.structure();
        let fresh = fresh_tree.structure();
        assert_eq!(assembled.len(), fresh.len());
        for ((_, scope, _, summary), (_, fresh_scope, _, fresh_summary)) in assembled.iter().zip(&fresh) {
            assert_eq!(scope, fresh_scope);
            assert!(
                summary.own_producers.covers(fresh_summary.own_producers)
                    && summary.subtree.covers(fresh_summary.subtree),
                "{scope:?} keeps {summary:?}, which does not cover {fresh_summary:?}"
            );
        }
    }

    // Card `card` draws in a band of its own in the root scroll node.
    fn card_reach(card: u32, kind: ProducerKind, offset: i32) -> CullReach {
        let band = IntRect::new(0, card as i32 * 1000 + offset, 800, 100);
        let bounds = match kind {
            ProducerKind::DrawForeground => band.shrunken(10, 10, 10, 10),
            _ => band,
        };
        CullReach::in_scroll_node(ROOT_SCROLL_NODE, bounds)
    }

    fn producer_entry(tree: &PaintOrderTree, scope: ScopeId, kind: ProducerKind) -> ChildEntry {
        *tree
            .children(scope)
            .iter()
            .find(|entry| entry.key() == ChildKey::Producer(kind))
            .unwrap()
    }

    fn card_scope_id(tree: &PaintOrderTree, card: u32) -> ScopeId {
        let parent = tree.published_scope(foreground(row(0)), tree.root()).unwrap();
        tree.published_scope(foreground(row(card)), parent).unwrap()
    }

    // A viewport context (row 0) with a background and a foreground scope listing three
    // card scopes (rows 1..=3) that each draw a background and a foreground with hit items.
    fn cards_host() -> TestHost {
        let mut host = TestHost::default();
        host.plan(
            context(row(0)),
            true,
            &[phase(PaintPhase::Background), child(foreground(row(0)))],
        );
        host.produce(row(0), ProducerKind::DrawBackground, content(1, 0));
        host.plan(
            foreground(row(0)),
            true,
            &[
                child(foreground(row(1))),
                child(foreground(row(2))),
                child(foreground(row(3))),
            ],
        );
        for card in 1..=3 {
            host.plan(
                foreground(row(card)),
                true,
                &[phase(PaintPhase::Background), phase(PaintPhase::Foreground)],
            );
            host.produce(
                row(card),
                ProducerKind::DrawBackground,
                Content {
                    cull_reach: card_reach(card, ProducerKind::DrawBackground, 0),
                    ..content(1, 0)
                },
            );
            host.produce(row(card), ProducerKind::HitForeground, content(0, 2));
            host.produce(
                row(card),
                ProducerKind::DrawForeground,
                Content {
                    cull_reach: card_reach(card, ProducerKind::DrawForeground, 0),
                    ..content(card, 0)
                },
            );
        }
        host
    }

    #[test]
    fn a_quiet_frame_is_unchanged_and_a_fresh_frame_records_everything() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        assert!(!frame_is_unchanged(
            &tree,
            false,
            false,
            AssemblyCulling::LeavesNothingOut
        ));

        let frame = record_frame(&mut host, &mut tree);

        assert_eq!(host.recorded.len(), 1 + 3 * 3);
        assert_eq!(frame.root_output.hits, 6);
        assert_eq!(frame.hit_test_items.len(), 6);
        assert_eq!(
            frame.root_output.bytes as usize,
            frame.display_list.bytes.len() - frame.prologue_bytes as usize
        );
        assert!(frame_is_unchanged(
            &tree,
            false,
            false,
            AssemblyCulling::LeavesNothingOut
        ));
        assert!(!frame_is_unchanged(
            &tree,
            true,
            false,
            AssemblyCulling::LeavesNothingOut
        ));
    }

    #[test]
    fn one_damaged_producer_records_alone_and_the_rest_is_copied_in_two_runs() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.produce(row(2), ProducerKind::DrawForeground, content(2, 0));
        host.contents
            .get_mut(&(row(2), ProducerKind::DrawForeground))
            .unwrap()
            .version = 2;
        host.push(row(2), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawForeground)]);
        let copied_scopes: Vec<AssemblyEvent> = host
            .events
            .iter()
            .copied()
            .filter(|event| matches!(event, AssemblyEvent::ScopeCopied(_)))
            .collect();
        assert_eq!(copied_scopes.len(), 2);
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn a_frame_that_copies_every_hit_test_producer_shares_the_published_items() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        let published_items = first.hit_test_items.clone();
        host.source = Some(first);
        host.clear_damage();

        host.contents
            .get_mut(&(row(2), ProducerKind::DrawForeground))
            .unwrap()
            .version = 2;
        host.push(row(2), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawForeground)]);
        assert!(std::sync::Arc::ptr_eq(&second.hit_test_items, &published_items));
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn a_recorded_hit_test_producer_assembles_items_of_its_own() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        let published_items = first.hit_test_items.clone();
        host.source = Some(first);
        host.clear_damage();

        host.contents
            .get_mut(&(row(2), ProducerKind::HitForeground))
            .unwrap()
            .version = 2;
        host.push(row(2), PaintDamage::HIT_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(2), ProducerKind::HitForeground)]);
        assert!(!std::sync::Arc::ptr_eq(&second.hit_test_items, &published_items));
        assert_eq!(second.hit_test_items.len(), published_items.len());
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn a_growing_child_shifts_its_later_siblings_without_recording_them() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.produce(row(1), ProducerKind::DrawForeground, content(5, 0));
        host.produce(row(1), ProducerKind::HitForeground, content(0, 4));
        host.push(row(1), PaintDamage::DRAW_FOREGROUND | PaintDamage::HIT_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let grown = record_next_frame(&mut host, &mut tree);
        assert_eq!(
            host.recorded_kinds_of(row(1)),
            vec![ProducerKind::HitForeground, ProducerKind::DrawForeground]
        );
        assert_eq!(host.recorded.len(), 2);
        assert_eq!(grown.hit_test_items.len(), 8);
        assert_matches_from_scratch(&mut host, &grown);
        host.clear_damage();

        host.produce(row(1), ProducerKind::DrawForeground, content(0, 0));
        host.push(row(1), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let shrunk = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.recorded, vec![(row(1), ProducerKind::DrawForeground)]);
        assert_matches_from_scratch(&mut host, &shrunk);
    }

    #[test]
    fn a_replanned_parent_copies_kept_children_and_retires_the_removed_one() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        let removed = tree
            .published_scope(
                foreground(row(2)),
                tree.published_scope(foreground(row(0)), tree.root()).unwrap(),
            )
            .unwrap();
        host.clear_damage();

        host.plan(
            foreground(row(0)),
            true,
            &[child(foreground(row(3))), child(foreground(row(1)))],
        );
        host.push(row(0), PaintDamage::ORDER | PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert!(host.recorded.is_empty());
        assert_matches_from_scratch(&mut host, &second);
        assert_eq!(second.hit_test_items.len(), 4);
        let parent = tree.published_scope(foreground(row(0)), tree.root()).unwrap();
        assert_eq!(tree.children(parent).len(), 2);
        assert_eq!(tree.published_scope(foreground(row(2)), parent), None);
        assert_eq!(tree.scope_count(), 4);
        assert_ne!(tree.published_scope(foreground(row(3)), parent), Some(removed));
    }

    #[test]
    fn a_new_child_is_recorded_under_the_replanned_parent() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.plan(
            foreground(row(0)),
            true,
            &[
                child(foreground(row(1))),
                child(foreground(row(4))),
                child(foreground(row(2))),
                child(foreground(row(3))),
            ],
        );
        host.plan(
            foreground(row(4)),
            true,
            &[phase(PaintPhase::Background), phase(PaintPhase::Foreground)],
        );
        host.produce(row(4), ProducerKind::DrawBackground, content(2, 0));
        host.produce(row(4), ProducerKind::HitForeground, content(0, 1));
        host.push(row(4), PaintDamage::ORDER | PaintDamage::ALL_PRODUCERS);
        host.push(row(0), PaintDamage::ORDER | PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(
            host.recorded,
            vec![
                (row(4), ProducerKind::DrawBackground),
                (row(4), ProducerKind::HitForeground)
            ]
        );
        assert_matches_from_scratch(&mut host, &second);
        assert_eq!(tree.scope_count(), 6);
    }

    #[test]
    fn a_frame_that_publishes_nothing_copies_clean_output_and_leaves_the_tree_as_published() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();
        let published_structure = tree.structure();
        let published_root_entry = tree.root_entry();

        host.produce(row(2), ProducerKind::DrawForeground, content(3, 0));
        host.push(row(2), PaintDamage::DRAW_FOREGROUND);
        let unpublished = record_frame_rewriting_tree(&mut host, &mut tree, false);

        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawForeground)]);
        assert_matches_from_scratch(&mut host, &unpublished);
        assert_eq!(tree.structure(), published_structure);
        assert_eq!(tree.root_entry(), published_root_entry);

        // The damage it read is still pending for the frame that publishes next.
        host.recorded.clear();
        let published = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawForeground)]);
        assert_matches_from_scratch(&mut host, &published);
        assert_eq!(published.display_list.bytes, unpublished.display_list.bytes);
    }

    #[test]
    fn a_frame_that_publishes_nothing_records_a_new_child_outside_the_tree() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();
        let published_structure = tree.structure();
        let published_scope_count = tree.scope_count();

        // Card 4 takes card 2's place.
        host.plan(
            foreground(row(0)),
            true,
            &[
                child(foreground(row(1))),
                child(foreground(row(4))),
                child(foreground(row(3))),
            ],
        );
        host.plan(
            foreground(row(4)),
            true,
            &[phase(PaintPhase::Background), phase(PaintPhase::Foreground)],
        );
        host.produce(row(4), ProducerKind::DrawBackground, content(2, 0));
        host.produce(row(4), ProducerKind::HitForeground, content(0, 1));
        host.push(row(4), PaintDamage::ORDER | PaintDamage::ALL_PRODUCERS);
        host.push(row(0), PaintDamage::ORDER | PaintDamage::DESCENDANT_READERS);
        let unpublished = record_frame_rewriting_tree(&mut host, &mut tree, false);

        assert_eq!(
            host.recorded,
            vec![
                (row(4), ProducerKind::DrawBackground),
                (row(4), ProducerKind::HitForeground)
            ]
        );
        assert_matches_from_scratch(&mut host, &unpublished);
        assert_eq!(tree.structure(), published_structure);
        assert_eq!(tree.scope_count(), published_scope_count);

        let published = record_next_frame(&mut host, &mut tree);
        assert_matches_from_scratch(&mut host, &published);
    }

    #[test]
    fn a_first_frame_that_publishes_nothing_leaves_the_tree_empty() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();

        let frame = record_frame_rewriting_tree(&mut host, &mut tree, false);

        assert!(tree.root().is_none());
        assert_eq!(tree.scope_count(), 0);
        assert_matches_from_scratch(&mut host, &frame);
    }

    #[test]
    fn nested_damage_walks_only_the_damaged_path() {
        let mut host = cards_host();
        // Card 3 gets a nested scope of its own.
        host.plan(
            foreground(row(3)),
            true,
            &[
                phase(PaintPhase::Background),
                child(foreground(row(5))),
                phase(PaintPhase::Foreground),
            ],
        );
        host.plan(foreground(row(5)), true, &[phase(PaintPhase::Foreground)]);
        host.produce(row(5), ProducerKind::DrawForeground, content(3, 0));
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.contents
            .get_mut(&(row(5), ProducerKind::DrawForeground))
            .unwrap()
            .version = 9;
        host.push(row(5), PaintDamage::DRAW_FOREGROUND);
        for ancestor in [row(3), row(0)] {
            host.push(ancestor, PaintDamage::DESCENDANT_READERS);
        }
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(5), ProducerKind::DrawForeground)]);
        let assembled: Vec<PaintScope> = host
            .events
            .iter()
            .filter_map(|event| match event {
                AssemblyEvent::ScopeBegin(scope, ScopeAction::Assembled) => Some(*scope),
                _ => None,
            })
            .collect();
        assert_eq!(
            assembled.len(),
            4,
            "viewport, its foreground, card 3 and its nested scope"
        );
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn a_moved_subtree_records_every_producer_inside_it() {
        let mut host = cards_host();
        host.plan(
            foreground(row(3)),
            true,
            &[phase(PaintPhase::Background), child(foreground(row(5)))],
        );
        host.plan(foreground(row(5)), true, &[phase(PaintPhase::Foreground)]);
        host.produce(row(5), ProducerKind::DrawForeground, content(2, 0));
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.moved_subtrees.extend([row(3), row(5)]);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(
            host.recorded,
            vec![
                (row(3), ProducerKind::DrawBackground),
                (row(5), ProducerKind::DrawForeground)
            ]
        );
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn descendant_readers_record_when_something_below_them_changed() {
        let mut host = cards_host();
        host.plan(foreground(row(3)), true, &[svg(), child(foreground(row(5)))]);
        host.produce(row(3), ProducerKind::Svg, content(1, 1));
        host.descendant_readers.insert((row(3), ProducerKind::Svg));
        host.plan(foreground(row(5)), true, &[phase(PaintPhase::Foreground)]);
        host.produce(row(5), ProducerKind::DrawForeground, content(1, 0));
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        host.push(row(5), PaintDamage::DRAW_FOREGROUND);
        host.push(row(3), PaintDamage::DESCENDANT_READERS);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(
            host.recorded,
            vec![(row(3), ProducerKind::Svg), (row(5), ProducerKind::DrawForeground)]
        );
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn live_producers_record_every_frame_and_keep_the_frame_changing() {
        let mut host = cards_host();
        host.produce(
            row(2),
            ProducerKind::DrawBackground,
            Content {
                live: true,
                ..content(1, 0)
            },
        );
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();
        assert!(!frame_is_unchanged(
            &tree,
            false,
            false,
            AssemblyCulling::LeavesNothingOut
        ));

        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawBackground)]);
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn an_inactive_context_records_nothing_and_retires_its_children() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();

        let mut hidden = host.plans[&context(row(0))].clone();
        hidden.active = false;
        host.plans.insert(context(row(0)), hidden);
        host.push(row(0), PaintDamage::ELIGIBILITY);
        let second = record_next_frame(&mut host, &mut tree);

        assert!(host.recorded.is_empty());
        assert_eq!(second.root_output, OutputSize::default());
        assert_eq!(second.display_list.bytes.len(), second.prologue_bytes as usize);
        assert_eq!(tree.scope_count(), 1);
        assert_matches_from_scratch(&mut host, &second);
    }

    #[test]
    fn blocking_wheel_regions_are_counted_through_copies() {
        let mut host = cards_host();
        host.produce(
            row(1),
            ProducerKind::HitForeground,
            Content {
                blocking: 1,
                ..content(0, 2)
            },
        );
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        assert_eq!(first.root_output.blocking_wheel_event_regions, 1);
        assert_eq!(tree.root_entry().output().blocking_wheel_event_regions, 1);
        host.source = Some(first);
        host.clear_damage();

        host.push(row(3), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);

        assert_eq!(host.recorded, vec![(row(3), ProducerKind::DrawForeground)]);
        assert_eq!(second.root_output.blocking_wheel_event_regions, 1);
        assert_eq!(tree.root_entry().output().blocking_wheel_event_regions, 1);
    }

    fn cards_reach(cards: &[(u32, i32)]) -> CullReach {
        cards.iter().fold(CullReach::NOTHING, |reach, (card, offset)| {
            reach
                .united(card_reach(*card, ProducerKind::DrawBackground, *offset))
                .united(card_reach(*card, ProducerKind::DrawForeground, *offset))
        })
    }

    fn moves_card(host: &mut TestHost, card: u32, kind: ProducerKind, offset: i32) {
        let content = host.contents.get_mut(&(row(card), kind)).unwrap();
        content.version += 1;
        content.cull_reach = card_reach(card, kind, offset);
    }

    #[test]
    fn scopes_summarize_the_reach_of_their_recorded_producers_and_keep_it_while_copied() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);

        let parent = tree.published_scope(foreground(row(0)), tree.root()).unwrap();
        for card in 1..=3 {
            let own = cards_reach(&[(card, 0)]);
            assert_eq!(
                tree.cull_summary(card_scope_id(&tree, card)),
                ScopeCullSummary {
                    own_producers: own,
                    subtree: own,
                }
            );
        }
        let every_card = cards_reach(&[(1, 0), (2, 0), (3, 0)]);
        assert_eq!(
            tree.cull_summary(parent),
            ScopeCullSummary {
                own_producers: CullReach::NOTHING,
                subtree: every_card,
            }
        );
        assert_eq!(tree.cull_summary(tree.root()).subtree, every_card);
        // Only the commands of the cards may be left out, not the viewport background or the hit-test items.
        let cullable_kinds: Vec<ProducerKind> = tree
            .children(card_scope_id(&tree, 1))
            .iter()
            .filter(|entry| entry.has_cullable_output())
            .filter_map(|entry| match entry.key() {
                ChildKey::Producer(kind) => Some(kind),
                ChildKey::Scope(_) => None,
            })
            .collect();
        assert_eq!(
            cullable_kinds,
            vec![ProducerKind::DrawBackground, ProducerKind::DrawForeground]
        );
        assert!(!producer_entry(&tree, tree.root(), ProducerKind::DrawBackground).has_cullable_output());
        assert!(tree.root_entry().has_cullable_output());
        host.source = Some(first);
        host.clear_damage();

        // Card 2's foreground moves down; its background is copied, so card 2 keeps reaching where
        // its foreground was as well, and the copied cards keep their summaries.
        let summary_of_card_1 = tree.cull_summary(card_scope_id(&tree, 1));
        moves_card(&mut host, 2, ProducerKind::DrawForeground, 300);
        host.push(row(2), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let second = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.recorded, vec![(row(2), ProducerKind::DrawForeground)]);
        assert_cull_summaries_cover(&tree, &assert_matches_from_scratch(&mut host, &second));
        let moved_foreground = card_reach(2, ProducerKind::DrawForeground, 300);
        assert_eq!(
            tree.cull_summary(card_scope_id(&tree, 2)).own_producers,
            cards_reach(&[(2, 0)]).united(moved_foreground)
        );
        assert_eq!(tree.cull_summary(card_scope_id(&tree, 1)), summary_of_card_1);
        assert_eq!(tree.cull_summary(parent).subtree, every_card.united(moved_foreground));
        host.clear_damage();

        // Once every producer of card 2 records again, its summary is exact again.
        moves_card(&mut host, 2, ProducerKind::DrawBackground, 300);
        host.push(row(2), PaintDamage::DRAW_BACKGROUND | PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let third = record_next_frame(&mut host, &mut tree);
        assert_cull_summaries_cover(&tree, &assert_matches_from_scratch(&mut host, &third));
        assert_eq!(
            tree.cull_summary(card_scope_id(&tree, 2)).own_producers,
            cards_reach(&[(2, 300)])
        );
        host.clear_damage();

        // A retired card no longer counts toward its parent.
        host.plan(
            foreground(row(0)),
            true,
            &[child(foreground(row(1))), child(foreground(row(2)))],
        );
        host.push(row(0), PaintDamage::ORDER);
        let fourth = record_next_frame(&mut host, &mut tree);
        assert_cull_summaries_cover(&tree, &assert_matches_from_scratch(&mut host, &fourth));
        assert_eq!(tree.cull_summary(parent).subtree, cards_reach(&[(1, 0), (2, 300)]));
    }

    #[test]
    fn a_producer_without_commands_has_nothing_to_leave_out_and_an_unplaced_one_may_land_anywhere() {
        let mut host = cards_host();
        host.contents
            .get_mut(&(row(1), ProducerKind::DrawForeground))
            .unwrap()
            .rects = 0;
        host.contents
            .get_mut(&(row(2), ProducerKind::DrawForeground))
            .unwrap()
            .cull_reach = CullReach::UNPLACED;
        let mut tree = PaintOrderTree::default();
        record_frame(&mut host, &mut tree);

        let card_1 = card_scope_id(&tree, 1);
        assert!(!producer_entry(&tree, card_1, ProducerKind::DrawForeground).has_cullable_output());
        assert_eq!(
            tree.cull_summary(card_1).own_producers,
            card_reach(1, ProducerKind::DrawBackground, 0)
        );
        assert_eq!(
            tree.cull_summary(card_scope_id(&tree, 2)).own_producers,
            CullReach::UNPLACED
        );
        assert_eq!(tree.cull_summary(tree.root()).subtree, CullReach::UNPLACED);
    }

    #[test]
    fn content_in_two_scroll_nodes_keeps_bounds_only_in_the_scopes_inside_one() {
        let mut host = cards_host();
        host.plan(
            foreground(row(3)),
            true,
            &[phase(PaintPhase::Background), child(foreground(row(5)))],
        );
        host.plan(foreground(row(5)), true, &[phase(PaintPhase::Foreground)]);
        let nested_reach = CullReach::in_scroll_node(NESTED_SCROLL_NODE, IntRect::new(0, 3000, 400, 50));
        host.produce(
            row(5),
            ProducerKind::DrawForeground,
            Content {
                cull_reach: nested_reach,
                ..content(1, 0)
            },
        );
        let mut tree = PaintOrderTree::default();
        record_frame(&mut host, &mut tree);

        let card_3 = card_scope_id(&tree, 3);
        let nested = tree.published_scope(foreground(row(5)), card_3).unwrap();
        assert_eq!(tree.cull_summary(nested).subtree, nested_reach);
        let card_3_summary = tree.cull_summary(card_3);
        assert_eq!(
            card_3_summary.own_producers,
            card_reach(3, ProducerKind::DrawBackground, 0)
        );
        assert_eq!(
            card_3_summary.subtree,
            card_3_summary.own_producers.united(nested_reach)
        );
        assert!(card_3_summary.subtree.covers(nested_reach));
        assert!(
            !card_3_summary
                .subtree
                .covers(CullReach::in_scroll_node(SpatialNodeIndex(3), IntRect::new(0, 0, 1, 1)))
        );
    }

    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self, limit: u32) -> u32 {
            self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (self.0 >> 8) % limit
        }
    }

    const CARD_ROWS: [u32; 6] = [1, 2, 3, 4, 5, 6];

    fn nested_row(card: u32) -> Option<u32> {
        matches!(card, 2 | 3 | 5).then_some(10 + card)
    }

    fn parent_row(row_index: u32) -> Option<u32> {
        match row_index {
            0 => None,
            index if index >= 10 => Some(index - 10),
            _ => Some(0),
        }
    }

    fn push_with_hints(host: &mut TestHost, row_index: u32, damage: PaintDamage) {
        host.push(row(row_index), damage);
        let mut ancestor = parent_row(row_index);
        while let Some(index) = ancestor {
            host.push(row(index), PaintDamage::DESCENDANT_READERS);
            ancestor = parent_row(index);
        }
    }

    fn producer_kinds_of(host: &TestHost, row_index: u32) -> Vec<ProducerKind> {
        let mut kinds: Vec<ProducerKind> = host
            .contents
            .keys()
            .filter(|(owner, _)| *owner == row(row_index))
            .map(|(_, kind)| *kind)
            .collect();
        kinds.sort_unstable_by_key(|kind| kind.code());
        kinds
    }

    fn bump_version(host: &mut TestHost, row_index: u32, kind: ProducerKind) {
        let content = host.contents.get_mut(&(row(row_index), kind)).unwrap();
        content.version = content.version.wrapping_add(1).max(1);
    }

    // The cards draw in the root scroll node and the contexts nested in them in a nested one, where
    // their content says; card 3's svg may draw anywhere in the root scroll node, and card 4's
    // foreground records without being placed.
    fn synthetic_cull_reach(row_index: u32, kind: ProducerKind, content: &Content) -> CullReach {
        match (row_index, kind) {
            (3, ProducerKind::Svg) => {
                CullReach::in_scroll_node(ROOT_SCROLL_NODE, IntRect::new(-(1 << 24), -(1 << 24), 1 << 25, 1 << 25))
            }
            (4, ProducerKind::DrawForeground) => CullReach::UNPLACED,
            (index, _) => CullReach::in_scroll_node(
                if index >= 10 {
                    NESTED_SCROLL_NODE
                } else {
                    ROOT_SCROLL_NODE
                },
                IntRect::new(
                    index as i32 * 100,
                    i32::from(content.version) * 10,
                    50,
                    content.rects as i32 + 1,
                ),
            ),
        }
    }

    // A producer's reach changes only with its content, which pushes damage for it.
    fn update_synthetic_cull_reaches(host: &mut TestHost) {
        for ((owner, kind), content) in host.contents.iter_mut() {
            content.cull_reach = synthetic_cull_reach(owner.slot_index(), *kind, content);
        }
    }

    // Six card contexts under the viewport foreground. Cards 2 and 5 nest a context of their
    // own; card 3 paints an svg whose output depends on the foreground scope nested below it.
    fn random_tree_host() -> TestHost {
        let mut host = TestHost::default();
        host.plan(
            context(row(0)),
            true,
            &[phase(PaintPhase::Background), child(foreground(row(0)))],
        );
        host.produce(row(0), ProducerKind::DrawBackground, content(1, 0));
        let cards: Vec<PaintOrderItem> = CARD_ROWS.iter().map(|card| child(context(row(*card)))).collect();
        host.plan(foreground(row(0)), true, &cards);
        for card in CARD_ROWS {
            host.plan(
                context(row(card)),
                true,
                &[phase(PaintPhase::Background), child(foreground(row(card)))],
            );
            host.produce(row(card), ProducerKind::DrawBackground, content(1, 0));
            host.produce(row(card), ProducerKind::HitForeground, content(0, 2));
            host.produce(row(card), ProducerKind::DrawForeground, content(card, 0));
            let mut foreground_items = vec![phase(PaintPhase::Foreground)];
            match nested_row(card) {
                Some(nested) if card == 3 => {
                    foreground_items = vec![svg(), child(foreground(row(nested)))];
                    host.produce(row(card), ProducerKind::Svg, content(1, 1));
                    host.descendant_readers.insert((row(card), ProducerKind::Svg));
                    host.plan(foreground(row(nested)), true, &[phase(PaintPhase::Foreground)]);
                    host.produce(row(nested), ProducerKind::DrawForeground, content(1, 0));
                }
                Some(nested) => {
                    foreground_items.push(child(context(row(nested))));
                    host.plan(
                        context(row(nested)),
                        true,
                        &[phase(PaintPhase::Background), phase(PaintPhase::Foreground)],
                    );
                    host.produce(row(nested), ProducerKind::DrawBackground, content(2, 1));
                    host.produce(row(nested), ProducerKind::DrawForeground, content(1, 0));
                }
                None => {}
            }
            host.plan(foreground(row(card)), true, &foreground_items);
        }
        update_synthetic_cull_reaches(&mut host);
        host
    }

    // The svg of card 3 reads the scope nested below it, so a change there changes the svg's
    // output without a push of its own.
    fn change_content(host: &mut TestHost, random: &mut Lcg, row_index: u32) {
        let kinds = producer_kinds_of(host, row_index);
        let kind = kinds[random.next(kinds.len() as u32) as usize];
        let content = host.contents.get_mut(&(row(row_index), kind)).unwrap();
        match random.next(3) {
            0 => content.version = content.version.wrapping_add(1).max(1),
            1 if matches!(
                kind,
                ProducerKind::HitBackground | ProducerKind::HitForeground | ProducerKind::HitOverlay
            ) =>
            {
                content.hits = random.next(4);
            }
            _ => content.rects = random.next(4),
        }
        push_with_hints(host, row_index, kind.damage());
        if row_index == 13 {
            bump_version(host, 3, ProducerKind::Svg);
        }
    }

    // Which cards the viewport foreground lists, which it no longer lists, and which do not paint.
    struct RandomCards {
        listed: Vec<u32>,
        removed: Vec<u32>,
        inactive: HashSet<u32>,
    }

    impl RandomCards {
        fn new() -> Self {
            Self {
                listed: CARD_ROWS.to_vec(),
                removed: Vec::new(),
                inactive: HashSet::new(),
            }
        }
    }

    // Changes content, order, eligibility and positions at random, and pushes the damage that says so.
    fn damage_at_random(host: &mut TestHost, random: &mut Lcg, cards: &mut RandomCards) {
        let rows_with_content = [1, 2, 3, 4, 5, 6, 12, 13, 15];
        let RandomCards {
            listed,
            removed,
            inactive,
        } = cards;
        for _ in 0..=random.next(3) {
            match random.next(7) {
                0 | 1 => {
                    let row_index = rows_with_content[random.next(rows_with_content.len() as u32) as usize];
                    change_content(host, random, row_index);
                }
                2 => {
                    for index in (1..listed.len()).rev() {
                        listed.swap(index, random.next(index as u32 + 1) as usize);
                    }
                    let items: Vec<PaintOrderItem> = listed.iter().map(|card| child(context(row(*card)))).collect();
                    host.plan(foreground(row(0)), true, &items);
                    host.push(row(0), PaintDamage::ORDER);
                }
                3 if listed.len() > 1 => {
                    let card = listed.remove(random.next(listed.len() as u32) as usize);
                    removed.push(card);
                    let items: Vec<PaintOrderItem> = listed.iter().map(|card| child(context(row(*card)))).collect();
                    host.plan(foreground(row(0)), true, &items);
                    host.push(row(0), PaintDamage::ORDER);
                }
                4 if !removed.is_empty() => {
                    let card = removed.remove(random.next(removed.len() as u32) as usize);
                    listed.insert(random.next(listed.len() as u32 + 1) as usize, card);
                    let items: Vec<PaintOrderItem> = listed.iter().map(|card| child(context(row(*card)))).collect();
                    host.plan(foreground(row(0)), true, &items);
                    push_with_hints(host, card, PaintDamage::ORDER | PaintDamage::ALL_PRODUCERS);
                    if let Some(nested) = nested_row(card) {
                        push_with_hints(host, nested, PaintDamage::ORDER | PaintDamage::ALL_PRODUCERS);
                    }
                    host.push(row(0), PaintDamage::ORDER);
                }
                5 => {
                    let card = listed[random.next(listed.len() as u32) as usize];
                    if !inactive.remove(&card) {
                        inactive.insert(card);
                    }
                    host.plans.get_mut(&context(row(card))).unwrap().active = !inactive.contains(&card);
                    push_with_hints(host, card, PaintDamage::ELIGIBILITY);
                }
                6 => {
                    let card = listed[random.next(listed.len() as u32) as usize];
                    let mut subtree = vec![card];
                    subtree.extend(nested_row(card));
                    for row_index in subtree {
                        for kind in producer_kinds_of(host, row_index) {
                            bump_version(host, row_index, kind);
                        }
                        host.moved_subtrees.insert(row(row_index));
                    }
                    push_with_hints(host, card, PaintDamage::MOVED);
                }
                _ => {}
            }
        }
        update_synthetic_cull_reaches(host);
    }

    #[test]
    fn random_damage_over_a_synthetic_tree_always_matches_from_scratch() {
        let mut random = Lcg(0x2545_f491);
        let mut host = random_tree_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();
        let mut cards = RandomCards::new();

        for step in 0..400 {
            damage_at_random(&mut host, &mut random, &mut cards);

            // Hit tests between two published frames record frames that publish nothing.
            let published_structure = tree.structure();
            for _ in 0..step % 3 {
                let unpublished = record_frame_rewriting_tree(&mut host, &mut tree, false);
                assert_matches_from_scratch(&mut host, &unpublished);
                assert_eq!(tree.structure(), published_structure);
            }

            let frame = record_next_frame(&mut host, &mut tree);
            assert_cull_summaries_cover(&tree, &assert_matches_from_scratch(&mut host, &frame));
            host.clear_damage();
            assert!(tree.scope_count() <= 32, "retired scopes are freed at once");
        }
    }

    // Every scope's summary reaches wherever the producers it lists that may be left out land, and wherever its child
    // scopes reach.
    fn assert_cull_summaries_reach_their_producers(tree: &PaintOrderTree, host: &TestHost) {
        for (id, scope, children, summary) in tree.structure() {
            for entry in children {
                match entry.key() {
                    ChildKey::Producer(kind) if entry.has_cullable_output() => {
                        let reach = host.contents[&(scope.owner, kind)].cull_reach;
                        assert!(
                            summary.own_producers.covers(reach),
                            "{id:?} keeps {summary:?}, which misses {kind:?} at {reach:?}"
                        );
                    }
                    ChildKey::Producer(_) => {}
                    ChildKey::Scope(child) => assert!(summary.subtree.covers(tree.cull_summary(child).subtree)),
                }
            }
            assert!(summary.subtree.covers(summary.own_producers));
        }
    }

    // Sets the interest rects of the next frame and leaves out by them, with the scroll nodes whose rect changed.
    fn aim(host: &mut TestHost, interest_rects: &[(SpatialNodeIndex, IntRect)]) {
        host.published_interest_rects = host.interest_rects.clone();
        let mut changed_scroll_nodes = ScrollNodeSet::default();
        for (scroll_node, rect) in interest_rects {
            if host.interest_rects.insert(*scroll_node, *rect) != Some(*rect) {
                changed_scroll_nodes.insert(*scroll_node);
            }
        }
        let places_every_producer_again = host.culling == AssemblyCulling::LeavesNothingOut;
        host.culling = AssemblyCulling::ByInterest {
            changed_scroll_nodes,
            places_every_producer_again,
        };
    }

    #[test]
    fn random_damage_and_interest_rects_left_out_exactly_what_they_miss() {
        let mut random = Lcg(0x6d2b_79f5);
        let mut host = random_tree_host();
        let mut tree = PaintOrderTree::default();
        let first = record_frame(&mut host, &mut tree);
        host.source = Some(first);
        host.clear_damage();
        let mut cards = RandomCards::new();

        for step in 0..400 {
            damage_at_random(&mut host, &mut random, &mut cards);
            // The synthetic reaches lie between 10 and 2600 px down, so these rects reach some of them, all or none.
            let random_rect =
                |random: &mut Lcg| IntRect::new(0, random.next(300) as i32 * 10, 2000, random.next(120) as i32 * 10);
            match random.next(4) {
                0 => {
                    let rect = random_rect(&mut random);
                    aim(&mut host, &[(ROOT_SCROLL_NODE, rect)]);
                }
                1 => {
                    let rect = random_rect(&mut random);
                    aim(&mut host, &[(NESTED_SCROLL_NODE, rect)]);
                }
                2 => aim(&mut host, &[]),
                _ => {
                    host.culling = AssemblyCulling::LeavesNothingOut;
                    host.interest_rects.clear();
                }
            }

            let published_structure = tree.structure();
            for _ in 0..step % 3 {
                let unpublished = record_frame_rewriting_tree(&mut host, &mut tree, false);
                assert_matches_from_scratch(&mut host, &unpublished);
                assert_eq!(tree.structure(), published_structure);
            }

            let frame = record_next_frame(&mut host, &mut tree);
            assert_matches_from_scratch(&mut host, &frame);
            assert_cull_summaries_reach_their_producers(&tree, &host);
            host.clear_damage();
        }
    }

    // Cards 2 and 3 draw where an interest rect around card 1 misses them, and card 3's background routes wheels over
    // a blocking region.
    fn cards_host_with_wheel_routing_in_card_3() -> TestHost {
        let mut host = cards_host();
        let background = host.contents.get_mut(&(row(3), ProducerKind::DrawBackground)).unwrap();
        background.blocking = 1;
        background.routes_wheels = true;
        host
    }

    fn culled_kinds(host: &TestHost) -> Vec<(NodeSlotId, ProducerKind)> {
        host.events
            .iter()
            .filter_map(|event| match event {
                AssemblyEvent::ProducerCulled(owner, kind) => Some((*owner, *kind)),
                _ => None,
            })
            .collect()
    }

    fn draws_of(cards: &[u32]) -> Vec<(NodeSlotId, ProducerKind)> {
        cards
            .iter()
            .flat_map(|card| {
                [
                    (row(*card), ProducerKind::DrawBackground),
                    (row(*card), ProducerKind::DrawForeground),
                ]
            })
            .collect()
    }

    #[test]
    fn producers_an_interest_rect_misses_are_left_out_and_record_once_it_reaches_them() {
        let mut host = cards_host_with_wheel_routing_in_card_3();
        let mut tree = PaintOrderTree::default();
        let whole = record_frame(&mut host, &mut tree);
        let whole_bytes = whole.display_list.bytes.len();
        host.source = Some(whole);
        host.clear_damage();

        // Around card 1, from a recording of the whole document: every producer is placed again, nothing records, and
        // the producers left out keep their hit-test items and blocking region.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 900, 800, 400))]);
        let around_card_1 = record_next_frame(&mut host, &mut tree);
        assert!(host.recorded.is_empty() && host.left_out.is_empty());
        assert_eq!(culled_kinds(&host), draws_of(&[2, 3]));
        assert_eq!(around_card_1.hit_test_items.len(), 6);
        let root_output = around_card_1.root_output;
        assert!(root_output.culled && root_output.culled_wheel_routing);
        assert_eq!(root_output.blocking_wheel_event_regions, 1);
        assert!(around_card_1.display_list.bytes.len() < whole_bytes);
        assert_matches_from_scratch(&mut host, &around_card_1);
        assert_cull_summaries_reach_their_producers(&tree, &host);
        assert!(!frame_is_unchanged(
            &tree,
            false,
            false,
            AssemblyCulling::LeavesNothingOut
        ));
        host.clear_damage();

        // Around card 3: its producers record, card 1's are left out, and card 2, which neither rect reaches, is copied
        // with its producers left out.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 2900, 800, 400))]);
        let around_card_3 = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.recorded, draws_of(&[3]));
        assert_eq!(culled_kinds(&host), draws_of(&[1]));
        assert_eq!(copied_scopes(&host), vec![foreground(row(2))]);
        assert!(!around_card_3.root_output.culled_wheel_routing);
        assert_eq!(around_card_3.root_output.blocking_wheel_event_regions, 1);
        assert_matches_from_scratch(&mut host, &around_card_3);
        host.clear_damage();

        // Nothing changed, so the published recording is the frame.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 2900, 800, 400))]);
        assert!(frame_is_unchanged(&tree, false, false, host.culling));
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 2800, 800, 400))]);
        assert!(!frame_is_unchanged(&tree, false, false, host.culling));

        // A damaged producer left out records nothing.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 2900, 800, 400))]);
        moves_card(&mut host, 1, ProducerKind::DrawForeground, 50);
        host.push(row(1), PaintDamage::DRAW_FOREGROUND);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let damaged_card_left_out = record_next_frame(&mut host, &mut tree);
        assert!(host.recorded.is_empty());
        assert_eq!(host.left_out, vec![(row(1), ProducerKind::DrawForeground)]);
        assert_matches_from_scratch(&mut host, &damaged_card_left_out);
        host.clear_damage();

        // A frame that leaves nothing out records everything left out.
        host.culling = AssemblyCulling::LeavesNothingOut;
        let whole_again = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.recorded, draws_of(&[1, 2]));
        assert!(!whole_again.root_output.culled);
        assert_matches_from_scratch(&mut host, &whole_again);
    }

    #[test]
    fn only_scopes_that_reach_a_scroll_node_whose_interest_rect_changed_are_walked() {
        let mut host = cards_host();
        for kind in [ProducerKind::DrawBackground, ProducerKind::DrawForeground] {
            host.contents.get_mut(&(row(3), kind)).unwrap().cull_reach =
                CullReach::in_scroll_node(NESTED_SCROLL_NODE, IntRect::new(0, 0, 800, 100));
        }
        let mut tree = PaintOrderTree::default();
        host.source = Some(record_frame(&mut host, &mut tree));
        aim(
            &mut host,
            &[
                (ROOT_SCROLL_NODE, IntRect::new(0, 0, 800, 1500)),
                (NESTED_SCROLL_NODE, IntRect::new(0, 0, 800, 50)),
            ],
        );
        record_next_frame(&mut host, &mut tree);
        host.clear_damage();

        aim(
            &mut host,
            &[
                (ROOT_SCROLL_NODE, IntRect::new(0, 0, 800, 1500)),
                (NESTED_SCROLL_NODE, IntRect::new(0, 500, 800, 50)),
            ],
        );
        let frame = record_next_frame(&mut host, &mut tree);
        assert_eq!(copied_scopes(&host), vec![foreground(row(1)), foreground(row(2))]);
        assert_eq!(culled_kinds(&host), draws_of(&[3]));
        assert_matches_from_scratch(&mut host, &frame);
    }

    fn copied_scopes(host: &TestHost) -> Vec<PaintScope> {
        host.events
            .iter()
            .filter_map(|event| match event {
                AssemblyEvent::ScopeCopied(scope) => Some(*scope),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_clean_scope_is_copied_while_a_changed_interest_rect_cuts_what_it_draws_alike() {
        let mut host = cards_host();
        let mut tree = PaintOrderTree::default();
        host.source = Some(record_frame(&mut host, &mut tree));
        host.clear_damage();
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 900, 800, 400))]);
        record_next_frame(&mut host, &mut tree);
        host.clear_damage();

        // Grown around card 1 without reaching card 2, the rect still holds all of card 1 and none of cards 2 and 3.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 800, 800, 600))]);
        let grown = record_next_frame(&mut host, &mut tree);
        assert_eq!(
            copied_scopes(&host),
            vec![foreground(row(1)), foreground(row(2)), foreground(row(3))]
        );
        assert!(host.recorded.is_empty());
        assert_matches_from_scratch(&mut host, &grown);
        host.clear_damage();

        // Reaching into card 2, the rect makes it record, while cards 1 and 3 stay as they were.
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 800, 800, 1250))]);
        let reaching_card_2 = record_next_frame(&mut host, &mut tree);
        assert_eq!(copied_scopes(&host), vec![foreground(row(1)), foreground(row(3))]);
        assert_eq!(host.recorded, draws_of(&[2]));
        assert_matches_from_scratch(&mut host, &reaching_card_2);
    }

    #[test]
    fn a_damaged_producer_left_out_keeps_the_hit_test_items_it_records() {
        let mut host = cards_host();
        host.plan(foreground(row(3)), true, &[svg()]);
        host.produce(
            row(3),
            ProducerKind::Svg,
            Content {
                cull_reach: card_reach(3, ProducerKind::DrawBackground, 0),
                ..content(2, 3)
            },
        );
        let mut tree = PaintOrderTree::default();
        host.source = Some(record_frame(&mut host, &mut tree));
        aim(&mut host, &[(ROOT_SCROLL_NODE, IntRect::new(0, 900, 800, 400))]);
        bump_version(&mut host, 3, ProducerKind::Svg);
        host.push(row(3), PaintDamage::SVG);
        host.push(row(0), PaintDamage::DESCENDANT_READERS);
        let frame = record_next_frame(&mut host, &mut tree);
        assert_eq!(host.left_out, vec![(row(3), ProducerKind::Svg)]);
        assert_eq!(frame.hit_test_items.len(), 2 + 2 + 3);
        assert_eq!(
            frame
                .hit_test_items
                .iter()
                .filter(|item| item.paintable == row(3))
                .count(),
            3
        );
        assert_matches_from_scratch(&mut host, &frame);
    }
}
