/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Slot identities invalidated since the last published recording. Retirement and
//! recycling are remembered even when the numeric index becomes live again. Value
//! updates keep their identities and are applied by the visual-context tree at replay.

use crate::css::style::fast_hash::FastMap;
use crate::painting::display_list::commands::{ClipNodeIndex, EffectNodeIndex, SpatialNodeIndex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum VisualContextNodeRef {
    Spatial(SpatialNodeIndex),
    Clip(ClipNodeIndex),
    Effect(EffectNodeIndex),
}

pub(crate) enum CacheChanges<'a> {
    Unchanged,
    Full,
    Nodes(&'a FastMap<VisualContextNodeRef, u64>),
}

#[derive(Default)]
pub(crate) struct VisualContextCacheChanges {
    source_epoch: u64,
    latest_epoch: u64,
    full_epoch: u64,
    nodes: FastMap<VisualContextNodeRef, u64>,
}

impl VisualContextCacheChanges {
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.nodes.capacity() * std::mem::size_of::<(VisualContextNodeRef, u64)>()
    }

    pub fn record(&mut self, before: u64, after: u64, nodes: &[VisualContextNodeRef]) {
        if self.latest_epoch != before || (before != after && nodes.is_empty()) {
            self.reset(after);
            return;
        }
        self.latest_epoch = after;
        for &node in nodes {
            self.nodes.insert(node, after);
        }
    }

    pub fn reset(&mut self, epoch: u64) {
        self.latest_epoch = epoch;
        self.full_epoch = epoch;
        self.nodes.clear();
    }

    pub fn since(&self, source_epoch: u64, current_epoch: u64) -> CacheChanges<'_> {
        if source_epoch == current_epoch {
            return CacheChanges::Unchanged;
        }
        if self.source_epoch != source_epoch || self.latest_epoch != current_epoch || self.full_epoch > source_epoch {
            return CacheChanges::Full;
        }
        CacheChanges::Nodes(&self.nodes)
    }

    // Only a cache-writing publication acknowledges the snapshot. Changes introduced
    // by resource callbacks after recording must remain pending for the following frame.
    pub fn publish(&mut self, epoch: u64) {
        self.nodes.retain(|_, changed_at| *changed_at > epoch);
        self.source_epoch = epoch;
        self.latest_epoch = self.latest_epoch.max(epoch);
        if self.full_epoch <= epoch {
            self.full_epoch = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(index: u32) -> VisualContextNodeRef {
        VisualContextNodeRef::Spatial(SpatialNodeIndex(index))
    }

    #[test]
    fn retirement_and_recycling_coalesce_until_publication() {
        let mut changes = VisualContextCacheChanges::default();
        changes.reset(1);
        changes.publish(1);
        changes.record(1, 2, &[node(4)]);
        changes.record(2, 3, &[node(4), node(5)]);
        for _ in 0..2 {
            let CacheChanges::Nodes(nodes) = changes.since(1, 3) else {
                panic!("expected slot changes")
            };
            assert_eq!(nodes.len(), 2);
            assert_eq!(nodes[&node(4)], 3);
        }
        changes.publish(3);
        assert!(changes.nodes.is_empty());
        assert!(matches!(changes.since(3, 3), CacheChanges::Unchanged));
    }

    #[test]
    fn publication_preserves_changes_newer_than_the_recorded_snapshot() {
        let mut changes = VisualContextCacheChanges::default();
        changes.reset(1);
        changes.publish(1);
        changes.record(1, 2, &[node(4)]);
        changes.record(2, 3, &[node(4), node(5)]);
        changes.publish(2);
        let CacheChanges::Nodes(nodes) = changes.since(2, 3) else {
            panic!("lost subsequent changes")
        };
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn resets_and_missing_history_require_fresh_output() {
        let mut changes = VisualContextCacheChanges::default();
        changes.reset(1);
        changes.publish(1);
        changes.reset(5);
        assert!(matches!(changes.since(1, 5), CacheChanges::Full));
        changes.publish(5);
        changes.record(6, 7, &[node(4)]);
        assert!(matches!(changes.since(5, 7), CacheChanges::Full));
        changes.publish(7);
        changes.record(7, 8, &[]);
        assert!(matches!(changes.since(7, 8), CacheChanges::Full));
    }

    #[test]
    fn node_kinds_and_source_epochs_are_distinct() {
        let mut changes = VisualContextCacheChanges::default();
        changes.reset(1);
        changes.publish(1);
        changes.record(1, 2, &[node(4), VisualContextNodeRef::Clip(ClipNodeIndex(4))]);
        assert!(matches!(changes.since(0, 2), CacheChanges::Full));
        assert!(matches!(changes.since(1, 3), CacheChanges::Full));
        let CacheChanges::Nodes(nodes) = changes.since(1, 2) else {
            panic!("expected slot changes")
        };
        assert_eq!(nodes.len(), 2);
    }
}
