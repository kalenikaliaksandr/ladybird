/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/CanvasHost.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaBackendContext.h>

namespace Compositor {

CanvasHost::CanvasHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

CanvasHost::~CanvasHost() = default;

bool CanvasHost::create_context(Web::Painting::CanvasContextId canvas_context_id, Gfx::IntSize size, bool alpha)
{
    if (m_contexts.contains(canvas_context_id))
        return false;
    if (size.is_empty() || static_cast<i64>(size.width()) * static_cast<i64>(size.height()) > Gfx::max_canvas_area)
        return false;

    auto format = alpha ? Gfx::BitmapFormat::BGRA8888 : Gfx::BitmapFormat::BGRx8888;
    auto player = make<Gfx::CanvasCommandPlayer>(m_skia_backend_context, size, format, Gfx::AlphaType::Premultiplied);

    // https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-settings:concept-canvas-alpha
    // "Thus, the bitmap of such a context starts off as opaque black instead of transparent black"
    // AD-HOC: Skia hands out a fully transparent surface by default; only clear when alpha is disabled.
    if (!alpha)
        player->clear(Gfx::Color::Black);

    m_contexts.set(canvas_context_id, move(player));
    return true;
}

void CanvasHost::destroy_context(Web::Painting::CanvasContextId canvas_context_id)
{
    m_contexts.remove(canvas_context_id);
}

bool CanvasHost::has_context(Web::Painting::CanvasContextId canvas_context_id) const
{
    return m_contexts.contains(canvas_context_id);
}

void CanvasHost::apply_commands(Web::Painting::CanvasContextId canvas_context_id, Gfx::CanvasCommandList const& commands)
{
    auto player = m_contexts.get(canvas_context_id);
    VERIFY(player.has_value());
    (*player)->play(commands);
}

RefPtr<Gfx::PaintingSurface> CanvasHost::context_surface(Web::Painting::CanvasContextId canvas_context_id)
{
    auto player = m_contexts.get(canvas_context_id);
    if (!player.has_value())
        return nullptr;
    return (*player)->surface();
}

Gfx::ShareableBitmap CanvasHost::read_back_pixels(Web::Painting::CanvasContextId canvas_context_id, Gfx::IntRect rect)
{
    auto surface = context_surface(canvas_context_id);
    if (!surface)
        return {};

    auto clipped_rect = rect.intersected(surface->rect());
    if (clipped_rect.is_empty())
        return {};

    auto bitmap_or_error = Gfx::Bitmap::create_shareable(Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, clipped_rect.size());
    if (bitmap_or_error.is_error())
        return {};

    auto bitmap = bitmap_or_error.release_value();
    surface->flush();
    surface->read_into_bitmap(*bitmap, clipped_rect.location());
    return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

}
