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

    // 1. Find the space to distribute:
    let spanned_size = tracks
        .iter()
        .fold(CssPixels::default(), |sum, track| sum + track.base_size);
    // Subtract the corresponding size of every spanned track from the item’s size contribution to find the item’s
    // remaining size contribution.
    let mut extra_space = CssPixels::default().max(item_size_contribution - spanned_size);

    // 2. Distribute space up to limits:
    while extra_space > CssPixels::default() {
        if affected_indices.iter().all(|index| tracks[*index].base_size_frozen) {
            break;
        }
        // Find the item-incurred increase for each spanned track with an affected size by: distributing the space
        // equally among such tracks, freezing a track’s item-incurred increase as its affected size + item-incurred
        // increase reaches its limit
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

    // 3. Distribute space beyond limits
    if extra_space > CssPixels::default() {
        // If space remains after all tracks are frozen, unfreeze and continue to
        // distribute space to the item-incurred increase of...
        let mut beyond_limits = affected_indices
            .iter()
            .copied()
            .filter(|index| match phase {
                // when accommodating minimum contributions or accommodating min-content contributions: any affected track
                // that happens to also have an intrinsic max track sizing function
                SpaceDistributionPhase::Minimum | SpaceDistributionPhase::MinContent => tracks[*index].max_is_intrinsic,
                // when accommodating max-content contributions into base sizes: any affected track that happens to also have
                // a max-content max track sizing function;
                SpaceDistributionPhase::MaxContent => tracks[*index].max_is_max_content,
            })
            .collect::<Vec<_>>();
        if beyond_limits.is_empty() {
            // if there are no such tracks, then all affected tracks.
            beyond_limits.clone_from(&affected_indices);
        }

        let increase_per_track = extra_space / beyond_limits.len();
        for index in beyond_limits {
            let increase = increase_per_track.min(extra_space);
            increases[index] += increase;
            extra_space -= increase;
        }
    }

    // 4. For each affected track, if the track’s item-incurred increase is larger than the track’s planned increase
    //    set the track’s planned increase to that value.
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
    // 1. Find the space to distribute:
    let accounted = spanned.iter().fold(CssPixels::default(), |sum, index| {
        sum + tracks[*index].growth_limit.unwrap_or(tracks[*index].base_size)
    });
    // Subtract the corresponding size of every spanned track from the item’s size contribution to find the item’s
    // remaining size contribution.
    let mut extra = CssPixels::default().max(contribution - accounted);
    // 2. Distribute space up to limits:
    while extra > CssPixels::default() {
        if affected.iter().all(|index| tracks[*index].growth_limit_frozen) {
            break;
        }
        // Find the item-incurred increase for each spanned track with an affected size by: distributing the space
        // equally among such tracks, freezing a track’s item-incurred increase as its affected size + item-incurred
        // increase reaches its limit
        let per_track = CssPixels::from_raw(1).max(extra / affected.len());
        for &index in affected {
            if tracks[index].growth_limit_frozen {
                continue;
            }
            let mut increase = per_track.min(extra);
            if !tracks[index].infinitely_growable
                && let Some(limit) = tracks[index].growth_limit
            {
                // For growth limits, the limit is infinity if it is marked as infinitely growable, and equal to the
                // growth limit otherwise.
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
    // FIXME: 3. Distribute space beyond limits
    // The C++ source intentionally leaves spec step 3 (growth beyond limits)
    // as a FIXME. Keep that omission.
    // 4. For each affected track, if the track’s item-incurred increase is larger than the track’s planned increase
    //    set the track’s planned increase to that value.
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

    // 1. For intrinsic minimums: First increase the base size of tracks with an intrinsic min track sizing
    //    function by distributing extra space as needed to accommodate these items’ minimum contributions.
    let minimum = if available.is_intrinsic_sizing_constraint() {
        // If the grid container is being sized under a min- or max-content constraint, use the items’ limited
        // min-content contributions in place of their minimum contributions here.
        item.limited_min_content
    } else {
        item.minimum
    };
    distribute_base_for_item(tracks, spanned, minimum, SpaceDistributionPhase::Minimum, |track| {
        track.min_sizing.is_intrinsic(available)
    });
    apply_planned_base_increases(tracks, spanned);

    // 2. For content-based minimums: Next continue to increase the base size of tracks with a min track
    //    sizing function of min-content or max-content by distributing extra space as needed to account for
    //    these items' min-content contributions.
    distribute_base_for_item(
        tracks,
        spanned,
        item.min_content,
        SpaceDistributionPhase::MinContent,
        |track| track.min_sizing.is_min_content() || track.min_sizing.is_max_content(),
    );
    apply_planned_base_increases(tracks, spanned);

    if available.is_max_content() {
        // 3. For max-content minimums: Next, if the grid container is being sized under a max-content constraint,
        //    continue to increase the base size of tracks with a min track sizing function of auto or max-content by
        //    distributing extra space as needed to account for these items' limited max-content contributions.
        distribute_base_for_item(
            tracks,
            spanned,
            item.limited_max_content,
            SpaceDistributionPhase::MaxContent,
            |track| track.min_sizing.is_auto(available) || track.min_sizing.is_max_content(),
        );
        apply_planned_base_increases(tracks, spanned);
    }

    // 4. If at this point any track’s growth limit is now less than its base size, increase its growth limit to
    //    match its base size.
    for track in tracks.iter_mut() {
        // C++ performs this step over m_grid_{rows,columns}, not the
        // interleaved gutter list. A gap intentionally keeps its zero growth
        // limit even when its fixed base size is non-zero.
        if !track.is_gap && track.growth_limit.is_some_and(|limit| limit < track.base_size) {
            track.growth_limit = Some(track.base_size);
        }
    }

    // 5. For intrinsic maximums: Next increase the growth limit of tracks with an intrinsic max track sizing
    let affected = spanned
        .iter()
        .copied()
        .filter(|index| tracks[*index].max_sizing.is_intrinsic(available))
        .collect::<Vec<_>>();
    distribute_growth_limit(tracks, spanned, &affected, item.min_content);
    for &index in spanned {
        if tracks[index].growth_limit.is_none() {
            // If the affected size is an infinite growth limit, set it to the track’s base size plus the planned increase.
            tracks[index].growth_limit = Some(tracks[index].base_size + tracks[index].planned_increase);
            // Mark any tracks whose growth limit changed from infinite to finite in this step as infinitely growable
            // for the next step.
            tracks[index].infinitely_growable = true;
        } else {
            tracks[index].growth_limit = Some(tracks[index].growth_limit.unwrap() + tracks[index].planned_increase);
        }
        tracks[index].planned_increase = CssPixels::default();
    }

    // 6. For max-content maximums: Lastly continue to increase the growth limit of tracks with a max track
    //    sizing function of max-content by distributing extra space as needed to account for these items' max-
    //    content contributions. However, limit the growth of any fit-content() tracks by their fit-content() argument.
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
            // If the affected size is an infinite growth limit, set it to the track’s base size plus the planned increase.
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
    // https://www.w3.org/TR/css-grid-2/#algo-content
    // 12.5. Resolve Intrinsic Track Sizes
    // This step resolves intrinsic track sizing functions to absolute lengths. First it resolves those
    // sizes based on items that are contained wholly within a single track. Then it gradually adds in
    // the space requirements of items that span multiple tracks, evenly distributing the extra space
    // across those tracks insofar as possible.
    //
    // FIXME: 1. Shim baseline-aligned items so their intrinsic size contributions reflect their baseline alignment.

    // 2. Size tracks to fit non-spanning items:

    // 3. Increase sizes to accommodate spanning items crossing content-sized tracks: Next, consider the
    // items with a span of 2 that do not span a track with a flexible sizing function.
    // Repeat incrementally for items with greater spans until all items have been considered.
    let max_span = items.iter().map(|item| item.span).max().unwrap_or(1).max(1);
    for span in 1..=max_span {
        for item in items.iter().filter(|item| item.span == span) {
            grow_content_sized_tracks_for_item(tracks, item, available);
        }
    }

    // 4. Increase sizes to accommodate spanning items crossing flexible tracks: Next, repeat the previous
    // step instead considering (together, rather than grouped by span size) all items that do span a
    // track with a flexible sizing function while
    //
    // https://www.w3.org/TR/css-grid-1/#algo-spanning-flex-items
    // 11.5.4. Increase sizes to accommodate spanning items crossing flexible tracks
    let dominated = |track: &Track| {
        available.is_max_content()
            || (row_axis && available.is_min_content())
            || track.min_sizing.is_intrinsic(available)
    };
    let mut contributions = vec![CssPixels::default(); tracks.len()];
    for item in items {
        // NB: This step repeats the content-sized track step, but only distributes space to flexible tracks.
        // For min-content column sizing, the later "Expand Flexible Tracks" step resolves the flex fraction
        // to zero, so fixed-min flexible columns must not grow from their items' intrinsic width here.
        // Keep min-content row sizing here so intrinsic-height grids still account for their contents.
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
        // If the grid container is being sized under a min- or max-content constraint, use the items' limited
        // min-content contributions in place of their minimum contributions here.
        let mut contribution = if available.is_intrinsic_sizing_constraint() {
            if total_flex == 0.0 && item.is_scroll_container {
                // https://drafts.csswg.org/css-grid-2/#min-size-auto
                // A grid item's automatic minimum size is zero if its computed overflow is a scrollable
                // overflow value. Preserve that zero minimum for collapsed zero-flex tracks.
                item.minimum
            } else {
                item.limited_min_content
            }
        } else {
            item.minimum
        };
        // NB: Subtract the space already accounted for by non-flexible spanned tracks (sized in 11.5.3), since only
        //     the remaining contribution needs to be distributed among flexible tracks.
        contribution = CssPixels::default().max(contribution - non_flexible_space);
        // Distributing space to flexible tracks:
        // - If the sum of the flexible sizing functions of all flexible tracks spanned by the item is greater
        //   than or equal to one, distributing space to such tracks according to the ratios of their flexible
        //   sizing functions rather than distributing space equally.
        // - If the sum is less than one, distributing that proportion of space according to the ratios of their
        //   flexible sizing functions and the rest equally.
        // FIXME: Handle 0 < total_flex < 1 case separately per spec.
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
            // If at this point any track's growth limit is now less than its base size, increase its growth limit to match
            // its base size.
            track.growth_limit = Some(track.base_size);
        }
    }

    // 5. If any track still has an infinite growth limit (because, for example, it had no items placed in
    // it or it is a flexible track), set its growth limit to its base size.
    for track in tracks {
        if track.growth_limit.is_none() {
            track.growth_limit = Some(track.base_size);
        }
    }
}

pub(crate) fn maximize_tracks(tracks: &mut [Track], gap_size: CssPixels, available: AvailableSize) {
    // https://www.w3.org/TR/css-grid-2/#algo-grow-tracks
    // 12.6. Maximize Tracks
    // https://www.w3.org/TR/css-grid-2/#algo-terms
    // free space: Equal to the available grid space minus the sum of the base sizes of all the grid
    // tracks (including gutters), floored at zero. If available grid space is indefinite, the free
    // space is indefinite as well.
    // For the purpose of this step: if sizing the grid container under a max-content constraint, the
    // free space is infinite; if sizing under a min-content constraint, the free space is zero.
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
    // If the free space is positive, distribute it equally to the base sizes of all tracks, freezing
    // tracks as they reach their growth limits (and continuing to grow the unfrozen tracks as needed).
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
    // First, find the grid’s used flex fraction:
    // Otherwise, if the free space is an indefinite length:
    // The used flex fraction is the maximum of:
    let mut flex_fraction = PixelFraction::zero();
    // For each flexible track, if the flexible track’s flex factor is greater than one, the result of dividing
    // the track’s base size by its flex factor; otherwise, the track’s base size.
    for track in tracks.iter() {
        if let Some(factor) = track.flex_factor {
            let divisor = CssPixels::nearest_value_for(factor.max(1.0));
            flex_fraction = flex_fraction.max(PixelFraction::new(track.base_size, divisor));
        }
    }
    // For each grid item that crosses a flexible track, the result of finding the size of an fr using all the
    // grid tracks that the item crosses and a space to fill of the item’s max-content contribution.
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
    // https://www.w3.org/TR/css-grid-2/#algo-stretch
    // 12.8. Stretch auto Tracks
    // This step expands tracks that have an auto max track sizing function by dividing any remaining positive,
    // definite free space equally amongst them. If the free space is indefinite, but the grid container has a
    // definite min-width/height, use that size to calculate the free space for this step instead.
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

pub(crate) fn run_track_sizing<MaximumSize>(
    tracks: &mut [Track],
    gap_size: CssPixels,
    items: &[ItemContribution],
    available: AvailableSize,
    grid_container_maximum_size: MaximumSize,
    row_axis: bool,
    content_distribution_is_normal_or_stretch: bool,
) where
    MaximumSize: FnOnce() -> Option<CssPixels>,
{
    // https://www.w3.org/TR/css-grid-2/#algo-track-sizing
    // 12.3. Track Sizing Algorithm
    //
    // 1. Initialize Track Sizes
    let has_flexible = initialize_track_sizes(tracks, available);
    // 2. Resolve Intrinsic Track Sizes
    resolve_intrinsic_track_sizes(tracks, items, available, row_axis);
    // 3. Maximize Tracks
    let saved_base_sizes = tracks
        .iter()
        .filter(|track| !track.is_gap)
        .map(|track| track.base_size)
        .collect::<Vec<_>>();
    maximize_tracks(tracks, gap_size, available);
    // If this would cause the grid to be larger than the grid container’s inner size as limited by its
    // max-width/height, then redo this step, treating the available grid space as equal to the grid
    // container’s inner size when it’s sized to its max-width/height.
    let grid_container_inner_size = tracks
        .iter()
        .filter(|track| !track.is_gap)
        .fold(CssPixels::default(), |sum, track| sum + track.base_size);
    if let Some(maximum_size) = grid_container_maximum_size()
        && grid_container_inner_size > maximum_size
    {
        for (track, saved_base_size) in tracks.iter_mut().filter(|track| !track.is_gap).zip(saved_base_sizes) {
            track.base_size = saved_base_size;
        }
        maximize_tracks(tracks, gap_size, AvailableSize::definite(maximum_size));
    }
    // 4. Expand Flexible Tracks
    if has_flexible {
        // https://drafts.csswg.org/css-grid/#algo-flex-tracks
        // 12.7. Expand Flexible Tracks
        // This step sizes flexible tracks using the largest value it can assign to an fr without exceeding
        // the available space.
        if available.is_definite() {
            // First, find the grid’s used flex fraction:
            // Otherwise, if the free space is a definite length:
            // The used flex fraction is the result of finding the size of an fr using all of the grid tracks and a space
            // to fill of the available grid space.
            super::tracks::expand_flexible_tracks(tracks, available.value - gap_size);
        // If the free space is zero or if sizing the grid container under a min-content constraint:
        // The used flex fraction is zero.
        } else if !available.is_min_content() {
            expand_flexible_tracks_indefinite(tracks, items);
        }
    }
    // 5. Expand Stretched auto Tracks
    stretch_auto_tracks(tracks, gap_size, available, content_distribution_is_normal_or_stretch);
    // If calculating the layout of a grid item in this step depends on the available space in the block
    // axis, assume the available space that it would have if any row with a definite max track sizing
    // function had that size and all other rows were infinite. If both the grid container and all
    // tracks have definite sizes, also apply align-content to find the final effective size of any gaps
    // spanned by such items; otherwise ignore the effects of track alignment in this estimation.
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
            || None,
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
