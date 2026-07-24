/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::facts::{FfiGridPlacement, FfiGridPlacementKind};
use super::template::{LineName, nth_named_line};
use std::collections::HashSet;

const MAX_GRID_LINE_NUMBER: i32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPlacementPosition {
    pub(crate) start: i32,
    pub(crate) end: i32,
    pub(crate) span: usize,
}

fn placement_kind(placement: FfiGridPlacement) -> FfiGridPlacementKind {
    assert!(placement.kind <= FfiGridPlacementKind::Span as u8);
    // SAFETY: The range check covers every repr(u8) variant.
    unsafe { std::mem::transmute(placement.kind) }
}

fn is_positioned(placement: FfiGridPlacement) -> bool {
    placement_kind(placement) == FfiGridPlacementKind::Line
}

fn is_span(placement: FfiGridPlacement) -> bool {
    placement_kind(placement) == FfiGridPlacementKind::Span
}

fn clamped_line_number(placement: FfiGridPlacement) -> Option<i32> {
    placement
        .has_line_number
        .then(|| placement.line_number.clamp(-MAX_GRID_LINE_NUMBER, MAX_GRID_LINE_NUMBER))
}

fn clamped_span(placement: FfiGridPlacement) -> usize {
    placement.line_number.clamp(1, MAX_GRID_LINE_NUMBER) as usize
}

/// Resolves one axis exactly as `GridFormattingContext::resolve_grid_position`.
/// Names are pre-interned in F1, including the area `-start`/`-end` aliases.
pub(crate) fn resolve_placement_position(
    placement_start: FfiGridPlacement,
    placement_end: FfiGridPlacement,
    placement_names: &[usize],
    lines: &[Vec<LineName>],
    explicit_line_count: usize,
    occupation_track_count: usize,
) -> ResolvedPlacementPosition {
    let start_line_number = clamped_line_number(placement_start);
    let end_line_number = clamped_line_number(placement_end);
    let mut result = ResolvedPlacementPosition {
        start: 0,
        end: 0,
        span: 1,
    };

    if let Some(number) = start_line_number {
        result.start = if number > 0 {
            number - 1
        } else {
            explicit_line_count as i32 + number
        };
    }
    if let Some(number) = end_line_number {
        result.end = number - 1;
    }
    if result.end < 0 {
        result.end = occupation_track_count as i32 + result.end + 2;
    }

    if is_span(placement_end) {
        result.span = clamped_span(placement_end);
    }
    if is_span(placement_start) {
        result.span = clamped_span(placement_start);
        result.start = result.end - result.span as i32;
        // Preserve the C++ workaround for spans that would overflow into
        // negative implicit tracks.
        result.start = result.start.max(0);
    }

    if placement_end.has_name {
        let number = end_line_number.unwrap_or(1);
        result.end = placement_names
            .get(placement_end.implicit_end_name_index as usize)
            .and_then(|name| nth_named_line(lines, *name, number))
            .or_else(|| {
                placement_names
                    .get(placement_end.name_index as usize)
                    .and_then(|name| nth_named_line(lines, *name, number))
            })
            .unwrap_or(explicit_line_count as i32);
        if !placement_start.has_line_number {
            result.start = result.end - 1;
        }
    }

    if placement_start.has_name {
        let number = start_line_number.unwrap_or(1);
        result.start = placement_names
            .get(placement_start.implicit_start_name_index as usize)
            .and_then(|name| nth_named_line(lines, *name, number))
            .or_else(|| {
                placement_names
                    .get(placement_start.name_index as usize)
                    .and_then(|name| nth_named_line(lines, *name, number))
            })
            .unwrap_or(explicit_line_count as i32);
    }

    if !is_positioned(placement_start) && is_positioned(placement_end) && !is_span(placement_end) {
        result.start = result.end - result.span as i32;
    }

    if is_positioned(placement_start) && is_positioned(placement_end) {
        if result.start > result.end {
            std::mem::swap(&mut result.start, &mut result.end);
        }
        if result.start != result.end {
            result.span = (result.end - result.start) as usize;
        } else {
            result.span = 1;
            result.end = result.start + 1;
        }
    }

    if is_span(placement_start) && is_span(placement_end) {
        result.span = clamped_span(placement_start);
    }
    result
}

