/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::css_pixels::CssPixels;
use crate::display::FfiDisplay;
use std::ffi::c_void;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiSizeKind {
    Auto,
    Px,
    Percentage,
    Calc,
    MinContent,
    MaxContent,
    FitContent,
    None_,
}

/// A computed CSS size value with no Rust-owned allocation.
///
/// `kind` is an `FfiSizeKind`. `fraction` is used for Percentage, `px` for Px,
/// and `calc` for Calc. FitContent uses the matching payload for its optional
/// inner length-percentage; an all-zero payload is its keyword/zero form. A
/// non-null calc handle points to retained Rust-owned calculated style-value
/// data; the caller must balance every returned handle with
/// `ladybird_layout_release_calc_handle`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiSizeValue {
    pub kind: u8,
    pub px: CssPixels,
    pub fraction: f64,
    pub calc: *const c_void,
}

impl FfiSizeValue {
    #[cfg(test)]
    fn with_kind(kind: FfiSizeKind) -> Self {
        Self {
            kind: kind as u8,
            px: CssPixels::default(),
            fraction: 0.0,
            calc: std::ptr::null(),
        }
    }

    #[cfg(test)]
    fn px(px: CssPixels) -> Self {
        Self {
            px,
            ..Self::with_kind(FfiSizeKind::Px)
        }
    }

    #[cfg(test)]
    fn percentage(fraction: f64) -> Self {
        Self {
            fraction,
            ..Self::with_kind(FfiSizeKind::Percentage)
        }
    }
}

