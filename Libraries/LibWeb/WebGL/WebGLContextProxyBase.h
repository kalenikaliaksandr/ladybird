/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/String.h>
#include <AK/Vector.h>
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
// return zeroed replies.
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
    WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport>, WebGLContextId, WebGLVersion, Vector<String> supported_extensions);
    ~WebGLContextProxyBase();

    static WebGLContextId allocate_webgl_context_id();

    void flush_commands();
    void set_lost() { m_lost = true; }

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

    // Records a Present op targeting the canvas element's compositor surface slot and
    // flushes; this is the per-frame flush point.
    void present_to_compositor(Compositor::CompositorContextId target_context_id, Painting::CompositorSurfaceId surface_id, bool preserve_drawing_buffer);

    // Synchronously reads back the live drawing buffer (premultiplied BGRA8888).
    RefPtr<Gfx::Bitmap> read_back_drawing_buffer();

    void read_pixels_into_pixel_pack_buffer(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, long long offset);
    void read_buffer_sub_data(GLenum target, long long offset, Bytes destination);

protected:
    // A single batch never approaches IPC's message size limit; commands that would are
    // dropped with a FIXME (chunked uploads are a follow-up).
    static constexpr size_t max_pending_command_bytes = 4 * MiB;
    static constexpr size_t max_single_command_bytes = 48 * MiB;

    WebGLObjectId allocate_object_id() { return m_next_object_id++; }

    template<typename Command>
    void record(Command const& command, ReadonlyBytes inline_data = {})
    {
        if (m_lost)
            return;
        if (inline_data.size() > max_single_command_bytes) {
            dbgln("FIXME: Dropping {} byte WebGL command; chunked uploads are not implemented yet", inline_data.size());
            return;
        }
        m_commands.append(command, inline_data);
        if (m_commands.size_in_bytes() >= max_pending_command_bytes)
            flush_commands();
    }

    ByteBuffer send_sync_call(ByteBuffer request);

    // Cached so getParameter(VENDOR / RENDERER / ...) costs one round trip per name.
    HashMap<GLenum, ByteBuffer> m_string_cache;

private:
    NonnullRefPtr<RemoteWebGLTransport> m_transport;
    WebGLContextId m_webgl_context_id { 0 };
    WebGLVersion m_webgl_version { WebGLVersion::WebGL1 };
    Vector<String> m_supported_extensions;
    WebGLCommandList m_commands;
    u32 m_next_object_id { 1 };
    bool m_lost { false };
};

}
