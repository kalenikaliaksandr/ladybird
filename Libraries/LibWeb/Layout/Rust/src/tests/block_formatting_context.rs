/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::ffi::c_void;

use crate::css_pixels::CssPixels;
use crate::formatting_context::block::BlockMarginState;

fn px(value: i64) -> CssPixels {
    CssPixels::from_integer(value)
}

#[test]
fn collapsed_margin_uses_the_largest_positive_and_most_negative_values() {
    let mut state = BlockMarginState::default();

    state.add_margin(px(10));
    state.add_margin(px(20));
    state.add_margin(px(-4));
    state.add_margin(px(-7));

    assert_eq!(state.current_collapsed_margin(), px(13));
}

#[test]
fn only_the_last_pending_top_margin_group_remains_open() {
    let mut state = BlockMarginState::default();
    let mut first_box_value = 0u8;
    let mut second_box_value = 0u8;
    let first_box = (&raw mut first_box_value).cast::<c_void>();
    let second_box = (&raw mut second_box_value).cast::<c_void>();

    state.add_margin(px(8));
    state.open_top_margin_group(first_box, false);
    state.update_open_top_margin_group();
    state.reset();

    state.add_margin(px(3));
    state.add_margin(px(-5));
    state.open_top_margin_group(second_box, true);
    state.update_open_top_margin_group();

    let groups = state.pending_top_margin_groups();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].box_, first_box);
    assert_eq!(groups[0].collapsed_margin, px(8));
    assert!(!groups[0].open);
    assert_eq!(groups[1].box_, second_box);
    assert_eq!(groups[1].collapsed_margin_at_open, px(-2));
    assert_eq!(groups[1].collapsed_margin, px(-2));
    assert!(groups[1].open);
    assert!(groups[1].pinned_by_clearance);

    assert_eq!(state.take_pending_top_margin(), px(-2));
    assert!(!state.has_open_top_margin_group());
}
