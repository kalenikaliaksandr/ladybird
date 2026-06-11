/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/Error.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/OwnPtr.h>
#include <AK/RefPtr.h>
#include <AK/String.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/Forward.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/WebGL/Types.h>

namespace Compositor {

class HostWebGLContext;

// All remote canvas contexts created by one WebContent connection; reaped with it.
// Contexts are connection-level: they are not tied to any compositor display-list
// context, so detached canvases (and, later, offscreen canvases) work the same as
// displayed ones. Displaying a canvas binds the context's surface into a display-list
// context's canvas surface slot via prepare_canvas_surface().
class CanvasHost {
public:
    struct CreateContextResult {
        bool success { false };
        Vector<String> supported_extensions;
    };

    explicit CanvasHost(RefPtr<Gfx::SkiaBackendContext>);
    ~CanvasHost();

    // Allocates the context's backing surface up front; surface parameters are fixed
    // for the context's lifetime, except for WebGL drawing-buffer resize commands.
    CreateContextResult create_context(Web::Painting::CanvasContextId, Web::Compositor::CanvasContextCreationAttributes const&);
    void destroy_context(Web::Painting::CanvasContextId);
    bool has_context(Web::Painting::CanvasContextId) const;

    void apply_commands(Web::Painting::CanvasContextId, Gfx::CanvasCommandList const&);
    void execute_webgl_commands(Web::Painting::CanvasContextId, ByteBuffer const&, Vector<Gfx::DecodedImageFrame> const&);
    ErrorOr<ByteBuffer> execute_webgl_sync_call(Web::Painting::CanvasContextId, ByteBuffer request);
    Web::WebGL::ReadPixelsResult read_pixels_robust_angle(Web::Painting::CanvasContextId, Web::WebGL::GLint x, Web::WebGL::GLint y, Web::WebGL::GLsizei width, Web::WebGL::GLsizei height, Web::WebGL::GLenum format, Web::WebGL::GLenum type, Web::WebGL::GLsizei buf_size, Core::AnonymousBuffer pixels);
    void read_buffer_sub_data(Web::Painting::CanvasContextId, Web::WebGL::GLenum target, Web::WebGL::GLintptr offset, Web::WebGL::GLintptr size, Core::AnonymousBuffer data);

    ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> prepare_surface(Web::Painting::CanvasContextId, bool preserve_drawing_buffer);
    Gfx::ShareableBitmap read_back_pixels(Web::Painting::CanvasContextId, Gfx::IntRect);

private:
    using Canvas2DContext = NonnullOwnPtr<Gfx::CanvasCommandPlayer>;
    using WebGLContext = NonnullOwnPtr<HostWebGLContext>;
    using Context = Variant<Canvas2DContext, WebGLContext>;

    Context* context(Web::Painting::CanvasContextId);
    OwnPtr<Gfx::CanvasCommandPlayer> create_2d_context(Gfx::IntSize, bool alpha);
    static Gfx::CanvasCommandPlayer& as_2d(Context&);
    static HostWebGLContext& as_webgl(Context&);

    RefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    HashMap<Web::Painting::CanvasContextId, Context> m_contexts;
};

}
