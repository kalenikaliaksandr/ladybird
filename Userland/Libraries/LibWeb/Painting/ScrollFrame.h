/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/PixelUnits.h>

namespace Web::Painting {

class ScrollFrame : public RefCounted<ScrollFrame> {
public:
    i32 id { -1 };
    CSSPixelPoint cumulative_offset;
    CSSPixelPoint own_offset;
};

struct StickyFrame : public ScrollFrame {
};

}
