/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/Layout/AvailableSpace.h>
#include <LibWeb/Layout/LayoutState.h>

namespace Web::Layout {

TEST_CASE(used_values_core_fields_round_trip_through_cpp_accessors)
{
    auto* rust_state = RustFFI::rust_layout_state_create();
    VERIFY(rust_state);
    RustFFI::rust_layout_state_ensure_capacity(rust_state, 65);
    auto* core = RustFFI::rust_layout_state_create_used_values(rust_state, 64);
    VERIFY(core);
    EXPECT_EQ(RustFFI::rust_layout_state_get_used_values(rust_state, 64), core);
    EXPECT_EQ(RustFFI::rust_layout_state_get_used_values(rust_state, 63), nullptr);

    {
        LayoutState::UsedValues used_values { *core };

        core->node = core;
        EXPECT_EQ(core->node, core);

        used_values.set_content_inline_size(CSSPixels::from_raw(101));
        used_values.set_content_block_size(CSSPixels::from_raw(202));
        EXPECT_EQ(used_values.content_inline_size().raw_value(), 101);
        EXPECT_EQ(used_values.content_block_size().raw_value(), 202);

        used_values.set_margin_left(CSSPixels::from_raw(11));
        used_values.set_margin_right(CSSPixels::from_raw(12));
        used_values.set_margin_top(CSSPixels::from_raw(13));
        used_values.set_margin_bottom(CSSPixels::from_raw(14));
        used_values.set_border_left(CSSPixels::from_raw(21));
        used_values.set_border_right(CSSPixels::from_raw(22));
        used_values.set_border_top(CSSPixels::from_raw(23));
        used_values.set_border_bottom(CSSPixels::from_raw(24));
        used_values.set_padding_left(CSSPixels::from_raw(31));
        used_values.set_padding_right(CSSPixels::from_raw(32));
        used_values.set_padding_top(CSSPixels::from_raw(33));
        used_values.set_padding_bottom(CSSPixels::from_raw(34));
        used_values.set_inset_left(CSSPixels::from_raw(41));
        used_values.set_inset_right(CSSPixels::from_raw(42));
        used_values.set_inset_top(CSSPixels::from_raw(43));
        used_values.set_inset_bottom(CSSPixels::from_raw(44));

        EXPECT_EQ(core->margin_left, 11);
        EXPECT_EQ(core->margin_right, 12);
        EXPECT_EQ(core->margin_top, 13);
        EXPECT_EQ(core->margin_bottom, 14);
        EXPECT_EQ(core->border_left, 21);
        EXPECT_EQ(core->border_right, 22);
        EXPECT_EQ(core->border_top, 23);
        EXPECT_EQ(core->border_bottom, 24);
        EXPECT_EQ(core->padding_left, 31);
        EXPECT_EQ(core->padding_right, 32);
        EXPECT_EQ(core->padding_top, 33);
        EXPECT_EQ(core->padding_bottom, 34);
        EXPECT_EQ(core->inset_left, 41);
        EXPECT_EQ(core->inset_right, 42);
        EXPECT_EQ(core->inset_top, 43);
        EXPECT_EQ(core->inset_bottom, 44);

        used_values.set_has_definite_inline_size(true);
        used_values.set_has_definite_block_size(true);
        used_values.set_inline_size_constraint(SizeConstraint::None);
        used_values.set_block_size_constraint(SizeConstraint::MaxContent);
        EXPECT(core->has_definite_inline_size);
        EXPECT(core->has_definite_block_size);
        EXPECT(used_values.has_definite_inline_size());
        EXPECT(!used_values.has_definite_block_size());
        EXPECT_EQ(core->inline_size_constraint, RustFFI::FfiSizeConstraint::None);
        EXPECT_EQ(core->block_size_constraint, RustFFI::FfiSizeConstraint::MaxContent);

        core->materialized_from_paintable = true;
        EXPECT(used_values.is_materialized_from_paintable());

        core->has_content_offset = true;
        core->content_offset = { .x = -51, .y = 52 };
        EXPECT(used_values.is_placed());
        EXPECT_EQ(used_values.content_offset().x().raw_value(), -51);
        EXPECT_EQ(used_values.content_offset().y().raw_value(), 52);

        used_values.set_first_baseline(CSSPixels::from_raw(61));
        used_values.set_last_baseline(CSSPixels::from_raw(62));
        EXPECT(core->has_first_baseline);
        EXPECT(core->has_last_baseline);
        EXPECT_EQ(used_values.first_baseline()->raw_value(), 61);
        EXPECT_EQ(used_values.last_baseline()->raw_value(), 62);
        used_values.set_first_baseline({});
        used_values.set_last_baseline({});
        EXPECT(!core->has_first_baseline);
        EXPECT(!core->has_last_baseline);

        used_values.set_containing_line_box_fragment(LineBoxFragmentCoordinate {
            .line_box_index = 71,
            .fragment_index = 72,
        });
        EXPECT(core->has_containing_line_box_fragment);
        EXPECT_EQ(core->containing_line_box_fragment.line_box_index, 71u);
        EXPECT_EQ(core->containing_line_box_fragment.fragment_index, 72u);
        auto coordinate = used_values.containing_line_box_fragment();
        EXPECT_EQ(coordinate->line_box_index, 71u);
        EXPECT_EQ(coordinate->fragment_index, 72u);
        used_values.set_containing_line_box_fragment({});
        EXPECT(!core->has_containing_line_box_fragment);
    }

    RustFFI::rust_layout_state_destroy(rust_state);
}

TEST_CASE(collapsed_border_rounding_and_inner_available_space_are_preserved)
{
    auto* rust_state = RustFFI::rust_layout_state_create();
    VERIFY(rust_state);
    auto* core = RustFFI::rust_layout_state_create_used_values(rust_state, 0);
    VERIFY(core);

    {
        LayoutState::UsedValues used_values { *core };
        used_values.set_margin_left(1);
        used_values.set_border_left(5);
        used_values.set_padding_left(2);
        EXPECT_EQ(used_values.margin_box_left(), CSSPixels(8));

        Painting::Paintable::BorderDataWithElementKind border {
            .border_data = {},
            .element_kind = Painting::Paintable::ConflictingElementKind::Cell,
        };
        used_values.set_override_borders_data({
            .top = border,
            .right = border,
            .bottom = border,
            .left = border,
        });
        EXPECT_EQ(used_values.margin_box_left(), CSSPixels(6));

        used_values.set_content_inline_size(120);
        used_values.set_content_block_size(80);
        used_values.set_has_definite_block_size(true);
        used_values.set_inline_size_constraint(SizeConstraint::None);
        used_values.set_block_size_constraint(SizeConstraint::None);
        auto definite_inner = used_values.available_inner_space_or_constraints_from({
            AvailableSize::make_max_content(),
            AvailableSize::make_min_content(),
        });
        EXPECT_EQ(definite_inner.inline_size, AvailableSize::make_definite(120));
        EXPECT_EQ(definite_inner.block_size, AvailableSize::make_definite(80));

        used_values.set_indefinite_content_inline_size();
        used_values.set_indefinite_content_block_size();
        auto constrained_inner = used_values.available_inner_space_or_constraints_from({
            AvailableSize::make_max_content(),
            AvailableSize::make_min_content(),
        });
        EXPECT(constrained_inner.inline_size.is_max_content());
        EXPECT(constrained_inner.block_size.is_min_content());

        used_values.set_has_definite_inline_size(true);
        used_values.set_has_definite_block_size(true);
        used_values.set_inline_size_constraint(SizeConstraint::MinContent);
        used_values.set_block_size_constraint(SizeConstraint::MaxContent);
        auto explicit_constraints = used_values.available_inner_space_or_constraints_from({
            AvailableSize::make_definite(1),
            AvailableSize::make_definite(1),
        });
        EXPECT(explicit_constraints.inline_size.is_min_content());
        EXPECT(explicit_constraints.block_size.is_max_content());
    }

    RustFFI::rust_layout_state_destroy(rust_state);
}

}
