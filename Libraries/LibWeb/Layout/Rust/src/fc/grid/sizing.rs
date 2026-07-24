/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::tracks::{PixelFraction, Track, TrackSizingFunction, find_fr_size, initialize_track_sizes};
use crate::css_pixels::CssPixels;
use crate::geometry::AvailableSize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpaceDistributionPhase {
    Minimum,
    MinContent,
    MaxContent,
}

/// Distributes one spanning item's contribution into planned base-size
/// increases. The caller supplies the affected tracks in span order.
pub(crate) fn distribute_spanning_base_size(
    tracks: &mut [Track],
    affected: &[bool],
    item_size_contribution: CssPixels,
    phase: SpaceDistributionPhase,
) -> Vec<CssPixels> {
    assert_eq!(tracks.len(), affected.len());
    let affected_indices = affected
        .iter()
        .enumerate()
        .filter_map(|(index, affected)| affected.then_some(index))
        .collect::<Vec<_>>();
    let mut increases = vec![CssPixels::default(); tracks.len()];
    if affected_indices.is_empty() {
        return increases;
    }

    let spanned_size = tracks
        .iter()
        .fold(CssPixels::default(), |sum, track| sum + track.base_size);
    let mut extra_space = CssPixels::default().max(item_size_contribution - spanned_size);

    while extra_space > CssPixels::default() {
        if affected_indices.iter().all(|index| tracks[*index].base_size_frozen) {
            break;
        }
        let increase_per_track = CssPixels::from_raw(1).max(extra_space / affected_indices.len());
        for &index in &affected_indices {
            if tracks[index].base_size_frozen {
                continue;
            }
            let mut increase = increase_per_track.min(extra_space);
            if let Some(growth_limit) = tracks[index].growth_limit {
                let maximum_increase = growth_limit - tracks[index].base_size;
                if increases[index] + increase >= maximum_increase {
                    tracks[index].base_size_frozen = true;
                    increase = maximum_increase - increases[index];
                }
            }
            increases[index] += increase;
            extra_space -= increase;
        }
    }

    if extra_space > CssPixels::default() {
        let mut beyond_limits = affected_indices
            .iter()
            .copied()
            .filter(|index| match phase {
                SpaceDistributionPhase::Minimum | SpaceDistributionPhase::MinContent => tracks[*index].max_is_intrinsic,
                SpaceDistributionPhase::MaxContent => tracks[*index].max_is_max_content,
            })
            .collect::<Vec<_>>();
        if beyond_limits.is_empty() {
            beyond_limits.clone_from(&affected_indices);
        }

        let increase_per_track = extra_space / beyond_limits.len();
        for index in beyond_limits {
            let increase = increase_per_track.min(extra_space);
            increases[index] += increase;
            extra_space -= increase;
        }
    }

    increases
}

