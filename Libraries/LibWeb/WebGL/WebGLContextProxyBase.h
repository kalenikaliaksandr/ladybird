/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/String.h>
#include <AK/Vector.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Size.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Web::WebGL {

// State and transport shared by every generated WebGLContextProxy method: the pending
// command batch, the client-side object id allocator, and the lost flag. When the
// context is lost (the compositor went away), recording becomes a no-op and sync calls
// return empty replies for the explicit lost-context path.
//
// Also carries the non-GL surface of the old in-process OpenGLContext so the WebGL
// implementation above the seam keeps compiling unchanged: most of those members
// dissolve into no-ops here because their work now happens in the Compositor (for
// example, binding "the default framebuffer" simply records object id 0, which the
// host resolves to its real drawing-buffer framebuffer).
class WEB_API WebGLContextProxyBase {
    AK_MAKE_NONCOPYABLE(WebGLContextProxyBase);
    AK_MAKE_NONMOVABLE(WebGLContextProxyBase);

public:
    WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport>, Painting::CanvasContextId, WebGLVersion, Vector<String> supported_extensions);
    ~WebGLContextProxyBase();

    static Painting::CanvasContextId allocate_canvas_context_id();

    void flush_commands();
    void set_lost() { m_lost = true; }
    Painting::CanvasContextId canvas_context_id() const { return m_canvas_context_id; }

    // Rebinds a lost context to a freshly created host context on a new compositor connection
    // (after a compositor restart). The GL object namespace is gone, so pending commands and
    // the string cache are dropped; the page re-creates its objects after webglcontextrestored.
    void restore(NonnullRefPtr<RemoteWebGLTransport>, Vector<String> supported_extensions);

    // The non-GL part of the seam, mirroring the old in-process OpenGLContext.
    void make_current() { }
    void notify_content_will_change() { }
    u32 default_framebuffer() const { return 0; }
    u32 default_renderbuffer() const { return 0; }
    WebGLVersion webgl_version() const { return m_webgl_version; }
    Vector<String> const& get_supported_opengl_extensions() const { return m_supported_extensions; }
    void set_size(Gfx::IntSize const&);

    // Flushes pending GL commands, then asks the Compositor to sample this context's
    // drawing buffer for the element's canvas surface slot.
    void prepare_canvas_surface_for_compositing(Compositor::CompositorContextId target_context_id, Painting::CanvasId canvas_id, bool preserve_drawing_buffer);

    // Synchronously reads back the live drawing buffer (premultiplied BGRA8888).
    RefPtr<Gfx::Bitmap> read_back_drawing_buffer(Gfx::IntRect const&);

    void read_pixels_into_pixel_pack_buffer(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, long long offset);
    void read_buffer_sub_data(GLenum target, long long offset, Bytes destination);

    // Records an image upload (texImage2D/texSubImage2D with a TexImageSource). The
    // pixels travel to the Compositor as the bitmap's anonymous buffer (a file
    // descriptor) instead of inline command bytes, and the host performs the pixel
    // conversion next to GL. A destination_size of {0, 0} keeps the image's natural
    // size.
    void tex_image2d_from_bitmap(GLenum target, GLint level, GLint internalformat, GLenum format, GLenum type, Gfx::DecodedImageFrame, Gfx::IntSize destination_size, bool flip_y, bool premultiply_alpha);
    void tex_sub_image2d_from_bitmap(GLenum target, GLint level, GLint xoffset, GLint yoffset, GLenum format, GLenum type, Gfx::DecodedImageFrame, Gfx::IntSize destination_size, bool flip_y, bool premultiply_alpha);

    // A GL error detected locally (currently only an upload too large to send). Reported by
    // the WebGL context's getError() ahead of the host's errors.
    GLenum take_pending_local_error()
    {
        auto error = m_pending_local_error;
        m_pending_local_error = 0;
        return error;
    }

protected:
    // A single batch never approaches IPC's message size limit; commands that would are
    // dropped with a FIXME (chunked uploads are a follow-up).
    static constexpr size_t max_pending_command_bytes = 4 * MiB;
    static constexpr size_t max_single_command_bytes = 48 * MiB;

    WebGLObjectId allocate_object_id() { return m_next_object_id++; }

    // GL_OUT_OF_MEMORY; spelled out to avoid pulling a GL header into this header.
    static constexpr GLenum out_of_memory_error = 0x0505;

    template<typename Command>
    void record(Command const& command, ReadonlyBytes inline_data = {})
    {
        if (m_lost)
            return;
        if (inline_data.size() > max_single_command_bytes) {
            // Too large to carry inline over IPC (message payloads are capped). Report
            // OUT_OF_MEMORY so the page observes the failure rather than silently sampling an
            // incomplete texture or buffer.
            // FIXME: Carry large uploads over shared memory so they aren't size-limited.
            if (m_pending_local_error == 0)
                m_pending_local_error = out_of_memory_error;
            return;
        }
        m_commands.append(command, inline_data);
        if (m_commands.size_in_bytes() >= max_pending_command_bytes)
            flush_commands();
    }

    ByteBuffer send_sync_call(ByteBuffer request);
    bool is_lost() const { return m_lost; }

    // Cached so getParameter(VENDOR / RENDERER / ...) costs one round trip per name. The
    // value is heap-allocated so the pointer get_string() returns into it stays valid even
    // when a later insertion rehashes the map (a small string lives inline in the ByteBuffer
    // object, so storing the ByteBuffer by value would dangle earlier-returned pointers).
    HashMap<GLenum, NonnullOwnPtr<ByteBuffer>> m_string_cache;

private:
    NonnullRefPtr<RemoteWebGLTransport> m_transport;
    Painting::CanvasContextId m_canvas_context_id { 0 };
    WebGLVersion m_webgl_version { WebGLVersion::WebGL1 };
    Vector<String> m_supported_extensions;
    WebGLCommandList m_commands;
    // Image uploads recorded into m_commands reference these by index; flushed (and
    // index numbering restarted) together with the batch.
    Vector<Gfx::DecodedImageFrame> m_pending_bitmaps;
    u32 m_next_object_id { 1 };
    bool m_lost { false };
    GLenum m_pending_local_error { 0 };
};

}
