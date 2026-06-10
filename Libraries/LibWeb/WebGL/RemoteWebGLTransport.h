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
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Export.h>
#include <LibWeb/WebGL/Types.h>

namespace Web::WebGL {

// WebContent's channel to a remote WebGL host in the Compositor process. RefCounted and
// bound to one compositor connection: a context created against one connection keeps a
// stable channel for its whole lifetime, and if the compositor dies the channel simply
// goes dead (sends are dropped, sync calls return empty replies) - the context is lost.
class WEB_API RemoteWebGLTransport : public AK::RefCounted<RemoteWebGLTransport> {
public:
    virtual ~RemoteWebGLTransport() = default;

    struct CreateResult {
        bool success { false };
        Vector<String> supported_extensions;
    };
    virtual CreateResult create_context(WebGLContextId, WebGLVersion, bool depth, bool stencil, bool antialias) = 0;
    virtual void destroy_context(WebGLContextId) = 0;
    virtual void send_commands(WebGLContextId, ByteBuffer const&) = 0;
    virtual ByteBuffer sync_call(WebGLContextId, ByteBuffer request) = 0;

    // Synchronously reads back the live drawing buffer; pixel data travels as shared
    // memory because the generic sync-call framing cannot carry file descriptors.
    virtual Gfx::ShareableBitmap read_back_drawing_buffer(WebGLContextId) = 0;
};

}