pub(crate) fn apply_base_size_increases(tracks: &mut [Track], planned_increases: &[CssPixels]) {
    assert_eq!(tracks.len(), planned_increases.len());
    for (track, increase) in tracks.iter_mut().zip(planned_increases) {
        track.base_size += *increase;
        if track
            .growth_limit
            .is_some_and(|growth_limit| growth_limit < track.base_size)
        {
            track.growth_limit = Some(track.base_size);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ItemContribution {
    /// Indices into the axis's interleaved track-and-gap array, in span order.
    pub(crate) spanned_tracks: Vec<usize>,
    pub(crate) span: usize,
    pub(crate) minimum: CssPixels,
    pub(crate) min_content: CssPixels,
    pub(crate) limited_min_content: CssPixels,
    pub(crate) max_content: CssPixels,
    pub(crate) limited_max_content: CssPixels,
    pub(crate) is_scroll_container: bool,
}

fn distribute_growth_limit(tracks: &mut [Track], spanned: &[usize], affected: &[usize], contribution: CssPixels) {
    if affected.is_empty() {
        return;
    }
    for &index in affected {
        tracks[index].item_incurred_increase = CssPixels::default();
    }
    let accounted = spanned.iter().fold(CssPixels::default(), |sum, index| {
        sum + tracks[*index].growth_limit.unwrap_or(tracks[*index].base_size)
    });
    let mut extra = CssPixels::default().max(contribution - accounted);
    while extra > CssPixels::default() {
        if affected.iter().all(|index| tracks[*index].growth_limit_frozen) {
            break;
        }
        let per_track = CssPixels::from_raw(1).max(extra / affected.len());
        for &index in affected {
            if tracks[index].growth_limit_frozen {
                continue;
            }
            let mut increase = per_track.min(extra);
            if !tracks[index].infinitely_growable
                && let Some(limit) = tracks[index].growth_limit
            {
                let maximum = limit - tracks[index].base_size;
                if tracks[index].item_incurred_increase + increase >= maximum {
                    tracks[index].growth_limit_frozen = true;
                    increase = maximum - tracks[index].item_incurred_increase;
                }
            }
            tracks[index].item_incurred_increase += increase;
            extra -= increase;
        }
    }
    // The C++ source intentionally leaves spec step 3 (growth beyond limits)
    // as a FIXME. Keep that omission.
    for &index in spanned {
        tracks[index].planned_increase = tracks[index].planned_increase.max(tracks[index].item_incurred_increase);
    }
}

fn distribute_base_for_item(
    tracks: &mut [Track],
    spanned: &[usize],
    contribution: CssPixels,
    phase: SpaceDistributionPhase,
    matcher: impl Fn(&Track) -> bool,
) {
    let mut local = spanned.iter().map(|index| tracks[*index]).collect::<Vec<_>>();
    let affected = local.iter().map(matcher).collect::<Vec<_>>();
    let increases = distribute_spanning_base_size(&mut local, &affected, contribution, phase);
    for (position, ((&index, increase), local_track)) in spanned.iter().zip(increases).zip(local).enumerate() {
        tracks[index].base_size_frozen = local_track.base_size_frozen;
        if affected[position] {
            // C++ stores this scratch value on the track and only clears
            // affected tracks in each distribution phase. Growth-limit
            // distribution later reads every spanned track, so stale values
            // on unaffected tracks observably leak into their limits.
            tracks[index].item_incurred_increase = increase;
        }
        tracks[index].planned_increase = tracks[index].planned_increase.max(increase);
    }
}

fn apply_planned_base_increases(tracks: &mut [Track], spanned: &[usize]) {
    for &index in spanned {
        tracks[index].base_size += tracks[index].planned_increase;
        tracks[index].planned_increase = CssPixels::default();
    }
}

fn grow_content_sized_tracks_for_item(tracks: &mut [Track], item: &ItemContribution, available: AvailableSize) {
    let spanned = &item.spanned_tracks;
    let has_flexible = spanned
        .iter()
        .any(|index| tracks[*index].max_sizing.flex_factor().is_some());
    let has_intrinsic = spanned.iter().any(|index| {
        tracks[*index].min_sizing.is_intrinsic(available) || tracks[*index].max_sizing.is_intrinsic(available)
    });
    if !has_intrinsic || has_flexible {
        return;
    }

    let minimum = if available.is_intrinsic_sizing_constraint() {
        item.limited_min_content
    } else {
        item.minimum
    };
    distribute_base_for_item(tracks, spanned, minimum, SpaceDistributionPhase::Minimum, |track| {
        track.min_sizing.is_intrinsic(available)
    });
    apply_planned_base_increases(tracks, spanned);

    distribute_base_for_item(
        tracks,
        spanned,
        item.min_content,
        SpaceDistributionPhase::MinContent,
        |track| track.min_sizing.is_min_content() || track.min_sizing.is_max_content(),
    );
    apply_planned_base_increases(tracks, spanned);

    if available.is_max_content() {
        distribute_base_for_item(
            tracks,
            spanned,
            item.limited_max_content,
            SpaceDistributionPhase::MaxContent,
            |track| track.min_sizing.is_auto(available) || track.min_sizing.is_max_content(),
        );
        apply_planned_base_increases(tracks, spanned);
    }

    for track in tracks.iter_mut() {
        // C++ performs this step over m_grid_{rows,columns}, not the
        // interleaved gutter list. A gap intentionally keeps its zero growth
        // limit even when its fixed base size is non-zero.
        if !track.is_gap && track.growth_limit.is_some_and(|limit| limit < track.base_size) {
            track.growth_limit = Some(track.base_size);
        }
    }

    let affected = spanned
        .iter()
        .copied()
        .filter(|index| tracks[*index].max_sizing.is_intrinsic(available))
        .collect::<Vec<_>>();
    distribute_growth_limit(tracks, spanned, &affected, item.min_content);
    for &index in spanned {
        if tracks[index].growth_limit.is_none() {
            tracks[index].growth_limit = Some(tracks[index].base_size + tracks[index].planned_increase);
            tracks[index].infinitely_growable = true;
        } else {
            tracks[index].growth_limit = Some(tracks[index].growth_limit.unwrap() + tracks[index].planned_increase);
        }
        tracks[index].planned_increase = CssPixels::default();
    }

    let affected = spanned
        .iter()
        .copied()
        .filter(|index| {
            tracks[*index].max_sizing.is_max_content()
                || tracks[*index].max_sizing.is_auto(available)
                || tracks[*index].max_sizing.is_fit_content()
        })
        .collect::<Vec<_>>();
    distribute_growth_limit(tracks, spanned, &affected, item.max_content);
    for &index in spanned {
        let increase = tracks[index].planned_increase;
        if let TrackSizingFunction::FitContent(_) = tracks[index].max_sizing {
            let mut limit = tracks[index].growth_limit.unwrap() + increase;
            limit = limit.max(tracks[index].base_size);
            let fit_limit = tracks[index].max_sizing.resolve(available);
            if limit > fit_limit {
                limit = tracks[index].base_size.max(fit_limit);
            }
            tracks[index].growth_limit = Some(limit);
        } else if tracks[index].growth_limit.is_none() {
            tracks[index].growth_limit = Some(tracks[index].base_size + increase);
        } else {
            tracks[index].growth_limit = Some(tracks[index].growth_limit.unwrap() + increase);
        }
        tracks[index].planned_increase = CssPixels::default();
    }
}

pub(crate) fn resolve_intrinsic_track_sizes(
    tracks: &mut [Track],
    items: &[ItemContribution],
    available: AvailableSize,
    row_axis: bool,
) {
    let max_span = items.iter().map(|item| item.span).max().unwrap_or(1).max(1);
    for span in 1..=max_span {
        for item in items.iter().filter(|item| item.span == span) {
            grow_content_sized_tracks_for_item(tracks, item, available);
        }
    }

    let dominated = |track: &Track| {
        available.is_max_content()
            || (row_axis && available.is_min_content())
            || track.min_sizing.is_intrinsic(available)
    };
    let mut contributions = vec![CssPixels::default(); tracks.len()];
    for item in items {
        let mut total_flex = 0.0;
        let mut flexible_count = 0usize;
        let mut non_flexible_space = CssPixels::default();
        for &index in &item.spanned_tracks {
            if let Some(factor) = tracks[index].max_sizing.flex_factor()
                && dominated(&tracks[index])
            {
                total_flex += factor;
                flexible_count += 1;
            } else {
                non_flexible_space += tracks[index].base_size;
            }
        }
        if flexible_count == 0 {
            continue;
        }
        let mut contribution = if available.is_intrinsic_sizing_constraint() {
            if total_flex == 0.0 && item.is_scroll_container {
                item.minimum
            } else {
                item.limited_min_content
            }
        } else {
            item.minimum
        };
        contribution = CssPixels::default().max(contribution - non_flexible_space);
        for &index in &item.spanned_tracks {
            let Some(factor) = tracks[index].max_sizing.flex_factor() else {
                continue;
            };
            if !dominated(&tracks[index]) {
                continue;
            }
            let share = if total_flex > 0.0 {
                CssPixels::nearest_value_for(contribution.to_double() * (factor / total_flex))
            } else {
                contribution / flexible_count
            };
            contributions[index] = contributions[index].max(share);
        }
    }
    for (track, contribution) in tracks.iter_mut().zip(contributions) {
        track.base_size = track.base_size.max(contribution);
        if track.growth_limit.is_some_and(|limit| limit < track.base_size) {
            track.growth_limit = Some(track.base_size);
        }
    }

    for track in tracks {
        if track.growth_limit.is_none() {
            track.growth_limit = Some(track.base_size);
        }
    }
}

pub(crate) fn maximize_tracks(tracks: &mut [Track], gap_size: CssPixels, available: AvailableSize) {
    let mut free_space = match available.type_ {
        crate::geometry::AvailableSizeType::Definite => {
            let track_sum = tracks.iter().fold(gap_size, |sum, track| sum + track.base_size);
            CssPixels::default().max(available.value - track_sum)
        }
        crate::geometry::AvailableSizeType::MinContent => CssPixels::default(),
        crate::geometry::AvailableSizeType::MaxContent | crate::geometry::AvailableSizeType::Indefinite => {
            CssPixels::from_raw(i32::MAX)
        }
    };
    let mut growable = tracks
        .iter()
        .filter(|track| !track.base_size_frozen && track.growth_limit.is_some_and(|limit| track.base_size < limit))
        .count();
    while free_space > CssPixels::default() && growable > 0 {
        let per_track = free_space / growable;
        let old_free_space = free_space;
        for track in tracks.iter_mut() {
            if track.base_size_frozen {
                continue;
            }
            let Some(limit) = track.growth_limit else {
                continue;
            };
            if track.base_size >= limit {
                continue;
            }
            track.base_size = limit.min(track.base_size + per_track);
            if track.base_size >= limit {
                growable -= 1;
            }
        }
        free_space = match available.type_ {
            crate::geometry::AvailableSizeType::Definite => {
                let sum = tracks.iter().fold(gap_size, |sum, track| sum + track.base_size);
                CssPixels::default().max(available.value - sum)
            }
            crate::geometry::AvailableSizeType::MinContent => CssPixels::default(),
            _ => CssPixels::from_raw(i32::MAX),
        };
        if free_space == old_free_space {
            break;
        }
    }
}

pub(crate) fn expand_flexible_tracks_indefinite(tracks: &mut [Track], items: &[ItemContribution]) {
    let mut flex_fraction = PixelFraction::zero();
    for track in tracks.iter() {
        if let Some(factor) = track.flex_factor {
            let divisor = CssPixels::nearest_value_for(factor.max(1.0));
            flex_fraction = flex_fraction.max(PixelFraction::new(track.base_size, divisor));
        }
    }
    for item in items {
        if !item
            .spanned_tracks
            .iter()
            .any(|index| tracks[*index].flex_factor.is_some())
        {
            continue;
        }
        let local = item
            .spanned_tracks
            .iter()
            .map(|index| tracks[*index])
            .collect::<Vec<_>>();
        flex_fraction = flex_fraction.max(find_fr_size(&local, item.max_content));
    }
    for track in tracks {
        if let Some(factor) = track.flex_factor {
            track.base_size = track
                .base_size
                .max(flex_fraction.multiply(CssPixels::nearest_value_for(factor)));
        }
    }
}

pub(crate) fn stretch_auto_tracks(
    tracks: &mut [Track],
    gap_size: CssPixels,
    available: AvailableSize,
    content_distribution_is_normal_or_stretch: bool,
) {
    if !content_distribution_is_normal_or_stretch {
        return;
    }
    let auto_count = tracks
        .iter()
        .filter(|track| track.max_sizing.is_auto(available))
        .count();
    if auto_count == 0 {
        return;
    }
    let remaining = if available.is_definite() {
        let sum = tracks.iter().fold(gap_size, |sum, track| sum + track.base_size);
        CssPixels::default().max(available.value - sum)
    } else {
        CssPixels::default()
    };
    let per_track = remaining / auto_count;
    for track in tracks {
        if track.max_sizing.is_auto(available) {
            track.base_size += per_track;
        }
    }
}

pub(crate) fn run_track_sizing(
    tracks: &mut [Track],
    gap_size: CssPixels,
    items: &[ItemContribution],
    available: AvailableSize,
    row_axis: bool,
    content_distribution_is_normal_or_stretch: bool,
) {
    let has_flexible = initialize_track_sizes(tracks, available);
    resolve_intrinsic_track_sizes(tracks, items, available, row_axis);
    maximize_tracks(tracks, gap_size, available);
    if has_flexible {
        if available.is_definite() {
            super::tracks::expand_flexible_tracks(tracks, available.value - gap_size);
        } else if !available.is_min_content() {
            expand_flexible_tracks_indefinite(tracks, items);
        }
    }
    stretch_auto_tracks(tracks, gap_size, available, content_distribution_is_normal_or_stretch);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn spanning_item_distributes_in_span_order_and_grows_past_limits() {
        let mut tracks = [
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::Fixed(crate::style_facts::FfiSizeValue::auto_value()),
                base_size: px(50),
                growth_limit: Some(px(100)),
                flex_factor: None,
                max_is_intrinsic: false,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::MaxContent,
                base_size: px(50),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
        ];
        let increases =
            distribute_spanning_base_size(&mut tracks, &[true, true], px(300), SpaceDistributionPhase::Minimum);
        assert_eq!(increases, vec![px(50), px(150)]);
        apply_base_size_increases(&mut tracks, &increases);
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(200));
    }

    #[test]
    fn unaffected_tracks_still_reduce_the_remaining_contribution() {
        let mut tracks = [
            Track::fixed(px(80)),
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::MaxContent,
                base_size: px(20),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
        ];
        let increases =
            distribute_spanning_base_size(&mut tracks, &[false, true], px(160), SpaceDistributionPhase::MinContent);
        assert_eq!(increases, vec![px(0), px(60)]);
    }

    #[test]
    fn full_order_sizes_spanning_intrinsic_tracks_before_maximize_and_stretch() {
        let mut tracks = [Track::auto(), Track::auto()];
        let item = ItemContribution {
            spanned_tracks: vec![0, 1],
            span: 2,
            minimum: px(120),
            min_content: px(120),
            limited_min_content: px(120),
            max_content: px(160),
            limited_max_content: px(160),
            is_scroll_container: false,
        };
        run_track_sizing(
            &mut tracks,
            px(10),
            &[item],
            AvailableSize::definite(px(210)),
            false,
            true,
        );
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(100));
    }

    #[test]
    fn indefinite_fr_fraction_uses_spanning_item_max_content() {
        let mut tracks = [Track::flexible(px(0), 1.0), Track::flexible(px(0), 2.0)];
        let item = ItemContribution {
            spanned_tracks: vec![0, 1],
            span: 2,
            minimum: px(0),
            min_content: px(0),
            limited_min_content: px(0),
            max_content: px(300),
            limited_max_content: px(300),
            is_scroll_container: false,
        };
        initialize_track_sizes(&mut tracks, AvailableSize::indefinite());
        expand_flexible_tracks_indefinite(&mut tracks, &[item]);
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(200));
    }

    #[test]
    fn percentage_fit_content_becomes_max_content_when_indefinite() {
        use super::super::facts::{FfiGridTrackBreadth, FfiGridTrackBreadthKind};
        use super::super::template::TrackDefinition;
        use crate::style_facts::{FfiSizeKind, FfiSizeValue};

        let percentage = FfiSizeValue {
            kind: FfiSizeKind::Percentage as u8,
            px: px(0),
            fraction: 0.5,
            calc: std::ptr::null(),
            contains_percentage: true,
            contains_anchor_function: false,
        };
        let auto = FfiGridTrackBreadth {
            kind: FfiGridTrackBreadthKind::Auto as u8,
            value: FfiSizeValue::auto_value(),
            flex_factor: 0.0,
        };
        let mut tracks = [Track::from_definition(TrackDefinition {
            min: auto,
            max: FfiGridTrackBreadth {
                kind: FfiGridTrackBreadthKind::FitContent as u8,
                value: percentage,
                flex_factor: 0.0,
            },
            is_auto_fit: false,
            is_auto_repeat: false,
        })];
        initialize_track_sizes(&mut tracks, AvailableSize::indefinite());
        assert!(tracks[0].max_sizing.is_max_content());
    }
}
