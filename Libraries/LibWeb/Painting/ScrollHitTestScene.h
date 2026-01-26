/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/Vector.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

enum class HitTestOpaqueness : u8 {
    Transparent, // Skip during hit-test (opacity: 0, pointer-events: none)
    Opaque,      // Normal element, blocks hit-test to elements underneath
};

struct ScrollHitTestItem {
    CSSPixelRect hit_rect;                                 // Border box rect in local coords
    RefPtr<AccumulatedVisualContext const> visual_context; // For coordinate transforms + scroll containment
    size_t stacking_order;                                 // Paint order (higher = painted later = on top)
    Optional<size_t> scroll_frame_id;                      // Set only if THIS element is scrollable
    HitTestOpaqueness opaqueness { HitTestOpaqueness::Opaque };
};

class ScrollHitTestScene : public AtomicRefCounted<ScrollHitTestScene> {
public:
    static NonnullRefPtr<ScrollHitTestScene> create();

    void add_item(ScrollHitTestItem);
    void finalize(); // Sort by stacking order for hit-testing

    // Returns scroll_frame_id for the scroller that should receive the scroll event.
    // Returns empty if:
    //  - Point hits an opaque element not inside any scroller (blocked)
    //  - Point hits nothing (viewport scroll)
    Optional<size_t> hit_test_scroll(CSSPixelPoint screen_point, ScrollStateSnapshot const&) const;

    // For debugging/testing
    size_t item_count() const { return m_items.size(); }

private:
    ScrollHitTestScene() = default;

    // Find the nearest ScrollData in the visual context chain
    static Optional<size_t> find_enclosing_scroll_frame(AccumulatedVisualContext const* context);

    Vector<ScrollHitTestItem> m_items;
    bool m_finalized { false };
};

}
