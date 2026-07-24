/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Track {
    pub(crate) base_size: CssPixels,
    pub(crate) growth_limit: Option<CssPixels>,
    pub(crate) flex_factor: Option<f64>,
    pub(crate) max_is_intrinsic: bool,
    pub(crate) max_is_max_content: bool,
}

impl Track {
    #[cfg(test)]
    pub(crate) fn fixed(base_size: CssPixels) -> Self {
        Self {
            base_size,
            growth_limit: Some(base_size),
            flex_factor: None,
            max_is_intrinsic: false,
            max_is_max_content: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn flexible(base_size: CssPixels, flex_factor: f64) -> Self {
        Self {
            base_size,
            growth_limit: None,
            flex_factor: Some(flex_factor),
            max_is_intrinsic: false,
            max_is_max_content: false,
        }
    }
}

/// The "find the size of an fr" sub-algorithm, preserving the C++ operation
/// order and its conversion of every flex factor through `CSSPixels`.
pub(crate) fn find_fr_size(tracks: &[Track], space_to_fill: CssPixels) -> CssPixels {
    let mut inflexible = vec![false; tracks.len()];
    loop {
        let mut leftover_space = space_to_fill;
        for (index, track) in tracks.iter().enumerate() {
            if inflexible[index] || track.flex_factor.is_none() {
                leftover_space -= track.base_size;
            }
        }

        let mut flex_factor_sum = CssPixels::default();
        for (index, track) in tracks.iter().enumerate() {
            if inflexible[index] {
                continue;
            }
            if let Some(factor) = track.flex_factor {
                flex_factor_sum += CssPixels::nearest_value_for(factor);
            }
        }
        if flex_factor_sum < CssPixels::from_integer(1) {
            flex_factor_sum = CssPixels::from_integer(1);
        }
        let hypothetical_fr_size = leftover_space.div_as_fraction(flex_factor_sum);

        let mut restart = false;
        for (index, track) in tracks.iter().enumerate() {
            if inflexible[index] {
                continue;
            }
            let Some(factor) = track.flex_factor else {
                continue;
            };
            let scaled = CssPixels::nearest_value_for(factor) * hypothetical_fr_size;
            if scaled < track.base_size {
                inflexible[index] = true;
                restart = true;
            }
        }
        if !restart {
            return hypothetical_fr_size;
        }
    }
}

pub(crate) fn expand_flexible_tracks(tracks: &mut [Track], space_to_fill: CssPixels) {
    let flex_fraction = find_fr_size(tracks, space_to_fill);
    for track in tracks {
        let Some(factor) = track.flex_factor else {
            continue;
        };
        let scaled = CssPixels::nearest_value_for(factor) * flex_fraction;
        if scaled > track.base_size {
            track.base_size = scaled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn fr_distribution_respects_fixed_tracks_and_factor_ratios() {
        let mut tracks = [
            Track::fixed(px(100)),
            Track::flexible(px(0), 1.0),
            Track::flexible(px(0), 2.0),
        ];
        expand_flexible_tracks(&mut tracks, px(400));
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(100));
        assert_eq!(tracks[2].base_size, px(200));
    }

    #[test]
    fn fr_distribution_restarts_with_base_size_violations_inflexible() {
        let mut tracks = [
            Track::fixed(px(100)),
            Track::flexible(px(150), 1.0),
            Track::flexible(px(0), 2.0),
        ];
        expand_flexible_tracks(&mut tracks, px(400));
        assert_eq!(tracks[1].base_size, px(150));
        assert_eq!(tracks[2].base_size, px(150));
    }

    #[test]
    fn sub_one_flex_sum_leaves_the_remaining_fraction_unfilled() {
        let mut tracks = [Track::flexible(px(0), 0.5)];
        expand_flexible_tracks(&mut tracks, px(100));
        assert_eq!(tracks[0].base_size, px(50));
    }
}
