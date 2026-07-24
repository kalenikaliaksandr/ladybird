/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::fc::{FfiBorderData, FfiBorderDataWithElementKind, FfiBordersData};

pub(crate) const ELEMENT_CELL: u8 = 0;
pub(crate) const ELEMENT_ROW: u8 = 1;
pub(crate) const ELEMENT_ROW_GROUP: u8 = 2;
pub(crate) const ELEMENT_COLUMN: u8 = 3;
pub(crate) const ELEMENT_TABLE: u8 = 5;

const LINE_STYLE_NONE: u8 = 0;
const LINE_STYLE_HIDDEN: u8 = 1;
const LINE_STYLE_DOTTED: u8 = 2;
const LINE_STYLE_DASHED: u8 = 3;
const LINE_STYLE_SOLID: u8 = 4;
const LINE_STYLE_DOUBLE: u8 = 5;
const LINE_STYLE_GROOVE: u8 = 6;
const LINE_STYLE_RIDGE: u8 = 7;
const LINE_STYLE_INSET: u8 = 8;
const LINE_STYLE_OUTSET: u8 = 9;

fn line_style_score(line_style: u8) -> u8 {
    match line_style {
        LINE_STYLE_INSET => 0,
        LINE_STYLE_GROOVE => 1,
        LINE_STYLE_OUTSET => 2,
        LINE_STYLE_RIDGE => 3,
        LINE_STYLE_DOTTED => 4,
        LINE_STYLE_DASHED => 5,
        LINE_STYLE_SOLID => 6,
        LINE_STYLE_DOUBLE => 7,
        _ => panic!("line style has no conflict-resolution score"),
    }
}

pub(crate) fn border_is_less_specific(incumbent: FfiBorderData, candidate: FfiBorderData) -> bool {
    // Implements criteria for steps 1, 2 and 3 of border conflict resolution algorithm, as described in
    // https://www.w3.org/TR/CSS22/tables.html#border-conflict-resolution.

    // 1. Borders with the 'border-style' of 'hidden' take precedence over all other conflicting borders. Any border with this
    //    value suppresses all borders at this location.
    if incumbent.line_style == LINE_STYLE_HIDDEN {
        return false;
    }
    if candidate.line_style == LINE_STYLE_HIDDEN {
        return true;
    }
    // 2. Borders with a style of 'none' have the lowest priority. Only if the border properties of all the elements meeting
    //    at this edge are 'none' will the border be omitted (but note that 'none' is the default value for the border style.)
    if incumbent.line_style == LINE_STYLE_NONE {
        return true;
    }
    if candidate.line_style == LINE_STYLE_NONE {
        return false;
    }
    // 3. If none of the styles are 'hidden' and at least one of them is not 'none', then narrow borders are discarded in favor
    //    of wider ones. If several have the same 'border-width' then styles are preferred in this order: 'double', 'solid',
    //    'dashed', 'dotted', 'ridge', 'outset', 'groove', and the lowest: 'inset'.
    if incumbent.width != candidate.width {
        return incumbent.width < candidate.width;
    }
    line_style_score(incumbent.line_style) < line_style_score(candidate.line_style)
}

