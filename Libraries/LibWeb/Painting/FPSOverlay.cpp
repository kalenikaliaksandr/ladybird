/*
 * Copyright (c) 2025-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/Math.h>
#include <LibGfx/Font/TypefaceSkia.h>
#include <LibGfx/Painter.h>
#include <LibGfx/PainterSkia.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/Path.h>
#include <LibWeb/Painting/FPSOverlay.h>

namespace Web::Painting {

static constexpr float padding = 12.0f;
static constexpr float margin = 16.0f;
static constexpr float graph_width = 400.0f;
static constexpr float graph_height = 160.0f;
static constexpr float gap = 8.0f;
static constexpr float time_window = 10.0f;

void FPSOverlay::ensure_fonts()
{
    if (m_title_font)
        return;

    auto typeface = Gfx::TypefaceSkia::find_typeface_for_code_point('A', 400, 5, 0);
    if (typeface.is_error() || !typeface.value())
        return;

    m_title_font = typeface.value()->font(32);
    m_label_font = typeface.value()->font(20);
}

void FPSOverlay::update_fps_sample()
{
    auto now = MonotonicTime::now();

    if (m_last_frame_time.has_value()) {
        auto frame_duration = now - m_last_frame_time.value();
        auto frame_seconds = frame_duration.to_seconds_f64();

        if (frame_seconds > 1.0) {
            m_current_fps = 0.0f;
        } else if (frame_seconds > 0.0) {
            float instantaneous_fps = static_cast<float>(1.0 / frame_seconds);
            if (m_current_fps == 0.0f)
                m_current_fps = instantaneous_fps;
            else
                m_current_fps = (m_current_fps * 0.9f) + (instantaneous_fps * 0.1f);
        }
    }

    m_last_frame_time = now;
    m_fps_history.enqueue(FpsSample { now, m_current_fps });
}

FPSOverlay::TextLayout FPSOverlay::draw_fps_text(Gfx::Painter& painter)
{
    auto fps_text = ByteString::formatted("FPS: {:.1f}", static_cast<double>(m_current_fps));

    Gfx::Path text_path;
    text_path.move_to({ 0, 0 });
    text_path.text(Utf8View { fps_text.view() }, *m_title_font);
    auto text_bounds = text_path.bounding_box();

    float overlay_width = max(text_bounds.width() + padding * 2, graph_width + padding * 2);

    // Draw text background
    Gfx::FloatRect text_bg_rect { margin, margin, overlay_width, text_bounds.height() + padding * 2 };
    painter.fill_rect(text_bg_rect, Gfx::Color(0, 0, 0, 180));

    // Draw text by translating path to correct position
    Gfx::Path positioned_text;
    positioned_text.move_to({ margin + padding - text_bounds.left(), margin + padding - text_bounds.top() });
    positioned_text.text(Utf8View { fps_text.view() }, *m_title_font);
    painter.fill_path(positioned_text, Gfx::Color(138, 100, 229), Gfx::WindingRule::Nonzero);

    return { overlay_width, text_bg_rect.bottom() };
}

void FPSOverlay::draw_fps_graph(Gfx::Painter& painter, float overlay_width, float text_bg_bottom)
{
    auto now = m_last_frame_time.value();

    // Graph area
    float graph_left = margin + padding;
    float graph_top = text_bg_bottom + gap + padding;
    float graph_right = graph_left + graph_width;
    float graph_bottom = graph_top + graph_height;

    // Graph background
    Gfx::FloatRect graph_bg_rect { margin, text_bg_bottom + gap, overlay_width, graph_height + padding * 2 };
    painter.fill_rect(graph_bg_rect, Gfx::Color(0, 0, 0, 180));

    // Downsample FPS history into display buckets
    static constexpr size_t num_buckets = 200;
    auto window_start = now - AK::Duration::from_seconds_f64(time_window);

    struct Bucket {
        float fps_sum { 0.0f };
        size_t count { 0 };
    };
    Bucket buckets[num_buckets] {};

    for (size_t i = 0; i < m_fps_history.size(); ++i) {
        auto const& sample = m_fps_history.at(i);
        if (sample.timestamp < window_start)
            continue;
        float time_offset = static_cast<float>((sample.timestamp - window_start).to_seconds_f64());
        size_t bucket_index = static_cast<size_t>((time_offset / time_window) * num_buckets);
        if (bucket_index >= num_buckets)
            bucket_index = num_buckets - 1;
        buckets[bucket_index].fps_sum += sample.fps;
        buckets[bucket_index].count++;
    }

    // Find max FPS from bucket averages for Y-axis scaling
    float max_fps = 60.0f;
    for (size_t i = 0; i < num_buckets; ++i) {
        if (buckets[i].count > 0) {
            float avg = buckets[i].fps_sum / buckets[i].count;
            if (avg > max_fps)
                max_fps = avg;
        }
    }
    // Round up to nearest multiple of 30
    float y_max = AK::ceil<float>(max_fps / 30.0f) * 30.0f;
    if (y_max < 60.0f)
        y_max = 60.0f;

    // Draw reference lines and labels
    float min_label_spacing = 35.0f;
    float step = AK::ceil<float>((min_label_spacing * y_max) / (graph_height * 30.0f)) * 30.0f;
    for (float fps_level = step; fps_level <= y_max; fps_level += step) {
        float y = graph_bottom - (fps_level / y_max) * graph_height;

        // Reference line
        Gfx::Path line_path;
        line_path.move_to({ graph_left, y });
        line_path.line_to({ graph_right, y });
        painter.stroke_path(line_path, Gfx::Color(255, 255, 255, 60), 1.0f);

        // Label
        auto label = ByteString::formatted("{}", static_cast<int>(fps_level));
        Gfx::Path label_path;
        label_path.move_to({ graph_left + 2, y - 2 });
        label_path.text(Utf8View { label.view() }, *m_label_font);
        painter.fill_path(label_path, Gfx::Color(255, 255, 255, 120), Gfx::WindingRule::Nonzero);
    }

    // Clip graph drawing to the graph background area
    Gfx::Path clip_path;
    clip_path.move_to({ graph_left, graph_top });
    clip_path.line_to({ graph_right, graph_top });
    clip_path.line_to({ graph_right, graph_bottom });
    clip_path.line_to({ graph_left, graph_bottom });
    clip_path.close();
    painter.save();
    painter.clip(clip_path, Gfx::WindingRule::Nonzero);

    // Build graph path from downsampled buckets
    Gfx::Path line_path;
    Gfx::Path fill_path;
    bool first_point = true;

    for (size_t i = 0; i < num_buckets; ++i) {
        if (buckets[i].count == 0)
            continue;
        float avg_fps = buckets[i].fps_sum / buckets[i].count;
        float x = graph_left + (static_cast<float>(i) + 0.5f) / num_buckets * graph_width;
        float y = graph_bottom - (min(avg_fps, y_max) / y_max) * graph_height;

        if (first_point) {
            line_path.move_to({ x, y });
            fill_path.move_to({ x, graph_bottom });
            fill_path.line_to({ x, y });
            first_point = false;
        } else {
            line_path.line_to({ x, y });
            fill_path.line_to({ x, y });
        }
    }

    if (!first_point) {
        // Close fill path along the bottom
        auto last_point = line_path.last_point();
        fill_path.line_to({ last_point.x(), graph_bottom });
        fill_path.close();

        // Draw filled area
        painter.fill_path(fill_path, Gfx::Color(138, 100, 229, 40), Gfx::WindingRule::Nonzero);

        // Draw line
        painter.stroke_path(line_path, Gfx::Color(138, 100, 229), 3.0f);
    }

    painter.restore();
}

void FPSOverlay::draw(Gfx::PaintingSurface& surface)
{
    update_fps_sample();

    ensure_fonts();
    if (!m_title_font)
        return;

    auto painter = Gfx::PainterSkia(surface);
    auto [overlay_width, text_bg_bottom] = draw_fps_text(painter);
    draw_fps_graph(painter, overlay_width, text_bg_bottom);
}

}
