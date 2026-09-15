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
    order_revision: Cell<u64>,
    subtree_order_revision: Cell<u64>,
}

impl PaintCache {
    pub(crate) fn reset_dirty_generations(&self) {
        self.self_dirty_gen.set(0);
        self.descendant_dirty_gen.set(0);
    }
    pub(crate) fn mark_self_dirty(&self, generation: u64) {
        self.self_dirty_gen.set(generation);
    }
    pub(crate) fn mark_descendants_dirty(&self, generation: u64) -> bool {
        self.descendant_dirty_gen.replace(generation) == generation
    }
    pub(crate) fn note_order_revision(&self, revision: u64) {
        self.order_revision.set(revision);
    }
    pub(crate) fn note_subtree_order_revision(&self, revision: u64) {
        self.subtree_order_revision.set(revision);
    }
    pub(crate) fn order_unchanged_since(&self, revision: u64) -> bool {
        self.order_revision.get() <= revision
    }
    pub(crate) fn subtree_order_unchanged_since(&self, revision: u64) -> bool {
        self.subtree_order_revision.get() <= revision
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
