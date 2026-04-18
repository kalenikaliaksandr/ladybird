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
AK_TYPEDEF_DISTINCT_ORDERED_ID(size_t, ClipEffectContextIndex);

struct PaintContext {
    SpatialContextIndex spatial_context_index {};
    ClipEffectContextIndex clip_effect_context_index {};

    bool operator==(PaintContext const&) const = default;
};

struct ScrollData {
    ScrollFrameIndex scroll_frame_index;
    bool is_sticky;
};

struct ClipData {
    DevicePixelRect rect;
    CornerRadii corner_radii;
    SpatialContextIndex spatial_context_index {};

    ClipData(DevicePixelRect r, CornerRadii radii, SpatialContextIndex spatial_context = {})
        : rect(r)
        , corner_radii(radii)
        , spatial_context_index(spatial_context)
    {
    }

    bool contains(DevicePixelPoint point) const;
};

struct TransformData {
    Gfx::FloatMatrix4x4 matrix;
    Gfx::FloatPoint origin;
};

struct PerspectiveData {
    Gfx::FloatMatrix4x4 matrix;
};

struct ClipPathData {
    Gfx::Path path;
    DevicePixelRect bounding_rect;
    Gfx::WindingRule fill_rule;
    SpatialContextIndex spatial_context_index {};
};

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

using SpatialContextData = Variant<ScrollData, TransformData, PerspectiveData>;
using ClipEffectContextData = Variant<ClipData, ClipPathData, EffectsData>;

struct SpatialContextNode {
    SpatialContextData data;
    SpatialContextIndex parent_index {};
    size_t depth { 0 };
};

struct ClipEffectContextNode {
    ClipEffectContextData data;
    ClipEffectContextIndex parent_index {};
    size_t depth { 0 };
    bool has_empty_effective_clip { false };
};

class SpatialContextTree : public AtomicRefCounted<SpatialContextTree> {
public:
    static NonnullRefPtr<SpatialContextTree> create();

    SpatialContextIndex append(SpatialContextData data, SpatialContextIndex parent_index);

    SpatialContextNode const& node_at(SpatialContextIndex index) const { return m_nodes[index.value()]; }

    SpatialContextIndex find_common_ancestor(SpatialContextIndex a, SpatialContextIndex b) const;
    Gfx::FloatPoint inverse_transform_point(SpatialContextIndex, Gfx::FloatPoint, ScrollStateSnapshot const&) const;
    Gfx::FloatRect transform_rect_to_viewport(SpatialContextIndex, Gfx::FloatRect const&, ScrollStateSnapshot const&) const;
    Gfx::FloatMatrix4x4 transform_matrix_to_viewport(SpatialContextIndex, ScrollStateSnapshot const&) const;
    void dump(SpatialContextIndex, StringBuilder&) const;

private:
    SpatialContextTree() = default;

    Vector<size_t, 8> build_ancestor_chain(SpatialContextIndex index) const;

    Vector<SpatialContextNode> m_nodes;
};

class ClipEffectContextTree : public AtomicRefCounted<ClipEffectContextTree> {
public:
    static NonnullRefPtr<ClipEffectContextTree> create();

    ClipEffectContextIndex append(ClipEffectContextData data, ClipEffectContextIndex parent_index);

    ClipEffectContextNode const& node_at(ClipEffectContextIndex index) const { return m_nodes[index.value()]; }

    ClipEffectContextIndex find_common_ancestor(ClipEffectContextIndex a, ClipEffectContextIndex b) const;
    bool has_empty_effective_clip(ClipEffectContextIndex i) const { return m_nodes[i.value()].has_empty_effective_clip; }
    bool is_effect(ClipEffectContextIndex i) const { return m_nodes[i.value()].data.has<EffectsData>(); }
    bool contains_point(ClipEffectContextIndex, DevicePixelPoint, SpatialContextTree const&, ScrollStateSnapshot const&) const;
    void dump(ClipEffectContextIndex, StringBuilder&) const;

private:
    ClipEffectContextTree() = default;

    Vector<size_t, 8> build_ancestor_chain(ClipEffectContextIndex index) const;

    Vector<ClipEffectContextNode> m_nodes;
};

Optional<Gfx::FloatPoint> transform_point_for_hit_test(SpatialContextTree const&, ClipEffectContextTree const&, PaintContext, Gfx::FloatPoint, ScrollStateSnapshot const&);

}
