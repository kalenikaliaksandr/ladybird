/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

mod borders;
mod distribution;
mod grid;

use super::sizing::SizingContext;
use super::{
    FfiBorderData, FfiCaptionLayoutResult, FfiChildLayoutResult, FfiFormattingContextType, FfiLayoutFcCallbacks,
    FfiMeasuredCellContent, FormattingContextInstance,
};
use crate::box_facts::FfiLayoutBoxFacts;
use crate::css_pixels::CssPixels;
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::{
    AvailableSize, AvailableSizeType, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput,
};
use crate::layout_state::state_mut;
use crate::style_facts::{FfiSizeValue, FfiStyleFacts};
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use borders::{
    CollapsedBorderGrid, ELEMENT_CELL, ELEMENT_COLUMN, ELEMENT_ROW, ELEMENT_ROW_GROUP, ELEMENT_TABLE, ElementBorders,
};
use distribution::{Column, distribute_inline_size};
use grid::{Cell, Row};
use std::ffi::c_void;

pub(crate) type Node = *mut c_void;

const LAYOUT_MODE_NORMAL: u8 = 0;
const BOX_SIZING_BORDER_BOX: u8 = 0;
const BORDER_COLLAPSE_SEPARATE: u8 = 0;
const CAPTION_SIDE_TOP: u8 = 0;
const CAPTION_SIDE_BOTTOM: u8 = 1;
const TABLE_LAYOUT_FIXED: u8 = 1;
const VERTICAL_ALIGN_BASELINE: u8 = 0;
const VERTICAL_ALIGN_BOTTOM: u8 = 1;
const VERTICAL_ALIGN_MIDDLE: u8 = 2;
const VERTICAL_ALIGN_SUB: u8 = 3;
const VERTICAL_ALIGN_SUPER: u8 = 4;
const VERTICAL_ALIGN_TEXT_BOTTOM: u8 = 5;
const VERTICAL_ALIGN_TEXT_TOP: u8 = 6;
const VERTICAL_ALIGN_TOP: u8 = 7;

pub(crate) trait TableTree {
    fn first_child(&mut self, node: Node) -> Node;
    fn next_sibling(&mut self, node: Node) -> Node;
    fn box_facts(&mut self, node: Node) -> FfiLayoutBoxFacts;
    fn style_facts(&mut self, node: Node) -> FfiStyleFacts;
    fn table_facts(&mut self, node: Node) -> super::FfiTableBoxFacts;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackAxis {
    Row,
    Column,
}

struct TableFormattingContext {
    state: *mut c_void,
    table_box: Node,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    table_constraints: FfiContainingBlockConstraints,
    participant_constraints: FfiContainingBlockConstraints,
    available_space: AvailableSpace,
    table_block_size: CssPixels,
    automatic_content_block_size: CssPixels,
    pending_table_offset: crate::geometry::LogicalOffset,
    min_border_box_block_size_from_flex_item: Option<CssPixels>,
    needs_fixed_mode_row_measurement: bool,
    cells: Vec<Cell>,
    columns: Vec<Column>,
    rows: Vec<Row>,
}

impl TableTree for TableFormattingContext {
    fn first_child(&mut self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.first_child, node)
    }

    fn next_sibling(&mut self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.next_sibling, node)
    }

    fn box_facts(&mut self, node: Node) -> FfiLayoutBoxFacts {
        state_mut(self.state).box_facts(&self.callbacks, node)
    }

    fn style_facts(&mut self, node: Node) -> FfiStyleFacts {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn table_facts(&mut self, node: Node) -> super::FfiTableBoxFacts {
        state_mut(self.state).table_facts(&self.callbacks, node)
    }
}

impl TableFormattingContext {
    fn new(instance: &FormattingContextInstance) -> Self {
        assert_eq!(instance.fc_type, FfiFormattingContextType::Table as u8);
        Self {
            state: instance.state,
            table_box: instance.box_,
            layout_mode: instance.layout_mode,
            callbacks: instance.callbacks,
            table_constraints: FfiContainingBlockConstraints::default(),
            participant_constraints: FfiContainingBlockConstraints::default(),
            available_space: AvailableSpace::default(),
            table_block_size: CssPixels::default(),
            automatic_content_block_size: CssPixels::default(),
            pending_table_offset: instance.pending_table_box_content_offset_in_wrapper,
            min_border_box_block_size_from_flex_item: None,
            needs_fixed_mode_row_measurement: false,
            cells: Vec::new(),
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn navigate(&self, callback: crate::box_facts::FfiLayoutNavCallback, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: Navigation is synchronous and the host owns every node.
        unsafe { callback(self.callbacks.navigation.context, node) }
    }

    fn sizing(&self) -> SizingContext {
        SizingContext::new(self.state, self.callbacks)
    }

    fn parent(&self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.parent, node)
    }

    fn matching_children(&mut self, parent: Node, predicate: impl Fn(FfiLayoutBoxFacts) -> bool) -> Vec<Node> {
        let mut children = Vec::new();
        let mut child = self.first_child(parent);
        while !child.is_null() {
            let facts = self.box_facts(child);
            if facts.is_box && predicate(facts) {
                children.push(child);
            }
            child = self.next_sibling(child);
        }
        children
    }

    fn used_values(&self, node: Node) -> *mut UsedValuesCore {
        bump(FfiOp::UsedValuesGetCallback);
        // SAFETY: The callback returns state-owned storage.
        let result = unsafe { (self.callbacks.get_used_values)(self.callbacks.context, node) };
        assert!(!result.is_null());
        result
    }

    fn create_used_values(&self, node: Node, constraints: FfiContainingBlockConstraints) -> *mut UsedValuesCore {
        bump(FfiOp::UsedValuesCreateCallback);
        // SAFETY: The host creates one state entry for this participant.
        let result = unsafe {
            (self.callbacks.create_used_values)(
                self.callbacks.context,
                node,
                constraints.has_percentage_basis_inline_size,
                constraints.percentage_basis_inline_size,
                constraints.has_percentage_basis_block_size,
                constraints.percentage_basis_block_size,
            )
        };
        assert!(!result.is_null());
        result
    }

    fn set_cell_coordinates(&self, cell: Cell) {
        bump(FfiOp::SetTableCellCoordinatesCallback);
        // SAFETY: The cell and state live for the current pass.
        unsafe {
            (self.callbacks.set_table_cell_coordinates)(
                self.callbacks.context,
                cell.box_,
                cell.row_index,
                cell.column_index,
                cell.row_span,
                cell.column_span,
            );
        }
    }

    fn place_child(&self, node: Node, x: CssPixels, y: CssPixels) {
        bump(FfiOp::PlaceChildCallback);
        // SAFETY: The node has a live used-values entry.
        unsafe {
            (self.callbacks.place_child)(self.callbacks.context, node, FfiCssPixelPoint { x, y });
        }
    }

    fn border_spacing_inline(&mut self) -> CssPixels {
        let style = self.style_facts(self.table_box);
        // When a table is laid out in collapsed-borders mode, the border-spacing of the table-root is ignored (as if it was set to 0px):
        // https://www.w3.org/TR/css-tables-3/#collapsed-style-overrides
        if style.border_collapse != BORDER_COLLAPSE_SEPARATE {
            CssPixels::default()
        } else {
            style.border_spacing_horizontal
        }
    }

    fn border_spacing_block(&mut self) -> CssPixels {
        let style = self.style_facts(self.table_box);
        // When a table is laid out in collapsed-borders mode, the border-spacing of the table-root is ignored (as if it was set to 0px):
        // https://www.w3.org/TR/css-tables-3/#collapsed-style-overrides
        if style.border_collapse != BORDER_COLLAPSE_SEPARATE {
            CssPixels::default()
        } else {
            style.border_spacing_vertical
        }
    }

    fn element_borders(&mut self, node: Node) -> ElementBorders {
        let style = self.style_facts(node);
        let table = self.table_facts(node);
        ElementBorders {
            top: FfiBorderData {
                color: table.border_top_color,
                line_style: style.border_top_style,
                width: style.border_top_width,
            },
            right: FfiBorderData {
                color: table.border_right_color,
                line_style: style.border_right_style,
                width: style.border_right_width,
            },
            bottom: FfiBorderData {
                color: table.border_bottom_color,
                line_style: style.border_bottom_style,
                width: style.border_bottom_width,
            },
            left: FfiBorderData {
                color: table.border_left_color,
                line_style: style.border_left_style,
                width: style.border_left_width,
            },
        }
    }

    fn border_conflict_resolution(&mut self) {
        if self.style_facts(self.table_box).border_collapse == BORDER_COLLAPSE_SEPARATE {
            for cell in self.cells.clone() {
                self.set_cell_coordinates(cell);
            }
            return;
        }

        let row_count = self.rows.len();
        let column_count = self.columns.len();
        let mut grid = CollapsedBorderGrid::new(row_count, column_count);
        // Cells, column by column so that on ties the cell further to the left, then further to the
        // top, wins. Cell spans are already clipped to the table end by TableGrid.
        let mut cells = self.cells.clone();
        cells.sort_by_key(|cell| (cell.column_index, cell.row_index));
        for cell in cells {
            if cell.row_span > 1 || cell.column_span > 1 {
                grid.hide_segments_inside_span(
                    cell.row_index,
                    cell.row_index + cell.row_span,
                    cell.column_index,
                    cell.column_index + cell.column_span,
                );
            }
            let borders = self.element_borders(cell.box_);
            grid.apply_borders(
                borders,
                cell.row_index,
                cell.row_index + cell.row_span,
                cell.column_index,
                cell.column_index + cell.column_span,
                ELEMENT_CELL,
            );
        }
        for row_index in 0..row_count {
            let row_box = self.rows[row_index].box_;
            let borders = self.element_borders(row_box);
            grid.apply_borders(borders, row_index, row_index + 1, 0, column_count, ELEMENT_ROW);
        }
        // Row groups, in the order their rows appear in the grid. Rows of a group are contiguous in
        // m_rows, since TableGrid collects them in tree order.
        let mut row_index = 0;
        while row_index < row_count {
            let group = self.parent(self.rows[row_index].box_);
            if group.is_null() {
                row_index += 1;
                continue;
            }
            let facts = self.box_facts(group);
            if !(facts.is_table_row_group || facts.is_table_header_group || facts.is_table_footer_group) {
                row_index += 1;
                continue;
            }
            let start = row_index;
            while row_index < row_count && self.parent(self.rows[row_index].box_) == group {
                row_index += 1;
            }
            let borders = self.element_borders(group);
            grid.apply_borders(borders, start, row_index, 0, column_count, ELEMENT_ROW_GROUP);
        }

        // Column (<col>) elements.
        let mut column_index = 0usize;
        for column_group in self.matching_children(self.table_box, |facts| facts.is_table_column_group) {
            for column in self.matching_children(column_group, |facts| facts.is_table_column) {
                let span = self.table_facts(column).column_span as usize;
                let end = (column_index + span).min(column_count);
                let borders = self.element_borders(column);
                while column_index < end {
                    grid.apply_borders(borders, 0, row_count, column_index, column_index + 1, ELEMENT_COLUMN);
                    column_index += 1;
                }
            }
        }
        let table_borders = self.element_borders(self.table_box);
        grid.apply_borders(table_borders, 0, row_count, 0, column_count, ELEMENT_TABLE);

        for cell in self.cells.clone() {
            let own = self.element_borders(cell.box_);
            let row_end = cell.row_index + cell.row_span;
            let column_end = cell.column_index + cell.column_span;
            let resolved = grid.resolve_for_cell(cell.row_index, row_end, cell.column_index, column_end, own);
            self.set_cell_coordinates(cell);
            let used = self.used_values(cell.box_);
            // SAFETY: `used` is the unique entry for this cell.
            unsafe {
                (*used).border_top = resolved.top.border_data.width;
                (*used).border_right = resolved.right.border_data.width;
                (*used).border_bottom = resolved.bottom.border_data.width;
                (*used).border_left = resolved.left.border_data.width;
            }
            bump(FfiOp::SetOverrideBordersCallback);
            // SAFETY: The host copies the POD border data synchronously.
            unsafe {
                (self.callbacks.set_override_borders_data)(self.callbacks.context, cell.box_, &raw const resolved);
            }
        }
    }

    fn seed_table_participant_used_values(&mut self) {
        for group in self.matching_children(self.table_box, |facts| {
            facts.is_table_row_group || facts.is_table_header_group || facts.is_table_footer_group
        }) {
            self.create_used_values(group, self.participant_constraints);
        }
        for row in &self.rows {
            self.create_used_values(row.box_, self.participant_constraints);
        }
        for cell in &self.cells {
            self.create_used_values(cell.box_, self.participant_constraints);
        }
        for caption in self.matching_children(self.table_box, |facts| facts.is_table_caption) {
            self.create_used_values(caption, self.participant_constraints);
        }
    }

    fn use_fixed_mode_layout(&mut self) -> bool {
        // Implements https://www.w3.org/TR/css-tables-3/#in-fixed-mode.
        // A table-root is said to be laid out in fixed mode whenever the computed value of the table-layout property is equal to fixed, and the
        // specified width of the table root is either a <length-percentage>, min-content or fit-content. When the specified width is not one of
        // those values, or if the computed value of the table-layout property is auto, then the table-root is said to be laid out in auto mode.
        let style = self.style_facts(self.table_box);
        style.table_layout == TABLE_LAYOUT_FIXED
            && (style.width.is_length()
                || style.width.is_percentage()
                || style.width.is_min_content()
                || style.width.is_fit_content())
    }

    fn compute_constrainedness(&mut self) {
        // Definition of constrainedness: https://www.w3.org/TR/css-tables-3/#constrainedness
        // NB: The definition uses https://www.w3.org/TR/CSS21/visudet.html#propdef-width for width, which doesn't include
        //     keyword values. The remaining checks can be simplified to checking whether the size is a length.
        let mut column_index = 0usize;
        for group in self.matching_children(self.table_box, |facts| facts.is_table_column_group) {
            for column in self.matching_children(group, |facts| facts.is_table_column) {
                if self.style_facts(column).width.is_length() {
                    self.columns[column_index].is_constrained = true;
                }
                column_index += self.table_facts(column).raw_column_span as usize;
            }
        }
        for row_index in 0..self.rows.len() {
            let row_box = self.rows[row_index].box_;
            if self.style_facts(row_box).height.is_length() {
                self.rows[row_index].is_constrained = true;
            }
        }
        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let style = self.style_facts(cell.box_);
            if style.width.is_length() {
                self.columns[cell.column_index].is_constrained = true;
            }
            if style.height.is_length() {
                self.rows[cell.row_index].is_constrained = true;
            }
        }
    }

