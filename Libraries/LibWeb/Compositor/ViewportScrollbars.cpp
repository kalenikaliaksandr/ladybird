/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaUtils.h>
#include <LibWeb/Compositor/ViewportScrollbars.h>

#include <core/SkCanvas.h>
#include <core/SkPaint.h>
#include <core/SkRRect.h>

namespace Web::Compositor {

static SkRect to_skia_rect(Gfx::IntRect const& rect)
{
    return SkRect::MakeXYWH(rect.x(), rect.y(), rect.width(), rect.height());
}

Gfx::Orientation orientation_for_scrollbar(ViewportScrollbar const& scrollbar)
{
    return scrollbar.vertical ? Gfx::Orientation::Vertical : Gfx::Orientation::Horizontal;
}

ViewportScrollbarIdentity viewport_scrollbar_identity(ViewportScrollbar const& scrollbar)
{
    return { scrollbar.scroll_node_id, scrollbar.vertical };
}

Optional<ViewportScrollbarIdentity> viewport_scrollbar_identity_at(ReadonlySpan<ViewportScrollbar> scrollbars, Optional<size_t> scrollbar_index)
{
    if (!scrollbar_index.has_value() || *scrollbar_index >= scrollbars.size())
        return {};
    return viewport_scrollbar_identity(scrollbars[*scrollbar_index]);
}

Optional<size_t> find_viewport_scrollbar_index(ReadonlySpan<ViewportScrollbar> scrollbars, ViewportScrollbarIdentity identity)
{
    for (size_t i = 0; i < scrollbars.size(); ++i) {
        if (scrollbars[i].scroll_node_id == identity.scroll_node_id && scrollbars[i].vertical == identity.vertical)
            return i;
    }
    return {};
}

void set_or_append_pending_scroll_offset(Vector<AsyncScrollOffset>& pending_scroll_offsets, AsyncScrollOffset const& scroll_offset)
{
    for (auto& existing : pending_scroll_offsets) {
        if (existing.stable_node_id == scroll_offset.stable_node_id) {
            existing.compositor_scroll_offset = scroll_offset.compositor_scroll_offset;
            existing.unadopted_scroll_delta.translate_by(scroll_offset.unadopted_scroll_delta);
            return;
        }
    }
    pending_scroll_offsets.append(scroll_offset);
}

Optional<Gfx::FloatPoint> viewport_scroll_offset_from(ReadonlySpan<AsyncScrollOffset> scroll_offsets)
{
    for (auto const& scroll_offset : scroll_offsets) {
        if (scroll_offset.stable_node_id.kind == AsyncScrollNodeKind::Viewport)
            return scroll_offset.compositor_scroll_offset;
    }
    return {};
}

Gfx::IntRect scrollbar_gutter_rect(ViewportScrollbar const& scrollbar, bool expanded)
{
    return expanded ? scrollbar.expanded_gutter_rect : scrollbar.gutter_rect;
}

double scrollbar_scroll_size(ViewportScrollbar const& scrollbar, bool expanded)
{
    return expanded ? scrollbar.expanded_scroll_size : scrollbar.scroll_size;
}

Gfx::IntRect translated_thumb_rect(ViewportScrollbar const& scrollbar, Painting::ScrollStateSnapshot const& scroll_state_snapshot, bool expanded)
{
    auto thumb_rect = expanded ? scrollbar.expanded_thumb_rect : scrollbar.thumb_rect;
    auto scroll_size = scrollbar_scroll_size(scrollbar, expanded);
    auto device_offset = scroll_state_snapshot.device_offset_for_index(scrollbar.scroll_frame_index);
    if (scrollbar.vertical)
        thumb_rect.translate_by(0, static_cast<int>(-device_offset.y() * scroll_size));
    else
        thumb_rect.translate_by(static_cast<int>(-device_offset.x() * scroll_size), 0);
    return thumb_rect;
}

Gfx::IntRect translated_thumb_rect(ViewportScrollbar const& scrollbar, Gfx::FloatPoint scroll_offset, bool expanded)
{
    auto orientation = orientation_for_scrollbar(scrollbar);
    auto thumb_rect = expanded ? scrollbar.expanded_thumb_rect : scrollbar.thumb_rect;
    thumb_rect.translate_primary_offset_for_orientation(orientation, static_cast<int>(scroll_offset.primary_offset_for_orientation(orientation) * scrollbar_scroll_size(scrollbar, expanded)));
    return thumb_rect;
}

Gfx::IntRect scrollbar_hit_rect(ViewportScrollbar const& scrollbar, Gfx::FloatPoint scroll_offset)
{
    static constexpr int scrollbar_hit_slop = 4;

    auto rect = translated_thumb_rect(scrollbar, scroll_offset, false).united(translated_thumb_rect(scrollbar, scroll_offset, true));
    auto expanded_gutter_rect = scrollbar_gutter_rect(scrollbar, true);
    if (!expanded_gutter_rect.is_empty())
        rect.unite(expanded_gutter_rect);
    rect.inflate(scrollbar_hit_slop, scrollbar_hit_slop);
    return rect;
}

static void paint_viewport_scrollbar(Gfx::PaintingSurface& surface, ViewportScrollbar const& scrollbar, Painting::ScrollStateSnapshot const& scroll_state_snapshot, bool expanded)
{
    auto thumb_rect = translated_thumb_rect(scrollbar, scroll_state_snapshot, expanded);
    auto& canvas = surface.canvas();

    SkPaint gutter_fill_paint;
    gutter_fill_paint.setColor(to_skia_color(scrollbar.track_color));
    canvas.drawRect(to_skia_rect(scrollbar_gutter_rect(scrollbar, expanded)), gutter_fill_paint);

    auto skia_thumb_rect = to_skia_rect(thumb_rect);
    auto radius = skia_thumb_rect.width() / 2;
    auto thumb_rrect = SkRRect::MakeRectXY(skia_thumb_rect, radius, radius);

    SkPaint thumb_fill_paint;
    thumb_fill_paint.setColor(to_skia_color(scrollbar.thumb_color));
    canvas.drawRRect(thumb_rrect, thumb_fill_paint);

    SkPaint stroke_paint;
    stroke_paint.setStroke(true);
    stroke_paint.setStrokeWidth(1);
    stroke_paint.setAntiAlias(true);
    stroke_paint.setColor(to_skia_color(scrollbar.thumb_color.lightened()));
    canvas.drawRRect(thumb_rrect, stroke_paint);
}

void paint_viewport_scrollbars(Gfx::PaintingSurface& surface, ReadonlySpan<ViewportScrollbar> scrollbars, Painting::ScrollStateSnapshot const& scroll_state_snapshot, Optional<size_t> hovered_scrollbar_index, Optional<size_t> captured_scrollbar_index)
{
    for (size_t i = 0; i < scrollbars.size(); ++i)
        paint_viewport_scrollbar(surface, scrollbars[i], scroll_state_snapshot, hovered_scrollbar_index == i || captured_scrollbar_index == i);
}

}
