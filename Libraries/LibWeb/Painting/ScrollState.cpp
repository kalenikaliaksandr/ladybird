/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Vector.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

void precompute_sticky_constraints(ScrollState& scroll_state, ScrollFrameIndex sticky_frame_index, Paintable const& paintable_box)
{
    auto nearest_scrolling_ancestor_index = scroll_state.nearest_scrolling_ancestor(sticky_frame_index);
    if (!nearest_scrolling_ancestor_index.value())
        return;

    auto const& scroll_ancestor_paintable = scroll_state.frame_at(nearest_scrolling_ancestor_index).paintable_box();
    RefPtr<Paintable const> scroll_ancestor_paintable_ref = scroll_ancestor_paintable;
    auto sticky_border_box_rect = paintable_box.absolute_border_box_rect();
    RefPtr<Paintable const> containing_block_of_sticky = paintable_box.containing_block();

    CSSPixelRect containing_block_region;
    bool needs_parent_offset_adjustment = false;
    if (containing_block_of_sticky == scroll_ancestor_paintable_ref) {
        containing_block_region = { {}, containing_block_of_sticky->scrollable_overflow_rect()->size() };
    } else {
        containing_block_region = containing_block_of_sticky->absolute_border_box_rect()
                                      .translated(-scroll_ancestor_paintable.absolute_rect().top_left());
        needs_parent_offset_adjustment = true;
    }

    scroll_state.frame_at(sticky_frame_index).set_sticky_constraints({
        .position_relative_to_scroll_ancestor = sticky_border_box_rect.top_left() - scroll_ancestor_paintable.absolute_rect().top_left(),
        .border_box_size = sticky_border_box_rect.size(),
        .scrollport_size = scroll_ancestor_paintable.absolute_rect().size(),
        .containing_block_region = containing_block_region,
        .needs_parent_offset_adjustment = needs_parent_offset_adjustment,
        .insets = paintable_box.sticky_insets(),
    });
}

ScrollStateSnapshot ScrollStateSnapshot::create(Vector<ScrollFrame> const& scroll_frames, double device_pixels_per_css_pixel)
{
    ScrollStateSnapshot snapshot;
    auto scale = static_cast<float>(device_pixels_per_css_pixel);
    snapshot.m_device_offsets.ensure_capacity(scroll_frames.size());
    for (auto const& scroll_frame : scroll_frames) {
        auto const& offset = scroll_frame.own_offset();
        snapshot.m_device_offsets.unchecked_append(offset.to_type<float>() * scale);
    }
    return snapshot;
}

ScrollStateSnapshot ScrollStateSnapshot::create_from_device_offsets(Vector<Gfx::FloatPoint>&& device_offsets)
{
    ScrollStateSnapshot snapshot;
    snapshot.m_device_offsets = move(device_offsets);
    return snapshot;
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollStateSnapshot const& snapshot)
{
    TRY(encoder.encode(snapshot.device_offsets()));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollStateSnapshot> decode(Decoder& decoder)
{
    return Web::Painting::ScrollStateSnapshot::create_from_device_offsets(TRY(decoder.decode<Vector<Gfx::FloatPoint>>()));
}

}
