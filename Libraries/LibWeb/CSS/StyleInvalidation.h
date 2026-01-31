/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Forward.h>

namespace Web::CSS {

struct RequiredInvalidationAfterStyleChange {
    bool rebuild_display_list : 1 { false };
    bool rebuild_stacking_context_tree : 1 { false };
    bool relayout : 1 { false };
    bool rebuild_layout_tree : 1 { false };
    bool rebuild_accumulated_visual_contexts : 1 { false };
    bool refresh_accumulated_visual_contexts : 1 { false };

    void operator|=(RequiredInvalidationAfterStyleChange const& other)
    {
        rebuild_display_list |= other.rebuild_display_list;
        rebuild_stacking_context_tree |= other.rebuild_stacking_context_tree;
        relayout |= other.relayout;
        rebuild_layout_tree |= other.rebuild_layout_tree;
        rebuild_accumulated_visual_contexts |= other.rebuild_accumulated_visual_contexts;
        refresh_accumulated_visual_contexts |= other.refresh_accumulated_visual_contexts;
    }

    [[nodiscard]] bool is_none() const { return !rebuild_display_list && !rebuild_stacking_context_tree && !relayout && !rebuild_layout_tree && !rebuild_accumulated_visual_contexts && !refresh_accumulated_visual_contexts; }
    [[nodiscard]] bool is_full() const { return rebuild_display_list && rebuild_stacking_context_tree && relayout && rebuild_layout_tree; }
    static RequiredInvalidationAfterStyleChange full() { return { true, true, true, true, true, false }; }
};

RequiredInvalidationAfterStyleChange compute_property_invalidation(CSS::PropertyID property_id, RefPtr<StyleValue const> const& old_value, RefPtr<StyleValue const> const& new_value);

}
