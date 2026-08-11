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

mod block_axis_available_space_funnel {
    use super::{AvailableSize, CssPixels};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(transparent)]
    pub struct BlockAxisAvailableSize(AvailableSize);

    impl BlockAxisAvailableSize {
        pub fn definite(value: CssPixels) -> Self {
            Self(AvailableSize::definite(value))
        }

        pub(crate) fn indefinite() -> Self {
            Self(AvailableSize::Indefinite)
        }

        pub(crate) fn min_content() -> Self {
            Self(AvailableSize::MinContent)
        }

        pub(crate) fn max_content() -> Self {
            Self(AvailableSize::MaxContent)
        }

        pub(crate) fn from_axis_generic_available_size(value: AvailableSize) -> Self {
            Self(value)
        }

        pub(crate) fn is_intrinsic_sizing_constraint_for_mode_dispatch(self) -> bool {
            self.0.is_intrinsic_sizing_constraint()
        }

        pub(crate) fn is_min_content_sizing_constraint_for_mode_dispatch(self) -> bool {
            self.0 == AvailableSize::MinContent
        }

        pub(crate) fn is_max_content_sizing_constraint_for_mode_dispatch(self) -> bool {
            self.0 == AvailableSize::MaxContent
        }

        pub(crate) fn axis_generic_value_for_mode_dispatch(self) -> AvailableSize {
            self.0
        }

        pub(crate) fn axis_generic_value_for_flex_and_grid_axis_erasure(self) -> AvailableSize {
            self.0
        }

        pub(crate) fn value_for_intrinsic_size_cache_key(self) -> AvailableSize {
            self.0
        }

        pub(crate) fn definite_value_required_by_out_of_flow_construction(self) -> CssPixels {
            debug_assert!(matches!(self.0, AvailableSize::Definite(_)));
            self.0.to_px_or_zero()
        }

        pub(crate) fn axis_generic_value_for_internally_consistent_decision(self) -> AvailableSize {
            self.0
        }

        pub(crate) fn definite_value_for_already_derived_state_gate(self) -> Option<CssPixels> {
            match self.0 {
                AvailableSize::Definite(value) => Some(value),
                _ => None,
            }
        }

        pub(crate) fn percentage_basis_or_zero_noting_dependence_when_indefinite(self) -> CssPixels {
            if self.0 == AvailableSize::Indefinite {
                crate::layout::note_block_size_dependence_observation();
            }
            self.0.to_px_or_zero()
        }

        pub(crate) fn is_indefinite_noting_dependence_when_indefinite(self) -> bool {
            let is_indefinite = self.0 == AvailableSize::Indefinite;
            if is_indefinite {
                crate::layout::note_block_size_dependence_observation();
            }
            is_indefinite
        }

        pub(crate) fn to_px_or_zero_for_already_derived_state(self) -> CssPixels {
            self.0.to_px_or_zero()
        }
    }
}

pub use block_axis_available_space_funnel::BlockAxisAvailableSize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct AvailableSpace {
    pub inline_size: AvailableSize,
    pub block_size: BlockAxisAvailableSize,
}

