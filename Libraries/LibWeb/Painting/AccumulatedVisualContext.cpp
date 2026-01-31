/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringBuilder.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/CSS/PropertyID.h>
#include <LibWeb/CSS/StyleValues/LengthStyleValue.h>
#include <LibWeb/CSS/StyleValues/TransformationStyleValue.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/Blending.h>
#include <LibWeb/Painting/PaintableBox.h>

namespace Web::Painting {

NonnullRefPtr<AccumulatedVisualContext> AccumulatedVisualContext::create(size_t id, VisualContextData data, RefPtr<AccumulatedVisualContext const> parent, GC::Weak<PaintableBox const> source_paintable)
{
    return adopt_ref(*new AccumulatedVisualContext(id, move(data), move(parent), move(source_paintable)));
}

bool ClipData::contains(CSSPixelPoint point) const
{
    return corner_radii.contains(point, rect);
}

Optional<CSSPixelPoint> AccumulatedVisualContext::transform_point_for_hit_test(CSSPixelPoint screen_point, AccumulatedVisualContextSnapshot const& snapshot) const
{
    Vector<AccumulatedVisualContext const*> chain;
    for (auto const* node = this; node; node = node->parent().ptr())
        chain.append(node);

    auto point = screen_point;
    for (size_t i = chain.size(); i > 0; --i) {
        auto const* node = chain[i - 1];

        auto result = snapshot.visual_context_data(node->id()).visit([&](PerspectiveData const& perspective) -> Optional<CSSPixelPoint> {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                auto inverse = affine.inverse();
                if (!inverse.has_value())
                    return {};
                point = inverse->map(point.to_type<float>()).to_type<CSSPixels>();
                return point; }, [&](ScrollData const& scroll) -> Optional<CSSPixelPoint> {
                auto offset = snapshot.scroll_offset_for_frame_id(scroll.scroll_frame_id);
                point.translate_by(-offset);
                return point; }, [&](TransformData const& transform) -> Optional<CSSPixelPoint> {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                auto inverse = affine.inverse();
                if (!inverse.has_value())
                    return {};

                auto offset_point = point - transform.origin;
                auto transformed = inverse->map(offset_point.to_type<float>()).to_type<CSSPixels>();
                point = transformed + transform.origin;
                return point; }, [&](ClipData const& clip) -> Optional<CSSPixelPoint> {
                // NOTE: The clip rect is stored in absolute (layout) coordinates. After inverse-transforming, `point`
                //       is also in layout coordinates, so we compare them directly without mapping back to screen space.
                if (!clip.contains(point))
                    return {};
                return point; }, [&](ClipPathData const& clip_path) -> Optional<CSSPixelPoint> {
                // NOTE: The clip path is stored in absolute (layout) coordinates. After inverse-transforming, `point`
                //       is also in layout coordinates, so we compare them directly without mapping back to screen space.
                if (!clip_path.bounding_rect.contains(point))
                    return {};
                if (!clip_path.path.contains(point.to_type<float>(), clip_path.fill_rule))
                    return {};
                return point; }, [&](EffectsData const&) -> Optional<CSSPixelPoint> {
                // Effects don't affect coordinate transforms
                return point; });

        if (!result.has_value())
            return {};
    }

    return point;
}

CSSPixelRect AccumulatedVisualContext::transform_rect_to_viewport(CSSPixelRect const& source_rect, AccumulatedVisualContextSnapshot const& snapshot) const
{
    Vector<AccumulatedVisualContext const*> chain;
    for (auto const* node = this; node; node = node->parent().ptr())
        chain.append(node);

    auto rect = source_rect.to_type<float>();
    for (auto const* node : chain) {
        snapshot.visual_context_data(node->id()).visit([&](TransformData const& transform) {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                auto origin = transform.origin.to_type<float>();
                rect.translate_by(-origin);
                rect = affine.map(rect);
                rect.translate_by(origin); }, [&](PerspectiveData const& perspective) {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                rect = affine.map(rect); }, [&](ScrollData const& scroll) {
                auto offset = snapshot.scroll_offset_for_frame_id(scroll.scroll_frame_id);
                rect.translate_by(offset.to_type<float>()); }, [&](ClipData const&) {}, [&](ClipPathData const&) {}, [&](EffectsData const&) {});
    }

    return rect.to_type<CSSPixels>();
}

void AccumulatedVisualContext::dump(StringBuilder& builder) const
{
    m_data.visit(
        [&](PerspectiveData const&) {
            builder.append("perspective"sv);
        },
        [&](ScrollData const& scroll) {
            builder.appendff("scroll_frame_id={}", scroll.scroll_frame_id);
            if (scroll.is_sticky)
                builder.append(" (sticky)"sv);
        },
        [&](TransformData const& transform) {
            auto const& matrix = transform.matrix.elements();
            auto const& origin = transform.origin;
            builder.appendff("transform=[{},{},{},{},{},{}] origin=({},{})", matrix[0][0], matrix[0][1], matrix[1][0], matrix[1][1], matrix[0][3], matrix[1][3], origin.x().to_float(), origin.y().to_float());
        },
        [&](ClipData const& clip) {
            auto const& rect = clip.rect;
            builder.appendff("clip=[{},{} {}x{}]", rect.x().to_float(), rect.y().to_float(), rect.width().to_float(), rect.height().to_float());

            if (clip.corner_radii.has_any_radius()) {
                auto const& corner_radii = clip.corner_radii;
                builder.appendff(" radii=({},{},{},{})", corner_radii.top_left.horizontal_radius, corner_radii.top_right.horizontal_radius, corner_radii.bottom_right.horizontal_radius, corner_radii.bottom_left.horizontal_radius);
            }
        },
        [&](ClipPathData const& clip_path) {
            auto const& rect = clip_path.bounding_rect;
            builder.appendff("clip_path=[bounds: {},{} {}x{}, path: {}]", rect.x().to_float(), rect.y().to_float(), rect.width().to_float(), rect.height().to_float(), clip_path.path.to_svg_string());
        },
        [&](EffectsData const& effects) {
            builder.append("effects=["sv);
            bool has_content = false;
            if (effects.opacity < 1.0f) {
                builder.appendff("opacity={}", effects.opacity);
                has_content = true;
            }
            if (effects.blend_mode != Gfx::CompositingAndBlendingOperator::Normal) {
                if (has_content)
                    builder.append(' ');
                builder.appendff("blend_mode={}", static_cast<int>(effects.blend_mode));
                has_content = true;
            }
            if (effects.filter.has_filters()) {
                if (has_content)
                    builder.append(' ');
                effects.filter.dump(builder);
                has_content = true;
            }
            if (effects.isolate) {
                if (has_content)
                    builder.append(' ');
                builder.append("isolate"sv);
            }
            builder.append("]"sv);
        });
}

void AccumulatedVisualContext::refresh()
{
    auto source = m_source_paintable.ptr();
    if (!source)
        return;
    auto const& cv = source->computed_values();
    auto const& layout_node = source->layout_node();
    m_data.visit(
        [&](TransformData const&) {
            auto const& transformations = cv.transformations();
            auto const& translate = cv.translate();
            auto const& rotate = cv.rotate();
            auto const& scale = cv.scale();
            auto matrix = Gfx::FloatMatrix4x4::identity();
            if (translate)
                matrix = matrix * translate->to_matrix(*source).release_value();
            if (rotate)
                matrix = matrix * rotate->to_matrix(*source).release_value();
            if (scale)
                matrix = matrix * scale->to_matrix(*source).release_value();
            for (auto const& transform : transformations)
                matrix = matrix * transform->to_matrix(*source).release_value();

            auto const& transform_origin = cv.transform_origin();
            auto reference_box = source->transform_reference_box();
            auto x = reference_box.left() + transform_origin.x.to_px(layout_node, reference_box.width());
            auto y = reference_box.top() + transform_origin.y.to_px(layout_node, reference_box.height());

            m_data = TransformData { matrix, { x, y } };
        },
        [&](PerspectiveData const&) {
            // Refresh is only called for value-to-value changes, so the source must still have a perspective.
            auto perspective = cv.perspective();
            VERIFY(perspective.has_value());

            auto reference_box = source->transform_reference_box();
            auto perspective_origin = cv.perspective_origin().resolved(layout_node, reference_box).to_type<float>();
            auto computed_x = perspective_origin.x();
            auto computed_y = perspective_origin.y();
            auto perspective_matrix = Gfx::translation_matrix(Vector3<float>(computed_x, computed_y, 0));
            perspective_matrix = perspective_matrix * CSS::TransformationStyleValue::create(CSS::PropertyID::Transform, CSS::TransformFunction::Perspective, CSS::StyleValueVector { CSS::LengthStyleValue::create(CSS::Length::make_px(perspective.value())) })->to_matrix({}).release_value();
            perspective_matrix = perspective_matrix * Gfx::translation_matrix(Vector3<float>(-computed_x, -computed_y, 0));

            m_data = PerspectiveData { perspective_matrix };
        },
        [&](EffectsData const&) {
            m_data = EffectsData {
                cv.opacity(),
                mix_blend_mode_to_compositing_and_blending_operator(cv.mix_blend_mode()),
                source->resolve_css_filter(cv.filter()),
                cv.isolation() == CSS::Isolation::Isolate
            };
        },
        [&](ClipData const&) {
            // Refresh is only called for value-to-value changes, so the source must still have a clip rect.
            auto css_clip = source->get_clip_rect();
            VERIFY(css_clip.has_value());
            m_data = ClipData { effective_css_clip_rect(*css_clip), {} };
        },
        [&](ClipPathData const&) {
            // Refresh is only called for value-to-value changes, so the source must still have a clip path.
            auto const& clip_path = cv.clip_path();
            VERIFY(clip_path.has_value() && clip_path->is_basic_shape());
            auto masking_area = source->absolute_border_box_rect();
            auto reference_box = CSSPixelRect { {}, masking_area.size() };
            auto const& basic_shape = clip_path->basic_shape();
            auto path = basic_shape.to_path(reference_box, layout_node);
            path.offset(masking_area.top_left().template to_type<float>());
            auto fill_rule = basic_shape.basic_shape().visit(
                [](CSS::Polygon const& polygon) { return polygon.fill_rule; },
                [](CSS::Path const& p) { return p.fill_rule; },
                [](auto const&) { return Gfx::WindingRule::Nonzero; });
            m_data = ClipPathData { move(path), masking_area, fill_rule };
        },
        [&](ScrollData const&) {});
}

void AccumulatedVisualContext::copy_data_into_snapshot(AccumulatedVisualContextSnapshot& snapshot) const
{
    snapshot.m_context_data[m_id] = m_data;
}

}
