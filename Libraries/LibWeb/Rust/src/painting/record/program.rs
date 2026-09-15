/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! One packed paint-order representation. Operations name actual occurrences; scopes are
//! intervals in that array. The only vectors belong to the whole program, not to its nodes.

use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::paint_order_plan::{PaintOrderItem, PaintProducer, PaintScope, PaintScopeKind, PaintScopePlan};
use crate::painting::paintable_rows::PaintableRowsRef;
use std::rc::Rc;

pub(crate) const NO_INDEX: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintAction {
    BeginScope,
    EndScope,
    Produce(PaintProducer),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintOp {
    pub owner: u32,
    pub scope: u32,
    pub action: PaintAction,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProgramScope {
    pub owner: u32,
    pub parent: u32,
    pub begin: u32,
    pub end: u32,
    pub topology_owner: NodeSlotId,
    pub kind: PaintScopeKind,
    pub establishes_context: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct ProgramOwner {
    pub row: NodeSlotId,
    first_use: u32,
    end_use: u32,
}

#[derive(Default)]
pub(crate) struct PaintProgram {
    pub ops: Vec<PaintOp>,
    pub scopes: Vec<ProgramScope>,
    pub owners: Vec<ProgramOwner>,
    owner_uses: Vec<u32>,
    sorted_owners: Vec<u32>,
    pub paint_overlay: bool,
}

// Keep metadata cost explicit as the representation evolves.
const _: () = assert!(std::mem::size_of::<PaintOp>() == 12);
const _: () = assert!(std::mem::size_of::<ProgramScope>() == 24);
const _: () = assert!(std::mem::size_of::<ProgramOwner>() == 12);

pub(crate) struct ProgramUpdate {
    pub program: Rc<PaintProgram>,
    // Empty means identity. Otherwise each new operation names its counterpart in the source,
    // or NO_INDEX when there was no unambiguous occurrence in that generation.
    pub source_ops: Vec<u32>,
    // For every operation, the start of the contiguous source run that contains it.
    run_starts: Vec<u32>,
}

impl ProgramUpdate {
    pub fn source_op(&self, operation: u32) -> Option<u32> {
        let source = if self.source_ops.is_empty() {
            operation
        } else {
            self.source_ops[operation as usize]
        };
        (source != NO_INDEX).then_some(source)
    }

    pub fn source_interval(&self, begin: u32, end: u32) -> Option<(u32, u32)> {
        let first = self.source_op(begin)?;
        if !self.run_starts.is_empty() && self.run_starts[(end - 1) as usize] > begin {
            return None;
        }
        Some((first, first + end - begin))
    }
}

impl PaintProgram {
    pub(super) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.ops.capacity() * std::mem::size_of::<PaintOp>()
            + self.scopes.capacity() * std::mem::size_of::<ProgramScope>()
            + self.owners.capacity() * std::mem::size_of::<ProgramOwner>()
            + (self.owner_uses.capacity() + self.sorted_owners.capacity()) * std::mem::size_of::<u32>()
    }

    pub fn owner_index(&self, row: NodeSlotId) -> Option<u32> {
        let index = self
            .sorted_owners
            .binary_search_by_key(&row.index, |&owner| self.owners[owner as usize].row.index)
            .ok()?;
        Some(self.sorted_owners[index])
    }

    pub fn uses(&self, owner: u32) -> &[u32] {
        let owner = self.owners[owner as usize];
        &self.owner_uses[owner.first_use as usize..owner.end_use as usize]
    }

    pub fn scope_key(&self, scope: u32) -> PaintScope {
        let entry = self.scopes[scope as usize];
        PaintScope {
            owner: self.owners[entry.owner as usize].row,
            kind: entry.kind,
        }
    }

    fn find_scope(&self, key: PaintScope) -> Option<u32> {
        let owner = self.owner_index(key.owner)?;
        let mut found = None;
        for &index in self.uses(owner) {
            let op = self.ops[index as usize];
            if op.action == PaintAction::BeginScope && self.scopes[op.scope as usize].kind == key.kind {
                if found.is_some() {
                    return None;
                }
                found = Some(op.scope);
            }
        }
        found
    }

    fn find_producer(&self, row: NodeSlotId, action: PaintAction, scope: Option<u32>) -> Option<u32> {
        let owner = self.owner_index(row)?;
        let mut found = None;
        for &index in self.uses(owner) {
            let op = self.ops[index as usize];
            if op.action == action && op.scope == scope.unwrap_or(NO_INDEX) {
                if found.is_some() {
                    return None;
                }
                found = Some(index);
            }
        }
        found
    }

    fn finish_owner_index(&mut self) {
        for op in &self.ops {
            if op.action != PaintAction::EndScope {
                self.owners[op.owner as usize].end_use += 1;
            }
        }
        let mut cursor = 0;
        for owner in &mut self.owners {
            owner.first_use = cursor;
            cursor += owner.end_use;
            owner.end_use = owner.first_use;
        }
        self.owner_uses.resize(cursor as usize, 0);
        for (index, op) in self.ops.iter().enumerate() {
            if op.action == PaintAction::EndScope {
                continue;
            }
            let owner = &mut self.owners[op.owner as usize];
            self.owner_uses[owner.end_use as usize] = index as u32;
            owner.end_use += 1;
        }
        self.sorted_owners.extend(0..self.owners.len() as u32);
        self.sorted_owners
            .sort_unstable_by_key(|&index| self.owners[index as usize].row.index);
    }

    pub fn compile(
        arena: &PaintableRowsRef<'_>,
        viewport: NodeSlotId,
        paint_overlay: bool,
        source: Option<(&Rc<PaintProgram>, u64)>,
    ) -> ProgramUpdate {
        if let Some((program, revision)) = source
            && revision == arena.paint_topology_revision()
            && program.paint_overlay == paint_overlay
            && program.scope_key(0) == PaintScope::stacking_context(viewport)
        {
            return ProgramUpdate {
                program: program.clone(),
                source_ops: Vec::new(),
                run_starts: Vec::new(),
            };
        }
        let mut compiler = ProgramCompiler {
            arena,
            source,
            program: PaintProgram {
                paint_overlay,
                ..Default::default()
            },
            owner_indices: FastMap::default(),
            source_ops: Vec::new(),
        };
        compiler.emit_producer(viewport, PaintProducer::Canvas, NO_INDEX, None);
        compiler.emit_scope(PaintScope::stacking_context(viewport), NO_INDEX);
        compiler.emit_producer(viewport, PaintProducer::InspectorOverlays, NO_INDEX, None);
        compiler.program.finish_owner_index();
        let mut run_starts = Vec::with_capacity(compiler.source_ops.len());
        let mut run_start = 0;
        for (index, &source_op) in compiler.source_ops.iter().enumerate() {
            if index == 0
                || source_op == NO_INDEX
                || compiler.source_ops[index - 1] == NO_INDEX
                || compiler.source_ops[index - 1] + 1 != source_op
            {
                run_start = index as u32;
            }
            run_starts.push(run_start);
        }
        ProgramUpdate {
            program: Rc::new(compiler.program),
            source_ops: compiler.source_ops,
            run_starts,
        }
    }
}

struct ProgramCompiler<'a, 'arena> {
    arena: &'a PaintableRowsRef<'arena>,
    source: Option<(&'a Rc<PaintProgram>, u64)>,
    program: PaintProgram,
    owner_indices: FastMap<NodeSlotId, u32>,
    source_ops: Vec<u32>,
}

impl ProgramCompiler<'_, '_> {
    fn owner(&mut self, row: NodeSlotId) -> u32 {
        *self.owner_indices.entry(row).or_insert_with(|| {
            let index = self.program.owners.len() as u32;
            self.program.owners.push(ProgramOwner {
                row,
                first_use: 0,
                end_use: 0,
            });
            index
        })
    }

    fn push(&mut self, op: PaintOp, source: Option<u32>) {
        assert!(self.program.ops.len() < NO_INDEX as usize);
        self.program.ops.push(op);
        if self.source.is_some() {
            self.source_ops.push(source.unwrap_or(NO_INDEX));
        }
    }

    fn emit_producer(&mut self, row: NodeSlotId, producer: PaintProducer, scope: u32, old_scope: Option<u32>) {
        let action = PaintAction::Produce(producer);
        let source = self
            .source
            .and_then(|(program, _)| program.find_producer(row, action, old_scope));
        let owner = self.owner(row);
        self.push(PaintOp { owner, scope, action }, source);
    }

    fn emit_scope(&mut self, key: PaintScope, parent: u32) {
        let old_scope = self.source.and_then(|(program, _)| program.find_scope(key));
        let topology_owner = self.arena.paint_scope_owner(key).unwrap_or(NodeSlotId::INVALID);
        if let Some((source, revision)) = self.source
            && let Some(old) = old_scope
            && source.paint_overlay == self.program.paint_overlay
            && source.scopes[old as usize].topology_owner == topology_owner
            && self
                .arena
                .paint_scope_topology_unchanged(key.owner, topology_owner, revision)
        {
            self.copy_scope(source, old, parent);
            return;
        }
        let plan = PaintScopePlan::build(self.arena, key, self.program.paint_overlay);
        // Helper descents with no operations have no rendering or eligibility semantics.
        if plan.items.is_empty() && !plan.establishes_stacking_context {
            return;
        }
        let owner = self.owner(key.owner);
        let scope = self.program.scopes.len() as u32;
        let begin = self.program.ops.len() as u32;
        self.program.scopes.push(ProgramScope {
            owner,
            parent,
            begin,
            end: 0,
            topology_owner,
            kind: key.kind,
            establishes_context: plan.establishes_stacking_context,
        });
        self.push(
            PaintOp {
                owner,
                scope,
                action: PaintAction::BeginScope,
            },
            self.source
                .and_then(|(source, _)| old_scope.map(|old| source.scopes[old as usize].begin)),
        );
        for item in plan.items {
            match item {
                PaintOrderItem::Producer(site) => self.emit_producer(site.owner, site.producer, scope, old_scope),
                PaintOrderItem::Scope(child) => self.emit_scope(child, scope),
            }
        }
        self.push(
            PaintOp {
                owner,
                scope,
                action: PaintAction::EndScope,
            },
            self.source
                .and_then(|(source, _)| old_scope.map(|old| source.scopes[old as usize].end - 1)),
        );
        self.program.scopes[scope as usize].end = self.program.ops.len() as u32;
    }

    fn copy_scope(&mut self, source: &PaintProgram, old: u32, parent: u32) {
        let old_scope = source.scopes[old as usize];
        let old_scope_end = source.scopes.partition_point(|scope| scope.begin < old_scope.end);
        let new_scope = self.program.scopes.len() as u32;
        let new_begin = self.program.ops.len() as u32;
        for (index, old_entry) in source.scopes[old as usize..old_scope_end].iter().enumerate() {
            let owner = self.owner(source.owners[old_entry.owner as usize].row);
            self.program.scopes.push(ProgramScope {
                owner,
                parent: if index == 0 {
                    parent
                } else {
                    new_scope + old_entry.parent - old
                },
                begin: new_begin + old_entry.begin - old_scope.begin,
                end: new_begin + old_entry.end - old_scope.begin,
                ..*old_entry
            });
        }
        for index in old_scope.begin..old_scope.end {
            let old_op = source.ops[index as usize];
            let owner = self.owner(source.owners[old_op.owner as usize].row);
            self.push(
                PaintOp {
                    owner,
                    scope: new_scope + old_op.scope - old,
                    action: old_op.action,
                },
                Some(index),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutNodeArena;
    use crate::painting::record::PaintPhase;

    #[test]
    fn copying_a_scope_rebases_metadata_without_changing_its_source_or_occurrence_order() {
        let parent = NodeSlotId::new(1, 1);
        let child = NodeSlotId::new(2, 1);
        let source = Rc::new(PaintProgram {
            owners: vec![
                ProgramOwner {
                    row: parent,
                    first_use: 0,
                    end_use: 0,
                },
                ProgramOwner {
                    row: child,
                    first_use: 0,
                    end_use: 0,
                },
            ],
            scopes: vec![
                ProgramScope {
                    owner: 0,
                    parent: NO_INDEX,
                    begin: 0,
                    end: 6,
                    topology_owner: parent,
                    kind: PaintScopeKind::PaintedAsStackingContext,
                    establishes_context: true,
                },
                ProgramScope {
                    owner: 1,
                    parent: 0,
                    begin: 2,
                    end: 5,
                    topology_owner: child,
                    kind: PaintScopeKind::PaintedAsStackingContext,
                    establishes_context: true,
                },
            ],
            ops: vec![
                PaintOp {
                    owner: 0,
                    scope: 0,
                    action: PaintAction::BeginScope,
                },
                PaintOp {
                    owner: 0,
                    scope: 0,
                    action: PaintAction::Produce(PaintProducer::DrawBoxPhase(PaintPhase::Background)),
                },
                PaintOp {
                    owner: 1,
                    scope: 1,
                    action: PaintAction::BeginScope,
                },
                PaintOp {
                    owner: 1,
                    scope: 1,
                    action: PaintAction::Produce(PaintProducer::DrawBoxPhase(PaintPhase::Foreground)),
                },
                PaintOp {
                    owner: 1,
                    scope: 1,
                    action: PaintAction::EndScope,
                },
                PaintOp {
                    owner: 0,
                    scope: 0,
                    action: PaintAction::EndScope,
                },
            ],
            ..Default::default()
        });
        let arena = LayoutNodeArena::new();
        let rows = arena.paintable_rows();
        let mut compiler = ProgramCompiler {
            arena: &rows,
            source: Some((&source, 0)),
            program: PaintProgram::default(),
            owner_indices: FastMap::default(),
            source_ops: Vec::new(),
        };
        compiler.copy_scope(&source, 1, NO_INDEX);
        assert_eq!(compiler.source_ops, [2, 3, 4]);
        assert_eq!(compiler.program.owners[0].row, child);
        let scope = compiler.program.scopes[0];
        assert_eq!((scope.begin, scope.end, scope.parent), (0, 3, NO_INDEX));
        assert!(compiler.program.ops.iter().all(|op| op.owner == 0 && op.scope == 0));
        assert_eq!(
            compiler.program.ops.iter().map(|op| op.action).collect::<Vec<_>>(),
            source.ops[2..5].iter().map(|op| op.action).collect::<Vec<_>>()
        );
        assert_eq!(
            (source.scopes[1].begin, source.scopes[1].end, source.scopes[1].parent),
            (2, 5, 0)
        );
    }
}
