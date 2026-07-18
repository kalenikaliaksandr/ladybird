/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/CanvasCommandPlayer.h>
#include <LibGfx/Color.h>
#include <LibGfx/PainterSkia.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaUtils.h>
#include <core/SkCanvas.h>
#include <core/SkPaint.h>

namespace Gfx {

CanvasCommandPlayer::CanvasCommandPlayer(RefPtr<SkiaBackendContext> skia_backend_context, IntSize size, BitmapFormat format, AlphaType alpha_type, CanvasSurfaceResolver canvas_surface_resolver)
    : m_surface(PaintingSurface::create_with_size(size, format, alpha_type, move(skia_backend_context)))
    , m_painter(make<PainterSkia>(*m_surface))
    , m_canvas_surface_resolver(move(canvas_surface_resolver))
{
}

CanvasCommandPlayer::~CanvasCommandPlayer() = default;

NonnullRefPtr<PaintingSurface> CanvasCommandPlayer::surface() const
{
    return m_surface;
}

void CanvasCommandPlayer::clear(Color color)
{
    m_painter->clear_rect({ {}, m_surface->size().to_type<float>() }, color);
}

void CanvasCommandPlayer::play(CanvasCommandList const& command_list)
{
    for (auto const& command : command_list.commands())
        command.visit([&](auto const& command) { play_command(command); });
}

void CanvasCommandPlayer::play_command(CanvasCommands::ClearRect const& command)
{
    m_painter->clear_rect(command.rect, command.color);
}

void CanvasCommandPlayer::play_command(CanvasCommands::FillRect const& command)
{
    m_painter->fill_rect(command.rect, command.color);
}

void CanvasCommandPlayer::play_command(CanvasCommands::DrawBitmap const& command)
{
    m_painter->draw_bitmap(command.dst_rect, command.frame, command.src_rect, command.scaling_mode, command.filter, command.global_alpha, command.compositing_and_blending_operator);
}

void CanvasCommandPlayer::play_command(CanvasCommands::DrawCanvas const& command)
{
    if (!m_canvas_surface_resolver)
        return;

    auto* source_surface = m_canvas_surface_resolver(command.source_canvas_id);
    if (!source_surface)
        return;

    auto image = source_surface->sk_image_snapshot<sk_sp<SkImage>>();
    if (!image)
        return;

    SkPaint paint;
    if (command.filter.has_value())
        paint.setImageFilter(to_skia_image_filter(command.filter.value()));
    paint.setAlpha(static_cast<u8>(command.global_alpha * 255));
    paint.setBlender(to_skia_blender(command.compositing_and_blending_operator));

    m_surface->canvas().drawImageRect(
        image.get(),
        to_skia_rect(command.src_rect),
        to_skia_rect(command.dst_rect),
        to_skia_sampling_options(command.scaling_mode),
        &paint,
        SkCanvas::kStrict_SrcRectConstraint);
}

void CanvasCommandPlayer::play_command(CanvasCommands::FillPath const& command)
{
    // Shadows are recorded as blurred solid-color fills; everything else goes through the general paint-style overload.
    if (command.blur_radius > 0 && command.style.has<Color>()) {
        m_painter->fill_path(command.path, command.style.get<Color>(), command.winding_rule, command.blur_radius, command.compositing_and_blending_operator);
        return;
    }

    m_painter->fill_path(command.path, resolve_paint_style(command.style), command.filter, command.global_alpha, command.compositing_and_blending_operator, command.winding_rule);
}

void CanvasCommandPlayer::play_command(CanvasCommands::StrokePath const& command)
{
    if (command.blur_radius > 0 && command.style.has<Color>()) {
        m_painter->stroke_path(command.path, command.style.get<Color>(), command.thickness, command.blur_radius, command.compositing_and_blending_operator, command.cap_style, command.join_style, command.miter_limit, command.dash_array, command.dash_offset);
        return;
    }

    m_painter->stroke_path(command.path, resolve_paint_style(command.style), command.filter, command.thickness, command.global_alpha, command.compositing_and_blending_operator, command.cap_style, command.join_style, command.miter_limit, command.dash_array, command.dash_offset);
}

static constexpr size_t max_tracked_state_commands = 4096;

void CanvasCommandPlayer::record_state_command(CanvasCommand&& command)
{
    if (m_state_commands_overflowed)
        return;
    // Consecutive absolute transforms supersede each other, so a rAF loop
    // calling setTransform()/translate() every frame stays bounded.
    if (command.has<CanvasCommands::SetTransform>() && !m_state_commands.is_empty() && m_state_commands.last().has<CanvasCommands::SetTransform>()) {
        m_state_commands.last() = move(command);
        return;
    }
    if (m_state_commands.size() >= max_tracked_state_commands) {
        m_state_commands.clear();
        m_state_commands_overflowed = true;
        return;
    }
    m_state_commands.append(move(command));
}

void CanvasCommandPlayer::play_command(CanvasCommands::SetTransform const& command)
{
    record_state_command(CanvasCommands::SetTransform { command.transform });
    m_painter->set_transform(command.transform);
}

void CanvasCommandPlayer::play_command(CanvasCommands::Save const&)
{
    record_state_command(CanvasCommands::Save {});
    m_painter->save();
}

void CanvasCommandPlayer::play_command(CanvasCommands::Restore const&)
{
    // Drop the state recorded since (and including) the matching Save.
    while (!m_state_commands.is_empty()) {
        bool was_save = m_state_commands.last().has<CanvasCommands::Save>();
        (void)m_state_commands.take_last();
        if (was_save)
            break;
    }
    m_painter->restore();
}

void CanvasCommandPlayer::play_command(CanvasCommands::ClipPath const& command)
{
    record_state_command(CanvasCommands::ClipPath { command.path.clone(), command.winding_rule });
    m_painter->clip(command.path, command.winding_rule);
}

void CanvasCommandPlayer::play_command(CanvasCommands::Reset const&)
{
    m_state_commands.clear();
    m_state_commands_overflowed = false;
    m_painter->reset();
}

void CanvasCommandPlayer::play_command(CanvasCommands::ClearCanvas const& command)
{
    // The clear must ignore the painter's current transform and clip without
    // disturbing them (the recording context's drawing state survives a bitmap
    // reset). Skia offers no clip-defeating clear, so pop the painter back to
    // its pristine state, clear everything, and replay the tracked state
    // commands — no surface-sized temporary needed.
    if (!m_state_commands_overflowed) {
        m_painter->reset();
        m_painter->clear_rect({ {}, m_surface->size().to_type<float>() }, command.color);
        for (auto const& state_command : m_state_commands) {
            state_command.visit(
                [&](CanvasCommands::SetTransform const& transform) { m_painter->set_transform(transform.transform); },
                [&](CanvasCommands::Save const&) { m_painter->save(); },
                [&](CanvasCommands::ClipPath const& clip) { m_painter->clip(clip.path, clip.winding_rule); },
                [&](auto const&) { VERIFY_NOT_REACHED(); });
        }
        return;
    }

    // The state log overflowed (a pathological unbalanced state-op stream), so
    // the painter state cannot be re-established after a reset; clear by
    // writing pixel strips straight into the surface instead, which bypasses
    // canvas state with only a strip-sized temporary.
    auto size = m_surface->size();
    int const strip_height = min(size.height(), 64);
    if (strip_height == 0)
        return;
    auto bitmap_or_error = Bitmap::create(BitmapFormat::BGRA8888, AlphaType::Premultiplied, { size.width(), strip_height });
    if (bitmap_or_error.is_error())
        return;
    auto bitmap = bitmap_or_error.release_value();
    if (command.color != Color::Transparent) {
        for (int y = 0; y < bitmap->height(); ++y) {
            auto* scanline = bitmap->scanline(y);
            for (int x = 0; x < bitmap->width(); ++x)
                scanline[x] = command.color.value();
        }
    }
    for (int y = 0; y < size.height(); y += strip_height)
        m_surface->write_from_bitmap(*bitmap, { 0, y });
}

NonnullRefPtr<PaintStyle> CanvasCommandPlayer::resolve_paint_style(CanvasPaintStyle const& style) const
{
    auto with_color_stops = [](auto paint_style, auto const& gradient) -> NonnullRefPtr<PaintStyle> {
        paint_style->set_color_stops(Vector<ColorStop> { gradient.color_stops });
        if (gradient.repeat_length.has_value())
            paint_style->set_repeat_length(*gradient.repeat_length);
        return paint_style;
    };

    return style.visit(
        [](Color const& color) -> NonnullRefPtr<PaintStyle> {
            return MUST(SolidColorPaintStyle::create(color));
        },
        [&](CanvasLinearGradient const& gradient) -> NonnullRefPtr<PaintStyle> {
            return with_color_stops(MUST(CanvasLinearGradientPaintStyle::create(gradient.start_point, gradient.end_point)), gradient);
        },
        [&](CanvasRadialGradient const& gradient) -> NonnullRefPtr<PaintStyle> {
            return with_color_stops(MUST(CanvasRadialGradientPaintStyle::create(gradient.start_center, gradient.start_radius, gradient.end_center, gradient.end_radius)), gradient);
        },
        [&](CanvasConicGradient const& gradient) -> NonnullRefPtr<PaintStyle> {
            return with_color_stops(MUST(CanvasConicGradientPaintStyle::create(gradient.center, gradient.start_angle)), gradient);
        },
        [&](CanvasPatternStyle const& pattern) -> NonnullRefPtr<PaintStyle> {
            auto paint_style = MUST(CanvasPatternPaintStyle::create(pattern.image, pattern.repetition));
            if (pattern.transform.has_value())
                paint_style->set_transform(*pattern.transform);
            return paint_style;
        });
}

}
