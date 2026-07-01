/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibWeb/CSS/Enums.h>
#include <LibWeb/Layout/AvailableSpace.h>
#include <LibWeb/Layout/StaticPositionRect.h>

namespace Web::Layout {

struct LayoutInput {
    explicit LayoutInput(AvailableSpace new_available_space)
        : LayoutInput(
              new_available_space,
              definite_size_from_available_size(new_available_space.width),
              definite_size_from_available_size(new_available_space.height),
              CSS::WritingMode::HorizontalTb,
              {},
              {})
    {
    }

    LayoutInput(
        AvailableSpace new_available_space,
        Optional<CSSPixels> new_percentage_basis_width,
        Optional<CSSPixels> new_percentage_basis_height,
        CSS::WritingMode new_containing_block_writing_mode,
        Optional<StaticPositionRect> new_static_position_rect = {},
        Optional<CSSPixels> new_percentage_resolution_block_size = {})
        : available_space(move(new_available_space))
        , percentage_basis_width(new_percentage_basis_width)
        , percentage_basis_height(new_percentage_basis_height)
        , containing_block_writing_mode(new_containing_block_writing_mode)
        , static_position_rect(new_static_position_rect)
        , percentage_resolution_block_size(new_percentage_resolution_block_size)
    {
    }

    AvailableSpace const available_space;
    // The percentage bases transport containing-block constraints into a formatting context root
    // (or an intrinsic-sizing subtree root). They are consumed exactly once, when the root box's
    // UsedValues are seeded; sizing helpers read the basis from UsedValues, never from here.
    Optional<CSSPixels> const percentage_basis_width;
    Optional<CSSPixels> const percentage_basis_height;
    CSS::WritingMode const containing_block_writing_mode;
    Optional<StaticPositionRect> const static_position_rect;
    Optional<CSSPixels> const percentage_resolution_block_size;

private:
    static Optional<CSSPixels> definite_size_from_available_size(AvailableSize const& available_size)
    {
        if (!available_size.is_definite())
            return {};
        return available_size.to_px_or_zero();
    }
};

}
