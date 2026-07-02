/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibWeb/Layout/AvailableSpace.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Layout {

struct LayoutInput {
    explicit LayoutInput(
        AvailableSpace new_available_space,
        Optional<CSSPixels> new_percentage_basis_width = {},
        Optional<CSSPixels> new_percentage_basis_height = {},
        Optional<CSSPixels> new_percentage_resolution_block_size = {})
        : available_space(move(new_available_space))
        , percentage_basis_width(new_percentage_basis_width)
        , percentage_basis_height(new_percentage_basis_height)
        , percentage_resolution_block_size(new_percentage_resolution_block_size)
    {
    }

    [[nodiscard]] LayoutInput with_available_space(AvailableSpace new_available_space) const
    {
        return LayoutInput { move(new_available_space), percentage_basis_width, percentage_basis_height, percentage_resolution_block_size };
    }

    AvailableSpace const available_space;
    // The containing-block constraints for the box laid out with this input, derived one
    // containing-block hop at a time as formatting contexts descend, so resolving a
    // percentage never walks ancestor state. The percentage bases mirror the values pinned
    // on the box's UsedValues; percentage_resolution_block_size is the quirks-mode
    // percentage height basis:
    // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
    Optional<CSSPixels> const percentage_basis_width;
    Optional<CSSPixels> const percentage_basis_height;
    Optional<CSSPixels> const percentage_resolution_block_size;
};

}
