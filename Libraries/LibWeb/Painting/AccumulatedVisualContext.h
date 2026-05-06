/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/DistinctNumeric.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/CompositingAndBlendingOperator.h>
#include <LibGfx/Filter.h>
#include <LibGfx/Matrix4x4.h>
#include <LibGfx/Path.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/WindingRule.h>
#include <LibWeb/Painting/BorderRadiiData.h>
#include <LibWeb/Painting/ScrollFrame.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Painting {

class ScrollStateSnapshot;

AK_TYPEDEF_DISTINCT_ORDERED_ID(size_t, SpatialContextIndex);
AK_TYPEDEF_DISTINCT_ORDERED_ID(size_t, ClipContextIndex);
AK_TYPEDEF_DISTINCT_ORDERED_ID(size_t, EffectContextIndex);

struct VisualContextIndex {
    SpatialContextIndex spatial {};
    ClipContextIndex clip {};
    EffectContextIndex effect {};

    bool is_empty() const { return !spatial.value() && !clip.value() && !effect.value(); }
    bool operator==(VisualContextIndex const&) const = default;
};

struct ScrollData {
    ScrollFrameIndex scroll_frame_index;
    bool is_sticky;
};

struct TransformData {
    Gfx::FloatMatrix4x4 matrix;
    Gfx::FloatPoint origin;
};

struct PerspectiveData {
    Gfx::FloatMatrix4x4 matrix;
};

using SpatialContextData = Variant<ScrollData, TransformData, PerspectiveData>;

struct ClipData {
    DevicePixelRect rect;
    CornerRadii corner_radii;

    ClipData(DevicePixelRect r, CornerRadii radii)
        : rect(r)
        , corner_radii(radii)
    {
    }

    bool contains(DevicePixelPoint point) const;
};

struct ClipPathData {
    Gfx::Path path;
    DevicePixelRect bounding_rect;
    Gfx::WindingRule fill_rule;
};

using ClipContextData = Variant<ClipData, ClipPathData>;

struct EffectsData {
    float opacity { 1.0f };
    Gfx::CompositingAndBlendingOperator blend_mode { Gfx::CompositingAndBlendingOperator::Normal };
    Optional<Gfx::Filter> gfx_filter;

    bool needs_layer() const
    {
        return opacity < 1.0f
            || blend_mode != Gfx::CompositingAndBlendingOperator::Normal
            || gfx_filter.has_value();
    }
};

struct SpatialContextNode {
    SpatialContextData data;
    SpatialContextIndex parent_index {};
    size_t depth { 0 };
    size_t sequence_number { 0 };
};

struct ClipContextNode {
    ClipContextData data;
    ClipContextIndex parent_index {};
    size_t depth { 0 };
    size_t sequence_number { 0 };
    bool has_empty_effective_clip { false };
};

struct EffectContextNode {
    EffectsData data;
    EffectContextIndex parent_index {};
    size_t depth { 0 };
    size_t sequence_number { 0 };
};

struct OrderedVisualContextNode {
    enum class Type : u8 {
        Spatial,
        Clip,
        Effect,
    };

    Type type;
    size_t index { 0 };
    size_t sequence_number { 0 };

    bool operator==(OrderedVisualContextNode const&) const = default;
};

class AccumulatedVisualContextTree : public AtomicRefCounted<AccumulatedVisualContextTree> {
public:
    static NonnullRefPtr<AccumulatedVisualContextTree> create();

    SpatialContextIndex append_spatial(SpatialContextData, SpatialContextIndex parent_index);
    ClipContextIndex append_clip(ClipContextData, ClipContextIndex parent_index);
    EffectContextIndex append_effect(EffectsData, EffectContextIndex parent_index);

    SpatialContextNode const& node_at(SpatialContextIndex index) const { return m_spatial_nodes[index.value()]; }
    ClipContextNode const& node_at(ClipContextIndex index) const { return m_clip_nodes[index.value()]; }
    EffectContextNode const& node_at(EffectContextIndex index) const { return m_effect_nodes[index.value()]; }

    SpatialContextIndex find_common_ancestor(SpatialContextIndex a, SpatialContextIndex b) const;
    ClipContextIndex find_common_ancestor(ClipContextIndex a, ClipContextIndex b) const;
    EffectContextIndex find_common_ancestor(EffectContextIndex a, EffectContextIndex b) const;

    Vector<OrderedVisualContextNode, 16> build_ordered_context_chain(VisualContextIndex) const;

    Optional<Gfx::FloatPoint> transform_point_for_hit_test(VisualContextIndex, Gfx::FloatPoint, ScrollStateSnapshot const&) const;
    Gfx::FloatPoint inverse_transform_point(SpatialContextIndex, Gfx::FloatPoint) const;
    Gfx::FloatRect transform_rect_to_viewport(SpatialContextIndex, Gfx::FloatRect const&, ScrollStateSnapshot const&) const;

    void dump(SpatialContextIndex, StringBuilder&) const;
    void dump(ClipContextIndex, StringBuilder&) const;
    void dump(EffectContextIndex, StringBuilder&) const;

    bool has_empty_effective_clip(ClipContextIndex i) const { return i.value() && m_clip_nodes[i.value()].has_empty_effective_clip; }
    bool has_empty_effective_clip(VisualContextIndex state) const { return has_empty_effective_clip(state.clip); }

private:
    AccumulatedVisualContextTree() = default;

    Vector<size_t, 8> build_ancestor_chain(SpatialContextIndex index) const;
    Vector<size_t, 8> build_ancestor_chain(ClipContextIndex index) const;
    Vector<size_t, 8> build_ancestor_chain(EffectContextIndex index) const;

    Vector<SpatialContextNode> m_spatial_nodes;
    Vector<ClipContextNode> m_clip_nodes;
    Vector<EffectContextNode> m_effect_nodes;
    size_t m_next_sequence_number { 1 };
};

}
