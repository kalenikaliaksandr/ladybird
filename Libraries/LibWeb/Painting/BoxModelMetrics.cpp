/*
 * Copyright (c) 2018-2025, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Painting/BoxModelMetrics.h>

namespace Web::Painting {

PixelBox BoxModelMetrics::border_box() const
{
    return {
        border.top + scrollbar_gutter.top + padding.top,
        border.right + scrollbar_gutter.right + padding.right,
        border.bottom + scrollbar_gutter.bottom + padding.bottom,
        border.left + scrollbar_gutter.left + padding.left,
    };
}

}
