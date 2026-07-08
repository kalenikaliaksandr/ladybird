/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/SVGClipBox.h>
#include <LibWeb/Layout/SVGMaskBox.h>
#include <LibWeb/Layout/SVGSVGBox.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/DisplayListRecorder.h>
#include <LibWeb/Painting/SVGClipPaintable.h>
#include <LibWeb/Painting/SVGMaskable.h>
#include <LibWeb/Painting/SVGPaintable.h>
#include <LibWeb/Painting/StackingContext.h>

namespace Web::Painting {

template<typename T>
static T const* first_child_layout_node_of_type(SVG::SVGGraphicsElement const& graphics_element)
{
    // NB: Called during painting.
    if (!graphics_element.unsafe_layout_node())
        return nullptr;
    return graphics_element.unsafe_layout_node()->first_child_of_type<T>();
}

// The target's rect relative to its nearest viewport, in the viewport's user units.
static CSSPixelRect svg_local_reference_box(Paintable const& target)
{
    auto const* svg_root = target.layout_node().first_ancestor_of_type<Layout::SVGSVGBox>();
    if (!svg_root || !svg_root->paintable_box())
        return { {}, { target.content_width(), target.content_height() } };
    return {
        target.absolute_rect().location() - svg_root->paintable_box()->absolute_rect().location(),
        { target.content_width(), target.content_height() }
    };
}

// Maps bounding-box content units onto the target's local reference box;
// identity for userSpaceOnUse, whose content is already in viewport user units.
static Gfx::AffineTransform svg_content_units_transform(Paintable const& target, SVG::SVGUnits content_units)
{
    if (content_units != SVG::SVGUnits::ObjectBoundingBox)
        return {};
    auto reference_box = svg_local_reference_box(target);
    return Gfx::AffineTransform {}
        .translate(reference_box.location().to_type<float>())
        .scale(reference_box.width().to_float(), reference_box.height().to_float());
}

static auto get_mask_box(SVG::SVGGraphicsElement const& graphics_element)
{
    return first_child_layout_node_of_type<Layout::SVGMaskBox>(graphics_element);
}

static auto get_clip_box(SVG::SVGGraphicsElement const& graphics_element)
{
    return first_child_layout_node_of_type<Layout::SVGClipBox>(graphics_element);
}

Optional<CSSPixelRect> SVGMaskable::get_svg_mask_area() const
{
    auto const& graphics_element = as<SVG::SVGGraphicsElement const>(*dom_node_of_svg());
    if (auto* mask_box = get_mask_box(graphics_element))
        return mask_box->dom_node().resolve_masking_area(mask_box->paintable_box()->absolute_border_box_rect());
    return {};
}

Optional<CSSPixelRect> SVGMaskable::get_svg_clip_area() const
{
    auto const& graphics_element = as<SVG::SVGGraphicsElement const>(*dom_node_of_svg());
    auto const* clip_box = get_clip_box(graphics_element);
    if (!clip_box)
        return {};

    auto const& clip_paintable = as<SVGPaintable>(*clip_box->paintable_box());

    // Bounding-box content units map onto the target's local reference box; layout
    // no longer bakes that mapping into the clip content, so apply it here.
    auto clip_path_transform = clip_box->dom_node().element_transform();
    if (auto target_paintable = graphics_element.unsafe_layout_node() ? graphics_element.unsafe_layout_node()->first_paintable() : nullptr)
        clip_path_transform = svg_content_units_transform(*target_paintable, clip_box->dom_node().clip_path_units()).multiply(clip_path_transform);
    // An empty clipping path will completely clip away the element that had the clip-path property applied.
    return clip_paintable.clip_path_geometry_bounds(clip_path_transform).value_or(CSSPixelRect {});
}

static Gfx::MaskKind mask_type_to_gfx_mask_kind(CSS::MaskType mask_type)
{
    switch (mask_type) {
    case CSS::MaskType::Alpha:
        return Gfx::MaskKind::Alpha;
    case CSS::MaskType::Luminance:
        return Gfx::MaskKind::Luminance;
    default:
        VERIFY_NOT_REACHED();
    }
}

Optional<Gfx::MaskKind> SVGMaskable::get_svg_mask_type() const
{
    auto const& graphics_element = as<SVG::SVGGraphicsElement const>(*dom_node_of_svg());
    if (auto* mask_box = get_mask_box(graphics_element))
        return mask_type_to_gfx_mask_kind(mask_box->computed_values().mask_type());
    return {};
}

static Optional<DisplayListResource> paint_mask_or_clip_to_display_list(
    DisplayListRecordingContext& context,
    Paintable const& target,
    Paintable const& paintable,
    CSSPixelRect const& area,
    bool is_clip_path)
{
    auto mask_rect = context.enclosing_device_rect(area);

    auto content_units = [&] {
        if (auto const* mask_box = as_if<Layout::SVGMaskBox>(paintable.layout_node()))
            return mask_box->dom_node().mask_content_units();
        return as<Layout::SVGClipBox>(paintable.layout_node()).dom_node().clip_path_units();
    }();

    // The shift into mask-surface-local coordinates and the mapping of
    // bounding-box content units onto the target both go into the root transform
    // node of the subtree's private visual context tree rather than a recorder
    // translation: record-time culling bounds don't see visual context nodes, so
    // recorded coordinates must not carry a baked translation the nodes rescale.
    // The node's origin anchors the mapping to the viewport the recorded
    // coordinates are flattened against.
    auto scale = static_cast<float>(context.device_pixels_per_css_pixel());
    auto root_affine = Gfx::AffineTransform {}
                           .translate(mask_rect.location().to_type<int>().to_type<float>().scaled(-1.f / scale))
                           .multiply(svg_content_units_transform(target, content_units));
    Optional<TransformData> root_transform;
    if (!root_affine.is_identity()) {
        Gfx::FloatPoint origin;
        auto const* svg_root = target.layout_node().first_ancestor_of_type<Layout::SVGSVGBox>();
        if (svg_root && svg_root->paintable_box())
            origin = svg_root->paintable_box()->absolute_rect().location().to_type<float>().scaled(scale);
        root_transform = TransformData { matrix_for_svg_transform(root_affine, scale), origin };
    }

    // The content of bounding-box-unit subtrees is scaled by the reference box on
    // top of the target's accumulated scale; record-time raster consumers (like
    // SVG images) need the combined factor.
    auto content_units_transform = svg_content_units_transform(target, content_units);
    auto inherited_svg_scale = target.svg_user_units_to_css_pixels_scale();
    inherited_svg_scale.scale_by(content_units_transform.x_scale(), content_units_transform.y_scale());

    SVGSubtreeVisualContextIndicesSaver indices_saver(const_cast<Paintable&>(paintable));
    auto visual_context_tree = build_nested_svg_visual_context_tree(const_cast<Paintable&>(paintable), move(root_transform), inherited_svg_scale);
    auto display_list = DisplayList::create(visual_context_tree);
    DisplayListRecorder display_list_recorder(*display_list, visual_context_tree, context.display_list_recorder().resource_storage());
    auto paint_context = context.clone(display_list_recorder);
    paint_context.set_draw_svg_geometry_for_clip_path(is_clip_path);
    StackingContext::paint_svg(paint_context, paintable, PaintPhase::Foreground);
    return DisplayListResource { *display_list, move(visual_context_tree) };
}

Optional<DisplayListResource> SVGMaskable::calculate_svg_mask_display_list(DisplayListRecordingContext& context, CSSPixelRect const& mask_area) const
{
    auto const& graphics_element = as<SVG::SVGGraphicsElement const>(*dom_node_of_svg());
    auto* mask_box = get_mask_box(graphics_element);
    if (!mask_box || !graphics_element.unsafe_layout_node() || !graphics_element.unsafe_layout_node()->first_paintable())
        return {};
    auto& target_paintable = *graphics_element.unsafe_layout_node()->first_paintable();
    auto& mask_paintable = static_cast<Paintable const&>(*mask_box->first_paintable());
    return paint_mask_or_clip_to_display_list(context, target_paintable, mask_paintable, mask_area, false);
}

Optional<DisplayListResource> SVGMaskable::calculate_svg_clip_display_list(DisplayListRecordingContext& context, CSSPixelRect const& clip_area) const
{
    auto const& graphics_element = as<SVG::SVGGraphicsElement const>(*dom_node_of_svg());
    auto* clip_box = get_clip_box(graphics_element);
    if (!clip_box || !graphics_element.unsafe_layout_node() || !graphics_element.unsafe_layout_node()->first_paintable())
        return {};
    auto& target_paintable = *graphics_element.unsafe_layout_node()->first_paintable();
    auto& clip_paintable = static_cast<Paintable const&>(*clip_box->first_paintable());
    return paint_mask_or_clip_to_display_list(context, target_paintable, clip_paintable, clip_area, true);
}

}