/// Snapshot of computed values consumed by layout formatting contexts.
///
/// Enum-valued fields contain the corresponding C++ CSS enum's underlying
/// `u8` value. `display` reuses the CSS crate's `FfiDisplay` source.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiStyleFacts {
    pub width: FfiSizeValue,
    pub height: FfiSizeValue,
    pub min_width: FfiSizeValue,
    pub min_height: FfiSizeValue,
    pub max_width: FfiSizeValue,
    pub max_height: FfiSizeValue,

    pub margin_top: FfiSizeValue,
    pub margin_right: FfiSizeValue,
    pub margin_bottom: FfiSizeValue,
    pub margin_left: FfiSizeValue,
    pub padding_top: FfiSizeValue,
    pub padding_right: FfiSizeValue,
    pub padding_bottom: FfiSizeValue,
    pub padding_left: FfiSizeValue,
    pub inset_top: FfiSizeValue,
    pub inset_right: FfiSizeValue,
    pub inset_bottom: FfiSizeValue,
    pub inset_left: FfiSizeValue,

    pub border_top_width: CssPixels,
    pub border_right_width: CssPixels,
    pub border_bottom_width: CssPixels,
    pub border_left_width: CssPixels,
    pub border_top_style: u8,
    pub border_right_style: u8,
    pub border_bottom_style: u8,
    pub border_left_style: u8,

    pub display: FfiDisplay,
    pub position: u8,
    pub float_: u8,
    pub clear: u8,
    pub writing_mode: u8,
    pub direction: u8,
    pub text_align: u8,
    pub text_justify: u8,
    pub white_space_collapse: u8,
    pub text_wrap_mode: u8,

    pub vertical_align_is_keyword: bool,
    pub vertical_align_keyword: u8,
    pub vertical_align_value: FfiSizeValue,
    pub line_height: CssPixels,
    pub font_size: CssPixels,

    pub box_sizing: u8,
    pub box_sizing_for_aspect_ratio: u8,
    pub overflow_x: u8,
    pub overflow_y: u8,
    pub flex_direction: u8,
    pub flex_wrap: u8,
    pub flex_grow: f64,
    pub flex_shrink: f64,
    pub flex_basis_is_content: bool,
    pub flex_basis: FfiSizeValue,
    pub order: i32,
    pub align_items: u8,
    pub align_self: u8,
    pub align_content: u8,
    pub justify_content: u8,
    pub justify_items: u8,
    pub justify_self: u8,
    pub row_gap: FfiSizeValue,
    pub column_gap: FfiSizeValue,

    pub has_aspect_ratio: bool,
    pub aspect_ratio_width: f64,
    pub aspect_ratio_height: f64,
    pub aspect_ratio_is_degenerate: bool,

    pub appearance: u8,
    pub border_collapse: u8,
    pub border_spacing_horizontal: CssPixels,
    pub border_spacing_vertical: CssPixels,
    pub caption_side: u8,
    pub table_layout: u8,
    pub column_width: FfiSizeValue,
    pub has_column_count: bool,
    pub column_count: i32,
    pub containment_bits: u8,
    pub container_type_bits: u8,
    pub content_visibility: u8,
    pub visibility: u8,
    pub word_break: u8,
    pub has_z_index: bool,
    pub z_index: i32,

    pub font_variant_emoji: u8,
    pub letter_spacing: CssPixels,
    pub word_spacing: CssPixels,
    pub unicode_bidi: u8,
    pub text_transform: u8,
    pub text_indent: FfiSizeValue,
    pub text_indent_each_line: bool,
    pub text_indent_hanging: bool,
    pub tab_size_is_number: bool,
    pub tab_size: CssPixels,
    pub tab_size_number: f64,
    pub grid_auto_flow_row: bool,
    pub grid_auto_flow_dense: bool,
    pub x: FfiSizeValue,
    pub y: FfiSizeValue,
    pub user_select: u8,
    pub opacity: f64,
    pub isolation: u8,
    pub mix_blend_mode: u8,
    pub transform_style: u8,
    pub has_perspective: bool,
    pub perspective: CssPixels,
    pub list_style_position: u8,
    pub text_decoration_style: u8,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiNumericType {
    has_exponent: [bool; 7],
    exponents: [i32; 7],
    has_percent_hint: bool,
    percent_hint: u8,
    valid: bool,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiResolvedCalc {
    resolved: bool,
    value: f64,
    numeric_type: CssFfiNumericType,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiCalcResolutionContext {
    basis_kind: u8,
    basis_value: f64,
    basis_unit: u8,
    length_resolution_context: *const c_void,
    callback_context: *mut c_void,
    resolve_non_math_function: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_channel_keyword: unsafe extern "C" fn(*mut c_void, u8, *mut f64) -> bool,
    random_base_value: unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> bool,
    absolutize_random_sharing: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_length: unsafe extern "C" fn(*mut c_void, f64, u8, *mut f64) -> bool,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn rust_calc_resolve(
        calculated: *const c_void,
        context: *const CssFfiCalcResolutionContext,
        apply_censoring_and_clamping: bool,
    ) -> CssFfiResolvedCalc;
}

#[cfg(test)]
unsafe fn rust_calc_resolve(
    _calculated: *const c_void,
    _context: *const CssFfiCalcResolutionContext,
    _apply_censoring_and_clamping: bool,
) -> CssFfiResolvedCalc {
    unreachable!("the CSS static library is not linked into layout Rust unit tests")
}

unsafe extern "C" fn no_non_math_function(_context: *mut c_void, _value: *const c_void) -> *const c_void {
    std::ptr::null()
}

unsafe extern "C" fn no_channel_keyword(_context: *mut c_void, _channel: u8, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_random_base_value(_context: *mut c_void, _value: *const c_void, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_absolutized_random_sharing(_context: *mut c_void, _value: *const c_void) -> *const c_void {
    std::ptr::null()
}

unsafe extern "C" fn no_fallback_length(_context: *mut c_void, _value: f64, _unit: u8, _out: *mut f64) -> bool {
    false
}

/// Resolves a snapshot calc handle through the CSS crate's existing
/// `rust_calc_resolve` export. This parity hook only supports already
/// absolutized px/percentage calculations, which need no host callbacks.
#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_resolve_calc_handle_for_parity(calc: *const c_void, percentage_basis_raw: i32) -> i32 {
    abort_on_panic(|| {
        // Pinned to C++ LengthUnit::Px; LayoutRustBridge.cpp static-asserts it.
        const LENGTH_UNIT_PX: u8 = 29;
        let context = CssFfiCalcResolutionContext {
            basis_kind: 3,
            basis_value: CssPixels::from_raw(percentage_basis_raw).to_double(),
            basis_unit: LENGTH_UNIT_PX,
            length_resolution_context: std::ptr::null(),
            callback_context: std::ptr::null_mut(),
            resolve_non_math_function: no_non_math_function,
            resolve_channel_keyword: no_channel_keyword,
            random_base_value: no_random_base_value,
            absolutize_random_sharing: no_absolutized_random_sharing,
            resolve_length: no_fallback_length,
        };
        let result = unsafe { rust_calc_resolve(calc, &raw const context, true) };
        assert!(result.resolved);
        CssPixels::nearest_value_for(result.value).raw_value()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_size_values_keep_inactive_payloads_zeroed() {
        let px = FfiSizeValue::px(CssPixels::from_raw(-65));
        assert_eq!(px.kind, FfiSizeKind::Px as u8);
        assert_eq!(px.px.raw_value(), -65);
        assert_eq!(px.fraction, 0.0);
        assert!(px.calc.is_null());

        let percentage = FfiSizeValue::percentage(-0.25);
        assert_eq!(percentage.kind, FfiSizeKind::Percentage as u8);
        assert_eq!(percentage.px.raw_value(), 0);
        assert_eq!(percentage.fraction, -0.25);
        assert!(percentage.calc.is_null());
    }

    #[test]
    fn size_kind_values_are_pinned() {
        assert_eq!(FfiSizeKind::Auto as u8, 0);
        assert_eq!(FfiSizeKind::Px as u8, 1);
        assert_eq!(FfiSizeKind::Percentage as u8, 2);
        assert_eq!(FfiSizeKind::Calc as u8, 3);
        assert_eq!(FfiSizeKind::MinContent as u8, 4);
        assert_eq!(FfiSizeKind::MaxContent as u8, 5);
        assert_eq!(FfiSizeKind::FitContent as u8, 6);
        assert_eq!(FfiSizeKind::None_ as u8, 7);
    }
}
