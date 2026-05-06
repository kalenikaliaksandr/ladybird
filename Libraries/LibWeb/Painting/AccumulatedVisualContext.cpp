/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/QuickSort.h>
#include <AK/StringBuilder.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

bool ClipData::contains(DevicePixelPoint point) const
{
    return corner_radii.contains(point.to_type<int>(), rect.to_type<int>());
}

NonnullRefPtr<AccumulatedVisualContextTree> AccumulatedVisualContextTree::create()
{
    auto tree = adopt_ref(*new AccumulatedVisualContextTree());
    // Sentinels at index 0 (null contexts). Data values don't matter; they're never accessed.
    tree->m_spatial_nodes.append({ ScrollData { {}, false }, {}, 0, 0 });
    tree->m_clip_nodes.append({ ClipData { {}, {} }, {}, 0, 0, false });
    tree->m_effect_nodes.append({ EffectsData {}, {}, 0, 0 });
    return tree;
}

SpatialContextIndex AccumulatedVisualContextTree::append_spatial(SpatialContextData data, SpatialContextIndex parent_index)
{
    size_t depth = parent_index.value() ? m_spatial_nodes[parent_index.value()].depth + 1 : 1;
    auto index = SpatialContextIndex(m_spatial_nodes.size());
    m_spatial_nodes.append({ move(data), parent_index, depth, m_next_sequence_number++ });
    return index;
}

ClipContextIndex AccumulatedVisualContextTree::append_clip(ClipContextData data, ClipContextIndex parent_index)
{
    size_t depth = parent_index.value() ? m_clip_nodes[parent_index.value()].depth + 1 : 1;

    bool empty_clip = false;
    if (parent_index.value() && m_clip_nodes[parent_index.value()].has_empty_effective_clip) {
        empty_clip = true;
    } else if (data.has<ClipData>()) {
        empty_clip = data.get<ClipData>().rect.is_empty();
    } else if (data.has<ClipPathData>()) {
        empty_clip = data.get<ClipPathData>().path.bounding_box().is_empty();
    }

    auto index = ClipContextIndex(m_clip_nodes.size());
    m_clip_nodes.append({ move(data), parent_index, depth, m_next_sequence_number++, empty_clip });
    return index;
}

EffectContextIndex AccumulatedVisualContextTree::append_effect(EffectsData data, EffectContextIndex parent_index)
{
    size_t depth = parent_index.value() ? m_effect_nodes[parent_index.value()].depth + 1 : 1;
    auto index = EffectContextIndex(m_effect_nodes.size());
    m_effect_nodes.append({ move(data), parent_index, depth, m_next_sequence_number++ });
    return index;
}

template<typename Index, typename NodeVector>
static Index find_common_ancestor_impl(NodeVector const& nodes, Index a, Index b)
{
    if (!a.value() || !b.value())
        return {};
    size_t a_index = a.value();
    size_t b_index = b.value();
    while (nodes[a_index].depth > nodes[b_index].depth)
        a_index = nodes[a_index].parent_index.value();
    while (nodes[b_index].depth > nodes[a_index].depth)
        b_index = nodes[b_index].parent_index.value();
    while (a_index != b_index) {
        a_index = nodes[a_index].parent_index.value();
        b_index = nodes[b_index].parent_index.value();
    }
    return Index(a_index);
}

SpatialContextIndex AccumulatedVisualContextTree::find_common_ancestor(SpatialContextIndex a, SpatialContextIndex b) const
{
    return find_common_ancestor_impl(m_spatial_nodes, a, b);
}

ClipContextIndex AccumulatedVisualContextTree::find_common_ancestor(ClipContextIndex a, ClipContextIndex b) const
{
    return find_common_ancestor_impl(m_clip_nodes, a, b);
}

EffectContextIndex AccumulatedVisualContextTree::find_common_ancestor(EffectContextIndex a, EffectContextIndex b) const
{
    return find_common_ancestor_impl(m_effect_nodes, a, b);
}

template<typename Index, typename NodeVector>
static Vector<size_t, 8> build_ancestor_chain_impl(NodeVector const& nodes, Index index)
{
    auto const& node = nodes[index.value()];
    Vector<size_t, 8> chain;
    chain.ensure_capacity(node.depth);
    for (size_t i = index.value(); i; i = nodes[i].parent_index.value())
        chain.append(i);
    return chain;
}

Vector<size_t, 8> AccumulatedVisualContextTree::build_ancestor_chain(SpatialContextIndex index) const
{
    return build_ancestor_chain_impl(m_spatial_nodes, index);
}

Vector<size_t, 8> AccumulatedVisualContextTree::build_ancestor_chain(ClipContextIndex index) const
{
    return build_ancestor_chain_impl(m_clip_nodes, index);
}

Vector<size_t, 8> AccumulatedVisualContextTree::build_ancestor_chain(EffectContextIndex index) const
{
    return build_ancestor_chain_impl(m_effect_nodes, index);
}

Vector<OrderedVisualContextNode, 16> AccumulatedVisualContextTree::build_ordered_context_chain(VisualContextIndex state) const
{
    Vector<OrderedVisualContextNode, 16> chain;

    if (state.spatial.value()) {
        for (auto index : build_ancestor_chain(state.spatial)) {
            auto const& node = m_spatial_nodes[index];
            chain.append({ OrderedVisualContextNode::Type::Spatial, index, node.sequence_number });
        }
    }
    if (state.clip.value()) {
        for (auto index : build_ancestor_chain(state.clip)) {
            auto const& node = m_clip_nodes[index];
            chain.append({ OrderedVisualContextNode::Type::Clip, index, node.sequence_number });
        }
    }
    if (state.effect.value()) {
        for (auto index : build_ancestor_chain(state.effect)) {
            auto const& node = m_effect_nodes[index];
            chain.append({ OrderedVisualContextNode::Type::Effect, index, node.sequence_number });
        }
    }

    quick_sort(chain, [](auto const& a, auto const& b) {
        return a.sequence_number < b.sequence_number;
    });
    return chain;
}

