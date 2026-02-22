/*
 * Copyright (c) 2025-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/CircularQueue.h>
#include <AK/Time.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Forward.h>

namespace Web::Painting {

class FPSOverlay {
public:
    void draw(Gfx::PaintingSurface&);

private:
    struct TextLayout {
        float overlay_width;
        float text_bg_bottom;
    };

    void ensure_fonts();
    void update_fps_sample();
    TextLayout draw_fps_text(Gfx::Painter&);
    void draw_fps_graph(Gfx::Painter&, float overlay_width, float text_bg_bottom);

    Optional<MonotonicTime> m_last_frame_time;
    float m_current_fps { 0.0f };
    RefPtr<Gfx::Font> m_title_font;
    RefPtr<Gfx::Font> m_label_font;

    struct FpsSample {
        MonotonicTime timestamp;
        float fps;
    };
    CircularQueue<FpsSample, 2048> m_fps_history;
};

}
