/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#![allow(dead_code)]

use super::alignment::{
    Alignment, ItemAlignment, align_item, block_content_alignment, block_item_alignment, content_start_offset,
    distributed_gap_size, inline_content_alignment, inline_item_alignment,
};
use super::facts::{
    FfiGridLayoutArea, FfiGridLayoutData, FfiGridLayoutDimension, FfiGridLayoutFragment, FfiGridLayoutLine,
    FfiGridLayoutTrack, FfiGridPlacement, FfiUsedGridLine, FfiUsedGridTrackList, GridStyleFacts,
};
use super::placement::{
    AutoFlowAxis, PlacementInput, ResolvedAxisPlacement, resolve_placement_position, resolve_placement_span,
};
use super::sizing::{ItemContribution, run_track_sizing};
use super::template::{
    ExpandedTrackList, LineName, TrackDefinition, TrackListSource, add_template_area_lines, automatic_subgrid_span,
    expand_standalone, expand_subgrid,
};
use super::tracks::{Track, TrackSizingFunction};
use crate::box_facts::FfiLayoutBoxFacts;
use crate::css_enums::{align_content, justify_content};
use crate::css_pixels::CssPixels;
use crate::fc::sizing::SizingContext;
use crate::fc::{
    FfiChildLayoutResult, FfiFlexAxis, FfiFlexSizeProperty, FfiFormattingContextType, FfiLayoutFcCallbacks,
    FormattingContextInstance,
};
use crate::geometry::{
    AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput, LogicalOffset, LogicalRect,
    LogicalSize,
};
use crate::layout_state::{
    FfiAbsposAlignment, FfiAbsposAxisMode, FfiAbsposContainingBlockInfo, FfiStaticPositionAlignment,
    FfiStaticPositionRect, state_mut,
};
use crate::style_facts::{FfiSizeValue, FfiStyleFacts};
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use std::ffi::c_void;

type Node = *mut c_void;

const LAYOUT_MODE_NORMAL: u8 = 0;
const LAYOUT_MODE_INTRINSIC_SIZING: u8 = 1;
const MAX_DIMENSION: i64 = 17_895_700;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    Column,
    Row,
}

impl Axis {
    fn is_column(self) -> bool {
        self == Self::Column
    }
}

#[derive(Clone, Copy)]
struct GridItem {
    box_: Node,
    used_values: *mut UsedValuesCore,
    row: i32,
    row_span: usize,
    column: i32,
    column_span: usize,
    extra_margin_top: CssPixels,
    extra_margin_right: CssPixels,
    extra_margin_bottom: CssPixels,
    extra_margin_left: CssPixels,
}

impl GridItem {
    fn position(self, axis: Axis) -> i32 {
        if axis.is_column() { self.column } else { self.row }
    }

    fn span(self, axis: Axis) -> usize {
        if axis.is_column() {
            self.column_span
        } else {
            self.row_span
        }
    }
}

#[derive(Clone)]
struct GridFactsCopy {
    names: Vec<usize>,
    entries: Vec<super::facts::FfiGridTrackEntry>,
    name_indices: Vec<u32>,
    template_columns: super::facts::FfiGridTrackList,
    template_rows: super::facts::FfiGridTrackList,
    auto_columns: super::facts::FfiGridTrackList,
    auto_rows: super::facts::FfiGridTrackList,
    areas: Vec<super::facts::FfiGridArea>,
    column_start: FfiGridPlacement,
    column_end: FfiGridPlacement,
    row_start: FfiGridPlacement,
    row_end: FfiGridPlacement,
}

impl GridFactsCopy {
    fn from(facts: &GridStyleFacts) -> Self {
        Self {
            names: (0..facts.name_count())
                .map(|index| facts.name_raw(index as u32))
                .collect(),
            entries: facts.entries.clone(),
            name_indices: facts.name_indices.clone(),
            template_columns: facts.template_columns,
            template_rows: facts.template_rows,
            auto_columns: facts.auto_columns,
            auto_rows: facts.auto_rows,
            areas: facts.areas.clone(),
            column_start: facts.column_start,
            column_end: facts.column_end,
            row_start: facts.row_start,
            row_end: facts.row_end,
        }
    }

    fn source(&self) -> TrackListSource<'_> {
        TrackListSource {
            names: &self.names,
            entries: &self.entries,
            name_indices: &self.name_indices,
        }
    }
}

pub(crate) struct GridFormattingContext {
    state: *mut c_void,
    grid_container: Node,
    parent_rust_fc: *mut c_void,
    parent_grid_override: *const GridFormattingContext,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    should_collect_devtools_layout_data: bool,
    container_used_values: *mut UsedValuesCore,
    available_space: Option<AvailableSpace>,
    layout_input: Option<FfiLayoutInput>,
    column_lines: Vec<Vec<LineName>>,
    row_lines: Vec<Vec<LineName>>,
    columns: Vec<Track>,
    rows: Vec<Track>,
    column_gaps: Vec<Track>,
    row_gaps: Vec<Track>,
    items: Vec<GridItem>,
    explicit_column_line_count: usize,
    explicit_row_line_count: usize,
    explicit_column_start: usize,
    explicit_row_start: usize,
    automatic_content_block_size: CssPixels,
    row_alignment_container_size: CssPixels,
    use_row_alignment_container_size: bool,
}

impl GridFormattingContext {
    pub(crate) fn new(
        state: *mut c_void,
        grid_container: Node,
        parent_rust_fc: *mut c_void,
        layout_mode: u8,
        callbacks: FfiLayoutFcCallbacks,
        should_collect_devtools_layout_data: bool,
    ) -> Self {
        // SAFETY: C++ owns stable used-values storage for the formatting
        // context root for the lifetime of this Rust context.
        let container_used_values = unsafe { (callbacks.get_used_values)(callbacks.context, grid_container) };
        assert!(!container_used_values.is_null());
        Self {
            state,
            grid_container,
            parent_rust_fc,
            parent_grid_override: std::ptr::null(),
            layout_mode,
            callbacks,
            should_collect_devtools_layout_data,
            container_used_values,
            available_space: None,
            layout_input: None,
            column_lines: Vec::new(),
            row_lines: Vec::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            column_gaps: Vec::new(),
            row_gaps: Vec::new(),
            items: Vec::new(),
            explicit_column_line_count: 0,
            explicit_row_line_count: 0,
            explicit_column_start: 0,
            explicit_row_start: 0,
            automatic_content_block_size: CssPixels::default(),
            row_alignment_container_size: CssPixels::default(),
            use_row_alignment_container_size: false,
        }
    }

    fn reset_for_run(&mut self, input: FfiLayoutInput) {
        self.available_space = Some(input.available_space);
        self.layout_input = Some(input);
        self.column_lines.clear();
        self.row_lines.clear();
        self.columns.clear();
        self.rows.clear();
        self.column_gaps.clear();
        self.row_gaps.clear();
        self.items.clear();
        self.explicit_column_line_count = 0;
        self.explicit_row_line_count = 0;
        self.explicit_column_start = 0;
        self.explicit_row_start = 0;
        self.automatic_content_block_size = CssPixels::default();
        self.row_alignment_container_size = CssPixels::default();
        self.use_row_alignment_container_size = false;
    }

    fn container_used(&self) -> &UsedValuesCore {
        // SAFETY: The entry is state-owned and stable for this pass.
        unsafe { &*self.container_used_values }
    }

    fn container_used_mut(&mut self) -> &mut UsedValuesCore {
        // SAFETY: Layout mutation is single-threaded.
        unsafe { &mut *self.container_used_values }
    }

    fn used(&self, item: GridItem) -> &UsedValuesCore {
        // SAFETY: Every item stores state-owned used-values storage.
        unsafe { &*item.used_values }
    }

    fn used_mut(&mut self, item: GridItem) -> &mut UsedValuesCore {
        // SAFETY: Layout mutation is single-threaded.
        unsafe { &mut *item.used_values }
    }

