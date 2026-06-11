/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteString.h>
#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/String.h>
#include <AK/Vector.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Size.h>
#include <LibWeb/Export.h>
#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <LibWeb/WebGL/Types.h>

namespace Web::WebGL {

// State shared by every generated WebGLContextProxy method: the transport, the
// client-side object id allocator, and the lost flag. When the context is lost (the
// compositor went away), every call becomes a no-op and getters return zeroes.
//
// Also carries the non-GL surface of the old in-process OpenGLContext so the WebGL
// implementation above the seam keeps compiling unchanged: most of those members
// dissolve into no-ops here because their work now happens in the Compositor (for
// example, binding "the default framebuffer" simply sends object id 0, which the host
// resolves to its real drawing-buffer framebuffer).
class WEB_API WebGLContextProxyBase {
    AK_MAKE_NONCOPYABLE(WebGLContextProxyBase);
    AK_MAKE_NONMOVABLE(WebGLContextProxyBase);

public:
    WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport>, u64 webgl_context_id, WebGLVersion, Vector<String> supported_extensions);
    ~WebGLContextProxyBase();

    static u64 allocate_webgl_context_id();

    // Per-call transport: every call already went out; nothing is buffered.
    void flush_commands() { }
    void set_lost() { m_lost = true; }
    bool is_lost() const { return m_lost; }
    u64 webgl_context_id() const { return m_webgl_context_id; }

    // The non-GL part of the seam, mirroring the old in-process OpenGLContext.
    void make_current() { }
    void notify_content_will_change() { }
    void allocate_painting_surface_if_needed() { }
    RefPtr<Gfx::PaintingSurface> surface();
    u32 default_framebuffer() const { return 0; }
    u32 default_renderbuffer() const { return 0; }
    WebGLVersion webgl_version() const { return m_webgl_version; }
    Vector<String> get_supported_opengl_extensions() const { return m_supported_extensions; }
    void set_size(Gfx::IntSize const&);

    // Publishes the drawing buffer into the canvas element's compositor surface slot.
    void present_to_compositor(u64 target_context_id, u64 surface_id, bool preserve_drawing_buffer);

    // Synchronously reads back the live drawing buffer (premultiplied BGRA8888).
    RefPtr<Gfx::Bitmap> read_back_drawing_buffer();

    void read_pixels_into_pixel_pack_buffer(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, long long offset);
    void read_buffer_sub_data(GLenum target, long long offset, Bytes destination);

protected:
    // A single message must stay clear of IPC's 64 MiB payload limit; larger uploads
    // are dropped with a FIXME (chunking is a follow-up).
    static constexpr size_t max_single_message_payload_bytes = 48 * MiB;

    WebGLObjectId allocate_object_id() { return m_next_object_id++; }
    RemoteWebGLTransport& transport() { return *m_transport; }

    // Cached so getParameter(VENDOR / RENDERER / ...) costs one round trip per name;
    // ByteString keeps the NUL terminator the GL-shaped getter hands out.
    HashMap<GLenum, ByteString> m_string_cache;

private:
    NonnullRefPtr<RemoteWebGLTransport> m_transport;
    u64 m_webgl_context_id { 0 };
    WebGLVersion m_webgl_version { WebGLVersion::WebGL1 };
    Vector<String> m_supported_extensions;
    u32 m_next_object_id { 1 };
    bool m_lost { false };
};

}
