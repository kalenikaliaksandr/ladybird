/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <LibGfx/AffineTransform.h>
#include <LibWeb/Layout/AvailableSpace.h>

namespace Web::Layout {

enum class SizeConstraint {
    None,
    MinContent,
    MaxContent,
};

struct LayoutInput {
    static LayoutInput from_available_space(AvailableSpace available_space)
    {
        auto percentage_resolution_width = available_space.width.to_px_or_zero();
        auto percentage_resolution_height = available_space.height.to_px_or_zero();
        auto containing_block_content_width = available_space.width.to_px_or_zero();
        auto definite_percentage_resolution_width = definite_size_or_none(available_space.width);
        auto definite_percentage_resolution_height = definite_size_or_none(available_space.height);
        return {
            move(available_space),
            percentage_resolution_width,
            percentage_resolution_height,
            containing_block_content_width,
            move(definite_percentage_resolution_width),
            move(definite_percentage_resolution_height),
            {},
            {},
        };
    }

    static Optional<CSSPixels> definite_size_or_none(AvailableSize const& available_size)
    {
        if (!available_size.is_definite())
            return {};
        return available_size.to_px_or_zero();
    }

    void set_definite_percentage_resolution_size(AvailableSpace const& available_space)
    {
        definite_percentage_resolution_width = definite_size_or_none(available_space.width);
        definite_percentage_resolution_height = definite_size_or_none(available_space.height);
    }

    AvailableSpace available_space;
    CSSPixels percentage_resolution_width { 0 };
    CSSPixels percentage_resolution_height { 0 };
    CSSPixels containing_block_content_width { 0 };
    Optional<CSSPixels> definite_percentage_resolution_width {};
    Optional<CSSPixels> definite_percentage_resolution_height {};
    Optional<CSSPixelSize> table_wrapper_grid_area_size {};
    Optional<CSSPixels> table_wrapper_content_width {};
    Gfx::AffineTransform svg_parent_viewbox_transform {};
    Optional<Gfx::AffineTransform> svg_parent_transform {};
};

}
