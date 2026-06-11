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
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/Size.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/WebGL/OpenGLContext.h>

namespace Compositor {

// One remote WebGL context: the real ANGLE-backed OpenGLContext plus the table mapping
// the client's object ids to GL names. Command batches execute immediately on receipt,
// on the compositor main loop; GL-level validation stays in ANGLE (the context runs in
// WebGL-compatibility mode), so replay only has to keep the byte stream and the id
// table honest.
class HostWebGLContext {
public:
    // Returns null if the initial drawing buffer size is invalid or context creation
    // failed; the buffer is reallocated by SetDrawingBufferSize commands on resize.
    static OwnPtr<HostWebGLContext> create(NonnullRefPtr<Gfx::SkiaBackendContext>, Web::WebGL::OpenGLContext::WebGLVersion, Web::WebGL::OpenGLContext::DrawingBufferOptions, Gfx::IntSize initial_size);

    ErrorOr<void> execute_commands(ReadonlyBytes);
    ErrorOr<ByteBuffer> execute_sync_call(ReadonlyBytes request);
    Gfx::ShareableBitmap read_back_drawing_buffer(Gfx::IntRect);
    ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> prepare_for_compositing(bool preserve_drawing_buffer);

    Web::WebGL::OpenGLContext& gl_context() { return *m_gl_context; }

private:
    explicit HostWebGLContext(NonnullOwnPtr<Web::WebGL::OpenGLContext>);

    NonnullOwnPtr<Web::WebGL::OpenGLContext> m_gl_context;
    WebGLObjectMap m_objects;
    // For a non-preserving context the post-compositing clear is deferred until just before
    // the next frame's commands, so a readback between compositing preparation and the next
    // frame (drawImage, toDataURL, getImageData of the WebGL canvas) still observes the
    // rendered frame.
    bool m_needs_clear_before_next_frame { false };
};

// All remote WebGL contexts created by one WebContent connection; reaped with it.
class WebGLHost {
public:
    explicit WebGLHost(RefPtr<Gfx::SkiaBackendContext>);

    // Returns the new context, or null if the id is taken or context creation failed.
    HostWebGLContext* create_context(Web::Painting::CanvasContextId, Web::WebGL::OpenGLContext::WebGLVersion, Web::WebGL::OpenGLContext::DrawingBufferOptions, Gfx::IntSize initial_size);
    void destroy_context(Web::Painting::CanvasContextId);
    HostWebGLContext* context(Web::Painting::CanvasContextId);

private:
    RefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    HashMap<Web::Painting::CanvasContextId, NonnullOwnPtr<HostWebGLContext>> m_contexts;
};

}
