/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub(crate) fn to_physical<T>(writing_mode: u8, inline: T, block: T) -> (T, T) {
    if writing_mode == writing_mode::HORIZONTAL_TB {
        (inline, block)
    } else {
        (block, inline)
    }
}

pub(crate) fn to_logical<T>(writing_mode: u8, horizontal: T, vertical: T) -> (T, T) {
    if writing_mode == writing_mode::HORIZONTAL_TB {
        (horizontal, vertical)
    } else {
        (vertical, horizontal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailableSize {
    Definite(CssPixels),
    Indefinite,
    MinContent,
    MaxContent,
}

impl AvailableSize {
    pub fn definite(value: CssPixels) -> Self {
        assert!(!matches!(value.raw_value(), i32::MIN | i32::MAX));
        Self::Definite(value)
    }

    pub(crate) fn is_intrinsic_sizing_constraint(self) -> bool {
        matches!(self, Self::MinContent | Self::MaxContent)
    }

    pub(crate) fn to_px_or_zero(self) -> CssPixels {
        match self {
            Self::Definite(value) => value,
            _ => CssPixels::default(),
        }
    }

    pub(crate) fn pixels_greater_than(self, pixels: CssPixels) -> bool {
        match self {
            Self::MaxContent | Self::Indefinite => false,
            Self::MinContent => true,
            Self::Definite(value) => pixels > value,
        }
    }

    pub(crate) fn pixels_less_than(self, pixels: CssPixels) -> bool {
        match self {
            Self::MaxContent | Self::Indefinite => true,
            Self::MinContent => false,
            Self::Definite(value) => pixels < value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct AvailableSpace {
    pub inline_size: AvailableSize,
    pub block_size: AvailableSize,
}

impl Default for AvailableSpace {
    fn default() -> Self {
        Self {
            inline_size: AvailableSize::Indefinite,
            block_size: AvailableSize::Indefinite,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogicalSize {
    pub(crate) inline_size: CssPixels,
    pub(crate) block_size: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogicalOffset {
    pub(crate) inline_offset: CssPixels,
    pub(crate) block_offset: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogicalRect {
    pub(crate) offset: LogicalOffset,
    pub(crate) size: LogicalSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainingBlockConstraints {
    pub(crate) percentage_basis_inline_size: Option<CssPixels>,
    pub(crate) percentage_basis_block_size: Option<CssPixels>,
    pub(crate) quirks_mode_percentage_basis_block_size: Option<CssPixels>,
}

impl ContainingBlockConstraints {
    pub(crate) fn inline_basis(self) -> CssPixels {
        self.percentage_basis_inline_size.unwrap_or_default()
    }

    pub(crate) fn block_basis(self) -> CssPixels {
        self.percentage_basis_block_size.unwrap_or_default()
    }
}

// Parent-authoritative sizing directives for the root box of a formatting
// context run. A forced content size is adopted verbatim by the run prelude
// and marks the axis definite; resolution steps for that axis are skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RootSizingDirectives {
    pub(crate) forced_content_inline_size: Option<CssPixels>,
    pub(crate) forced_content_block_size: Option<CssPixels>,
    // Minimum border-box block size a flex container imposes on a stretched
    // table-wrapper item; relayed by the wrapper's block formatting context
    // into the table run.
    pub(crate) forced_min_border_box_block_size: Option<CssPixels>,
    // The table box's content-box offset within its anonymous wrapper, in the
    // wrapper's coordinate space. Set only by the block parent launching a
    // table formatting context; the table run bases its row and row-group
    // placement on it and reports the caption-adjusted offset back through
    // ChildLayoutResult.
    pub(crate) table_box_content_offset_in_wrapper: Option<LogicalOffset>,
    // The container wants the run root's content block size adopted verbatim
    // from the run's automatic content block size — no min/max clamping and
    // no quirks. Requested for container-internal roots whose block size the
    // container derives from content (auto-height table cells, the anonymous
    // wrapper of replaced-with-children boxes).
    pub(crate) adopt_automatic_content_block_size: bool,
}

// How the root box of a formatting context run participates in its parent's
// layout. This decides which sizing rules the run prelude applies to the
// root; it is stated explicitly by the parent building the input rather than
// derived from node facts, because the same box kind can participate
// differently depending on tree position (a table wrapper is an `Item` as a
// flex item but `BlockLevel` in a block flow).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FcParticipation {
    // In-flow block-level box in a block formatting context.
    BlockLevel,
    // Floating box; sized shrink-to-fit by its block parent.
    Float,
    // Atomic inline-level box in an inline formatting context.
    Atomic,
    // Absolutely positioned box sized by the abspos engine.
    OutOfFlow,
    // Flex item, grid item, table participant, or other container-internal
    // box whose used values the container creates and sizes itself.
    Item,
    // FFI entry or measurement root; sized by forced directives, previously
    // committed geometry, or the measurement constraints.
    Root,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutInput {
    pub(crate) available_space: AvailableSpace,
    pub(crate) containing_block_constraints: ContainingBlockConstraints,
    pub(crate) content_box_position_in_bfc_root: Option<FfiCssPixelPoint>,
    pub(crate) sizing: RootSizingDirectives,
    pub(crate) participation: FcParticipation,
}

impl LayoutInput {
    pub(crate) fn new(
        available_space: AvailableSpace,
        containing_block_constraints: ContainingBlockConstraints,
        participation: FcParticipation,
    ) -> Self {
        Self {
            available_space,
            containing_block_constraints,
            content_box_position_in_bfc_root: None,
            sizing: RootSizingDirectives::default(),
            participation,
        }
    }

    pub(crate) fn with_forced_sizes(
        mut self,
        forced_content_inline_size: Option<CssPixels>,
        forced_content_block_size: Option<CssPixels>,
    ) -> Self {
        self.sizing.forced_content_inline_size = forced_content_inline_size;
        self.sizing.forced_content_block_size = forced_content_block_size;
        self
    }
}
