/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringBuilder.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

bool ClipData::contains(DevicePixelPoint point) const
{
    return corner_radii.contains(point.to_type<int>(), rect.to_type<int>());
}

NonnullRefPtr<SpatialContextTree> SpatialContextTree::create()
{
    auto context_tree = adopt_ref(*new SpatialContextTree());
    context_tree->m_nodes.append({ ScrollData { {}, false }, {}, 0 });
    return context_tree;
}

SpatialContextIndex SpatialContextTree::append(SpatialContextData data, SpatialContextIndex parent_index)
{
    size_t depth = parent_index.value() ? m_nodes[parent_index.value()].depth + 1 : 1;
    auto index = SpatialContextIndex(m_nodes.size());
    m_nodes.append({ move(data), parent_index, depth });
    return index;
}

SpatialContextIndex SpatialContextTree::find_common_ancestor(SpatialContextIndex a, SpatialContextIndex b) const
{
    if (!a.value() || !b.value())
        return {};
    size_t a_index = a.value();
    size_t b_index = b.value();
    while (m_nodes[a_index].depth > m_nodes[b_index].depth)
        a_index = m_nodes[a_index].parent_index.value();
    while (m_nodes[b_index].depth > m_nodes[a_index].depth)
        b_index = m_nodes[b_index].parent_index.value();
    while (a_index != b_index) {
        a_index = m_nodes[a_index].parent_index.value();
        b_index = m_nodes[b_index].parent_index.value();
    }
    return SpatialContextIndex(a_index);
}

Vector<size_t, 8> SpatialContextTree::build_ancestor_chain(SpatialContextIndex index) const
{
    auto const& node = m_nodes[index.value()];
    Vector<size_t, 8> chain;
    chain.ensure_capacity(node.depth);
    for (size_t i = index.value(); i; i = m_nodes[i].parent_index.value())
        chain.append(i);
    return chain;
}

Gfx::FloatPoint SpatialContextTree::inverse_transform_point(SpatialContextIndex index, Gfx::FloatPoint screen_point, ScrollStateSnapshot const& scroll_state) const
{
    if (!index.value())
        return screen_point;

    auto chain = build_ancestor_chain(index);

    auto point = screen_point;
    for (size_t i = chain.size(); i > 0; --i) {
        auto const& node = m_nodes[chain[i - 1]];

        node.data.visit(
            [&](PerspectiveData const& perspective) {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                auto inverse = affine.inverse();
                if (inverse.has_value())
                    point = inverse->map(point);
            },
            [&](ScrollData const& scroll) {
                point.translate_by(-scroll_state.device_offset_for_index(scroll.scroll_frame_index));
            },
            [&](TransformData const& transform) {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                auto inverse = affine.inverse();
                if (!inverse.has_value())
                    return;

                auto offset_point = point - transform.origin;
                auto transformed = inverse->map(offset_point);
                point = transformed + transform.origin;
            });
    }

    return point;
}

Gfx::FloatRect SpatialContextTree::transform_rect_to_viewport(SpatialContextIndex index, Gfx::FloatRect const& source_rect, ScrollStateSnapshot const& scroll_state) const
{
    if (!index.value())
        return source_rect;

    auto rect = source_rect;
    for (size_t i = index.value(); i; i = m_nodes[i].parent_index.value()) {
        auto const& node = m_nodes[i];
        node.data.visit(
            [&](TransformData const& transform) {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                rect.translate_by(-transform.origin);
                rect = affine.map(rect);
                rect.translate_by(transform.origin);
            },
            [&](PerspectiveData const& perspective) {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                rect = affine.map(rect);
            },
            [&](ScrollData const& scroll) {
                rect.translate_by(scroll_state.device_offset_for_index(scroll.scroll_frame_index));
            });
    }

    return rect;
}

Gfx::FloatMatrix4x4 SpatialContextTree::transform_matrix_to_viewport(SpatialContextIndex index, ScrollStateSnapshot const& scroll_state) const
{
    auto matrix = Gfx::FloatMatrix4x4::identity();

    for (size_t i = index.value(); i; i = m_nodes[i].parent_index.value()) {
        auto const& node = m_nodes[i];
        node.data.visit(
            [&](TransformData const& transform) {
                auto node_matrix = Gfx::translation_matrix(Vector3<float>(transform.origin.x(), transform.origin.y(), 0));
                node_matrix = node_matrix * transform.matrix;
                node_matrix = node_matrix * Gfx::translation_matrix(Vector3<float>(-transform.origin.x(), -transform.origin.y(), 0));
                matrix = node_matrix * matrix;
            },
            [&](PerspectiveData const& perspective) {
                matrix = perspective.matrix * matrix;
            },
            [&](ScrollData const& scroll) {
                auto offset = scroll_state.device_offset_for_index(scroll.scroll_frame_index);
                matrix = Gfx::translation_matrix(Vector3<float>(offset.x(), offset.y(), 0)) * matrix;
            });
    }

    return matrix;
}

void SpatialContextTree::dump(SpatialContextIndex index, StringBuilder& builder) const
{
    if (!index.value())
        return;

    auto const& node = m_nodes[index.value()];
    node.data.visit(
        [&](PerspectiveData const&) {
            builder.append("perspective"sv);
        },
        [&](ScrollData const& scroll) {
            builder.appendff("scroll_frame_id={}", scroll.scroll_frame_index);
            if (scroll.is_sticky)
                builder.append(" (sticky)"sv);
        },
        [&](TransformData const& transform) {
            auto const& matrix = transform.matrix.elements();
            auto const& origin = transform.origin;
            builder.appendff("transform=[{},{},{},{},{},{}] origin=({},{})", matrix[0][0], matrix[0][1], matrix[1][0], matrix[1][1], matrix[0][3], matrix[1][3], origin.x(), origin.y());
        });
}

