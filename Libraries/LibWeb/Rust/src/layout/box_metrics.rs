/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PhysicalEdges {
    pub(crate) left: CssPixels,
    pub(crate) right: CssPixels,
    pub(crate) top: CssPixels,
    pub(crate) bottom: CssPixels,
}

/// The plain-value view of one box's geometry: the input payload a parent
/// hands a child run, the working state formatting contexts keep locally,
/// and the geometry a completed run reports back. The cell-based
/// UsedValues record mirrors this state during the transition and dies
/// with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BoxMetrics {
    pub(crate) content_inline_size: CssPixels,
    pub(crate) content_block_size: CssPixels,
    pub(crate) margin: PhysicalEdges,
    pub(crate) border: PhysicalEdges,
    pub(crate) padding: PhysicalEdges,
    pub(crate) inset: PhysicalEdges,
    pub(crate) has_definite_inline_size: bool,
    pub(crate) has_definite_block_size: bool,
    pub(crate) uses_collapsing_borders_model: bool,
    pub(crate) inline_size_constraint: SizeConstraint,
    pub(crate) block_size_constraint: SizeConstraint,
}

impl BoxMetrics {
    pub(crate) fn has_definite_block_size(&self) -> bool {
        self.has_definite_block_size && self.block_size_constraint == SizeConstraint::None
    }

    fn rounded_half_border(value: CssPixels) -> CssPixels {
        let value = CssPixels::from_raw(value.raw_value() / 2);
        let raw = value.raw_value();
        let rounded = if raw > 0 {
            (raw.saturating_add(32) & !63).min(i32::MAX & !63)
        } else if raw < 0 {
            let adjusted = raw.saturating_sub(32);
            let floor = adjusted & !63;
            floor.saturating_add(if adjusted & 63 != 0 { 64 } else { 0 })
        } else {
            0
        };
        CssPixels::from_raw(rounded)
    }

    pub(crate) fn border_top_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border.top)
        } else {
            self.border.top
        }
    }

    pub(crate) fn border_bottom_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border.bottom)
        } else {
            self.border.bottom
        }
    }

    pub(crate) fn border_box_top(&self, collapsed: bool) -> CssPixels {
        self.border_top_collapsed(collapsed) + self.padding.top
    }

    pub(crate) fn border_box_bottom(&self, collapsed: bool) -> CssPixels {
        self.border_bottom_collapsed(collapsed) + self.padding.bottom
    }

    pub(crate) fn border_box_block_size(&self, collapsed: bool) -> CssPixels {
        self.border_box_top(collapsed) + self.content_block_size + self.border_box_bottom(collapsed)
    }

    pub(crate) fn set_content_inline_size(&mut self, value: CssPixels) {
        self.content_inline_size = clamp_to_max_dimension_value(value.max(CssPixels::default()));
        self.has_definite_inline_size = true;
    }

    pub(crate) fn set_content_block_size(&mut self, value: CssPixels) {
        self.content_block_size = clamp_to_max_dimension_value(value.max(CssPixels::default()));
    }

    pub(crate) fn capture_from_record(used: &UsedValues) -> Self {
        Self {
            content_inline_size: used.content_inline_size.get(),
            content_block_size: used.content_block_size.get(),
            margin: PhysicalEdges {
                left: used.margin_left.get(),
                right: used.margin_right.get(),
                top: used.margin_top.get(),
                bottom: used.margin_bottom.get(),
            },
            border: PhysicalEdges {
                left: used.border_left.get(),
                right: used.border_right.get(),
                top: used.border_top.get(),
                bottom: used.border_bottom.get(),
            },
            padding: PhysicalEdges {
                left: used.padding_left.get(),
                right: used.padding_right.get(),
                top: used.padding_top.get(),
                bottom: used.padding_bottom.get(),
            },
            inset: PhysicalEdges {
                left: used.inset_left.get(),
                right: used.inset_right.get(),
                top: used.inset_top.get(),
                bottom: used.inset_bottom.get(),
            },
            has_definite_inline_size: used.has_definite_inline_size.get(),
            has_definite_block_size: used.has_definite_block_size.get(),
            uses_collapsing_borders_model: used.uses_collapsing_borders_model.get(),
            inline_size_constraint: used.inline_size_constraint.get(),
            block_size_constraint: used.block_size_constraint.get(),
        }
    }
}

/// Writes an input-carried metrics value into the handed root record. The
/// single record writer of the run prelude: parents declare root state
/// through the input, and this is where it lands, so the record's pre-run
/// state stays a pure function of the input. Raw cell writes on purpose —
/// the value already carries its definiteness decisions.
pub(crate) fn seed_root_record(used: &UsedValues, metrics: &BoxMetrics) {
    used.content_inline_size.set(metrics.content_inline_size);
    used.content_block_size.set(metrics.content_block_size);
    used.margin_left.set(metrics.margin.left);
    used.margin_right.set(metrics.margin.right);
    used.margin_top.set(metrics.margin.top);
    used.margin_bottom.set(metrics.margin.bottom);
    used.border_left.set(metrics.border.left);
    used.border_right.set(metrics.border.right);
    used.border_top.set(metrics.border.top);
    used.border_bottom.set(metrics.border.bottom);
    used.padding_left.set(metrics.padding.left);
    used.padding_right.set(metrics.padding.right);
    used.padding_top.set(metrics.padding.top);
    used.padding_bottom.set(metrics.padding.bottom);
    used.inset_left.set(metrics.inset.left);
    used.inset_right.set(metrics.inset.right);
    used.inset_top.set(metrics.inset.top);
    used.inset_bottom.set(metrics.inset.bottom);
    used.has_definite_inline_size.set(metrics.has_definite_inline_size);
    used.has_definite_block_size.set(metrics.has_definite_block_size);
    used.uses_collapsing_borders_model.set(metrics.uses_collapsing_borders_model);
    used.inline_size_constraint.set(metrics.inline_size_constraint);
    used.block_size_constraint.set(metrics.block_size_constraint);
}
