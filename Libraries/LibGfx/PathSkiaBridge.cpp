/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/GenericShorthands.h>
#include <AK/Math.h>
#include <LibGfx/Path.h>
#include <LibGfx/SkiaUtils.h>
#include <RustFFI.h>
#include <core/SkPathBuilder.h>
#include <core/SkPathMeasure.h>
#include <core/SkString.h>
#include <utils/SkParsePath.h>

// The path queries that libgfx_rust does not answer itself yet.
extern "C" {
bool ladybird_gfx_skia_path_contains(void const*, float, float, Gfx::FFI::PathFillType);
float ladybird_gfx_skia_path_length(void const*);
float ladybird_gfx_skia_path_first_contour_length(void const*);
bool ladybird_gfx_skia_path_position_and_tangent(void const*, float, float*);
void ladybird_gfx_skia_path_append_svg_string(void const*, void (*)(void*, u8 const*, size_t), void*);
}

static SkPath skia_path(void const* path)
{
    return Gfx::to_skia_path(Gfx::Path::data_of(static_cast<Gfx::FFI::RustPath const*>(path)));
}

// By default, Skia measures a curve by the lengths of the lines between points along it that are at most half a unit off
// the curve, which leaves the length of a circle with a radius of 25 units about 1 unit short. Measure curves sixteen
// times as precisely instead.
static constexpr SkScalar path_measure_resolution_scale = 16;

// The furthest that the lines between points along a curve stay off it when Skia measures the curve.
static constexpr float path_measure_tolerance = 0.5f / path_measure_resolution_scale;

// The most lines with which to follow a single curve, which bounds the number of vertices of a huge curve.
static constexpr u32 maximum_line_count_along_curve = 4096;

extern "C" bool ladybird_gfx_skia_path_contains(void const* path, float x, float y, Gfx::FFI::PathFillType fill_type)
{
    auto sk_path = skia_path(path);
    sk_path.setFillType(static_cast<SkPathFillType>(fill_type));
    return sk_path.contains(x, y);
}

extern "C" float ladybird_gfx_skia_path_length(void const* path)
{
    SkPathMeasure path_measure(skia_path(path), false, path_measure_resolution_scale);
    float length = 0;
    do {
        length += path_measure.getLength();
    } while (path_measure.nextContour());
    return length;
}

extern "C" float ladybird_gfx_skia_path_first_contour_length(void const* path)
{
    SkPathMeasure path_measure(skia_path(path), false, path_measure_resolution_scale);
    return path_measure.getLength();
}

extern "C" bool ladybird_gfx_skia_path_position_and_tangent(void const* path, float distance, float* out_position_and_tangent)
{
    SkPathMeasure path_measure(skia_path(path), false, path_measure_resolution_scale);
    SkPoint position;
    SkVector tangent;
    if (!path_measure.getPosTan(distance, &position, &tangent))
        return false;
    out_position_and_tangent[0] = position.x();
    out_position_and_tangent[1] = position.y();
    out_position_and_tangent[2] = tangent.x();
    out_position_and_tangent[3] = tangent.y();
    return true;
}

extern "C" void ladybird_gfx_skia_path_append_svg_string(void const* path, void (*append)(void*, u8 const*, size_t), void* context)
{
    auto svg_string = SkParsePath::ToSVGString(skia_path(path));
    append(context, reinterpret_cast<u8 const*>(svg_string.c_str()), svg_string.size());
}

