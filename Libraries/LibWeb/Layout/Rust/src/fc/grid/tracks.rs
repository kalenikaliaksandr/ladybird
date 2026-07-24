/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::facts::{FfiGridTrackBreadth, FfiGridTrackBreadthKind};
use super::template::TrackDefinition;
use crate::css_pixels::CssPixels;
use crate::geometry::AvailableSize;
use crate::style_facts::FfiSizeValue;

#[derive(Clone, Copy, Debug)]
pub(crate) enum TrackSizingFunction {
    Auto,
    Fixed(FfiSizeValue),
    Flex(f64),
    MinContent,
    MaxContent,
    FitContent(FfiSizeValue),
}

impl TrackSizingFunction {
    pub(crate) fn from_ffi(value: FfiGridTrackBreadth) -> Self {
        match value.kind {
            kind if kind == FfiGridTrackBreadthKind::Auto as u8 => Self::Auto,
            kind if kind == FfiGridTrackBreadthKind::LengthPercentage as u8 => Self::Fixed(value.value),
            kind if kind == FfiGridTrackBreadthKind::Flex as u8 => Self::Flex(value.flex_factor),
            kind if kind == FfiGridTrackBreadthKind::MinContent as u8 => Self::MinContent,
            kind if kind == FfiGridTrackBreadthKind::MaxContent as u8 => Self::MaxContent,
            kind if kind == FfiGridTrackBreadthKind::FitContent as u8 => Self::FitContent(value.value),
            _ => unreachable!("invalid grid track breadth"),
        }
    }

    pub(crate) fn is_auto(self, available: AvailableSize) -> bool {
        match self {
            Self::Auto => true,
            Self::Fixed(value) => value.contains_percentage && !available.is_definite(),
            _ => false,
        }
    }

    pub(crate) fn is_fixed(self, available: AvailableSize) -> bool {
        matches!(self, Self::Fixed(value) if !value.contains_percentage || available.is_definite())
    }

    pub(crate) fn is_intrinsic(self, available: AvailableSize) -> bool {
        self.is_auto(available) || matches!(self, Self::MinContent | Self::MaxContent | Self::FitContent(_))
    }

    pub(crate) fn is_min_content(self) -> bool {
        matches!(self, Self::MinContent)
    }

    pub(crate) fn is_max_content(self) -> bool {
        matches!(self, Self::MaxContent)
    }

    pub(crate) fn is_fit_content(self) -> bool {
        matches!(self, Self::FitContent(_))
    }

    pub(crate) fn flex_factor(self) -> Option<f64> {
        match self {
            Self::Flex(factor) => Some(factor),
            _ => None,
        }
    }

