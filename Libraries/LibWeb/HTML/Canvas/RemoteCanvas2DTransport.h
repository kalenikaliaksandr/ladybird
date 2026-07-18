/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/RefCounted.h>
#include <AK/RefPtr.h>
#include <LibGfx/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/Painting/OffscreenCanvasLink.h>

namespace Web::HTML {

class WEB_API RemoteCanvas2DTransport : public RefCounted<RemoteCanvas2DTransport> {
public:
    virtual ~RemoteCanvas2DTransport() = default;

    virtual bool create_context(Gfx::IntSize, bool alpha, Optional<Painting::OffscreenCanvasPlaceholderLink> = {}) = 0;
    virtual Optional<Painting::CanvasId> canvas_id() const = 0;
    virtual void destroy_context() = 0;

    virtual Painting::Canvas2DCommandStream& shared_stream() = 0;
    virtual void flush_shared_stream() = 0;

    struct PixelReadback {
        RefPtr<Gfx::Bitmap> bitmap;
        // The Compositor may know taint the client could not observe
        // synchronously (a tainted committed source composited at replay time).
        bool origin_clean { true };
    };
    virtual PixelReadback read_back_pixels(Gfx::IntRect const&) = 0;
};

}
