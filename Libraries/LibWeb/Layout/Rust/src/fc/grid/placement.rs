/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::collections::HashSet;

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
pub(crate) fn place_items(
    items: &[PlacementInput],
    explicit_column_count: usize,
    explicit_row_count: usize,
    flow: AutoFlowAxis,
    dense: bool,
) -> Vec<PlacedItem> {
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

    for item in &mut output {
        item.row -= grid.min_row_index;
        item.column -= grid.min_column_index;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
