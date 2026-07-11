/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/StdLibExtras.h>
#include <AK/WeakPtr.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/BoxModelMetrics.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Painting {

enum class PaintPhase {
    Background,
    Border,
    TableCollapsedBorder,
    Foreground,
    Outline,
    Overlay,
};

enum class SelectionState : u8 {
    None,
    Start,
    End,
    StartAndEnd,
    Full,
};

struct LineBoxData {
    size_t index { 0 };
    CSSPixelRect rect;
};

// The part of an inline box (InlineNode or inline-flow ListItemBox) fragmented across
// lines that lies on one line: its border-box rect there and which box edges are present
// on this piece (box-decoration-break: slice).
struct InlineBoxPiece {
    enum class Edge : u8 {
        Top = 1 << 0,
        Right = 1 << 1,
        Bottom = 1 << 2,
        Left = 1 << 3,
    };

    WeakPtr<Layout::Node const> node;
    u32 line_index { 0 };
    // Nesting depth below the block container establishing the inline formatting context
    // (outermost inline box = 1). Outer pieces paint before inner ones on the same line;
    // inner pieces win hit-test precedence.
    u32 depth { 0 };
    // Range into the containing block paintable's fragment list covering this node's
    // fragments on this line.
    u32 first_fragment_index { 0 };
    u32 fragment_count { 0 };
    // Relative to the containing block's content-box origin.
    CSSPixelRect border_box_rect;
    u8 edges { 0 };
    // A piece emitted only so the node has geometry: its content on this line is an
    // interrupting block-in-inline, or it has no content at all.
    bool is_placeholder { false };

    bool has_edge(Edge edge) const { return edges & to_underlying(edge); }

    // Shrinks the rect by the given side widths on the edges present on this piece
    // (absent edges are cut by fragmentation and carry no border or padding).
    CSSPixelRect shrunken_by_included_edges(CSSPixelRect rect, PixelBox const& sides) const
    {
        rect.shrink(
            has_edge(Edge::Top) ? sides.top : 0,
            has_edge(Edge::Right) ? sides.right : 0,
            has_edge(Edge::Bottom) ? sides.bottom : 0,
            has_edge(Edge::Left) ? sides.left : 0);
        return rect;
    }
};

}
