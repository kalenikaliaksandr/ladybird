/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/Color.h>
#include <LibGfx/PainterSkia.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/Painting/CanvasCommandPlayer.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Painting {

// Command lists arrive over IPC from WebContent, and LibIPC decodes enums with a plain
// cast that does no range check. The recorder should only produce values that Skia can
// consume, so invalid values are protocol bugs.
static bool is_valid(Gfx::CompositingAndBlendingOperator op)
{
    return static_cast<int>(op) >= static_cast<int>(Gfx::CompositingAndBlendingOperator::Normal)
        && static_cast<int>(op) <= static_cast<int>(Gfx::CompositingAndBlendingOperator::PlusLighter);
}

static bool is_valid(Gfx::WindingRule rule)
{
    return rule == Gfx::WindingRule::Nonzero || rule == Gfx::WindingRule::EvenOdd;
}

static bool is_valid(Gfx::ScalingMode mode)
{
    switch (mode) {
    case Gfx::ScalingMode::None:
    case Gfx::ScalingMode::Bilinear:
    case Gfx::ScalingMode::BilinearMipmap:
    case Gfx::ScalingMode::NearestNeighbor:
        return true;
    }
    return false;
}

static bool is_valid(Gfx::Path::CapStyle cap)
{
    switch (cap) {
    case Gfx::Path::CapStyle::Butt:
    case Gfx::Path::CapStyle::Round:
    case Gfx::Path::CapStyle::Square:
        return true;
    }
    return false;
}

static bool is_valid(Gfx::Path::JoinStyle join)
{
    switch (join) {
    case Gfx::Path::JoinStyle::Miter:
    case Gfx::Path::JoinStyle::Round:
    case Gfx::Path::JoinStyle::Bevel:
        return true;
    }
    return false;
}

