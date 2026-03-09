/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/StyleInvalidationTracing.h>

namespace Web::CSS {

bool g_enable_style_invalidation_tracing = true;

thread_local StyleInvalidationCycleStats* g_current_cycle_stats = nullptr;
thread_local size_t g_style_invalidation_cycle_counter = 0;

void set_enable_style_invalidation_tracing(bool enabled)
{
    g_enable_style_invalidation_tracing = enabled;
}

}
