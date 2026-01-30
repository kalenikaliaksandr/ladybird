/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/IntrusiveList.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGC/Weak.h>
#include <LibGfx/CompositingAndBlendingOperator.h>
#include <LibGfx/Matrix4x4.h>
#include <LibGfx/Path.h>
#include <LibGfx/WindingRule.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/BorderRadiiData.h>
#include <LibWeb/Painting/ResolvedCSSFilter.h>

namespace Web::Painting {

struct ClipRect {
    CSSPixelRect rect;
    BorderRadiiData corner_radii;
};

struct ScrollData {
    size_t scroll_frame_id;
    bool is_sticky;
};

struct ClipData {
    CSSPixelRect rect;
    BorderRadiiData corner_radii;

    explicit ClipData(ClipRect const& clip_rect)
        : rect(clip_rect.rect)
        , corner_radii(clip_rect.corner_radii)
    {
    }

    ClipData(CSSPixelRect r, BorderRadiiData radii)
        : rect(r)
        , corner_radii(radii)
    {
    }

    bool contains(CSSPixelPoint point) const;
};

struct TransformData {
    Gfx::FloatMatrix4x4 matrix;
    CSSPixelPoint origin;
};

struct PerspectiveData {
    Gfx::FloatMatrix4x4 matrix;
};

struct ClipPathData {
    Gfx::Path path;
    CSSPixelRect bounding_rect;
    Gfx::WindingRule fill_rule;
};

struct EffectsData {
    float opacity { 1.0f };
    Gfx::CompositingAndBlendingOperator blend_mode { Gfx::CompositingAndBlendingOperator::Normal };
    ResolvedCSSFilter filter;
    bool isolate { false };

    bool needs_layer() const
    {
        return opacity < 1.0f
            || blend_mode != Gfx::CompositingAndBlendingOperator::Normal
            || filter.has_filters()
            || isolate;
    }
};

using VisualContextData = Variant<ScrollData, ClipData, TransformData, PerspectiveData, ClipPathData, EffectsData>;

inline CSSPixelRect effective_css_clip_rect(CSSPixelRect const& css_clip)
{
    if (css_clip.width() < 0 || css_clip.height() < 0)
        return CSSPixelRect { 0, 0, 0, 0 };
    return css_clip;
}

class AccumulatedVisualContextSnapshot {
public:
    VisualContextData const& visual_context_data(size_t id) const
    {
        VERIFY(id < m_context_data.size());
        return m_context_data[id];
    }

    CSSPixelPoint scroll_offset_for_frame_id(size_t id) const
    {
        if (id >= m_scroll_offsets.size())
            return {};
        return m_scroll_offsets[id];
    }

private:
    friend class AccumulatedVisualContext;
    friend class ScrollState;
    friend class ViewportPaintable;
    Vector<VisualContextData> m_context_data;
    Vector<CSSPixelPoint> m_scroll_offsets;
};

class AccumulatedVisualContext : public AtomicRefCounted<AccumulatedVisualContext> {
public:
    static NonnullRefPtr<AccumulatedVisualContext> create(size_t id, VisualContextData data, RefPtr<AccumulatedVisualContext const> parent, GC::Weak<PaintableBox const> source_paintable = {});

    RefPtr<AccumulatedVisualContext const> parent() const { return m_parent; }

    void refresh();
    void copy_data_into_snapshot(AccumulatedVisualContextSnapshot&) const;

    bool is_effect() const { return m_data.has<EffectsData>(); }

    size_t depth() const { return m_depth; }
    size_t id() const { return m_id; }

    void dump(StringBuilder&) const;

    Optional<CSSPixelPoint> transform_point_for_hit_test(CSSPixelPoint screen_point, AccumulatedVisualContextSnapshot const& snapshot) const;
    CSSPixelRect transform_rect_to_viewport(CSSPixelRect const&, AccumulatedVisualContextSnapshot const&) const;

private:
    AccumulatedVisualContext(size_t id, VisualContextData data, RefPtr<AccumulatedVisualContext const> parent, GC::Weak<PaintableBox const> source_paintable)
        : m_data(move(data))
        , m_parent(move(parent))
        , m_depth(m_parent ? m_parent->depth() + 1 : 1)
        , m_id(id)
        , m_source_paintable(move(source_paintable))
    {
    }

    VisualContextData m_data;
    RefPtr<AccumulatedVisualContext const> m_parent;
    size_t m_depth;
    size_t m_id;
    GC::Weak<PaintableBox const> m_source_paintable;
    mutable IntrusiveListNode<AccumulatedVisualContext> m_list_node;

public:
    using List = IntrusiveList<&AccumulatedVisualContext::m_list_node>;
};

}
