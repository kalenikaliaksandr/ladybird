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
        if style.border_collapse != BORDER_COLLAPSE_SEPARATE {
            CssPixels::default()
        } else {
            style.border_spacing_horizontal
        }
    }

    fn border_spacing_block(&mut self) -> CssPixels {
        let style = self.style_facts(self.table_box);
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
        let style = self.style_facts(self.table_box);
        style.table_layout == TABLE_LAYOUT_FIXED
            && (style.width.is_length()
                || style.width.is_percentage()
                || style.width.is_min_content()
                || style.width.is_fit_content())
    }

    fn compute_constrainedness(&mut self) {
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
            self.cells[cell_index].outer_min_inline_size = min_inline.max(min_content_inline) + inline_offsets;

            if include_rows {
                let min_content_block = self.calculate_min_content_block_size(cell.box_, max_content_inline);
                let max_content_block = self.calculate_max_content_block_size(cell.box_, min_content_inline);
                let min_block = style.min_height.to_px(block_basis);
                let block_offsets = padding_block_start + padding_block_end + border_block_start + border_block_end;
                self.cells[cell_index].outer_min_block_size = min_block.max(min_content_block) + block_offsets;
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
                    min_block.max(block_size.max(min_content_block)) + block_offsets
                } else {
                    min_block.max(block_size.max(min_content_block.max(max_block.min(max_content_block))))
                        + block_offsets
                };
            }

            self.cells[cell_index].outer_max_inline_size = if self.columns[cell.column_index].is_constrained {
                min_inline.max(max_inline.min(inline_size.max(min_content_inline))) + inline_offsets
            } else {
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
            self.rows[row_index].min_size = min_size.max(size);
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
                self.columns[column_index].min_size = min_size.max(size);
                self.columns[column_index].max_size = min_size.max(max_size.min(size));
                column_index += self.table_facts(column).raw_column_span as usize;
            }
        }
        self.initialize_row_content_sizes();
    }

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
                self.rows[index].has_intrinsic_percentage =
                    style.max_height.is_percentage() || style.height.is_percentage();
                self.rows[index].intrinsic_percentage = Self::cell_percentage(style, axis);
            }
        } else {
            let mut column_index = 0usize;
            for group in self.matching_children(self.table_box, |facts| facts.is_table_column_group) {
                for column in self.matching_children(group, |facts| facts.is_table_column) {
                    let style = self.style_facts(column);
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
        self.initialize_intrinsic_percentages(axis);
        let count = self.track_count(axis);
        let mut contributions = (0..count)
            .map(|index| self.track_percentage(axis, index))
            .collect::<Vec<_>>();
        for current_span in 2..=max_span {
            for cell_index in 0..self.cells.len() {
                let cell = self.cells[cell_index];
                if Self::cell_span(cell, axis) != current_span {
                    continue;
                }
                let style = self.style_facts(cell.box_);
                let start = Self::cell_index(cell, axis);
                let end = start + current_span;
                let mut contribution = CssPixels::nearest_value_for(Self::cell_percentage(style, axis));
                for index in start..end {
                    contribution -= CssPixels::nearest_value_for(self.track_percentage(axis, index));
                    contribution = contribution.max(CssPixels::default());
                }
                let mut zero_sum = CssPixels::default();
                let mut zero_count = 0usize;
                for index in start..end {
                    if self.track_percentage(axis, index) == 0.0 {
                        zero_sum += self.track_max(axis, index);
                        zero_count += 1;
                    }
                }
                for (index, saved) in contributions.iter_mut().enumerate().take(end).skip(start) {
                    if self.track_percentage(axis, index) > 0.0 {
                        continue;
                    }
                    let adjusted = if zero_sum != CssPixels::default() {
                        contribution.scaled(self.track_max(axis, index).to_double() / zero_sum.to_double())
                    } else {
                        contribution / zero_count
                    };
                    *saved = saved.max(adjusted.to_double());
                }
            }
            for (index, value) in contributions.iter().copied().enumerate() {
                self.set_track_percentage(axis, index, value);
            }
        }
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
                    let row = &mut self.rows[cell.row_index];
                    row.min_size = row.min_size.max(cell.outer_min_block_size.max(specified));
                    row.max_size = row.max_size.max(cell.outer_max_block_size);
                }
            }
        } else {
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
        self.compute_intrinsic_percentage(axis, max_span);
        let track_count = self.track_count(axis);
        for current_span in 2..=max_span {
            let mut min_contributions = vec![Vec::new(); track_count];
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
                let baseline_max =
                    (start..end).fold(CssPixels::default(), |sum, index| sum + self.track_max(axis, index));
                let baseline_min =
                    (start..end).fold(CssPixels::default(), |sum, index| sum + self.track_min(axis, index));
                let spacing = track_spacing * (current_span - 1);
                for index in start..end {
                    let mut min_contribution = self.track_min(axis, index);
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
                    if baseline_max != CssPixels::default() {
                        min_contribution += CssPixels::nearest_value_for(
                            self.track_max(axis, index).to_double() / baseline_max.to_double(),
                        ) * (Self::cell_min(cell, axis) - baseline_max - spacing)
                            .max(CssPixels::default());
                    } else {
                        min_contribution +=
                            (Self::cell_min(cell, axis) - spacing).max(CssPixels::default()) / current_span;
                    }

                    let mut max_contribution = self.track_max(axis, index);
                    if baseline_max != CssPixels::default() {
                        max_contribution += CssPixels::nearest_value_for(
                            self.track_max(axis, index).to_double() / baseline_max.to_double(),
                        ) * (Self::cell_max(cell, axis) - baseline_max - spacing)
                            .max(CssPixels::default());
                    } else {
                        max_contribution +=
                            (Self::cell_max(cell, axis) - spacing).max(CssPixels::default()) / current_span;
                    }
                    min_contributions[index].push(min_contribution);
                    max_contributions[index].push(max_contribution);
                }
            }
            for index in 0..track_count {
                let mut min_size = self.track_min(axis, index);
                for contribution in &min_contributions[index] {
                    min_size = min_size.max(*contribution);
                }
                self.set_track_min(axis, index, min_size);
                let mut max_size = self.track_max(axis, index);
                for contribution in &max_contributions[index] {
                    max_size = max_size.max(*contribution);
                }
                self.set_track_max(axis, index, max_size);
            }
        }
    }

    fn compute_capmin(&mut self) -> CssPixels {
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
        let table_style = self.style_facts(self.table_box);
        let available_inline = self.available_space.inline_size;
        let basis = inline_basis(self.table_constraints);
        let spacing = (self.columns.len() + 1) * self.border_spacing_inline();
        let grid_min = self
            .columns
            .iter()
            .fold(CssPixels::default(), |sum, column| sum + column.min_size)
            + spacing;
        let grid_max = self
            .columns
            .iter()
            .fold(CssPixels::default(), |sum, column| sum + column.max_size)
            + spacing;
        let capmin = self.compute_capmin();
        let mut used_min = grid_min.max(capmin);
        if !table_style.min_width.is_auto() {
            used_min = used_min.max(self.resolve_inline_constraint(table_style.min_width, grid_min, grid_max, basis));
        }
        let width_is_auto_or_indefinite_percentage = table_style.width.is_auto()
            || (table_style.width.contains_percentage && !self.table_constraints.has_percentage_basis_inline_size);
        let mut used = if width_is_auto_or_indefinite_percentage {
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
        let table_grid = grid::calculate(self, self.table_box);
        self.cells = table_grid.cells;
        self.rows = table_grid.rows;
        self.columns = vec![Column::default(); table_grid.column_count];
        for cell in &self.cells {
            self.columns[cell.column_index].has_originating_cells = true;
        }

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
        if include_rows && self.can_skip_row_intrinsic_measurement() {
            include_rows = false;
        }
        if include_rows && self.use_fixed_mode_layout() {
            include_rows = false;
            self.needs_fixed_mode_row_measurement = true;
        }
        self.compute_cell_measures(include_rows);
        self.compute_outer_content_sizes();
        self.compute_table_measures(TrackAxis::Column);
        if include_rows {
            self.compute_table_measures(TrackAxis::Row);
        }
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
        for row_index in 0..self.rows.len() {
            if self.rows[row_index].is_collapsed {
                self.rows[row_index].base_block_size = CssPixels::default();
                continue;
            }
            let style = self.style_facts(self.rows[row_index].box_);
            if style.height.is_length() {
                self.rows[row_index].base_block_size = self.rows[row_index]
                    .base_block_size
                    .max(style.height.to_px(CssPixels::default()));
            }
        }
        let inline_basis = inline_basis(self.participant_constraints);
        let participant_block_basis = block_basis(self.participant_constraints);
        let collapsed = self.style_facts(self.table_box).border_collapse != BORDER_COLLAPSE_SEPARATE;
        let inline_spacing = self.border_spacing_inline();
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
            let baseline = measured_baseline.unwrap_or_else(|| self.box_baseline(cell.box_));
            self.cells[cell_index].baseline = baseline;
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
            row.reference_block_size = row.base_block_size;
        }
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
            for row in &mut self.rows {
                row.final_block_size = row.reference_block_size;
            }
            let increment = (self.table_block_size - sum) / auto_rows.len();
            for index in auto_rows {
                self.rows[index].final_block_size += increment;
            }
        } else {
            let increment = (self.table_block_size - sum) / visible;
            for row in &mut self.rows {
                row.final_block_size = if row.is_collapsed {
                    CssPixels::default()
                } else {
                    row.reference_block_size + increment
                };
            }
        }
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
        let first = self.used_values(self.rows[cell.row_index].box_);
        if cell.row_span == 1 {
            // SAFETY: Read-only row geometry.
            return unsafe { (*first).content_block_size };
        }
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
            let anonymous_wrapper = self.anonymous_cell_wraps_flex_or_grid(cell);
            // SAFETY: Unique cell entry, read-only row entry.
            unsafe {
                if anonymous_wrapper {
                    (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                } else if style.vertical_align_is_keyword {
                    match style.vertical_align_keyword {
                        VERTICAL_ALIGN_MIDDLE => {
                            let difference = row_size - (*used).border_box_block_size(collapsed);
                            (*used).padding_top += difference / 2;
                            (*used).padding_bottom += difference / 2;
                        }
                        VERTICAL_ALIGN_TOP => {
                            (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                        }
                        VERTICAL_ALIGN_BOTTOM => {
                            (*used).padding_top += row_size - (*used).border_box_block_size(collapsed);
                        }
                        VERTICAL_ALIGN_SUB
                        | VERTICAL_ALIGN_SUPER
                        | VERTICAL_ALIGN_TEXT_BOTTOM
                        | VERTICAL_ALIGN_TEXT_TOP
                        | VERTICAL_ALIGN_BASELINE => {
                            (*used).padding_top += self.rows[cell.row_index].baseline - cell.baseline;
                            (*used).padding_bottom += row_size - (*used).border_box_block_size(collapsed);
                        }
                        _ => panic!("invalid vertical-align keyword"),
                    }
                }
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
            let mut result = FfiCaptionLayoutResult::default();
            bump(FfiOp::CaptionLayoutCallback);
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
        let total_spacing = if self.columns.is_empty() {
            CssPixels::default()
        } else {
            (self.columns.len() + 1) * self.border_spacing_inline()
        };
        // SAFETY: Read-only table content width.
        let assignable = unsafe { (*table_used).content_inline_size } - total_spacing;
        let fixed = self.use_fixed_mode_layout();
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
        unsafe {
            (*table_used).margin_bottom += captions;
        }
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
