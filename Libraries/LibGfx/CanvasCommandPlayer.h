/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/RefPtr.h>
#include <LibGfx/CanvasCommandList.h>
#include <LibGfx/Forward.h>

namespace Gfx {

class CanvasCommandPlayer {
    AK_MAKE_NONCOPYABLE(CanvasCommandPlayer);
    AK_MAKE_NONMOVABLE(CanvasCommandPlayer);

public:
    // committed_only asks for the last committed (presented) surface even when a
    // live drawing surface exists on the same host: placeholder canvas sources
    // must never expose uncommitted pixels.
    using CanvasSurfaceResolver = Function<PaintingSurface const*(u64, bool committed_only)>;

    // Whether a committed source canvas's frame is origin-tainted; consulted when
    // compositing committed-only DrawCanvas commands so the taint decision is
    // atomic with the pixels actually composited.
    using CanvasTaintResolver = Function<bool(u64)>;

    CanvasCommandPlayer(RefPtr<SkiaBackendContext>, IntSize, BitmapFormat, AlphaType, CanvasSurfaceResolver = {}, CanvasTaintResolver = {});
    ~CanvasCommandPlayer();

    NonnullRefPtr<PaintingSurface> surface() const;

    void clear(Color);

    void play(CanvasCommandList const&);

    // True once a committed-only DrawCanvas composited a source whose committed
    // frame was tainted; cleared when the bitmap is replaced (ClearCanvas).
    bool has_composited_tainted_source() const { return m_composited_tainted_source; }

private:
    void play_command(CanvasCommands::ClearRect const&);
    void play_command(CanvasCommands::FillRect const&);
    void play_command(CanvasCommands::DrawBitmap const&);
    void play_command(CanvasCommands::DrawCanvas const&);
    void play_command(CanvasCommands::FillPath const&);
    void play_command(CanvasCommands::StrokePath const&);
    void play_command(CanvasCommands::SetTransform const&);
    void play_command(CanvasCommands::Save const&);
    void play_command(CanvasCommands::Restore const&);
    void play_command(CanvasCommands::ClipPath const&);
    void play_command(CanvasCommands::Reset const&);
    void play_command(CanvasCommands::ClearCanvas const&);

    NonnullRefPtr<PaintStyle> resolve_paint_style(CanvasPaintStyle const&) const;

    void record_state_command(CanvasCommand&&);

    NonnullRefPtr<PaintingSurface> m_surface;
    NonnullOwnPtr<PainterSkia> m_painter;
    CanvasSurfaceResolver m_canvas_surface_resolver;
    CanvasTaintResolver m_canvas_taint_resolver;

    // The transform/clip/save commands currently in effect, so ClearCanvas can
    // reset the painter, clear the whole surface, and re-establish the state
    // without a surface-sized temporary. Reset empties it. Consecutive absolute
    // transforms coalesce, and a pathological unbalanced stream that exceeds the
    // cap flips the overflow flag instead of growing without bound (ClearCanvas
    // then clears by writing pixel strips).
    Vector<CanvasCommand> m_state_commands;
    bool m_state_commands_overflowed { false };

    bool m_composited_tainted_source { false };
};

}
