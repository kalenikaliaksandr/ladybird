/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/BitCast.h>
#include <AK/NumericLimits.h>
#include <LibTest/TestCase.h>
#include <LibWeb/Layout/AvailableSpace.h>
#include <LibWeb/Layout/LayoutRustFFI.h>

namespace Web::Layout {

struct AvailableSizeCase {
    AvailableSize cpp;
    RustFFI::AvailableSize rust;
};

static AvailableSizeCase definite_case(i32 raw)
{
    struct AvailableSizeRepresentation {
        AvailableSize::Type type;
        CSSPixels value;
    };
    static_assert(sizeof(AvailableSizeRepresentation) == sizeof(AvailableSize));
    static_assert(alignof(AvailableSizeRepresentation) == alignof(AvailableSize));

    auto cpp = CSSPixels::from_raw(raw).might_be_saturated()
        // The public factory rejects saturated definite values, but the
        // comparison operators can still be exhaustively parity-tested by
        // constructing their exact trivially-copyable representation.
        ? bit_cast<AvailableSize>(AvailableSizeRepresentation { AvailableSize::Type::Definite, CSSPixels::from_raw(raw) })
        : AvailableSize::make_definite(CSSPixels::from_raw(raw));
    return {
        .cpp = cpp,
        .rust = {
            .type_ = RustFFI::AvailableSizeType::Definite,
            .value = raw,
        },
    };
}

static Vector<AvailableSizeCase> available_size_cases()
{
    Vector<AvailableSizeCase> cases;
    for (auto raw : { NumericLimits<i32>::min(), NumericLimits<i32>::min() + 1, -4097, -64, -1, 0, 1, 64, 4097, NumericLimits<i32>::max() - 1, NumericLimits<i32>::max() })
        cases.append(definite_case(raw));
    cases.append({
        .cpp = AvailableSize::make_indefinite(),
        .rust = {
            .type_ = RustFFI::AvailableSizeType::Indefinite,
            .value = NumericLimits<i32>::max(),
        },
    });
    cases.append({
        .cpp = AvailableSize::make_min_content(),
        .rust = {
            .type_ = RustFFI::AvailableSizeType::MinContent,
            .value = 0,
        },
    });
    cases.append({
        .cpp = AvailableSize::make_max_content(),
        .rust = {
            .type_ = RustFFI::AvailableSizeType::MaxContent,
            .value = NumericLimits<i32>::max(),
        },
    });
    return cases;
}

TEST_CASE(available_size_comparisons_match_rust)
{
    auto cases = available_size_cases();
    for (auto const& left : cases) {
        for (auto const& right : cases) {
            EXPECT_EQ(left.cpp == right.cpp, RustFFI::rust_layout_available_size_equals(left.rust, right.rust));
            EXPECT_EQ(left.cpp < right.cpp, RustFFI::rust_layout_available_size_less_than(left.rust, right.rust));
        }
    }
}

TEST_CASE(css_pixel_and_available_size_comparisons_match_rust)
{
    auto cases = available_size_cases();
    for (auto raw : { NumericLimits<i32>::min(), NumericLimits<i32>::min() + 1, -65, -1, 0, 1, 65, NumericLimits<i32>::max() - 1, NumericLimits<i32>::max() }) {
        auto pixels = CSSPixels::from_raw(raw);
        for (auto const& size : cases) {
            EXPECT_EQ(pixels > size.cpp, RustFFI::rust_layout_css_pixels_greater_than_available_size(raw, size.rust));
            EXPECT_EQ(pixels < size.cpp, RustFFI::rust_layout_css_pixels_less_than_available_size(raw, size.rust));
            EXPECT_EQ(size.cpp < pixels, RustFFI::rust_layout_available_size_less_than_css_pixels(size.rust, raw));
        }
    }
}

TEST_CASE(available_space_equality_matches_rust)
{
    auto cases = available_size_cases();
    for (auto const& inline_size : cases) {
        for (auto const& block_size : cases) {
            AvailableSpace cpp { inline_size.cpp, block_size.cpp };
            RustFFI::AvailableSpace rust {
                .inline_size = inline_size.rust,
                .block_size = block_size.rust,
            };
            EXPECT(RustFFI::rust_layout_available_space_equals(rust, rust));

            AvailableSpace swapped_cpp { block_size.cpp, inline_size.cpp };
            RustFFI::AvailableSpace swapped_rust {
                .inline_size = block_size.rust,
                .block_size = inline_size.rust,
            };
            EXPECT_EQ(cpp == swapped_cpp, RustFFI::rust_layout_available_space_equals(rust, swapped_rust));
        }
    }
}

}
