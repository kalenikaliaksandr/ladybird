/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/GPUResourceRegistry.h>

namespace Web::Painting {

class DisplayListSerializer {
public:
    static ErrorOr<Core::AnonymousBuffer> serialize(
        DisplayList const&,
        ScrollStateSnapshotByDisplayList const&,
        GPUResourceRegistry&);

private:
    explicit DisplayListSerializer(GPUResourceRegistry&);

    ErrorOr<void> serialize_visual_context_tree(AccumulatedVisualContextTree const&);
    ErrorOr<void> serialize_commands(DisplayList const&);
    ErrorOr<void> serialize_scroll_state(ScrollStateSnapshotByDisplayList const&, DisplayList const&);
    ErrorOr<void> serialize_nested_display_lists(ScrollStateSnapshotByDisplayList const&);

    ErrorOr<void> serialize_command(DisplayListCommand const&);
    u32 register_nested_display_list(RefPtr<DisplayList> const&);

    template<typename T>
    ErrorOr<void> write(T const& value);
    ErrorOr<void> write_bytes(ReadonlyBytes);
    ErrorOr<void> write_u8(u8);
    ErrorOr<void> write_u32(u32);
    ErrorOr<void> write_u64(u64);
    ErrorOr<void> write_i32(i32);
    ErrorOr<void> write_float(float);
    ErrorOr<void> write_bool(bool);
    ErrorOr<void> write_color(Gfx::Color);
    ErrorOr<void> write_int_rect(Gfx::IntRect);
    ErrorOr<void> write_float_point(Gfx::FloatPoint);
    ErrorOr<void> write_int_point(Gfx::IntPoint);
    ErrorOr<void> write_int_size(Gfx::IntSize);
    ErrorOr<void> write_corner_radii(CornerRadii const&);
    ErrorOr<void> write_paint_style_or_color(PaintStyleOrColor const&);
    ErrorOr<void> write_gradient_paint_data(GradientPaintData const&);

    GPUResourceRegistry& m_registry;
    ByteBuffer m_buffer;
    Vector<RefPtr<DisplayList>> m_nested_display_lists;
    HashMap<DisplayList const*, u32> m_nested_display_list_indices;
};

}
