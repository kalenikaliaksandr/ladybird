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
