/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/Layout/LayoutRustBridge.h>

namespace Web::Layout {

static void expect_empty_payload(RustFFI::FfiSizeValue const& value, RustFFI::FfiSizeKind kind)
{
    EXPECT_EQ(value.kind, to_underlying(kind));
    EXPECT_EQ(value.px, 0);
    EXPECT_EQ(value.fraction, 0);
    EXPECT_EQ(value.calc, nullptr);
}

TEST_CASE(size_value_kinds_and_payloads_match_cpp)
{
    expect_empty_payload(ladybird_layout_test_build_auto_margin_value(), RustFFI::FfiSizeKind::Auto);

    auto px = ladybird_layout_test_build_size_value(to_underlying(RustFFI::FfiSizeKind::Px), -65, 0);
    EXPECT_EQ(px.kind, to_underlying(RustFFI::FfiSizeKind::Px));
    EXPECT_EQ(px.px, -65);
    EXPECT_EQ(px.fraction, 0);
    EXPECT_EQ(px.calc, nullptr);

    auto percentage = ladybird_layout_test_build_size_value(to_underlying(RustFFI::FfiSizeKind::Percentage), 0, -0.25);
    EXPECT_EQ(percentage.kind, to_underlying(RustFFI::FfiSizeKind::Percentage));
    EXPECT_EQ(percentage.px, 0);
    EXPECT_EQ(percentage.fraction, -0.25);
    EXPECT_EQ(percentage.calc, nullptr);

    for (auto kind : {
             RustFFI::FfiSizeKind::Auto,
             RustFFI::FfiSizeKind::MinContent,
             RustFFI::FfiSizeKind::MaxContent,
             RustFFI::FfiSizeKind::FitContent,
             RustFFI::FfiSizeKind::None_,
         })
        expect_empty_payload(ladybird_layout_test_build_size_value(to_underlying(kind), 0, 0), kind);
}

TEST_CASE(vertical_align_keywords_and_lengths_are_distinct)
{
    auto keyword = ladybird_layout_test_build_vertical_align(true, to_underlying(CSS::VerticalAlign::TextTop), 0);
    EXPECT(keyword.is_keyword);
    EXPECT_EQ(keyword.keyword, to_underlying(CSS::VerticalAlign::TextTop));
    expect_empty_payload(keyword.value, RustFFI::FfiSizeKind::Auto);

    auto length = ladybird_layout_test_build_vertical_align(false, 0, CSSPixels(-3.5).raw_value());
    EXPECT(!length.is_keyword);
    EXPECT_EQ(length.keyword, 0);
    EXPECT_EQ(length.value.kind, to_underlying(RustFFI::FfiSizeKind::Px));
    EXPECT_EQ(length.value.px, CSSPixels(-3.5).raw_value());
}

TEST_CASE(calculated_sizes_resolve_through_the_css_rust_export)
{
    auto facts = ladybird_layout_test_build_calc_size_value();
    EXPECT_EQ(facts.kind, to_underlying(RustFFI::FfiSizeKind::Calc));
    EXPECT_NE(facts.calc, nullptr);

    for (auto basis : { CSSPixels(-40), CSSPixels(0), CSSPixels(80), CSSPixels::from_raw(65) }) {
        auto expected = CSSPixels::from_raw(ladybird_layout_test_resolve_calc_handle_cpp(facts.calc, basis.raw_value()));
        auto actual = CSSPixels::from_raw(RustFFI::rust_layout_resolve_calc_handle_for_parity(facts.calc, basis.raw_value()));
        EXPECT_EQ(actual, expected);
    }

    ladybird_layout_release_calc_handle(facts.calc);
    ladybird_layout_test_verify_calc_handles_balanced();
}

}
