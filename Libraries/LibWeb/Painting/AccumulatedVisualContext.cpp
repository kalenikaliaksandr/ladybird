/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/StringBuilder.h>
#include <LibGfx/Matrix4x4.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

bool ClipData::contains(DevicePixelPoint point) const
{
    return corner_radii.contains(point.to_type<int>(), rect.to_type<int>());
}

static Atomic<u64> s_next_accumulated_visual_context_tree_version { 1 };

static TransformData identity_visual_viewport_transform()
{
    return { Gfx::FloatMatrix4x4::identity(), { 0.f, 0.f } };
}

AccumulatedVisualContextTree AccumulatedVisualContextTree::create()
{
    return create(identity_visual_viewport_transform());
}

AccumulatedVisualContextTree AccumulatedVisualContextTree::create(TransformData visual_viewport_transform)
{
    Vector<AccumulatedVisualContextNode> nodes;
    // Visual viewport transform root. This is identity for trees that are not attached to a document viewport.
    nodes.append({ move(visual_viewport_transform), {}, 0, false });
    return AccumulatedVisualContextTree {
        s_next_accumulated_visual_context_tree_version.fetch_add(1, AK::MemoryOrder::memory_order_relaxed),
        move(nodes)
    };
}

bool AccumulatedVisualContextTree::derive_has_empty_effective_clip(VisualContextData const& data, VisualContextIndex parent_index) const
{
    if (m_nodes[parent_index.value()].has_empty_effective_clip)
        return true;
    if (auto const* clip = data.get_pointer<ClipData>())
        return clip->rect.is_empty();
    if (auto const* clip_path = data.get_pointer<ClipPathData>())
        return clip_path->path.bounding_box().is_empty();
    return false;
}

VisualContextIndex AccumulatedVisualContextTree::append(VisualContextData data, VisualContextIndex parent_index)
{
    VERIFY(parent_index.value() < m_nodes.size());
    size_t depth = m_nodes[parent_index.value()].depth + 1;
    bool empty_clip = derive_has_empty_effective_clip(data, parent_index);

    auto index = VisualContextIndex(m_nodes.size());
    m_nodes.append({ move(data), parent_index, depth, empty_clip });
    return index;
}

VisualContextIndex AccumulatedVisualContextTree::allocate_node(VisualContextData data, VisualContextIndex parent_index)
{
    if (m_reusable_free_slots.is_empty())
        return append(move(data), parent_index);

    VERIFY(parent_index.value() < m_nodes.size());
    auto index = m_reusable_free_slots.take_last();
    auto& node = m_nodes[index.value()];
    VERIFY(node.data.has<FreeSlot>());
    node.has_empty_effective_clip = derive_has_empty_effective_clip(data, parent_index);
    node.data = move(data);
    node.parent_index = parent_index;
    node.depth = m_nodes[parent_index.value()].depth + 1;
    return index;
}

AccumulatedVisualContextTree::NodePatchResult AccumulatedVisualContextTree::patch_node(VisualContextIndex index, VisualContextData data, VisualContextIndex parent_index)
{
    VERIFY(parent_index.value() < m_nodes.size());
    auto& node = m_nodes[index.value()];
    VERIFY(!node.data.has<FreeSlot>());

    NodePatchResult result;
    auto empty_clip = derive_has_empty_effective_clip(data, parent_index);
    result.empty_effective_clip_flipped = node.has_empty_effective_clip != empty_clip;
    node.has_empty_effective_clip = empty_clip;
    node.data = move(data);
    if (node.parent_index != parent_index) {
        node.parent_index = parent_index;
        result.parent_or_depth_changed = true;
    }
    auto depth = m_nodes[parent_index.value()].depth + 1;
    if (node.depth != depth) {
        node.depth = depth;
        result.parent_or_depth_changed = true;
    }
    return result;
}

void AccumulatedVisualContextTree::free_node(VisualContextIndex index)
{
    VERIFY(index != VISUAL_VIEWPORT_NODE_INDEX);
    auto& node = m_nodes[index.value()];
    VERIFY(!node.data.has<FreeSlot>());
    node.data = FreeSlot {};
    node.parent_index = {};
    node.depth = 0;
    node.has_empty_effective_clip = false;
    m_quarantined_free_slots.append(index);
}

void AccumulatedVisualContextTree::promote_quarantined_free_slots()
{
    m_reusable_free_slots.extend(move(m_quarantined_free_slots));
}