impl Default for AvailableSpace {
    fn default() -> Self {
        Self {
            inline_size: AvailableSize::Indefinite,
            block_size: BlockAxisAvailableSize::indefinite(),
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

mod block_axis_percentage_basis_funnel {
    use super::CssPixels;
    use crate::layout::note_block_size_dependence_observation;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct BlockAxisPercentageBases {
        percentage_basis_block_size: Option<CssPixels>,
        quirks_mode_percentage_basis_block_size: Option<CssPixels>,
    }

    impl BlockAxisPercentageBases {
        pub(crate) fn new(
            percentage_basis_block_size: Option<CssPixels>,
            quirks_mode_percentage_basis_block_size: Option<CssPixels>,
        ) -> Self {
            Self {
                percentage_basis_block_size,
                quirks_mode_percentage_basis_block_size,
            }
        }

        pub(crate) fn with_percentage_basis_block_size(self, value: Option<CssPixels>) -> Self {
            Self {
                percentage_basis_block_size: value,
                ..self
            }
        }

        pub(crate) fn percentage_basis_block_size_noting_dependence_when_unavailable(self) -> Option<CssPixels> {
            if self.percentage_basis_block_size.is_none() {
                note_block_size_dependence_observation();
            }
            self.percentage_basis_block_size
        }

        pub(crate) fn percentage_basis_block_size_for_child_constraint_propagation(self) -> Option<CssPixels> {
            self.percentage_basis_block_size
        }

        pub(crate) fn percentage_basis_block_size_for_cache_key(self) -> Option<CssPixels> {
            self.percentage_basis_block_size
        }

        pub(crate) fn percentage_basis_block_size_or_zero_for_document_quirk_sizing(self) -> CssPixels {
            self.percentage_basis_block_size.unwrap_or_default()
        }

        pub(crate) fn quirks_mode_percentage_basis_block_size_for_quirk_resolution(self) -> Option<CssPixels> {
            self.quirks_mode_percentage_basis_block_size
        }

        pub(crate) fn quirks_mode_percentage_basis_block_size_for_child_constraint_propagation(
            self,
        ) -> Option<CssPixels> {
            self.quirks_mode_percentage_basis_block_size
        }

        pub(crate) fn quirks_mode_percentage_basis_block_size_for_cache_key(self) -> Option<CssPixels> {
            self.quirks_mode_percentage_basis_block_size
        }
    }
}

pub(crate) use block_axis_percentage_basis_funnel::BlockAxisPercentageBases;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainingBlockConstraints {
    pub(crate) percentage_basis_inline_size: Option<CssPixels>,
    pub(crate) block_axis_bases: BlockAxisPercentageBases,
}

impl ContainingBlockConstraints {
    pub(crate) fn inline_basis(self) -> CssPixels {
        self.percentage_basis_inline_size.unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RootSizingDirectives {
    pub(crate) forced_content_inline_size: Option<CssPixels>,
    pub(crate) forced_content_block_size: Option<CssPixels>,
    pub(crate) forced_min_border_box_block_size: Option<CssPixels>,
    // Input-only: the table formatting context supplies the cell's intrinsic block padding
    // (the vertical-alignment stretch) before laying out the cell's contents.
    pub(crate) table_cell_intrinsic_block_padding: Option<(CssPixels, CssPixels)>,
    // Input-only: the wrapper's BFC tells the table formatting context where the table box's
    // content box sits in the wrapper, so rows and row groups (whose containing block is the
    // wrapper) can be placed in wrapper coordinates.
    pub(crate) table_box_content_block_offset_in_wrapper: Option<CssPixels>,
    pub(crate) adopt_automatic_content_block_size: bool,
    pub(crate) flex_self_block_size_resolution_space: Option<AvailableSpace>,
    pub(crate) float_avoidance_inline_size: Option<CssPixels>,
    pub(crate) outer_float_intrusion_before_list_item_children: SpaceUsedByFloats,
    pub(crate) treat_block_axis_percentage_insets_as_auto_beyond_root: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParticipationInParentFormattingContext {
    BlockLevel,
    Float,
    AtomicInline,
    AbsolutelyPositioned(AbsposLayoutInputs),
    Item,
    Root,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutInput {
    pub(crate) available_space: AvailableSpace,
    pub(crate) containing_block_constraints: ContainingBlockConstraints,
    pub(crate) content_box_position_in_bfc_root: Option<FfiCssPixelPoint>,
    pub(crate) sizing: RootSizingDirectives,
    pub(crate) participation: ParticipationInParentFormattingContext,
}

impl LayoutInput {
    pub(crate) fn new(
        available_space: AvailableSpace,
        containing_block_constraints: ContainingBlockConstraints,
        participation: ParticipationInParentFormattingContext,
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
        forced_content_inline_size: CssPixels,
        forced_content_block_size: CssPixels,
    ) -> Self {
        self.sizing.forced_content_inline_size = Some(forced_content_inline_size);
        self.sizing.forced_content_block_size = Some(forced_content_block_size);
        self
    }
}
