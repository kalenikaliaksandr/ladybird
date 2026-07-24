/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/CSS/GridTrackSize.h>
#include <LibWeb/Layout/LayoutRustBridge.h>

namespace Web::Layout {

static u64 ffi_counter(StringView name)
{
    for (size_t index = 0; index < RustFFI::rust_layout_ffi_counter_count(); ++index) {
        auto const* characters = reinterpret_cast<char const*>(RustFFI::rust_layout_ffi_counter_name(index));
        auto counter_name = StringView { characters, __builtin_strlen(characters) };
        if (counter_name == name)
            return RustFFI::rust_layout_ffi_counter_value(index);
    }
    VERIFY_NOT_REACHED();
}

TEST_CASE(grid_snapshot_preserves_nested_tracks_names_areas_and_placements)
{
    RustFFI::rust_layout_ffi_counters_reset();
    auto facts = ladybird_layout_test_build_grid_facts_snapshot();
    EXPECT_EQ(ffi_counter("gridNameRetainEntries"sv), 4u);
    EXPECT_EQ(ffi_counter("gridNameReleaseEntries"sv), 0u);

    constexpr auto no_index = static_cast<u32>(-1);
    EXPECT_EQ(facts.name_count, 4u);
    EXPECT_EQ(facts.entry_count, 6u);
    EXPECT_EQ(facts.template_columns.first_entry, 0u);
    EXPECT_EQ(facts.template_rows.first_entry, no_index);

    auto const& names = facts.entries[0];
    EXPECT_EQ(names.kind, to_underlying(RustFFI::FfiGridTrackEntryKind::LineNames));
    EXPECT_EQ(names.name_index_count, 1u);
    EXPECT_EQ(facts.name_indices[names.name_index_start], 0u);
    EXPECT_EQ(names.next_sibling, 1u);

    auto const& fixed = facts.entries[1];
    EXPECT_EQ(fixed.kind, to_underlying(RustFFI::FfiGridTrackEntryKind::TrackSize));
    EXPECT_EQ(fixed.size.kind, to_underlying(RustFFI::FfiGridTrackBreadthKind::LengthPercentage));
    EXPECT_EQ(fixed.size.value.kind, to_underlying(RustFFI::FfiSizeKind::Px));
    EXPECT_EQ(fixed.size.value.px, CSSPixels(10).raw_value());

    auto const& minmax = facts.entries[2];
    EXPECT_EQ(minmax.kind, to_underlying(RustFFI::FfiGridTrackEntryKind::MinMax));
    EXPECT_EQ(minmax.min_size.value.kind, to_underlying(RustFFI::FfiSizeKind::Percentage));
    EXPECT_EQ(minmax.min_size.value.fraction, 0.25);
    EXPECT_EQ(minmax.max_size.kind, to_underlying(RustFFI::FfiGridTrackBreadthKind::Flex));
    EXPECT_EQ(minmax.max_size.flex_factor, 2);

    auto const& repeat = facts.entries[3];
    EXPECT_EQ(repeat.kind, to_underlying(RustFFI::FfiGridTrackEntryKind::Repeat));
    EXPECT_EQ(repeat.repeat_type, to_underlying(CSS::GridRepeatType::Fixed));
    EXPECT_EQ(repeat.repeat_count, 3u);
    EXPECT_EQ(repeat.next_sibling, no_index);
    EXPECT_EQ(repeat.repeat_list.first_entry, 4u);
    EXPECT_EQ(facts.entries[4].next_sibling, 5u);
    EXPECT_EQ(facts.entries[5].next_sibling, no_index);
    EXPECT_EQ(facts.entries[5].size.kind, to_underlying(RustFFI::FfiGridTrackBreadthKind::FitContent));
    EXPECT_EQ(facts.entries[5].size.value.kind, to_underlying(RustFFI::FfiSizeKind::FitContent));
    EXPECT_EQ(facts.entries[5].size.value.px, CSSPixels(40).raw_value());

    EXPECT_EQ(facts.area_count, 1u);
    EXPECT_EQ(facts.areas[0].name_index, 0u);
    EXPECT_EQ(facts.areas[0].implicit_start_name_index, 2u);
    EXPECT_EQ(facts.areas[0].implicit_end_name_index, 3u);
    EXPECT_EQ(facts.areas[0].row_start, 1u);
    EXPECT_EQ(facts.areas[0].column_end, 4u);

    EXPECT_EQ(facts.column_start.kind, to_underlying(RustFFI::FfiGridPlacementKind::Line));
    EXPECT(facts.column_start.has_line_number);
    EXPECT_EQ(facts.column_start.line_number, -1);
    EXPECT(facts.column_start.has_name);
    EXPECT_EQ(facts.column_start.name_index, 0u);
    EXPECT_EQ(facts.column_start.implicit_start_name_index, 2u);
    EXPECT_EQ(facts.column_start.implicit_end_name_index, 3u);

    EXPECT_EQ(facts.column_end.kind, to_underlying(RustFFI::FfiGridPlacementKind::Span));
    EXPECT_EQ(facts.column_end.line_number, 2);
    EXPECT_EQ(facts.column_end.name_index, 1u);

    ladybird_layout_test_release_grid_facts_snapshot(&facts);
    EXPECT_EQ(ffi_counter("gridNameRetainEntries"sv), 4u);
    EXPECT_EQ(ffi_counter("gridNameReleaseEntries"sv), 4u);
}

}
