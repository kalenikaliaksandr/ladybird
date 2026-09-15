/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::PaintPhase;
use crate::layout::node_data::NodeSlotId;
use crate::painting::hit_test::HitTestItem;
use crate::painting::paint_order_plan::StackingContextPaintPhase;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CaptureKind {
    BoxPhase(PaintPhase),
    DescendantSubtreePhase(StackingContextPaintPhase),
    PaintedAsStackingContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CaptureSite {
    pub paintable: NodeSlotId,
    pub kind: CaptureKind,
}

pub(crate) type RecordGen = u32;
pub(crate) fn narrow_record_gen(generation: u64) -> RecordGen {
    RecordGen::try_from(generation).expect("paint cache record generation exceeds u32")
}

/// Invalidation stamps compared with the published source frame's generation. Whether an
/// occurrence actually produced reusable output belongs to that frame's complete directory.
#[derive(Default)]
pub struct PaintCache {
    self_dirty_gen: Cell<u64>,
    descendant_dirty_gen: Cell<u64>,
}

impl PaintCache {
    pub(crate) fn reset_dirty_generations(&self) {
        self.self_dirty_gen.set(0);
        self.descendant_dirty_gen.set(0);
    }
    pub(crate) fn mark_self_dirty(&self, generation: u64) -> bool {
        self.self_dirty_gen.replace(generation) == generation
    }
    pub(crate) fn mark_descendants_dirty(&self, generation: u64) -> bool {
        self.descendant_dirty_gen.replace(generation) == generation
    }
    pub(crate) fn is_self_dirty_since(&self, generation: RecordGen) -> bool {
        self.self_dirty_gen.get() > u64::from(generation)
    }
    pub(crate) fn has_dirty_descendants_since(&self, generation: RecordGen) -> bool {
        self.descendant_dirty_gen.get() > u64::from(generation)
    }
}

pub struct HitTestItemCacheSource {
    pub items: Rc<Vec<HitTestItem>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dirty_stamps_are_relative_to_the_published_source() {
        let cache = PaintCache::default();
        let captured_at = 1;
        cache.mark_self_dirty(2);
        cache.mark_descendants_dirty(2);
        assert!(cache.is_self_dirty_since(captured_at));
        assert!(cache.has_dirty_descendants_since(captured_at));
        assert!(!cache.is_self_dirty_since(2));
        // A read-only recording has not advanced the source generation.
        assert!(cache.is_self_dirty_since(captured_at));
    }
}

/// A bounded scheduling hint. Persistent row stamps and source-directory availability remain
/// the reuse contract; uncertain subtree dependencies retain the validated recording path.
#[derive(Clone, Copy)]
pub(crate) struct DirtyPaintRow {
    pub row: NodeSlotId,
    pub descendants: bool,
}

#[derive(Default)]
pub(crate) struct PendingPaintRows {
    pub rows: Vec<DirtyPaintRow>,
    pub requires_validation: bool,
}

impl PendingPaintRows {
    pub fn note(&mut self, row: NodeSlotId, descendants: bool) {
        if self.requires_validation {
            return;
        }
        if self.rows.len() == 4096 {
            self.require_validation();
            return;
        }
        self.rows.push(DirtyPaintRow { row, descendants });
    }
    pub fn require_validation(&mut self) {
        self.requires_validation = true;
        self.rows.clear();
    }
    pub fn clear(&mut self) {
        self.rows.clear();
        self.requires_validation = false;
    }
}

#[cfg(test)]
mod scheduling_tests {
    use crate::layout::LayoutNodeArena;

    #[test]
    fn own_and_descendant_work_are_distinct_and_deduplicated_until_publication() {
        let mut arena = LayoutNodeArena::new();
        let parent = arena.allocate_for_test().slot;
        let child = arena.allocate_for_test().slot;
        arena.data(child).parent.set(parent);
        arena.populate_paintable_row(parent);
        arena.populate_paintable_row(child);
        arena.invalidate_paint_cache(child);
        arena.invalidate_paint_cache(child);
        {
            let pending = arena.pending_paint_rows();
            assert_eq!(pending.rows.len(), 2);
            assert!(
                pending
                    .rows
                    .iter()
                    .any(|entry| entry.row == child && !entry.descendants)
            );
            assert!(
                pending
                    .rows
                    .iter()
                    .any(|entry| entry.row == parent && entry.descendants)
            );
            assert!(!pending.requires_validation);
        }
        arena.note_paint_record_completed_with_cache_writes();
        assert!(arena.pending_paint_rows().rows.is_empty());
        arena
            .paintable_rows()
            .mark_descendant_subtree_caches_dirty_along_paint_chain(parent);
        assert!(arena.pending_paint_rows().requires_validation);
    }
}