fn candidate_wins(candidate: FfiBorderData, incumbent: FfiBorderData) -> bool {
    candidate.line_style != LINE_STYLE_NONE && border_is_less_specific(incumbent, candidate)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ElementBorders {
    pub(crate) top: FfiBorderData,
    pub(crate) right: FfiBorderData,
    pub(crate) bottom: FfiBorderData,
    pub(crate) left: FfiBorderData,
}

// Each segment stores the border that currently wins at one slot boundary of the table grid,
// together with the kind of element it came from. A value-initialized segment acts as a sentinel
// meaning "no border applied yet": since candidates with a line style of 'none' never replace an
// incumbent, a segment whose winner still has style 'none' received no visible contribution at all.

// Implements border conflict resolution, as described in
// https://www.w3.org/TR/CSS22/tables.html#border-conflict-resolution, with a "push" model over a
// grid of border line segments: each boundary between two grid slots is a single shared segment, so
// borders of adjacent elements collapse by construction instead of requiring neighbor lookups.
//
// Horizontal border lines run between (and around) rows: there are row_count + 1 of them, each with
// one segment per column. Vertical border lines run between (and around) columns: column_count + 1
// of them, each with one segment per row.
//
// Table parts must be applied in order of decreasing precedence — cells, rows, row groups, columns,
// column groups, and lastly the table — with parts of equal precedence applied leftmost/topmost
// first. A candidate border only replaces the current winner of a segment when it is strictly more
// specific (steps 1-3 of the border conflict resolution algorithm), so ties resolve towards the
// earlier-applied part, which implements step 4 without tracking element kinds or coordinates.
pub(crate) struct CollapsedBorderGrid {
    horizontal_lines: Vec<Vec<FfiBorderDataWithElementKind>>,
    vertical_lines: Vec<Vec<FfiBorderDataWithElementKind>>,
}

impl CollapsedBorderGrid {
    pub(crate) fn new(row_count: usize, column_count: usize) -> Self {
        Self {
            horizontal_lines: vec![vec![FfiBorderDataWithElementKind::default(); column_count]; row_count + 1],
            vertical_lines: vec![vec![FfiBorderDataWithElementKind::default(); row_count]; column_count + 1],
        }
    }

    pub(crate) fn apply_borders(
        &mut self,
        borders: ElementBorders,
        row_start: usize,
        row_end: usize,
        column_start: usize,
        column_end: usize,
        element_kind: u8,
    ) {
        Self::apply_to_segments(
            &mut self.horizontal_lines[row_start],
            column_start,
            column_end,
            borders.top,
            element_kind,
        );
        Self::apply_to_segments(
            &mut self.horizontal_lines[row_end],
            column_start,
            column_end,
            borders.bottom,
            element_kind,
        );
        Self::apply_to_segments(
            &mut self.vertical_lines[column_start],
            row_start,
            row_end,
            borders.left,
            element_kind,
        );
        Self::apply_to_segments(
            &mut self.vertical_lines[column_end],
            row_start,
            row_end,
            borders.right,
            element_kind,
        );
    }

    pub(crate) fn hide_segments_inside_span(
        &mut self,
        row_start: usize,
        row_end: usize,
        column_start: usize,
        column_end: usize,
    ) {
        // Segments strictly inside a spanning cell are not borders of any element; mark them as hidden
        // so that borders of rows and columns crossing the span cannot win there.
        let hidden = FfiBorderDataWithElementKind {
            border_data: FfiBorderData {
                color: 0,
                line_style: LINE_STYLE_HIDDEN,
                width: CssPixels::default(),
            },
            element_kind: ELEMENT_CELL,
        };
        for row in row_start + 1..row_end {
            for column in column_start..column_end {
                self.horizontal_lines[row][column] = hidden;
            }
        }
        for column in column_start + 1..column_end {
            for row in row_start..row_end {
                self.vertical_lines[column][row] = hidden;
            }
        }
    }

    pub(crate) fn resolve_for_cell(
        &self,
        row_start: usize,
        row_end: usize,
        column_start: usize,
        column_end: usize,
        own: ElementBorders,
    ) -> FfiBordersData {
        let harvest = |winner: FfiBorderDataWithElementKind, own_border: FfiBorderData| {
            // A winner whose style is 'none' means every border meeting at this edge is 'none'; fall
            // back to the cell's own (invisible) border so the stored winner matches the cell.
            if winner.border_data.line_style == LINE_STYLE_NONE {
                FfiBorderDataWithElementKind {
                    border_data: own_border,
                    element_kind: ELEMENT_CELL,
                }
            } else {
                winner
            }
        };
        FfiBordersData {
            top: harvest(
                Self::most_specific(&self.horizontal_lines[row_start], column_start, column_end),
                own.top,
            ),
            right: harvest(
                Self::most_specific(&self.vertical_lines[column_end], row_start, row_end),
                own.right,
            ),
            bottom: harvest(
                Self::most_specific(&self.horizontal_lines[row_end], column_start, column_end),
                own.bottom,
            ),
            left: harvest(
                Self::most_specific(&self.vertical_lines[column_start], row_start, row_end),
                own.left,
            ),
        }
    }

    fn apply_to_segments(
        line: &mut [FfiBorderDataWithElementKind],
        start: usize,
        end: usize,
        data: FfiBorderData,
        element_kind: u8,
    ) {
        if data.line_style == LINE_STYLE_NONE {
            return;
        }
        for segment in &mut line[start..end] {
            if candidate_wins(data, segment.border_data) {
                *segment = FfiBorderDataWithElementKind {
                    border_data: data,
                    element_kind,
                };
            }
        }
    }

    fn most_specific(line: &[FfiBorderDataWithElementKind], start: usize, end: usize) -> FfiBorderDataWithElementKind {
        let mut winner = line[start];
        for segment in &line[start + 1..end] {
            if candidate_wins(segment.border_data, winner.border_data) {
                winner = *segment;
            }
        }
        winner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn border(width: i32, style: u8, color: u32) -> FfiBorderData {
        FfiBorderData {
            width: CssPixels::from_raw(width),
            line_style: style,
            color,
        }
    }

    #[test]
    fn conflict_priority_matches_width_style_and_hidden_rules() {
        assert!(border_is_less_specific(
            border(64, LINE_STYLE_SOLID, 1),
            border(128, LINE_STYLE_DOTTED, 2),
        ));
        assert!(border_is_less_specific(
            border(64, LINE_STYLE_DASHED, 1),
            border(64, LINE_STYLE_SOLID, 2),
        ));
        assert!(border_is_less_specific(
            border(1024, LINE_STYLE_SOLID, 1),
            border(0, LINE_STYLE_HIDDEN, 2),
        ));
        assert!(!border_is_less_specific(
            border(0, LINE_STYLE_HIDDEN, 1),
            border(1024, LINE_STYLE_DOUBLE, 2),
        ));
    }

    #[test]
    fn spanning_cell_hides_crossing_row_and_column_segments() {
        let visible = ElementBorders {
            top: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            right: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            bottom: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            left: border(64, LINE_STYLE_SOLID, 0xff00_0000),
        };
        let own = ElementBorders::default();
        let mut grid = CollapsedBorderGrid::new(2, 2);
        grid.hide_segments_inside_span(0, 2, 0, 2);
        grid.apply_borders(visible, 0, 1, 0, 2, ELEMENT_ROW);
        let result = grid.resolve_for_cell(0, 2, 0, 2, own);
        assert_eq!(result.top.border_data.line_style, LINE_STYLE_SOLID);
        assert_eq!(result.bottom.border_data.line_style, LINE_STYLE_NONE);
    }
}
