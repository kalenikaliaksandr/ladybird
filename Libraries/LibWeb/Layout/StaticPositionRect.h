/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/PixelUnits.h>

namespace Web::Layout {

// https://www.w3.org/TR/css-position-3/#static-position-rectangle
struct StaticPositionRect {
    enum class Alignment {
        Start,
        Center,
        End,
    };

    CSSPixelRect rect;
    Alignment horizontal_alignment { Alignment::Start };
    Alignment vertical_alignment { Alignment::Start };

    CSSPixelPoint aligned_position_for_box_with_size(CSSPixelSize const& size) const
    {
        CSSPixelPoint position = rect.location();
        if (horizontal_alignment == Alignment::Center)
            position.set_x(position.x() + (rect.width() - size.width()) / 2);
        else if (horizontal_alignment == Alignment::End)
            position.set_x(position.x() + rect.width() - size.width());

        if (vertical_alignment == Alignment::Center)
            position.set_y(position.y() + (rect.height() - size.height()) / 2);
        else if (vertical_alignment == Alignment::End)
            position.set_y(position.y() + rect.height() - size.height());

        return position;
    }
};

}