void AccumulatedVisualContextTree::shrink(size_t node_count)
{
    // The visual viewport root node must always remain.
    VERIFY(node_count >= 1);
    VERIFY(node_count <= m_nodes.size());
    // Only tail-appended inspector overlay nodes are ever shrunk away; freed slots always sit
    // below the overlay watermark, so shrinking must never cut into the freelist.
    for (auto index : m_reusable_free_slots)
        VERIFY(index.value() < node_count);
    for (auto index : m_quarantined_free_slots)
        VERIFY(index.value() < node_count);
    m_nodes.shrink(node_count, true);
}

void AccumulatedVisualContextTree::set_visual_viewport_transform(TransformData transform)
{
    VERIFY(!m_nodes.is_empty());
    VERIFY(m_nodes[VISUAL_VIEWPORT_NODE_INDEX.value()].data.has<TransformData>());
    m_nodes[VISUAL_VIEWPORT_NODE_INDEX.value()].data = move(transform);
}

void AccumulatedVisualContextTree::mint_new_version()
{
    m_version = s_next_accumulated_visual_context_tree_version.fetch_add(1, AK::MemoryOrder::memory_order_relaxed);
}

VisualContextIndex AccumulatedVisualContextTree::find_common_ancestor(VisualContextIndex a, VisualContextIndex b) const
{
    VERIFY(a.value() < m_nodes.size());
    VERIFY(b.value() < m_nodes.size());
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
    return a_index;
}

Vector<size_t, 8> AccumulatedVisualContextTree::build_ancestor_chain(VisualContextIndex index) const
{
    VERIFY(index.value() < m_nodes.size());
    auto const& node = m_nodes[index.value()];
    Vector<size_t, 8> chain;
    chain.ensure_capacity(node.depth + 1);
    for (size_t i = index.value();; i = m_nodes[i].parent_index.value()) {
        chain.append(i);
        if (i == VISUAL_VIEWPORT_NODE_INDEX.value())
            break;
    }
    return chain;
}

Optional<Gfx::FloatPoint> AccumulatedVisualContextTree::transform_point_for_hit_test(VisualContextIndex index, Gfx::FloatPoint screen_point, ScrollStateSnapshot const& scroll_state, ClipBehavior clip_behavior) const
{
    auto chain = build_ancestor_chain(index);

    auto point = screen_point;
    for (size_t i = chain.size(); i > 0; --i) {
        auto node_index = VisualContextIndex { chain[i - 1] };
        auto const& node = m_nodes[node_index.value()];

        auto result = node.data.visit(
            [&](PerspectiveData const& perspective) -> Optional<Gfx::FloatPoint> {
                auto affine = Gfx::extract_2d_affine_transform(perspective.matrix);
                auto inverse = affine.inverse();
                if (!inverse.has_value())
                    return {};
                point = inverse->map(point);
                return point;
            },
            [&](ScrollData const&) -> Optional<Gfx::FloatPoint> {
                point.translate_by(-scroll_state.device_offset_for_index(node_index));
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
            },
            [&](ClipData const& clip) -> Optional<Gfx::FloatPoint> {
                if (clip_behavior == ClipBehavior::Ignore)
                    return point;
                // NOTE: The clip rect is in absolute device-pixel coordinates. After inverse-transforming, `point`
                //       is also in device-pixel coordinates, so we compare them directly.
                if (!clip.contains(point.to_type<int>().to_type<DevicePixels>()))
                    return {};
                return point;
            },
            [&](ClipPathData const& clip_path) -> Optional<Gfx::FloatPoint> {
                if (clip_behavior == ClipBehavior::Ignore)
                    return point;
                // NOTE: The clip path is in absolute device-pixel coordinates. After inverse-transforming, `point`
                //       is also in device-pixel coordinates, so we compare them directly.
                if (!clip_path.bounding_rect.contains(point.to_type<int>().to_type<DevicePixels>()))
                    return {};
                if (!clip_path.path.contains(point, clip_path.fill_rule))
                    return {};
                return point;
            },
            [&](EffectsData const&) -> Optional<Gfx::FloatPoint> {
                // Effects don't affect coordinate transforms
                return point;
            },
            [&](ScrollCompensation const& compensation) -> Optional<Gfx::FloatPoint> {
                auto offset = scroll_state.device_offset_for_index(compensation.scroll_node_index);
                point.translate_by(compensation.negate ? offset : -offset);
                return point;
            },
            [&](AnchorScrollShift const& shift) -> Optional<Gfx::FloatPoint> {
                point.translate_by(-shift.masked_offset(scroll_state));
                return point;
            },
            [&](FreeSlot const&) -> Optional<Gfx::FloatPoint> {
                VERIFY_NOT_REACHED();
            });

        if (!result.has_value())
            return {};
    }

    return point;
}

