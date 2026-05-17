/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Vector.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Orientation.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Gfx {

class PaintingSurface;

}

namespace Web::Compositor {

struct WEB_API ViewportScrollbarIdentity {
    AsyncScrollNodeID scroll_node_id;
    bool vertical { false };
};

WEB_API Gfx::Orientation orientation_for_scrollbar(ViewportScrollbar const&);
WEB_API ViewportScrollbarIdentity viewport_scrollbar_identity(ViewportScrollbar const&);
WEB_API Optional<ViewportScrollbarIdentity> viewport_scrollbar_identity_at(ReadonlySpan<ViewportScrollbar>, Optional<size_t> scrollbar_index);
WEB_API Optional<size_t> find_viewport_scrollbar_index(ReadonlySpan<ViewportScrollbar>, ViewportScrollbarIdentity);
WEB_API void set_or_append_pending_scroll_offset(Vector<AsyncScrollOffset>& pending_scroll_offsets, AsyncScrollOffset const&);
WEB_API Optional<Gfx::FloatPoint> viewport_scroll_offset_from(ReadonlySpan<AsyncScrollOffset>);

WEB_API Gfx::IntRect scrollbar_gutter_rect(ViewportScrollbar const&, bool expanded);
WEB_API double scrollbar_scroll_size(ViewportScrollbar const&, bool expanded);
WEB_API Gfx::IntRect translated_thumb_rect(ViewportScrollbar const&, Painting::ScrollStateSnapshot const&, bool expanded);
WEB_API Gfx::IntRect translated_thumb_rect(ViewportScrollbar const&, Gfx::FloatPoint scroll_offset, bool expanded);
WEB_API Gfx::IntRect scrollbar_hit_rect(ViewportScrollbar const&, Gfx::FloatPoint scroll_offset);
WEB_API void paint_viewport_scrollbars(Gfx::PaintingSurface&, ReadonlySpan<ViewportScrollbar>, Painting::ScrollStateSnapshot const&, Optional<size_t> hovered_scrollbar_index, Optional<size_t> captured_scrollbar_index);

}