namespace Gfx {

// The point at a distance along the path, clamped to its ends, and the unit tangent of the path there. The gaps between
// the subpaths do not add to the distance. A path without length is only its start point, where it has no tangent.
Optional<PointAlongPath> Path::point_at_length(float distance) const
{
    auto sk_path = to_skia_path(data());
    SkPathMeasure path_measure(sk_path, false, path_measure_resolution_scale);
    Optional<PointAlongPath> end_of_path;
    auto remaining_distance = max(distance, 0.f);
    do {
        auto subpath_length = path_measure.getLength();
        if (subpath_length <= 0)
            continue;
        SkPoint position;
        SkVector tangent;
        auto distance_in_subpath = min(remaining_distance, subpath_length);
        if (!path_measure.getPosTan(distance_in_subpath, &position, &tangent))
            continue;
        end_of_path = PointAlongPath { { position.x(), position.y() }, { tangent.x(), tangent.y() } };
        if (remaining_distance <= subpath_length)
            return end_of_path;
        remaining_distance -= subpath_length;
    } while (path_measure.nextContour());

    if (end_of_path.has_value())
        return end_of_path;
    if (sk_path.countPoints() == 0)
        return {};
    auto start = sk_path.getPoint(0);
    return PointAlongPath { { start.x(), start.y() }, { 0, 0 } };
}

// The number of lines with which to start following a curve, by Wang's formula: the square root of the largest second
// difference of its control points, scaled by its degree, over the tolerance. That keeps lines between points at equal
// parameter steps within the tolerance of the curve, and depends on how much the curve bends rather than on how long it
// is.
static u32 line_count_to_follow_curve(SkPathIter::Rec const& segment)
{
    auto const& points = segment.fPoints;
    auto second_difference = [&](size_t index) {
        return SkPoint::Length(points[index].x() - 2 * points[index + 1].x() + points[index + 2].x(),
            points[index].y() - 2 * points[index + 1].y() + points[index + 2].y());
    };

    float scaled_second_difference = 0;
    switch (segment.fVerb) {
    case SkPathVerb::kQuad:
        scaled_second_difference = second_difference(0) / 4;
        break;
    case SkPathVerb::kConic:
        // A conic with a weight of at most 1, like the arcs of SVG paths, bends no more than the quadratic with the same
        // control points.
        scaled_second_difference = max(1.f, segment.conicWeight()) * second_difference(0) / 4;
        break;
    case SkPathVerb::kCubic:
        scaled_second_difference = 3 * max(second_difference(0), second_difference(1)) / 4;
        break;
    default:
        return 1;
    }
    auto line_count = AK::ceil(AK::sqrt(static_cast<double>(scaled_second_difference) / path_measure_tolerance));
    return static_cast<u32>(clamp(line_count, 1., static_cast<double>(maximum_line_count_along_curve)));
}

struct CurveToFollow {
    SkPathMeasure& measure;
    float distance_to_curve;
    Vector<PathVertex>& vertices;
    u32 remaining_line_count;
};

// Appends the vertices between two points along a curve, splitting the line between them in half for as long as the
// curve passes further than the tolerance from it halfway, or until the curve has as many lines as it may have.
static void follow_curve_between(CurveToFollow& curve, float start_distance, FloatPoint start_point, float end_distance, FloatPoint end_point)
{
    if (curve.remaining_line_count == 0)
        return;
    auto middle_distance = (start_distance + end_distance) / 2;
    SkPoint position;
    SkVector tangent;
    if (!curve.measure.getPosTan(middle_distance, &position, &tangent))
        return;
    FloatPoint middle_point { position.x(), position.y() };
    if (middle_point.distance_from((start_point + end_point) / 2) <= path_measure_tolerance)
        return;

    --curve.remaining_line_count;
    follow_curve_between(curve, start_distance, start_point, middle_distance, middle_point);
    curve.vertices.append({ middle_point, curve.distance_to_curve + middle_distance, { tangent.x(), tangent.y() } });
    follow_curve_between(curve, middle_distance, middle_point, end_distance, end_point);
}

// The vertices of polylines through the ends of the segments of the path, with their distance along it. Along a curved
// segment, the polylines pass through points along the curve that keep them within path_measure_tolerance of it. The
// gaps between the subpaths do not add to the distance, so a subpath starts at the distance its predecessor ends at. A
// path without length is only its start point.
Vector<PathVertex> Path::vertices() const
{
    auto to_point = [](SkPoint point) { return FloatPoint { point.x(), point.y() }; };
    auto sk_path = to_skia_path(data());

    // Each vertex has the unit tangent of the path there as its direction. Where the path changes direction at once,
    // the vertex appears twice: with the direction in which the path arrives, and with the one in which it leaves. A
    // subpath start that no segment with length leaves from has no direction.
    Vector<PathVertex> vertices;
    float distance = 0;
    auto iterator = sk_path.iter();
    while (auto segment = iterator.next()) {
        auto const& points = segment->fPoints;
        SkPathBuilder segment_builder;
        segment_builder.moveTo(points[0]);
        switch (segment->fVerb) {
        case SkPathVerb::kMove:
            vertices.append({ to_point(points[0]), distance, {} });
            continue;
        case SkPathVerb::kLine:
        case SkPathVerb::kClose:
            segment_builder.lineTo(points[1]);
            break;
        case SkPathVerb::kQuad:
            segment_builder.quadTo(points[1], points[2]);
            break;
        case SkPathVerb::kConic:
            segment_builder.conicTo(points[1], points[2], segment->conicWeight());
            break;
        case SkPathVerb::kCubic:
            segment_builder.cubicTo(points[1], points[2], points[3]);
            break;
        }

        // A segment without length has no direction, and leaves the path where it was.
        SkPathMeasure segment_measure(segment_builder.detach(), false, path_measure_resolution_scale);
        auto segment_length = segment_measure.getLength();
        if (segment_length <= 0)
            continue;
        auto direction_at = [&](float distance_in_segment) {
            SkVector tangent;
            if (!segment_measure.getPosTan(distance_in_segment, nullptr, &tangent))
                return FloatVector2 {};
            return FloatVector2 { tangent.x(), tangent.y() };
        };

        auto start_direction = direction_at(0);
        if (vertices.last().direction == FloatVector2 {})
            vertices.last().direction = start_direction;
        else if (vertices.last().direction != start_direction)
            vertices.append({ to_point(points[0]), distance, start_direction });

        auto line_count = line_count_to_follow_curve(*segment);
        CurveToFollow curve { segment_measure, distance, vertices, maximum_line_count_along_curve - line_count };
        auto previous_distance = 0.f;
        auto previous_point = to_point(points[0]);
        for (u32 line = 1; line <= line_count; ++line) {
            auto distance_in_segment = segment_length * static_cast<float>(line) / static_cast<float>(line_count);
            SkPoint position;
            SkVector tangent;
            if (line == line_count)
                position = points.back();
            else if (!segment_measure.getPosTan(distance_in_segment, &position, &tangent))
                continue;
            follow_curve_between(curve, previous_distance, previous_point, distance_in_segment, to_point(position));
            if (line < line_count)
                vertices.append({ to_point(position), distance + distance_in_segment, { tangent.x(), tangent.y() } });
            previous_distance = distance_in_segment;
            previous_point = to_point(position);
        }
        distance += segment_length;
        vertices.append({ to_point(points.back()), distance, direction_at(segment_length) });
    }

    // Skia's iterator leaves out a move that ends the path.
    if (vertices.is_empty() && sk_path.countPoints() > 0)
        vertices.append({ to_point(sk_path.getPoint(0)), 0, {} });
    return vertices;
}

}
