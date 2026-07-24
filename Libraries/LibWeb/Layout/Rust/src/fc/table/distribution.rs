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
    // Store whether the column is constrained: https://www.w3.org/TR/css-tables-3/#constrainedness
    pub(crate) is_constrained: bool,
    // Store whether the column has originating cells, defined in https://www.w3.org/TR/css-tables-3/#terminology.
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
    // Implements the fixed mode for https://www.w3.org/TR/css-tables-3/#distributing-width-to-columns.

    // If any columns have no specified inline size, distribute the excess inline size equally to them.
    if distribute_equally(columns, excess, |column| {
        !column.is_constrained && !column.has_intrinsic_percentage
    }) {
        return;
    }
    // Otherwise, distribute proportionally among columns with non-zero length sizes from the base assignment.
    if distribute_proportionally(
        columns,
        excess,
        |column| column.used_inline_size > CssPixels::default(),
        |column| column.used_inline_size,
    ) {
        return;
    }
    // Otherwise, distribute proportionally among columns with non-zero percentage sizes from the base assignment.
    if distribute_by_percentage(columns, excess, |column| column.intrinsic_percentage > 0.0) {
        return;
    }
    // Otherwise, distribute the excess inline size equally to the zero-sized columns.
    distribute_equally(columns, excess, |column| {
        column.used_inline_size == CssPixels::default()
    });
}

fn distribute_excess(columns: &mut [Column], available: CssPixels, fixed: bool) {
    // Implements https://www.w3.org/TR/css-tables-3/#distributing-width-to-columns
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

    // 1. If there are non-constrained columns that have originating cells with intrinsic percentage width of 0% and with nonzero
    //    max-content width (aka the columns allowed to grow by this rule), the distributed widths of the columns allowed to grow
    //    by this rule are increased in proportion to max-content width so the total increase adds to the excess width.
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
    // 2. Otherwise, if there are non-constrained columns that have originating cells with intrinsic percentage width of 0% (aka the columns
    //    allowed to grow by this rule, which thanks to the previous rule must have zero max-content width), the distributed widths of the
    //    columns allowed to grow by this rule are increased by equal amounts so the total increase adds to the excess width.
    if distribute_equally(columns, excess, |column| {
        !column.is_constrained && column.has_originating_cells && column.intrinsic_percentage == 0.0
    }) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    // 3. Otherwise, if there are constrained columns with intrinsic percentage width of 0% and with nonzero max-content width
    //    (aka the columns allowed to grow by this rule, which, due to other rules, must have originating cells), the distributed widths of the
    //    columns allowed to grow by this rule are increased in proportion to max-content width so the total increase adds to the excess width.
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
    // 4. Otherwise, if there are columns with intrinsic percentage width greater than 0% (aka the columns allowed to grow by this rule,
    //    which, due to other rules, must have originating cells), the distributed widths of the columns allowed to grow by this rule are
    //    increased in proportion to intrinsic percentage width so the total increase adds to the excess width.
    if distribute_by_percentage(columns, excess, |column| column.intrinsic_percentage > 0.0) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    // 5. Otherwise, if there is any such column, the distributed widths of all columns that have originating cells are increased by equal amounts
    //    so the total increase adds to the excess width.
    if distribute_equally(columns, excess, |column| column.has_originating_cells) {
        excess = available - total_used(columns);
    }
    if excess == CssPixels::default() {
        return;
    }
    // 6. Otherwise, the distributed widths of all columns are increased by equal amounts so the total increase adds to the excess width.
    distribute_equally(columns, excess, |_| true);
}

pub(crate) fn distribute_inline_size(columns: &mut [Column], available: CssPixels, fixed: bool) {
    // Implements https://www.w3.org/TR/css-tables-3/#width-distribution-algorithm

    let mut candidates = vec![CssPixels::default(); columns.len()];
    // 1. The min-content sizing guess assigns every column its min-content inline size.
    for (index, column) in columns.iter_mut().enumerate() {
        // In fixed mode, the min-content width of percent-columns and auto-columns is considered to be zero:
        // https://www.w3.org/TR/css-tables-3/#width-distribution-in-fixed-mode
        if fixed && !column.is_constrained {
            continue;
        }
        column.used_inline_size = column.min_size;
        candidates[index] = column.min_size;
    }
    // 2. The min-content-percentage sizing-guess is the set of column width assignments where:
    //    - each percent-column is assigned the larger of:
    //      - its intrinsic percentage width times the assignable width and
    //      - its min-content width.
    //    - all other columns are assigned their min-content width.
    for (candidate, column) in candidates.iter_mut().zip(columns.iter()) {
        if column.has_intrinsic_percentage {
            *candidate = column.min_size.max(CssPixels::nearest_value_for(
                column.intrinsic_percentage / 100.0 * available.to_double(),
            ));
        }
    }
    // If the assignable inline size is no larger than the max-content sizing guess, use the linear combination
    // of the consecutive sizing guesses whose sums bound the available inline size.
    if available < total_candidates(&candidates) {
        assign_linear_combination(columns, &candidates, available);
        return;
    }
    commit_candidates(columns, &candidates);

    // 3. The min-content-specified sizing-guess is the set of column width assignments where:
    //    - each percent-column is assigned the larger of:
    //      - its intrinsic percentage width times the assignable width and
    //      - its min-content width
    //    - any other column that is constrained is assigned its max-content width
    //    - all other columns are assigned their min-content width.
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

    // 4. The max-content sizing-guess is the set of column width assignments where:
    //    - each percent-column is assigned the larger of:
    //      - its intrinsic percentage width times the assignable width and
    //      - its min-content width
    //    - all other columns are assigned their max-content width.
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
    // Otherwise, start from the max-content sizing guess and distribute the excess inline size to the columns.
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
