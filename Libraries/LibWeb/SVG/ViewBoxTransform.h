/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibGfx/AffineTransform.h>
#include <LibWeb/Forward.h>
#include <LibWeb/PixelUnits.h>
#include <LibWeb/SVG/AttributeParser.h>

namespace Web::SVG {

// The view box a viewport-establishing element renders with, if any:
// a negative size invalidates the viewBox attribute, and a zero size
// disables rendering of the element, so neither is returned.
// https://svgwg.org/svg2-draft/coords.html#ViewBoxAttribute
[[nodiscard]] Optional<ViewBox> active_view_box_for_rendering(DOM::Node const&);

// Applies the viewBox-to-viewport mapping for the given viewport size (the
// preserveAspectRatio scale and alignment translation, then the viewBox
// origin translation) onto the given base transform.
[[nodiscard]] Gfx::AffineTransform viewbox_transform_applied_to(Gfx::AffineTransform base, ViewBox const&, PreserveAspectRatio const&, CSSPixelSize viewport_size);

}
