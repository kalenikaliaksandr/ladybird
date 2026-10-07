/*
 * Copyright (c) 2024, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Forward.h>
#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Vector.h>
#include <AK/kmalloc.h>
#include <LibGfx/AffineTransform.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/Vector2.h>
#include <LibGfx/WindingRule.h>
#include <LibIPC/Forward.h>

namespace Gfx {

namespace FFI {

struct RustPath;

}

// The geometry of a path, as the compositor uses it: verbs, points and conic weights with the values of Skia's path
// representation. A path with a point that is not finite has no geometry. The data is valid until the path changes
// or is destroyed.
struct PathData {
    ReadonlySpan<u8> verbs;
    ReadonlySpan<FloatPoint> points;
    ReadonlySpan<float> conic_weights;
    u8 fill_type { 0 };
};

struct PointAlongPath {
    FloatPoint point;
    FloatVector2 tangent;
};

struct PathVertex {
    FloatPoint point;
    float distance;
    FloatVector2 direction;
};

class Path {
public:
    AK_ALLOC_WITH_KMALLOC;

    Path();
    ~Path();

    Path(Path const&);
    Path& operator=(Path const&);
    Path(Path&&);
    Path& operator=(Path&&);

    [[nodiscard]] static Path from_serialized_bytes(ReadonlyBytes);

    // Takes ownership of a path that the Rust side made.
    [[nodiscard]] static Path adopt(FFI::RustPath*);

    // The data of a path that Rust code owns.
    [[nodiscard]] static PathData data_of(FFI::RustPath const*);

    void clear();
    void move_to(FloatPoint const&);
    void line_to(FloatPoint const&);
    void close();

    enum class CapStyle {
        Butt,
        Round,
        Square,
    };

    enum class JoinStyle {
        Miter,
        Round,
        Bevel,
    };

    void elliptical_arc_to(FloatPoint point, FloatSize radii, float x_axis_rotation, bool large_arc, bool sweep);
    void arc_to(FloatPoint point, float radius, bool large_arc, bool sweep);
    void quadratic_bezier_curve_to(FloatPoint through, FloatPoint point);
    void cubic_bezier_curve_to(FloatPoint c1, FloatPoint c2, FloatPoint p2);
    void glyph_run(GlyphRun const&);

    void horizontal_line_to(float x) { line_to({ x, last_point().y() }); }
    void vertical_line_to(float y) { line_to({ last_point().x(), y }); }

    void append_path(Path const&);

    [[nodiscard]] Vector<u8> serialize_to_bytes() const;

    [[nodiscard]] bool is_empty() const;
    FloatPoint last_point() const;
    FloatRect bounding_box() const;
    float length() const;
    Optional<PointAlongPath> point_at_length(float distance) const;
    Vector<PathVertex> vertices() const;
    bool contains(FloatPoint, WindingRule) const;
    void set_fill_type(WindingRule);

    Path clone() const { return *this; }
    Path copy_transformed(AffineTransform const&) const;
    void transform(AffineTransform const&);

    PathData data() const { return data_of(m_path); }

    bool operator==(Path const&) const;

private:
    explicit Path(FFI::RustPath*);

    FFI::RustPath* m_path { nullptr };
};

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder&, Gfx::Path const&);

template<>
ErrorOr<Gfx::Path> decode(Decoder&);

}
