/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/OwnPtr.h>
#include <AK/RefPtr.h>
#include <Compositor/WebGLObjectMap.h>
#include <LibGfx/Forward.h>
#include <LibWeb/WebGL/OpenGLContext.h>

namespace Compositor {

// Reply-buffer sizes in synchronous WebGL messages come from the client; the host never
// allocates more than these per reply.
inline constexpr size_t max_webgl_sync_reply_size = 1 * MiB;
inline constexpr size_t max_webgl_readback_size = 16 * MiB;

// String lists (shader varyings, uniform names) are bounded well above anything a real
// program produces.
inline constexpr size_t max_webgl_string_list_entries = 16384;

// One remote WebGL context: the real ANGLE-backed OpenGLContext plus the table mapping
// the client's object ids to GL names. Each WebGL call arrives as its own IPC message
// and executes immediately on the compositor main loop; GL-level validation stays in
// ANGLE (the context runs in WebGL-compatibility mode), so the dispatcher only has to
// keep the id table and message payload sizes honest.
class HostWebGLContext {
public:
    static OwnPtr<HostWebGLContext> create(NonnullRefPtr<Gfx::SkiaBackendContext>, Web::WebGL::OpenGLContext::WebGLVersion, Web::WebGL::OpenGLContext::DrawingBufferOptions);

    // EGL current-context state is per-thread and the compositor thread interleaves GL
    // messages from many contexts with Skia work, so this queries EGL rather than
    // caching "last current" anywhere.
    void make_current_if_needed() { m_gl_context->make_current_if_needed(); }

    Web::WebGL::OpenGLContext& gl_context() { return *m_gl_context; }
    WebGLObjectMap& objects() { return m_objects; }

    // Flushes pending GL work, copies the drawing buffer into the publish surface (GL
    // writes bypass Skia copy-on-write, so publishing must copy), and clears the
    // drawing buffer unless the context preserves it.
    ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> snapshot_for_present(bool preserve_drawing_buffer);
    ErrorOr<void> set_drawing_buffer_size(int width, int height);
    Gfx::ShareableBitmap read_back_drawing_buffer();

private:
    HostWebGLContext(NonnullRefPtr<Gfx::SkiaBackendContext>, NonnullOwnPtr<Web::WebGL::OpenGLContext>);

    NonnullRefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    NonnullOwnPtr<Web::WebGL::OpenGLContext> m_gl_context;
    RefPtr<Gfx::PaintingSurface> m_publish_surface;
    WebGLObjectMap m_objects;
};

// All remote WebGL contexts created by one WebContent connection; reaped with it.
class WebGLHost {
public:
    explicit WebGLHost(RefPtr<Gfx::SkiaBackendContext>);

    bool create_context(u64 webgl_context_id, Web::WebGL::OpenGLContext::WebGLVersion, Web::WebGL::OpenGLContext::DrawingBufferOptions);
    void destroy_context(u64 webgl_context_id);
    HostWebGLContext* context(u64 webgl_context_id);

private:
    RefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    HashMap<u64, NonnullOwnPtr<HostWebGLContext>> m_contexts;
};

}
