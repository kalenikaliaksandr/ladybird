/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/WebGLCommandReplayer.h>
#include <Compositor/WebGLHost.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Compositor {

HostWebGLContext::HostWebGLContext(NonnullOwnPtr<Web::WebGL::OpenGLContext> gl_context)
    : m_gl_context(move(gl_context))
{
}

OwnPtr<HostWebGLContext> HostWebGLContext::create(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options, Gfx::IntSize initial_size)
{
    if (initial_size.width() < 1 || initial_size.width() > Web::WebGL::max_webgl_drawing_buffer_dimension
        || initial_size.height() < 1 || initial_size.height() > Web::WebGL::max_webgl_drawing_buffer_dimension)
        return {};
    auto gl_context = Web::WebGL::OpenGLContext::create(move(skia_backend_context), version, options);
    if (!gl_context)
        return {};
    gl_context->set_size(initial_size);
    return adopt_own(*new HostWebGLContext(gl_context.release_nonnull()));
}

ErrorOr<void> HostWebGLContext::execute_commands(ReadonlyBytes bytes)
{
    m_gl_context->make_current();

    // A non-preserving context's drawing buffer is cleared after being prepared for
    // compositing, but the clear is deferred to here (the start of the next frame's
    // commands) so a readback taken before then still sees the rendered frame.
    if (m_needs_clear_before_next_frame) {
        m_gl_context->clear_buffer_to_default_values();
        m_needs_clear_before_next_frame = false;
    }

    return Web::WebGL::WebGLCommandList::for_each_command(bytes, [&](auto const& command, ReadonlyBytes payload) -> ErrorOr<void> {
        return replay_webgl_command(*m_gl_context, m_objects, command, payload);
    });
}

ErrorOr<ByteBuffer> HostWebGLContext::execute_sync_call(ReadonlyBytes request)
{
    m_gl_context->make_current();
    return handle_webgl_sync_call(*m_gl_context, m_objects, request);
}

ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> HostWebGLContext::prepare_for_compositing(bool preserve_drawing_buffer)
{
    // Flush all pending GL work so Skia samples the finished drawing buffer. The
    // default framebuffer was written behind Skia's back, so discard cached snapshots
    // before the display-list player asks Skia for an image.
    m_gl_context->present(/* preserve_drawing_buffer= */ true);

    auto drawing_surface = m_gl_context->surface();
    if (!drawing_surface)
        return Error::from_string_literal("WebGL context has no drawing buffer");
    m_gl_context->notify_content_will_change();

    // Defer the clear (see execute_commands) so a readback before the next frame still sees
    // this frame.
    if (!preserve_drawing_buffer)
        m_needs_clear_before_next_frame = true;

    return drawing_surface.release_nonnull();
}

Gfx::ShareableBitmap HostWebGLContext::read_back_drawing_buffer(Gfx::IntRect rect)
{
    m_gl_context->make_current();
    m_gl_context->present(/* preserve_drawing_buffer= */ true);
    auto surface = m_gl_context->surface();
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

WebGLHost::WebGLHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

HostWebGLContext* WebGLHost::create_context(Web::Painting::CanvasContextId canvas_context_id, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options, Gfx::IntSize initial_size)
{
    if (!m_skia_backend_context)
        return nullptr;
    if (m_contexts.contains(canvas_context_id))
        return nullptr;
    auto context = HostWebGLContext::create(*m_skia_backend_context, version, options, initial_size);
    if (!context)
        return nullptr;
    auto* context_ptr = context.ptr();
    m_contexts.set(canvas_context_id, context.release_nonnull());
    return context_ptr;
}

void WebGLHost::destroy_context(Web::Painting::CanvasContextId canvas_context_id)
{
    m_contexts.remove(canvas_context_id);
}

HostWebGLContext* WebGLHost::context(Web::Painting::CanvasContextId canvas_context_id)
{
    return m_contexts.get(canvas_context_id).value_or(nullptr);
}

}
