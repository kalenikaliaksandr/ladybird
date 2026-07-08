/*
 * Copyright (c) 2021-2023, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2022-2026, Sam Atkins <sam@ladybird.org>
 * Copyright (c) 2022, Tobias Christiansen <tobyase@serenityos.org>
 * Copyright (c) 2023, MacDue <macdue@dueutil.tech>
 * Copyright (c) 2025, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Debug.h>
#include <AK/Utf16StringBuilder.h>
#include <LibGfx/BoundingBox.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Path.h>
#include <LibGfx/TextLayout.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Text.h>
#include <LibWeb/Layout/BlockFormattingContext.h>
#include <LibWeb/Layout/DominantBaseline.h>
#include <LibWeb/Layout/SVGClipBox.h>
#include <LibWeb/Layout/SVGFormattingContext.h>
#include <LibWeb/Layout/SVGGeometryBox.h>
#include <LibWeb/Layout/SVGImageBox.h>
#include <LibWeb/Layout/SVGMaskBox.h>
#include <LibWeb/Layout/SVGPatternBox.h>
#include <LibWeb/Layout/Viewport.h>
#include <LibWeb/SVG/SVGAElement.h>
#include <LibWeb/SVG/SVGForeignObjectElement.h>
#include <LibWeb/SVG/SVGGElement.h>
#include <LibWeb/SVG/SVGImageElement.h>
#include <LibWeb/SVG/SVGMaskElement.h>
#include <LibWeb/SVG/SVGSVGElement.h>
#include <LibWeb/SVG/SVGSwitchElement.h>
#include <LibWeb/SVG/SVGSymbolElement.h>
#include <LibWeb/SVG/SVGUseElement.h>

namespace Web::Layout {

SVGFormattingContext::SVGFormattingContext(LayoutState& state, LayoutMode layout_mode, Box const& box, FormattingContext* parent)
    : FormattingContext(Type::SVG, layout_mode, state, box, parent)
{
}

SVGFormattingContext::~SVGFormattingContext() = default;

CSSPixels SVGFormattingContext::automatic_content_width() const
{
    return 0;
}

CSSPixels SVGFormattingContext::automatic_content_height() const
{
    return 0;
}

// https://svgwg.org/svg2-draft/struct.html#GroupsOverview
static bool is_container_element(Node const& node)
{
    // container element
    // An element which can have graphics elements and other container elements as child elements.
    // Specifically: ‘a’, ‘clipPath’, ‘defs’, ‘g’, ‘marker’, ‘mask’, ‘pattern’, ‘svg’, ‘switch’ and ‘symbol’.
    auto* dom_node = node.dom_node();
    if (!dom_node)
        return false;
    if (is<SVG::SVGAElement>(dom_node))
        return true;
    // FIXME: clipPath
    // FIXME: defs
    if (is<SVG::SVGGElement>(dom_node))
        return true;
    // FIXME: marker
    if (is<SVG::SVGMaskElement>(dom_node))
        return true;
    // FIXME: pattern
    if (is<SVG::SVGSVGElement>(dom_node))
        return true;
    if (is<SVG::SVGSwitchElement>(dom_node))
        return true;
    if (is<SVG::SVGSymbolElement>(dom_node))
        return true;
    // AD-HOC: Do we need `use` to be here?
    if (is<SVG::SVGUseElement>(dom_node))
        return true;
    return false;
}

void SVGFormattingContext::run(LayoutInput const& layout_input)
{
    auto const& available_space = layout_input.available_space;
    FORMATTING_CONTEXT_TRACE();
    // NOTE: SVG doesn't have a "formatting context" in the spec, but this is the most
    //       obvious way to drive SVG layout in our engine at the moment.
    // FIXME: Box geometry is stored in CSSPixels, which quantizes user units to
    //        1/64 and saturates near ±33.5 million; under large viewport scales
    //        the quantization error becomes visible. Box-driven SVG geometry
    //        should eventually be stored as floats, like path geometry already is.

    auto& svg_box_state = m_state.get_mutable(context_box());

    auto const& document = context_box().document();
    if (document.document_element() == context_box().dom_node() && !document.is_decoded_svg()) {
        // Overwrite the content width/height with the styled node width/height (from <svg width height ...>)

        // NOTE: If a height had not been provided by the svg element, it was set to the height of the container
        if (svg_box_state.node().computed_values().width().is_length())
            svg_box_state.set_content_width(svg_box_state.node().computed_values().width().length().to_px(svg_box_state.node()));
        if (svg_box_state.node().computed_values().height().is_length())
            svg_box_state.set_content_height(svg_box_state.node().computed_values().height().length().to_px(svg_box_state.node()));
        // FIXME: In SVG 2, length can also be a percentage. We'll need to support that.
    }

    // NOTE: We consider all SVG root elements to have definite size in both axes.
    //       I'm not sure if this is good or bad, but our viewport transform logic depends on it.
    svg_box_state.set_has_definite_width(true);
    svg_box_state.set_has_definite_height(true);

    auto* dom_node = context_box().dom_node();
    VERIFY(dom_node);
    Optional<SVG::ViewBox> active_view_box;
    if (auto* svg_graphics_element = as_if<SVG::SVGGraphicsElement>(*dom_node))
        active_view_box = svg_graphics_element->active_view_box();
    else if (auto* svg_fit_to_view_box = as_if<SVG::SVGFitToViewBox>(*dom_node))
        active_view_box = svg_fit_to_view_box->view_box();
    // https://svgwg.org/svg2-draft/coords.html#ViewBoxAttribute
    if (active_view_box.has_value()) {
        if (active_view_box->width < 0 || active_view_box->height < 0) {
            // A negative value for <width> or <height> is an error and invalidates the ‘viewBox’ attribute.
            active_view_box = {};
        } else if (active_view_box->width == 0 || active_view_box->height == 0) {
            // A value of zero disables rendering of the element.
            return;
        }
    }

    // The viewport size is expressed in the viewport's own user units: the viewBox
    // dimensions when a viewBox is active, and the box's local content size (which
    // layout keeps in user units) otherwise. The mapping onto the viewport box is
    // carried by the accumulated visual context tree, not by layout.
    auto viewport_width = [&] {
        if (active_view_box.has_value())
            return CSSPixels::nearest_value_for(active_view_box->width);
        if (svg_box_state.has_definite_width())
            return svg_box_state.content_width();
        dbgln_if(LIBWEB_CSS_DEBUG, "FIXME: Failed to resolve width of SVG viewport!");
        return CSSPixels {};
    }();

    auto viewport_height = [&] {
        if (active_view_box.has_value())
            return CSSPixels::nearest_value_for(active_view_box->height);
        if (svg_box_state.has_definite_height())
            return svg_box_state.content_height();
        dbgln_if(LIBWEB_CSS_DEBUG, "FIXME: Failed to resolve height of SVG viewport!");
        return CSSPixels {};
    }();

    m_available_space = available_space;
    m_quirks_mode_percentage_basis_height = layout_input.containing_block_constraints.quirks_mode_percentage_basis_height;
    m_viewport_size = { viewport_width, viewport_height };

    context_box().for_each_child_of_type<Box>([&](Box const& child) {
        layout_svg_element(child, layout_input);
        return IterationDecision::Continue;
    });
}

void SVGFormattingContext::layout_svg_element(Box const& child, LayoutInput const& layout_input)
{
    if (is<SVG::SVGFitToViewBox>(child.dom_node())) {
        layout_nested_viewport(child);
    } else if (auto* foreign_object_element = as_if<SVG::SVGForeignObjectElement>(child.dom_node()); foreign_object_element && is<BlockContainer>(child)) {
        Layout::BlockFormattingContext bfc(m_state, m_layout_mode, as<BlockContainer>(child), this);
        // SVG layout resolves percentages against the SVG viewport, not a CSS containing
        // block, so boxes inside the SVG subtree carry no percentage basis.
        auto& child_state = m_state.create(child, {}, {});
        CSSPixelRect rect {
            {
                child.computed_values().x().to_px(m_available_space->width.to_px_or_zero()),
                child.computed_values().y().to_px(m_available_space->height.to_px_or_zero()),
            },
            {
                child.computed_values().width().to_px(m_available_space->width.to_px_or_zero()),
                child.computed_values().height().to_px(m_available_space->height.to_px_or_zero()),
            }
        };
        child_state.set_content_offset(rect.location());
        child_state.set_content_width(rect.width());
        child_state.set_content_height(rect.height());

        auto child_available_space = AvailableSpace(AvailableSize::make_definite(child_state.content_width()), AvailableSize::make_definite(child_state.content_height()));
        bfc.run(LayoutInput { child_available_space, { {}, {}, m_quirks_mode_percentage_basis_height } });

        if (auto* mask_box = child.first_child_of_type<SVGMaskBox>())
            layout_mask_or_clip(*mask_box);

        if (auto* clip_box = child.first_child_of_type<SVGClipBox>())
            layout_mask_or_clip(*clip_box);
    } else if (is<SVGGraphicsBox>(child)) {
        layout_graphics_element(static_cast<SVGGraphicsBox const&>(child), layout_input);
    }
}

void SVGFormattingContext::layout_nested_viewport(Box const& viewport)
{
    // Layout for a nested SVG viewport.
    // https://svgwg.org/svg2-draft/coords.html#EstablishingANewSVGViewport.
    auto& nested_viewport_state = m_state.create(viewport, {}, {});
    auto resolve_dimension = [](auto size, auto reference_value) {
        // The value auto for width and height on the ‘svg’ element is treated as 100%.
        // https://svgwg.org/svg2-draft/geometry.html#Sizing
        if (size.is_auto())
            return reference_value;
        return size.to_px(reference_value);
    };

    auto nested_viewport_x = viewport.computed_values().x().to_px(m_viewport_size.width());
    auto nested_viewport_y = viewport.computed_values().y().to_px(m_viewport_size.height());
    auto nested_viewport_width = resolve_dimension(viewport.computed_values().width(), m_viewport_size.width());
    auto nested_viewport_height = resolve_dimension(viewport.computed_values().height(), m_viewport_size.height());

    nested_viewport_state.set_content_offset({ nested_viewport_x, nested_viewport_y });
    nested_viewport_state.set_content_width(nested_viewport_width);
    nested_viewport_state.set_content_height(nested_viewport_height);
    nested_viewport_state.set_has_definite_width(true);
    nested_viewport_state.set_has_definite_height(true);
    SVGFormattingContext nested_context(m_state, m_layout_mode, viewport, this);
    nested_context.run(LayoutInput { *m_available_space, { {}, {}, m_quirks_mode_percentage_basis_height } });
}

Gfx::Path SVGFormattingContext::compute_path_for_text(SVGTextBox const& text_box) const
{
    auto& text_element = text_box.dom_node();
    // FIXME: Use per-code-point fonts.
    auto& font = text_box.first_available_font();
    auto text_contents = text_element.text_contents();
    auto text_width = font.width(text_contents);
    auto text_offset = m_current_text_position;

    // https://svgwg.org/svg2-draft/text.html#TextAnchoringProperties
    switch (text_element.text_anchor().value_or(SVG::TextAnchor::Start)) {
    case SVG::TextAnchor::Start:
        // The rendered characters are aligned such that the start of the resulting rendered text is at the initial
        // current text position.
        break;
    case SVG::TextAnchor::Middle: {
        // The rendered characters are shifted such that the geometric middle of the resulting rendered text
        // (determined from the initial and final current text position before applying the text-anchor property)
        // is at the initial current text position.
        text_offset.translate_by(-text_width / 2, 0);
        break;
    }
    case SVG::TextAnchor::End: {
        // The rendered characters are shifted such that the end of the resulting rendered text (final current text
        // position before applying the text-anchor property) is at the initial current text position.
        text_offset.translate_by(-text_width, 0);
        break;
    }
    default:
        VERIFY_NOT_REACHED();
    }

    auto baseline_metric = resolve_dominant_baseline_metric(text_box.computed_values());
    text_offset.translate_by(0, dominant_baseline_offset(baseline_metric, font.pixel_metrics()));

    Gfx::Path path;
    path.move_to(text_offset);
    path.text(text_contents, font);
    return path;
}

static Utf16String rendered_text_contents(SVG::SVGTextContentElement const& element)
{
    Utf16StringBuilder builder;
    element.for_each_in_subtree_of_type<DOM::Text>([&](auto const& text_node) {
        if (text_node.parent() && text_node.parent()->unsafe_layout_node()) {
            if (auto content = text_node.text_content(); content.has_value())
                builder.append(*content);
        }
        return TraversalDecision::Continue;
    });
    return builder.to_string().trim_ascii_whitespace();
}

Gfx::Path SVGFormattingContext::compute_path_for_text_path(SVGTextPathBox const& text_path_box) const
{
    auto& text_path_element = static_cast<SVG::SVGTextPathElement const&>(text_path_box.dom_node());
    auto path_or_shape = text_path_element.path_or_shape();
    if (!path_or_shape)
        return {};

    // FIXME: Use per-code-point fonts.
    auto& font = text_path_box.first_available_font();
    auto text_contents = rendered_text_contents(text_path_element);

    auto shape_path = const_cast<SVG::SVGGeometryElement&>(*path_or_shape).get_path(m_viewport_size);
    auto start_offset = text_path_element.start_offset_for_path_length(shape_path.length());

    // https://svgwg.org/svg2-draft/text.html#TextAnchoringProperties
    // FIXME: Take writing mode and text direction into account.
    auto total_advance = font.width(text_contents);
    switch (text_path_element.text_anchor().value_or(SVG::TextAnchor::Start)) {
    case SVG::TextAnchor::Start:
        break;
    case SVG::TextAnchor::Middle:
        start_offset -= total_advance / 2;
        break;
    case SVG::TextAnchor::End:
        start_offset -= total_advance;
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    return shape_path.place_text_along(text_contents, font, start_offset);
}

void SVGFormattingContext::layout_path_like_element(SVGGraphicsBox const& graphics_box, LayoutInput const& layout_input)
{
    auto& graphics_box_state = m_state.get_mutable(graphics_box);

    Gfx::Path path;
    if (is<SVGGeometryBox>(graphics_box)) {
        auto& geometry_box = static_cast<SVGGeometryBox const&>(graphics_box);
        path = const_cast<SVGGeometryBox&>(geometry_box).dom_node().get_path(m_viewport_size);
    } else if (auto* text_box = as_if<SVGTextBox>(graphics_box)) {
        // FIXME: Text offsets must be calculated per character. This only applies the first character's offset.
        auto text_positioning = text_box->dom_node().text_positioning();
        text_positioning.apply_to_text_position(m_viewport_size, m_current_text_position, 0u);

        path = compute_path_for_text(*text_box);

        // <text> and <tspan> elements can contain more text elements.
        text_box->for_each_child_of_type<SVGGraphicsBox>([&](auto& child) {
            if (is<SVGTextBox>(child) || is<SVGTextPathBox>(child))
                layout_graphics_element(child, layout_input);
            return IterationDecision::Continue;
        });
    } else if (auto* text_path_box = as_if<SVGTextPathBox>(graphics_box)) {
        // FIXME: Support <tspan> in <textPath>.
        path = compute_path_for_text_path(*text_path_box);
    }

    auto path_bounding_box = path.bounding_box();
    m_current_text_position = path_bounding_box.bottom_right();
    auto bounding_box = path_bounding_box.to_type<CSSPixels>();
    // Stroke increases the path's size by stroke_width/2 per side.
    CSSPixels stroke_width = CSSPixels::nearest_value_for(graphics_box.dom_node().visible_stroke_width());
    bounding_box.inflate(stroke_width, stroke_width);
    graphics_box_state.set_content_offset(bounding_box.top_left());
    graphics_box_state.set_content_width(bounding_box.width());
    graphics_box_state.set_content_height(bounding_box.height());
    graphics_box_state.set_has_definite_width(true);
    graphics_box_state.set_has_definite_height(true);
    graphics_box_state.set_computed_svg_path(move(path));
}

void SVGFormattingContext::layout_graphics_element(SVGGraphicsBox const& graphics_box, LayoutInput const& layout_input)
{
    m_state.create(graphics_box, {}, {});

    if (is_container_element(graphics_box)) {
        // https://svgwg.org/svg2-draft/struct.html#Groups
        // 5.2. Grouping: the ‘g’ element
        // The ‘g’ element is a container element for grouping together related graphics elements.
        layout_container_element(graphics_box, layout_input);
    } else if (is<SVGImageBox>(graphics_box)) {
        layout_image_element(static_cast<SVGImageBox const&>(graphics_box));
    } else {
        // Assume this is a path-like element.
        layout_path_like_element(graphics_box, layout_input);
    }

    if (auto* mask_box = graphics_box.first_child_of_type<SVGMaskBox>())
        layout_mask_or_clip(*mask_box);

    if (auto* clip_box = graphics_box.first_child_of_type<SVGClipBox>())
        layout_mask_or_clip(*clip_box);

    graphics_box.for_each_child_of_type<SVGPatternBox>([&](auto const& pattern_box) {
        layout_mask_or_clip(pattern_box);
        return IterationDecision::Continue;
    });
}

void SVGFormattingContext::layout_image_element(SVGImageBox const& image_box)
{
    auto& box_state = m_state.get_mutable(image_box);
    auto bounding_box = image_box.dom_node().bounding_box(m_viewport_size).to_type<CSSPixels>();

    box_state.set_content_x(bounding_box.x());
    box_state.set_content_y(bounding_box.y());
    box_state.set_content_width(bounding_box.width());
    box_state.set_content_height(bounding_box.height());
    box_state.set_has_definite_width(true);
    box_state.set_has_definite_height(true);
}

void SVGFormattingContext::layout_mask_or_clip(SVGBox const& mask_or_clip)
{
    SVG::SVGUnits content_units {};
    if (is<SVGMaskBox>(mask_or_clip))
        content_units = static_cast<SVGMaskBox const&>(mask_or_clip).dom_node().mask_content_units();
    else if (is<SVGClipBox>(mask_or_clip))
        content_units = static_cast<SVGClipBox const&>(mask_or_clip).dom_node().clip_path_units();
    else if (is<SVGPatternBox>(mask_or_clip))
        content_units = static_cast<SVGPatternBox const&>(mask_or_clip).dom_node().pattern_content_units();
    else
        VERIFY_NOT_REACHED();
    // FIXME: Somehow limit <clipPath> contents to: shape elements, <text>, and <use>.
    auto& layout_state = m_state.create(mask_or_clip, {}, {});

    auto const* pattern_box = as_if<SVGPatternBox>(mask_or_clip);
    if (pattern_box && pattern_box->dom_node().view_box().has_value()) {
        auto const& pattern = pattern_box->dom_node();
        if (pattern.pattern_units() == SVG::SVGUnits::UserSpaceOnUse) {
            layout_state.set_content_width(CSSPixels::nearest_value_for(pattern.pattern_width().resolve_relative_to(m_viewport_size.width().to_float())));
            layout_state.set_content_height(CSSPixels::nearest_value_for(pattern.pattern_height().resolve_relative_to(m_viewport_size.height().to_float())));
        } else {
            auto& parent_node_state = m_state.get(*mask_or_clip.parent());
            layout_state.set_content_width(CSSPixels::nearest_value_for(pattern.pattern_width().value() * parent_node_state.content_width().to_double()));
            layout_state.set_content_height(CSSPixels::nearest_value_for(pattern.pattern_height().value() * parent_node_state.content_height().to_double()));
        }
    } else if (content_units == SVG::SVGUnits::ObjectBoundingBox) {
        // NOTE: The mapping from bounding-box units onto the target's bounding box
        //       happens when the mask/clip/pattern content is recorded, via the root
        //       transform of its private visual context tree. Mask and clip boxes
        //       sit over the target so the masking area covers it; pattern boxes
        //       must stay at the origin, since pattern placement is carried by the
        //       tile rect and their content is recorded viewport-relative.
        auto& parent_node_state = m_state.get(*mask_or_clip.parent());
        if (!pattern_box)
            layout_state.set_content_offset(parent_node_state.offset);
        layout_state.set_content_width(parent_node_state.content_width());
        layout_state.set_content_height(parent_node_state.content_height());
    } else {
        layout_state.set_content_width(m_viewport_size.width());
        layout_state.set_content_height(m_viewport_size.height());
    }
    // Pretend masks/clips are a viewport so contents can resolve against a viewport size
    // that depends on the `contentUnits`.
    SVGFormattingContext nested_context(m_state, m_layout_mode, mask_or_clip, this);
    layout_state.set_has_definite_width(true);
    layout_state.set_has_definite_height(true);
    nested_context.run(LayoutInput { *m_available_space, { {}, {}, m_quirks_mode_percentage_basis_height } });
}

void SVGFormattingContext::layout_container_element(SVGBox const& container, LayoutInput const& layout_input)
{
    auto& box_state = m_state.get_mutable(container);
    Gfx::BoundingBox<float> bounding_box;
    container.for_each_child_of_type<Box>([&](Box const& child) {
        // Masks/clips/patterns do not change the bounding box of their parents.
        if (is<SVGMaskBox>(child) || is<SVGClipBox>(child) || is<SVGPatternBox>(child))
            return IterationDecision::Continue;
        layout_svg_element(child, layout_input);
        auto& child_state = m_state.get(child);
        Gfx::FloatRect child_rect {
            child_state.offset.to_type<float>(),
            { child_state.content_width().to_float(), child_state.content_height().to_float() }
        };
        // The child's transform attribute is applied at paint time, but the container's
        // box must still cover the transformed child.
        if (auto const* graphics_element = as_if<SVG::SVGGraphicsElement>(child.dom_node()))
            child_rect = graphics_element->element_transform().map(child_rect);
        bounding_box.add_point(child_rect.top_left());
        bounding_box.add_point(child_rect.bottom_right());
        return IterationDecision::Continue;
    });
    box_state.set_content_x(CSSPixels::nearest_value_for(bounding_box.x()));
    box_state.set_content_y(CSSPixels::nearest_value_for(bounding_box.y()));
    box_state.set_content_width(CSSPixels::nearest_value_for(bounding_box.width()));
    box_state.set_content_height(CSSPixels::nearest_value_for(bounding_box.height()));
    box_state.set_has_definite_width(true);
    box_state.set_has_definite_height(true);
}

}
