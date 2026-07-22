/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/DistinctNumeric.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/CompositingAndBlendingOperator.h>
#include <LibGfx/CornerRadii.h>
#include <LibGfx/Filter.h>
#include <LibGfx/Matrix4x4.h>
#include <LibGfx/Path.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/WindingRule.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/ScrollNodeState.h>
#include <LibWeb/PixelUnits.h>

namespace Web::CSS {

class ComputedValues;

}

namespace Web::Painting {

class Paintable;
class ScrollStateSnapshot;

// The node's own VisualContextIndex keys the scroll offset snapshot; the paintable that owns the
// node and its sticky constraints live in the ScrollState entry addressed by state_slot, stamped
// at registration. The slot is process-local bookkeeping: it stays off the wire and takes no part
// in tree compatibility or damage comparisons.
struct ScrollData {
    bool is_sticky { false };
    ScrollStateSlot state_slot { NO_SCROLL_STATE_SLOT };
};

struct ClipData {
    DevicePixelRect rect;
    Gfx::CornerRadii corner_radii;

    ClipData(DevicePixelRect r, Gfx::CornerRadii radii)
        : rect(r)
        , corner_radii(radii)
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

// Translates by another scroll node's negated offset during display list replay, keeping fixed
// backgrounds stationary relative to the viewport regardless of scroll position.
struct ScrollCompensation {
    ScrollNodeIndex scroll_node_index;
};

// One scroll node's contribution to the default scroll shift of an anchor-positioned box, masked to the axes in which
// the box compensates for scroll. Nodes that move the box but not its default anchor contribute negated.
struct AnchorScrollShift {
    ScrollNodeIndex scroll_node_index;
    bool negate { false };
    bool compensate_horizontal_scroll { true };
    bool compensate_vertical_scroll { true };

    Gfx::FloatPoint masked_offset(ScrollStateSnapshot const&) const;
};

using VisualContextData = Variant<ScrollData, TransformData, PerspectiveData, ScrollCompensation, AnchorScrollShift>;

Optional<TransformData> compute_transform(Paintable const&, CSS::ComputedValues const&, double pixel_ratio);

struct AccumulatedVisualContextNode {
    VisualContextData data;
    VisualContextIndex parent_index {};
    size_t depth { 0 };
    // Position in the shared append order across the main, clip, and effect node lists; replay
    // interleaves the three chains by it.
    u32 sequence { 0 };
};

// Overflow clips, CSS `clip`, and clip-path live in their own node list. Each node pins the
// coordinate space its rect or path is expressed in via spatial_ref, decoupled from its
// parent_clip chain, which expresses pure set intersection.
struct ClipNode {
    Variant<ClipData, ClipPathData> data;
    ClipNodeIndex parent_index { ROOT_CLIP_NODE_INDEX };
    VisualContextIndex spatial_ref { VISUAL_VIEWPORT_NODE_INDEX };
    u32 depth { 0 };
    // Position in the shared append order across the main, clip, and effect node lists.
    u32 sequence { 0 };
    // True when this node's region or any ancestor's is empty; commands recorded under such a
    // chain are dropped at append time.
    bool has_empty_effective_clip { false };
};

// Opacity/blend/filter groups live in their own node list, referencing the main tree for the
// space their layer is established in and for the output-clip cut point: that clip and its
// ancestors apply outside the group's layer, deeper clips apply to the layer's contents.
struct EffectNode {
    EffectsData data;
    EffectNodeIndex parent_index { ROOT_EFFECT_NODE_INDEX };
    VisualContextIndex spatial_ref { VISUAL_VIEWPORT_NODE_INDEX };
    ClipNodeIndex clip_ref { ROOT_CLIP_NODE_INDEX };
    u32 depth { 0 };
    // Position in the shared append order across the main, clip, and effect node lists.
    u32 sequence { 0 };
};

class AccumulatedVisualContextTree {
public:
    enum class IncludeVisualViewportTransform {
        No,
        Yes,
    };

    enum class ClipBehavior {
        Respect,
        // Transform the point without rejecting it against clip rects and clip paths. Used when searching for the
        // closest caret position within a scope the point may lie entirely outside of.
        Ignore,
    };

    static WEB_API AccumulatedVisualContextTree create();
    static WEB_API AccumulatedVisualContextTree create(TransformData visual_viewport_transform);

    AccumulatedVisualContextTree(AccumulatedVisualContextTree const&) = default;
    AccumulatedVisualContextTree& operator=(AccumulatedVisualContextTree const&) = default;
    AccumulatedVisualContextTree(AccumulatedVisualContextTree&&) = default;
    AccumulatedVisualContextTree& operator=(AccumulatedVisualContextTree&&) = default;
    ~AccumulatedVisualContextTree() = default;

    u64 version() const { return m_version; }

