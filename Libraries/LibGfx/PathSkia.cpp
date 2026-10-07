/*
 * Copyright (c) 2024, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#define AK_DONT_REPLACE_STD

#include <AK/ScopeGuard.h>
#include <AK/Span.h>
#include <AK/TypeCasts.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/PathSkia.h>
#include <LibGfx/Rect.h>
#include <LibGfx/SkiaUtils.h>
#include <LibGfx/TextLayout.h>
#include <core/SkPath.h>
#include <core/SkPathBuilder.h>
#include <core/SkPathMeasure.h>
#include <core/SkString.h>
#include <utils/SkParsePath.h>

#include <harfbuzz/hb.h>

#ifdef AK_OS_MACOS
#    include <LibGfx/Font/TypefaceCoreText.h>
#endif

template<>
constexpr bool AllocatedWithSystemAllocator<SkPath> = true;

template<>
constexpr bool AllocatedWithSystemAllocator<SkPathBuilder> = true;

namespace Gfx {

static FloatPoint to_gfx_point(SkPoint const& point)
{
    return { point.x(), point.y() };
}

namespace {

// Maps the coordinates of a glyph outline from font units, with y up, to pixels, with y down. A contour starts only at
// its first segment that goes somewhere, and segments that stay at one point are left out, so contours of a single
// point, which fonts use as anchors, add nothing to the path.
struct GlyphOutline {
    SkPathBuilder& builder;
    float units_to_pixels_x;
    float units_to_pixels_y;
    bool has_contour { false };
    float current_x { 0 };
    float current_y { 0 };

    float x(float units) const { return units * units_to_pixels_x; }
    // Subtract from 0, so that a point on the baseline gets 0 and not -0.
    float y(float units) const { return 0.0f - units * units_to_pixels_y; }

    bool is_at(float to_x, float to_y) const { return current_x == to_x && current_y == to_y; }

    void go_to(float to_x, float to_y)
    {
        if (!has_contour) {
            has_contour = true;
            builder.moveTo(x(current_x), y(current_y));
        }
        current_x = to_x;
        current_y = to_y;
    }

    void close_contour()
    {
        if (!has_contour)
            return;
        builder.close();
        has_contour = false;
    }
};

// NOTE: HarfBuzz emits the implicit line that closes each contour as a regular line_to before close_path. The
//       quadratic callback must be set, otherwise HarfBuzz converts quadratic segments to cubics.
hb_draw_funcs_t* glyph_outline_draw_funcs()
{
    static hb_draw_funcs_t* draw_funcs = [] {
        auto* funcs = hb_draw_funcs_create();
        hb_draw_funcs_set_move_to_func(
            funcs, [](hb_draw_funcs_t*, void* draw_data, hb_draw_state_t*, float to_x, float to_y, void*) {
                auto& outline = *static_cast<GlyphOutline*>(draw_data);
                outline.close_contour();
                outline.current_x = to_x;
                outline.current_y = to_y;
            },
            nullptr, nullptr);
        hb_draw_funcs_set_line_to_func(
            funcs, [](hb_draw_funcs_t*, void* draw_data, hb_draw_state_t*, float to_x, float to_y, void*) {
                auto& outline = *static_cast<GlyphOutline*>(draw_data);
                if (outline.is_at(to_x, to_y))
                    return;
                outline.go_to(to_x, to_y);
                outline.builder.lineTo(outline.x(to_x), outline.y(to_y));
            },
            nullptr, nullptr);
        hb_draw_funcs_set_quadratic_to_func(
            funcs, [](hb_draw_funcs_t*, void* draw_data, hb_draw_state_t*, float control_x, float control_y, float to_x, float to_y, void*) {
                auto& outline = *static_cast<GlyphOutline*>(draw_data);
                if (outline.is_at(control_x, control_y) && outline.is_at(to_x, to_y))
                    return;
                outline.go_to(to_x, to_y);
                outline.builder.quadTo(outline.x(control_x), outline.y(control_y), outline.x(to_x), outline.y(to_y));
            },
            nullptr, nullptr);
        hb_draw_funcs_set_cubic_to_func(
            funcs, [](hb_draw_funcs_t*, void* draw_data, hb_draw_state_t*, float control1_x, float control1_y, float control2_x, float control2_y, float to_x, float to_y, void*) {
                auto& outline = *static_cast<GlyphOutline*>(draw_data);
                if (outline.is_at(control1_x, control1_y) && outline.is_at(control2_x, control2_y) && outline.is_at(to_x, to_y))
                    return;
                outline.go_to(to_x, to_y);
                outline.builder.cubicTo(outline.x(control1_x), outline.y(control1_y), outline.x(control2_x), outline.y(control2_y), outline.x(to_x), outline.y(to_y));
            },
            nullptr, nullptr);
        hb_draw_funcs_set_close_path_func(
            funcs, [](hb_draw_funcs_t*, void* draw_data, hb_draw_state_t*, void*) {
                static_cast<GlyphOutline*>(draw_data)->close_contour();
            },
            nullptr, nullptr);
        hb_draw_funcs_make_immutable(funcs);
        return funcs;
    }();
    return draw_funcs;
}

}

#ifdef AK_OS_MACOS
static SkPath core_text_glyph_outline(Font const& font, CTFontRef core_text_font, u32 glyph_id)
{
    auto variations = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    ScopeGuard release_variations = [&] { CFRelease(variations); };
    for (auto const& axis : font.variation_settings().axes) {
        i64 tag = axis.key.to_u32();
        float value = axis.value;
        auto tag_number = CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt64Type, &tag);
        auto value_number = CFNumberCreate(kCFAllocatorDefault, kCFNumberFloat32Type, &value);
        CFDictionarySetValue(variations, tag_number, value_number);
        CFRelease(tag_number);
        CFRelease(value_number);
    }
    CFTypeRef attribute_keys[] = { kCTFontVariationAttribute };
    CFTypeRef attribute_values[] = { variations };
    auto attributes = CFDictionaryCreate(kCFAllocatorDefault, attribute_keys, attribute_values, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    ScopeGuard release_attributes = [&] { CFRelease(attributes); };
    auto descriptor = CTFontDescriptorCreateWithAttributes(attributes);
    ScopeGuard release_descriptor = [&] { CFRelease(descriptor); };
    auto sized_font = CTFontCreateCopyWithAttributes(core_text_font, font.pixel_size(), nullptr, descriptor);
    if (!sized_font)
        return {};
    ScopeGuard release_sized_font = [&] { CFRelease(sized_font); };

    auto path = CTFontCreatePathForGlyph(sized_font, static_cast<CGGlyph>(glyph_id), nullptr);
    if (!path)
        return {};
    ScopeGuard release_path = [&] { CGPathRelease(path); };

    // CoreText gives points in pixels with y up.
    SkPathBuilder builder;
    CGPathApply(path, &builder, [](void* info, CGPathElement const* element) {
        auto& builder = *static_cast<SkPathBuilder*>(info);
        auto point = [&](size_t index) {
            return SkPoint::Make(static_cast<float>(element->points[index].x), 0.0f - static_cast<float>(element->points[index].y));
        };
        switch (element->type) {
        case kCGPathElementMoveToPoint:
            builder.moveTo(point(0));
            break;
        case kCGPathElementAddLineToPoint:
            builder.lineTo(point(0));
            break;
        case kCGPathElementAddQuadCurveToPoint:
            builder.quadTo(point(0), point(1));
            break;
        case kCGPathElementAddCurveToPoint:
            builder.cubicTo(point(0), point(1), point(2));
            break;
        case kCGPathElementCloseSubpath:
            builder.close();
            break;
        }
    });
    return builder.detach();
}
#endif

// The outline of a glyph in pixels, with its origin on the baseline.
static SkPath glyph_outline(Font const& font, u32 glyph_id)
{
#ifdef AK_OS_MACOS
    if (auto const* core_text_typeface = as_if<TypefaceCoreText>(font.typeface()); core_text_typeface && core_text_typeface->has_outlines_that_only_core_text_draws())
        return core_text_glyph_outline(font, core_text_typeface->core_text_font(), glyph_id);
#endif

    auto* hb_font = font.harfbuzz_font();
    int x_scale = 0;
    int y_scale = 0;
    hb_font_get_scale(hb_font, &x_scale, &y_scale);
    if (x_scale <= 0 || y_scale <= 0)
        return {};
    SkPathBuilder builder;
    GlyphOutline outline {
        .builder = builder,
        .units_to_pixels_x = font.pixel_size() / static_cast<float>(x_scale),
        .units_to_pixels_y = font.pixel_size() / static_cast<float>(y_scale),
    };
    hb_font_draw_glyph(hb_font, glyph_id, glyph_outline_draw_funcs(), &outline);
    outline.close_contour();
    return builder.detach();
}

NonnullOwnPtr<Gfx::PathImplSkia> PathImplSkia::create()
{
    return adopt_own(*new PathImplSkia);
}

PathImplSkia::PathImplSkia()
    : m_path_builder(adopt_own(*new SkPathBuilder))
{
}

PathImplSkia::PathImplSkia(PathImplSkia const& other)
    : m_last_move_to(other.m_last_move_to)
    , m_has_current_point(other.m_has_current_point)
    , m_path_builder(adopt_own(*new SkPathBuilder(*other.m_path_builder)))
{
}

PathImplSkia::~PathImplSkia()
{
    delete m_cached_path.load(AK::memory_order_relaxed);
}

SkPath const& PathImplSkia::sk_path() const
{
    if (auto* cached_path = m_cached_path.load(AK::memory_order_acquire))
        return *cached_path;
    auto* path = new SkPath(m_path_builder->snapshot());
    SkPath* expected = nullptr;
    if (m_cached_path.compare_exchange_strong(expected, path, AK::memory_order_acq_rel))
        return *path;
    delete path;
    return *expected;
}

SkPathBuilder& PathImplSkia::sk_path_builder()
{
    delete m_cached_path.exchange(nullptr, AK::memory_order_relaxed);
    return *m_path_builder;
}

void PathImplSkia::update_state_from_builder()
{
    update_state_from_path(sk_path());
}

void PathImplSkia::set_path(SkPath const& path)
{
    sk_path_builder() = SkPathBuilder(path);
    update_state_from_path(path);
}

void PathImplSkia::update_state_from_path(SkPath const& path)
{
    m_has_current_point = false;
    m_last_move_to = {};

    auto const points = path.points();
    size_t point_index = 0;
    for (auto verb : path.verbs()) {
        switch (verb) {
        case SkPathVerb::kMove:
            m_has_current_point = true;
            m_last_move_to = to_gfx_point(points[point_index++]);
            break;
        case SkPathVerb::kLine:
            m_has_current_point = true;
            point_index += 1;
            break;
        case SkPathVerb::kQuad:
        case SkPathVerb::kConic:
            m_has_current_point = true;
            point_index += 2;
            break;
        case SkPathVerb::kCubic:
            m_has_current_point = true;
            point_index += 3;
            break;
        case SkPathVerb::kClose:
            m_has_current_point = true;
            break;
        }
    }
}

void PathImplSkia::clear()
{
    sk_path_builder().reset();
    m_last_move_to = {};
    m_has_current_point = false;
}

void PathImplSkia::move_to(Gfx::FloatPoint const& point)
{
    m_last_move_to = point;
    m_has_current_point = true;
    sk_path_builder().moveTo(point.x(), point.y());
}

void PathImplSkia::line_to(Gfx::FloatPoint const& point)
{
    if (!m_has_current_point) {
        move_to(point);
        return;
    }
    sk_path_builder().lineTo(point.x(), point.y());
}

void PathImplSkia::close()
{
    if (!m_has_current_point)
        return;
    sk_path_builder().close();
    sk_path_builder().moveTo(m_last_move_to.x(), m_last_move_to.y());
}

void PathImplSkia::elliptical_arc_to(FloatPoint point, FloatSize radii, float x_axis_rotation, bool large_arc, bool sweep)
{
    if (!m_has_current_point) {
        move_to(point);
        return;
    }

    SkPoint skPoint = SkPoint::Make(point.x(), point.y());
    SkPoint skRadii = SkPoint::Make(radii.width(), radii.height());
    SkScalar skXRotation = SkFloatToScalar(sk_float_radians_to_degrees(x_axis_rotation));
    SkPathBuilder::ArcSize skLargeArc = large_arc ? SkPathBuilder::kLarge_ArcSize : SkPathBuilder::kSmall_ArcSize;
    SkPathDirection skSweep = sweep ? SkPathDirection::kCW : SkPathDirection::kCCW;
    sk_path_builder().arcTo(skRadii, skXRotation, skLargeArc, skSweep, skPoint);
}

void PathImplSkia::arc_to(FloatPoint point, float radius, bool large_arc, bool sweep)
{
    if (!m_has_current_point) {
        move_to(point);
        return;
    }

    SkPoint skPoint = SkPoint::Make(point.x(), point.y());
    SkPoint skRadii = SkPoint::Make(radius, radius);
    SkPathBuilder::ArcSize skLargeArc = large_arc ? SkPathBuilder::kLarge_ArcSize : SkPathBuilder::kSmall_ArcSize;
    SkPathDirection skSweep = sweep ? SkPathDirection::kCW : SkPathDirection::kCCW;
    sk_path_builder().arcTo(skRadii, 0, skLargeArc, skSweep, skPoint);
}

void PathImplSkia::quadratic_bezier_curve_to(FloatPoint through, FloatPoint point)
{
    if (!m_has_current_point)
        move_to(through);
    sk_path_builder().quadTo(through.x(), through.y(), point.x(), point.y());
}

void PathImplSkia::cubic_bezier_curve_to(FloatPoint c1, FloatPoint c2, FloatPoint p2)
{
    if (!m_has_current_point)
        move_to(c1);
    sk_path_builder().cubicTo(c1.x(), c1.y(), c2.x(), c2.y(), p2.x(), p2.y());
}

void PathImplSkia::glyph_run(GlyphRun const& glyph_run)
{
    if (glyph_run.font().is_invisible())
        return;
    auto& path_builder = sk_path_builder();
    path_builder.setFillType(SkPathFillType::kWinding);
    auto font_ascent = glyph_run.font().pixel_metrics().ascent;
    for (auto const& glyph : glyph_run.glyphs())
        path_builder.addPath(glyph_outline(glyph_run.font(), glyph.glyph_id), glyph.position.x(), glyph.position.y() + font_ascent);
    update_state_from_path(sk_path());
}

NonnullOwnPtr<PathImpl> PathImplSkia::place_glyph_runs_along(ReadonlySpan<NonnullRefPtr<GlyphRun>> glyph_runs, float offset) const
{
    SkPathMeasure path_measure(sk_path(), false);
    SkScalar path_length = path_measure.getLength();

    auto output_path = PathImplSkia::create();

    bool reached_end_of_path = false;
    for (auto const& glyph_run : glyph_runs) {
        if (glyph_run->font().is_invisible())
            continue;
        for (auto const& glyph : glyph_run->glyphs()) {
            SkScalar glyph_distance = offset + glyph.position.x();

            SkPoint position;
            SkVector tangent;
            if (!path_measure.getPosTan(glyph_distance, &position, &tangent))
                continue;

            SkScalar midpoint_distance = glyph_distance + (glyph.glyph_width / 2.0f);
            if (midpoint_distance > path_length) {
                reached_end_of_path = true;
                break;
            }

            SkMatrix matrix;
            matrix.setTranslate(position.x(), position.y());
            matrix.preRotate(SkRadiansToDegrees(std::atan2(tangent.y(), tangent.x())));
            output_path->sk_path_builder().addPath(glyph_outline(glyph_run->font(), glyph.glyph_id), matrix);
        }
        if (reached_end_of_path)
            break;
    }
    output_path->update_state_from_builder();

    return output_path;
}

void PathImplSkia::append_path(Gfx::Path const& other)
{
    auto const& other_impl = static_cast<PathImplSkia const&>(other.impl());
    sk_path_builder().addPath(other_impl.sk_path());
    if (other_impl.m_has_current_point) {
        m_has_current_point = true;
        m_last_move_to = other_impl.m_last_move_to;
    }
}

Vector<u8> PathImplSkia::serialize_to_bytes() const
{
    auto const& path = sk_path();
    auto path_data_size = path.writeToMemory(nullptr);
    Vector<u8> path_data;
    path_data.resize(path_data_size);
    path.writeToMemory(path_data.data());
    return path_data;
}

void PathImplSkia::deserialize_from_bytes(ReadonlyBytes bytes)
{
    set_path(SkPath::ReadFromMemory(bytes.data(), bytes.size()).value_or(SkPath {}));
}

bool PathImplSkia::is_empty() const
{
    return !m_has_current_point;
}

Gfx::FloatPoint PathImplSkia::last_point() const
{
    auto last = m_path_builder->getLastPt();
    if (!last.has_value())
        return {};
    return { last->fX, last->fY };
}

Gfx::FloatRect PathImplSkia::bounding_box() const
{
    auto bounds = m_path_builder->computeBounds();
    return { bounds.fLeft, bounds.fTop, bounds.fRight - bounds.fLeft, bounds.fBottom - bounds.fTop };
}

float PathImplSkia::length() const
{
    SkPathMeasure path_measure(sk_path(), false);
    float length = 0;
    do {
        length += path_measure.getLength();
    } while (path_measure.nextContour());
    return length;
}

bool PathImplSkia::contains(FloatPoint point, Gfx::WindingRule winding_rule) const
{
    SkPath temp_path = sk_path();
    temp_path.setFillType(to_skia_path_fill_type(winding_rule));
    return temp_path.contains(point.x(), point.y());
}

void PathImplSkia::set_fill_type(Gfx::WindingRule winding_rule)
{
    sk_path_builder().setFillType(to_skia_path_fill_type(winding_rule));
}

NonnullOwnPtr<PathImpl> PathImplSkia::clone() const
{
    return adopt_own(*new PathImplSkia(*this));
}

NonnullOwnPtr<PathImpl> PathImplSkia::copy_transformed(Gfx::AffineTransform const& transform) const
{
    auto new_path = adopt_own(*new PathImplSkia(*this));
    auto matrix = SkMatrix::MakeAll(
        transform.a(), transform.c(), transform.e(),
        transform.b(), transform.d(), transform.f(),
        0, 0, 1);
    new_path->sk_path_builder().transform(matrix);
    if (new_path->m_has_current_point)
        new_path->m_last_move_to = transform.map(new_path->m_last_move_to);
    return new_path;
}

String PathImplSkia::to_svg_string() const
{
    auto svg_string = SkParsePath::ToSVGString(sk_path());
    return MUST(String::from_utf8(StringView { svg_string.c_str(), svg_string.size() }));
}

}
