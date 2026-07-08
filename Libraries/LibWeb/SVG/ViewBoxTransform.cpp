/*
 * Copyright (c) 2021-2023, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2023, MacDue <macdue@dueutil.tech>
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/SVG/SVGGraphicsElement.h>
#include <LibWeb/SVG/ViewBoxTransform.h>

namespace Web::SVG {

Optional<ViewBox> active_view_box_for_rendering(DOM::Node const& dom_node)
{
    Optional<ViewBox> view_box;
    if (auto const* svg_graphics_element = as_if<SVGGraphicsElement>(dom_node))
        view_box = svg_graphics_element->active_view_box();
    else if (auto const* fit_to_view_box = as_if<SVGFitToViewBox>(dom_node))
        view_box = fit_to_view_box->view_box();
    if (view_box.has_value() && (view_box->width <= 0 || view_box->height <= 0))
        return {};
    return view_box;
}

struct ViewBoxOffsetAndScale {
    CSSPixelPoint offset;
    double scale_factor_x;
    double scale_factor_y;
};

// https://svgwg.org/svg2-draft/coords.html#PreserveAspectRatioAttribute
static ViewBoxOffsetAndScale scale_and_align_viewbox_content(PreserveAspectRatio const& preserve_aspect_ratio,
    ViewBox const& view_box, Gfx::FloatSize viewbox_scale, CSSPixelSize viewport_size)
{
    ViewBoxOffsetAndScale viewbox_transform {};

    if (preserve_aspect_ratio.align == PreserveAspectRatio::Align::None) {
        viewbox_transform.scale_factor_x = viewbox_scale.width();
        viewbox_transform.scale_factor_y = viewbox_scale.height();
        viewbox_transform.offset = {};
        return viewbox_transform;
    }

    switch (preserve_aspect_ratio.meet_or_slice) {
    case PreserveAspectRatio::MeetOrSlice::Meet:
        // meet (the default) - Scale the graphic such that:
        // - aspect ratio is preserved
        // - the entire ‘viewBox’ is visible within the SVG viewport
        // - the ‘viewBox’ is scaled up as much as possible, while still meeting the other criteria
        viewbox_transform.scale_factor_x = viewbox_transform.scale_factor_y = min(viewbox_scale.width(), viewbox_scale.height());
        break;
    case PreserveAspectRatio::MeetOrSlice::Slice:
        // slice - Scale the graphic such that:
        // aspect ratio is preserved
        // the entire SVG viewport is covered by the ‘viewBox’
        // the ‘viewBox’ is scaled down as much as possible, while still meeting the other criteria
        viewbox_transform.scale_factor_x = viewbox_transform.scale_factor_y = max(viewbox_scale.width(), viewbox_scale.height());
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    // Handle X alignment:
    switch (preserve_aspect_ratio.align) {
    case PreserveAspectRatio::Align::xMinYMin:
    case PreserveAspectRatio::Align::xMinYMid:
    case PreserveAspectRatio::Align::xMinYMax:
        // Align the <min-x> of the element's ‘viewBox’ with the smallest X value of the SVG viewport.
        viewbox_transform.offset.translate_by(0, 0);
        break;
    case PreserveAspectRatio::Align::None: {
        // Do not force uniform scaling. Scale the graphic content of the given element non-uniformly
        // if necessary such that the element's bounding box exactly matches the SVG viewport rectangle.
        // FIXME: None is unimplemented (treat as xMidYMid)
        [[fallthrough]];
    }
    case PreserveAspectRatio::Align::xMidYMin:
    case PreserveAspectRatio::Align::xMidYMid:
    case PreserveAspectRatio::Align::xMidYMax:
        // Align the midpoint X value of the element's ‘viewBox’ with the midpoint X value of the SVG viewport.
        viewbox_transform.offset.translate_by((viewport_size.width() - CSSPixels::nearest_value_for(view_box.width * viewbox_transform.scale_factor_x)) / 2, 0);
        break;
    case PreserveAspectRatio::Align::xMaxYMin:
    case PreserveAspectRatio::Align::xMaxYMid:
    case PreserveAspectRatio::Align::xMaxYMax:
        // Align the <min-x>+<width> of the element's ‘viewBox’ with the maximum X value of the SVG viewport.
        viewbox_transform.offset.translate_by((viewport_size.width() - CSSPixels::nearest_value_for(view_box.width * viewbox_transform.scale_factor_x)), 0);
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    // Handle Y alignment:
    switch (preserve_aspect_ratio.align) {
    case PreserveAspectRatio::Align::xMinYMin:
    case PreserveAspectRatio::Align::xMidYMin:
    case PreserveAspectRatio::Align::xMaxYMin:
        // Align the <min-y> of the element's ‘viewBox’ with the smallest Y value of the SVG viewport.
        viewbox_transform.offset.translate_by(0, 0);
        break;
    case PreserveAspectRatio::Align::None: {
        // Do not force uniform scaling. Scale the graphic content of the given element non-uniformly
        // if necessary such that the element's bounding box exactly matches the SVG viewport rectangle.
        // FIXME: None is unimplemented (treat as xMidYMid)
        [[fallthrough]];
    }
    case PreserveAspectRatio::Align::xMinYMid:
    case PreserveAspectRatio::Align::xMidYMid:
    case PreserveAspectRatio::Align::xMaxYMid:
        // Align the midpoint Y value of the element's ‘viewBox’ with the midpoint Y value of the SVG viewport.
        viewbox_transform.offset.translate_by(0, (viewport_size.height() - CSSPixels::nearest_value_for(view_box.height * viewbox_transform.scale_factor_y)) / 2);
        break;
    case PreserveAspectRatio::Align::xMinYMax:
    case PreserveAspectRatio::Align::xMidYMax:
    case PreserveAspectRatio::Align::xMaxYMax:
        // Align the <min-y>+<height> of the element's ‘viewBox’ with the maximum Y value of the SVG viewport.
        viewbox_transform.offset.translate_by(0, (viewport_size.height() - CSSPixels::nearest_value_for(view_box.height * viewbox_transform.scale_factor_y)));
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    return viewbox_transform;
}

Gfx::AffineTransform viewbox_transform_applied_to(Gfx::AffineTransform base, ViewBox const& view_box, PreserveAspectRatio const& preserve_aspect_ratio, CSSPixelSize viewport_size)
{
    auto scale_width = viewport_size.width() / view_box.width;
    auto scale_height = viewport_size.height() / view_box.height;
    auto viewbox_offset_and_scale = scale_and_align_viewbox_content(preserve_aspect_ratio, view_box, { scale_width, scale_height }, viewport_size);
    CSSPixelPoint offset = viewbox_offset_and_scale.offset;
    return base
        .translate(offset.to_type<float>())
        .scale(viewbox_offset_and_scale.scale_factor_x, viewbox_offset_and_scale.scale_factor_y)
        .translate({ -view_box.min_x, -view_box.min_y });
}

}
