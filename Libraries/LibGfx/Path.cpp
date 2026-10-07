/*
 * Copyright (c) 2024, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Path.h>
#include <LibGfx/TextLayout.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <RustFFI.h>

namespace Gfx {

static_assert(sizeof(FFI::FloatPoint) == sizeof(FloatPoint));
static_assert(sizeof(FFI::PathVerb) == sizeof(u8));
static_assert(to_underlying(WindingRule::Nonzero) == 0);
static_assert(to_underlying(WindingRule::EvenOdd) == 1);

Path::Path()
    : m_path(FFI::ladybird_gfx_path_create())
{
}

Path::Path(FFI::RustPath* path)
    : m_path(path)
{
    VERIFY(m_path);
}

Path::~Path()
{
    FFI::ladybird_gfx_path_destroy(m_path);
}

Path::Path(Path const& other)
    : m_path(FFI::ladybird_gfx_path_clone(other.m_path))
{
}

Path& Path::operator=(Path const& other)
{
    if (this != &other) {
        FFI::ladybird_gfx_path_destroy(m_path);
        m_path = FFI::ladybird_gfx_path_clone(other.m_path);
    }
    return *this;
}

Path::Path(Path&& other)
    : m_path(exchange(other.m_path, nullptr))
{
}

Path& Path::operator=(Path&& other)
{
    if (this != &other) {
        FFI::ladybird_gfx_path_destroy(m_path);
        m_path = exchange(other.m_path, nullptr);
    }
    return *this;
}

Path Path::from_serialized_bytes(ReadonlyBytes bytes)
{
    return Path { FFI::ladybird_gfx_path_create_from_serialized_bytes(bytes.data(), bytes.size()) };
}

Path Path::adopt(FFI::RustPath* path)
{
    return Path { path };
}

PathData Path::data_of(FFI::RustPath const* path)
{
    auto data = FFI::ladybird_gfx_path_data(path);
    return {
        .verbs = { reinterpret_cast<u8 const*>(data.verbs), data.verb_count },
        .points = { reinterpret_cast<FloatPoint const*>(data.points), data.point_count },
        .conic_weights = { data.conic_weights, data.conic_weight_count },
        .fill_type = to_underlying(data.fill_type),
    };
}

void Path::clear()
{
    FFI::ladybird_gfx_path_clear(m_path);
}

void Path::move_to(FloatPoint const& point)
{
    FFI::ladybird_gfx_path_move_to(m_path, point.x(), point.y());
}

void Path::line_to(FloatPoint const& point)
{
    FFI::ladybird_gfx_path_line_to(m_path, point.x(), point.y());
}

void Path::close()
{
    FFI::ladybird_gfx_path_close(m_path);
}

void Path::elliptical_arc_to(FloatPoint point, FloatSize radii, float x_axis_rotation, bool large_arc, bool sweep)
{
    FFI::ladybird_gfx_path_elliptical_arc_to(m_path, point.x(), point.y(), radii.width(), radii.height(), x_axis_rotation, large_arc, sweep);
}

void Path::arc_to(FloatPoint point, float radius, bool large_arc, bool sweep)
{
    FFI::ladybird_gfx_path_arc_to(m_path, point.x(), point.y(), radius, large_arc, sweep);
}

void Path::quadratic_bezier_curve_to(FloatPoint through, FloatPoint point)
{
    FFI::ladybird_gfx_path_quadratic_bezier_curve_to(m_path, through.x(), through.y(), point.x(), point.y());
}

void Path::cubic_bezier_curve_to(FloatPoint c1, FloatPoint c2, FloatPoint p2)
{
    FFI::ladybird_gfx_path_cubic_bezier_curve_to(m_path, c1.x(), c1.y(), c2.x(), c2.y(), p2.x(), p2.y());
}

void Path::glyph_run(GlyphRun const& glyph_run)
{
    auto const& glyphs = glyph_run.glyphs();
    FFI::ladybird_gfx_path_append_glyph_run(m_path, &glyph_run.font(), reinterpret_cast<FFI::DrawGlyph const*>(glyphs.data()), glyphs.size());
}

void Path::append_path(Path const& other)
{
    FFI::ladybird_gfx_path_append_path(m_path, other.m_path);
}

Vector<u8> Path::serialize_to_bytes() const
{
    Vector<u8> bytes;
    FFI::ladybird_gfx_path_serialize(
        m_path, [](void* context, u8 const* data, size_t size) {
            static_cast<Vector<u8>*>(context)->append(data, size);
        },
        &bytes);
    return bytes;
}

bool Path::is_empty() const
{
    return FFI::ladybird_gfx_path_is_empty(m_path);
}

FloatPoint Path::last_point() const
{
    auto point = FFI::ladybird_gfx_path_last_point(m_path);
    return { point.x, point.y };
}

FloatRect Path::bounding_box() const
{
    auto rect = FFI::ladybird_gfx_path_bounding_box(m_path);
    return { rect.x, rect.y, rect.width, rect.height };
}

float Path::length() const
{
    return FFI::ladybird_gfx_path_length(m_path);
}

bool Path::contains(FloatPoint point, WindingRule winding_rule) const
{
    return FFI::ladybird_gfx_path_contains(m_path, point.x(), point.y(), to_underlying(winding_rule));
}

void Path::set_fill_type(WindingRule winding_rule)
{
    FFI::ladybird_gfx_path_set_fill_type(m_path, to_underlying(winding_rule));
}

Path Path::copy_transformed(AffineTransform const& transform) const
{
    auto copy = *this;
    copy.transform(transform);
    return copy;
}

void Path::transform(AffineTransform const& transform)
{
    Array<float, 6> values { transform.a(), transform.b(), transform.c(), transform.d(), transform.e(), transform.f() };
    FFI::ladybird_gfx_path_transform(m_path, values.data());
}

bool Path::operator==(Path const& other) const
{
    return FFI::ladybird_gfx_path_equals(m_path, other.m_path);
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::Path const& path)
{
    return encoder.encode(path.serialize_to_bytes());
}

template<>
ErrorOr<Gfx::Path> decode(Decoder& decoder)
{
    auto path_data = TRY(decoder.decode<Vector<u8>>());
    return Gfx::Path::from_serialized_bytes(path_data);
}

}
