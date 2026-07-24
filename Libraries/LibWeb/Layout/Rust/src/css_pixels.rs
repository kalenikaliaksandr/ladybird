/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

/// ABI storage for the shared CSS fixed-point pixel representation.
///
/// Pixel arithmetic stays in the shared CSS implementation. Layout's used
/// values and dormant geometry types only need to preserve and compare the raw
/// signed fixed-point value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct CssPixels(i32);

impl CssPixels {
    pub fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn raw_value(self) -> i32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_css_pixels;

    #[test]
    fn representation_matches_the_shared_css_type() {
        assert_eq!(size_of::<CssPixels>(), size_of::<shared_css_pixels::CssPixels>());
        assert_eq!(align_of::<CssPixels>(), align_of::<shared_css_pixels::CssPixels>());
        for raw in [i32::MIN, -65, -1, 0, 1, 65, i32::MAX] {
            assert_eq!(
                CssPixels::from_raw(raw).raw_value(),
                shared_css_pixels::CssPixels::from_raw(raw).raw_value()
            );
        }
    }
}