    WEB_API VisualContextIndex append(VisualContextData data, VisualContextIndex parent_index);
    WEB_API EffectNodeIndex append_effect(EffectsData, EffectNodeIndex parent_index, VisualContextIndex spatial_ref, ClipNodeIndex clip_ref);
    WEB_API ClipNodeIndex append_clip(Variant<ClipData, ClipPathData>, ClipNodeIndex parent_index, VisualContextIndex spatial_ref);
    WEB_API void set_visual_viewport_transform(TransformData);
    WEB_API bool is_compatible_with(AccumulatedVisualContextTree const&) const;
    WEB_API void reuse_version_from(AccumulatedVisualContextTree const&);

    AccumulatedVisualContextNode const& node_at(VisualContextIndex index) const { return m_nodes[index.value()]; }
    AccumulatedVisualContextNode& node_at(VisualContextIndex index) { return m_nodes[index.value()]; }
    ReadonlySpan<AccumulatedVisualContextNode> nodes() const { return m_nodes.span(); }

    EffectNode const& effect_node_at(EffectNodeIndex index) const { return m_effect_nodes[index.value()]; }
    EffectNode& effect_node_at(EffectNodeIndex index) { return m_effect_nodes[index.value()]; }
    ReadonlySpan<EffectNode> effect_nodes() const { return m_effect_nodes.span(); }

    ClipNode const& clip_node_at(ClipNodeIndex index) const { return m_clip_nodes[index.value()]; }
    ClipNode& clip_node_at(ClipNodeIndex index) { return m_clip_nodes[index.value()]; }
    ReadonlySpan<ClipNode> clip_nodes() const { return m_clip_nodes.span(); }

    VisualContextIndex find_common_ancestor(VisualContextIndex a, VisualContextIndex b) const;
    Optional<Gfx::FloatPoint> transform_point_for_hit_test(VisualContextRefs, Gfx::FloatPoint, ScrollStateSnapshot const&, ClipBehavior = ClipBehavior::Respect) const;
    Gfx::FloatPoint inverse_transform_point(VisualContextRefs, Gfx::FloatPoint) const;
    Gfx::FloatRect transform_rect_to_viewport(VisualContextRefs, Gfx::FloatRect const&, ScrollStateSnapshot const&, IncludeVisualViewportTransform = IncludeVisualViewportTransform::Yes) const;
    void dump(VisualContextIndex, StringBuilder&) const;
    void dump_effect(EffectNodeIndex, StringBuilder&) const;
    void dump_clip(ClipNodeIndex, StringBuilder&) const;

    // With clips and effects in their own node lists, every main-tree node is spatial: the
    // spatial reference for a chain position is the position itself. The builder tracks clip and
    // effect contexts explicitly.

    bool has_empty_effective_clip(ClipNodeIndex i) const { return m_clip_nodes[i.value()].has_empty_effective_clip; }

    ScrollStateSlot scroll_state_slot_for_node(ScrollNodeIndex index) const
    {
        if (!index.value())
            return NO_SCROLL_STATE_SLOT;
        return m_nodes[index.value()].data.get<ScrollData>().state_slot;
    }

private:
    AccumulatedVisualContextTree(u64 version, Vector<AccumulatedVisualContextNode>&& nodes, Vector<EffectNode>&& effect_nodes, Vector<ClipNode>&& clip_nodes)
        : m_version(version)
        , m_nodes(move(nodes))
        , m_effect_nodes(move(effect_nodes))
        , m_clip_nodes(move(clip_nodes))
        , m_next_sequence(static_cast<u32>(m_nodes.size() + m_effect_nodes.size() + m_clip_nodes.size()))
    {
    }

    Vector<size_t, 8> build_ancestor_chain(VisualContextIndex index) const;

    u64 m_version { 0 };
    Vector<AccumulatedVisualContextNode> m_nodes;
    Vector<EffectNode> m_effect_nodes;
    Vector<ClipNode> m_clip_nodes;
    u32 m_next_sequence { 0 };

    template<typename T>
    friend ErrorOr<void> IPC::encode(IPC::Encoder&, T const&);
    template<typename T>
    friend ErrorOr<T> IPC::decode(IPC::Decoder&);
};

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ScrollData const&);
template<>
WEB_API ErrorOr<Web::Painting::ScrollData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ClipData const&);
template<>
WEB_API ErrorOr<Web::Painting::ClipData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::TransformData const&);
template<>
WEB_API ErrorOr<Web::Painting::TransformData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::PerspectiveData const&);
template<>
WEB_API ErrorOr<Web::Painting::PerspectiveData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ClipPathData const&);
template<>
WEB_API ErrorOr<Web::Painting::ClipPathData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::EffectsData const&);
template<>
WEB_API ErrorOr<Web::Painting::EffectsData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ScrollCompensation const&);
template<>
WEB_API ErrorOr<Web::Painting::ScrollCompensation> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::AnchorScrollShift const&);
template<>
WEB_API ErrorOr<Web::Painting::AnchorScrollShift> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::AccumulatedVisualContextNode const&);
template<>
WEB_API ErrorOr<Web::Painting::AccumulatedVisualContextNode> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::AccumulatedVisualContextTree const&);
template<>
WEB_API ErrorOr<Web::Painting::AccumulatedVisualContextTree> decode(Decoder&);

}
