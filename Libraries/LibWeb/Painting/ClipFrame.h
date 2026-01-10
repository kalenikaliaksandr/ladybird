/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/BorderRadiiData.h>
#include <LibWeb/Painting/ScrollFrame.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Painting {

struct ClipRectWithScrollFrame {
    CSSPixelRect rect;
    BorderRadiiData corner_radii;
    RefPtr<ScrollFrame const> enclosing_scroll_frame;
    Optional<size_t> enclosing_scroll_frame_id;
};

struct WEB_API ClipFrame : public AtomicRefCounted<ClipFrame> {
    ClipFrame(CSSPixelRect rect, BorderRadiiData radii, RefPtr<ScrollFrame const> enclosing_scroll_frame);

    ClipRectWithScrollFrame const& clip_rect() const { return m_clip_rect; }

    bool includes_rect_from_clip_property { false };

private:
    ClipRectWithScrollFrame m_clip_rect;
};

}
