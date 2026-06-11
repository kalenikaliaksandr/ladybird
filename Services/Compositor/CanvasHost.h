/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/RefPtr.h>
#include <LibGfx/CanvasCommandPlayer.h>
#include <LibGfx/Forward.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Compositor {

// All remote 2D canvas contexts created by one WebContent connection; reaped with it.
// Each context is a CanvasCommandPlayer that replays command deltas onto a persistent
// surface. Contexts are connection-level: they are not tied to any compositor
// display-list context, so detached canvases (and, later, offscreen canvases) work the
// same as displayed ones. Displaying a canvas binds the player's surface into a
// display-list context's canvas surface slot via prepare_canvas_surface().
class CanvasHost {
public:
    explicit CanvasHost(RefPtr<Gfx::SkiaBackendContext>);
    ~CanvasHost();

    // Returns false if the id is taken.
    bool create_context(Web::Painting::CanvasContextId);
    void destroy_context(Web::Painting::CanvasContextId);
    bool has_context(Web::Painting::CanvasContextId) const;

    void apply_commands(Web::Painting::CanvasContextId, Gfx::CanvasCommandList const&);

    // Null when the context does not exist or no Initialize op has played yet.
    RefPtr<Gfx::PaintingSurface> context_surface(Web::Painting::CanvasContextId);

    Gfx::ShareableBitmap read_back_pixels(Web::Painting::CanvasContextId, Gfx::IntRect);

private:
    RefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    HashMap<Web::Painting::CanvasContextId, NonnullOwnPtr<Gfx::CanvasCommandPlayer>> m_contexts;
};

}
