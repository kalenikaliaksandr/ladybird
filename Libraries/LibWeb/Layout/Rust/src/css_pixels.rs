/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[path = "../../../CSS/Rust/src/css_pixels.rs"]
mod shared;

pub use shared::*;

const MAX_DIMENSION_RAW: i32 = 17_895_700 * 64;

pub(crate) fn max_dimension_value() -> CssPixels {
    CssPixels::from_raw(MAX_DIMENSION_RAW)
}

pub(crate) fn clamp_to_max_dimension_value(value: CssPixels) -> CssPixels {
    if matches!(value.raw_value(), i32::MIN | i32::MAX) {
        max_dimension_value()
    } else {
        value
    }
}

pub(crate) fn css_clamp(value: CssPixels, min: CssPixels, max: CssPixels) -> CssPixels {
    min.max(value.min(max))
}

impl std::ops::Add for CssPixels {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::from_raw(self.raw_value().saturating_add(other.raw_value()))
    }
}

impl std::ops::AddAssign for CssPixels {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl std::ops::Sub for CssPixels {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::from_raw(self.raw_value().saturating_sub(other.raw_value()))
    }
}

impl std::ops::SubAssign for CssPixels {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl std::ops::Neg for CssPixels {
    type Output = Self;

    fn neg(self) -> Self {
        Self::from_raw(0i32.saturating_sub(self.raw_value()))
    }
}

impl std::ops::Mul<usize> for CssPixels {
    type Output = Self;

    fn mul(self, other: usize) -> Self {
        let raw = (self.raw_value() as i64).saturating_mul(other as i64);
        Self::from_raw(raw.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }
}

impl std::ops::Mul<CssPixels> for usize {
    type Output = CssPixels;

    fn mul(self, other: CssPixels) -> CssPixels {
        other * self
    }
}

impl std::ops::Div<usize> for CssPixels {
    type Output = Self;

    fn div(self, other: usize) -> Self {
        assert_ne!(other, 0);
        Self::from_raw((self.raw_value() as i64 / other as i64) as i32)
    }
}
