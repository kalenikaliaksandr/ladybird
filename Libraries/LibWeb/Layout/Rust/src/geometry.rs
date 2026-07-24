/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// The layout-input mirrors are intentionally dormant until later port phases.
#![allow(dead_code)]

use crate::abort_on_panic;
use crate::css_pixels::CssPixels;
use crate::used_values::FfiCssPixelPoint;

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

    fn less_than(self, other: Self) -> bool {
        self.value < other.value
    }

    fn pixels_greater_than(self, pixels: CssPixels) -> bool {
        match self.type_ {
            AvailableSizeType::MaxContent | AvailableSizeType::Indefinite => false,
            AvailableSizeType::MinContent => true,
            AvailableSizeType::Definite => pixels > self.value,
        }
    }

    fn pixels_less_than(self, pixels: CssPixels) -> bool {
        match self.type_ {
            AvailableSizeType::MaxContent | AvailableSizeType::Indefinite => true,
            AvailableSizeType::MinContent => false,
            AvailableSizeType::Definite => pixels < self.value,
        }
    }

    fn less_than_pixels(self, pixels: CssPixels) -> bool {
        match self.type_ {
            AvailableSizeType::MinContent => true,
            AvailableSizeType::MaxContent | AvailableSizeType::Indefinite => false,
            AvailableSizeType::Definite => self.value < pixels,
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
#[repr(C)]
pub struct LogicalSize {
    pub inline_size: CssPixels,
    pub block_size: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct LogicalOffset {
    pub inline_offset: CssPixels,
    pub block_offset: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct LogicalRect {
    pub offset: LogicalOffset,
    pub size: LogicalSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiContainingBlockConstraints {
    pub has_percentage_basis_inline_size: bool,
    pub percentage_basis_inline_size: CssPixels,
    pub has_percentage_basis_block_size: bool,
    pub percentage_basis_block_size: CssPixels,
    pub has_quirks_mode_percentage_basis_block_size: bool,
    pub quirks_mode_percentage_basis_block_size: CssPixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiLayoutInput {
    pub available_space: AvailableSpace,
    pub containing_block_constraints: FfiContainingBlockConstraints,
    pub has_content_box_position_in_bfc_root: bool,
    pub content_box_position_in_bfc_root: FfiCssPixelPoint,
    pub has_table_grid_min_border_box_block_size: bool,
    pub table_grid_min_border_box_block_size: CssPixels,
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_available_size_equals(left: AvailableSize, right: AvailableSize) -> bool {
    abort_on_panic(|| left == right)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_available_size_less_than(left: AvailableSize, right: AvailableSize) -> bool {
    abort_on_panic(|| left.less_than(right))
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_available_space_equals(left: AvailableSpace, right: AvailableSpace) -> bool {
    abort_on_panic(|| left == right)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_css_pixels_greater_than_available_size(pixels_raw: i32, right: AvailableSize) -> bool {
    abort_on_panic(|| right.pixels_greater_than(CssPixels::from_raw(pixels_raw)))
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_css_pixels_less_than_available_size(pixels_raw: i32, right: AvailableSize) -> bool {
    abort_on_panic(|| right.pixels_less_than(CssPixels::from_raw(pixels_raw)))
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_available_size_less_than_css_pixels(left: AvailableSize, pixels_raw: i32) -> bool {
    abort_on_panic(|| left.less_than_pixels(CssPixels::from_raw(pixels_raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(raw: i32) -> CssPixels {
        CssPixels::from_raw(raw)
    }

    #[test]
    fn equality_compares_both_tag_and_stored_value() {
        assert_eq!(AvailableSize::definite(px(17)), AvailableSize::definite(px(17)));
        assert_ne!(AvailableSize::definite(px(17)), AvailableSize::definite(px(18)));
        assert_ne!(AvailableSize::min_content(), AvailableSize::definite(px(0)));
        assert_ne!(AvailableSize::indefinite(), AvailableSize::max_content());
    }

    #[test]
    fn available_size_less_than_compares_the_stored_pixel_value() {
        let sizes = [
            AvailableSize::definite(px(-64)),
            AvailableSize::definite(px(64)),
            AvailableSize::min_content(),
            AvailableSize::max_content(),
            AvailableSize::indefinite(),
        ];
        for left in sizes {
            for right in sizes {
                assert_eq!(left.less_than(right), left.value < right.value);
            }
        }
    }

    #[test]
    fn mixed_comparisons_match_cpp_special_values() {
        let values = [px(i32::MIN), px(-1), px(0), px(1), px(i32::MAX)];
        for value in values {
            assert!(!AvailableSize::max_content().pixels_greater_than(value));
            assert!(!AvailableSize::indefinite().pixels_greater_than(value));
            assert!(AvailableSize::min_content().pixels_greater_than(value));

            assert!(AvailableSize::max_content().pixels_less_than(value));
            assert!(AvailableSize::indefinite().pixels_less_than(value));
            assert!(!AvailableSize::min_content().pixels_less_than(value));

            assert!(!AvailableSize::max_content().less_than_pixels(value));
            assert!(!AvailableSize::indefinite().less_than_pixels(value));
            assert!(AvailableSize::min_content().less_than_pixels(value));
        }
    }
}
