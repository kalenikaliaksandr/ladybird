/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibGfx/AffineTransform.h>
#include <LibWeb/PixelUnits.h>
#include <LibWeb/SVG/AttributeParser.h>

namespace Web::SVG {

// Applies the viewBox-to-viewport mapping for the given viewport size (the
// preserveAspectRatio scale and alignment translation, then the viewBox
// origin translation) onto the given base transform.
[[nodiscard]] Gfx::AffineTransform viewbox_transform_applied_to(Gfx::AffineTransform base, ViewBox const&, PreserveAspectRatio const&, CSSPixelSize viewport_size);

}
