/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/DOM/Element.h>
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

        // Store stable ID for sync back to main thread
        data.stable_id = scroll_frame->stable_id();

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

        // Get generation from the DOM element (which persists across relayouts)
        if (auto dom_node = paintable_box.dom_node()) {
            if (auto const* element = as_if<DOM::Element>(*dom_node)) {
                auto pseudo = paintable_box.layout_node().generated_for_pseudo_element();
                data.generation = element->scroll_generation(pseudo);
            }
        }

        snapshot.m_frames.append(data);
    }

    return snapshot;
}

}
