/*
 * Copyright (c) 2020-2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/CanvasBox.h>
#include <LibWeb/Painting/CanvasPaintable.h>

namespace Web::Layout {

CanvasBox::CanvasBox(DOM::Document& document, HTML::HTMLCanvasElement& element, NonnullRefPtr<CSS::ComputedValues const> style)
    : ReplacedBox(document, element, style)
{
}

CanvasBox::~CanvasBox() = default;

CSS::SizeWithAspectRatio CanvasBox::compute_auto_content_box_size() const
{
    auto natural_size = dom_node().natural_size();
    auto width = natural_size.width();
    auto height = natural_size.height();
    if (width == 0 || height == 0)
        return { width, height, {} };
    return { width, height, CSSPixelFraction(width, height) };
}

RefPtr<Painting::Paintable> CanvasBox::create_paintable() const
{
    return Painting::CanvasPaintable::create(*this);
}

}
