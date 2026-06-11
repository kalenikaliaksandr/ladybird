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
#include <LibGfx/DecodedImageFrame.h>
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
    virtual CreateResult create_context(Painting::CanvasContextId, WebGLVersion, bool depth, bool stencil, bool antialias) = 0;
    virtual void destroy_context(Painting::CanvasContextId) = 0;

    // Image uploads in the batch reference `bitmaps` by index; the bitmaps travel as
    // shared memory (each bitmap's anonymous buffer) rather than inline command bytes.
    virtual void send_commands(Painting::CanvasContextId, ByteBuffer const&, Vector<Gfx::DecodedImageFrame> const& bitmaps) = 0;
    virtual void prepare_canvas_surface(Painting::CanvasContextId, Compositor::CompositorContextId target_context_id, Painting::CanvasId, bool preserve_drawing_buffer) = 0;
    virtual ByteBuffer sync_call(Painting::CanvasContextId, ByteBuffer request) = 0;

    // Synchronously reads back the live drawing buffer; pixel data travels as shared
    // memory because the generic sync-call framing cannot carry file descriptors.
    virtual Gfx::ShareableBitmap read_back_drawing_buffer(Painting::CanvasContextId, Gfx::IntRect const&) = 0;
};

}
