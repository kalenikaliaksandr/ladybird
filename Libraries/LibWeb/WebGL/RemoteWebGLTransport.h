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
    virtual void send_commands(Painting::CanvasContextId, ByteBuffer const&) = 0;
    virtual ByteBuffer sync_call(Painting::CanvasContextId, ByteBuffer request) = 0;
};

}
