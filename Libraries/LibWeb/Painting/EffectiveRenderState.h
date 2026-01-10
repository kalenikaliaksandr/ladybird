/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/RefCounted.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/Painting/ClipFrame.h>
#include <LibWeb/Painting/ScrollState.h>
#include <LibWeb/Painting/TransformFrame.h>

namespace Web::Painting {

// Represents the cumulative rendering state (transforms, scroll offsets, clips) at a point in the paint tree.
// Nodes form a tree via parent pointers, enabling efficient sharing between paintables with common ancestors.
// During display list execution, the player walks this tree to apply/unapply effects as needed.
class EffectiveRenderState : public AtomicRefCounted<EffectiveRenderState> {
public:
    static NonnullRefPtr<EffectiveRenderState> create(
        RefPtr<TransformFrame const> transform_frame,
        Optional<size_t> scroll_frame_id,
        RefPtr<ClipFrame const> clip_frame,
        RefPtr<EffectiveRenderState const> parent,
        Optional<Gfx::FloatMatrix4x4> perspective = {})
    {
        return adopt_ref(*new EffectiveRenderState(move(transform_frame), scroll_frame_id, move(clip_frame), move(parent), move(perspective)));
    }

    RefPtr<TransformFrame const> transform_frame() const { return m_transform_frame; }
    Optional<size_t> scroll_frame_id() const { return m_scroll_frame_id; }
    RefPtr<ClipFrame const> clip_frame() const { return m_clip_frame; }
    RefPtr<EffectiveRenderState const> parent() const { return m_parent; }
    Optional<Gfx::FloatMatrix4x4> const& perspective() const { return m_perspective; }

    // Depth in the tree (1 for root node). Used for efficient LCA computation.
    size_t depth() const { return m_depth; }

    // For hit-testing: transforms a screen-space point to local coordinates by walking
    // the context tree and applying inverse transformations. Returns nullopt if the point
    // is outside any clip region or if a transform is singular (non-invertible).
    Optional<CSSPixelPoint> transform_point_for_hit_test(
        CSSPixelPoint screen_point,
        ScrollStateSnapshot const& scroll_state) const;

private:
    EffectiveRenderState(
        RefPtr<TransformFrame const> transform_frame,
        Optional<size_t> scroll_frame_id,
        RefPtr<ClipFrame const> clip_frame,
        RefPtr<EffectiveRenderState const> parent,
        Optional<Gfx::FloatMatrix4x4> perspective)
        : m_transform_frame(move(transform_frame))
        , m_scroll_frame_id(scroll_frame_id)
        , m_clip_frame(move(clip_frame))
        , m_parent(move(parent))
        , m_perspective(move(perspective))
        , m_depth(m_parent ? m_parent->depth() + 1 : 1)
    {
    }

    RefPtr<TransformFrame const> m_transform_frame;
    Optional<size_t> m_scroll_frame_id;
    RefPtr<ClipFrame const> m_clip_frame;
    RefPtr<EffectiveRenderState const> m_parent;
    Optional<Gfx::FloatMatrix4x4> m_perspective;
    size_t m_depth;
};

// Key for EffectiveRenderState deduplication in the node cache.
// States with identical keys can be shared between different paintables.
struct EffectiveRenderStateKey {
    RefPtr<TransformFrame const> transform_frame;
    Optional<size_t> scroll_frame_id;
    RefPtr<ClipFrame const> clip_frame;
    RefPtr<EffectiveRenderState const> parent;

    bool operator==(EffectiveRenderStateKey const&) const = default;
};

}

template<>
struct AK::Traits<Web::Painting::EffectiveRenderStateKey> : public DefaultTraits<Web::Painting::EffectiveRenderStateKey> {
    static unsigned hash(Web::Painting::EffectiveRenderStateKey const& key)
    {
        auto scroll_hash = static_cast<unsigned>(key.scroll_frame_id.value_or(0));
        auto combined = pair_int_hash(ptr_hash(key.clip_frame.ptr()), ptr_hash(key.parent.ptr()));
        combined = pair_int_hash(scroll_hash, combined);
        return pair_int_hash(ptr_hash(key.transform_frame.ptr()), combined);
    }
};