    pub(crate) fn resolve(self, available: AvailableSize) -> CssPixels {
        match self {
            Self::Fixed(value) | Self::FitContent(value) => value.to_px(available.to_px_or_zero()),
            _ => CssPixels::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Track {
    pub(crate) min_sizing: TrackSizingFunction,
    pub(crate) max_sizing: TrackSizingFunction,
    pub(crate) base_size: CssPixels,
    pub(crate) growth_limit: Option<CssPixels>,
    pub(crate) flex_factor: Option<f64>,
    pub(crate) max_is_intrinsic: bool,
    pub(crate) max_is_max_content: bool,
    pub(crate) base_size_frozen: bool,
    pub(crate) growth_limit_frozen: bool,
    pub(crate) infinitely_growable: bool,
    pub(crate) planned_increase: CssPixels,
    pub(crate) item_incurred_increase: CssPixels,
    pub(crate) is_gap: bool,
    pub(crate) is_auto_fit: bool,
    pub(crate) is_auto_repeat: bool,
    pub(crate) is_collapsed: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PixelFraction {
    numerator: CssPixels,
    denominator: CssPixels,
}

impl PixelFraction {
    pub(crate) fn new(numerator: CssPixels, denominator: CssPixels) -> Self {
        assert_ne!(denominator, CssPixels::default());
        Self { numerator, denominator }
    }

    pub(crate) fn zero() -> Self {
        Self::new(CssPixels::default(), CssPixels::from_integer(1))
    }

    pub(crate) fn multiply(self, value: CssPixels) -> CssPixels {
        let wide = value.raw_value() as i64 * self.numerator.raw_value() as i64;
        CssPixels::from_raw((wide / self.denominator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }

    pub(crate) fn max(self, other: Self) -> Self {
        let left = self.numerator.raw_value() as i64 * other.denominator.raw_value() as i64;
        let right = other.numerator.raw_value() as i64 * self.denominator.raw_value() as i64;
        if left >= right { self } else { other }
    }
}

impl Track {
    pub(crate) fn fixed(base_size: CssPixels) -> Self {
        let value = fixed_size_value(base_size);
        Self {
            min_sizing: TrackSizingFunction::Fixed(value),
            max_sizing: TrackSizingFunction::Fixed(value),
            base_size,
            growth_limit: Some(base_size),
            flex_factor: None,
            max_is_intrinsic: false,
            max_is_max_content: false,
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: false,
            is_auto_repeat: false,
            is_collapsed: false,
        }
    }

    pub(crate) fn flexible(base_size: CssPixels, flex_factor: f64) -> Self {
        Self {
            min_sizing: TrackSizingFunction::Auto,
            max_sizing: TrackSizingFunction::Flex(flex_factor),
            base_size,
            growth_limit: None,
            flex_factor: Some(flex_factor),
            max_is_intrinsic: false,
            max_is_max_content: false,
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: false,
            is_auto_repeat: false,
            is_collapsed: false,
        }
    }

    pub(crate) fn auto() -> Self {
        Self {
            min_sizing: TrackSizingFunction::Auto,
            max_sizing: TrackSizingFunction::Auto,
            base_size: CssPixels::default(),
            growth_limit: Some(CssPixels::default()),
            flex_factor: None,
            max_is_intrinsic: true,
            max_is_max_content: false,
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: false,
            is_auto_repeat: false,
            is_collapsed: false,
        }
    }

    pub(crate) fn from_definition(definition: TrackDefinition) -> Self {
        let min_sizing = TrackSizingFunction::from_ffi(definition.min);
        let max_sizing = TrackSizingFunction::from_ffi(definition.max);
        Self {
            min_sizing,
            max_sizing,
            base_size: CssPixels::default(),
            growth_limit: Some(CssPixels::default()),
            flex_factor: max_sizing.flex_factor(),
            max_is_intrinsic: false,
            max_is_max_content: max_sizing.is_max_content(),
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: definition.is_auto_fit,
            is_auto_repeat: definition.is_auto_repeat,
            is_collapsed: false,
        }
    }

    pub(crate) fn gap(size: CssPixels) -> Self {
        let mut track = Self::fixed(size);
        // GridTrack::create_gap leaves the growth limit at its default zero,
        // and initialization deliberately skips gaps. Keep that observable
        // C++ arithmetic even though the gap has a non-zero base size.
        track.growth_limit = Some(CssPixels::default());
        track.is_gap = true;
        track
    }

    pub(crate) fn collapse(&mut self) {
        let zero = fixed_size_value(CssPixels::default());
        self.min_sizing = TrackSizingFunction::Fixed(zero);
        self.max_sizing = TrackSizingFunction::Fixed(zero);
        self.flex_factor = None;
        self.is_collapsed = true;
    }
}

fn fixed_size_value(value: CssPixels) -> FfiSizeValue {
    use crate::style_facts::FfiSizeKind;
    FfiSizeValue {
        kind: FfiSizeKind::Px as u8,
        px: value,
        fraction: 0.0,
        calc: std::ptr::null(),
        contains_percentage: false,
        contains_anchor_function: false,
    }
}

pub(crate) fn initialize_track_sizes(tracks: &mut [Track], available: AvailableSize) -> bool {
    let mut has_flexible_tracks = false;
    for track in tracks {
        track.base_size_frozen = false;
        track.growth_limit_frozen = false;
        track.infinitely_growable = false;
        track.planned_increase = CssPixels::default();
        track.item_incurred_increase = CssPixels::default();
        if track.is_gap {
            continue;
        }

        if !available.is_definite()
            && matches!(track.max_sizing, TrackSizingFunction::FitContent(value) if value.contains_percentage)
        {
            track.max_sizing = TrackSizingFunction::MaxContent;
        }

        if track.min_sizing.is_fixed(available) {
            track.base_size = track.min_sizing.resolve(available);
        } else if track.min_sizing.is_intrinsic(available) {
            track.base_size = CssPixels::default();
        } else {
            unreachable!("flexible values cannot be minimum track sizing functions");
        }

        if track.max_sizing.is_fixed(available) {
            track.growth_limit = Some(track.max_sizing.resolve(available));
        } else if let Some(factor) = track.max_sizing.flex_factor() {
            has_flexible_tracks = true;
            track.flex_factor = Some(factor);
            track.growth_limit = None;
        } else if track.max_sizing.is_intrinsic(available) {
            track.growth_limit = None;
        } else {
            unreachable!("invalid maximum track sizing function");
        }
        track.max_is_intrinsic = track.max_sizing.is_intrinsic(available);
        track.max_is_max_content = track.max_sizing.is_max_content();
        if track.growth_limit.is_some_and(|limit| limit < track.base_size) {
            track.growth_limit = Some(track.base_size);
        }
    }
    has_flexible_tracks
}

/// The "find the size of an fr" sub-algorithm, preserving the C++ operation
/// order and its conversion of every flex factor through `CSSPixels`.
pub(crate) fn find_fr_size(tracks: &[Track], space_to_fill: CssPixels) -> PixelFraction {
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
        let hypothetical_fr_size = PixelFraction::new(leftover_space, flex_factor_sum);

        let mut restart = false;
        for (index, track) in tracks.iter().enumerate() {
            if inflexible[index] {
                continue;
            }
            let Some(factor) = track.flex_factor else {
                continue;
            };
            let scaled = hypothetical_fr_size.multiply(CssPixels::nearest_value_for(factor));
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
        let scaled = flex_fraction.multiply(CssPixels::nearest_value_for(factor));
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
