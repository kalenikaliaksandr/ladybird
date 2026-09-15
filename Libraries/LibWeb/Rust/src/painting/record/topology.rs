/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Changes to CSS ordering accumulated against the last published program. The source
//! owns occurrence identities and ancestry even after their layout rows have been retired.
//! The log identifies stale plans; the CSS planner derives their replacement from the
//! final committed state, without replaying intermediate DOM mutations.

use super::program::{NO_INDEX, PaintAction, PaintProgram};
use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::paint_order_plan::PaintScope;
use std::rc::Rc;

#[derive(Clone, Copy)]
struct ScopeChange {
    revision: u64,
    descendants: bool,
}

#[derive(Default)]
pub(crate) struct PaintTopologyChanges {
    source: Option<Rc<PaintProgram>>,
    source_revision: u64,
    revision: u64,
    // Keys belong to the retained source, bounding the log by its scope count. Repeated
    // notifications merge into one entry, including multiple commits before recording.
    scopes: FastMap<PaintScope, ScopeChange>,
}

impl PaintTopologyChanges {
    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn matches_source(&self, source: &Rc<PaintProgram>, revision: u64) -> bool {
        self.source_revision == revision && self.source.as_ref().is_some_and(|old| Rc::ptr_eq(old, source))
    }

    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.scopes.capacity() * std::mem::size_of::<(PaintScope, ScopeChange)>()
        // The source's arrays are shared with, and accounted for by, the frame cache.
    }

    fn note_scope(&mut self, scope: PaintScope, descendants: bool) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("paint topology revision overflowed");
        let change = self.scopes.entry(scope).or_insert(ScopeChange {
            revision: self.revision,
            descendants,
        });
        change.revision = self.revision;
        change.descendants |= descendants;
    }

    // Resolve all old occurrences now. No later query needs the row or its paint parents
    // to remain alive. An absent row contributed no operations to this source generation.
    pub fn note_row(&mut self, row: NodeSlotId, descendants: bool) -> bool {
        let Some(source) = self.source.clone() else {
            return false;
        };
        let Some(owner) = source.owner_index(row) else {
            return false;
        };
        for &index in source.uses(owner) {
            let op = source.ops[index as usize];
            if op.scope != NO_INDEX {
                self.note_scope(source.scope_key(op.scope), descendants);
            }
        }
        true
    }

    pub fn note_context(&mut self, row: NodeSlotId, descendants: bool) -> bool {
        let key = PaintScope::stacking_context(row);
        let Some(source) = &self.source else {
            return false;
        };
        let Some(owner) = source.owner_index(row) else {
            return false;
        };
        if !source.uses(owner).iter().any(|&index| {
            let op = source.ops[index as usize];
            op.action == PaintAction::BeginScope && source.scope_key(op.scope) == key
        }) {
            return false;
        }
        self.note_scope(key, descendants);
        true
    }

    pub fn invalidated_scopes(&self, source: &Rc<PaintProgram>, revision: u64) -> Vec<bool> {
        if !self.matches_source(source, revision) {
            return vec![true; source.scopes.len()];
        }
        let mut invalid = vec![false; source.scopes.len()];
        for (&key, change) in &self.scopes {
            let Some(owner) = source.owner_index(key.owner) else {
                continue;
            };
            // A key can name several occurrences. Invalidate all of them; matching one
            // arbitrarily would hide changes behind another occurrence's clean interval.
            for &index in source.uses(owner) {
                let op = source.ops[index as usize];
                if op.action != PaintAction::BeginScope || source.scope_key(op.scope) != key {
                    continue;
                }
                let scope = op.scope as usize;
                if change.descendants {
                    let end = source
                        .scopes
                        .partition_point(|entry| entry.begin < source.scopes[scope].end);
                    invalid[scope + 1..end].fill(true);
                }
                let mut current = op.scope;
                while current != NO_INDEX && !invalid[current as usize] {
                    invalid[current as usize] = true;
                    current = source.scopes[current as usize].parent;
                }
            }
        }
        invalid
    }

    // Only publication advances the source. Read-only recordings inspect this log without
    // consuming it. Resource callbacks can add changes after compilation; keep those pending.
    pub fn publish(&mut self, source: Rc<PaintProgram>, revision: u64) {
        self.scopes.retain(|_, change| change.revision > revision);
        self.source = Some(source);
        self.source_revision = revision;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutNodeArena;
    use crate::painting::record::program::test_program;

    fn row(index: u32) -> NodeSlotId {
        NodeSlotId::new(index, 1)
    }

    fn program() -> Rc<PaintProgram> {
        test_program(&[(row(0), NO_INDEX), (row(1), 0), (row(2), 1), (row(3), 0)])
    }

    #[test]
    fn local_changes_rebuild_ancestors_without_invalidating_siblings_or_children() {
        let program = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(program.clone(), 0);
        changes.note_row(row(1), false);
        assert_eq!(changes.invalidated_scopes(&program, 0), [true, true, false, false]);
        changes.note_row(row(2), false);
        assert_eq!(changes.invalidated_scopes(&program, 0), [true, true, true, false]);
    }

    #[test]
    fn participation_changes_invalidate_nested_plans() {
        let program = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(program.clone(), 0);
        changes.note_row(row(1), true);
        assert_eq!(changes.invalidated_scopes(&program, 0), [true, true, true, false]);
    }

    #[test]
    fn context_composition_changes_preserve_the_internal_plans_of_its_children() {
        let source = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(source.clone(), 0);
        changes.note_context(row(0), false);
        assert_eq!(changes.invalidated_scopes(&source, 0), [true, false, false, false]);
    }

    #[test]
    fn geometry_is_not_an_ordering_input_but_flex_item_participation_is() {
        let mut arena = LayoutNodeArena::new();
        let root = arena.allocate_for_test().slot;
        let child = arena.allocate_for_test().slot;
        let sibling = arena.allocate_for_test().slot;
        for node in [child, sibling] {
            arena.insert_child(root, node, NodeSlotId::INVALID);
        }
        for node in [root, child, sibling] {
            arena.populate_paintable_row(node);
            arena.refresh_paint_order_inputs(node);
        }
        let source = test_program(&[(root, NO_INDEX), (child, 0), (sibling, 0)]);
        arena.publish_paint_topology(source.clone(), 0);
        arena.paintable_rows_mut().paintable_data_mut(child).offset.x =
            crate::css::css_pixels::CssPixels::from_integer(100);
        arena.refresh_paint_order_inputs(child);
        assert_eq!(
            arena.pending_paint_topology_changes().invalidated_scopes(&source, 0),
            [false; 3]
        );
        let flags = &arena.data(child).flags;
        flags.set(flags.get() | crate::layout::node_data::NodeFlag::IsFlexItem as u32);
        arena.refresh_paint_order_inputs(child);
        assert_eq!(
            arena.pending_paint_topology_changes().invalidated_scopes(&source, 0),
            [true, true, false]
        );
    }

    #[test]
    fn notifications_coalesce_and_read_only_compilation_does_not_consume_them() {
        let program = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(program.clone(), 0);
        for _ in 0..10_000 {
            changes.note_row(row(2), false);
        }
        assert_eq!(changes.scopes.len(), 1);
        let expected = [true, true, true, false];
        assert_eq!(changes.invalidated_scopes(&program, 0), expected);
        assert_eq!(changes.invalidated_scopes(&program, 0), expected);
        let revision = changes.revision();
        changes.publish(program.clone(), revision);
        assert!(changes.scopes.is_empty());
        assert_eq!(changes.invalidated_scopes(&program, revision), [false; 4]);
    }

    #[test]
    fn publication_keeps_notifications_added_after_the_recording_snapshot() {
        let source = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(source.clone(), 0);
        changes.note_row(row(1), false);
        let revision = changes.revision();
        changes.note_row(row(3), false);
        let destination = program();
        changes.publish(destination.clone(), revision);
        assert_eq!(changes.scopes.len(), 1);
        assert_eq!(
            changes.invalidated_scopes(&destination, revision),
            [true, false, false, true]
        );
    }

    #[test]
    fn generation_checked_keys_do_not_invalidate_a_reused_layout_slot() {
        let source = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(source.clone(), 0);
        assert!(!changes.note_row(NodeSlotId::new(2, 2), false));
        assert!(changes.scopes.is_empty());
        assert_eq!(changes.invalidated_scopes(&source, 0), [false; 4]);
    }

    #[test]
    fn all_occurrences_of_an_ambiguous_scope_key_are_invalidated() {
        let source = test_program(&[(row(0), NO_INDEX), (row(1), 0), (row(1), 0), (row(3), 0)]);
        let mut changes = PaintTopologyChanges::default();
        changes.publish(source.clone(), 0);
        changes.note_row(row(1), false);
        assert_eq!(changes.invalidated_scopes(&source, 0), [true, true, true, false]);
    }

    #[test]
    fn a_different_source_generation_cannot_reuse_the_logs_scope_decisions() {
        let source = program();
        let mut changes = PaintTopologyChanges::default();
        changes.publish(source.clone(), 0);
        assert_eq!(changes.invalidated_scopes(&program(), 0), [true; 4]);
        assert_eq!(changes.invalidated_scopes(&source, 1), [true; 4]);
    }

    #[test]
    fn clearing_parents_before_descendants_preserves_unrelated_scope_plans() {
        let mut arena = LayoutNodeArena::new();
        let root = arena.allocate_for_test().slot;
        let branch = arena.allocate_for_test().slot;
        let child = arena.allocate_for_test().slot;
        let sibling = arena.allocate_for_test().slot;
        arena.insert_child(root, branch, NodeSlotId::INVALID);
        arena.insert_child(branch, child, NodeSlotId::INVALID);
        arena.insert_child(root, sibling, NodeSlotId::INVALID);
        for node in [root, branch, child, sibling] {
            arena.populate_paintable_row(node);
        }
        let source = test_program(&[(root, NO_INDEX), (branch, 0), (child, 1), (sibling, 0)]);
        arena.publish_paint_topology(source.clone(), 0);
        for node in [branch, child] {
            let reset = arena.prepare_paintable_row_cleared_reset(node).unwrap();
            arena.paintable_row_cleared(reset);
        }
        arena.remove_child(root, branch);
        let _freed = arena.free_subtree(branch);
        assert_eq!(
            arena.pending_paint_topology_changes().invalidated_scopes(&source, 0),
            [true, true, true, false]
        );
    }

    #[test]
    fn reparenting_invalidates_both_source_and_destination_scopes() {
        let mut arena = LayoutNodeArena::new();
        let root = arena.allocate_for_test().slot;
        let old_parent = arena.allocate_for_test().slot;
        let child = arena.allocate_for_test().slot;
        let new_parent = arena.allocate_for_test().slot;
        let sibling = arena.allocate_for_test().slot;
        for (parent, node) in [
            (root, old_parent),
            (old_parent, child),
            (root, new_parent),
            (root, sibling),
        ] {
            arena.insert_child(parent, node, NodeSlotId::INVALID);
        }
        for node in [root, old_parent, child, new_parent, sibling] {
            arena.populate_paintable_row(node);
        }
        let source = test_program(&[
            (root, NO_INDEX),
            (old_parent, 0),
            (child, 1),
            (new_parent, 0),
            (sibling, 0),
        ]);
        arena.publish_paint_topology(source.clone(), 0);
        arena.remove_child(old_parent, child);
        arena.insert_child(new_parent, child, NodeSlotId::INVALID);
        assert_eq!(
            arena.pending_paint_topology_changes().invalidated_scopes(&source, 0),
            [true, true, true, true, false]
        );
    }
}