Gfx::FloatPoint AccumulatedVisualContextTree::inverse_transform_point(VisualContextIndex index, Gfx::FloatPoint screen_point) const
{
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
            [&](TransformData const& transform) {
                auto affine = Gfx::extract_2d_affine_transform(transform.matrix);
                auto inverse = affine.inverse();
                if (inverse.has_value()) {
                    auto offset_point = point - transform.origin;
                    auto transformed = inverse->map(offset_point);
                    point = transformed + transform.origin;
                }
            },
            [&](FreeSlot const&) { VERIFY_NOT_REACHED(); },
            [&](auto const&) {});
    }

    return point;
}

Gfx::FloatRect AccumulatedVisualContextTree::transform_rect_to_viewport(VisualContextIndex index, Gfx::FloatRect const& source_rect, ScrollStateSnapshot const& scroll_state, IncludeVisualViewportTransform include_visual_viewport_transform) const
{
    auto rect = source_rect;
    for (size_t i = index.value();; i = m_nodes[i].parent_index.value()) {
        auto const& node = m_nodes[i];
        if (i != VISUAL_VIEWPORT_NODE_INDEX.value() || include_visual_viewport_transform == IncludeVisualViewportTransform::Yes) {
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
                [&](ScrollData const&) {
                    rect.translate_by(scroll_state.device_offset_for_index(VisualContextIndex { i }));
                },
                [&](ScrollCompensation const& compensation) {
                    auto offset = scroll_state.device_offset_for_index(compensation.scroll_node_index);
                    rect.translate_by(compensation.negate ? -offset : offset);
                },
                [&](AnchorScrollShift const& shift) {
                    rect.translate_by(shift.masked_offset(scroll_state));
                },
                [&](ClipData const&) { /* clips don't affect rect coordinates */ },
                [&](ClipPathData const&) { /* clip paths don't affect rect coordinates */ },
                [&](EffectsData const&) { /* effects don't affect rect coordinates */ },
                [&](FreeSlot const&) { VERIFY_NOT_REACHED(); });
        }
        if (i == VISUAL_VIEWPORT_NODE_INDEX.value())
            break;
    }

    return rect;
}

Gfx::FloatPoint AnchorScrollShift::masked_offset(ScrollStateSnapshot const& scroll_state) const
{
    auto offset = scroll_state.device_offset_for_index(scroll_node_index);
    if (!compensate_horizontal_scroll)
        offset.set_x(0);
    if (!compensate_vertical_scroll)
        offset.set_y(0);
    return negate ? -offset : offset;
}

