/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::{PaintPhase, PaintRecorder};
use crate::layout::node_data::NodeSlotId;
use crate::painting::hit_test::HitTestItem;
pub(crate) use crate::painting::hit_test::items::{HitTestFacts, hit_test_facts};
use crate::painting::hit_test::items::{HitTestItemSink, hit_test_items_of_phase};
use crate::painting::record::trace::Observer;

impl<O: Observer> HitTestItemSink for PaintRecorder<'_, O> {
    fn hit_test_facts(&mut self, paintable: NodeSlotId) -> HitTestFacts {
        self.paintable_facts(paintable)
    }

    fn own_accumulated_2d_scale(&self, paintable: NodeSlotId) -> libgfx_rust::FloatSize {
        PaintRecorder::own_accumulated_2d_scale(self, paintable)
    }

    fn append(&mut self, item: HitTestItem) {
        self.list.append(item);
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    pub(crate) fn record_foreign_object_descendant_hit_test_items(&mut self, paintable: NodeSlotId) {
        let mut next_child = crate::painting::paint_order::first_paint_child(self.source, paintable);
        while let Some(child) = next_child {
            next_child = crate::painting::paint_order::next_paint_sibling(self.source, child);
            self.record_hit_test_items(child, PaintPhase::Background);
            self.record_foreign_object_descendant_hit_test_items(child);
            self.record_hit_test_items(child, PaintPhase::Foreground);
            self.record_hit_test_items(child, PaintPhase::Overlay);
        }
    }

    pub(crate) fn record_hit_test_items(&mut self, paintable: NodeSlotId, phase: PaintPhase) {
        if self.is_recording_svg_resource_content() {
            return;
        }
        let rows = self.source;
        hit_test_items_of_phase(rows, self, paintable, phase);
    }
}
