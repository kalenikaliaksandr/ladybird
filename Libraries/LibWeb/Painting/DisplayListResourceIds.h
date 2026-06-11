/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Atomic.h>
#include <AK/DistinctNumeric.h>
#include <AK/Types.h>

namespace Web::Painting {

AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, FontResourceId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, ImageFrameResourceId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, VideoFrameResourceId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, DisplayListResourceId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, CompositorSurfaceId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, CanvasId);
AK_TYPEDEF_DISTINCT_ORDERED_ID(u64, CanvasContextId);

// Each id type draws from its own process-wide counter; ids are never zero.
template<typename IdType>
inline IdType allocate_display_list_resource_id()
{
    static Atomic<u64> s_next_id { 1 };
    return IdType { s_next_id.fetch_add(1, AK::MemoryOrder::memory_order_relaxed) };
}

}
