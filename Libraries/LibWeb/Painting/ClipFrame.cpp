/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Painting/ClipFrame.h>

namespace Web::Painting {

ClipFrame::ClipFrame(CSSPixelRect rect, BorderRadiiData radii, RefPtr<ScrollFrame const> enclosing_scroll_frame)
{
    Optional<size_t> enclosing_scroll_frame_id;
    if (enclosing_scroll_frame)
        enclosing_scroll_frame_id = enclosing_scroll_frame->id();
    m_clip_rect = ClipRectWithScrollFrame { rect, radii, move(enclosing_scroll_frame), enclosing_scroll_frame_id };
}

CSSPixelRect ClipFrame::clip_rect_for_hit_testing() const
{
    auto rect = m_clip_rect.rect;
    if (m_clip_rect.enclosing_scroll_frame) {
        rect.translate_by(m_clip_rect.enclosing_scroll_frame->cumulative_offset());
    }
    return rect;
}

}
