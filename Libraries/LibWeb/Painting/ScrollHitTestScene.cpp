/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/QuickSort.h>
#include <LibWeb/Painting/ScrollHitTestScene.h>

namespace Web::Painting {

NonnullRefPtr<ScrollHitTestScene> ScrollHitTestScene::create()
{
    return adopt_ref(*new ScrollHitTestScene);
}

void ScrollHitTestScene::add_item(ScrollHitTestItem item)
{
    VERIFY(!m_finalized);
    m_items.append(move(item));
}

void ScrollHitTestScene::finalize()
{
    // Sort by stacking order (ascending) so we can iterate in reverse for hit-testing
    AK::quick_sort(m_items, [](auto const& a, auto const& b) {
        return a.stacking_order < b.stacking_order;
    });
    m_finalized = true;
}

Optional<size_t> ScrollHitTestScene::find_enclosing_scroll_frame(AccumulatedVisualContext const* context)
{
    // Walk the visual context chain to find the nearest ScrollData
    for (auto const* current = context; current; current = current->parent().ptr()) {
        if (auto const* scroll_data = current->data().get_pointer<ScrollData>()) {
            if (!scroll_data->is_sticky)
                return scroll_data->scroll_frame_id;
        }
    }
    return {};
}

Optional<size_t> ScrollHitTestScene::hit_test_scroll(
    CSSPixelPoint screen_point,
    ScrollStateSnapshot const& scroll_state) const
{
    VERIFY(m_finalized);

    // Traverse in reverse stacking order (topmost/last-painted first)
    for (auto it = m_items.rbegin(); it != m_items.rend(); ++it) {
        auto const& item = *it;

        // Skip transparent elements
        if (item.opaqueness == HitTestOpaqueness::Transparent)
            continue;

        // Transform screen point to local coordinates using visual context
        Optional<CSSPixelPoint> local_point;
        if (item.visual_context) {
            local_point = item.visual_context->transform_point_for_hit_test(
                screen_point, scroll_state);
        } else {
            local_point = screen_point;
        }

        // Point was clipped out by a clip rect/path in the visual context chain
        if (!local_point.has_value())
            continue;

        if (!item.hit_rect.contains(local_point.value()))
            continue;

        // Found the topmost element at this point!

        // If this element is itself scrollable, return its scroll frame
        if (item.scroll_frame_id.has_value())
            return item.scroll_frame_id;

        // Otherwise, check if it's inside a scrollable container
        // by walking the visual context chain
        if (item.visual_context) {
            auto enclosing = find_enclosing_scroll_frame(item.visual_context.ptr());
            if (enclosing.has_value())
                return enclosing;
        }

        // Element is not inside any scroller - it blocks scrolling
        return {};
    }

    return {}; // Nothing hit - could scroll viewport
}

}
