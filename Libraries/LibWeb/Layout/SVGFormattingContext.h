/*
 * Copyright (c) 2021, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Forward.h>
#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/SVGImageBox.h>
#include <LibWeb/Layout/SVGSVGBox.h>
#include <LibWeb/Layout/SVGTextBox.h>
#include <LibWeb/Layout/SVGTextPathBox.h>

namespace Web::Layout {

// Lays out SVG content in the local user units of its viewport. The viewBox
// and transform-attribute mappings are not applied to layout geometry; they
// are carried by the accumulated visual context tree and applied at paint,
// hit-test, and geometry-query time, like CSS transforms.
class SVGFormattingContext final : public FormattingContext {
public:
    explicit SVGFormattingContext(LayoutState&, LayoutMode, Box const&, FormattingContext* parent);
    ~SVGFormattingContext();

    virtual void run(LayoutInput const&) override;
    virtual CSSPixels automatic_content_width() const override;
    virtual CSSPixels automatic_content_height() const override;

private:
    void layout_svg_element(Box const&, LayoutInput const&);
    void layout_nested_viewport(Box const&);
    void layout_container_element(SVGBox const&, LayoutInput const&);
    void layout_graphics_element(SVGGraphicsBox const&, LayoutInput const&);
    void layout_path_like_element(SVGGraphicsBox const&, LayoutInput const&);
    void layout_mask_or_clip(SVGBox const&);
    void layout_image_element(SVGImageBox const& image_box);

    [[nodiscard]] Gfx::Path compute_path_for_text(SVGTextBox const&) const;
    [[nodiscard]] Gfx::Path compute_path_for_text_path(SVGTextPathBox const&) const;

    Optional<AvailableSpace> m_available_space {};
    Optional<CSSPixels> m_quirks_mode_percentage_basis_height {};
    CSSPixelSize m_viewport_size {};
    Gfx::FloatPoint m_current_text_position {};
};

}
