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
};

AccumulatedVisualContextTree build_accumulated_visual_context_tree(ViewportPaintable&);
VisualContextReconcileOutcome reconcile_accumulated_visual_context_tree(ViewportPaintable&, AccumulatedVisualContextTree&);
bool update_accumulated_visual_context_values(ViewportPaintable&, Paintable&);
void update_visual_viewport_accumulated_visual_context(ViewportPaintable&);

Optional<TransformData> compute_transform(Paintable const&, CSS::ComputedValues const&, double pixel_ratio);

}