CanvasCommandPlayer::CanvasCommandPlayer(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

CanvasCommandPlayer::~CanvasCommandPlayer() = default;

RefPtr<Gfx::PaintingSurface> CanvasCommandPlayer::surface() const
{
    return m_surface;
}

bool CanvasCommandPlayer::play(CanvasCommandList const& command_list, DisplayListResourceStorage& resource_storage)
{
    m_current_resource_storage = &resource_storage;
    for (auto const& command : command_list.commands())
        command.visit([&](auto const& command) { play_command(command); });
    m_current_resource_storage = nullptr;
    if (m_painter)
        m_painter->prune_caches();
    return false;
}

void CanvasCommandPlayer::play_command(CanvasCommands::Initialize const& command)
{
    m_painter = nullptr;
    m_surface = nullptr;

    if (command.size.is_empty())
        return;
    VERIFY(static_cast<i64>(command.size.width()) * static_cast<i64>(command.size.height()) <= max_canvas_area);

    VERIFY(Gfx::is_valid_bitmap_format(static_cast<u32>(command.format)));
    VERIFY(command.format != Gfx::BitmapFormat::Invalid);
    VERIFY(Gfx::is_valid_alpha_type(static_cast<u32>(command.alpha_type)));

    m_surface = Gfx::PaintingSurface::create_with_size(command.size, command.format, command.alpha_type, m_skia_backend_context);
    m_painter = make<Gfx::PainterSkia>(*m_surface);
}

void CanvasCommandPlayer::play_command(CanvasCommands::ClearRect const& command)
{
    VERIFY(m_painter);
    m_painter->clear_rect(command.rect, command.color);
}

void CanvasCommandPlayer::play_command(CanvasCommands::FillRect const& command)
{
    VERIFY(m_painter);
    m_painter->fill_rect(command.rect, command.color);
}

void CanvasCommandPlayer::play_command(CanvasCommands::DrawBitmap const& command)
{
    VERIFY(m_painter);
    VERIFY(is_valid(command.scaling_mode));
    VERIFY(is_valid(command.compositing_and_blending_operator));
    m_painter->draw_bitmap(command.dst_rect, command.frame, command.src_rect, command.scaling_mode, command.filter, command.global_alpha, command.compositing_and_blending_operator);
}

void CanvasCommandPlayer::play_command(CanvasCommands::FillPath const& command)
{
    VERIFY(m_painter);
    VERIFY(is_valid(command.winding_rule));
    VERIFY(is_valid(command.compositing_and_blending_operator));

    // Shadows are recorded as blurred solid-color fills; everything else goes through
    // the general paint-style overload.
    if (command.blur_radius > 0 && command.style.has<Gfx::Color>()) {
        m_painter->fill_path(command.path, command.style.get<Gfx::Color>(), command.winding_rule, command.blur_radius, command.compositing_and_blending_operator);
        return;
    }

    m_painter->fill_path(command.path, resolve_paint_style(command.style), command.filter, command.global_alpha, command.compositing_and_blending_operator, command.winding_rule);
}

void CanvasCommandPlayer::play_command(CanvasCommands::StrokePath const& command)
{
    VERIFY(m_painter);
    VERIFY(is_valid(command.cap_style));
    VERIFY(is_valid(command.join_style));
    VERIFY(is_valid(command.compositing_and_blending_operator));

    if (command.blur_radius > 0 && command.style.has<Gfx::Color>()) {
        m_painter->stroke_path(command.path, command.style.get<Gfx::Color>(), command.thickness, command.blur_radius, command.compositing_and_blending_operator, command.cap_style, command.join_style, command.miter_limit, command.dash_array, command.dash_offset);
        return;
    }

    m_painter->stroke_path(command.path, resolve_paint_style(command.style), command.filter, command.thickness, command.global_alpha, command.compositing_and_blending_operator, command.cap_style, command.join_style, command.miter_limit, command.dash_array, command.dash_offset);
}

void CanvasCommandPlayer::play_command(CanvasCommands::SetTransform const& command)
{
    VERIFY(m_painter);
    m_painter->set_transform(command.transform);
}

void CanvasCommandPlayer::play_command(CanvasCommands::Save const&)
{
    VERIFY(m_painter);
    m_painter->save();
}

void CanvasCommandPlayer::play_command(CanvasCommands::Restore const&)
{
    VERIFY(m_painter);
    m_painter->restore();
}

void CanvasCommandPlayer::play_command(CanvasCommands::ClipPath const& command)
{
    VERIFY(m_painter);
    VERIFY(is_valid(command.winding_rule));
    m_painter->clip(command.path, command.winding_rule);
}

void CanvasCommandPlayer::play_command(CanvasCommands::Reset const&)
{
    VERIFY(m_painter);
    m_painter->reset();
}

void CanvasCommandPlayer::play_command(CanvasCommands::DrawCanvasContext const& command)
{
    VERIFY(m_painter);
    VERIFY(is_valid(command.scaling_mode));
    VERIFY(is_valid(command.compositing_and_blending_operator));
    auto source_surface = m_current_resource_storage->canvas_context_surface(command.source_context_id);
    VERIFY(source_surface);

    // The copy-on-write Skia image snapshot makes self-draw (source == destination)
    // well-defined without reading the source surface back to the CPU.
    m_painter->draw_painting_surface(command.dst_rect, *source_surface, command.src_rect, command.scaling_mode, command.filter, command.global_alpha, command.compositing_and_blending_operator);
}

NonnullRefPtr<Gfx::PaintStyle> CanvasCommandPlayer::resolve_paint_style(CanvasPaintStyle const& style) const
{
    auto with_color_stops = [](auto paint_style, auto const& gradient) -> NonnullRefPtr<Gfx::PaintStyle> {
        paint_style->set_color_stops(Vector<Gfx::ColorStop> { gradient.color_stops });
        if (gradient.repeat_length.has_value())
            paint_style->set_repeat_length(*gradient.repeat_length);
        return paint_style;
    };

    return style.visit(
        [](Gfx::Color const& color) -> NonnullRefPtr<Gfx::PaintStyle> {
            return MUST(Gfx::SolidColorPaintStyle::create(color));
        },
        [&](CanvasLinearGradient const& gradient) -> NonnullRefPtr<Gfx::PaintStyle> {
            return with_color_stops(MUST(Gfx::CanvasLinearGradientPaintStyle::create(gradient.start_point, gradient.end_point)), gradient);
        },
        [&](CanvasRadialGradient const& gradient) -> NonnullRefPtr<Gfx::PaintStyle> {
            return with_color_stops(MUST(Gfx::CanvasRadialGradientPaintStyle::create(gradient.start_center, gradient.start_radius, gradient.end_center, gradient.end_radius)), gradient);
        },
        [&](CanvasConicGradient const& gradient) -> NonnullRefPtr<Gfx::PaintStyle> {
            return with_color_stops(MUST(Gfx::CanvasConicGradientPaintStyle::create(gradient.center, gradient.start_angle)), gradient);
        },
        [&](CanvasPatternStyle const& pattern) -> NonnullRefPtr<Gfx::PaintStyle> {
            auto paint_style = MUST(Gfx::CanvasPatternPaintStyle::create(pattern.image, pattern.repetition));
            if (pattern.transform.has_value())
                paint_style->set_transform(*pattern.transform);
            return paint_style;
        });
}

}
