/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibWeb/Layout/LayoutInput.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Layout {

// Snapshot of the parts of a formatting context root's used values that run() consumes as
// inputs. The parent context resolves these before running the context, so together with
// LayoutInput they determine the run's output for an unchanged subtree.
struct LayoutRunRootSnapshot {
    CSSPixels content_width;
    CSSPixels content_height;
    bool has_definite_width { false };
    bool has_definite_height { false };

    CSSPixels margin_left;
    CSSPixels margin_right;
    CSSPixels margin_top;
    CSSPixels margin_bottom;

    CSSPixels border_left;
    CSSPixels border_right;
    CSSPixels border_top;
    CSSPixels border_bottom;

    CSSPixels padding_left;
    CSSPixels padding_right;
    CSSPixels padding_top;
    CSSPixels padding_bottom;

    bool operator==(LayoutRunRootSnapshot const&) const = default;
};

// Everything a committing formatting context run consumed as input the last time it laid out
// a box's subtree. If a later pass reaches the same box with equal inputs and a clean subtree,
// the committed output of that run is still valid.
//
// content_box_position_in_bfc_root is deliberately absent: positions within a block formatting
// context never reach an independent context's run (the BFC root reseeds them).
struct SavedLayoutRunInput {
    AvailableSpace available_space;
    ContainingBlockConstraints containing_block_constraints;
    Optional<CSSPixels> table_grid_min_border_box_height;

    LayoutRunRootSnapshot root_snapshot;

    // Some layout code reads the viewport size directly (e.g. the fallback size for replaced
    // elements without natural dimensions), so it is an ambient input to every run.
    CSSPixelSize viewport_size;

    bool operator==(SavedLayoutRunInput const&) const = default;
};

}
