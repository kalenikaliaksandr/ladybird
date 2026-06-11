/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/RefCounted.h>
#include <AK/RefPtr.h>
#include <LibGfx/Forward.h>
#include <LibWeb/Compositor/Types.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Web::Compositor {

// WebContent's channel to a remote 2D canvas context hosted in the Compositor process.
// RefCounted and bound to one compositor connection: a context created against one
// connection keeps a stable channel for its whole lifetime, and if the compositor dies
// the channel simply goes dead - the context's backing storage is lost. Canvas contexts
// are connection-level (not tied to any compositor display-list context), so detached
// canvases and canvases that move between navigables need no rebinding.
class WEB_API RemoteCanvasTransport : public AK::RefCounted<RemoteCanvasTransport> {
public:
    virtual ~RemoteCanvasTransport() = default;

    virtual bool create_context(Painting::CanvasContextId) = 0;
    virtual void destroy_context(Painting::CanvasContextId) = 0;
    virtual void update_commands(Painting::CanvasContextId, Gfx::CanvasCommandList const&) = 0;

    // Binds the context's live backing surface to the element's canvas surface slot in
    // the given display-list context, so the page display list can sample it.
    virtual void prepare_canvas_surface(Painting::CanvasContextId, CompositorContextId target_context_id, Painting::CanvasId) = 0;

    // Synchronously reads back the backing surface (premultiplied BGRA8888); pixel data
    // travels as shared memory.
    virtual RefPtr<Gfx::Bitmap> read_back_pixels(Painting::CanvasContextId, Gfx::IntRect const&) = 0;
};

}
