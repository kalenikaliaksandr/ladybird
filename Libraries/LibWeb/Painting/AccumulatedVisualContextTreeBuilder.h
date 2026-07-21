/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Painting/AccumulatedVisualContext.h>

namespace Web::Painting {

class Paintable;
class ViewportPaintable;

struct VisualContextReconcileOutcome {
    bool structural_change { false };
    size_t visited_paintable_count { 0 };
    size_t allocated_node_count { 0 };
    size_t freed_node_count { 0 };

    void merge(VisualContextReconcileOutcome const& other)
    {
        structural_change |= other.structural_change;
        visited_paintable_count += other.visited_paintable_count;
        allocated_node_count += other.allocated_node_count;
        freed_node_count += other.freed_node_count;
    }
};

AccumulatedVisualContextTree build_accumulated_visual_context_tree(ViewportPaintable&);
VisualContextReconcileOutcome reconcile_accumulated_visual_context_tree(ViewportPaintable&, AccumulatedVisualContextTree&);
VisualContextReconcileOutcome reconcile_accumulated_visual_context_subtree(ViewportPaintable&, AccumulatedVisualContextTree&, Paintable& subtree_root);
bool verify_visual_context_tree_reconcile_enabled();
void verify_reconciled_visual_context_tree_matches_fresh_build(ViewportPaintable&, AccumulatedVisualContextTree const& reconciled_tree);
bool update_accumulated_visual_context_values(ViewportPaintable&, Paintable&);
void update_visual_viewport_accumulated_visual_context(ViewportPaintable&);

Optional<TransformData> compute_transform(Paintable const&, CSS::ComputedValues const&, double pixel_ratio);

}