    fn calculate_min_content_inline_size(&self, node: Node) -> CssPixels {
        self.sizing()
            .calculate_min_content_inline_size(node, self.participant_constraints)
    }

    fn calculate_max_content_inline_size(&self, node: Node) -> CssPixels {
        self.sizing()
            .calculate_max_content_inline_size(node, self.participant_constraints)
    }

    fn calculate_min_content_block_size(&self, node: Node, inline_size: CssPixels) -> CssPixels {
        self.sizing()
            .calculate_min_content_block_size(node, inline_size, self.participant_constraints)
    }

    fn calculate_max_content_block_size(&self, node: Node, inline_size: CssPixels) -> CssPixels {
        self.sizing()
            .calculate_max_content_block_size(node, inline_size, self.participant_constraints)
    }

    fn compute_cell_measures(&mut self, include_rows: bool) {
        // Implements https://www.w3.org/TR/css-tables-3/#computing-cell-measures.
        let inline_basis = inline_basis(self.table_constraints);
        let block_basis = block_basis(self.table_constraints);
        self.compute_constrainedness();
        let fixed = self.use_fixed_mode_layout();
        let collapsed = self.style_facts(self.table_box).border_collapse != BORDER_COLLAPSE_SEPARATE;

        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let style = self.style_facts(cell.box_);
            let padding_block_start = style.padding_top.to_px(block_basis);
            let padding_block_end = style.padding_bottom.to_px(block_basis);
            let padding_inline_start = style.padding_left.to_px(inline_basis);
            let padding_inline_end = style.padding_right.to_px(inline_basis);
            let used = self.used_values(cell.box_);
            // Implement the collapsing border model https://www.w3.org/TR/CSS22/tables.html#collapsing-borders.
            let (border_block_start, border_block_end, border_inline_start, border_inline_end) = if collapsed {
                // SAFETY: The entry remains live throughout this pass.
                unsafe {
                    (
                        (*used).border_top_collapsed(true),
                        (*used).border_bottom_collapsed(true),
                        (*used).border_left_collapsed(true),
                        (*used).border_right_collapsed(true),
                    )
                }
            } else {
                (
                    style.border_top_width,
                    style.border_bottom_width,
                    style.border_left_width,
                    style.border_right_width,
                )
            };
            let inline_offsets = padding_inline_start + padding_inline_end + border_inline_start + border_inline_end;
            let mut min_inline = style.min_width.to_px(inline_basis);
            let mut inline_size = if style.width.is_length() {
                style.width.to_px(inline_basis)
            } else {
                CssPixels::default()
            };
            let mut max_inline = if style.max_width.is_length() {
                style.max_width.to_px(inline_basis)
            } else {
                CssPixels::from_raw(i32::MAX)
            };
            if style.box_sizing == BOX_SIZING_BORDER_BOX {
                min_inline -= inline_offsets;
                inline_size -= inline_offsets;
                max_inline -= inline_offsets;
            }

            // https://drafts.csswg.org/css-tables-3/#computing-column-measures
            // For the purpose of measuring a column when laid out in fixed mode [...] the min-content and max-content width
            // of cells is considered zero unless they are directly specified as a length-percentage, in which case they are
            // resolved based on the table width (if it is definite, otherwise use 0).
            let (min_content_inline, max_content_inline) = if fixed {
                if style.width.is_length_percentage() {
                    (inline_size, inline_size)
                } else {
                    (CssPixels::default(), CssPixels::default())
                }
            } else {
                (
                    self.calculate_min_content_inline_size(cell.box_),
                    self.calculate_max_content_inline_size(cell.box_),
                )
            };
            // The outer min-content inline size of a table cell is its minimum inline size adjusted by the cell intrinsic offsets.
            self.cells[cell_index].outer_min_inline_size = min_inline.max(min_content_inline) + inline_offsets;

            if include_rows {
                let min_content_block = self.calculate_min_content_block_size(cell.box_, max_content_inline);
                let max_content_block = self.calculate_max_content_block_size(cell.box_, min_content_inline);
                let min_block = style.min_height.to_px(block_basis);
                let block_offsets = padding_block_start + padding_block_end + border_block_start + border_block_end;
                // The outer min-content block size of a table cell is its minimum block size adjusted by the cell intrinsic offsets.
                self.cells[cell_index].outer_min_block_size = min_block.max(min_content_block) + block_offsets;
                // The tables specification isn't explicit on how to use the height and max-height CSS properties in the outer max-content formulas.
                // However, during this early phase we don't have enough information to resolve percentage sizes yet and the formulas for outer sizes
                // in the specification give enough clues to pick defaults in a way that makes sense.
                let block_size = if style.height.is_length() {
                    style.height.to_px(block_basis)
                } else {
                    CssPixels::default()
                };
                let max_block = if style.max_height.is_length() {
                    style.max_height.to_px(block_basis)
                } else {
                    CssPixels::from_raw(i32::MAX)
                };
                self.cells[cell_index].outer_max_block_size = if self.rows[cell.row_index].is_constrained {
                    // The outer max-content height of a table-cell in a constrained row is
                    // max(min-height, height, min-content height, min(max-height, height)) adjusted by the cell intrinsic offsets.
                    // NB: min(max-height, height) doesn't have any effect here, we can simplify the expression to max(min-height, height, min-content height).
                    min_block.max(block_size.max(min_content_block)) + block_offsets
                } else {
                    // The outer max-content height of a table-cell in a non-constrained row is
                    // max(min-height, height, min-content height, min(max-height, max-content height)) adjusted by the cell intrinsic offsets.
                    min_block.max(block_size.max(min_content_block.max(max_block.min(max_content_block))))
                        + block_offsets
                };
            }

            // See the explanation for block_size and max_block_size above.
            self.cells[cell_index].outer_max_inline_size = if self.columns[cell.column_index].is_constrained {
                // The outer max-content width of a table-cell in a constrained column is
                // max(min-width, width, min-content width, min(max-width, width)) adjusted by the cell intrinsic offsets.

                // AD-HOC: The formula defined by the spec doesn't respect max-width. We use a different formula that
                //         matches the behavior that is expected by WPT and is implemented by other browsers.
                // FIXME: Open a spec issue about this.
                min_inline.max(max_inline.min(inline_size.max(min_content_inline))) + inline_offsets
            } else {
                // The outer max-content width of a table-cell in a non-constrained column is
                // max(min-width, width, min-content width, min(max-width, max-content width)) adjusted by the cell intrinsic offsets.
                min_inline.max(inline_size.max(min_content_inline.max(max_inline.min(max_content_inline))))
                    + inline_offsets
            };
        }
    }

    fn initialize_row_content_sizes(&mut self) {
        let basis = block_basis(self.table_constraints);
        for row_index in 0..self.rows.len() {
            let style = self.style_facts(self.rows[row_index].box_);
            let min_size = style.min_height.to_px(basis);
            let max_size = if style.max_height.is_length() {
                style.max_height.to_px(basis)
            } else {
                CssPixels::from_raw(i32::MAX)
            };
            let size = style.height.to_px(basis);
            // The outer min-content block size of a table row or row group is max(min-block-size, block-size).
            self.rows[row_index].min_size = min_size.max(size);
            // The outer max-content block size is max(min-block-size, min(max-block-size, block-size)).
            self.rows[row_index].max_size = min_size.max(max_size.min(size));
        }
    }

    fn compute_outer_content_sizes(&mut self) {
        let basis = inline_basis(self.table_constraints);
        let mut column_index = 0usize;
        for group in self.matching_children(self.table_box, |facts| facts.is_table_column_group) {
            for column in self.matching_children(group, |facts| facts.is_table_column) {
                let style = self.style_facts(column);
                let min_size = style.min_width.to_px(basis);
                let max_size = if style.max_width.is_length() {
                    style.max_width.to_px(basis)
                } else {
                    CssPixels::from_raw(i32::MAX)
                };
                let size = style.width.to_px(basis);
                // The outer min-content inline size of a table-column or table-column-group is max(min-width, width).
                self.columns[column_index].min_size = min_size.max(size);
                // The outer max-content inline size of a table-column or table-column-group is max(min-width, min(max-width, width)).
                self.columns[column_index].max_size = min_size.max(max_size.min(size));
                column_index += self.table_facts(column).raw_column_span as usize;
            }
        }
        self.initialize_row_content_sizes();
    }

    // Accessors to enable direction-agnostic table measurement.

    fn track_count(&self, axis: TrackAxis) -> usize {
        match axis {
            TrackAxis::Row => self.rows.len(),
            TrackAxis::Column => self.columns.len(),
        }
    }

    fn track_min(&self, axis: TrackAxis, index: usize) -> CssPixels {
        match axis {
            TrackAxis::Row => self.rows[index].min_size,
            TrackAxis::Column => self.columns[index].min_size,
        }
    }

    fn set_track_min(&mut self, axis: TrackAxis, index: usize, value: CssPixels) {
        match axis {
            TrackAxis::Row => self.rows[index].min_size = value,
            TrackAxis::Column => self.columns[index].min_size = value,
        }
    }

    fn track_max(&self, axis: TrackAxis, index: usize) -> CssPixels {
        match axis {
            TrackAxis::Row => self.rows[index].max_size,
            TrackAxis::Column => self.columns[index].max_size,
        }
    }

    fn set_track_max(&mut self, axis: TrackAxis, index: usize, value: CssPixels) {
        match axis {
            TrackAxis::Row => self.rows[index].max_size = value,
            TrackAxis::Column => self.columns[index].max_size = value,
        }
    }

    fn track_percentage(&self, axis: TrackAxis, index: usize) -> f64 {
        match axis {
            TrackAxis::Row => self.rows[index].intrinsic_percentage,
            TrackAxis::Column => self.columns[index].intrinsic_percentage,
        }
    }

    fn set_track_percentage(&mut self, axis: TrackAxis, index: usize, value: f64) {
        match axis {
            TrackAxis::Row => self.rows[index].intrinsic_percentage = value,
            TrackAxis::Column => self.columns[index].intrinsic_percentage = value,
        }
    }

    fn set_track_has_percentage(&mut self, axis: TrackAxis, index: usize, value: bool) {
        match axis {
            TrackAxis::Row => self.rows[index].has_intrinsic_percentage = value,
            TrackAxis::Column => self.columns[index].has_intrinsic_percentage = value,
        }
    }

    fn cell_span(cell: Cell, axis: TrackAxis) -> usize {
        match axis {
            TrackAxis::Row => cell.row_span,
            TrackAxis::Column => cell.column_span,
        }
    }

    fn cell_index(cell: Cell, axis: TrackAxis) -> usize {
        match axis {
            TrackAxis::Row => cell.row_index,
            TrackAxis::Column => cell.column_index,
        }
    }

    fn cell_min(cell: Cell, axis: TrackAxis) -> CssPixels {
        match axis {
            TrackAxis::Row => cell.outer_min_block_size,
            TrackAxis::Column => cell.outer_min_inline_size,
        }
    }

    fn cell_max(cell: Cell, axis: TrackAxis) -> CssPixels {
        match axis {
            TrackAxis::Row => cell.outer_max_block_size,
            TrackAxis::Column => cell.outer_max_inline_size,
        }
    }

    fn cell_percentage(style: FfiStyleFacts, axis: TrackAxis) -> f64 {
        // Definition of percentage contribution: https://www.w3.org/TR/css-tables-3/#percentage-contribution
        let (size, max_size) = match axis {
            TrackAxis::Row => (style.height, style.max_height),
            TrackAxis::Column => (style.width, style.max_width),
        };
        let maximum = if max_size.is_percentage() {
            max_size.fraction * 100.0
        } else {
            f64::INFINITY
        };
        let preferred = if size.is_percentage() {
            size.fraction * 100.0
        } else {
            0.0
        };
        preferred.min(maximum)
    }

    fn initialize_intrinsic_percentages(&mut self, axis: TrackAxis) {
        if axis == TrackAxis::Row {
            for index in 0..self.rows.len() {
                let style = self.style_facts(self.rows[index].box_);
                // Definition of percentage contribution: https://www.w3.org/TR/css-tables-3/#percentage-contribution
                self.rows[index].has_intrinsic_percentage =
                    style.max_height.is_percentage() || style.height.is_percentage();
                self.rows[index].intrinsic_percentage = Self::cell_percentage(style, axis);
            }
        } else {
            let mut column_index = 0usize;
            for group in self.matching_children(self.table_box, |facts| facts.is_table_column_group) {
                for column in self.matching_children(group, |facts| facts.is_table_column) {
                    let style = self.style_facts(column);
                    // Definition of percentage contribution: https://www.w3.org/TR/css-tables-3/#percentage-contribution
                    self.columns[column_index].has_intrinsic_percentage =
                        style.max_width.is_percentage() || style.width.is_percentage();
                    self.columns[column_index].intrinsic_percentage = Self::cell_percentage(style, axis);
                    column_index += self.table_facts(column).raw_column_span as usize;
                }
            }
        }

        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let style = self.style_facts(cell.box_);
            let size = match axis {
                TrackAxis::Row => style.height,
                TrackAxis::Column => style.width,
            };
            if !size.is_percentage() {
                continue;
            }
            let start = Self::cell_index(cell, axis);
            let span = Self::cell_span(cell, axis);
            for index in start..start + span {
                self.set_track_has_percentage(axis, index, true);
            }
            if span == 1 {
                self.set_track_percentage(
                    axis,
                    start,
                    self.track_percentage(axis, start)
                        .max(Self::cell_percentage(style, axis)),
                );
            }
        }
    }

    fn compute_intrinsic_percentage(&mut self, axis: TrackAxis, max_span: usize) {
        // https://www.w3.org/TR/css-tables-3/#intrinsic-percentage-width-of-a-column-based-on-cells-of-span-up-to-1
        self.initialize_intrinsic_percentages(axis);
        let count = self.track_count(axis);
        // Stores intermediate values for intrinsic percentage based on cells of span up to N for the iterative algorithm, to store them back at the end of the step.
        let mut contributions = (0..count)
            .map(|index| self.track_percentage(axis, index))
            .collect::<Vec<_>>();
        for current_span in 2..=max_span {
            // https://www.w3.org/TR/css-tables-3/#intrinsic-percentage-width-of-a-column-based-on-cells-of-span-up-to-n-n--1
            for cell_index in 0..self.cells.len() {
                let cell = self.cells[cell_index];
                if Self::cell_span(cell, axis) != current_span {
                    continue;
                }
                let style = self.style_facts(cell.box_);
                let start = Self::cell_index(cell, axis);
                let end = start + current_span;
                // 1. Start with the percentage contribution of the cell.
                let mut contribution = CssPixels::nearest_value_for(Self::cell_percentage(style, axis));
                // 2. Subtract the intrinsic percentage width of the column based on cells of span up to N-1 of all columns
                //    that the cell spans. If this gives a negative result, change it to 0%.
                for index in start..end {
                    contribution -= CssPixels::nearest_value_for(self.track_percentage(axis, index));
                    contribution = contribution.max(CssPixels::default());
                }
                // Compute the sum of the non-spanning max-content sizes of all rows / columns spanned by the cell that have an intrinsic percentage
                // size of the row / column based on cells of span up to N-1 equal to 0%, to be used in step 3 of the cell contribution algorithm.
                let mut zero_sum = CssPixels::default();
                let mut zero_count = 0usize;
                for index in start..end {
                    if self.track_percentage(axis, index) == 0.0 {
                        zero_sum += self.track_max(axis, index);
                        zero_count += 1;
                    }
                }
                for (index, saved) in contributions.iter_mut().enumerate().take(end).skip(start) {
                    // If the intrinsic percentage width of a column based on cells of span up to N-1 is greater than 0%, then the intrinsic percentage width of
                    // the column based on cells of span up to N is the same as the intrinsic percentage width of the column based on cells of span up to N-1.
                    if self.track_percentage(axis, index) > 0.0 {
                        continue;
                    }
                    // Otherwise, it is the largest of the contributions of the cells in the column whose colSpan is N,
                    // where the contribution of a cell is the result of taking the following steps:
                    // 1. Start with the percentage contribution of the cell.
                    // 2. Subtract the intrinsic percentage width of the column based on cells of span up to N-1 of all columns
                    //    that the cell spans. If this gives a negative result, change it to 0%.
                    // 3. Multiply by the ratio of the column’s non-spanning max-content width to the sum of the non-spanning max-content widths of all
                    //    columns spanned by the cell that have an intrinsic percentage width of the column based on cells of span up to N-1 equal to 0%.
                    let adjusted = if zero_sum != CssPixels::default() {
                        contribution.scaled(self.track_max(axis, index).to_double() / zero_sum.to_double())
                    } else {
                        // However, if this ratio is undefined because the denominator is zero, instead use the 1 divided by the number of columns
                        // spanned by the cell that have an intrinsic percentage width of the column based on cells of span up to N-1 equal to zero.
                        contribution / zero_count
                    };
                    *saved = saved.max(adjusted.to_double());
                }
            }
            for (index, value) in contributions.iter().copied().enumerate() {
                self.set_track_percentage(axis, index, value);
            }
        }
        // Clamp total intrinsic percentage to 100%: https://www.w3.org/TR/css-tables-3/#intrinsic-percentage-width-of-a-column
        let mut total = 0.0;
        for index in 0..count {
            let value = self.track_percentage(axis, index).min(100.0 - total).max(0.0);
            self.set_track_percentage(axis, index, value);
            total += value;
        }
    }

    fn initialize_table_measures(&mut self, axis: TrackAxis) {
        if axis == TrackAxis::Row {
            let basis = block_basis(self.table_constraints);
            for cell_index in 0..self.cells.len() {
                let cell = self.cells[cell_index];
                if cell.row_span == 1 {
                    let specified = self.style_facts(cell.box_).height.to_px(basis);
                    // https://www.w3.org/TR/css-tables-3/#row-layout makes specified cell height part of the initialization formula for row table measures:
                    // This is done by running the same algorithm as the column measurement, with the span=1 value being initialized (for min-content) with
                    // the largest of the resulting height of the previous row layout, the height specified on the corresponding table-row (if any), and
                    // the largest height specified on cells that span this row only (the algorithm starts by considering cells of span 2 on top of that assignment).
                    let row = &mut self.rows[cell.row_index];
                    row.min_size = row.min_size.max(cell.outer_min_block_size.max(specified));
                    row.max_size = row.max_size.max(cell.outer_max_block_size);
                }
            }
        } else {
            // Implement the following parts of the specification, accounting for fixed layout mode:
            // https://www.w3.org/TR/css-tables-3/#min-content-width-of-a-column-based-on-cells-of-span-up-to-1
            // https://www.w3.org/TR/css-tables-3/#max-content-width-of-a-column-based-on-cells-of-span-up-to-1
            let fixed = self.use_fixed_mode_layout();
            for cell in self.cells.iter().copied() {
                if cell.column_span == 1 && (cell.row_index == 0 || !fixed) {
                    let column = &mut self.columns[cell.column_index];
                    column.min_size = column.min_size.max(cell.outer_min_inline_size);
                    column.max_size = column.max_size.max(cell.outer_max_inline_size);
                }
            }
        }
    }

    fn compute_table_measures(&mut self, axis: TrackAxis) {
        self.initialize_table_measures(axis);
        let max_span = self
            .cells
            .iter()
            .copied()
            .map(|cell| Self::cell_span(cell, axis))
            .max()
            .unwrap_or(1)
            .max(1);
        // Since the intrinsic percentage specification uses non-spanning max-content size for the iterative algorithm,
        // run it before we compute the spanning max-content size with its own iterative algorithm for span up to N.
        self.compute_intrinsic_percentage(axis, max_span);
        let track_count = self.track_count(axis);
        for current_span in 2..=max_span {
            // https://www.w3.org/TR/css-tables-3/#min-content-width-of-a-column-based-on-cells-of-span-up-to-n-n--1
            let mut min_contributions = vec![Vec::new(); track_count];
            // https://www.w3.org/TR/css-tables-3/#max-content-width-of-a-column-based-on-cells-of-span-up-to-n-n--1
            let mut max_contributions = vec![Vec::new(); track_count];
            let track_spacing = match axis {
                TrackAxis::Row => self.border_spacing_block(),
                TrackAxis::Column => self.border_spacing_inline(),
            };
            for cell in self.cells.iter().copied() {
                if Self::cell_span(cell, axis) != current_span {
                    continue;
                }
                let start = Self::cell_index(cell, axis);
                let end = start + current_span;
                // Define the baseline max-content size as the sum of the max-content sizes based on cells of span up to N-1 of all columns that the cell spans.
                let baseline_max =
                    (start..end).fold(CssPixels::default(), |sum, index| sum + self.track_max(axis, index));
                let baseline_min =
                    (start..end).fold(CssPixels::default(), |sum, index| sum + self.track_min(axis, index));
                // Define the baseline border spacing as the sum of the horizontal border-spacing for any columns spanned by the cell, other than the one in which the cell originates.
                let spacing = track_spacing * (current_span - 1);
                // Add contribution from all rows / columns, since we've weighted the gap to the desired spanned size by the the
                // ratio of the max-content size based on cells of span up to N-1 of the row / column to the baseline max-content width.
                for index in start..end {
                    // The contribution of the cell is the sum of:
                    // the min-content size of the column based on cells of span up to N-1
                    let mut min_contribution = self.track_min(axis, index);
                    // the product of:
                    // - the ratio of:
                    //   - the max-content size of the row / column based on cells of span up to N-1 of the row / column minus the
                    //     min-content size of the row / column based on cells of span up to N-1 of the row / column, to
                    //   - the baseline max-content size minus the baseline min-content size
                    //   or zero if this ratio is undefined, and
                    // - the outer min-content size of the cell minus the baseline min-content size and the baseline border spacing, clamped
                    //   to be at least 0 and at most the difference between the baseline max-content size and the baseline min-content size
                    let normalized = if baseline_max != baseline_min {
                        (self.track_max(axis, index) - self.track_min(axis, index)).to_double()
                            / (baseline_max - baseline_min).to_double()
                    } else {
                        0.0
                    };
                    let clamped = (Self::cell_min(cell, axis) - baseline_min - spacing)
                        .max(CssPixels::default())
                        .min(baseline_max - baseline_min);
                    min_contribution += CssPixels::nearest_value_for(normalized * clamped.to_double());
                    // the product of:
                    // - the ratio of the max-content size based on cells of span up to N-1 of the column to the baseline max-content size
                    // - the outer min-content size of the cell minus the baseline max-content size and baseline border spacing, or 0 if this is negative
                    if baseline_max != CssPixels::default() {
                        min_contribution += CssPixels::nearest_value_for(
                            self.track_max(axis, index).to_double() / baseline_max.to_double(),
                        ) * (Self::cell_min(cell, axis) - baseline_max - spacing)
                            .max(CssPixels::default());
                    } else {
                        // AD-HOC: The spec does not define behavior when baseline is zero. We distribute equally.
                        //         This matches how undefined ratios are handled elsewhere.
                        min_contribution +=
                            (Self::cell_min(cell, axis) - spacing).max(CssPixels::default()) / current_span;
                    }

                    // The contribution of the cell is the sum of:
                    // the max-content size of the column based on cells of span up to N-1
                    let mut max_contribution = self.track_max(axis, index);
                    // and the product of:
                    // - the ratio of the max-content size based on cells of span up to N-1 of the column to the baseline max-content size
                    // - the outer max-content size of the cell minus the baseline max-content size and the baseline border spacing, or 0 if this is negative
                    if baseline_max != CssPixels::default() {
                        max_contribution += CssPixels::nearest_value_for(
                            self.track_max(axis, index).to_double() / baseline_max.to_double(),
                        ) * (Self::cell_max(cell, axis) - baseline_max - spacing)
                            .max(CssPixels::default());
                    } else {
                        // AD-HOC: The spec does not define behavior when baseline is zero. We distribute equally,
                        //         This matches how undefined ratios are handled elsewhere.
                        max_contribution +=
                            (Self::cell_max(cell, axis) - spacing).max(CssPixels::default()) / current_span;
                    }
                    min_contributions[index].push(min_contribution);
                    max_contributions[index].push(max_contribution);
                }
            }
            for index in 0..track_count {
                // min-content size of a row / column based on cells of span up to N (N > 1) is
                // the largest of the min-content size of the row / column based on cells of span up to N-1 and
                // the contributions of the cells in the row / column whose rowSpan / colSpan is N
                let mut min_size = self.track_min(axis, index);
                for contribution in &min_contributions[index] {
                    min_size = min_size.max(*contribution);
                }
                self.set_track_min(axis, index, min_size);
                // max-content size of a row / column based on cells of span up to N (N > 1) is
                // the largest of the max-content size based on cells of span up to N-1 and the contributions of
                // the cells in the row / column whose rowSpan / colSpan is N
                let mut max_size = self.track_max(axis, index);
                for contribution in &max_contributions[index] {
                    max_size = max_size.max(*contribution);
                }
                self.set_track_max(axis, index, max_size);
            }
        }
    }

    fn compute_capmin(&mut self) -> CssPixels {
        // The caption width minimum (CAPMIN) is the largest of the table captions min-content contribution:
        // https://drafts.csswg.org/css-tables-3/#computing-the-table-width
        let basis = inline_basis(self.table_constraints);
        let mut capmin = CssPixels::default();
        for caption in self.matching_children(self.table_box, |facts| facts.is_table_caption) {
            let style = self.style_facts(caption);
            let outer = |inner: CssPixels| {
                inner
                    + style.margin_left.to_px(basis)
                    + style.border_left_width
                    + style.padding_left.to_px(basis)
                    + style.padding_right.to_px(basis)
                    + style.border_right_width
                    + style.margin_right.to_px(basis)
            };
            let mut contribution = outer(self.calculate_min_content_inline_size(caption));
            if !style.width.is_auto() && !style.width.contains_percentage {
                let preferred = self.sizing().calculate_inner_inline_width(
                    caption,
                    AvailableSize::definite(basis),
                    self.participant_constraints,
                );
                contribution = contribution.max(outer(preferred));
            }
            capmin = capmin.max(contribution);
        }
        capmin
    }

    fn resolve_inline_constraint(
        &mut self,
        constraint: FfiSizeValue,
        grid_min: CssPixels,
        grid_max: CssPixels,
        basis: CssPixels,
    ) -> CssPixels {
        if constraint.is_min_content() {
            return grid_min;
        }
        if constraint.is_max_content() {
            return grid_max;
        }
        if constraint.is_fit_content() {
            let limit = if constraint.calc.is_null()
                && constraint.px == CssPixels::default()
                && !constraint.contains_percentage
            {
                basis
            } else {
                constraint.to_px(basis)
            };
            return grid_max.min(grid_min.max(limit));
        }
        // CSS Sizing says box-sizing:border-box applies length/percentage width/min-width/max-width constraints to
        // the border box. The table inline-size algorithm compares content inline sizes, so convert them before comparing.
        let mut resolved = constraint.to_px(basis);
        if self.style_facts(self.table_box).box_sizing == BOX_SIZING_BORDER_BOX {
            let used = self.used_values(self.table_box);
            // SAFETY: Table state is live and is not a collapsed cell.
            unsafe {
                resolved -= (*used).border_box_left(false) + (*used).border_box_right(false);
            }
        }
        resolved.max(CssPixels::default())
    }

    fn should_treat_max_inline_size_as_none(&self, available: AvailableSize) -> bool {
        self.sizing()
            .should_treat_max_inline_size_as_none(self.table_box, available, self.table_constraints)
    }

    fn compute_table_inline_size(&mut self) {
        // https://drafts.csswg.org/css-tables-3/#computing-the-table-width

        let table_style = self.style_facts(self.table_box);
        let available_inline = self.available_space.inline_size;
        // Percentages on 'width' and 'height' on the table are relative to the table wrapper box's containing block,
        // not the table wrapper box itself.
        let basis = inline_basis(self.table_constraints);
        // Compute undistributable space due to border spacing: https://www.w3.org/TR/css-tables-3/#computing-undistributable-space.
        let spacing = (self.columns.len() + 1) * self.border_spacing_inline();
        // The row/column-grid inline-size minimum (GRIDMIN) is the sum of the min-content inline size
        // of all the columns plus cell spacing or borders.
        let grid_min = self
            .columns
            .iter()
            .fold(CssPixels::default(), |sum, column| sum + column.min_size)
            + spacing;
        // The row/column-grid inline-size maximum (GRIDMAX) is the sum of the max-content inline size
        // of all the columns plus cell spacing or borders.
        let grid_max = self
            .columns
            .iter()
            .fold(CssPixels::default(), |sum, column| sum + column.max_size)
            + spacing;
        let capmin = self.compute_capmin();
        // The used minimum inline size of a table is the greater of the resolved min-width, CAPMIN, and GRIDMIN.
        let mut used_min = grid_min.max(capmin);
        if !table_style.min_width.is_auto() {
            used_min = used_min.max(self.resolve_inline_constraint(table_style.min_width, grid_min, grid_max, basis));
        }
        let width_is_auto_or_indefinite_percentage = table_style.width.is_auto()
            || (table_style.width.contains_percentage && !self.table_constraints.has_percentage_basis_inline_size);
        let mut used = if width_is_auto_or_indefinite_percentage {
            // If the table-root has 'width: auto', the used inline size is the greater of
            // min(GRIDMAX, the table’s containing block inline size), the used minimum inline size of the table.
            // NOTE: In normal layout the available inline size already is the wrapper's used inline size, which the
            //       parent context resolved with shrink-to-fit; filling it keeps the table and its
            //       wrapper consistent without reading the wrapper's state from here.
            let mut value = match available_inline.type_ {
                AvailableSizeType::MinContent => grid_min,
                AvailableSizeType::MaxContent => grid_max,
                AvailableSizeType::Definite if self.layout_mode == LAYOUT_MODE_NORMAL => {
                    available_inline.value.max(used_min)
                }
                AvailableSizeType::Definite => grid_max.min(available_inline.value).max(used_min),
                AvailableSizeType::Indefinite => grid_max.max(used_min),
            };
            if !matches!(
                available_inline.type_,
                AvailableSizeType::MinContent | AvailableSizeType::MaxContent
            ) {
                // https://www.w3.org/TR/CSS22/tables.html#auto-table-layout
                // A percentage value for a column inline size is relative to the table inline size. If the table has
                // 'width: auto', a percentage represents a constraint on the column's inline size, which a UA should try to satisfy.
                for cell_index in 0..self.cells.len() {
                    let cell = self.cells[cell_index];
                    let cell_width = self.style_facts(cell.box_).width;
                    if cell_width.is_percentage() {
                        let mut adjusted = spacing;
                        let percentage = cell_width.fraction * 100.0;
                        if percentage != 0.0 {
                            adjusted += CssPixels::nearest_value_for(
                                (100.0 / percentage * cell.outer_max_inline_size.to_double()).ceil(),
                            );
                        }
                        value = if available_inline.type_ == AvailableSizeType::Definite {
                            value.max(adjusted).min(available_inline.value)
                        } else {
                            value.max(adjusted)
                        };
                    }
                }
            }
            value
        } else if table_style.width.is_max_content() {
            grid_max
        } else {
            // If the table-root’s width property has a computed value (resolving to the table inline size) other than auto,
            // the used inline size is the greater of the resolved table inline size and the used minimum inline size.
            let mut value = self
                .resolve_inline_constraint(table_style.width, grid_min, grid_max, basis)
                .max(used_min);
            if !self.should_treat_max_inline_size_as_none(available_inline) {
                value = value.min(self.resolve_inline_constraint(table_style.max_width, grid_min, grid_max, basis));
            }
            value
        };
        if !self.should_treat_max_inline_size_as_none(available_inline) {
            used = used.min(self.resolve_inline_constraint(table_style.max_width, grid_min, grid_max, basis));
        }
        used = used.max(used_min);
        let table_used = self.used_values(self.table_box);
        // SAFETY: Unique table state entry.
        unsafe {
            (*table_used).set_content_inline_size(used);
        }
    }

    fn can_skip_row_intrinsic_measurement(&mut self) -> bool {
        for row_index in 0..self.rows.len() {
            if self.rows[row_index].is_collapsed {
                return false;
            }
            let style = self.style_facts(self.rows[row_index].box_);
            if !style.height.is_auto() || !style.min_height.is_auto() || !style.max_height.is_none() {
                return false;
            }
        }
        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            if cell.row_span != 1 {
                return false;
            }
            let style = self.style_facts(cell.box_);
            if !style.height.is_auto() || !style.min_height.is_auto() || !style.max_height.is_none() {
                return false;
            }
        }
        true
    }

    fn run_until_inline_size_calculation(&mut self, input: FfiLayoutInput, skip_row_measurement: bool) {
        self.available_space = input.available_space;
        // Determine the number of rows/columns the table requires.
        let table_grid = grid::calculate(self, self.table_box);
        self.cells = table_grid.cells;
        self.rows = table_grid.rows;
        self.columns = vec![Column::default(); table_grid.column_count];
        for cell in &self.cells {
            self.columns[cell.column_index].has_originating_cells = true;
        }

        // The containing block of every internal table box and caption is the table wrapper;
        // the table's own input carries the wrapper's constraints, and participant percentages
        // resolve against those. Percentage block sizes of participants only resolve once the table
        // itself has a non-auto block size.
        self.table_constraints = input.containing_block_constraints;
        let table_height_auto = self.style_facts(self.table_box).height.is_auto();
        self.participant_constraints = FfiContainingBlockConstraints {
            has_percentage_basis_inline_size: self.table_constraints.has_percentage_basis_inline_size,
            percentage_basis_inline_size: self.table_constraints.percentage_basis_inline_size,
            has_percentage_basis_block_size: !table_height_auto
                && self.table_constraints.has_percentage_basis_block_size,
            percentage_basis_block_size: self.table_constraints.percentage_basis_block_size,
            has_quirks_mode_percentage_basis_block_size: self
                .table_constraints
                .has_quirks_mode_percentage_basis_block_size,
            quirks_mode_percentage_basis_block_size: self.table_constraints.quirks_mode_percentage_basis_block_size,
        };
        self.seed_table_participant_used_values();
        self.border_conflict_resolution();

        let mut include_rows = !skip_row_measurement;
        self.needs_fixed_mode_row_measurement = false;
        // OPTIMIZATION: Row intrinsic measurements are only needed when row block-size constraints or row spans can affect
        //               the later row distribution. Simple tables get their actual row block sizes from cell layout.
        if include_rows && self.can_skip_row_intrinsic_measurement() {
            include_rows = false;
        }
        if include_rows && self.use_fixed_mode_layout() {
            // https://drafts.csswg.org/css-tables-3/#computing-column-measures
            // For the purpose of measuring a column when laid out in fixed mode ... the min-content and max-content width
            // of cells is considered zero.
            //
            // https://drafts.csswg.org/css-tables-3/#ROWMIN
            // ROWMIN is defined as the sum of the minimum block sizes of the rows after a first row layout pass.
            // NB: So defer fixed-mode row measurement until after columns have their used widths.
            include_rows = false;
            self.needs_fixed_mode_row_measurement = true;
        }
        // Compute the minimum width of each column.
        self.compute_cell_measures(include_rows);
        self.compute_outer_content_sizes();
        self.compute_table_measures(TrackAxis::Column);
        if include_rows {
            // https://www.w3.org/TR/css-tables-3/#row-layout
            // Since specified cell block sizes were ignored during row layout and cells spanning multiple rows were not
            // sized correctly, their block size must eventually be distributed to the rows they span. This is done
            // by running the same algorithm as the column measurement, with the span=1 value being initialized (for min-content) with the largest
            // of the resulting block size of the previous row layout, the size specified on the corresponding table row,
            // and the largest block size specified on cells that span only this row.
            self.compute_table_measures(TrackAxis::Row);
        }
        // Compute the inline size of the table.
        self.compute_table_inline_size();
    }

    fn layout_inside_cell(&self, cell: Cell, input: AvailableSpace) -> Option<FfiChildLayoutResult> {
        let mut result = FfiChildLayoutResult::default();
        bump(FfiOp::LayoutInsideCallback);
        // SAFETY: The host keeps the returned child context until one of the
        // matching completion callbacks below.
        let created = unsafe {
            (self.callbacks.layout_inside_child)(
                self.callbacks.context,
                cell.box_,
                self.layout_mode,
                FfiLayoutInput {
                    available_space: input,
                    containing_block_constraints: FfiContainingBlockConstraints::default(),
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
                &raw mut result,
            )
        };
        created.then_some(result)
    }

    fn finish_child_layout(&self, cell: Node) {
        bump(FfiOp::ParentDidDimensionCallback);
        // SAFETY: This matches a successful layout_inside_child call.
        unsafe {
            (self.callbacks.parent_did_dimension_child_root_box)(self.callbacks.context, cell);
        }
    }

    fn box_baseline(&self, node: Node) -> CssPixels {
        bump(FfiOp::BoxBaselineCallback);
        // SAFETY: Baseline set 0 is First and is statically pinned by C++.
        unsafe { (self.callbacks.box_baseline)(self.callbacks.context, node, 0) }
    }

    fn measure_cell(
        &self,
        cell: Cell,
        used: *const UsedValuesCore,
        inner: AvailableSpace,
    ) -> Option<FfiMeasuredCellContent> {
        // The table formatting context owns the cell's outer geometry. Seed the inputs
        // needed to lay out its contents without copying placement or layout outputs.
        let mut measured = FfiMeasuredCellContent::default();
        bump(FfiOp::CellMeasurementCallback);
        // SAFETY: The host copies scalar input and returns scalar output.
        let did_measure = unsafe {
            (self.callbacks.measure_table_cell_content)(
                self.callbacks.context,
                cell.box_,
                self.layout_mode,
                used,
                inner,
                &raw mut measured,
            )
        };
        did_measure.then_some(measured)
    }

    fn compute_table_block_size(&mut self) {
        // First pass of row block-size calculation:
        for row_index in 0..self.rows.len() {
            if self.rows[row_index].is_collapsed {
                self.rows[row_index].base_block_size = CssPixels::default();
                continue;
            }
            let style = self.style_facts(self.rows[row_index].box_);
            if style.height.is_length() {
                // NOTE: A <length> block size resolves without a percentage basis.
                self.rows[row_index].base_block_size = self.rows[row_index]
                    .base_block_size
                    .max(style.height.to_px(CssPixels::default()));
            }
        }
        let inline_basis = inline_basis(self.participant_constraints);
        let participant_block_basis = block_basis(self.participant_constraints);
        let collapsed = self.style_facts(self.table_box).border_collapse != BORDER_COLLAPSE_SEPARATE;
        let inline_spacing = self.border_spacing_inline();
        // First pass of cells layout:
        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let style = self.style_facts(cell.box_);
            let span_inline = (0..cell.column_span).fold(CssPixels::default(), |sum, index| {
                sum + self.columns[cell.column_index + index].used_inline_size
            });
            let used = self.used_values(cell.box_);
            // SAFETY: Unique cell entry; no callback occurs while borrowed.
            unsafe {
                (*used).padding_top = style.padding_top.to_px(inline_basis);
                (*used).padding_bottom = style.padding_bottom.to_px(inline_basis);
                (*used).padding_left = style.padding_left.to_px(inline_basis);
                (*used).padding_right = style.padding_right.to_px(inline_basis);
                if !collapsed {
                    (*used).border_top = style.border_top_width;
                    (*used).border_bottom = style.border_bottom_width;
                    (*used).border_left = style.border_left_width;
                    (*used).border_right = style.border_right_width;
                }
                if !self.rows[cell.row_index].is_collapsed && style.height.is_length() {
                    let cell_size = style.height.to_px(participant_block_basis);
                    (*used).set_content_block_size(
                        cell_size - (*used).border_box_top(collapsed) - (*used).border_box_bottom(collapsed),
                    );
                    self.rows[cell.row_index].base_block_size =
                        self.rows[cell.row_index].base_block_size.max(cell_size);
                }
                // Compute cell inline size as specified by https://www.w3.org/TR/css-tables-3/#bounding-box-assignment:
                // The position of any table cell, track, or track group is defined by the sums of its spanned columns and rows:
                // - the inline/block sizes of all spanned visible columns/rows
                // - the inline/block border spacing times the amount of spanned visible columns/rows minus one
                // FIXME: Account for visibility.
                (*used).set_content_inline_size(
                    span_inline - (*used).border_box_left(collapsed) - (*used).border_box_right(collapsed)
                        + inline_spacing * (cell.column_span - 1),
                );
            }

            let outer_space = self.available_space;
            // SAFETY: Read-only access to the live cell entry.
            let inner = unsafe { (*used).available_inner_space_or_constraints_from(outer_space) };
            let mut measured_baseline = None;
            if style.height.is_percentage() {
                // This cell's final inside layout happens in the second pass below; measure its
                // content in a throwaway state instead of laying out the committing state twice.
                if let Some(measured) = self.measure_cell(cell, used, inner) {
                    // SAFETY: Unique cell entry.
                    unsafe {
                        (*used).set_content_block_size(measured.content_block_size);
                    }
                    measured_baseline = Some(measured.first_baseline);
                }
            } else if let Some(result) = self.layout_inside_cell(cell, inner) {
                // SAFETY: Unique cell entry.
                unsafe {
                    (*used).set_content_block_size(result.automatic_content_block_size);
                }
                self.finish_child_layout(cell.box_);
            }
            if self.needs_fixed_mode_row_measurement {
                let min_size = style.min_height.to_px(participant_block_basis);
                // SAFETY: Read-only geometry calculation.
                let offsets = unsafe { (*used).border_box_top(collapsed) + (*used).border_box_bottom(collapsed) };
                // SAFETY: Read-only geometry calculation.
                let measured = unsafe { (*used).border_box_block_size(collapsed).max(min_size + offsets) };
                self.cells[cell_index].outer_min_block_size = measured;
                self.cells[cell_index].outer_max_block_size = measured;
            }
            // https://drafts.csswg.org/css2/#height-layout
            // The baseline of a cell is the baseline of the first in-flow line box in the cell, or the first in-flow
            // table-row in the cell, whichever comes first.
            let baseline = measured_baseline.unwrap_or_else(|| self.box_baseline(cell.box_));
            self.cells[cell_index].baseline = baseline;
            // Implements https://www.w3.org/TR/css-tables-3/#computing-the-table-height

            // The minimum block size of a row is the maximum of:
            // - the computed block size (if definite, percentages being considered 0px) of its corresponding table row,
            // - the computed block size of each cell spanning the current row exclusively (if definite, percentages being treated as 0px), and
            // - the minimum block size (ROWMIN) required by the cells spanning the row.
            // Note that we've already applied the first rule at the top of the method.
            if !self.rows[cell.row_index].is_collapsed {
                if cell.row_span == 1 {
                    // SAFETY: Read-only cell geometry.
                    let size = unsafe { (*used).border_box_block_size(collapsed) };
                    self.rows[cell.row_index].base_block_size = self.rows[cell.row_index].base_block_size.max(size);
                }
                if !self.needs_fixed_mode_row_measurement {
                    self.rows[cell.row_index].base_block_size = self.rows[cell.row_index]
                        .base_block_size
                        .max(self.rows[cell.row_index].min_size);
                }
                self.rows[cell.row_index].baseline = self.rows[cell.row_index].baseline.max(baseline);
            }
        }

        if self.needs_fixed_mode_row_measurement {
            self.initialize_row_content_sizes();
            self.compute_table_measures(TrackAxis::Row);
            for row in &mut self.rows {
                if !row.is_collapsed {
                    row.base_block_size = row.base_block_size.max(row.min_size);
                }
            }
        }
        self.table_block_size = self
            .rows
            .iter()
            .fold(CssPixels::default(), |sum, row| sum + row.base_block_size);
        if let Some(minimum) = self.min_border_box_block_size_from_flex_item {
            let used = self.used_values(self.table_box);
            // SAFETY: Read-only table geometry.
            let content_min = unsafe { minimum - (*used).border_box_top(false) - (*used).border_box_bottom(false) };
            self.table_block_size = self.table_block_size.max(content_min);
        }
        let table_style = self.style_facts(self.table_box);
        if !table_style.height.is_auto() {
            // If the table has a `height` property other than auto, it is treated as a minimum block size for the
            // table grid, and will eventually be distributed to the rows if their collective minimum block size is smaller.
            let mut specified = table_style.height.to_px(block_basis(self.table_constraints));
            if table_style.box_sizing == BOX_SIZING_BORDER_BOX {
                let used = self.used_values(self.table_box);
                // SAFETY: Read-only table geometry.
                unsafe {
                    specified -= (*used).border_box_top(false) + (*used).border_box_bottom(false);
                }
            }
            self.table_block_size = self.table_block_size.max(specified);
        }
        for row in &mut self.rows {
            // Reference size is the largest of
            // - its initial base block size and
            // - its new base block size (the one evaluated during the second layout pass, where percentages used in
            //   row groups, rows, and cells were resolved according to the table block size, instead of
            //   being ignored as 0px).

            // Assign reference size to base size. Later, the reference size might change to a larger value during
            // the second pass of rows layout.
            row.reference_block_size = row.base_block_size;
        }
        // Second pass of row block-size calculation:
        // At this point, percentage row block sizes can be resolved because the final table block size is calculated.
        for row_index in 0..self.rows.len() {
            if self.rows[row_index].is_collapsed {
                self.rows[row_index].reference_block_size = CssPixels::default();
                continue;
            }
            let style = self.style_facts(self.rows[row_index].box_);
            if style.height.is_percentage() {
                let used = style.height.to_px(self.table_block_size);
                self.rows[row_index].reference_block_size = self.rows[row_index].reference_block_size.max(used);
            }
        }
        // Second pass cells layout:
        // At this point, percentage cell block sizes can be resolved because the final table block size is calculated.
        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let style = self.style_facts(cell.box_);
            if !style.height.is_percentage() {
                continue;
            }
            let cell_size = style.height.to_px(self.table_block_size);
            let used = self.used_values(cell.box_);
            // SAFETY: Unique cell geometry.
            unsafe {
                (*used).set_content_block_size(
                    cell_size - (*used).border_box_top(collapsed) - (*used).border_box_bottom(collapsed),
                );
            }
            if !self.rows[cell.row_index].is_collapsed {
                self.rows[cell.row_index].reference_block_size =
                    self.rows[cell.row_index].reference_block_size.max(cell_size);
            }
            let span_inline = (0..cell.column_span).fold(CssPixels::default(), |sum, index| {
                sum + self.columns[cell.column_index + index].used_inline_size
            });
            // SAFETY: Unique cell geometry.
            unsafe {
                (*used).set_content_inline_size(
                    span_inline - (*used).border_box_left(collapsed) - (*used).border_box_right(collapsed)
                        + inline_spacing * (cell.column_span - 1),
                );
            }
            let inner = unsafe { (*used).available_inner_space_or_constraints_from(self.available_space) };
            // The first pass only measured this cell in a throwaway state; this is its one and
            // only inside layout in the committing state.
            if self.layout_inside_cell(cell, inner).is_some() {
                self.finish_child_layout(cell.box_);
            }
            let baseline = self.box_baseline(cell.box_);
            self.cells[cell_index].baseline = baseline;
            if !self.rows[cell.row_index].is_collapsed {
                let border_size = unsafe { (*used).border_box_block_size(collapsed) };
                self.rows[cell.row_index].reference_block_size =
                    self.rows[cell.row_index].reference_block_size.max(border_size);
                self.rows[cell.row_index].baseline = self.rows[cell.row_index].baseline.max(baseline);
            }
        }
    }

    fn distribute_block_size_to_rows(&mut self) {
        let sum = self
            .rows
            .iter()
            .fold(CssPixels::default(), |sum, row| sum + row.reference_block_size);
        let visible = self.rows.iter().filter(|row| !row.is_collapsed).count();
        if sum == CssPixels::default() {
            return;
        }
        let auto_rows = (0..self.rows.len())
            .filter(|index| {
                !self.rows[*index].is_collapsed && self.style_facts(self.rows[*index].box_).height.is_auto()
            })
            .collect::<Vec<_>>();
        if self.table_block_size <= sum {
            // If the table block size is no larger than the sum of reference sizes, each final row block size is the
            // weighted mean of the base and reference sizes that yields the correct total block size.

            for row in &mut self.rows {
                if row.is_collapsed {
                    row.final_block_size = CssPixels::default();
                } else {
                    row.final_block_size = CssPixels::nearest_value_for(
                        self.table_block_size.to_double() * row.reference_block_size.to_double() / sum.to_double(),
                    );
                }
            }
        } else if !auto_rows.is_empty() {
            // Else, if the table owns any auto-block-size row, each non-auto row receives its reference block size and
            // auto rows receive their reference size plus an equal share of the missing table block size.

            for row in &mut self.rows {
                row.final_block_size = row.reference_block_size;
            }
            let increment = (self.table_block_size - sum) / auto_rows.len();
            for index in auto_rows {
                self.rows[index].final_block_size += increment;
            }
        } else {
            // Else, all rows receive their reference size plus an equal share of the missing table block size.

            let increment = (self.table_block_size - sum) / visible;
            for row in &mut self.rows {
                row.final_block_size = if row.is_collapsed {
                    CssPixels::default()
                } else {
                    row.reference_block_size + increment
                };
            }
        }
        // Add undistributable space due to border spacing: https://www.w3.org/TR/css-tables-3/#computing-undistributable-space.
        let spacing = self.border_spacing_block();
        self.table_block_size += (visible + 1) * spacing;
    }

    fn position_row_boxes(&mut self) {
        let table_used = self.used_values(self.table_box);
        let block_spacing = self.border_spacing_block();
        let inline_spacing = self.border_spacing_inline();
        // SAFETY: Read-only table geometry.
        let inline_offset = unsafe { (*table_used).border_left + (*table_used).padding_left + inline_spacing };
        let mut row_block_offset = self.pending_table_offset.block_offset + block_spacing;
        for row_index in 0..self.rows.len() {
            let row = &self.rows[row_index];
            let inline_size = self
                .columns
                .iter()
                .fold(CssPixels::default(), |sum, column| sum + column.used_inline_size)
                + if self.columns.len() >= 2 {
                    inline_spacing * (self.columns.len() - 1)
                } else {
                    CssPixels::default()
                };
            let used = self.used_values(row.box_);
            // SAFETY: Unique row entry.
            unsafe {
                (*used).set_content_block_size(row.final_block_size);
                (*used).set_content_inline_size(inline_size);
            }
            self.place_child(row.box_, inline_offset, row_block_offset);
            if !row.is_collapsed {
                row_block_offset += row.final_block_size + block_spacing;
            }
        }

        let mut group_block_offset = self.pending_table_offset.block_offset + block_spacing;
        for group in self.matching_children(self.table_box, |facts| {
            facts.is_table_row_group || facts.is_table_header_group || facts.is_table_footer_group
        }) {
            let group_rows = self.matching_children(group, |facts| facts.is_table_row);
            let mut block_size = CssPixels::default();
            let mut inline_size = CssPixels::default();
            for row in &group_rows {
                let used = self.used_values(*row);
                // SAFETY: Read-only row geometry.
                unsafe {
                    block_size += (*used).border_box_block_size(false);
                    inline_size = inline_size.max((*used).border_box_inline_size(false));
                }
            }
            if group_rows.len() >= 2 {
                block_size += block_spacing * (group_rows.len() - 1);
            }
            let used = self.used_values(group);
            // SAFETY: Unique group entry.
            unsafe {
                (*used).set_content_block_size(block_size);
                (*used).set_content_inline_size(inline_size);
            }
            self.place_child(group, inline_offset, group_block_offset);
            group_block_offset += block_size
                + if group_rows.is_empty() {
                    CssPixels::default()
                } else {
                    block_spacing
                };
        }
        // SAFETY: Read-only table padding.
        let padding_top = unsafe { (*table_used).padding_top };
        let total = row_block_offset.max(group_block_offset) - self.pending_table_offset.block_offset - padding_top;
        self.table_block_size = self.table_block_size.max(total);
    }

    fn compute_row_content_block_size(&mut self, cell: Cell) -> CssPixels {
        // The block size of a cell is the sum of all spanned rows, as described in
        // https://www.w3.org/TR/css-tables-3/#bounding-box-assignment
        let first = self.used_values(self.rows[cell.row_index].box_);
        if cell.row_span == 1 {
            // SAFETY: Read-only row geometry.
            return unsafe { (*first).content_block_size };
        }
        // When the row span is greater than 1, the borders of inner rows within the span have to be
        // included in the content block size of the spanning cell. First top and final bottom borders are
        // excluded to be consistent with the handling of row span 1 case above, which uses the content
        // block size (no top and bottom borders) of the row.
        let mut span = CssPixels::default();
        for index in 0..cell.row_span {
            let used = self.used_values(self.rows[cell.row_index + index].box_);
            // SAFETY: Read-only row geometry.
            unsafe {
                if index == 0 {
                    span += (*used).content_block_size + (*used).border_box_bottom(false);
                } else if index == cell.row_span - 1 {
                    span += (*used).border_box_top(false) + (*used).content_block_size;
                } else {
                    span += (*used).border_box_block_size(false);
                }
            }
        }
        // Compute cell block size as specified by https://www.w3.org/TR/css-tables-3/#bounding-box-assignment:
        // The logical size is the sum of:
        // - the inline/block sizes of all spanned visible columns/rows
        // - the inline/block border spacing times the amount of spanned visible columns/rows minus one
        // FIXME: Account for visibility.
        span + self.border_spacing_block() * (cell.row_span - 1)
    }

    fn anonymous_cell_wraps_flex_or_grid(&mut self, cell: Cell) -> bool {
        if !self.box_facts(cell.box_).is_anonymous {
            return false;
        }
        let child = self.first_child(cell.box_);
        if child.is_null() || !self.next_sibling(child).is_null() {
            return false;
        }
        let facts = self.box_facts(child);
        facts.is_box && (facts.display.is_flex_inside() || facts.display.is_grid_inside())
    }

    fn position_cell_boxes(&mut self) {
        let mut offset = CssPixels::default();
        for column in &mut self.columns {
            column.inline_offset = offset;
            offset += column.used_inline_size;
        }
        let spacing = self.border_spacing_inline();
        let collapsed = self.style_facts(self.table_box).border_collapse != BORDER_COLLAPSE_SEPARATE;
        for cell_index in 0..self.cells.len() {
            let cell = self.cells[cell_index];
            let used = self.used_values(cell.box_);
            let row_used = self.used_values(self.rows[cell.row_index].box_);
            let row_size = self.compute_row_content_block_size(cell);
            let style = self.style_facts(cell.box_);
            // When a table cell is an anonymous wrapper around a flex or grid container (e.g., a <td> with display:flex is
            // wrapped in an anonymous table-cell box per CSS Tables 3), the cell should be aligned to the top. This allows
            // the flex/grid container to fill the cell and handle alignment of its children via its own properties.
            let anonymous_wrapper = self.anonymous_cell_wraps_flex_or_grid(cell);
            // SAFETY: Unique cell entry, read-only row entry.
            unsafe {
                if anonymous_wrapper {
                    (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                } else if style.vertical_align_is_keyword {
                    // The following image shows various alignment lines of a row:
                    // https://www.w3.org/TR/css-tables-3/images/cell-align-explainer.png
                    // https://drafts.csswg.org/css2/#height-layout
                    // In the context of tables, values for vertical-align have the following meanings:
                    match style.vertical_align_keyword {
                        VERTICAL_ALIGN_MIDDLE => {
                            // The center of the cell is aligned with the center of the rows it spans.
                            let difference = row_size - (*used).border_box_block_size(collapsed);
                            (*used).padding_top += difference / 2;
                            (*used).padding_bottom += difference / 2;
                        }
                        VERTICAL_ALIGN_TOP => {
                            // The top of the cell box is aligned with the top of the first row it spans.
                            (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                        }
                        VERTICAL_ALIGN_BOTTOM => {
                            // The bottom of the cell box is aligned with the bottom of the last row it spans.
                            (*used).padding_top += row_size - (*used).border_box_block_size(collapsed);
                        }
                        VERTICAL_ALIGN_SUB
                        | VERTICAL_ALIGN_SUPER
                        | VERTICAL_ALIGN_TEXT_BOTTOM
                        | VERTICAL_ALIGN_TEXT_TOP
                        | VERTICAL_ALIGN_BASELINE => {
                            // These values do not apply to cells; the cell is aligned at the baseline instead.

                            // The baseline of the cell is put at the same height as the baseline of the first of the rows it spans.
                            (*used).padding_top += self.rows[cell.row_index].baseline - cell.baseline;
                            (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                        }
                        _ => panic!("invalid vertical-align keyword"),
                    }
                }
                // Compute cell position as specified by https://www.w3.org/TR/css-tables-3/#bounding-box-assignment:
                // left/top location is the sum of:
                // - for top: the height reserved for top captions (including margins), if any
                // - the padding-left/padding-top and border-left-width/border-top-width of the table
                // FIXME: Account for visibility.
                let x = (*row_used).content_offset.x
                    + (*used).border_box_left(collapsed)
                    + self.columns[cell.column_index].inline_offset
                    + spacing * cell.column_index;
                let y = (*row_used).content_offset.y + (*used).border_box_top(collapsed);
                self.place_child(cell.box_, x, y);
            }
        }
    }

    fn run_caption_layout(&mut self, phase: u8, available: AvailableSpace) -> CssPixels {
        let mut total = CssPixels::default();
        for caption in self.matching_children(self.table_box, |facts| facts.is_table_caption) {
            if self.style_facts(caption).caption_side != phase {
                continue;
            }
            // Captions live inside the table wrapper, so their quirks percentage height basis derives
            // from the wrapper, not from anything the table box inherited.
            let mut result = FfiCaptionLayoutResult::default();
            bump(FfiOp::CaptionLayoutCallback);
            // The caption boxes are principal block-level boxes that retain their own content, padding, margin, and border areas,
            // and are rendered as normal block boxes inside the table wrapper box, as described in https://www.w3.org/TR/CSS22/tables.html#model
            // SAFETY: The host executes the generic caption child layout and
            // copies the scalar result.
            let laid_out = unsafe {
                (self.callbacks.layout_table_caption)(
                    self.callbacks.context,
                    caption,
                    phase,
                    available,
                    self.participant_constraints,
                    &raw mut result,
                )
            };
            assert!(laid_out);
            if phase == CAPTION_SIDE_TOP {
                self.pending_table_offset.block_offset = result.pending_table_block_offset;
            }
            total += result.margin_box_block_size;
        }
        total
    }

    fn compute_and_store_baselines(&self, node: Node) {
        bump(FfiOp::ComputeBaselinesCallback);
        // SAFETY: The host computes baselines in the current state.
        unsafe {
            (self.callbacks.compute_and_store_baselines)(self.callbacks.context, node);
        }
    }

    fn run(&mut self, input: FfiLayoutInput) {
        self.available_space = input.available_space;
        self.min_border_box_block_size_from_flex_item = input
            .has_table_grid_min_border_box_block_size
            .then_some(input.table_grid_min_border_box_block_size);
        self.run_until_inline_size_calculation(input, false);
        if matches!(
            self.available_space.inline_size.type_,
            AvailableSizeType::MinContent | AvailableSizeType::MaxContent
        ) && !matches!(
            self.available_space.block_size.type_,
            AvailableSizeType::MinContent | AvailableSizeType::MaxContent
        ) {
            return;
        }

        let table_used = self.used_values(self.table_box);
        // SAFETY: Read-only table geometry.
        let table_border_inline = unsafe { (*table_used).border_box_inline_size(false) };
        let max_dimension = CssPixels::from_raw(17_895_700 * 64);
        let caption_available = AvailableSpace {
            inline_size: AvailableSize::definite(table_border_inline.min(max_dimension)),
            block_size: self.available_space.block_size,
        };
        let mut captions = self.run_caption_layout(CAPTION_SIDE_TOP, caption_available);
        // The total inline-axis border spacing is defined for each table:
        // - For tables laid out in separated-borders mode containing at least one column, the inline-axis component of the computed value of the border-spacing property times one plus the number of columns in the table
        // - Otherwise, 0
        let total_spacing = if self.columns.is_empty() {
            CssPixels::default()
        } else {
            (self.columns.len() + 1) * self.border_spacing_inline()
        };
        // SAFETY: Read-only table content width.
        // The assignable table inline size is its used inline size minus the inline-axis border spacing.
        let assignable = unsafe { (*table_used).content_inline_size } - total_spacing;
        let fixed = self.use_fixed_mode_layout();
        // Distribute the inline size of the table among columns.
        distribute_inline_size(&mut self.columns, assignable, fixed);
        self.compute_table_block_size();
        self.distribute_block_size_to_rows();
        self.position_row_boxes();
        self.position_cell_boxes();
        for cell in &self.cells {
            super::abspos::layout_children_native(
                self.state,
                self.callbacks,
                self.layout_mode,
                self.table_box,
                cell.box_,
            );
        }
        // SAFETY: Unique table entry.
        unsafe {
            (*table_used).set_content_block_size(self.table_block_size);
        }
        captions += self.run_caption_layout(CAPTION_SIDE_BOTTOM, caption_available);
        // SAFETY: Unique table entry.
        // Table captions are positioned between the table margins and its borders (outside the grid box borders) as described in
        // https://www.w3.org/TR/css-tables-3/#bounding-box-assignment
        // A visual representation of this model can be found at https://www.w3.org/TR/css-tables-3/images/table_container.png
        unsafe {
            (*table_used).margin_bottom += captions;
        }
        // Derive baselines for the table internals bottom-up (rows, then row groups, then the table box)
        // now that all offsets are final, so the table exports its baseline to outside consumers
        // (e.g. an inline-table participating in a line box).
        for row in &self.rows {
            self.compute_and_store_baselines(row.box_);
        }
        for group in self.matching_children(self.table_box, |facts| {
            facts.is_table_row_group || facts.is_table_header_group || facts.is_table_footer_group
        }) {
            self.compute_and_store_baselines(group);
        }
        self.compute_and_store_baselines(self.table_box);
        self.automatic_content_block_size = self.table_block_size;
    }
}

fn inline_basis(constraints: FfiContainingBlockConstraints) -> CssPixels {
    if constraints.has_percentage_basis_inline_size {
        constraints.percentage_basis_inline_size
    } else {
        CssPixels::default()
    }
}

fn block_basis(constraints: FfiContainingBlockConstraints) -> CssPixels {
    if constraints.has_percentage_basis_block_size {
        constraints.percentage_basis_block_size
    } else {
        CssPixels::default()
    }
}

pub(crate) fn run(instance: &mut FormattingContextInstance, input: FfiLayoutInput) {
    let mut table = TableFormattingContext::new(instance);
    table.run(input);
    let used = table.used_values(table.table_box);
    // SAFETY: The table used-values entry outlives the FC.
    instance.automatic_content_inline_size = unsafe { (*used).content_inline_size };
    instance.automatic_content_block_size = table.automatic_content_block_size;
    instance.pending_table_box_content_offset_in_wrapper = table.pending_table_offset;
}

pub(crate) fn run_until_inline_size_calculation(
    instance: &mut FormattingContextInstance,
    input: FfiLayoutInput,
    skip_row_measurement: bool,
) {
    let mut table = TableFormattingContext::new(instance);
    table.run_until_inline_size_calculation(input, skip_row_measurement);
    let used = table.used_values(table.table_box);
    // SAFETY: The table used-values entry outlives the FC.
    instance.automatic_content_inline_size = unsafe { (*used).content_inline_size };
    instance.pending_table_box_content_offset_in_wrapper = table.pending_table_offset;
}
