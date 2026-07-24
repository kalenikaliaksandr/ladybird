/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Column {
    pub(crate) inline_offset: CssPixels,
    pub(crate) min_size: CssPixels,
    pub(crate) max_size: CssPixels,
    pub(crate) used_inline_size: CssPixels,
    pub(crate) has_intrinsic_percentage: bool,
    pub(crate) intrinsic_percentage: f64,
    pub(crate) is_constrained: bool,
    pub(crate) has_originating_cells: bool,
}

fn total_used(columns: &[Column]) -> CssPixels {
    columns
        .iter()
        .fold(CssPixels::default(), |sum, column| sum + column.used_inline_size)
}

fn total_candidates(candidates: &[CssPixels]) -> CssPixels {
    candidates
        .iter()
        .copied()
        .fold(CssPixels::default(), std::ops::Add::add)
}

fn commit_candidates(columns: &mut [Column], candidates: &[CssPixels]) {
    assert_eq!(columns.len(), candidates.len());
    for (column, candidate) in columns.iter_mut().zip(candidates) {
        column.used_inline_size = *candidate;
    }
}

fn assign_linear_combination(columns: &mut [Column], candidates: &[CssPixels], available: CssPixels) {
    let candidate_total = total_candidates(candidates);
    let used_total = total_used(columns);
    if candidate_total == used_total {
        return;
    }
    let weight = (available - used_total).to_double() / (candidate_total - used_total).to_double();
    for (column, candidate) in columns.iter_mut().zip(candidates) {
        column.used_inline_size = CssPixels::nearest_value_for(
            weight * candidate.to_double() + (1.0 - weight) * column.used_inline_size.to_double(),
        );
    }
}

fn distribute_proportionally(
    columns: &mut [Column],
    excess: CssPixels,
    filter: impl Fn(&Column) -> bool,
    base: impl Fn(&Column) -> CssPixels,
) -> bool {
    let mut found = false;
    let mut total = CssPixels::default();
    for column in columns.iter() {
        if filter(column) {
            total += base(column);
            found = true;
        }
    }
    if !found {
        return false;
    }
    assert!(total > CssPixels::default());
    for column in columns.iter_mut() {
        if filter(column) {
            column.used_inline_size +=
                CssPixels::nearest_value_for(excess.to_double() * base(column).to_double() / total.to_double());
        }
    }
    true
}

fn distribute_equally(columns: &mut [Column], excess: CssPixels, filter: impl Fn(&Column) -> bool) -> bool {
    let count = columns.iter().filter(|column| filter(column)).count();
    if count == 0 {
        return false;
    }
    for column in columns.iter_mut() {
        if filter(column) {
            column.used_inline_size += excess / count;
        }
    }
    true
}

fn distribute_by_percentage(columns: &mut [Column], excess: CssPixels, filter: impl Fn(&Column) -> bool) -> bool {
    let mut found = false;
    let mut total = 0.0;
    for column in columns.iter() {
        if filter(column) {
            found = true;
            total += column.intrinsic_percentage;
        }
    }
    if !found {
        return false;
    }
    for column in columns.iter_mut() {
        if filter(column) {
            column.used_inline_size +=
                CssPixels::nearest_value_for(excess.to_double() * column.intrinsic_percentage / total);
        }
    }
    true
}

fn distribute_excess_fixed(columns: &mut [Column], excess: CssPixels) {
    if distribute_equally(columns, excess, |column| {
        !column.is_constrained && !column.has_intrinsic_percentage
    }) {
        return;
    }
    if distribute_proportionally(
        columns,
        excess,
        |column| column.used_inline_size > CssPixels::default(),
        |column| column.used_inline_size,
    ) {
        return;
    }
    if distribute_by_percentage(columns, excess, |column| column.intrinsic_percentage > 0.0) {
        return;
    }
    distribute_equally(columns, excess, |column| {
        column.used_inline_size == CssPixels::default()
    });
}

