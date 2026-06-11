/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/WebGLHost.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/SkiaBackendContext.h>

#include <core/SkCanvas.h>
#include <core/SkImage.h>

namespace Compositor {

static constexpr int max_webgl_drawing_buffer_dimension = 16384;

HostWebGLContext::HostWebGLContext(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, NonnullOwnPtr<Web::WebGL::OpenGLContext> gl_context)
    : m_skia_backend_context(move(skia_backend_context))
    , m_gl_context(move(gl_context))
{
}

OwnPtr<HostWebGLContext> HostWebGLContext::create(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    auto gl_context = Web::WebGL::OpenGLContext::create(skia_backend_context, version, options);
    if (!gl_context)
        return {};
    // The drawing buffer is allocated lazily on first use and needs a non-empty size;
    // the client follows up with its real size before issuing any drawing commands.
    gl_context->set_size({ 1, 1 });
    return adopt_own(*new HostWebGLContext(move(skia_backend_context), gl_context.release_nonnull()));
}

ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> HostWebGLContext::snapshot_for_present(bool preserve_drawing_buffer)
{
    make_current_if_needed();
    // Flush all pending GL work so Skia samples the finished frame; the clear (for
    // non-preserving contexts) must wait until after the copy.
    m_gl_context->present(/* preserve_drawing_buffer= */ true);

    auto drawing_surface = m_gl_context->surface();
    if (!drawing_surface)
        return Error::from_string_literal("WebGL context has no drawing buffer");
    // The drawing buffer was written behind Skia's back; drop any caches before sampling.
    m_gl_context->notify_content_will_change();

    if (!m_publish_surface || m_publish_surface->size() != drawing_surface->size())
        m_publish_surface = Gfx::PaintingSurface::create_with_size(drawing_surface->size(), Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, m_skia_backend_context);

    auto& canvas = m_publish_surface->canvas();
    canvas.drawImage(drawing_surface->sk_image_snapshot<sk_sp<SkImage>>(), 0, 0);
    m_publish_surface->flush();

    if (!preserve_drawing_buffer)
        m_gl_context->clear_buffer_to_default_values();

    return NonnullRefPtr { *m_publish_surface };
}

ErrorOr<void> HostWebGLContext::set_drawing_buffer_size(int width, int height)
{
    if (width < 1 || height < 1 || width > max_webgl_drawing_buffer_dimension || height > max_webgl_drawing_buffer_dimension)
        return Error::from_string_literal("Invalid WebGL drawing buffer size");
    m_gl_context->set_size({ width, height });
    m_gl_context->make_current();
    return {};
}

Gfx::ShareableBitmap HostWebGLContext::read_back_drawing_buffer()
{
    make_current_if_needed();
    m_gl_context->present(/* preserve_drawing_buffer= */ true);
    auto surface = m_gl_context->surface();
    if (!surface)
        return {};
    auto bitmap_or_error = Gfx::Bitmap::create_shareable(Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, surface->size());
    if (bitmap_or_error.is_error())
        return {};
    auto bitmap = bitmap_or_error.release_value();
    surface->read_into_bitmap(*bitmap);
    return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

WebGLHost::WebGLHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

bool WebGLHost::create_context(u64 webgl_context_id, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    if (!m_skia_backend_context)
        return false;
    if (m_contexts.contains(webgl_context_id))
        return false;
    auto context = HostWebGLContext::create(*m_skia_backend_context, version, options);
    if (!context)
        return false;
    m_contexts.set(webgl_context_id, context.release_nonnull());
    return true;
}

void WebGLHost::destroy_context(u64 webgl_context_id)
{
    m_contexts.remove(webgl_context_id);
}

HostWebGLContext* WebGLHost::context(u64 webgl_context_id)
{
    return m_contexts.get(webgl_context_id).value_or(nullptr);
}

}
