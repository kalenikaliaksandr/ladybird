/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>

namespace Web::CSS {

extern bool g_enable_style_invalidation_tracing;

void set_enable_style_invalidation_tracing(bool enabled);

struct StyleInvalidationCycleStats {
    size_t elements_recomputed { 0 };
    size_t elements_inherited_only { 0 };
    size_t nodes_visited_in_invalidator { 0 };
    size_t nodes_marked_subtree { 0 };
    size_t nodes_matched_rule { 0 };
    size_t has_pending_nodes { 0 };
    size_t has_ancestors_visited { 0 };
    size_t has_elements_marked { 0 };
};

extern thread_local StyleInvalidationCycleStats* g_current_cycle_stats;
extern thread_local size_t g_style_invalidation_cycle_counter;

}