fn distribute_excess(columns: &mut [Column], available: CssPixels, fixed: bool) {
    let used = total_used(columns);
    if used >= available {
        return;
    }
    let mut excess = available - used;
    if excess == CssPixels::default() {
        return;
    }
    if fixed {
        distribute_excess_fixed(columns, excess);
        return;
    }

    if distribute_proportionally(
        columns,
        excess,
        |column| {
            !column.is_constrained
                && column.has_originating_cells
                && column.intrinsic_percentage == 0.0
                && column.max_size > CssPixels::default()
        },
        |column| column.max_size,
    ) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    if distribute_equally(columns, excess, |column| {
        !column.is_constrained && column.has_originating_cells && column.intrinsic_percentage == 0.0
    }) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    if distribute_proportionally(
        columns,
        excess,
        |column| column.is_constrained && column.intrinsic_percentage == 0.0 && column.max_size > CssPixels::default(),
        |column| column.max_size,
    ) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    if distribute_by_percentage(columns, excess, |column| column.intrinsic_percentage > 0.0) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    if distribute_equally(columns, excess, |column| column.has_originating_cells) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    distribute_equally(columns, excess, |_| true);
}

pub(crate) fn distribute_inline_size(columns: &mut [Column], available: CssPixels, fixed: bool) {
    let mut candidates = vec![CssPixels::default(); columns.len()];
    for (index, column) in columns.iter_mut().enumerate() {
        if fixed && !column.is_constrained {
            continue;
        }
        column.used_inline_size = column.min_size;
        candidates[index] = column.min_size;
    }
    for (candidate, column) in candidates.iter_mut().zip(columns.iter()) {
        if column.has_intrinsic_percentage {
            *candidate = column.min_size.max(CssPixels::nearest_value_for(
                column.intrinsic_percentage / 100.0 * available.to_double(),
            ));
        }
    }
    if available < total_candidates(&candidates) {
        assign_linear_combination(columns, &candidates, available);
        return;
    }
    commit_candidates(columns, &candidates);

    for (candidate, column) in candidates.iter_mut().zip(columns.iter()) {
        if column.is_constrained {
            *candidate = column.max_size;
        }
    }
    if available < total_candidates(&candidates) {
        assign_linear_combination(columns, &candidates, available);
        return;
    }
    commit_candidates(columns, &candidates);

    for (candidate, column) in candidates.iter_mut().zip(columns.iter()) {
        if !column.has_intrinsic_percentage {
            *candidate = column.max_size;
        }
    }
    if available < total_candidates(&candidates) {
        assign_linear_combination(columns, &candidates, available);
        return;
    }
    commit_candidates(columns, &candidates);
    distribute_excess(columns, available, fixed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn interpolates_between_consecutive_width_guesses() {
        let mut columns = [
            Column {
                min_size: px(10),
                max_size: px(30),
                is_constrained: true,
                has_originating_cells: true,
                ..Column::default()
            },
            Column {
                min_size: px(20),
                max_size: px(40),
                is_constrained: true,
                has_originating_cells: true,
                ..Column::default()
            },
        ];
        distribute_inline_size(&mut columns, px(50), false);
        assert_eq!(columns[0].used_inline_size, px(20));
        assert_eq!(columns[1].used_inline_size, px(30));
    }

    #[test]
    fn fixed_mode_gives_unspecified_columns_equal_excess() {
        let mut columns = [
            Column {
                min_size: px(20),
                max_size: px(20),
                is_constrained: true,
                ..Column::default()
            },
            Column::default(),
            Column::default(),
        ];
        distribute_inline_size(&mut columns, px(100), true);
        assert_eq!(columns[0].used_inline_size, px(20));
        assert_eq!(columns[1].used_inline_size, px(40));
        assert_eq!(columns[2].used_inline_size, px(40));
    }

    #[test]
    fn percentage_guess_precedes_max_content_growth() {
        let mut columns = [
            Column {
                min_size: px(10),
                max_size: px(80),
                has_intrinsic_percentage: true,
                intrinsic_percentage: 50.0,
                has_originating_cells: true,
                ..Column::default()
            },
            Column {
                min_size: px(10),
                max_size: px(80),
                has_originating_cells: true,
                ..Column::default()
            },
        ];
        distribute_inline_size(&mut columns, px(100), false);
        assert_eq!(columns[0].used_inline_size, px(50));
        assert_eq!(columns[1].used_inline_size, px(50));
    }
}
