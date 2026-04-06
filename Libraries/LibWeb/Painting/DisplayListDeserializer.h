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
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Painting {

class DisplayListDeserializer {
public:
    struct ResourceRegistries {
        HashMap<u64, NonnullRefPtr<Gfx::Font>> const& fonts;
        HashMap<u64, NonnullRefPtr<Gfx::ImmutableBitmap>> const& images;
    };

    static ErrorOr<NonnullRefPtr<DisplayList>> deserialize(
        ReadonlyBytes buffer,
        ResourceRegistries const&);

private:
    DisplayListDeserializer(ReadonlyBytes buffer, ResourceRegistries const&);

    ErrorOr<NonnullRefPtr<DisplayList>> do_deserialize();
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

    ReadonlyBytes m_buffer;
    size_t m_offset { 0 };
    ResourceRegistries const& m_registries;
};

}
