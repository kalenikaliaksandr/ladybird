/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::cache_changes::VisualContextNodeRef;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisualContextTreeDelta {
    pub structural_epoch_changed: bool,
    pub requires_display_list_recording: bool,
    pub tombstoned_any_node: bool,
    pub(crate) invalidated_nodes: Vec<VisualContextNodeRef>,
}

impl VisualContextTreeDelta {
    pub(crate) fn note_tombstoned(&mut self, node: VisualContextNodeRef) {
        self.invalidated_nodes.push(node);
        self.tombstoned_any_node = true;
        self.structural_epoch_changed = true;
        self.requires_display_list_recording = true;
    }

    pub(crate) fn note_repurposed_in_place(&mut self, node: VisualContextNodeRef) {
        self.invalidated_nodes.push(node);
        self.structural_epoch_changed = true;
        self.requires_display_list_recording = true;
    }

    pub(crate) fn note_allocated(&mut self, node: VisualContextNodeRef, reused_freed_slot: bool) {
        self.requires_display_list_recording = true;
        self.structural_epoch_changed |= reused_freed_slot;
        if reused_freed_slot {
            self.invalidated_nodes.push(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painting::display_list::commands::SpatialNodeIndex;

    #[test]
    fn allocating_a_fresh_slot_requires_recording_without_an_epoch_change() {
        let mut delta = VisualContextTreeDelta::default();
        delta.note_allocated(VisualContextNodeRef::Spatial(SpatialNodeIndex(1)), false);
        assert!(!delta.structural_epoch_changed);
        assert!(delta.requires_display_list_recording);
        assert!(delta.invalidated_nodes.is_empty());
    }

    #[test]
    fn reusing_a_freed_slot_changes_the_structural_epoch() {
        let mut delta = VisualContextTreeDelta::default();
        let node = VisualContextNodeRef::Spatial(SpatialNodeIndex(1));
        delta.note_allocated(node, true);
        assert!(delta.structural_epoch_changed);
        assert!(delta.requires_display_list_recording);
        assert_eq!(delta.invalidated_nodes, [node]);
    }
}
