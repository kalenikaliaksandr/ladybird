/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Enum variants and repr(C) fields are consumed directly by C++ through the
// generated header even when Rust does not construct each one yet.
#![allow(dead_code)]

use crate::css_pixels::CssPixels;
use std::ffi::c_void;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiSizeConstraint {
    #[default]
    None,
    MinContent,
    MaxContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCssPixelPoint {
    pub x: CssPixels,
    pub y: CssPixels,
}

impl Default for FfiCssPixelPoint {
    fn default() -> Self {
        Self {
            x: CssPixels::from_raw(0),
            y: CssPixels::from_raw(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiLineBoxFragmentCoordinate {
    pub line_box_index: usize,
    pub fragment_index: usize,
}

/// The plain-data portion of LayoutState::UsedValues.
///
/// Rust owns these allocations. C++ caches pointers to them and reads or writes
/// the fields directly, so ordinary used-value access does not cross the FFI
/// boundary.
#[derive(Debug)]
#[repr(C)]
pub struct UsedValuesCore {
    pub node: *mut c_void,

    pub content_inline_size: CssPixels,
    pub content_block_size: CssPixels,

    pub margin_left: CssPixels,
    pub margin_right: CssPixels,
    pub margin_top: CssPixels,
    pub margin_bottom: CssPixels,

    pub border_left: CssPixels,
    pub border_right: CssPixels,
    pub border_top: CssPixels,
    pub border_bottom: CssPixels,

    pub padding_left: CssPixels,
    pub padding_right: CssPixels,
    pub padding_top: CssPixels,
    pub padding_bottom: CssPixels,

    pub inset_left: CssPixels,
    pub inset_right: CssPixels,
    pub inset_top: CssPixels,
    pub inset_bottom: CssPixels,

    pub has_definite_inline_size: bool,
    pub has_definite_block_size: bool,
    pub materialized_from_paintable: bool,

    pub inline_size_constraint: FfiSizeConstraint,
    pub block_size_constraint: FfiSizeConstraint,

    pub has_content_offset: bool,
    pub content_offset: FfiCssPixelPoint,

    pub has_first_baseline: bool,
    pub first_baseline: CssPixels,
    pub has_last_baseline: bool,
    pub last_baseline: CssPixels,

    pub has_containing_line_box_fragment: bool,
    pub containing_line_box_fragment: FfiLineBoxFragmentCoordinate,
}

impl Default for UsedValuesCore {
    fn default() -> Self {
        let zero = CssPixels::from_raw(0);
        Self {
            node: std::ptr::null_mut(),
            content_inline_size: zero,
            content_block_size: zero,
            margin_left: zero,
            margin_right: zero,
            margin_top: zero,
            margin_bottom: zero,
            border_left: zero,
            border_right: zero,
            border_top: zero,
            border_bottom: zero,
            padding_left: zero,
            padding_right: zero,
            padding_top: zero,
            padding_bottom: zero,
            inset_left: zero,
            inset_right: zero,
            inset_top: zero,
            inset_bottom: zero,
            has_definite_inline_size: false,
            has_definite_block_size: false,
            materialized_from_paintable: false,
            inline_size_constraint: FfiSizeConstraint::None,
            block_size_constraint: FfiSizeConstraint::None,
            has_content_offset: false,
            content_offset: FfiCssPixelPoint::default(),
            has_first_baseline: false,
            first_baseline: zero,
            has_last_baseline: false,
            last_baseline: zero,
            has_containing_line_box_fragment: false,
            containing_line_box_fragment: FfiLineBoxFragmentCoordinate::default(),
        }
    }
}

impl UsedValuesCore {
    const MAX_DIMENSION_RAW: i32 = 17_895_700 * 64;

    fn clamp_dimension(value: CssPixels) -> CssPixels {
        if value.raw_value() == i32::MAX || value.raw_value() == i32::MIN {
            CssPixels::from_raw(Self::MAX_DIMENSION_RAW)
        } else {
            value
        }
    }

    pub(crate) fn has_definite_inline_size(&self) -> bool {
        self.has_definite_inline_size && self.inline_size_constraint == FfiSizeConstraint::None
    }

    pub(crate) fn has_definite_block_size(&self) -> bool {
        self.has_definite_block_size && self.block_size_constraint == FfiSizeConstraint::None
    }

    pub(crate) fn set_content_inline_size(&mut self, value: CssPixels) {
        self.content_inline_size = Self::clamp_dimension(value.max(CssPixels::default()));
        self.has_definite_inline_size = true;
    }

    pub(crate) fn set_content_block_size(&mut self, value: CssPixels) {
        self.content_block_size = Self::clamp_dimension(value.max(CssPixels::default()));
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

    pub(crate) fn border_left_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border_left)
        } else {
            self.border_left
        }
    }

    pub(crate) fn border_right_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border_right)
        } else {
            self.border_right
        }
    }

    pub(crate) fn border_top_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border_top)
        } else {
            self.border_top
        }
    }

    pub(crate) fn border_bottom_collapsed(&self, collapsed: bool) -> CssPixels {
        if collapsed {
            Self::rounded_half_border(self.border_bottom)
        } else {
            self.border_bottom
        }
    }

    pub(crate) fn border_box_left(&self, collapsed: bool) -> CssPixels {
        self.border_left_collapsed(collapsed) + self.padding_left
    }

    pub(crate) fn border_box_right(&self, collapsed: bool) -> CssPixels {
        self.border_right_collapsed(collapsed) + self.padding_right
    }

    pub(crate) fn border_box_top(&self, collapsed: bool) -> CssPixels {
        self.border_top_collapsed(collapsed) + self.padding_top
    }

    pub(crate) fn border_box_bottom(&self, collapsed: bool) -> CssPixels {
        self.border_bottom_collapsed(collapsed) + self.padding_bottom
    }

    pub(crate) fn border_box_inline_size(&self, collapsed: bool) -> CssPixels {
        self.border_box_left(collapsed) + self.content_inline_size + self.border_box_right(collapsed)
    }

    pub(crate) fn border_box_block_size(&self, collapsed: bool) -> CssPixels {
        self.border_box_top(collapsed) + self.content_block_size + self.border_box_bottom(collapsed)
    }

    pub(crate) fn margin_box_bottom(&self, collapsed: bool) -> CssPixels {
        self.margin_bottom + self.border_box_bottom(collapsed)
    }

    pub(crate) fn margin_box_block_size(&self, collapsed: bool) -> CssPixels {
        self.margin_top + self.border_box_top(collapsed) + self.content_block_size + self.margin_box_bottom(collapsed)
    }

    pub(crate) fn available_inner_space_or_constraints_from(
        &self,
        outer: crate::geometry::AvailableSpace,
    ) -> crate::geometry::AvailableSpace {
        use crate::geometry::{AvailableSize, AvailableSizeType};

        let mut inline_size = match self.inline_size_constraint {
            FfiSizeConstraint::MinContent => AvailableSize::min_content(),
            FfiSizeConstraint::MaxContent => AvailableSize::max_content(),
            FfiSizeConstraint::None if self.has_definite_inline_size => {
                AvailableSize::definite(self.content_inline_size)
            }
            FfiSizeConstraint::None => AvailableSize::indefinite(),
        };
        let mut block_size = match self.block_size_constraint {
            FfiSizeConstraint::MinContent => AvailableSize::min_content(),
            FfiSizeConstraint::MaxContent => AvailableSize::max_content(),
            FfiSizeConstraint::None if self.has_definite_block_size => AvailableSize::definite(self.content_block_size),
            FfiSizeConstraint::None => AvailableSize::indefinite(),
        };
        if inline_size.type_ == AvailableSizeType::Indefinite
            && matches!(
                outer.inline_size.type_,
                AvailableSizeType::MinContent | AvailableSizeType::MaxContent
            )
        {
            inline_size = outer.inline_size;
        }
        if block_size.type_ == AvailableSizeType::Indefinite
            && matches!(
                outer.block_size.type_,
                AvailableSizeType::MinContent | AvailableSizeType::MaxContent
            )
        {
            block_size = outer.block_size;
        }
        crate::geometry::AvailableSpace {
            inline_size,
            block_size,
        }
    }
}
