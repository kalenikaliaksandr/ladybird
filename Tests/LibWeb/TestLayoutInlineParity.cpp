/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/BitCast.h>
#include <LibTest/TestCase.h>
#include <LibWeb/Layout/LayoutRustFFI.h>
#include <LibWeb/PixelUnits.h>

namespace Web {

TEST_CASE(inline_rust_f32_css_pixel_conversion_matches_the_cpp_float_instantiation)
{
    Array<float, 13> edge_cases {
        0.0f,
        -0.0f,
        0.0078125f,
        0.0234375f,
        -0.0078125f,
        -0.0234375f,
        0.1f,
        -0.1f,
        206.66646f,
        33'554'430.0f,
        -33'554'430.0f,
        AK::Infinity<float>,
        -AK::Infinity<float>,
    };
    for (auto value : edge_cases) {
        EXPECT_EQ(
            Layout::RustFFI::rust_layout_css_pixels_nearest_value_for_f32(value),
            CSSPixels::nearest_value_for(value).raw_value());
    }

    // Exercise normal, subnormal, and sign-bit combinations without relying
    // on a decimal conversion before the C++ overload is selected.
    for (u32 bits = 1; bits < 0x7f80'0000; bits += 65'537) {
        auto value = bit_cast<float>(bits);
        EXPECT_EQ(
            Layout::RustFFI::rust_layout_css_pixels_nearest_value_for_f32(value),
            CSSPixels::nearest_value_for(value).raw_value());
        value = -value;
        EXPECT_EQ(
            Layout::RustFFI::rust_layout_css_pixels_nearest_value_for_f32(value),
            CSSPixels::nearest_value_for(value).raw_value());
    }
}

TEST_CASE(inline_rust_css_pixel_floor_and_ceil_match_cpp)
{
    for (i32 raw = -1'000'000; raw <= 1'000'000; raw += 97) {
        auto value = CSSPixels::from_raw(raw);
        EXPECT_EQ(Layout::RustFFI::rust_layout_css_pixels_floor(raw), floor(value).raw_value());
        EXPECT_EQ(Layout::RustFFI::rust_layout_css_pixels_ceil(raw), ceil(value).raw_value());
    }
}

}