    fn style(&self, node: Node) -> FfiStyleFacts {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn facts(&self, node: Node) -> FfiLayoutBoxFacts {
        state_mut(self.state).box_facts(&self.callbacks, node)
    }

    fn grid_facts_copy(&self, node: Node) -> GridFactsCopy {
        GridFactsCopy::from(state_mut(self.state).grid_facts(&self.callbacks, node))
    }

    fn sizing(&self) -> SizingContext {
        SizingContext::new(self.state, self.callbacks)
    }

    fn navigate(&self, callback: crate::box_facts::FfiLayoutNavCallback, node: Node) -> Node {
        // SAFETY: Navigation is synchronous and the host owns every node.
        unsafe { callback(self.callbacks.navigation.context, node) }
    }

    fn parent_grid(&self) -> Option<&GridFormattingContext> {
        if !self.parent_grid_override.is_null() {
            // SAFETY: Intrinsic subgrid contribution traversal keeps the
            // parent context live on the caller's stack.
            return Some(unsafe { &*self.parent_grid_override });
        }
        if self.parent_rust_fc.is_null() {
            return None;
        }
        // SAFETY: The parent handle is supplied only for a live
        // RustFormattingContext and outlives this child context.
        let parent = unsafe { &*self.parent_rust_fc.cast::<FormattingContextInstance>() };
        if parent.fc_type != FfiFormattingContextType::Grid as u8 {
            return None;
        }
        parent.grid_context.as_deref()
    }

    fn parent_grid_item(&self) -> Option<GridItem> {
        self.parent_grid()?
            .items
            .iter()
            .copied()
            .find(|item| item.box_ == self.grid_container)
    }

    fn is_subgridded(&self, axis: Axis, facts: &GridFactsCopy) -> bool {
        let list = if axis.is_column() {
            facts.template_columns
        } else {
            facts.template_rows
        };
        list.is_subgrid && self.parent_grid_item().is_some()
    }

    fn axis_available(&self, axis: Axis) -> AvailableSize {
        let space = self.available_space.unwrap();
        if axis.is_column() {
            space.inline_size
        } else {
            space.block_size
        }
    }

    fn axis_gap_value(&self, axis: Axis) -> FfiSizeValue {
        let style = self.style(self.grid_container);
        if axis.is_column() {
            style.column_gap
        } else {
            style.row_gap
        }
    }

    fn parent_gap_size_for_subgrid(&self, axis: Axis) -> CssPixels {
        let Some(parent) = self.parent_grid() else {
            return CssPixels::default();
        };
        let Some(item) = self.parent_grid_item() else {
            return CssPixels::default();
        };
        if item.span(axis) <= 1 {
            return CssPixels::default();
        }
        let gaps = if axis.is_column() {
            &parent.column_gaps
        } else {
            &parent.row_gaps
        };
        if gaps.is_empty() {
            return CssPixels::default();
        }
        let position = item.position(axis);
        if position >= 0
            && let Some(gap) = gaps.get(position as usize)
        {
            return gap.base_size;
        }
        gaps[0].base_size
    }

    fn resolved_gap(&self, axis: Axis, available: AvailableSize) -> CssPixels {
        let facts = self.grid_facts_copy(self.grid_container);
        if self.is_subgridded(axis, &facts) {
            return self.parent_gap_size_for_subgrid(axis);
        }
        self.axis_gap_value(axis).to_px(available.to_px_or_zero())
    }

    fn subgrid_gap_extra_margin(&self, axis: Axis, available: AvailableSize) -> CssPixels {
        let facts = self.grid_facts_copy(self.grid_container);
        if !self.is_subgridded(axis, &facts) {
            return CssPixels::default();
        }
        let gap = self.axis_gap_value(axis);
        if gap.is_auto() {
            return CssPixels::default();
        }
        (gap.to_px(available.to_px_or_zero()) - self.parent_gap_size_for_subgrid(axis)) / 2
    }

    fn project_parent_grid_areas(
        &self,
        columns: &mut [Vec<LineName>],
        rows: &mut [Vec<LineName>],
        column_is_subgrid: bool,
        row_is_subgrid: bool,
    ) {
        #[derive(Clone, Copy)]
        struct Area {
            name_raw: usize,
            start_name_raw: usize,
            end_name_raw: usize,
            row_start: Option<usize>,
            row_end: Option<usize>,
            column_start: Option<usize>,
            column_end: Option<usize>,
        }

        let Some(parent) = self.parent_grid() else {
            return;
        };
        let Some(parent_item) = self.parent_grid_item() else {
            return;
        };
        let mut areas = Vec::<Area>::new();
        for (axis, lines) in [(Axis::Column, &parent.column_lines), (Axis::Row, &parent.row_lines)] {
            for (line_index, names) in lines.iter().enumerate() {
                for name in names.iter().filter(|name| name.implicit && name.area_name_raw != 0) {
                    let area_index = areas
                        .iter()
                        .position(|area| area.name_raw == name.area_name_raw)
                        .unwrap_or_else(|| {
                            areas.push(Area {
                                name_raw: name.area_name_raw,
                                start_name_raw: 0,
                                end_name_raw: 0,
                                row_start: None,
                                row_end: None,
                                column_start: None,
                                column_end: None,
                            });
                            areas.len() - 1
                        });
                    let area = &mut areas[area_index];
                    let coordinate = match (axis, name.area_is_start) {
                        (Axis::Column, true) => &mut area.column_start,
                        (Axis::Column, false) => &mut area.column_end,
                        (Axis::Row, true) => &mut area.row_start,
                        (Axis::Row, false) => &mut area.row_end,
                    };
                    *coordinate = Some(match (*coordinate, name.area_is_start) {
                        (Some(old), true) => old.min(line_index),
                        (Some(old), false) => old.max(line_index),
                        (None, _) => line_index,
                    });
                    if name.area_is_start {
                        area.start_name_raw = name.raw;
                    } else {
                        area.end_name_raw = name.raw;
                    }
                }
            }
        }

        let subgrid_column_start = parent_item.position(Axis::Column);
        let subgrid_column_end = subgrid_column_start + parent_item.span(Axis::Column) as i32;
        let subgrid_row_start = parent_item.position(Axis::Row);
        let subgrid_row_end = subgrid_row_start + parent_item.span(Axis::Row) as i32;
        let overlaps = |start_a: i32, end_a: i32, start_b: i32, end_b: i32| start_a.max(start_b) < end_a.min(end_b);

        for area in areas {
            let (Some(row_start), Some(row_end), Some(column_start), Some(column_end)) =
                (area.row_start, area.row_end, area.column_start, area.column_end)
            else {
                continue;
            };
            let row_start = row_start as i32;
            let row_end = row_end as i32;
            let column_start = column_start as i32;
            let column_end = column_end as i32;
            if !overlaps(row_start, row_end, subgrid_row_start, subgrid_row_end)
                || !overlaps(column_start, column_end, subgrid_column_start, subgrid_column_end)
            {
                continue;
            }

            if column_is_subgrid {
                let start = column_start.max(subgrid_column_start) - subgrid_column_start;
                let end = column_end.min(subgrid_column_end) - subgrid_column_start;
                if let Some(line) = columns.get_mut(start as usize) {
                    line.push(LineName {
                        name_index: super::facts::NO_GRID_INDEX,
                        raw: area.start_name_raw,
                        implicit: true,
                        adopted_from_parent: false,
                        area_name_raw: area.name_raw,
                        area_is_start: true,
                    });
                }
                if let Some(line) = columns.get_mut(end as usize) {
                    line.push(LineName {
                        name_index: super::facts::NO_GRID_INDEX,
                        raw: area.end_name_raw,
                        implicit: true,
                        adopted_from_parent: false,
                        area_name_raw: area.name_raw,
                        area_is_start: false,
                    });
                }
            }
            if row_is_subgrid {
                let start = row_start.max(subgrid_row_start) - subgrid_row_start;
                let end = row_end.min(subgrid_row_end) - subgrid_row_start;
                if let Some(line) = rows.get_mut(start as usize) {
                    line.push(LineName {
                        name_index: super::facts::NO_GRID_INDEX,
                        raw: area.start_name_raw,
                        implicit: true,
                        adopted_from_parent: false,
                        area_name_raw: area.name_raw,
                        area_is_start: true,
                    });
                }
                if let Some(line) = rows.get_mut(end as usize) {
                    line.push(LineName {
                        name_index: super::facts::NO_GRID_INDEX,
                        raw: area.end_name_raw,
                        implicit: true,
                        adopted_from_parent: false,
                        area_name_raw: area.name_raw,
                        area_is_start: false,
                    });
                }
            }
        }
    }

    fn automatic_repeat_count(
        &self,
        source: TrackListSource<'_>,
        entry: &super::facts::FfiGridTrackEntry,
        axis: Axis,
    ) -> usize {
        let available = self.axis_available(axis);
        // C++'s resolve_definite_track_size() uses the inline available
        // size even while counting repeats on the row axis.
        let resolution_available = self.available_space.unwrap().inline_size;
        let repeated = expand_standalone(source, entry.repeat_list, |_index, _entry| 1);
        let mut repeated_size = CssPixels::default();
        for definition in &repeated.tracks {
            let min = TrackSizingFunction::from_ffi(definition.min);
            let max = TrackSizingFunction::from_ffi(definition.max);
            let size = if matches!(max, TrackSizingFunction::Fixed(_)) {
                max.resolve(resolution_available)
                    .max(if matches!(min, TrackSizingFunction::Fixed(_)) {
                        min.resolve(resolution_available)
                    } else {
                        CssPixels::default()
                    })
            } else if matches!(min, TrackSizingFunction::Fixed(_)) {
                min.resolve(resolution_available)
            } else {
                return 1;
            };
            repeated_size += size.max(CssPixels::from_integer(1));
        }
        let gap = self.resolved_gap(axis, available);
        let denominator = repeated_size + gap * repeated.tracks.len();
        if available.is_definite() && denominator > CssPixels::default() {
            return (((available.value + gap).raw_value() as i64 / denominator.raw_value() as i64).max(1)) as usize;
        }
        1
    }

    fn expand_axis(&self, axis: Axis, facts: &GridFactsCopy) -> ExpandedTrackList {
        let list = if axis.is_column() {
            facts.template_columns
        } else {
            facts.template_rows
        };
        let source = facts.source();
        if self.is_subgridded(axis, facts) {
            let parent_item = self.parent_grid_item().unwrap();
            let track_count = parent_item.span(axis);
            let inherited = self
                .parent_grid()
                .map(|parent| {
                    let lines = if axis.is_column() {
                        &parent.column_lines
                    } else {
                        &parent.row_lines
                    };
                    let start = parent_item.position(axis).max(0) as usize;
                    lines
                        .iter()
                        .skip(start)
                        .take(track_count.saturating_add(1))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return expand_subgrid(source, list, track_count, &inherited);
        }
        expand_standalone(source, list, |_index, entry| {
            self.automatic_repeat_count(source, entry, axis)
        })
    }

    fn initialize_lines(&mut self, facts: &GridFactsCopy) -> (ExpandedTrackList, ExpandedTrackList) {
        let mut columns = self.expand_axis(Axis::Column, facts);
        let mut rows = self.expand_axis(Axis::Row, facts);
        self.project_parent_grid_areas(
            &mut columns.lines,
            &mut rows.lines,
            self.is_subgridded(Axis::Column, facts),
            self.is_subgridded(Axis::Row, facts),
        );
        add_template_area_lines(&mut columns.lines, &mut rows.lines, &facts.areas, &facts.names);
        self.explicit_column_line_count = columns.lines.len();
        self.explicit_row_line_count = rows.lines.len();
        self.column_lines.clone_from(&columns.lines);
        self.row_lines.clone_from(&rows.lines);
        (columns, rows)
    }

    fn axis_placements(
        &self,
        start: FfiGridPlacement,
        end: FfiGridPlacement,
        axis: Axis,
        automatic_subgrid_span_value: Option<usize>,
        placement_names: &[usize],
    ) -> ResolvedAxisPlacement {
        let start_is_auto = start.kind != super::facts::FfiGridPlacementKind::Line as u8;
        let end_is_auto = end.kind != super::facts::FfiGridPlacementKind::Line as u8;
        let lines = if axis.is_column() {
            &self.column_lines
        } else {
            &self.row_lines
        };
        let explicit_lines = if axis.is_column() {
            self.explicit_column_line_count
        } else {
            self.explicit_row_line_count
        };
        let explicit_tracks = lines.len().saturating_sub(1);
        if start_is_auto && end_is_auto {
            return ResolvedAxisPlacement {
                start: None,
                span: resolve_placement_span(start, end, automatic_subgrid_span_value),
            };
        }
        let resolved = resolve_placement_position(start, end, placement_names, lines, explicit_lines, explicit_tracks);
        ResolvedAxisPlacement {
            start: Some(resolved.start),
            span: resolved.span,
        }
    }

    fn create_item_used_values(&self, node: Node) -> *mut UsedValuesCore {
        // Intrinsic subgrid contribution contexts revisit descendants that
        // already have pass-local used values. This mirrors C++ get_mutable()
        // instead of attempting to allocate the same LayoutState entry twice.
        // SAFETY: Both callbacks synchronously access the live layout state.
        let existing = unsafe { (self.callbacks.get_used_values)(self.callbacks.context, node) };
        if !existing.is_null() {
            return existing;
        }
        let used = unsafe {
            (self.callbacks.create_used_values)(
                self.callbacks.context,
                node,
                false,
                CssPixels::default(),
                false,
                CssPixels::default(),
            )
        };
        assert!(!used.is_null());
        used
    }

    fn clamp_area_to_subgrid(start: &mut i32, span: &mut usize, track_count: usize) {
        if track_count == 0 {
            return;
        }
        let mut end = start.saturating_add(*span as i32);
        let limited_end = track_count as i32;
        if end <= 0 {
            *start = 0;
            end = 1;
        } else if *start >= limited_end {
            *start = limited_end - 1;
            end = limited_end;
        } else {
            *start = (*start).max(0);
            end = end.min(limited_end);
        }
        *span = (end - *start) as usize;
    }

    fn place_items(&mut self) {
        let mut nodes = Vec::new();
        let mut inputs = Vec::new();
        let container_grid = self.grid_facts_copy(self.grid_container);
        let subgridded_columns = self.is_subgridded(Axis::Column, &container_grid);
        let subgridded_rows = self.is_subgridded(Axis::Row, &container_grid);
        let mut child = self.navigate(self.callbacks.navigation.first_child, self.grid_container);
        while !child.is_null() {
            let next = self.navigate(self.callbacks.navigation.next_sibling, child);
            let box_facts = self.facts(child);
            if box_facts.is_box && !box_facts.is_absolutely_positioned {
                // SAFETY: The callback only inspects this live layout node.
                let skip = unsafe { (self.callbacks.can_skip_is_anonymous_text_run)(self.callbacks.context, child) };
                if !skip {
                    // SAFETY: The host mutates one flag on the live box.
                    unsafe { (self.callbacks.set_grid_item)(self.callbacks.context, child, true) };
                    let child_grid = self.grid_facts_copy(child);
                    let source = child_grid.source();
                    let column_subgrid_span = child_grid
                        .template_columns
                        .is_subgrid
                        .then(|| automatic_subgrid_span(source, child_grid.template_columns));
                    let row_subgrid_span = child_grid
                        .template_rows
                        .is_subgrid
                        .then(|| automatic_subgrid_span(source, child_grid.template_rows));
                    inputs.push(PlacementInput {
                        id: nodes.len(),
                        order: self.style(child).order,
                        row: self.axis_placements(
                            child_grid.row_start,
                            child_grid.row_end,
                            Axis::Row,
                            row_subgrid_span,
                            &child_grid.names,
                        ),
                        column: self.axis_placements(
                            child_grid.column_start,
                            child_grid.column_end,
                            Axis::Column,
                            column_subgrid_span,
                            &child_grid.names,
                        ),
                    });
                    nodes.push((child, self.create_item_used_values(child)));
                }
            }
            child = next;
        }

        let style = self.style(self.grid_container);
        let mut result = super::placement::place_items_with_grid(
            &inputs,
            self.column_lines.len().saturating_sub(1),
            self.row_lines.len().saturating_sub(1),
            if style.grid_auto_flow_row {
                AutoFlowAxis::Row
            } else {
                AutoFlowAxis::Column
            },
            style.grid_auto_flow_dense,
        );
        let column_track_count = self.column_lines.len().saturating_sub(1);
        let row_track_count = self.row_lines.len().saturating_sub(1);
        for placed in &mut result.items {
            if subgridded_columns {
                Self::clamp_area_to_subgrid(&mut placed.column, &mut placed.column_span, column_track_count);
            }
            if subgridded_rows {
                Self::clamp_area_to_subgrid(&mut placed.row, &mut placed.row_span, row_track_count);
            }
        }
        self.explicit_column_start = if subgridded_columns {
            0
        } else {
            result.explicit_column_start
        };
        self.explicit_row_start = if subgridded_rows { 0 } else { result.explicit_row_start };
        if !subgridded_columns && self.explicit_column_start > 0 {
            let mut lines = Vec::with_capacity(self.column_lines.len() + self.explicit_column_start);
            lines.resize_with(self.explicit_column_start, Vec::new);
            lines.append(&mut self.column_lines);
            self.column_lines = lines;
        }
        if !subgridded_rows && self.explicit_row_start > 0 {
            let mut lines = Vec::with_capacity(self.row_lines.len() + self.explicit_row_start);
            lines.resize_with(self.explicit_row_start, Vec::new);
            lines.append(&mut self.row_lines);
            self.row_lines = lines;
        }
        for placed in result.items {
            let (box_, used_values) = nodes[placed.id];
            self.items.push(GridItem {
                box_,
                used_values,
                row: placed.row,
                row_span: placed.row_span,
                column: placed.column,
                column_span: placed.column_span,
                extra_margin_top: CssPixels::default(),
                extra_margin_right: CssPixels::default(),
                extra_margin_bottom: CssPixels::default(),
                extra_margin_left: CssPixels::default(),
            });
        }
        if !subgridded_columns {
            self.column_lines
                .resize_with(result.column_count.saturating_add(1), Vec::new);
        }
        if !subgridded_rows {
            self.row_lines.resize_with(result.row_count.saturating_add(1), Vec::new);
        }
    }

    fn expanded_auto_tracks(&self, facts: &GridFactsCopy, axis: Axis) -> Vec<TrackDefinition> {
        let list = if axis.is_column() {
            facts.auto_columns
        } else {
            facts.auto_rows
        };
        expand_standalone(facts.source(), list, |_index, _entry| 1).tracks
    }

    fn initialize_tracks_for_axis(
        &self,
        axis: Axis,
        facts: &GridFactsCopy,
        explicit: &ExpandedTrackList,
        total_count: usize,
        explicit_start: usize,
    ) -> Vec<Track> {
        if self.is_subgridded(axis, facts) {
            let Some(parent) = self.parent_grid() else {
                return vec![Track::auto()];
            };
            let parent_item = self.parent_grid_item().unwrap();
            let parent_tracks = if axis.is_column() {
                &parent.columns
            } else {
                &parent.rows
            };
            let mut tracks = Vec::new();
            for offset in 0..parent_item.span(axis) {
                let index = parent_item.position(axis) + offset as i32;
                if let Some(parent_track) = usize::try_from(index).ok().and_then(|index| parent_tracks.get(index)) {
                    if self.layout_mode == LAYOUT_MODE_INTRINSIC_SIZING {
                        let mut track = *parent_track;
                        track.base_size = CssPixels::default();
                        track.growth_limit = Some(CssPixels::default());
                        tracks.push(track);
                    } else {
                        tracks.push(Track::fixed(parent_track.base_size));
                    }
                } else {
                    tracks.push(Track::auto());
                }
            }
            if tracks.is_empty() {
                tracks.push(Track::auto());
            }
            return tracks;
        }

        let automatic = self.expanded_auto_tracks(facts, axis);
        let mut automatic_index = 0usize;
        let mut tracks = Vec::with_capacity(total_count);
        for _ in 0..explicit_start {
            tracks.push(if automatic.is_empty() {
                Track::auto()
            } else {
                let definition = automatic[automatic_index % automatic.len()];
                automatic_index += 1;
                Track::from_definition(definition)
            });
        }
        tracks.extend(explicit.tracks.iter().copied().map(Track::from_definition));
        while tracks.len() < total_count {
            tracks.push(if automatic.is_empty() {
                Track::auto()
            } else {
                let definition = automatic[automatic_index % automatic.len()];
                automatic_index += 1;
                Track::from_definition(definition)
            });
        }
        tracks
    }

    fn collapse_auto_fit(&mut self, axis: Axis) {
        let occupied = |track_index: usize, items: &[GridItem]| {
            items.iter().any(|item| {
                let start = item.position(axis).max(0) as usize;
                track_index >= start && track_index < start.saturating_add(item.span(axis))
            })
        };
        let items = &self.items;
        let tracks = if axis.is_column() {
            &mut self.columns
        } else {
            &mut self.rows
        };
        for (index, track) in tracks.iter_mut().enumerate() {
            if track.is_auto_fit && !occupied(index, items) {
                track.collapse();
            }
        }
    }

    fn initialize_gaps_for_axis(&mut self, axis: Axis, available: AvailableSize) {
        let gap_size = self.resolved_gap(axis, available);
        let tracks = if axis.is_column() { &self.columns } else { &self.rows };
        let mut gaps = Vec::with_capacity(tracks.len().saturating_sub(1));
        let mut seen_non_collapsed = false;
        for index in 0..tracks.len().saturating_sub(1) {
            seen_non_collapsed |= !tracks[index].is_collapsed;
            let collapse = tracks[index + 1].is_collapsed || !seen_non_collapsed;
            gaps.push(Track::gap(if collapse { CssPixels::default() } else { gap_size }));
        }
        if axis.is_column() {
            self.column_gaps = gaps;
        } else {
            self.row_gaps = gaps;
        }
    }

    fn initialize_tracks(&mut self, facts: &GridFactsCopy, columns: &ExpandedTrackList, rows: &ExpandedTrackList) {
        self.columns = self.initialize_tracks_for_axis(
            Axis::Column,
            facts,
            columns,
            self.column_lines.len().saturating_sub(1),
            self.explicit_column_start,
        );
        self.rows = self.initialize_tracks_for_axis(
            Axis::Row,
            facts,
            rows,
            self.row_lines.len().saturating_sub(1),
            self.explicit_row_start,
        );
        self.collapse_auto_fit(Axis::Column);
        self.collapse_auto_fit(Axis::Row);
        let available = self.available_space.unwrap();
        self.initialize_gaps_for_axis(Axis::Column, available.inline_size);
        self.initialize_gaps_for_axis(Axis::Row, available.block_size);
    }

    fn axis_tracks(&self, axis: Axis) -> &[Track] {
        if axis.is_column() { &self.columns } else { &self.rows }
    }

    fn axis_gaps(&self, axis: Axis) -> &[Track] {
        if axis.is_column() {
            &self.column_gaps
        } else {
            &self.row_gaps
        }
    }

    fn interleaved_tracks(&self, axis: Axis) -> Vec<Track> {
        let tracks = self.axis_tracks(axis);
        let gaps = self.axis_gaps(axis);
        let mut result = Vec::with_capacity(tracks.len().saturating_mul(2).saturating_sub(1));
        for (index, track) in tracks.iter().copied().enumerate() {
            result.push(track);
            if let Some(gap) = gaps.get(index) {
                result.push(*gap);
            }
        }
        result
    }

    fn store_interleaved_tracks(&mut self, axis: Axis, interleaved: &[Track]) {
        let (tracks, gaps) = if axis.is_column() {
            (&mut self.columns, &mut self.column_gaps)
        } else {
            (&mut self.rows, &mut self.row_gaps)
        };
        for (index, track) in tracks.iter_mut().enumerate() {
            *track = interleaved[index * 2];
        }
        for (index, gap) in gaps.iter_mut().enumerate() {
            *gap = interleaved[index * 2 + 1];
        }
    }

    fn spanned_interleaved_indices(item: GridItem, axis: Axis, track_count: usize) -> Vec<usize> {
        let start = item.position(axis).max(0) as usize;
        let end = start.saturating_add(item.span(axis)).min(track_count);
        let mut indices = Vec::new();
        for track in start..end {
            indices.push(track * 2);
            if track + 1 < end {
                indices.push(track * 2 + 1);
            }
        }
        indices
    }

    fn containing_block_size(&self, item: GridItem, axis: Axis) -> CssPixels {
        let tracks = self.axis_tracks(axis);
        let gaps = self.axis_gaps(axis);
        let start = item.position(axis).max(0) as usize;
        let end = start.saturating_add(item.span(axis)).min(tracks.len());
        let mut size = CssPixels::default();
        for (index, track) in tracks.iter().enumerate().take(end).skip(start) {
            size += track.base_size;
            if index + 1 < end {
                size += gaps.get(index).map_or(CssPixels::default(), |gap| gap.base_size);
            }
        }
        size
    }

    fn item_available_space(&self, item: GridItem) -> AvailableSpace {
        let used = self.used(item);
        AvailableSpace {
            inline_size: if used.has_definite_inline_size() {
                AvailableSize::definite(used.content_inline_size)
            } else {
                AvailableSize::indefinite()
            },
            block_size: if used.has_definite_block_size() {
                AvailableSize::definite(used.content_block_size)
            } else {
                AvailableSize::indefinite()
            },
        }
    }

    fn track_sizing_constraints(&self) -> FfiContainingBlockConstraints {
        let inherited = self.sizing().constraints_for_child_context(
            self.grid_container,
            self.layout_input.unwrap().containing_block_constraints,
        );
        FfiContainingBlockConstraints {
            has_percentage_basis_inline_size: false,
            percentage_basis_inline_size: CssPixels::default(),
            has_percentage_basis_block_size: false,
            percentage_basis_block_size: CssPixels::default(),
            ..inherited
        }
    }

    fn container_constraints(&self) -> FfiContainingBlockConstraints {
        self.sizing().constraints_for_child_context(
            self.grid_container,
            self.layout_input.unwrap().containing_block_constraints,
        )
    }

    fn grid_area_constraints(&self, item: GridItem) -> FfiContainingBlockConstraints {
        let inherited = self.track_sizing_constraints();
        FfiContainingBlockConstraints {
            has_percentage_basis_inline_size: true,
            percentage_basis_inline_size: self.containing_block_size(item, Axis::Column),
            ..inherited
        }
    }

    fn outer_edges(&self, item: GridItem, axis: Axis) -> CssPixels {
        let used = self.used(item);
        if axis.is_column() {
            used.margin_left
                + used.border_left
                + used.padding_left
                + used.padding_right
                + used.border_right
                + used.margin_right
                + item.extra_margin_left
                + item.extra_margin_right
        } else {
            used.margin_top
                + used.border_top
                + used.padding_top
                + used.padding_bottom
                + used.border_bottom
                + used.margin_bottom
                + item.extra_margin_top
                + item.extra_margin_bottom
        }
    }

    fn add_outer_size(&self, item: GridItem, axis: Axis, size: CssPixels) -> CssPixels {
        size + self.outer_edges(item, axis)
    }

    fn preferred_size(&self, item: GridItem, axis: Axis) -> FfiSizeValue {
        let style = self.style(item.box_);
        if axis.is_column() { style.width } else { style.height }
    }

    fn minimum_size(&self, item: GridItem, axis: Axis) -> FfiSizeValue {
        let style = self.style(item.box_);
        if axis.is_column() {
            style.min_width
        } else {
            style.min_height
        }
    }

    fn maximum_size(&self, item: GridItem, axis: Axis) -> FfiSizeValue {
        let style = self.style(item.box_);
        if axis.is_column() {
            style.max_width
        } else {
            style.max_height
        }
    }

    fn preferred_behaves_as_auto(&self, item: GridItem, axis: Axis) -> bool {
        let available = self.item_available_space(item);
        let behaves_as_auto = self.sizing().should_treat_size_as_auto(
            item.box_,
            if axis.is_column() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
            available,
            self.track_sizing_constraints(),
        );
        behaves_as_auto
            || (!self.facts(item.box_).is_replaced_box && self.preferred_size(item, axis).contains_percentage)
    }

    fn min_content_size(&self, item: GridItem, axis: Axis) -> CssPixels {
        if axis.is_column() {
            self.sizing()
                .calculate_min_content_inline_size(item.box_, self.container_constraints())
        } else {
            self.sizing().calculate_min_content_block_size(
                item.box_,
                self.item_available_space(item).inline_size.to_px_or_zero(),
                self.container_constraints(),
            )
        }
    }

    fn max_content_size(&self, item: GridItem, axis: Axis) -> CssPixels {
        if axis.is_column() {
            self.sizing()
                .calculate_max_content_inline_size(item.box_, self.container_constraints())
        } else {
            self.sizing().calculate_max_content_block_size(
                item.box_,
                self.item_available_space(item).inline_size.to_px_or_zero(),
                self.container_constraints(),
            )
        }
    }

    fn min_content_contribution(&self, item: GridItem, axis: Axis) -> CssPixels {
        let max = self.maximum_size(item, axis);
        let maximum = if max.is_length_percentage() && !max.contains_percentage {
            max.to_px(CssPixels::default())
        } else {
            CssPixels::from_raw(i32::MAX)
        };
        let content = if self.preferred_behaves_as_auto(item, axis) {
            if axis.is_column() && self.facts(item.box_).is_scroll_container {
                CssPixels::default()
            } else {
                self.min_content_size(item, axis)
            }
        } else {
            let property = if axis.is_column() {
                FfiFlexSizeProperty::Width
            } else {
                FfiFlexSizeProperty::Height
            };
            self.sizing().calculate_inner_size_for_property(
                item.box_,
                if axis.is_column() {
                    FfiFlexAxis::Inline
                } else {
                    FfiFlexAxis::Block
                },
                property,
                self.available_space.unwrap(),
                self.track_sizing_constraints(),
            )
        };
        self.add_outer_size(item, axis, content).min(maximum)
    }

    fn max_content_contribution(&self, item: GridItem, axis: Axis) -> CssPixels {
        let max = self.maximum_size(item, axis);
        let maximum = if max.is_length_percentage() && !max.contains_percentage {
            max.to_px(CssPixels::default())
        } else {
            CssPixels::from_raw(i32::MAX)
        };
        let preferred = self.preferred_size(item, axis);
        let content = if self.preferred_behaves_as_auto(item, axis) || preferred.is_fit_content() {
            self.sizing().calculate_fit_content_size(
                item.box_,
                if axis.is_column() {
                    FfiFlexAxis::Inline
                } else {
                    FfiFlexAxis::Block
                },
                self.item_available_space(item),
                self.container_constraints(),
            )
        } else {
            let area_space = AvailableSpace {
                inline_size: AvailableSize::definite(clamp_dimension(self.containing_block_size(item, Axis::Column))),
                block_size: AvailableSize::definite(clamp_dimension(self.containing_block_size(item, Axis::Row))),
            };
            self.sizing().calculate_inner_size_for_property(
                item.box_,
                if axis.is_column() {
                    FfiFlexAxis::Inline
                } else {
                    FfiFlexAxis::Block
                },
                if axis.is_column() {
                    FfiFlexSizeProperty::Width
                } else {
                    FfiFlexSizeProperty::Height
                },
                area_space,
                self.track_sizing_constraints(),
            )
        };
        self.add_outer_size(item, axis, content).min(maximum)
    }

    fn automatic_minimum_size(&self, item: GridItem, axis: Axis) -> CssPixels {
        let available = self.axis_available(axis);
        let tracks = self.axis_tracks(axis);
        let start = item.position(axis).max(0) as usize;
        let end = start.saturating_add(item.span(axis)).min(tracks.len());
        if start >= end {
            return CssPixels::default();
        }
        let spans_auto = tracks[start..end]
            .iter()
            .any(|track| track.min_sizing.is_auto(available));
        let spans_flexible = tracks[start..end]
            .iter()
            .any(|track| track.max_sizing.flex_factor().is_some());
        if spans_auto && !self.facts(item.box_).is_scroll_container && (item.span(axis) == 1 || !spans_flexible) {
            let mut result = self.min_content_size(item, axis);
            let available = self.axis_available(axis);
            let mut fixed_track_limit = CssPixels::default();
            let mut all_fixed = true;
            for track in &tracks[start..end] {
                if !track.max_sizing.is_fixed(available) {
                    all_fixed = false;
                    break;
                }
                fixed_track_limit += track.max_sizing.resolve(available);
            }
            if all_fixed {
                result = result.min(fixed_track_limit);
            }
            let maximum = self.maximum_size(item, axis);
            if maximum.is_length_percentage() && !maximum.contains_percentage {
                result = result.min(maximum.to_px(CssPixels::default()));
            }
            let preferred = self.preferred_size(item, axis);
            if self.facts(item.box_).is_replaced_box
                && (preferred.kind() == crate::style_facts::FfiSizeKind::Percentage
                    || maximum.kind() == crate::style_facts::FfiSizeKind::Percentage)
            {
                result = CssPixels::default();
            }
            return result;
        }
        CssPixels::default()
    }

    fn minimum_contribution(&self, item: GridItem, axis: Axis) -> CssPixels {
        if !self.preferred_behaves_as_auto(item, axis) {
            return self.min_content_contribution(item, axis);
        }
        let minimum = self.minimum_size(item, axis);
        let content = if minimum.is_auto() {
            self.automatic_minimum_size(item, axis)
        } else if minimum.is_min_content() {
            return self.min_content_contribution(item, axis);
        } else if minimum.is_max_content() {
            return self.max_content_contribution(item, axis);
        } else {
            let mut available = self.item_available_space(item);
            if axis.is_column() && self.facts(item.box_).is_table_wrapper && minimum.contains_percentage {
                let containing = self.containing_block_size(item, Axis::Column);
                available.inline_size = AvailableSize::definite(clamp_dimension(
                    self.non_cyclic_table_wrapper_inline_size(item, containing),
                ));
            }
            self.sizing().calculate_inner_size_for_property(
                item.box_,
                if axis.is_column() {
                    FfiFlexAxis::Inline
                } else {
                    FfiFlexAxis::Block
                },
                if axis.is_column() {
                    FfiFlexSizeProperty::MinWidth
                } else {
                    FfiFlexSizeProperty::MinHeight
                },
                available,
                self.track_sizing_constraints(),
            )
        };
        self.add_outer_size(item, axis, content)
    }

    fn fixed_track_limit(&self, item: GridItem, axis: Axis) -> Option<CssPixels> {
        let available = self.axis_available(axis);
        let tracks = self.axis_tracks(axis);
        let gaps = self.axis_gaps(axis);
        let start = item.position(axis).max(0) as usize;
        let end = start.saturating_add(item.span(axis)).min(tracks.len());
        if start == end {
            return None;
        }
        let mut result = CssPixels::default();
        for (index, track) in tracks.iter().enumerate().take(end).skip(start) {
            let max = track.max_sizing;
            if max.is_fixed(available)
                || matches!(max, TrackSizingFunction::FitContent(value) if !value.contains_percentage || available.is_definite())
            {
                result += max.resolve(available);
            } else {
                return None;
            }
            if index + 1 < end {
                result += gaps.get(index).map_or(CssPixels::default(), |gap| gap.base_size);
            }
        }
        Some(result)
    }

    fn grid_container_maximum_size(&self, axis: Axis) -> Option<CssPixels> {
        let available = self.available_space.unwrap();
        let constraints = self.layout_input.unwrap().containing_block_constraints;
        let sizing = self.sizing();
        let is_none = if axis.is_column() {
            sizing.should_treat_max_inline_size_as_none(self.grid_container, available.inline_size, constraints)
        } else {
            sizing.should_treat_max_block_size_as_none(self.grid_container, available.block_size, constraints)
        };
        if is_none {
            return None;
        }
        Some(sizing.calculate_inner_size_for_property(
            self.grid_container,
            if axis.is_column() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
            if axis.is_column() {
                FfiFlexSizeProperty::MaxWidth
            } else {
                FfiFlexSizeProperty::MaxHeight
            },
            available,
            constraints,
        ))
    }

    fn limited_content_contribution(
        &self,
        content: CssPixels,
        minimum: CssPixels,
        item: GridItem,
        axis: Axis,
    ) -> CssPixels {
        if content < minimum {
            return minimum;
        }
        if let Some(limit) = self.fixed_track_limit(item, axis) {
            return content.min(limit).max(minimum);
        }
        if let Some(maximum) = self.grid_container_maximum_size(axis)
            && content > maximum
        {
            return maximum;
        }
        content
    }

    fn item_contribution(&self, item: GridItem, axis: Axis, combined_track_count: usize) -> ItemContribution {
        let minimum = self.minimum_contribution(item, axis);
        let min_content = self.min_content_contribution(item, axis);
        let max_content = self.max_content_contribution(item, axis);
        let limited_min = self.limited_content_contribution(min_content, minimum, item, axis);
        let limited_max = self.limited_content_contribution(max_content, minimum, item, axis);
        ItemContribution {
            spanned_tracks: Self::spanned_interleaved_indices(item, axis, combined_track_count),
            span: item.span(axis),
            minimum,
            min_content,
            limited_min_content: limited_min,
            max_content,
            limited_max_content: limited_max,
            is_scroll_container: self.facts(item.box_).is_scroll_container,
        }
    }

    fn grid_item_is_subgridded(&self, item: GridItem, axis: Axis) -> bool {
        if !self.facts(item.box_).display.is_grid_inside() {
            return false;
        }
        let facts = self.grid_facts_copy(item.box_);
        if axis.is_column() {
            facts.template_columns.is_subgrid
        } else {
            facts.template_rows.is_subgrid
        }
    }

    fn apply_subgrid_edge_extra_margins(&self, item: &mut GridItem, axis: Axis) {
        let facts = self.grid_facts_copy(self.grid_container);
        if !self.is_subgridded(axis, &facts) {
            return;
        }
        let used = self.container_used();
        let (start, end) = if axis.is_column() {
            (
                used.margin_left + used.border_left + used.padding_left,
                used.padding_right + used.border_right + used.margin_right,
            )
        } else {
            (
                used.margin_top + used.border_top + used.padding_top,
                used.padding_bottom + used.border_bottom + used.margin_bottom,
            )
        };
        if item.position(axis) == 0 {
            if axis.is_column() {
                item.extra_margin_left += start;
            } else {
                item.extra_margin_top += start;
            }
        }
        if item.position(axis) + item.span(axis) as i32 == self.axis_tracks(axis).len() as i32 {
            if axis.is_column() {
                item.extra_margin_right += end;
            } else {
                item.extra_margin_bottom += end;
            }
        }
    }

    fn subgrid_items_contributing_to_track_sizing(&self, subgrid: GridItem, axis: Axis) -> Vec<GridItem> {
        let mut context = GridFormattingContext::new(
            self.state,
            subgrid.box_,
            std::ptr::null_mut(),
            LAYOUT_MODE_INTRINSIC_SIZING,
            self.callbacks,
            false,
        );
        context.parent_grid_override = self;
        let mut available = self.available_space.unwrap();
        if !axis.is_column() && self.used(subgrid).has_definite_inline_size() {
            available.inline_size = AvailableSize::definite(self.used(subgrid).content_inline_size);
        }
        let input = FfiLayoutInput {
            available_space: available,
            containing_block_constraints: self.track_sizing_constraints(),
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: Default::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        };
        context.reset_for_run(input);
        let facts = context.grid_facts_copy(context.grid_container);
        let (columns, rows) = context.initialize_lines(&facts);
        context.place_items();
        context.initialize_tracks(&facts, &columns, &rows);
        if !axis.is_column() {
            context.resolve_item_metrics(Axis::Column);
            context.run_track_sizing(Axis::Column);
            context.resolve_item_metrics(Axis::Column);
            context.resolve_item_sizes(Axis::Column);
        }
        context.resolve_item_metrics(axis);

        let mut result = context.items_contributing_to_track_sizing(axis);
        for item in &mut result {
            context.apply_subgrid_edge_extra_margins(item, axis);
            if axis.is_column() {
                item.column += subgrid.column;
            } else {
                item.row += subgrid.row;
            }
        }
        result
    }

    fn items_contributing_to_track_sizing(&self, axis: Axis) -> Vec<GridItem> {
        let mut result = Vec::new();
        for item in self.items.iter().copied() {
            if self.grid_item_is_subgridded(item, axis) {
                result.extend(self.subgrid_items_contributing_to_track_sizing(item, axis));
            } else {
                result.push(item);
            }
        }
        result
    }

    fn run_track_sizing(&mut self, axis: Axis) {
        let mut tracks = self.interleaved_tracks(axis);
        let contributions = self
            .items_contributing_to_track_sizing(axis)
            .into_iter()
            .map(|item| self.item_contribution(item, axis, self.axis_tracks(axis).len()))
            .collect::<Vec<_>>();
        let style = self.style(self.grid_container);
        let distribution_stretches = if axis.is_column() {
            matches!(
                style.justify_content,
                justify_content::NORMAL | justify_content::STRETCH
            )
        } else {
            matches!(style.align_content, align_content::NORMAL | align_content::STRETCH)
        };
        run_track_sizing(
            &mut tracks,
            CssPixels::default(),
            &contributions,
            self.axis_available(axis),
            !axis.is_column(),
            distribution_stretches,
        );
        self.store_interleaved_tracks(axis, &tracks);
    }

    fn resolve_item_metrics(&mut self, axis: Axis) {
        let items = self.items.clone();
        for item in items {
            let style = self.style(item.box_);
            let inline_basis = self.containing_block_size(item, Axis::Column);
            let extra_margin = self.subgrid_gap_extra_margin(axis, self.axis_available(axis));
            let item_start = item.position(axis);
            let item_end = item_start + item.span(axis) as i32;
            let track_count = self.axis_tracks(axis).len() as i32;
            let used = self.used_mut(item);
            if axis.is_column() {
                used.padding_left = style.padding_left.to_px(inline_basis);
                used.padding_right = style.padding_right.to_px(inline_basis);
                used.margin_left = style.margin_left.to_px(inline_basis);
                used.margin_right = style.margin_right.to_px(inline_basis);
                used.border_left = style.border_left_width;
                used.border_right = style.border_right_width;
                if item_start > 0 {
                    used.margin_left += extra_margin;
                }
                if item_end < track_count {
                    used.margin_right += extra_margin;
                }
            } else {
                used.padding_top = style.padding_top.to_px(inline_basis);
                used.padding_bottom = style.padding_bottom.to_px(inline_basis);
                used.margin_top = style.margin_top.to_px(inline_basis);
                used.margin_bottom = style.margin_bottom.to_px(inline_basis);
                used.border_top = style.border_top_width;
                used.border_bottom = style.border_bottom_width;
                if item_start > 0 {
                    used.margin_top += extra_margin;
                }
                if item_end < track_count {
                    used.margin_bottom += extra_margin;
                }
            }
        }
    }

    fn item_alignment(&self, item: GridItem, axis: Axis) -> Alignment {
        let item_style = self.style(item.box_);
        let container_style = self.style(self.grid_container);
        if axis.is_column() {
            inline_item_alignment(item_style.justify_self, container_style.justify_items)
        } else {
            block_item_alignment(item_style.align_self, container_style.align_items)
        }
    }

    fn item_margin_box_start(&self, item: GridItem, axis: Axis) -> CssPixels {
        let used = self.used(item);
        if axis.is_column() {
            used.margin_left + used.border_left + used.padding_left + item.extra_margin_left
        } else {
            used.margin_top + used.border_top + used.padding_top + item.extra_margin_top
        }
    }

    fn item_margin_box_end(&self, item: GridItem, axis: Axis) -> CssPixels {
        let used = self.used(item);
        if axis.is_column() {
            used.padding_right + used.border_right + used.margin_right + item.extra_margin_right
        } else {
            used.padding_bottom + used.border_bottom + used.margin_bottom + item.extra_margin_bottom
        }
    }

    fn table_box_inside_wrapper(&self, wrapper: Node) -> Node {
        let mut pending = Vec::new();
        let first = self.navigate(self.callbacks.navigation.first_child, wrapper);
        if !first.is_null() {
            pending.push(first);
        }
        while let Some(node) = pending.pop() {
            if self.facts(node).is_table_box {
                return node;
            }
            let sibling = self.navigate(self.callbacks.navigation.next_sibling, node);
            if !sibling.is_null() {
                pending.push(sibling);
            }
            let child = self.navigate(self.callbacks.navigation.first_child, node);
            if !child.is_null() {
                pending.push(child);
            }
        }
        panic!("table wrapper without a table box");
    }

    fn non_cyclic_table_wrapper_inline_size(&self, item: GridItem, containing: CssPixels) -> CssPixels {
        let table_box = self.table_box_inside_wrapper(item.box_);
        let table_style = self.style(table_box);
        let wrapper_style = self.style(item.box_);
        if !wrapper_style.width.contains_percentage
            && !wrapper_style.min_width.contains_percentage
            && !wrapper_style.max_width.contains_percentage
            && !table_style.width.contains_percentage
            && !table_style.min_width.contains_percentage
            && !table_style.max_width.contains_percentage
        {
            return containing;
        }

        let container = self.container_used();
        if !container.has_definite_inline_size() {
            return containing;
        }

        let available = AvailableSize::definite(clamp_dimension(container.content_inline_size));
        let tracks = self.axis_tracks(Axis::Column);
        let start = item.position(Axis::Column).max(0) as usize;
        let end = start.saturating_add(item.span(Axis::Column)).min(tracks.len());
        if !tracks[start..end]
            .iter()
            .any(|track| track.min_sizing.is_intrinsic(available) || track.max_sizing.is_intrinsic(available))
        {
            return containing;
        }

        let total = self.track_sum(Axis::Column);
        if total <= container.content_inline_size {
            return containing;
        }

        let non_spanned = CssPixels::default().max(total - containing);
        let non_cyclic = CssPixels::default().max(container.content_inline_size - non_spanned);
        containing.min(non_cyclic)
    }

    fn resolve_table_wrapper_inline_size(&self, item: GridItem, containing: CssPixels) -> ItemAlignment {
        const BOX_SIZING_BORDER_BOX: u8 = 0;

        let containing_for_wrapper = self.non_cyclic_table_wrapper_inline_size(item, containing);
        let containing_block = self.containing_block_size(item, Axis::Row);
        let available = AvailableSpace {
            inline_size: AvailableSize::definite(clamp_dimension(containing_for_wrapper)),
            block_size: AvailableSize::definite(clamp_dimension(containing_block)),
        };
        let mut constraints = self.container_constraints();
        constraints.has_percentage_basis_inline_size = true;
        constraints.percentage_basis_inline_size = containing_for_wrapper;

        // SAFETY: The callback runs the existing table inline-size algorithm
        // synchronously in a throwaway measurement state.
        let mut wrapper_size = unsafe {
            (self.callbacks.compute_table_box_inline_size_inside_wrapper)(
                self.callbacks.context,
                item.box_,
                available,
                constraints,
                true,
                containing_for_wrapper,
                1,
            )
        };
        let wrapper_style = self.style(item.box_);
        let table_box = self.table_box_inside_wrapper(item.box_);
        let table_style = self.style(table_box);
        let sizing = self.sizing();
        let current_used = self.used(item);
        let base_margin_start = wrapper_style.margin_left.to_px(containing_for_wrapper);
        let base_margin_end = wrapper_style.margin_right.to_px(containing_for_wrapper);
        let margin_box_start =
            self.item_margin_box_start(item, Axis::Column) - current_used.margin_left + base_margin_start;
        let margin_box_end = self.item_margin_box_end(item, Axis::Column) - current_used.margin_right + base_margin_end;

        if !wrapper_style.width.is_auto() {
            wrapper_size = wrapper_size.max(sizing.calculate_inner_size_for_property(
                item.box_,
                FfiFlexAxis::Inline,
                FfiFlexSizeProperty::Width,
                available,
                self.grid_area_constraints(item),
            ));
        }
        if table_style.width.is_auto() {
            wrapper_size = wrapper_size.max(sizing.calculate_min_content_inline_size(item.box_, constraints));
        }

        let alignment = self.item_alignment(item, Axis::Column);
        if table_style.width.is_auto()
            && matches!(alignment, Alignment::Normal | Alignment::Stretch)
            && !wrapper_style.margin_left.is_auto()
            && !wrapper_style.margin_right.is_auto()
        {
            let mut stretched = containing_for_wrapper - margin_box_start - margin_box_end;
            let area_constraints = self.grid_area_constraints(item);
            if !sizing.should_treat_max_inline_size_as_none(item.box_, available.inline_size, area_constraints) {
                stretched = stretched.min(sizing.calculate_inner_size_for_property(
                    item.box_,
                    FfiFlexAxis::Inline,
                    FfiFlexSizeProperty::MaxWidth,
                    available,
                    area_constraints,
                ));
            }
            if !sizing.should_treat_max_inline_size_as_none(table_box, available.inline_size, area_constraints) {
                if table_style.max_width.is_length_percentage() {
                    let mut table_max = table_style.max_width.to_px(containing_for_wrapper);
                    if table_style.box_sizing != BOX_SIZING_BORDER_BOX {
                        table_max += table_style.border_left_width
                            + table_style.padding_left.to_px(containing_for_wrapper)
                            + table_style.padding_right.to_px(containing_for_wrapper)
                            + table_style.border_right_width;
                    }
                    stretched = stretched.min(table_max);
                } else {
                    stretched = stretched.min(wrapper_size);
                }
            }
            wrapper_size = wrapper_size.max(stretched);
        }

        let area_constraints = self.grid_area_constraints(item);
        if !sizing.should_treat_max_inline_size_as_none(item.box_, available.inline_size, area_constraints) {
            wrapper_size = wrapper_size.min(sizing.calculate_inner_size_for_property(
                item.box_,
                FfiFlexAxis::Inline,
                FfiFlexSizeProperty::MaxWidth,
                available,
                area_constraints,
            ));
        }
        if !wrapper_style.min_width.is_auto() {
            wrapper_size = wrapper_size.max(sizing.calculate_inner_size_for_property(
                item.box_,
                FfiFlexAxis::Inline,
                FfiFlexSizeProperty::MinWidth,
                available,
                area_constraints,
            ));
        }

        align_item(
            wrapper_size,
            false,
            false,
            containing_for_wrapper,
            margin_box_start,
            margin_box_end,
            base_margin_start + item.extra_margin_left,
            base_margin_end + item.extra_margin_right,
            wrapper_style.margin_left.is_auto(),
            wrapper_style.margin_right.is_auto(),
            alignment,
        )
    }

    fn resolve_item_sizes(&mut self, axis: Axis) {
        let items = self.items.clone();
        for item in items {
            let containing = self.containing_block_size(item, axis);
            let containing_inline = self.containing_block_size(item, Axis::Column);
            let containing_block = self.containing_block_size(item, Axis::Row);
            let available = AvailableSpace {
                inline_size: AvailableSize::definite(clamp_dimension(containing_inline)),
                block_size: AvailableSize::definite(clamp_dimension(containing_block)),
            };
            let mut constraints = self.grid_area_constraints(item);
            if !axis.is_column() {
                constraints.has_percentage_basis_block_size = true;
                constraints.percentage_basis_block_size = containing;
            }
            let style = self.style(item.box_);
            let preferred = if axis.is_column() { style.width } else { style.height };
            let alignment = self.item_alignment(item, axis);
            let facts = self.facts(item.box_);
            let has_natural = if axis.is_column() {
                facts.has_auto_content_width || (facts.has_auto_content_height && facts.has_preferred_aspect_ratio)
            } else {
                facts.has_auto_content_height || (facts.has_auto_content_width && facts.has_preferred_aspect_ratio)
            };
            let use_replaced = facts.is_replaced_box
                && (has_natural || (alignment == Alignment::Normal && !facts.is_replaced_box_with_children));

            if facts.is_table_wrapper && axis.is_column() {
                let resolved = self.resolve_table_wrapper_inline_size(item, containing);
                let used = self.used_mut(item);
                used.margin_left = resolved.margin_start;
                used.margin_right = resolved.margin_end;
                used.set_content_inline_size(resolved.size);
                continue;
            }

            let size = if use_replaced {
                if axis.is_column() {
                    self.sizing()
                        .compute_inline_size_for_replaced_element(item.box_, available, constraints)
                } else {
                    self.sizing()
                        .compute_block_size_for_replaced_element(item.box_, available, constraints)
                }
            } else if !axis.is_column()
                && preferred.is_auto()
                && facts.has_preferred_aspect_ratio
                && self.used(item).has_definite_inline_size()
            {
                self.sizing().calculate_inner_size_for_property(
                    item.box_,
                    FfiFlexAxis::Block,
                    FfiFlexSizeProperty::Height,
                    available,
                    constraints,
                )
            } else if preferred.is_auto()
                && matches!(alignment, Alignment::Stretch | Alignment::Normal)
                && !(if axis.is_column() {
                    style.margin_left.is_auto() || style.margin_right.is_auto()
                } else {
                    style.margin_top.is_auto() || style.margin_bottom.is_auto()
                })
            {
                containing - self.item_margin_box_start(item, axis) - self.item_margin_box_end(item, axis)
            } else if preferred.is_auto() || preferred.is_fit_content() {
                self.sizing().calculate_fit_content_size(
                    item.box_,
                    if axis.is_column() {
                        FfiFlexAxis::Inline
                    } else {
                        FfiFlexAxis::Block
                    },
                    available,
                    constraints,
                )
            } else {
                self.sizing().calculate_inner_size_for_property(
                    item.box_,
                    if axis.is_column() {
                        FfiFlexAxis::Inline
                    } else {
                        FfiFlexAxis::Block
                    },
                    if axis.is_column() {
                        FfiFlexSizeProperty::Width
                    } else {
                        FfiFlexSizeProperty::Height
                    },
                    available,
                    constraints,
                )
            };

            let used = self.used(item);
            let (used_margin_start, used_margin_end, start_auto, end_auto) = if axis.is_column() {
                (
                    used.margin_left + item.extra_margin_left,
                    used.margin_right + item.extra_margin_right,
                    style.margin_left.is_auto(),
                    style.margin_right.is_auto(),
                )
            } else {
                (
                    used.margin_top + item.extra_margin_top,
                    used.margin_bottom + item.extra_margin_bottom,
                    style.margin_top.is_auto(),
                    style.margin_bottom.is_auto(),
                )
            };
            let resolve_alignment = |size, size_is_auto| {
                align_item(
                    size,
                    size_is_auto,
                    facts.is_replaced_box,
                    containing,
                    self.item_margin_box_start(item, axis),
                    self.item_margin_box_end(item, axis),
                    used_margin_start,
                    used_margin_end,
                    start_auto,
                    end_auto,
                    alignment,
                )
            };
            let mut resolved = resolve_alignment(size, preferred.is_auto());

            let maximum_is_none = if axis.is_column() {
                self.sizing()
                    .should_treat_max_inline_size_as_none(item.box_, available.inline_size, constraints)
            } else {
                self.sizing()
                    .should_treat_max_block_size_as_none(item.box_, available.block_size, constraints)
            };
            if !maximum_is_none {
                let maximum = self.sizing().calculate_inner_size_for_property(
                    item.box_,
                    if axis.is_column() {
                        FfiFlexAxis::Inline
                    } else {
                        FfiFlexAxis::Block
                    },
                    if axis.is_column() {
                        FfiFlexSizeProperty::MaxWidth
                    } else {
                        FfiFlexSizeProperty::MaxHeight
                    },
                    available,
                    constraints,
                );
                if resolved.size > maximum {
                    resolved = resolve_alignment(maximum, false);
                }
            }
            let minimum = if axis.is_column() {
                style.min_width
            } else {
                style.min_height
            };
            if !minimum.is_auto() {
                let minimum = self.sizing().calculate_inner_size_for_property(
                    item.box_,
                    if axis.is_column() {
                        FfiFlexAxis::Inline
                    } else {
                        FfiFlexAxis::Block
                    },
                    if axis.is_column() {
                        FfiFlexSizeProperty::MinWidth
                    } else {
                        FfiFlexSizeProperty::MinHeight
                    },
                    available,
                    constraints,
                );
                if resolved.size < minimum {
                    resolved = resolve_alignment(minimum, false);
                }
            }

            let used = self.used_mut(item);
            if axis.is_column() {
                used.margin_left = resolved.margin_start;
                used.margin_right = resolved.margin_end;
                used.set_content_inline_size(resolved.size);
            } else {
                used.margin_top = resolved.margin_start;
                used.margin_bottom = resolved.margin_end;
                used.set_content_block_size(resolved.size);
            }
        }
    }

    fn track_sum(&self, axis: Axis) -> CssPixels {
        self.interleaved_tracks(axis)
            .iter()
            .fold(CssPixels::default(), |sum, track| sum + track.base_size)
    }

    fn grid_container_alignment_size(&self, axis: Axis) -> CssPixels {
        if axis.is_column() {
            return self.container_used().content_inline_size;
        }
        if self.use_row_alignment_container_size {
            let style = self.style(self.grid_container);
            if !style.min_height.is_auto() {
                return self
                    .row_alignment_container_size
                    .max(self.container_used().content_block_size);
            }
            return self.row_alignment_container_size;
        }
        if self.container_used().has_definite_block_size() {
            self.container_used().content_block_size
        } else {
            self.row_alignment_container_size
        }
    }

    fn resolve_track_spacing(&mut self, axis: Axis) {
        let container = self.grid_container_alignment_size(axis);
        let track_sum = self
            .axis_tracks(axis)
            .iter()
            .fold(CssPixels::default(), |sum, track| sum + track.base_size);
        let alignment = if axis.is_column() {
            inline_content_alignment(self.style(self.grid_container).justify_content)
        } else {
            block_content_alignment(self.style(self.grid_container).align_content)
        };
        let minimum = self.resolved_gap(axis, AvailableSize::definite(container));
        let size = distributed_gap_size(alignment, container, track_sum, self.axis_gaps(axis).len(), minimum);
        let gaps = if axis.is_column() {
            &mut self.column_gaps
        } else {
            &mut self.row_gaps
        };
        for gap in gaps {
            gap.base_size = size;
        }
    }

    fn used_container_block_size_for_second_row_layout(&self) -> CssPixels {
        let mut block_size = self.automatic_content_block_size;
        let available = self.available_space.unwrap();
        let constraints = self.layout_input.unwrap().containing_block_constraints;
        let style = self.style(self.grid_container);
        let sizing = self.sizing();
        if !style.max_height.is_auto()
            && !sizing.should_treat_max_block_size_as_none(self.grid_container, available.block_size, constraints)
        {
            block_size = block_size.min(sizing.calculate_inner_size_for_property(
                self.grid_container,
                FfiFlexAxis::Block,
                FfiFlexSizeProperty::MaxHeight,
                available,
                constraints,
            ));
        }
        if !style.min_height.is_auto() {
            block_size = block_size.max(sizing.calculate_inner_size_for_property(
                self.grid_container,
                FfiFlexAxis::Block,
                FfiFlexSizeProperty::MinHeight,
                available,
                constraints,
            ));
        }
        block_size
    }

    fn rerun_rows_with_container_block_size(&mut self, block_size: CssPixels) {
        self.available_space.as_mut().unwrap().block_size = AvailableSize::definite(block_size);
        self.initialize_gaps_for_axis(Axis::Row, AvailableSize::definite(block_size));
        self.resolve_item_metrics(Axis::Row);
        self.run_track_sizing(Axis::Row);
        self.resolve_item_metrics(Axis::Row);
        self.resolve_item_sizes(Axis::Row);
    }

    fn grid_area(&self, item: GridItem) -> LogicalRect {
        let (inline_offset, inline_size) = self.axis_grid_area(Axis::Column, Some((item.column, item.column_span)));
        let (block_offset, block_size) = self.axis_grid_area(Axis::Row, Some((item.row, item.row_span)));
        LogicalRect {
            offset: LogicalOffset {
                inline_offset,
                block_offset,
            },
            size: LogicalSize {
                inline_size,
                block_size,
            },
        }
    }

    fn axis_grid_area(&self, axis: Axis, placement: Option<(i32, usize)>) -> (CssPixels, CssPixels) {
        let tracks = self.interleaved_tracks(axis);
        let padding_start = if axis.is_column() {
            self.container_used().padding_left
        } else {
            self.container_used().padding_top
        };
        let padding_end = if axis.is_column() {
            self.container_used().padding_right
        } else {
            self.container_used().padding_bottom
        };
        let Some((position, span)) = placement else {
            let content_size = if self.axis_available(axis).is_definite() {
                self.axis_available(axis).value
            } else {
                self.track_sum(axis)
            };
            return (-padding_start, content_size + padding_start + padding_end);
        };
        if position == self.axis_tracks(axis).len() as i32 {
            return (self.track_sum(axis), padding_end);
        }

        let start = position.saturating_mul(2);
        let end = start.saturating_add(span.saturating_mul(2) as i32);
        let container = self.grid_container_alignment_size(axis);
        let alignment = if axis.is_column() {
            inline_content_alignment(self.style(self.grid_container).justify_content)
        } else {
            block_content_alignment(self.style(self.grid_container).align_content)
        };
        let initial_offset = content_start_offset(alignment, container, self.track_sum(axis));
        let mut start_offset = initial_offset;
        let mut end_offset = initial_offset;
        for track in tracks.iter().take(start.max(0) as usize) {
            start_offset += track.base_size;
        }
        for track in tracks.iter().take(end.max(0) as usize) {
            end_offset += track.base_size;
        }
        (start_offset, end_offset - start_offset)
    }

    fn absolute_axis_grid_area(
        &self,
        axis: Axis,
        start: FfiGridPlacement,
        end: FfiGridPlacement,
        placement_names: &[usize],
    ) -> (CssPixels, CssPixels) {
        let lines = if axis.is_column() {
            &self.column_lines
        } else {
            &self.row_lines
        };
        let explicit_lines = if axis.is_column() {
            self.explicit_column_line_count
        } else {
            self.explicit_row_line_count
        };
        let resolved = resolve_placement_position(
            start,
            end,
            placement_names,
            lines,
            explicit_lines,
            lines.len().saturating_sub(1),
        );
        let is_auto_positioned =
            |placement: FfiGridPlacement| placement.kind != super::facts::FfiGridPlacementKind::Line as u8;
        let mut rect = self.axis_grid_area(
            axis,
            (!(is_auto_positioned(start) && is_auto_positioned(end))).then_some((resolved.start, resolved.span)),
        );

        let start_is_augmented =
            is_auto_positioned(start) && end.kind == super::facts::FfiGridPlacementKind::Line as u8;
        let end_is_augmented = is_auto_positioned(end) && start.kind == super::facts::FfiGridPlacementKind::Line as u8;
        if !start_is_augmented && !end_is_augmented {
            return rect;
        }

        let explicit_line_position = |line: i32| {
            let tracks = self.interleaved_tracks(axis);
            let alignment = if axis.is_column() {
                inline_content_alignment(self.style(self.grid_container).justify_content)
            } else {
                block_content_alignment(self.style(self.grid_container).align_content)
            };
            let mut offset = content_start_offset(
                alignment,
                self.axis_available(axis).to_px_or_zero(),
                self.track_sum(axis),
            );
            for track in tracks.iter().take(line.saturating_mul(2).max(0) as usize) {
                offset += track.base_size;
            }
            offset
        };
        let augmented_edge = |is_start: bool| {
            if is_start {
                if axis.is_column() {
                    -self.container_used().padding_left
                } else {
                    -self.container_used().padding_top
                }
            } else {
                let mut offset = if self.axis_available(axis).is_definite() {
                    self.axis_available(axis).value
                } else {
                    self.track_sum(axis)
                };
                offset += if axis.is_column() {
                    self.container_used().padding_right
                } else {
                    self.container_used().padding_bottom
                };
                offset
            }
        };
        let start_offset = if start_is_augmented {
            augmented_edge(true)
        } else {
            explicit_line_position(resolved.start)
        };
        let end_offset = if end_is_augmented {
            augmented_edge(false)
        } else {
            explicit_line_position(resolved.end)
        };
        rect = (start_offset, end_offset - start_offset);
        rect
    }

    fn layout_items(&mut self) {
        let items = self.items.clone();
        for item in items {
            let area = self.grid_area(item);
            let mut table_wrapper_inline_basis = None;
            if self.facts(item.box_).is_table_wrapper {
                let resolved = self.resolve_table_wrapper_inline_size(item, area.size.inline_size);
                table_wrapper_inline_basis =
                    Some(self.non_cyclic_table_wrapper_inline_size(item, area.size.inline_size));
                let used = self.used_mut(item);
                used.margin_left = resolved.margin_start;
                used.margin_right = resolved.margin_end;
                used.set_content_inline_size(resolved.size);
            }
            {
                let used = self.used_mut(item);
                used.has_definite_inline_size = true;
                used.has_definite_block_size = true;
            }
            let input = FfiLayoutInput {
                available_space: AvailableSpace {
                    inline_size: AvailableSize::definite(self.used(item).content_inline_size),
                    block_size: AvailableSize::definite(self.used(item).content_block_size),
                },
                containing_block_constraints: {
                    let mut constraints = self.grid_area_constraints(item);
                    constraints.has_percentage_basis_block_size = true;
                    constraints.percentage_basis_block_size = area.size.block_size;
                    if let Some(inline_basis) = table_wrapper_inline_basis {
                        constraints.has_percentage_basis_inline_size = true;
                        constraints.percentage_basis_inline_size = inline_basis;
                    }
                    constraints
                },
                has_content_box_position_in_bfc_root: false,
                content_box_position_in_bfc_root: Default::default(),
                has_table_grid_min_border_box_block_size: false,
                table_grid_min_border_box_block_size: CssPixels::default(),
            };
            let mut result = FfiChildLayoutResult::default();
            // SAFETY: Child layout and callback dispatch are synchronous.
            let did_layout = unsafe {
                (self.callbacks.layout_inside_child)(
                    self.callbacks.context,
                    item.box_,
                    LAYOUT_MODE_NORMAL,
                    input,
                    &raw mut result,
                )
            };
            let offset = FfiCssPixelPoint {
                x: area.offset.inline_offset + self.item_margin_box_start(item, Axis::Column),
                y: area.offset.block_offset + self.item_margin_box_start(item, Axis::Row),
            };
            // SAFETY: The host owns this live item.
            unsafe {
                (self.callbacks.place_child)(self.callbacks.context, item.box_, offset);
            }
            super::super::abspos::compute_inset_native(
                self.state,
                self.callbacks,
                self.layout_mode,
                self.grid_container,
                item.box_,
                area.size.inline_size,
                area.size.block_size,
            );
            if did_layout {
                // SAFETY: A successful layout call retained this child
                // context until the matching parent-dimension callback.
                unsafe {
                    (self.callbacks.parent_did_dimension_child_root_box)(self.callbacks.context, item.box_);
                }
            }
        }
        // SAFETY: Baseline computation reads the finished live subtree.
        unsafe {
            (self.callbacks.compute_and_store_baselines)(self.callbacks.context, self.grid_container);
        }
    }

    fn used_track_list_storage(
        &self,
        axis: Axis,
        subgrid: bool,
    ) -> (
        Vec<Vec<usize>>,
        Vec<FfiUsedGridLine>,
        Vec<CssPixels>,
        FfiUsedGridTrackList,
    ) {
        let lines = if axis.is_column() {
            &self.column_lines
        } else {
            &self.row_lines
        };
        let mut names = Vec::with_capacity(lines.len());
        for line in lines {
            names.push(
                line.iter()
                    .filter(|name| !name.implicit && (!subgrid || !name.adopted_from_parent))
                    .map(|name| name.raw)
                    .collect::<Vec<_>>(),
            );
        }
        let ffi_lines = names
            .iter()
            .map(|names| FfiUsedGridLine {
                names: names.as_ptr(),
                name_count: names.len(),
            })
            .collect::<Vec<_>>();
        let track_sizes = if subgrid {
            Vec::new()
        } else {
            self.axis_tracks(axis).iter().map(|track| track.base_size).collect()
        };
        let ffi = FfiUsedGridTrackList {
            is_subgrid: subgrid,
            lines: ffi_lines.as_ptr(),
            line_count: ffi_lines.len(),
            track_sizes: track_sizes.as_ptr(),
            track_count: track_sizes.len(),
        };
        (names, ffi_lines, track_sizes, ffi)
    }

    fn save_used_tracks(&self, facts: &GridFactsCopy) {
        let (_column_names, _column_lines, _column_sizes, columns) =
            self.used_track_list_storage(Axis::Column, self.is_subgridded(Axis::Column, facts));
        let (_row_names, _row_lines, _row_sizes, rows) =
            self.used_track_list_storage(Axis::Row, self.is_subgridded(Axis::Row, facts));
        // SAFETY: Every pointer in both lists remains live for this
        // synchronous callback.
        unsafe {
            (self.callbacks.set_used_grid_template_tracks)(
                self.callbacks.context,
                self.grid_container,
                &raw const columns,
                &raw const rows,
            );
        }
    }

    fn save_devtools_data(&self, facts: &GridFactsCopy) {
        if !self.should_collect_devtools_layout_data {
            return;
        }
        let serialize = |axis: Axis| {
            let tracks = self.axis_tracks(axis);
            let gaps = self.axis_gaps(axis);
            let lines = if axis.is_column() {
                &self.column_lines
            } else {
                &self.row_lines
            };
            let explicit_start = if axis.is_column() {
                self.explicit_column_start
            } else {
                self.explicit_row_start
            };
            let explicit_count = if axis.is_column() {
                self.explicit_column_line_count
            } else {
                self.explicit_row_line_count
            };
            let alignment = if axis.is_column() {
                inline_content_alignment(self.style(self.grid_container).justify_content)
            } else {
                block_content_alignment(self.style(self.grid_container).align_content)
            };
            let mut start = content_start_offset(
                alignment,
                self.grid_container_alignment_size(axis),
                self.track_sum(axis),
            );
            let mut name_storage = Vec::with_capacity(lines.len());
            for line in lines {
                name_storage.push(line.iter().map(|name| name.raw).collect::<Vec<_>>());
            }
            let mut ffi_lines = Vec::with_capacity(lines.len());
            let mut ffi_tracks = Vec::with_capacity(tracks.len());
            for (index, names) in name_storage.iter().enumerate().take(lines.len()) {
                let breadth = index
                    .checked_sub(1)
                    .and_then(|gap| gaps.get(gap))
                    .map_or(CssPixels::default(), |gap| gap.base_size);
                ffi_lines.push(FfiGridLayoutLine {
                    names: names.as_ptr(),
                    name_count: names.len(),
                    start,
                    breadth,
                    type_: if index < explicit_start || index >= explicit_start + explicit_count {
                        1
                    } else {
                        0
                    },
                    number: if index < explicit_start {
                        0
                    } else {
                        (index - explicit_start + 1) as u32
                    },
                    negative_number: if index >= explicit_start + explicit_count {
                        0
                    } else {
                        -((explicit_start + explicit_count - index) as i32)
                    },
                });
                if let Some(track) = tracks.get(index) {
                    ffi_tracks.push(FfiGridLayoutTrack {
                        start: start + breadth,
                        breadth: track.base_size,
                        type_: if index < explicit_start || index >= explicit_start + explicit_count.saturating_sub(1) {
                            1
                        } else {
                            0
                        },
                        state: if track.is_auto_repeat {
                            if track.is_auto_fit && track.is_collapsed { 2 } else { 1 }
                        } else {
                            0
                        },
                    });
                    start += breadth + track.base_size;
                }
            }
            (name_storage, ffi_lines, ffi_tracks)
        };
        let (_column_names, column_lines, column_tracks) = serialize(Axis::Column);
        let (_row_names, row_lines, row_tracks) = serialize(Axis::Row);
        let areas = facts
            .areas
            .iter()
            .map(|area| FfiGridLayoutArea {
                name: facts.names[area.name_index as usize],
                type_: 0,
                row_start: area.row_start as u32 + 1,
                row_end: area.row_end as u32 + 1,
                column_start: area.column_start as u32 + 1,
                column_end: area.column_end as u32 + 1,
            })
            .collect::<Vec<_>>();
        let fragment = FfiGridLayoutFragment {
            areas: areas.as_ptr(),
            area_count: areas.len(),
            columns: FfiGridLayoutDimension {
                lines: column_lines.as_ptr(),
                line_count: column_lines.len(),
                tracks: column_tracks.as_ptr(),
                track_count: column_tracks.len(),
            },
            rows: FfiGridLayoutDimension {
                lines: row_lines.as_ptr(),
                line_count: row_lines.len(),
                tracks: row_tracks.as_ptr(),
                track_count: row_tracks.len(),
            },
        };
        let style = self.style(self.grid_container);
        let data = FfiGridLayoutData {
            direction: style.direction,
            writing_mode: style.writing_mode,
            is_subgrid: self.is_subgridded(Axis::Column, facts) || self.is_subgridded(Axis::Row, facts),
            fragments: &raw const fragment,
            fragment_count: 1,
        };
        // SAFETY: The nested stack/vector storage remains live throughout the
        // synchronous deep-copy callback.
        unsafe {
            (self.callbacks.set_grid_layout_data)(self.callbacks.context, self.grid_container, &raw const data);
        }
    }

    pub(crate) fn run(&mut self, input: FfiLayoutInput) {
        let available = input.available_space;
        if self.layout_mode == LAYOUT_MODE_INTRINSIC_SIZING
            && !available.inline_size.is_intrinsic_sizing_constraint()
            && !available.block_size.is_intrinsic_sizing_constraint()
            && !self.facts(self.grid_container).display.is_inline_outside()
        {
            return;
        }
        self.reset_for_run(input);
        let facts = self.grid_facts_copy(self.grid_container);
        let (columns, rows) = self.initialize_lines(&facts);
        self.place_items();
        self.initialize_tracks(&facts, &columns, &rows);

        self.resolve_item_metrics(Axis::Column);
        self.run_track_sizing(Axis::Column);
        self.resolve_item_metrics(Axis::Column);
        self.resolve_item_sizes(Axis::Column);

        self.resolve_item_metrics(Axis::Row);
        self.run_track_sizing(Axis::Row);
        self.resolve_item_metrics(Axis::Row);
        self.resolve_item_sizes(Axis::Row);

        self.automatic_content_block_size = self.track_sum(Axis::Row);
        self.row_alignment_container_size = self.automatic_content_block_size;
        self.use_row_alignment_container_size = false;
        let intrinsic_block_size = self.automatic_content_block_size;
        if self.layout_mode == LAYOUT_MODE_NORMAL && available.block_size.is_indefinite() {
            let resolved_block_size = self.used_container_block_size_for_second_row_layout();
            self.rerun_rows_with_container_block_size(resolved_block_size);
            self.row_alignment_container_size = resolved_block_size;
            self.use_row_alignment_container_size = true;
            self.automatic_content_block_size = intrinsic_block_size;
        } else if self.layout_mode == LAYOUT_MODE_NORMAL
            && available.block_size.is_definite()
            && self.sizing().should_treat_block_size_as_auto(
                self.grid_container,
                available,
                self.layout_input.unwrap().containing_block_constraints,
            )
        {
            self.row_alignment_container_size = available.block_size.value;
            self.use_row_alignment_container_size = true;
        }
        self.resolve_track_spacing(Axis::Column);
        self.resolve_track_spacing(Axis::Row);

        if available.inline_size.is_intrinsic_sizing_constraint()
            || available.block_size.is_intrinsic_sizing_constraint()
        {
            if available.inline_size.is_intrinsic_sizing_constraint() {
                let size = self.track_sum(Axis::Column);
                self.container_used_mut().set_content_inline_size(size);
            }
            if available.block_size.is_intrinsic_sizing_constraint() {
                let size = self.track_sum(Axis::Row);
                self.container_used_mut().set_content_block_size(size);
            }
            return;
        }

        self.layout_items();
        self.save_used_tracks(&facts);
        self.save_devtools_data(&facts);
    }

    pub(crate) fn automatic_content_inline_size(&self) -> CssPixels {
        self.container_used().content_inline_size
    }

    pub(crate) fn automatic_content_block_size(&self) -> CssPixels {
        self.automatic_content_block_size
    }

    pub(crate) fn parent_did_dimension(&self) {
        if self.layout_mode != LAYOUT_MODE_NORMAL {
            return;
        }
        let mut child = self.navigate(self.callbacks.navigation.first_child, self.grid_container);
        while !child.is_null() {
            let next = self.navigate(self.callbacks.navigation.next_sibling, child);
            if self.facts(child).is_absolutely_positioned {
                // Grid placement is supplied through the containing-block
                // query. Match the C++ registration's empty static rect.
                let rect = FfiStaticPositionRect {
                    rect: LogicalRect::default(),
                    inline_alignment: FfiStaticPositionAlignment::Start,
                    block_alignment: FfiStaticPositionAlignment::Start,
                    alignment_derives_from_own_computed_values: false,
                };
                // SAFETY: Registration deep-copies this POD value.
                unsafe {
                    (self.callbacks.register_contained_abspos_child)(self.callbacks.context, child, rect);
                }
            }
            child = next;
        }
    }

    pub(crate) fn abspos_containing_block_info(&self, node: Node) -> FfiAbsposContainingBlockInfo {
        let facts = self.grid_facts_copy(node);
        let (block_offset, block_size) =
            self.absolute_axis_grid_area(Axis::Row, facts.row_start, facts.row_end, &facts.names);
        let (inline_offset, inline_size) =
            self.absolute_axis_grid_area(Axis::Column, facts.column_start, facts.column_end, &facts.names);
        let item = GridItem {
            box_: node,
            used_values: std::ptr::null_mut(),
            row: 0,
            row_span: 1,
            column: 0,
            column_span: 1,
            extra_margin_top: CssPixels::default(),
            extra_margin_right: CssPixels::default(),
            extra_margin_bottom: CssPixels::default(),
            extra_margin_left: CssPixels::default(),
        };
        FfiAbsposContainingBlockInfo {
            rect: LogicalRect {
                offset: LogicalOffset {
                    inline_offset,
                    block_offset,
                },
                size: LogicalSize {
                    inline_size,
                    block_size,
                },
            },
            inline_axis_mode: FfiAbsposAxisMode::InsetFromRect,
            block_axis_mode: FfiAbsposAxisMode::InsetFromRect,
            has_inline_alignment: true,
            inline_alignment: abspos_alignment(self.item_alignment(item, Axis::Column)),
            has_block_alignment: true,
            block_alignment: abspos_alignment(self.item_alignment(item, Axis::Row)),
            derives_from_own_computed_values: true,
        }
    }
}

fn clamp_dimension(value: CssPixels) -> CssPixels {
    if matches!(value.raw_value(), i32::MIN | i32::MAX) {
        CssPixels::from_integer(MAX_DIMENSION)
    } else {
        value
    }
}

fn abspos_alignment(alignment: Alignment) -> FfiAbsposAlignment {
    match alignment {
        Alignment::Baseline => FfiAbsposAlignment::Baseline,
        Alignment::Center => FfiAbsposAlignment::Center,
        Alignment::End => FfiAbsposAlignment::End,
        Alignment::Normal => FfiAbsposAlignment::Normal,
        Alignment::Safe => FfiAbsposAlignment::Safe,
        Alignment::SelfEnd => FfiAbsposAlignment::SelfEnd,
        Alignment::SelfStart => FfiAbsposAlignment::SelfStart,
        Alignment::SpaceAround => FfiAbsposAlignment::SpaceAround,
        Alignment::SpaceBetween => FfiAbsposAlignment::SpaceBetween,
        Alignment::SpaceEvenly => FfiAbsposAlignment::SpaceEvenly,
        Alignment::Start => FfiAbsposAlignment::Start,
        Alignment::Stretch => FfiAbsposAlignment::Stretch,
        Alignment::Unsafe => FfiAbsposAlignment::Unsafe,
    }
}
