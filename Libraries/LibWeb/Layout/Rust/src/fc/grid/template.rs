/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::facts::{
    FfiGridArea, FfiGridTrackBreadth, FfiGridTrackBreadthKind, FfiGridTrackEntry, FfiGridTrackEntryKind,
    FfiGridTrackList, NO_GRID_INDEX,
};
use crate::style_facts::FfiSizeValue;

const REPEAT_AUTO_FIT: u8 = 0;
const REPEAT_AUTO_FILL: u8 = 1;
const REPEAT_FIXED: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineName {
    pub(crate) name_index: u32,
    pub(crate) raw: usize,
    pub(crate) implicit: bool,
    pub(crate) adopted_from_parent: bool,
    pub(crate) area_name_raw: usize,
    pub(crate) area_is_start: bool,
}

impl LineName {
    fn explicit(name_index: u32, raw: usize) -> Self {
        Self {
            name_index,
            raw,
            implicit: false,
            adopted_from_parent: false,
            area_name_raw: 0,
            area_is_start: false,
        }
    }

    fn implicit(name_index: u32, raw: usize, area_name_raw: usize, area_is_start: bool) -> Self {
        Self {
            name_index,
            raw,
            implicit: true,
            adopted_from_parent: false,
            area_name_raw,
            area_is_start,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TrackDefinition {
    pub(crate) min: FfiGridTrackBreadth,
    pub(crate) max: FfiGridTrackBreadth,
    pub(crate) is_auto_fit: bool,
    pub(crate) is_auto_repeat: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExpandedTrackList {
    pub(crate) lines: Vec<Vec<LineName>>,
    pub(crate) tracks: Vec<TrackDefinition>,
}

#[derive(Clone, Copy)]
pub(crate) struct TrackListSource<'a> {
    pub(crate) names: &'a [usize],
    pub(crate) entries: &'a [FfiGridTrackEntry],
    pub(crate) name_indices: &'a [u32],
}

impl TrackListSource<'_> {
    fn entry(&self, index: u32) -> &FfiGridTrackEntry {
        assert_ne!(index, NO_GRID_INDEX);
        &self.entries[index as usize]
    }

    fn names(&self, entry: &FfiGridTrackEntry) -> impl Iterator<Item = LineName> + '_ {
        let end = entry
            .name_index_start
            .checked_add(entry.name_index_count)
            .expect("grid line-name range overflow");
        self.name_indices[entry.name_index_start..end]
            .iter()
            .copied()
            .map(|name_index| LineName::explicit(name_index, self.names[name_index as usize]))
    }

    fn for_each_entry(&self, list: FfiGridTrackList, mut callback: impl FnMut(u32, &FfiGridTrackEntry)) {
        let mut index = list.first_entry;
        let mut visited = 0usize;
        while index != NO_GRID_INDEX {
            assert!(visited < self.entries.len(), "cyclic grid track-list snapshot");
            let entry = self.entry(index);
            callback(index, entry);
            index = entry.next_sibling;
            visited += 1;
        }
    }
}

fn auto_breadth() -> FfiGridTrackBreadth {
    FfiGridTrackBreadth {
        kind: FfiGridTrackBreadthKind::Auto as u8,
        value: FfiSizeValue::auto_value(),
        flex_factor: 0.0,
    }
}

fn definition_for(entry: &FfiGridTrackEntry, auto_fit: bool, auto_repeat: bool) -> TrackDefinition {
    match entry.kind {
        kind if kind == FfiGridTrackEntryKind::TrackSize as u8 => {
            let min = if matches!(
                entry.size.kind,
                kind if kind == FfiGridTrackBreadthKind::Flex as u8
                    || kind == FfiGridTrackBreadthKind::FitContent as u8
            ) {
                auto_breadth()
            } else {
                entry.size
            };
            TrackDefinition {
                min,
                max: entry.size,
                is_auto_fit: auto_fit,
                is_auto_repeat: auto_repeat,
            }
        }
        kind if kind == FfiGridTrackEntryKind::MinMax as u8 => TrackDefinition {
            min: entry.min_size,
            max: entry.max_size,
            is_auto_fit: auto_fit,
            is_auto_repeat: auto_repeat,
        },
        _ => unreachable!("only track-size entries form tracks"),
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_standalone_list(
    source: TrackListSource<'_>,
    list: FfiGridTrackList,
    lines: &mut Vec<Vec<LineName>>,
    tracks: &mut Vec<TrackDefinition>,
    pending_names: &mut Vec<LineName>,
    auto_repeat_count: &mut impl FnMut(u32, &FfiGridTrackEntry) -> usize,
    inherited_auto_fit: bool,
    inherited_auto_repeat: bool,
) {
    source.for_each_entry(list, |entry_index, entry| match entry.kind {
        kind if kind == FfiGridTrackEntryKind::LineNames as u8 => {
            pending_names.extend(source.names(entry));
        }
        kind if kind == FfiGridTrackEntryKind::TrackSize as u8 || kind == FfiGridTrackEntryKind::MinMax as u8 => {
            lines.push(std::mem::take(pending_names));
            tracks.push(definition_for(entry, inherited_auto_fit, inherited_auto_repeat));
        }
        kind if kind == FfiGridTrackEntryKind::Repeat as u8 => {
            let is_auto_fit = entry.repeat_type == REPEAT_AUTO_FIT;
            let is_auto_repeat = is_auto_fit || entry.repeat_type == REPEAT_AUTO_FILL;
            let repeat_count = match entry.repeat_type {
                REPEAT_FIXED => entry.repeat_count,
                REPEAT_AUTO_FIT | REPEAT_AUTO_FILL => auto_repeat_count(entry_index, entry),
                _ => unreachable!("invalid grid repeat type"),
            };
            for _ in 0..repeat_count {
                expand_standalone_list(
                    source,
                    entry.repeat_list,
                    lines,
                    tracks,
                    pending_names,
                    auto_repeat_count,
                    inherited_auto_fit || is_auto_fit,
                    inherited_auto_repeat || is_auto_repeat,
                );
            }
        }
        _ => unreachable!("invalid grid track-list entry kind"),
    });
}

/// Expands the recursive FFI encoding into the exact line/track order consumed
/// by placement. `auto_repeat_count` performs the container-size-dependent
/// auto-fill/auto-fit calculation.
pub(crate) fn expand_standalone(
    source: TrackListSource<'_>,
    list: FfiGridTrackList,
    mut auto_repeat_count: impl FnMut(u32, &FfiGridTrackEntry) -> usize,
) -> ExpandedTrackList {
    if list.is_subgrid {
        // This matches the C++ fallback for a subgrid declaration without a
        // usable parent grid.
        return ExpandedTrackList {
            lines: vec![Vec::new()],
            tracks: Vec::new(),
        };
    }

    let mut result = ExpandedTrackList::default();
    let mut pending_names = Vec::new();
    expand_standalone_list(
        source,
        list,
        &mut result.lines,
        &mut result.tracks,
        &mut pending_names,
        &mut auto_repeat_count,
        false,
        false,
    );
    result.lines.push(pending_names);
    result
}

pub(crate) fn count_subgrid_line_name_lists(source: TrackListSource<'_>, list: FfiGridTrackList) -> usize {
    let mut count = 0usize;
    source.for_each_entry(list, |_index, entry| match entry.kind {
        kind if kind == FfiGridTrackEntryKind::LineNames as u8 => count += 1,
        kind if kind == FfiGridTrackEntryKind::Repeat as u8 => {
            let nested = count_subgrid_line_name_lists(source, entry.repeat_list);
            count += if entry.repeat_type == REPEAT_FIXED {
                nested.saturating_mul(entry.repeat_count)
            } else {
                nested
            };
        }
        _ => {}
    });
    count
}

pub(crate) fn automatic_subgrid_span(source: TrackListSource<'_>, list: FfiGridTrackList) -> usize {
    count_subgrid_line_name_lists(source, list).saturating_sub(1).max(1)
}

fn expand_subgrid_names(
    source: TrackListSource<'_>,
    list: FfiGridTrackList,
    lines: &mut [Vec<LineName>],
    line_index: &mut usize,
) {
    let entries = {
        let mut entries = Vec::new();
        source.for_each_entry(list, |index, entry| entries.push((index, *entry)));
        entries
    };

    for (position, (_entry_index, entry)) in entries.iter().enumerate() {
        match entry.kind {
            kind if kind == FfiGridTrackEntryKind::LineNames as u8 => {
                if let Some(line) = lines.get_mut(*line_index) {
                    line.extend(source.names(entry));
                }
                *line_index += 1;
            }
            kind if kind == FfiGridTrackEntryKind::Repeat as u8 => {
                let repeat_count = match entry.repeat_type {
                    REPEAT_FIXED => entry.repeat_count,
                    REPEAT_AUTO_FILL => {
                        let per_repeat = count_subgrid_line_name_lists(source, entry.repeat_list);
                        let remaining = entries[position + 1..]
                            .iter()
                            .map(|(_, entry)| match entry.kind {
                                kind if kind == FfiGridTrackEntryKind::LineNames as u8 => 1,
                                kind if kind == FfiGridTrackEntryKind::Repeat as u8 => {
                                    count_subgrid_line_name_lists(source, entry.repeat_list)
                                        * if entry.repeat_type == REPEAT_FIXED {
                                            entry.repeat_count
                                        } else {
                                            1
                                        }
                                }
                                _ => 0,
                            })
                            .sum::<usize>();
                        if per_repeat > 0 && *line_index < lines.len() && lines.len() - *line_index > remaining {
                            (lines.len() - *line_index - remaining) / per_repeat
                        } else {
                            0
                        }
                    }
                    // auto-fit is invalid in a subgrid line-name list and the
                    // C++ implementation expands it zero times.
                    REPEAT_AUTO_FIT => 0,
                    _ => unreachable!("invalid grid repeat type"),
                };
                for _ in 0..repeat_count {
                    expand_subgrid_names(source, entry.repeat_list, lines, line_index);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn expand_subgrid(
    source: TrackListSource<'_>,
    list: FfiGridTrackList,
    track_count: usize,
    inherited_lines: &[Vec<LineName>],
) -> ExpandedTrackList {
    let mut lines = vec![Vec::new(); track_count.saturating_add(1)];
    for (line, inherited) in lines.iter_mut().zip(inherited_lines) {
        line.extend(inherited.iter().filter(|name| !name.implicit).map(|name| LineName {
            name_index: name.name_index,
            raw: name.raw,
            implicit: false,
            adopted_from_parent: true,
            area_name_raw: 0,
            area_is_start: false,
        }));
    }
    let mut line_index = 0;
    expand_subgrid_names(source, list, &mut lines, &mut line_index);
    ExpandedTrackList {
        lines,
        tracks: Vec::new(),
    }
}

pub(crate) fn add_template_area_lines(
    columns: &mut Vec<Vec<LineName>>,
    rows: &mut Vec<Vec<LineName>>,
    areas: &[FfiGridArea],
    names: &[usize],
) {
    let max_column = areas.iter().map(|area| area.column_end).max().unwrap_or_default();
    let max_row = areas.iter().map(|area| area.row_end).max().unwrap_or_default();
    columns.resize_with(columns.len().max(max_column.saturating_add(1)), Vec::new);
    rows.resize_with(rows.len().max(max_row.saturating_add(1)), Vec::new);

    for area in areas {
        columns[area.column_start].push(LineName::implicit(
            area.implicit_start_name_index,
            names[area.implicit_start_name_index as usize],
            names[area.name_index as usize],
            true,
        ));
        columns[area.column_end].push(LineName::implicit(
            area.implicit_end_name_index,
            names[area.implicit_end_name_index as usize],
            names[area.name_index as usize],
            false,
        ));
        rows[area.row_start].push(LineName::implicit(
            area.implicit_start_name_index,
            names[area.implicit_start_name_index as usize],
            names[area.name_index as usize],
            true,
        ));
        rows[area.row_end].push(LineName::implicit(
            area.implicit_end_name_index,
            names[area.implicit_end_name_index as usize],
            names[area.name_index as usize],
            false,
        ));
    }
}

pub(crate) fn nth_named_line(lines: &[Vec<LineName>], name_raw: usize, nth_line: i32) -> Option<i32> {
    let mut remaining = if nth_line < 0 {
        lines.len().wrapping_add_signed(nth_line as isize)
    } else {
        nth_line.saturating_sub(1) as usize
    };
    for (line_index, names) in lines.iter().enumerate() {
        for name in names {
            if name.raw != name_raw {
                continue;
            }
            if remaining == 0 {
                return Some(line_index as i32);
            }
            remaining = remaining.wrapping_sub(1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style_facts::FfiSizeKind;

    fn breadth(kind: FfiGridTrackBreadthKind) -> FfiGridTrackBreadth {
        FfiGridTrackBreadth {
            kind: kind as u8,
            value: FfiSizeValue {
                kind: FfiSizeKind::Auto as u8,
                px: Default::default(),
                fraction: 0.0,
                calc: std::ptr::null(),
                contains_percentage: false,
                contains_anchor_function: false,
            },
            flex_factor: 0.0,
        }
    }

    fn entry(kind: FfiGridTrackEntryKind, next: u32) -> FfiGridTrackEntry {
        FfiGridTrackEntry {
            kind: kind as u8,
            next_sibling: next,
            name_index_start: 0,
            name_index_count: 0,
            size: breadth(FfiGridTrackBreadthKind::Auto),
            min_size: breadth(FfiGridTrackBreadthKind::Auto),
            max_size: breadth(FfiGridTrackBreadthKind::Auto),
            repeat_type: REPEAT_FIXED,
            repeat_count: 0,
            repeat_list: FfiGridTrackList::default(),
        }
    }

    #[test]
    fn recursive_repeat_preserves_boundary_line_name_order() {
        let mut entries = vec![
            entry(FfiGridTrackEntryKind::LineNames, 1),
            entry(FfiGridTrackEntryKind::Repeat, 2),
            entry(FfiGridTrackEntryKind::LineNames, NO_GRID_INDEX),
            entry(FfiGridTrackEntryKind::LineNames, 4),
            entry(FfiGridTrackEntryKind::TrackSize, 5),
            entry(FfiGridTrackEntryKind::LineNames, NO_GRID_INDEX),
        ];
        entries[0].name_index_count = 1;
        entries[1].repeat_count = 2;
        entries[1].repeat_list.first_entry = 3;
        entries[2].name_index_start = 3;
        entries[2].name_index_count = 1;
        entries[3].name_index_start = 1;
        entries[3].name_index_count = 1;
        entries[5].name_index_start = 2;
        entries[5].name_index_count = 1;
        let source = TrackListSource {
            names: &(0..=40).collect::<Vec<_>>(),
            entries: &entries,
            name_indices: &[10, 20, 30, 40],
        };
        let expanded = expand_standalone(
            source,
            FfiGridTrackList {
                first_entry: 0,
                ..Default::default()
            },
            |_index, _entry| 1,
        );

        assert_eq!(expanded.tracks.len(), 2);
        assert_eq!(
            expanded
                .lines
                .iter()
                .map(|line| line.iter().map(|name| name.name_index).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![10, 20], vec![30, 20], vec![30, 40]]
        );
    }

    #[test]
    fn auto_repeat_marks_every_expanded_track() {
        let mut entries = vec![
            entry(FfiGridTrackEntryKind::Repeat, NO_GRID_INDEX),
            entry(FfiGridTrackEntryKind::TrackSize, NO_GRID_INDEX),
        ];
        entries[0].repeat_type = REPEAT_AUTO_FIT;
        entries[0].repeat_list.first_entry = 1;
        let expanded = expand_standalone(
            TrackListSource {
                names: &[],
                entries: &entries,
                name_indices: &[],
            },
            FfiGridTrackList {
                first_entry: 0,
                ..Default::default()
            },
            |_index, _entry| 3,
        );
        assert_eq!(expanded.tracks.len(), 3);
        assert!(
            expanded
                .tracks
                .iter()
                .all(|track| track.is_auto_fit && track.is_auto_repeat)
        );
    }

    #[test]
    fn template_areas_grow_axes_and_add_preinterned_names() {
        let mut columns = vec![Vec::new()];
        let mut rows = vec![Vec::new()];
        add_template_area_lines(
            &mut columns,
            &mut rows,
            &[FfiGridArea {
                name_index: 7,
                implicit_start_name_index: 8,
                implicit_end_name_index: 9,
                row_start: 1,
                row_end: 3,
                column_start: 2,
                column_end: 4,
            }],
            &(0..=9).collect::<Vec<_>>(),
        );
        assert_eq!(columns.len(), 5);
        assert_eq!(rows.len(), 4);
        assert_eq!(columns[2][0], LineName::implicit(8, 8, 7, true));
        assert_eq!(columns[4][0], LineName::implicit(9, 9, 7, false));
        assert_eq!(rows[1][0], LineName::implicit(8, 8, 7, true));
        assert_eq!(rows[3][0], LineName::implicit(9, 9, 7, false));
    }

    #[test]
    fn named_line_counting_matches_cpp_positive_and_negative_order() {
        let lines = vec![
            vec![LineName::explicit(1, 1)],
            vec![LineName::explicit(2, 2)],
            vec![LineName::explicit(1, 1)],
            vec![LineName::explicit(1, 1)],
        ];
        assert_eq!(nth_named_line(&lines, 1, 1), Some(0));
        assert_eq!(nth_named_line(&lines, 1, 2), Some(2));
        // The current C++ code converts -1 to lines.len() - 1 before
        // counting matching names from the start, so three matches are not
        // enough. Preserve that behavior for the port.
        assert_eq!(nth_named_line(&lines, 1, -1), None);
    }
}