Optional<Gfx::FloatPoint> AccumulatedVisualContextTree::transform_point_for_hit_test(VisualContextIndex state, Gfx::FloatPoint screen_point, ScrollStateSnapshot const& scroll_state) const
{
    if (state.is_empty())
        return screen_point;

    auto chain = build_ordered_context_chain(state);

    auto point = screen_point;
    for (auto const& ordered_node : chain) {
        Optional<Gfx::FloatPoint> result = point;
        switch (ordered_node.type) {
        case OrderedVisualContextNode::Type::Spatial:
            result = m_spatial_nodes[ordered_node.index].data.visit(
                [&](PerspectiveData const& perspective) -> Optional<Gfx::FloatPoint> {
                    auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                    auto inverse = affine.inverse();
                    if (!inverse.has_value())
                        return {};
                    point = inverse->map(point);
                    return point;
                },
                [&](ScrollData const& scroll) -> Optional<Gfx::FloatPoint> {
                    point.translate_by(-scroll_state.device_offset_for_index(scroll.scroll_frame_index));
                    return point;
                },
                [&](TransformData const& transform) -> Optional<Gfx::FloatPoint> {
                    auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                    auto inverse = affine.inverse();
                    if (!inverse.has_value())
                        return {};

                    auto offset_point = point - transform.origin;
                    auto transformed = inverse->map(offset_point);
                    point = transformed + transform.origin;
                    return point;
                });
            break;
        case OrderedVisualContextNode::Type::Clip:
            result = m_clip_nodes[ordered_node.index].data.visit(
                [&](ClipData const& clip) -> Optional<Gfx::FloatPoint> {
                    // NOTE: The clip rect is in absolute device-pixel coordinates. After inverse-transforming, `point`
                    //       is also in device-pixel coordinates, so we compare them directly.
                    if (!clip.contains(point.to_type<int>().to_type<DevicePixels>()))
                        return {};
                    return point;
                },
                [&](ClipPathData const& clip_path) -> Optional<Gfx::FloatPoint> {
                    // NOTE: The clip path is in absolute device-pixel coordinates. After inverse-transforming, `point`
                    //       is also in device-pixel coordinates, so we compare them directly.
                    if (!clip_path.bounding_rect.contains(point.to_type<int>().to_type<DevicePixels>()))
                        return {};
                    if (!clip_path.path.contains(point, clip_path.fill_rule))
                        return {};
                    return point;
                });
            break;
        case OrderedVisualContextNode::Type::Effect:
            // Effects don't affect hit-test coordinate transforms.
            break;
        }

        if (!result.has_value())
            return {};
    }

    return point;
}

Gfx::FloatPoint AccumulatedVisualContextTree::inverse_transform_point(SpatialContextIndex index, Gfx::FloatPoint screen_point) const
{
    if (!index.value())
        return screen_point;

    auto chain = build_ancestor_chain(index);

    auto point = screen_point;
    for (size_t i = chain.size(); i > 0; --i) {
        auto const& node = m_spatial_nodes[chain[i - 1]];

        node.data.visit(
            [&](PerspectiveData const& perspective) {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                auto inverse = affine.inverse();
                if (inverse.has_value())
                    point = inverse->map(point);
            },
            [&](TransformData const& transform) {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                auto inverse = affine.inverse();
                if (inverse.has_value()) {
                    auto offset_point = point - transform.origin;
                    auto transformed = inverse->map(offset_point);
                    point = transformed + transform.origin;
                }
            },
            [&](ScrollData const&) {});
    }

    return point;
}

Gfx::FloatRect AccumulatedVisualContextTree::transform_rect_to_viewport(SpatialContextIndex index, Gfx::FloatRect const& source_rect, ScrollStateSnapshot const& scroll_state) const
{
    if (!index.value())
        return source_rect;

    auto rect = source_rect;
    for (size_t i = index.value(); i; i = m_spatial_nodes[i].parent_index.value()) {
        auto const& node = m_spatial_nodes[i];
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

void AccumulatedVisualContextTree::dump(SpatialContextIndex index, StringBuilder& builder) const
{
    if (!index.value())
        return;

    auto const& node = m_spatial_nodes[index.value()];
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

void AccumulatedVisualContextTree::dump(ClipContextIndex index, StringBuilder& builder) const
{
    if (!index.value())
        return;

    auto const& node = m_clip_nodes[index.value()];
    node.data.visit(
        [&](ClipData const& clip) {
            auto const& rect = clip.rect;
            builder.appendff("clip=[{},{} {}x{}]", rect.x(), rect.y(), rect.width(), rect.height());

            if (clip.corner_radii.has_any_radius()) {
                auto const& corner_radii = clip.corner_radii;
                builder.appendff(" radii=({},{},{},{})", corner_radii.top_left.horizontal_radius, corner_radii.top_right.horizontal_radius, corner_radii.bottom_right.horizontal_radius, corner_radii.bottom_left.horizontal_radius);
            }
        },
        [&](ClipPathData const& clip_path) {
            auto const& rect = clip_path.bounding_rect;
            builder.appendff("clip_path=[bounds: {},{} {}x{}, path: {}]", rect.x(), rect.y(), rect.width(), rect.height(), clip_path.path.to_svg_string());
        });
}

void AccumulatedVisualContextTree::dump(EffectContextIndex index, StringBuilder& builder) const
{
    if (!index.value())
        return;

    auto const& effects = m_effect_nodes[index.value()].data;
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
}

}
