/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::css_pixels::CssPixels;

/// FFI hooks for the C++ parity test.
#[unsafe(no_mangle)]
pub extern "C" fn rust_css_pixels_multiply(left_raw: i32, right_raw: i32) -> i32 {
    abort_on_panic(|| (CssPixels::from_raw(left_raw) * CssPixels::from_raw(right_raw)).raw_value())
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_css_pixels_divide_as_fraction(numerator_raw: i32, denominator_raw: i32) -> i32 {
    abort_on_panic(|| {
        CssPixels::from_raw(numerator_raw)
            .div_as_fraction(CssPixels::from_raw(denominator_raw))
            .raw_value()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_css_pixels_nearest_value_for(value: f64) -> i32 {
    abort_on_panic(|| CssPixels::nearest_value_for(value).raw_value())
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_css_pixels_scaled(raw: i32, factor: f64) -> i32 {
    abort_on_panic(|| CssPixels::from_raw(raw).scaled(factor).raw_value())
}
