/*
 * Copyright (c) 2018-2023, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16StringBuilder.h>
#include <LibGfx/TextLayout.h>
#include <LibWeb/CSS/StyleValues/PositionStyleValue.h>
#include <LibWeb/HTML/DecodedImageData.h>
#include <LibWeb/HTML/HTMLImageElement.h>
#include <LibWeb/Layout/SVGImageBox.h>
#include <LibWeb/Painting/BorderRadiusCornerClipper.h>
#include <LibWeb/Painting/DisplayListRecorder.h>
#include <LibWeb/Painting/ImagePaintable.h>
#include <LibWeb/Painting/ReplacedElementCommon.h>
#include <LibWeb/Platform/FontPlugin.h>

namespace Web::Painting {

static void paint_alt_text(DisplayListRecordingContext& context, Layout::Node const& layout_node, Gfx::IntRect const& content_rect, String const& alt_text, Color color)
{
    auto const& font = layout_node.font(context);
    auto const metrics = font.pixel_metrics();
    auto line_height = metrics.ascent + metrics.descent;
    if (line_height <= 0)
        return;

    float baseline_y = content_rect.y() + metrics.ascent;
    Utf16String line;
    auto draw_line = [&] {
        if (line.is_empty())
            return;
        auto glyph_run = Gfx::shape_text({}, 0, line.utf16_view(), font, Gfx::GlyphRun::TextType::Ltr);
        context.display_list_recorder().draw_glyph_run({ static_cast<float>(content_rect.x()), baseline_y }, *glyph_run, color, content_rect, 1.0, Gfx::Orientation::Horizontal);
        baseline_y += line_height;
        line = {};
    };

    Utf16String::from_utf8(alt_text).for_each_split_view(' ', SplitBehavior::Nothing, [&](Utf16View const& word) {
        Utf16StringBuilder builder;
        builder.append(line);
        if (!line.is_empty())
            builder.append_ascii(' ');
        builder.append(word);

        auto candidate_line = builder.to_string();
        if (line.is_empty() || font.width(candidate_line) <= content_rect.width()) {
            line = move(candidate_line);
            return IterationDecision::Continue;
        }

        draw_line();
        builder.clear();
        builder.append(word);
        line = builder.to_string();
        return IterationDecision::Continue;
    });

    draw_line();
}

NonnullRefPtr<ImagePaintable> ImagePaintable::create(Layout::SVGImageBox const& layout_box)
{
    return adopt_ref(*new ImagePaintable(layout_box, layout_box.dom_node(), false, String {}, true));
}

NonnullRefPtr<ImagePaintable> ImagePaintable::create(Layout::ImageBox const& layout_box)
{
    String alt;
    if (auto element = layout_box.dom_node())
        alt = element->get_attribute_value(HTML::AttributeNames::alt);
    return adopt_ref(*new ImagePaintable(layout_box, layout_box.image_provider(), layout_box.renders_as_alt_text(), move(alt), false));
}

ImagePaintable::ImagePaintable(Layout::Box const& layout_box, Layout::ImageProvider const& image_provider, bool renders_as_alt_text, String alt_text, bool is_svg_image)
    : Paintable(layout_box)
    , m_renders_as_alt_text(renders_as_alt_text)
    , m_alt_text(move(alt_text))
    , m_image_provider(image_provider)
    , m_is_svg_image(is_svg_image)
{
}

void ImagePaintable::reset_for_relayout()
{
    Paintable::reset_for_relayout();

    if (!m_is_svg_image) {
        m_renders_as_alt_text = !m_image_provider.is_image_available();
        if (auto const* image_box = as_if<Layout::ImageBox>(layout_node())) {
            if (auto element = image_box->dom_node())
                m_alt_text = element->get_attribute_value(HTML::AttributeNames::alt);
        }
    }
}

// SVG image geometry stays in the local user units of its viewport and must not
// be snapped to whole device pixels: the replay transform scales any snapping
// error along with the content. Paint at the unsnapped destination and use the
// accumulated SVG scale only to size the raster requested from the image data.
void ImagePaintable::paint_svg_image(DisplayListRecordingContext& context, HTML::DecodedImageData const& decoded_image_data, CSSPixelRect image_rect) const
{
    // https://svgwg.org/svg2-draft/embedded.html#ImageElement
    // The default preserveAspectRatio (xMidYMid meet) behaves like object-fit: contain.
    auto dst_rect = image_rect.to_type<float>();
    auto natural_size = m_image_provider.intrinsic_size();
    if (natural_size.has_value() && !natural_size->is_empty()) {
        auto scale = min(dst_rect.width() / natural_size->width().to_float(), dst_rect.height() / natural_size->height().to_float());
        Gfx::FloatSize contained_size { natural_size->width().to_float() * scale, natural_size->height().to_float() * scale };
        dst_rect = { dst_rect.location() + Gfx::FloatPoint { (dst_rect.width() - contained_size.width()) / 2, (dst_rect.height() - contained_size.height()) / 2 }, contained_size };
    }
    if (dst_rect.is_empty())
        return;

    auto device_pixels_per_css_pixel = static_cast<float>(context.device_pixels_per_css_pixel());
    auto device_dst_rect = dst_rect.scaled(device_pixels_per_css_pixel, device_pixels_per_css_pixel);
    auto svg_scale = svg_user_units_to_css_pixels_scale();
    if (svg_scale == Gfx::FloatSize { 1, 1 }) {
        // With no scale to amplify snapping errors or blur a raster, snap the
        // destination for crisp edges and let the image data use its regular
        // path (a cached display list for SVG image documents).
        decoded_image_data.paint(context, device_dst_rect.to_rounded<int>().to_type<float>(), computed_values().image_rendering());
        return;
    }
    // The raster is sampled into the destination under the replay transform, so
    // request it at the final on-screen pixel density, within sane bounds.
    constexpr int max_bitmap_dimension = 16384;
    auto bitmap_size = Gfx::IntSize {
        clamp(static_cast<int>(ceilf(device_dst_rect.width() * svg_scale.width())), 1, max_bitmap_dimension),
        clamp(static_cast<int>(ceilf(device_dst_rect.height() * svg_scale.height())), 1, max_bitmap_dimension),
    };
    decoded_image_data.paint(context, device_dst_rect, computed_values().image_rendering(), bitmap_size);
}

void ImagePaintable::paint(DisplayListRecordingContext& context, PaintPhase phase) const
{
    if (!is_visible())
        return;

    Paintable::paint(context, phase);

    if (phase == PaintPhase::Foreground) {
        auto image_rect = absolute_rect();
        auto image_rect_device_pixels = context.rounded_device_rect(image_rect);
        auto renders_as_alt_text = m_is_svg_image ? m_renders_as_alt_text : !m_image_provider.is_image_available();
        if (renders_as_alt_text) {
            if (!m_alt_text.is_empty()) {
                auto enclosing_rect = context.enclosing_device_rect(image_rect).to_type<int>();
                context.display_list_recorder().save();
                context.display_list_recorder().add_clip_rect(enclosing_rect);
                paint_alt_text(context, layout_node(), enclosing_rect, m_alt_text, computed_values().color());
                context.display_list_recorder().restore();
            }
        } else if (auto decoded_image_data = m_image_provider.decoded_image_data()) {
            if (m_is_svg_image) {
                paint_svg_image(context, *decoded_image_data, image_rect);
            } else {
                ScopedCornerRadiusClip corner_clip { context, image_rect_device_pixels, normalized_border_radii_data(ShrinkRadiiForBorders::Yes) };
                auto image_int_rect_device_pixels = image_rect_device_pixels.to_type<int>();

                // https://drafts.csswg.org/css-images/#the-object-fit
                auto object_fit = computed_values().object_fit();

                auto intrinsic_size = m_image_provider.intrinsic_size()
                                          .map([](auto size) { return size.template to_type<int>(); })
                                          .value_or(image_int_rect_device_pixels.size());

                auto draw_rect = get_replaced_box_painting_area(*this, context, object_fit, intrinsic_size);
                if (!draw_rect.is_empty()) {
                    auto draw_rect_needs_clip = !image_int_rect_device_pixels.contains(draw_rect);
                    if (draw_rect_needs_clip) {
                        context.display_list_recorder().save();
                        context.display_list_recorder().add_clip_rect(image_int_rect_device_pixels);
                    }
                    decoded_image_data->paint(context, draw_rect.to_type<float>(), computed_values().image_rendering());
                    if (draw_rect_needs_clip)
                        context.display_list_recorder().restore();
                }
            }
        }

        if (selection_state() != SelectionState::None) {
            auto selection_background_color = selection_style().background_color;
            if (selection_background_color.alpha() > 0)
                context.display_list_recorder().fill_rect(image_rect_device_pixels.to_type<int>(), selection_background_color);
        }
    }
}

}
