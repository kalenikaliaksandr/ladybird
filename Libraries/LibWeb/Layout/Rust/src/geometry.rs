/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_enums::writing_mode;
use crate::css_pixels::CssPixels;
use crate::used_values::FfiCssPixelPoint;

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
#[repr(u8)]
pub enum AvailableSizeType {
    Definite,
    Indefinite,
    MinContent,
    MaxContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct AvailableSize {
    pub type_: AvailableSizeType,
    pub value: CssPixels,
}

impl AvailableSize {
    pub fn definite(value: CssPixels) -> Self {
        assert!(!matches!(value.raw_value(), i32::MIN | i32::MAX));
        Self {
            type_: AvailableSizeType::Definite,
            value,
        }
    }

    pub fn indefinite() -> Self {
        Self {
            type_: AvailableSizeType::Indefinite,
            value: CssPixels::from_raw(i32::MAX),
        }
    }

    pub fn min_content() -> Self {
        Self {
            type_: AvailableSizeType::MinContent,
            value: CssPixels::from_raw(0),
        }
    }

    pub fn max_content() -> Self {
        Self {
            type_: AvailableSizeType::MaxContent,
            value: CssPixels::from_raw(i32::MAX),
        }
    }

    pub(crate) fn is_definite(self) -> bool {
        self.type_ == AvailableSizeType::Definite
    }

    pub(crate) fn is_indefinite(self) -> bool {
        self.type_ == AvailableSizeType::Indefinite
    }

    pub(crate) fn is_min_content(self) -> bool {
        self.type_ == AvailableSizeType::MinContent
    }

    pub(crate) fn is_max_content(self) -> bool {
        self.type_ == AvailableSizeType::MaxContent
    }

    pub(crate) fn is_intrinsic_sizing_constraint(self) -> bool {
        matches!(
            self.type_,
            AvailableSizeType::MinContent | AvailableSizeType::MaxContent
        )
    }

    pub(crate) fn to_px_or_zero(self) -> CssPixels {
        if self.is_definite() {
            self.value
        } else {
            CssPixels::default()
        }
    }

    pub(crate) fn pixels_greater_than(self, pixels: CssPixels) -> bool {
        match self.type_ {
            AvailableSizeType::MaxContent | AvailableSizeType::Indefinite => false,
            AvailableSizeType::MinContent => true,
            AvailableSizeType::Definite => pixels > self.value,
        }
    }

    pub(crate) fn pixels_less_than(self, pixels: CssPixels) -> bool {
        match self.type_ {
            AvailableSizeType::MaxContent | AvailableSizeType::Indefinite => true,
            AvailableSizeType::MinContent => false,
            AvailableSizeType::Definite => pixels < self.value,
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
            inline_size: AvailableSize::indefinite(),
            block_size: AvailableSize::indefinite(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutInput {
    pub(crate) available_space: AvailableSpace,
    pub(crate) containing_block_constraints: ContainingBlockConstraints,
    pub(crate) has_content_box_position_in_bfc_root: bool,
    pub(crate) content_box_position_in_bfc_root: FfiCssPixelPoint,
    pub(crate) table_grid_min_border_box_block_size: Option<CssPixels>,
}
