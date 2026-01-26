/*
 * Copyright (c) 2024-2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Painting/ScrollFrame.h>

namespace Web::Painting {

struct ScrollFrameData {
    CSSPixelPoint own_offset;
    CSSPixelPoint max_offset;
    Optional<size_t> scrollable_parent_id;
};

class ScrollStateSnapshot {
public:
    static ScrollStateSnapshot create(Vector<NonnullRefPtr<ScrollFrame>> const& scroll_frames);

    CSSPixelPoint own_offset_for_frame_with_id(size_t id) const
    {
        if (id >= m_frames.size())
            return {};
        return m_frames[id].own_offset;
    }

    CSSPixelPoint max_offset_for_frame_with_id(size_t id) const
    {
        if (id >= m_frames.size())
            return {};
        return m_frames[id].max_offset;
    }

    Optional<size_t> scrollable_parent_for_frame_with_id(size_t id) const
    {
        if (id >= m_frames.size())
            return {};
        return m_frames[id].scrollable_parent_id;
    }

    // Apply scroll delta to a frame, returns true if scroll was applied
    bool apply_scroll_delta(size_t frame_id, CSSPixelPoint delta)
    {
        if (frame_id >= m_frames.size())
            return false;

        auto& frame = m_frames[frame_id];
        auto current = frame.own_offset; // Note: this is negated scroll offset
        auto max = frame.max_offset;

        // own_offset is negative, so we subtract delta to scroll down/right
        CSSPixelPoint new_offset {
            clamp(current.x() - delta.x(), -max.x(), CSSPixels(0)),
            clamp(current.y() - delta.y(), -max.y(), CSSPixels(0))
        };

        if (new_offset == current)
            return false;

        frame.own_offset = new_offset;
        return true;
    }

    // Walk parent chain trying to apply scroll
    bool scroll_frame_by_delta(size_t frame_id, CSSPixelPoint delta)
    {
        size_t current_id = frame_id;
        while (true) {
            if (apply_scroll_delta(current_id, delta))
                return true;

            auto parent_id = scrollable_parent_for_frame_with_id(current_id);
            if (!parent_id.has_value())
                return false;
            current_id = parent_id.value();
        }
    }

private:
    Vector<ScrollFrameData> m_frames;
};

class ScrollState {
public:
    NonnullRefPtr<ScrollFrame> create_scroll_frame_for(PaintableBox const& paintable_box, RefPtr<ScrollFrame const> parent)
    {
        auto scroll_frame = adopt_ref(*new ScrollFrame(paintable_box, m_scroll_frames.size(), false, move(parent)));
        m_scroll_frames.append(scroll_frame);
        return scroll_frame;
    }

    NonnullRefPtr<ScrollFrame> create_sticky_frame_for(PaintableBox const& paintable_box, RefPtr<ScrollFrame const> parent)
    {
        auto scroll_frame = adopt_ref(*new ScrollFrame(paintable_box, m_scroll_frames.size(), true, move(parent)));
        m_scroll_frames.append(scroll_frame);
        return scroll_frame;
    }

    CSSPixelPoint own_offset_for_frame_with_id(size_t id) const
    {
        return m_scroll_frames[id]->own_offset();
    }

    RefPtr<ScrollFrame const> scroll_frame_by_id(size_t id) const
    {
        if (id >= m_scroll_frames.size())
            return nullptr;
        return m_scroll_frames[id];
    }

    template<typename Callback>
    void for_each_scroll_frame(Callback callback) const
    {
        for (auto const& scroll_frame : m_scroll_frames) {
            if (scroll_frame->is_sticky())
                continue;
            callback(scroll_frame);
        }
    }

    template<typename Callback>
    void for_each_sticky_frame(Callback callback) const
    {
        for (auto const& scroll_frame : m_scroll_frames) {
            if (!scroll_frame->is_sticky())
                continue;
            callback(scroll_frame);
        }
    }

    void clear()
    {
        m_scroll_frames.clear();
    }

private:
    friend class ViewportPaintable;

    ScrollStateSnapshot snapshot() const
    {
        return ScrollStateSnapshot::create(m_scroll_frames);
    }

    Vector<NonnullRefPtr<ScrollFrame>> m_scroll_frames;
};

}