void AccumulatedVisualContextTree::dump(VisualContextIndex index, StringBuilder& builder) const
{
    auto const& node = m_nodes[index.value()];
    node.data.visit(
        [&](PerspectiveData const&) {
            builder.append("perspective"sv);
        },
        [&](ScrollData const& scroll) {
            builder.append("scroll"sv);
            if (scroll.is_sticky)
                builder.append(" (sticky)"sv);
        },
        [&](TransformData const& transform) {
            auto const& matrix = transform.matrix.elements();
            auto const& origin = transform.origin;
            builder.appendff("transform=[{},{},{},{},{},{}] origin=({},{})", matrix[0][0], matrix[0][1], matrix[1][0], matrix[1][1], matrix[0][3], matrix[1][3], origin.x(), origin.y());
        },
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
        },
        [&](ScrollCompensation const& compensation) {
            builder.appendff("scroll_compensation(node_index={}{})", compensation.scroll_node_index.value(), compensation.negate ? ""sv : ", applies_offset"sv);
        },
        [&](AnchorScrollShift const& shift) {
            builder.appendff("anchor_scroll_shift(node_index={}{}{}{})", shift.scroll_node_index.value(),
                shift.negate ? ", negate"sv : ""sv,
                shift.compensate_horizontal_scroll ? ""sv : ", no-x"sv,
                shift.compensate_vertical_scroll ? ""sv : ", no-y"sv);
        },
        [&](FreeSlot const&) {
            builder.append("free"sv);
        });
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollData const& data)
{
    TRY(encoder.encode(data.is_sticky));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollData> decode(Decoder& decoder)
{
    return Web::Painting::ScrollData {
        .is_sticky = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ClipData const& data)
{
    TRY(encoder.encode(data.rect));
    TRY(encoder.encode(data.corner_radii));
    return {};
}

template<>
ErrorOr<Web::Painting::ClipData> decode(Decoder& decoder)
{
    return Web::Painting::ClipData {
        TRY(decoder.decode<Web::DevicePixelRect>()),
        TRY(decoder.decode<Gfx::CornerRadii>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::TransformData const& data)
{
    TRY(encoder.encode(data.matrix));
    TRY(encoder.encode(data.origin));
    return {};
}

template<>
ErrorOr<Web::Painting::TransformData> decode(Decoder& decoder)
{
    return Web::Painting::TransformData {
        .matrix = TRY(decoder.decode<Gfx::FloatMatrix4x4>()),
        .origin = TRY(decoder.decode<Gfx::FloatPoint>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::PerspectiveData const& data)
{
    TRY(encoder.encode(data.matrix));
    return {};
}

template<>
ErrorOr<Web::Painting::PerspectiveData> decode(Decoder& decoder)
{
    return Web::Painting::PerspectiveData {
        .matrix = TRY(decoder.decode<Gfx::FloatMatrix4x4>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ClipPathData const& data)
{
    TRY(encoder.encode(data.path));
    TRY(encoder.encode(data.bounding_rect));
    TRY(encoder.encode(data.fill_rule));
    return {};
}

template<>
ErrorOr<Web::Painting::ClipPathData> decode(Decoder& decoder)
{
    return Web::Painting::ClipPathData {
        .path = TRY(decoder.decode<Gfx::Path>()),
        .bounding_rect = TRY(decoder.decode<Web::DevicePixelRect>()),
        .fill_rule = TRY(decoder.decode<Gfx::WindingRule>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::EffectsData const& data)
{
    TRY(encoder.encode(data.opacity));
    TRY(encoder.encode(data.blend_mode));
    TRY(encoder.encode(data.gfx_filter));
    return {};
}

template<>
ErrorOr<Web::Painting::EffectsData> decode(Decoder& decoder)
{
    return Web::Painting::EffectsData {
        .opacity = TRY(decoder.decode<float>()),
        .blend_mode = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
        .gfx_filter = TRY(decoder.decode<Optional<Gfx::Filter>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollCompensation const& data)
{
    TRY(encoder.encode(data.scroll_node_index));
    TRY(encoder.encode(data.negate));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollCompensation> decode(Decoder& decoder)
{
    return Web::Painting::ScrollCompensation {
        .scroll_node_index = TRY(decoder.decode<Web::Painting::VisualContextIndex>()),
        .negate = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::AnchorScrollShift const& data)
{
    TRY(encoder.encode(data.scroll_node_index));
    TRY(encoder.encode(data.negate));
    TRY(encoder.encode(data.compensate_horizontal_scroll));
    TRY(encoder.encode(data.compensate_vertical_scroll));
    return {};
}

template<>
ErrorOr<void> encode(Encoder&, Web::Painting::FreeSlot const&)
{
    return {};
}

template<>
ErrorOr<Web::Painting::FreeSlot> decode(Decoder&)
{
    return Web::Painting::FreeSlot {};
}

template<>
ErrorOr<Web::Painting::AnchorScrollShift> decode(Decoder& decoder)
{
    return Web::Painting::AnchorScrollShift {
        .scroll_node_index = TRY(decoder.decode<Web::Painting::VisualContextIndex>()),
        .negate = TRY(decoder.decode<bool>()),
        .compensate_horizontal_scroll = TRY(decoder.decode<bool>()),
        .compensate_vertical_scroll = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::AccumulatedVisualContextNode const& node)
{
    TRY(encoder.encode(node.data));
    TRY(encoder.encode(node.parent_index));
    TRY(encoder.encode(node.depth));
    TRY(encoder.encode(node.has_empty_effective_clip));
    return {};
}

template<>
ErrorOr<Web::Painting::AccumulatedVisualContextNode> decode(Decoder& decoder)
{
    return Web::Painting::AccumulatedVisualContextNode {
        .data = TRY(decoder.decode<Web::Painting::VisualContextData>()),
        .parent_index = TRY(decoder.decode<Web::Painting::VisualContextIndex>()),
        .depth = TRY(decoder.decode<size_t>()),
        .has_empty_effective_clip = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::AccumulatedVisualContextTree const& tree)
{
    TRY(encoder.encode(tree.m_version));
    TRY(encoder.encode(tree.m_nodes));
    return {};
}

template<>
ErrorOr<Web::Painting::AccumulatedVisualContextTree> decode(Decoder& decoder)
{
    auto version = TRY(decoder.decode<u64>());
    auto nodes = TRY(decoder.decode<Vector<Web::Painting::AccumulatedVisualContextNode>>());
    if (nodes.is_empty())
        return Error::from_string_literal("IPC decode: AccumulatedVisualContextTree missing visual viewport node");
    if (!nodes[Web::Painting::VISUAL_VIEWPORT_NODE_INDEX.value()].data.has<Web::Painting::TransformData>())
        return Error::from_string_literal("IPC decode: AccumulatedVisualContextTree visual viewport node is not a transform");
    return Web::Painting::AccumulatedVisualContextTree { version, move(nodes) };
}

}
