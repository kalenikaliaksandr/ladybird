/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Painting/PaintableBox.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

ScrollStateSnapshot ScrollStateSnapshot::create(Vector<NonnullRefPtr<ScrollFrame>> const& scroll_frames)
{
    ScrollStateSnapshot snapshot;
    snapshot.m_frames.ensure_capacity(scroll_frames.size());

    for (auto const& scroll_frame : scroll_frames) {
        ScrollFrameData data;

        // Current offset
        data.own_offset = scroll_frame->own_offset();

        // Compute max offset from scrollable overflow and padding box
        auto const& paintable_box = scroll_frame->paintable_box();
        auto scrollable_overflow_rect = paintable_box.scrollable_overflow_rect();
        if (scrollable_overflow_rect.has_value()) {
            auto padding_rect = paintable_box.absolute_padding_box_rect();
            data.max_offset = CSSPixelPoint {
                max(scrollable_overflow_rect->width() - padding_rect.width(), CSSPixels(0)),
                max(scrollable_overflow_rect->height() - padding_rect.height(), CSSPixels(0))
            };
        }

        // Find scrollable parent (skip sticky frames for bubbling)
        for (auto parent = scroll_frame->parent(); parent; parent = parent->parent()) {
            if (!parent->is_sticky()) {
                data.scrollable_parent_id = parent->id();
                break;
            }
        }

        snapshot.m_frames.append(data);
    }

    return snapshot;
}

}
