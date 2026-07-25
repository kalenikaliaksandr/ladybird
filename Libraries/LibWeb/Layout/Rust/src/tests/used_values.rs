/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::geometry::{AvailableSize, AvailableSpace};
use crate::used_values::{FfiCssPixelPoint, FfiLineBoxFragmentCoordinate, FfiSizeConstraint, UsedValuesCore};

fn raw(value: i32) -> CssPixels {
    CssPixels::from_raw(value)
}

fn px(value: i64) -> CssPixels {
    CssPixels::from_integer(value)
}

#[test]
fn core_fields_and_flags_round_trip_in_rust() {
    let mut used = UsedValuesCore {
        node: std::ptr::dangling_mut(),
        ..UsedValuesCore::default()
    };
    assert_eq!(used.node, std::ptr::dangling_mut());

    used.set_content_inline_size(raw(101));
    used.set_content_block_size(raw(202));
    assert_eq!(used.content_inline_size, raw(101));
    assert_eq!(used.content_block_size, raw(202));

    used.margin_left = raw(11);
    used.margin_right = raw(12);
    used.margin_top = raw(13);
    used.margin_bottom = raw(14);
    used.border_left = raw(21);
    used.border_right = raw(22);
    used.border_top = raw(23);
    used.border_bottom = raw(24);
    used.padding_left = raw(31);
    used.padding_right = raw(32);
    used.padding_top = raw(33);
    used.padding_bottom = raw(34);
    used.inset_left = raw(41);
    used.inset_right = raw(42);
    used.inset_top = raw(43);
    used.inset_bottom = raw(44);

    assert_eq!(used.margin_left, raw(11));
    assert_eq!(used.margin_right, raw(12));
    assert_eq!(used.margin_top, raw(13));
    assert_eq!(used.margin_bottom, raw(14));
    assert_eq!(used.border_left, raw(21));
    assert_eq!(used.border_right, raw(22));
    assert_eq!(used.border_top, raw(23));
    assert_eq!(used.border_bottom, raw(24));
    assert_eq!(used.padding_left, raw(31));
    assert_eq!(used.padding_right, raw(32));
    assert_eq!(used.padding_top, raw(33));
    assert_eq!(used.padding_bottom, raw(34));
    assert_eq!(used.inset_left, raw(41));
    assert_eq!(used.inset_right, raw(42));
    assert_eq!(used.inset_top, raw(43));
    assert_eq!(used.inset_bottom, raw(44));

    used.has_definite_inline_size = true;
    used.has_definite_block_size = true;
    used.inline_size_constraint = FfiSizeConstraint::None;
    used.block_size_constraint = FfiSizeConstraint::MaxContent;
    assert!(used.has_definite_inline_size());
    assert!(!used.has_definite_block_size());

    used.materialized_from_paintable = true;
    used.has_content_offset = true;
    used.content_offset = FfiCssPixelPoint {
        x: raw(-51),
        y: raw(52),
    };
    assert!(used.materialized_from_paintable);
    assert!(used.has_content_offset);
    assert_eq!(used.content_offset.x, raw(-51));
    assert_eq!(used.content_offset.y, raw(52));

    used.has_first_baseline = true;
    used.first_baseline = raw(61);
    used.has_last_baseline = true;
    used.last_baseline = raw(62);
    assert_eq!(used.first_baseline, raw(61));
    assert_eq!(used.last_baseline, raw(62));
    used.has_first_baseline = false;
    used.has_last_baseline = false;
    assert!(!used.has_first_baseline);
    assert!(!used.has_last_baseline);

    used.has_containing_line_box_fragment = true;
    used.containing_line_box_fragment = FfiLineBoxFragmentCoordinate {
        line_box_index: 71,
        fragment_index: 72,
    };
    assert_eq!(used.containing_line_box_fragment.line_box_index, 71);
    assert_eq!(used.containing_line_box_fragment.fragment_index, 72);
    used.has_containing_line_box_fragment = false;
    assert!(!used.has_containing_line_box_fragment);
}

#[test]
fn collapsed_border_rounding_and_inner_available_space_are_preserved() {
    let mut used = UsedValuesCore {
        margin_left: px(1),
        border_left: px(5),
        padding_left: px(2),
        ..UsedValuesCore::default()
    };
    assert_eq!(
        used.margin_left + used.border_left_collapsed(false) + used.padding_left,
        px(8)
    );
    assert_eq!(
        used.margin_left + used.border_left_collapsed(true) + used.padding_left,
        px(6)
    );

    used.set_content_inline_size(px(120));
    used.set_content_block_size(px(80));
    used.has_definite_block_size = true;
    let definite_inner = used.available_inner_space_or_constraints_from(AvailableSpace {
        inline_size: AvailableSize::max_content(),
        block_size: AvailableSize::min_content(),
    });
    assert_eq!(definite_inner.inline_size, AvailableSize::definite(px(120)));
    assert_eq!(definite_inner.block_size, AvailableSize::definite(px(80)));

    used.has_definite_inline_size = false;
    used.has_definite_block_size = false;
    let constrained_inner = used.available_inner_space_or_constraints_from(AvailableSpace {
        inline_size: AvailableSize::max_content(),
        block_size: AvailableSize::min_content(),
    });
    assert!(constrained_inner.inline_size.is_max_content());
    assert!(constrained_inner.block_size.is_min_content());

    used.has_definite_inline_size = true;
    used.has_definite_block_size = true;
    used.inline_size_constraint = FfiSizeConstraint::MinContent;
    used.block_size_constraint = FfiSizeConstraint::MaxContent;
    let explicit_constraints = used.available_inner_space_or_constraints_from(AvailableSpace {
        inline_size: AvailableSize::definite(px(1)),
        block_size: AvailableSize::definite(px(1)),
    });
    assert!(explicit_constraints.inline_size.is_min_content());
    assert!(explicit_constraints.block_size.is_max_content());
}
