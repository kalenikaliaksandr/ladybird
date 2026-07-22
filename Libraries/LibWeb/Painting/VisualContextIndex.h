/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/DistinctNumeric.h>

namespace Web::Painting {

AK_TYPEDEF_DISTINCT_ORDERED_ID(u32, VisualContextIndex);

// Node 0 is always the visual viewport transform node, so index 0 doubles as "no node" for
// references to scroll nodes, which can never sit at the root.
static constexpr VisualContextIndex VISUAL_VIEWPORT_NODE_INDEX { 0 };

// A reference to a scroll or sticky node specifically, as opposed to an arbitrary node of the
// tree: these are the references that key the scroll offset snapshot and appear in compositor
// metadata command payloads. Values share the tree's index space, but the distinct type keeps
// scroll-node references compiler-checked apart from recording context indices.
AK_TYPEDEF_DISTINCT_ORDERED_ID(u32, ScrollNodeIndex);

// The root transform node can never be a scroll node, so index 0 doubles as "no scroll node".
static constexpr ScrollNodeIndex NO_SCROLL_NODE_INDEX { 0 };

constexpr ScrollNodeIndex to_scroll_node_index(VisualContextIndex index)
{
    return ScrollNodeIndex { index.value() };
}

constexpr VisualContextIndex to_visual_context_index(ScrollNodeIndex index)
{
    return VisualContextIndex { index.value() };
}

// The per-kind references a display list command records: the deepest coordinate-affecting
// (spatial), clip, and effect node applying to it. Until the tree is physically split all three
// index the same tree; 0 means "none" for clip and effect, and the viewport transform root for
// spatial. Inspector overlays record { spatial, 0, 0 } so replay applies coordinates but neither
// clips nor effects.
struct VisualContextRefs {
    VisualContextIndex spatial { VISUAL_VIEWPORT_NODE_INDEX };
    VisualContextIndex clip { VISUAL_VIEWPORT_NODE_INDEX };
    VisualContextIndex effect { VISUAL_VIEWPORT_NODE_INDEX };

    bool operator==(VisualContextRefs const&) const = default;
};

}
