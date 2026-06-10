/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/OwnPtr.h>
#include <AK/RefPtr.h>
#include <LibGfx/CanvasCommandList.h>
#include <LibGfx/Forward.h>

namespace Gfx {

// Replays canvas command deltas onto a persistent painting surface. One player exists
// per canvas context; painter state (transform, clip, save stack) persists across
// play() calls, so a delta may be cut at any point in the recorded stream.
class CanvasCommandPlayer {
    AK_MAKE_NONCOPYABLE(CanvasCommandPlayer);
    AK_MAKE_NONMOVABLE(CanvasCommandPlayer);

public:
    explicit CanvasCommandPlayer(RefPtr<SkiaBackendContext>);
    ~CanvasCommandPlayer();

    // Null until an Initialize op has played; replaced by each subsequent Initialize.
    RefPtr<PaintingSurface> surface() const;

    bool play(CanvasCommandList const&);

    void prune_caches();

private:
    void play_command(CanvasCommands::Initialize const&);
    void play_command(CanvasCommands::ClearRect const&);
    void play_command(CanvasCommands::FillRect const&);
    void play_command(CanvasCommands::DrawBitmap const&);
    void play_command(CanvasCommands::FillPath const&);
    void play_command(CanvasCommands::StrokePath const&);
    void play_command(CanvasCommands::SetTransform const&);
    void play_command(CanvasCommands::Save const&);
    void play_command(CanvasCommands::Restore const&);
    void play_command(CanvasCommands::ClipPath const&);
    void play_command(CanvasCommands::Reset const&);

    NonnullRefPtr<PaintStyle> resolve_paint_style(CanvasPaintStyle const&) const;

    RefPtr<SkiaBackendContext> m_skia_backend_context;
    RefPtr<PaintingSurface> m_surface;
    OwnPtr<PainterSkia> m_painter;
};

}
