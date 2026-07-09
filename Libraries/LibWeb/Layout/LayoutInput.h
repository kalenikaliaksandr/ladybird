/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibWeb/Layout/AvailableSpace.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Layout {

struct ContainingBlockConstraints {
    Optional<CSSPixels> percentage_basis_width;
    Optional<CSSPixels> percentage_basis_height;
    Optional<CSSPixels> quirks_mode_percentage_basis_height;
};

struct LayoutInput {
    explicit LayoutInput(AvailableSpace new_available_space, ContainingBlockConstraints new_containing_block_constraints = {}, Optional<CSSPixelPoint> new_content_box_position_in_bfc_root = {})
        : available_space(move(new_available_space))
        , containing_block_constraints(move(new_containing_block_constraints))
        , content_box_position_in_bfc_root(new_content_box_position_in_bfc_root)
    {
    }

    // NOTE: The content box position is not carried over: every caller hands the result to a
    //       different box's inside layout (an independent formatting context root, which is the
    //       origin of its own coordinate space).
    [[nodiscard]] LayoutInput with_available_space(AvailableSpace new_available_space) const
    {
        return LayoutInput { move(new_available_space), containing_block_constraints };
    }

    [[nodiscard]] LayoutInput with_content_box_position_in_bfc_root(Optional<CSSPixelPoint> position) const
    {
        return LayoutInput { available_space, containing_block_constraints, position };
    }

    AvailableSpace const available_space;
    ContainingBlockConstraints const containing_block_constraints;

    // Content-box position, in the coordinate space of the nearest block formatting context
    // root, of the box whose inside layout this input describes. Float interaction is the one
    // legitimate flow-position dependency of inside layout, and it arrives through this field
    // instead of being read back from the box's own offset in the layout state. Unset while
    // the box's block position can still be moved by margin collapsing (see the y-position
    // update callback in BlockFormattingContext); such subtrees keep reading the live layout
    // state until margin collapsing is inverted.
    Optional<CSSPixelPoint> const content_box_position_in_bfc_root;
};

}
