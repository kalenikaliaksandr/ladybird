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