NonnullRefPtr<ClipEffectContextTree> ClipEffectContextTree::create()
{
    auto context_tree = adopt_ref(*new ClipEffectContextTree());
    context_tree->m_nodes.append({ ClipData { {}, {} }, {}, 0, false });
    return context_tree;
}

ClipEffectContextIndex ClipEffectContextTree::append(ClipEffectContextData data, ClipEffectContextIndex parent_index)
{
    size_t depth = parent_index.value() ? m_nodes[parent_index.value()].depth + 1 : 1;

    bool empty_clip = false;
    if (parent_index.value() && m_nodes[parent_index.value()].has_empty_effective_clip) {
        empty_clip = true;
    } else if (data.has<ClipData>()) {
        empty_clip = data.get<ClipData>().rect.is_empty();
    } else if (data.has<ClipPathData>()) {
        empty_clip = data.get<ClipPathData>().path.bounding_box().is_empty();
    }

    auto index = ClipEffectContextIndex(m_nodes.size());
    m_nodes.append({ move(data), parent_index, depth, empty_clip });
    return index;
}

ClipEffectContextIndex ClipEffectContextTree::find_common_ancestor(ClipEffectContextIndex a, ClipEffectContextIndex b) const
{
    if (!a.value() || !b.value())
        return {};
    size_t a_index = a.value();
    size_t b_index = b.value();
    while (m_nodes[a_index].depth > m_nodes[b_index].depth)
        a_index = m_nodes[a_index].parent_index.value();
    while (m_nodes[b_index].depth > m_nodes[a_index].depth)
        b_index = m_nodes[b_index].parent_index.value();
    while (a_index != b_index) {
        a_index = m_nodes[a_index].parent_index.value();
        b_index = m_nodes[b_index].parent_index.value();
    }
    return ClipEffectContextIndex(a_index);
}

Vector<size_t, 8> ClipEffectContextTree::build_ancestor_chain(ClipEffectContextIndex index) const
{
    auto const& node = m_nodes[index.value()];
    Vector<size_t, 8> chain;
    chain.ensure_capacity(node.depth);
    for (size_t i = index.value(); i; i = m_nodes[i].parent_index.value())
        chain.append(i);
    return chain;
}

bool ClipEffectContextTree::contains_point(ClipEffectContextIndex index, DevicePixelPoint point, SpatialContextTree const& spatial_context_tree, ScrollStateSnapshot const& scroll_state) const
{
    if (!index.value())
        return true;

    auto chain = build_ancestor_chain(index);
    for (size_t i = chain.size(); i > 0; --i) {
        auto const& node = m_nodes[chain[i - 1]];
        auto contained = node.data.visit(
            [&](ClipData const& clip) {
                auto clip_point = spatial_context_tree.inverse_transform_point(clip.spatial_context_index, point.to_type<int>().to_type<float>(), scroll_state);
                return clip.contains(clip_point.to_type<int>().to_type<DevicePixels>());
            },
            [&](ClipPathData const& clip_path) {
                auto clip_point = spatial_context_tree.inverse_transform_point(clip_path.spatial_context_index, point.to_type<int>().to_type<float>(), scroll_state);
                auto clip_device_point = clip_point.to_type<int>().to_type<DevicePixels>();
                if (!clip_path.bounding_rect.contains(clip_device_point))
                    return false;
                return clip_path.path.contains(clip_point, clip_path.fill_rule);
            },
            [&](EffectsData const&) {
                return true;
            });

        if (!contained)
            return false;
    }

    return true;
}

void ClipEffectContextTree::dump(ClipEffectContextIndex index, StringBuilder& builder) const
{
    if (!index.value())
        return;

    auto const& node = m_nodes[index.value()];
    node.data.visit(
        [&](ClipData const& clip) {
            auto const& rect = clip.rect;
            builder.appendff("clip=[{},{} {}x{}]", rect.x(), rect.y(), rect.width(), rect.height());
            if (clip.spatial_context_index.value())
                builder.appendff(" spatial={}", clip.spatial_context_index.value());

            if (clip.corner_radii.has_any_radius()) {
                auto const& corner_radii = clip.corner_radii;
                builder.appendff(" radii=({},{},{},{})", corner_radii.top_left.horizontal_radius, corner_radii.top_right.horizontal_radius, corner_radii.bottom_right.horizontal_radius, corner_radii.bottom_left.horizontal_radius);
            }
        },
        [&](ClipPathData const& clip_path) {
            auto const& rect = clip_path.bounding_rect;
            builder.appendff("clip_path=[bounds: {},{} {}x{}, path: {}]", rect.x(), rect.y(), rect.width(), rect.height(), clip_path.path.to_svg_string());
            if (clip_path.spatial_context_index.value())
                builder.appendff(" spatial={}", clip_path.spatial_context_index.value());
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
            if (effects.gfx_filter.has_value()) {
                if (has_content)
                    builder.append(' ');
                builder.append("filter"sv);
                has_content = true;
            }
            builder.append("]"sv);
        });
}

Optional<Gfx::FloatPoint> transform_point_for_hit_test(SpatialContextTree const& spatial_context_tree, ClipEffectContextTree const& clip_effect_context_tree, PaintContext paint_context, Gfx::FloatPoint screen_point, ScrollStateSnapshot const& scroll_state)
{
    if (paint_context.clip_effect_context_index.value() && !clip_effect_context_tree.contains_point(paint_context.clip_effect_context_index, screen_point.to_type<int>().to_type<DevicePixels>(), spatial_context_tree, scroll_state))
        return {};
    return spatial_context_tree.inverse_transform_point(paint_context.spatial_context_index, screen_point, scroll_state);
}

}
