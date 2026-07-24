/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::tracks::Track;
use crate::css_pixels::CssPixels;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpaceDistributionPhase {
    Minimum,
    MinContent,
    MaxContent,
}

/// Distributes one spanning item's contribution into planned base-size
/// increases. The caller supplies the affected tracks in span order.
pub(crate) fn distribute_spanning_base_size(
    tracks: &[Track],
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
    let mut frozen = vec![false; tracks.len()];

    while extra_space > CssPixels::default() {
        if affected_indices.iter().all(|index| frozen[*index]) {
            break;
        }
        let increase_per_track = CssPixels::from_raw(1).max(extra_space / affected_indices.len());
        for &index in &affected_indices {
            if frozen[index] {
                continue;
            }
            let mut increase = increase_per_track.min(extra_space);
            if let Some(growth_limit) = tracks[index].growth_limit {
                let maximum_increase = growth_limit - tracks[index].base_size;
                if increases[index] + increase >= maximum_increase {
                    frozen[index] = true;
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
                base_size: px(50),
                growth_limit: Some(px(100)),
                flex_factor: None,
                max_is_intrinsic: false,
                max_is_max_content: false,
            },
            Track {
                base_size: px(50),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
            },
        ];
        let increases = distribute_spanning_base_size(&tracks, &[true, true], px(300), SpaceDistributionPhase::Minimum);
        assert_eq!(increases, vec![px(50), px(150)]);
        apply_base_size_increases(&mut tracks, &increases);
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(200));
    }

    #[test]
    fn unaffected_tracks_still_reduce_the_remaining_contribution() {
        let tracks = [
            Track::fixed(px(80)),
            Track {
                base_size: px(20),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
            },
        ];
        let increases =
            distribute_spanning_base_size(&tracks, &[false, true], px(160), SpaceDistributionPhase::MinContent);
        assert_eq!(increases, vec![px(0), px(60)]);
    }
}
