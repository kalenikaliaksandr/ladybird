/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibGfx/Rect.h>

namespace Web::HTML {

// Chrome's shipped thresholds, matching ForceDarkSettings::default() on the Rust side; tests vary them through
// internals to isolate one role from the others.
inline constexpr i32 default_force_dark_foreground_threshold = 150;
inline constexpr i32 default_force_dark_background_threshold = 205;

struct PaintConfig {
    bool paint_overlay { false };
    bool should_show_line_box_borders { false };
    bool should_show_caret_hit_test_debug_overlay { false };
    bool force_dark_enabled { false };
    i32 force_dark_foreground_threshold { default_force_dark_foreground_threshold };
    i32 force_dark_background_threshold { default_force_dark_background_threshold };
    Optional<Gfx::IntRect> canvas_fill_rect {};
    // Whether the frame records the whole document even where frames record around the scroll positions, as a
    // screenshot that shows more than the viewport does.
    bool records_whole_document { false };

    bool operator==(PaintConfig const& other) const = default;

    // Whether a display list recorded with `other` paints what one recorded with this config paints: how much of the
    // document a list records leaves what it paints there alone.
    bool paints_as(PaintConfig const& other) const
    {
        auto other_recording_this_much = other;
        other_recording_this_much.records_whole_document = records_whole_document;
        return *this == other_recording_this_much;
    }
};

}
