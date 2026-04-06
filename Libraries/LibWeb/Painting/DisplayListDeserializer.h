/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/ImmutableBitmap.h>
#include <LibWeb/CSS/StyleValues/ColorInterpolationMethodStyleValue.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

class WEB_API DisplayListDeserializer {
public:
    struct ResourceRegistries {
        HashMap<u64, NonnullRefPtr<Gfx::Font>> const& fonts;
        HashMap<u64, NonnullRefPtr<Gfx::ImmutableBitmap>> const& images;
    };

    struct Result {
        NonnullRefPtr<DisplayList> display_list;
        ScrollStateSnapshotByDisplayList scroll_states;
    };

    static ErrorOr<Result> deserialize(
        ReadonlyBytes buffer,
        ResourceRegistries const&);

private:
    DisplayListDeserializer(ReadonlyBytes buffer, ResourceRegistries const&);

    ErrorOr<NonnullRefPtr<DisplayList>> do_deserialize();
    ErrorOr<NonnullRefPtr<AccumulatedVisualContextTree>> deserialize_visual_context_tree();
    ErrorOr<ScrollStateSnapshot> deserialize_scroll_state();
    ErrorOr<void> deserialize_nested_display_lists();
    ErrorOr<DisplayListCommand> deserialize_command(u8 type_index);

    template<typename T>
    ErrorOr<T> read();
    ErrorOr<u8> read_u8();
    ErrorOr<u32> read_u32();
    ErrorOr<u64> read_u64();
    ErrorOr<i32> read_i32();
    ErrorOr<float> read_float();
    ErrorOr<bool> read_bool();
    ErrorOr<Gfx::Color> read_color();
    ErrorOr<Gfx::IntRect> read_int_rect();
    ErrorOr<Gfx::FloatPoint> read_float_point();
    ErrorOr<Gfx::IntPoint> read_int_point();
    ErrorOr<Gfx::IntSize> read_int_size();
    ErrorOr<CornerRadii> read_corner_radii();
    ErrorOr<GradientPaintData> read_gradient_paint_data();
    ErrorOr<PaintStyleOrColor> read_paint_style_or_color();
    ErrorOr<CSS::ColorInterpolationMethodStyleValue::ColorInterpolationMethod> read_interpolation_method();

    ReadonlyBytes m_buffer;
    size_t m_offset { 0 };
    ResourceRegistries const& m_registries;
    ScrollStateSnapshot m_scroll_state;
    Vector<RefPtr<DisplayList>> m_nested_display_lists;
    HashMap<RefPtr<DisplayList>, ScrollStateSnapshot> m_nested_scroll_states;
};

}
