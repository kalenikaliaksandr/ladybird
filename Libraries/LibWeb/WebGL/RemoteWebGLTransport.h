/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/RefCounted.h>
#include <AK/String.h>
#include <AK/Vector.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/Forward.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/WebGL/Types.h>

namespace Web::WebGL {

// WebContent's channel to a remote WebGL host in the Compositor process. RefCounted and
// bound to one compositor connection: a context created against one connection keeps a
// stable channel for its whole lifetime, and if the compositor dies the channel simply
// goes dead - the context is lost. Failed transmissions on a live channel are treated as
// process invariants rather than recoverable WebGL errors.
class WEB_API RemoteWebGLTransport : public AK::RefCounted<RemoteWebGLTransport> {
public:
    virtual ~RemoteWebGLTransport() = default;

    struct CreateResult {
        bool success { false };
        Vector<String> supported_extensions;
    };
    // `initial_size` is the drawing buffer size at creation; later resizes travel as
    // SetDrawingBufferSize commands (a WebGL context survives canvas resizes).
    virtual CreateResult create_context(Painting::CanvasContextId, WebGLVersion, Gfx::IntSize initial_size, bool depth, bool stencil, bool antialias) = 0;
    virtual void destroy_context(Painting::CanvasContextId) = 0;
    virtual void send_commands(Painting::CanvasContextId, ByteBuffer const&) = 0;
    virtual void prepare_canvas_surface(Painting::CanvasContextId, Compositor::CompositorContextId target_context_id, Painting::CanvasId, bool preserve_drawing_buffer) = 0;
    virtual ByteBuffer sync_call(Painting::CanvasContextId, ByteBuffer request) = 0;
    virtual ReadPixelsResult read_pixels_robust_angle(Painting::CanvasContextId, GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, GLsizei buf_size, Core::AnonymousBuffer pixels) = 0;
    virtual void read_buffer_sub_data(Painting::CanvasContextId, GLenum target, GLintptr offset, GLintptr size, Core::AnonymousBuffer data) = 0;

    // Synchronously reads back the live drawing buffer; pixel data travels as shared
    // memory because the generic sync-call framing cannot carry file descriptors.
    virtual Gfx::ShareableBitmap read_back_drawing_buffer(Painting::CanvasContextId, Gfx::IntRect const&) = 0;
};

}
