/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::{Node, TableTree};
use crate::css_pixels::CssPixels;
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub(crate) box_: Node,
    pub(crate) column_index: usize,
    pub(crate) row_index: usize,
    pub(crate) column_span: usize,
    pub(crate) row_span: usize,
    pub(crate) baseline: CssPixels,
    pub(crate) outer_min_inline_size: CssPixels,
    pub(crate) outer_max_inline_size: CssPixels,
    pub(crate) outer_min_block_size: CssPixels,
    pub(crate) outer_max_block_size: CssPixels,
}

pub(crate) struct Row {
    pub(crate) box_: Node,
    pub(crate) base_block_size: CssPixels,
    pub(crate) reference_block_size: CssPixels,
    pub(crate) final_block_size: CssPixels,
    pub(crate) baseline: CssPixels,
    pub(crate) min_size: CssPixels,
    pub(crate) max_size: CssPixels,
    pub(crate) is_collapsed: bool,
    pub(crate) has_intrinsic_percentage: bool,
    pub(crate) intrinsic_percentage: f64,
    pub(crate) is_constrained: bool,
}

impl Row {
    fn new(box_: Node, is_collapsed: bool) -> Self {
        Self {
            box_,
            base_block_size: CssPixels::default(),
            reference_block_size: CssPixels::default(),
            final_block_size: CssPixels::default(),
            baseline: CssPixels::default(),
            min_size: CssPixels::default(),
            max_size: CssPixels::default(),
            is_collapsed,
            has_intrinsic_percentage: false,
            intrinsic_percentage: 0.0,
            is_constrained: false,
        }
    }
}

pub(crate) struct TableGrid {
    pub(crate) column_count: usize,
    pub(crate) cells: Vec<Cell>,
    pub(crate) rows: Vec<Row>,
}

fn matching_children<T: TableTree>(
    tree: &mut T,
    parent: Node,
    predicate: impl Fn(crate::box_facts::FfiLayoutBoxFacts) -> bool,
) -> Vec<Node> {
    let mut result = Vec::new();
    let mut child = tree.first_child(parent);
    while !child.is_null() {
        let facts = tree.box_facts(child);
        if facts.is_box && predicate(facts) {
            result.push(child);
        }
        child = tree.next_sibling(child);
    }
    result
}

fn count_columns_in_subtree<T: TableTree>(tree: &mut T, root: Node) -> usize {
    let mut count = 0usize;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut child = tree.first_child(node);
        let mut children = Vec::new();
        while !child.is_null() {
            children.push(child);
            child = tree.next_sibling(child);
        }
        for child in children.into_iter().rev() {
            let facts = tree.box_facts(child);
            if facts.is_box && facts.is_table_column {
                count = count.saturating_add(tree.table_facts(child).column_span as usize);
            }
            stack.push(child);
        }
    }
    count
}

pub(crate) fn calculate<T: TableTree>(tree: &mut T, table: Node) -> TableGrid {
    let mut cells = Vec::new();
    let mut rows = Vec::new();
    let mut occupancy = HashSet::new();
    let mut column_count = 0usize;
    let mut row_count = 0usize;
    let mut current_row = 0usize;

    for column_group in matching_children(tree, table, |facts| facts.is_table_column_group) {
        column_count = column_count.saturating_add(count_columns_in_subtree(tree, column_group));
    }

    let process_row = |tree: &mut T,
                       row: Node,
                       row_group: Option<Node>,
                       cells: &mut Vec<Cell>,
                       rows: &mut Vec<Row>,
                       occupancy: &mut HashSet<(usize, usize)>,
                       column_count: &mut usize,
                       row_count: &mut usize,
                       current_row: &mut usize| {
        if *row_count == *current_row {
            *row_count += 1;
        }
        let mut current_column = 0usize;
        for cell_box in matching_children(tree, row, |facts| facts.is_table_cell) {
            while current_column < *column_count && occupancy.contains(&(current_column, *current_row)) {
                current_column += 1;
            }
            if current_column == *column_count {
                *column_count += 1;
            }

            let table_facts = tree.table_facts(cell_box);
            let column_span = table_facts.cell_column_span;
            let mut row_span = table_facts.cell_row_span;
            if row_span == 0 {
                // Downward-growing cells remain deliberately unimplemented,
                // matching TableGrid.cpp.
                row_span = 1;
            }
            *column_count = (*column_count).max(current_column + column_span);
            *row_count = (*row_count).max(*current_row + row_span);
            for row_index in *current_row..*current_row + row_span {
                for column_index in current_column..current_column + column_span {
                    occupancy.insert((column_index, row_index));
                }
            }
            cells.push(Cell {
                box_: cell_box,
                column_index: current_column,
                row_index: *current_row,
                column_span,
                row_span,
                baseline: CssPixels::default(),
                outer_min_inline_size: CssPixels::default(),
                outer_max_inline_size: CssPixels::default(),
                outer_min_block_size: CssPixels::default(),
                outer_max_block_size: CssPixels::default(),
            });
            current_column += column_span;
        }

        // CSS::Visibility::Collapse is pinned to zero in
        // LayoutRustBridge.cpp.
        let row_collapsed = tree.style_facts(row).visibility == 0;
        let group_collapsed = row_group.is_some_and(|group| tree.style_facts(group).visibility == 0);
        rows.push(Row::new(row, row_collapsed || group_collapsed));
        *current_row += 1;
    };

    let mut child = tree.first_child(table);
    while !child.is_null() {
        let facts = tree.box_facts(child);
        if facts.is_box && (facts.is_table_row_group || facts.is_table_header_group || facts.is_table_footer_group) {
            for row in matching_children(tree, child, |row_facts| row_facts.is_table_row) {
                process_row(
                    tree,
                    row,
                    Some(child),
                    &mut cells,
                    &mut rows,
                    &mut occupancy,
                    &mut column_count,
                    &mut row_count,
                    &mut current_row,
                );
            }
        } else if facts.is_box && facts.is_table_row {
            process_row(
                tree,
                child,
                None,
                &mut cells,
                &mut rows,
                &mut occupancy,
                &mut column_count,
                &mut row_count,
                &mut current_row,
            );
        }
        child = tree.next_sibling(child);
    }

    for cell in &mut cells {
        cell.row_span = cell.row_span.min(rows.len() - cell.row_index);
        cell.column_span = cell.column_span.min(column_count - cell.column_index);
    }

    TableGrid {
        column_count,
        cells,
        rows,
    }
}
