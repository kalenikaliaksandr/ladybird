/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/TypeCasts.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/CanvasCommandList.h>

namespace Web::Painting {

CanvasPaintStyle to_canvas_paint_style(Gfx::PaintStyle const& paint_style)
{
    if (auto const* solid_color = as_if<Gfx::SolidColorPaintStyle>(paint_style))
        return solid_color->color();

    if (auto const* linear_gradient = as_if<Gfx::CanvasLinearGradientPaintStyle>(paint_style)) {
        return CanvasLinearGradient {
            .start_point = linear_gradient->start_point(),
            .end_point = linear_gradient->end_point(),
            .color_stops = Vector<Gfx::ColorStop> { linear_gradient->color_stops() },
            .repeat_length = linear_gradient->repeat_length(),
        };
    }

    if (auto const* radial_gradient = as_if<Gfx::CanvasRadialGradientPaintStyle>(paint_style)) {
        return CanvasRadialGradient {
            .start_center = radial_gradient->start_center(),
            .start_radius = radial_gradient->start_radius(),
            .end_center = radial_gradient->end_center(),
            .end_radius = radial_gradient->end_radius(),
            .color_stops = Vector<Gfx::ColorStop> { radial_gradient->color_stops() },
            .repeat_length = radial_gradient->repeat_length(),
        };
    }

    if (auto const* conic_gradient = as_if<Gfx::CanvasConicGradientPaintStyle>(paint_style)) {
        return CanvasConicGradient {
            .center = conic_gradient->center(),
            .start_angle = conic_gradient->start_angle(),
            .color_stops = Vector<Gfx::ColorStop> { conic_gradient->color_stops() },
            .repeat_length = conic_gradient->repeat_length(),
        };
    }

    if (auto const* pattern = as_if<Gfx::CanvasPatternPaintStyle>(paint_style)) {
        return CanvasPatternStyle {
            .image = pattern->image(),
            .repetition = pattern->repetition(),
            .transform = pattern->transform(),
        };
    }

    // Canvas code only feeds canvas paint styles; anything else is a bug in the
    // caller.
    VERIFY_NOT_REACHED();
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasLinearGradient const& gradient)
{
    TRY(encoder.encode(gradient.start_point));
    TRY(encoder.encode(gradient.end_point));
    TRY(encoder.encode(gradient.color_stops));
    TRY(encoder.encode(gradient.repeat_length));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasLinearGradient> decode(Decoder& decoder)
{
    return Web::Painting::CanvasLinearGradient {
        .start_point = TRY(decoder.decode<Gfx::FloatPoint>()),
        .end_point = TRY(decoder.decode<Gfx::FloatPoint>()),
        .color_stops = TRY(decoder.decode<Vector<Gfx::ColorStop>>()),
        .repeat_length = TRY(decoder.decode<Optional<float>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasRadialGradient const& gradient)
{
    TRY(encoder.encode(gradient.start_center));
    TRY(encoder.encode(gradient.start_radius));
    TRY(encoder.encode(gradient.end_center));
    TRY(encoder.encode(gradient.end_radius));
    TRY(encoder.encode(gradient.color_stops));
    TRY(encoder.encode(gradient.repeat_length));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasRadialGradient> decode(Decoder& decoder)
{
    return Web::Painting::CanvasRadialGradient {
        .start_center = TRY(decoder.decode<Gfx::FloatPoint>()),
        .start_radius = TRY(decoder.decode<float>()),
        .end_center = TRY(decoder.decode<Gfx::FloatPoint>()),
        .end_radius = TRY(decoder.decode<float>()),
        .color_stops = TRY(decoder.decode<Vector<Gfx::ColorStop>>()),
        .repeat_length = TRY(decoder.decode<Optional<float>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasConicGradient const& gradient)
{
    TRY(encoder.encode(gradient.center));
    TRY(encoder.encode(gradient.start_angle));
    TRY(encoder.encode(gradient.color_stops));
    TRY(encoder.encode(gradient.repeat_length));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasConicGradient> decode(Decoder& decoder)
{
    return Web::Painting::CanvasConicGradient {
        .center = TRY(decoder.decode<Gfx::FloatPoint>()),
        .start_angle = TRY(decoder.decode<float>()),
        .color_stops = TRY(decoder.decode<Vector<Gfx::ColorStop>>()),
        .repeat_length = TRY(decoder.decode<Optional<float>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasPatternStyle const& pattern)
{
    TRY(encoder.encode(pattern.image));
    TRY(encoder.encode(pattern.repetition));
    TRY(encoder.encode(pattern.transform));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasPatternStyle> decode(Decoder& decoder)
{
    return Web::Painting::CanvasPatternStyle {
        .image = TRY(decoder.decode<Optional<Gfx::DecodedImageFrame>>()),
        .repetition = TRY(decoder.decode<Gfx::CanvasPatternPaintStyle::Repetition>()),
        .transform = TRY(decoder.decode<Optional<Gfx::AffineTransform>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::Initialize const& command)
{
    TRY(encoder.encode(command.size));
    TRY(encoder.encode(command.format));
    TRY(encoder.encode(command.alpha_type));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::Initialize> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::Initialize {
        .size = TRY(decoder.decode<Gfx::IntSize>()),
        .format = TRY(decoder.decode<Gfx::BitmapFormat>()),
        .alpha_type = TRY(decoder.decode<Gfx::AlphaType>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::ClearRect const& command)
{
    TRY(encoder.encode(command.rect));
    TRY(encoder.encode(command.color));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::ClearRect> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::ClearRect {
        .rect = TRY(decoder.decode<Gfx::FloatRect>()),
        .color = TRY(decoder.decode<Gfx::Color>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::FillRect const& command)
{
    TRY(encoder.encode(command.rect));
    TRY(encoder.encode(command.color));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::FillRect> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::FillRect {
        .rect = TRY(decoder.decode<Gfx::FloatRect>()),
        .color = TRY(decoder.decode<Gfx::Color>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::DrawBitmap const& command)
{
    TRY(encoder.encode(command.frame));
    TRY(encoder.encode(command.dst_rect));
    TRY(encoder.encode(command.src_rect));
    TRY(encoder.encode(command.scaling_mode));
    TRY(encoder.encode(command.filter));
    TRY(encoder.encode(command.global_alpha));
    TRY(encoder.encode(command.compositing_and_blending_operator));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::DrawBitmap> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::DrawBitmap {
        .frame = TRY(decoder.decode<Gfx::DecodedImageFrame>()),
        .dst_rect = TRY(decoder.decode<Gfx::FloatRect>()),
        .src_rect = TRY(decoder.decode<Gfx::IntRect>()),
        .scaling_mode = TRY(decoder.decode<Gfx::ScalingMode>()),
        .filter = TRY(decoder.decode<Optional<Gfx::Filter>>()),
        .global_alpha = TRY(decoder.decode<float>()),
        .compositing_and_blending_operator = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::FillPath const& command)
{
    TRY(encoder.encode(command.path));
    TRY(encoder.encode(command.style));
    TRY(encoder.encode(command.winding_rule));
    TRY(encoder.encode(command.blur_radius));
    TRY(encoder.encode(command.filter));
    TRY(encoder.encode(command.global_alpha));
    TRY(encoder.encode(command.compositing_and_blending_operator));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::FillPath> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::FillPath {
        .path = TRY(decoder.decode<Gfx::Path>()),
        .style = TRY(decoder.decode<Web::Painting::CanvasPaintStyle>()),
        .winding_rule = TRY(decoder.decode<Gfx::WindingRule>()),
        .blur_radius = TRY(decoder.decode<float>()),
        .filter = TRY(decoder.decode<Optional<Gfx::Filter>>()),
        .global_alpha = TRY(decoder.decode<float>()),
        .compositing_and_blending_operator = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::StrokePath const& command)
{
    TRY(encoder.encode(command.path));
    TRY(encoder.encode(command.style));
    TRY(encoder.encode(command.thickness));
    TRY(encoder.encode(command.cap_style));
    TRY(encoder.encode(command.join_style));
    TRY(encoder.encode(command.miter_limit));
    TRY(encoder.encode(command.dash_array));
    TRY(encoder.encode(command.dash_offset));
    TRY(encoder.encode(command.blur_radius));
    TRY(encoder.encode(command.filter));
    TRY(encoder.encode(command.global_alpha));
    TRY(encoder.encode(command.compositing_and_blending_operator));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::StrokePath> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::StrokePath {
        .path = TRY(decoder.decode<Gfx::Path>()),
        .style = TRY(decoder.decode<Web::Painting::CanvasPaintStyle>()),
        .thickness = TRY(decoder.decode<float>()),
        .cap_style = TRY(decoder.decode<Gfx::Path::CapStyle>()),
        .join_style = TRY(decoder.decode<Gfx::Path::JoinStyle>()),
        .miter_limit = TRY(decoder.decode<float>()),
        .dash_array = TRY(decoder.decode<Vector<float>>()),
        .dash_offset = TRY(decoder.decode<float>()),
        .blur_radius = TRY(decoder.decode<float>()),
        .filter = TRY(decoder.decode<Optional<Gfx::Filter>>()),
        .global_alpha = TRY(decoder.decode<float>()),
        .compositing_and_blending_operator = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::SetTransform const& command)
{
    TRY(encoder.encode(command.transform));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::SetTransform> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::SetTransform {
        .transform = TRY(decoder.decode<Gfx::AffineTransform>()),
    };
}

template<>
ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Save const&)
{
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::Save> decode(Decoder&)
{
    return Web::Painting::CanvasCommands::Save {};
}

template<>
ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Restore const&)
{
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::Restore> decode(Decoder&)
{
    return Web::Painting::CanvasCommands::Restore {};
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::ClipPath const& command)
{
    TRY(encoder.encode(command.path));
    TRY(encoder.encode(command.winding_rule));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::ClipPath> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::ClipPath {
        .path = TRY(decoder.decode<Gfx::Path>()),
        .winding_rule = TRY(decoder.decode<Gfx::WindingRule>()),
    };
}

template<>
ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Reset const&)
{
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::Reset> decode(Decoder&)
{
    return Web::Painting::CanvasCommands::Reset {};
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommands::DrawCanvasContext const& command)
{
    TRY(encoder.encode(command.source_context_id));
    TRY(encoder.encode(command.dst_rect));
    TRY(encoder.encode(command.src_rect));
    TRY(encoder.encode(command.scaling_mode));
    TRY(encoder.encode(command.filter));
    TRY(encoder.encode(command.global_alpha));
    TRY(encoder.encode(command.compositing_and_blending_operator));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommands::DrawCanvasContext> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommands::DrawCanvasContext {
        .source_context_id = TRY(decoder.decode<Web::Painting::CanvasContextId>()),
        .dst_rect = TRY(decoder.decode<Gfx::FloatRect>()),
        .src_rect = TRY(decoder.decode<Gfx::IntRect>()),
        .scaling_mode = TRY(decoder.decode<Gfx::ScalingMode>()),
        .filter = TRY(decoder.decode<Optional<Gfx::Filter>>()),
        .global_alpha = TRY(decoder.decode<float>()),
        .compositing_and_blending_operator = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::CanvasCommandList const& command_list)
{
    TRY(encoder.encode(command_list.commands()));
    return {};
}

template<>
ErrorOr<Web::Painting::CanvasCommandList> decode(Decoder& decoder)
{
    return Web::Painting::CanvasCommandList { TRY(decoder.decode<Vector<Web::Painting::CanvasCommand>>()) };
}

}