pub(crate) fn resolve_placement_span(
    placement_start: FfiGridPlacement,
    placement_end: FfiGridPlacement,
    automatic_subgrid_span: Option<usize>,
) -> usize {
    if is_span(placement_start) {
        return clamped_span(placement_start);
    }
    if is_span(placement_end) {
        return clamped_span(placement_end);
    }
    if !(is_positioned(placement_start) && is_positioned(placement_end))
        && let Some(span) = automatic_subgrid_span
    {
        return span.max(1);
    }
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoFlowAxis {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedAxisPlacement {
    pub(crate) start: Option<i32>,
    pub(crate) span: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlacementInput {
    pub(crate) id: usize,
    pub(crate) order: i32,
    pub(crate) row: ResolvedAxisPlacement,
    pub(crate) column: ResolvedAxisPlacement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlacedItem {
    pub(crate) id: usize,
    pub(crate) row: i32,
    pub(crate) row_span: usize,
    pub(crate) column: i32,
    pub(crate) column_span: usize,
}

#[derive(Debug)]
pub(crate) struct PlacementResult {
    pub(crate) items: Vec<PlacedItem>,
    pub(crate) column_count: usize,
    pub(crate) row_count: usize,
    pub(crate) explicit_column_start: usize,
    pub(crate) explicit_row_start: usize,
}

#[derive(Default)]
pub(crate) struct OccupationGrid {
    occupied: HashSet<(i32, i32)>,
    min_column_index: i32,
    max_column_index: i32,
    min_row_index: i32,
    max_row_index: i32,
}

impl OccupationGrid {
    pub(crate) fn new(column_count: usize, row_count: usize) -> Self {
        Self {
            occupied: HashSet::new(),
            min_column_index: 0,
            max_column_index: column_count.saturating_sub(1) as i32,
            min_row_index: 0,
            max_row_index: row_count.saturating_sub(1) as i32,
        }
    }

    pub(crate) fn set_occupied(&mut self, column_start: i32, column_end: i32, row_start: i32, row_end: i32) {
        for row in row_start..row_end {
            for column in column_start..column_end {
                self.min_column_index = self.min_column_index.min(column);
                self.max_column_index = self.max_column_index.max(column);
                self.min_row_index = self.min_row_index.min(row);
                self.max_row_index = self.max_row_index.max(row);
                self.occupied.insert((column, row));
            }
        }
    }

    pub(crate) fn is_occupied(&self, column: i32, row: i32) -> bool {
        self.occupied.contains(&(column, row))
    }

    pub(crate) fn is_area_occupied(
        &self,
        column_start: i32,
        row_start: i32,
        column_span: usize,
        row_span: usize,
    ) -> bool {
        for row in row_start..row_start + row_span as i32 {
            for column in column_start..column_start + column_span as i32 {
                if self.is_occupied(column, row) {
                    return true;
                }
            }
        }
        false
    }

    fn find_unoccupied_grid_area(
        &self,
        flow: AutoFlowAxis,
        column: &mut i32,
        row: &mut i32,
        column_span: usize,
        row_span: usize,
    ) {
        match flow {
            AutoFlowAxis::Row => {
                while *row <= self.max_row_index {
                    while *column <= self.max_column_index {
                        let minor_axis_fits = *column + column_span as i32 - 1 <= self.max_column_index;
                        if minor_axis_fits && !self.is_area_occupied(*column, *row, column_span, row_span) {
                            return;
                        }
                        *column += 1;
                    }
                    *row += 1;
                    *column = self.min_column_index;
                }
            }
            AutoFlowAxis::Column => {
                while *column <= self.max_column_index {
                    while *row <= self.max_row_index {
                        let minor_axis_fits = *row + row_span as i32 - 1 <= self.max_row_index;
                        if minor_axis_fits && !self.is_area_occupied(*column, *row, column_span, row_span) {
                            return;
                        }
                        *row += 1;
                    }
                    *column += 1;
                    *row = self.min_row_index;
                }
            }
        }
    }
}

fn record(grid: &mut OccupationGrid, output: &mut Vec<PlacedItem>, item: PlacementInput, row: i32, column: i32) {
    grid.set_occupied(
        column,
        column + item.column.span as i32,
        row,
        row + item.row.span as i32,
    );
    output.push(PlacedItem {
        id: item.id,
        row,
        row_span: item.row.span,
        column,
        column_span: item.column.span,
    });
}

/// Runs the C++ grid auto-placement phase after named lines and explicit
/// placements have already been resolved to integer start lines.
pub(crate) fn place_items_with_grid(
    items: &[PlacementInput],
    explicit_column_count: usize,
    explicit_row_count: usize,
    flow: AutoFlowAxis,
    dense: bool,
) -> PlacementResult {
    let mut ordered_indices = (0..items.len()).collect::<Vec<_>>();
    ordered_indices.sort_by_key(|index| (items[*index].order, *index));

    let mut remaining = vec![true; items.len()];
    let mut grid = OccupationGrid::new(explicit_column_count, explicit_row_count);
    let mut output = Vec::with_capacity(items.len());

    // 1. Position items that are definite in both axes.
    for &index in &ordered_indices {
        let item = items[index];
        if let (Some(row), Some(column)) = (item.row.start, item.column.start) {
            record(&mut grid, &mut output, item, row, column);
            remaining[index] = false;
        }
    }

    // 2. Position items locked to a row.
    for &index in &ordered_indices {
        if !remaining[index] {
            continue;
        }
        let item = items[index];
        let Some(row) = item.row.start else {
            continue;
        };
        if item.column.start.is_some() {
            continue;
        }

        let mut column = 0;
        let mut found = false;
        while column <= grid.max_column_index {
            if !grid.is_area_occupied(column, row, item.column.span, item.row.span) {
                found = true;
                break;
            }
            column += 1;
        }
        if !found {
            column = grid.max_column_index + 1;
        }
        record(&mut grid, &mut output, item, row, column);
        remaining[index] = false;
    }

    // 3. Make the implicit grid wide enough for every auto-column span.
    for &index in &ordered_indices {
        if !remaining[index] {
            continue;
        }
        let span = items[index].column.span;
        if span.saturating_sub(1) > grid.max_column_index as usize {
            grid.max_column_index = span.saturating_sub(1) as i32;
        }
    }

    // 4. Position the remaining items in order-modified document order.
    let mut cursor_column = 0;
    let mut cursor_row = 0;
    for &index in &ordered_indices {
        if !remaining[index] {
            continue;
        }
        let item = items[index];
        if let Some(column) = item.column.start {
            if dense {
                cursor_row = grid.min_row_index;
            } else if column < cursor_column {
                cursor_row += 1;
            }
            cursor_column = column;
            while grid.is_area_occupied(cursor_column, cursor_row, item.column.span, item.row.span) {
                cursor_row += 1;
            }
            record(&mut grid, &mut output, item, cursor_row, cursor_column);
        } else {
            if dense {
                cursor_column = grid.min_column_index;
                cursor_row = grid.min_row_index;
            }
            grid.find_unoccupied_grid_area(
                flow,
                &mut cursor_column,
                &mut cursor_row,
                item.column.span,
                item.row.span,
            );
            let column = cursor_column;
            let row = cursor_row;
            match flow {
                AutoFlowAxis::Row => cursor_column += item.column.span as i32,
                AutoFlowAxis::Column => cursor_row += item.row.span as i32,
            }
            record(&mut grid, &mut output, item, row, column);
        }
    }

    let explicit_column_start = grid.min_column_index.unsigned_abs() as usize;
    let explicit_row_start = grid.min_row_index.unsigned_abs() as usize;
    let column_count = explicit_column_start
        .saturating_add(grid.max_column_index.max(0) as usize)
        .saturating_add(1);
    let row_count = explicit_row_start
        .saturating_add(grid.max_row_index.max(0) as usize)
        .saturating_add(1);
    for item in &mut output {
        item.row -= grid.min_row_index;
        item.column -= grid.min_column_index;
    }
    PlacementResult {
        items: output,
        column_count,
        row_count,
        explicit_column_start,
        explicit_row_start,
    }
}

pub(crate) fn place_items(
    items: &[PlacementInput],
    explicit_column_count: usize,
    explicit_row_count: usize,
    flow: AutoFlowAxis,
    dense: bool,
) -> Vec<PlacedItem> {
    place_items_with_grid(items, explicit_column_count, explicit_row_count, flow, dense).items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fc::grid::facts::NO_GRID_INDEX;
    use crate::fc::grid::template::LineName;

    fn auto(id: usize, column_span: usize) -> PlacementInput {
        PlacementInput {
            id,
            order: 0,
            row: ResolvedAxisPlacement { start: None, span: 1 },
            column: ResolvedAxisPlacement {
                start: None,
                span: column_span,
            },
        }
    }

    fn by_id(items: Vec<PlacedItem>) -> Vec<PlacedItem> {
        let mut items = items;
        items.sort_by_key(|item| item.id);
        items
    }

    fn auto_placement() -> FfiGridPlacement {
        FfiGridPlacement::default()
    }

    fn line(number: Option<i32>, name: Option<(u32, u32, u32)>) -> FfiGridPlacement {
        FfiGridPlacement {
            kind: FfiGridPlacementKind::Line as u8,
            has_line_number: number.is_some(),
            line_number: number.unwrap_or_default(),
            has_name: name.is_some(),
            name_index: name.map(|indices| indices.0).unwrap_or(NO_GRID_INDEX),
            implicit_start_name_index: name.map(|indices| indices.1).unwrap_or(NO_GRID_INDEX),
            implicit_end_name_index: name.map(|indices| indices.2).unwrap_or(NO_GRID_INDEX),
        }
    }

    fn span(count: i32) -> FfiGridPlacement {
        FfiGridPlacement {
            kind: FfiGridPlacementKind::Span as u8,
            has_line_number: true,
            line_number: count,
            ..FfiGridPlacement::default()
        }
    }

    fn explicit_name(index: u32) -> LineName {
        LineName {
            name_index: index,
            raw: index as usize,
            implicit: false,
            adopted_from_parent: false,
            area_name_raw: 0,
            area_is_start: false,
        }
    }

    #[test]
    fn dense_packing_backfills_a_hole_while_sparse_keeps_cursor_order() {
        let inputs = [auto(0, 2), auto(1, 2), auto(2, 1)];
        let sparse = by_id(place_items(&inputs, 3, 1, AutoFlowAxis::Row, false));
        let dense = by_id(place_items(&inputs, 3, 1, AutoFlowAxis::Row, true));

        assert_eq!((sparse[0].row, sparse[0].column), (0, 0));
        assert_eq!((sparse[1].row, sparse[1].column), (1, 0));
        assert_eq!((sparse[2].row, sparse[2].column), (1, 2));
        assert_eq!((dense[2].row, dense[2].column), (0, 2));
    }

    #[test]
    fn order_buckets_keep_document_order_within_equal_orders() {
        let mut first = auto(0, 1);
        first.order = 2;
        let mut second = auto(1, 1);
        second.order = -1;
        let mut third = auto(2, 1);
        third.order = -1;
        let placed = place_items(&[first, second, third], 3, 1, AutoFlowAxis::Row, false);
        assert_eq!(placed.iter().map(|item| item.id).collect::<Vec<_>>(), vec![1, 2, 0]);
    }

    #[test]
    fn column_flow_advances_rows_before_columns() {
        let placed = by_id(place_items(
            &[auto(0, 1), auto(1, 1), auto(2, 1)],
            1,
            2,
            AutoFlowAxis::Column,
            false,
        ));
        assert_eq!((placed[0].row, placed[0].column), (0, 0));
        assert_eq!((placed[1].row, placed[1].column), (1, 0));
        assert_eq!((placed[2].row, placed[2].column), (0, 1));
    }

    #[test]
    fn positive_and_negative_numeric_lines_resolve_against_explicit_grid() {
        let lines = vec![Vec::new(); 5];
        let resolved = resolve_placement_position(line(Some(-3), None), line(Some(-1), None), &[], &lines, 5, 4);
        assert_eq!(
            resolved,
            ResolvedPlacementPosition {
                start: 2,
                end: 4,
                span: 2
            }
        );
    }

    #[test]
    fn area_aliases_win_over_bare_named_lines() {
        let lines = vec![
            vec![explicit_name(1)],
            vec![explicit_name(2)],
            vec![explicit_name(3)],
            vec![explicit_name(1)],
        ];
        let resolved = resolve_placement_position(
            line(None, Some((1, 2, 3))),
            line(None, Some((1, 2, 3))),
            &[0, 1, 2, 3],
            &lines,
            4,
            3,
        );
        assert_eq!(
            resolved,
            ResolvedPlacementPosition {
                start: 1,
                end: 2,
                span: 1
            }
        );
    }

    #[test]
    fn span_from_end_and_two_span_rule_match_cpp() {
        let lines = vec![Vec::new(); 5];
        let from_end = resolve_placement_position(span(2), line(Some(4), None), &[], &lines, 5, 4);
        assert_eq!(
            from_end,
            ResolvedPlacementPosition {
                start: 1,
                end: 3,
                span: 2
            }
        );
        let two_spans = resolve_placement_position(span(3), span(7), &[], &lines, 5, 4);
        assert_eq!(two_spans.span, 3);
    }

    #[test]
    fn automatic_subgrid_span_is_floored_at_one() {
        assert_eq!(resolve_placement_span(auto_placement(), auto_placement(), Some(0)), 1);
        assert_eq!(resolve_placement_span(auto_placement(), auto_placement(), Some(4)), 4);
    }
}
