/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/Span.h>
#include <AK/Time.h>
#include <AK/Vector.h>
#include <Compositor/CompositorCoreRustFFI.h>
#include <Compositor/FrameInputs.h>
#include <LibCompositing/DisplayList/DisplayList.h>
#include <LibGfx/Rect.h>

namespace Compositor {

using FrameStep = RustFFI::FfiFrameStep;
using FrameStepKind = RustFFI::FfiFrameStepKind;
using SlotBlend = RustFFI::FfiSlotBlend;
using SublayerDescription = RustFFI::FfiSublayer;
using SublayerContentKind = RustFFI::FfiSublayerContentKind;

struct LiveMediaSample {
    Vector<RustFFI::FfiCanvasSample> canvases;
    Vector<RustFFI::FfiVideoSample> videos;
    Vector<RustFFI::FfiImageFrameSample> image_frames;
};

struct FrameRequest {
    Gfx::IntSize viewport_size;
    Gfx::IntRect forced_damage_rect;
    MonotonicTime now;
    bool presents_to_client { false };
    bool window_server_may_show_media { false };
    bool window_server_may_show_high_dynamic_range { false };
};

struct LastFrame {
    FrameInputs const* frame_inputs { nullptr };
    Compositing::DisplayList const* display_list { nullptr };
    Gfx::IntSize viewport_size;
    bool is_same_display_list { false };
    bool has_other_surface_clear_color { false };
};

struct FramePlan {
    Gfx::IntRect damage_rect;
    bool paints_from_raster_slots { false };
    size_t raster_slot_count { 0 };
    bool only_window_server_content_changed { false };
    ReadonlySpan<FrameStep> steps;
};

struct CommittedFrame {
    bool keeps_raster_slots { false };
    AK::Duration release_idle_raster_slots_after;
};

inline ReadonlySpan<Compositing::RustFFI::RecordSpan> record_spans_of(FrameStep const& step)
{
    return { step.record_spans, step.record_span_count };
}

class FramePlanner {
    AK_MAKE_NONCOPYABLE(FramePlanner);
    AK_MAKE_NONMOVABLE(FramePlanner);

public:
    FramePlanner();
    ~FramePlanner();

    FramePlan plan(FrameRequest const&, FrameInputs const&, Compositing::DisplayList const&, LastFrame const*, LiveMediaSample const&);
    bool did_fail_to_allocate_raster_slot();
    CommittedFrame commit();
    bool release_idle_raster_slots();
    ReadonlySpan<SublayerDescription> sublayers() const;
    ReadonlySpan<FrameStep> steps_compositing_presented_frame_into_store();
    bool last_frame_drew_canvas(Compositing::CanvasId) const;

private:
    RustFFI::ContextFramePlanner* m_planner { nullptr };
};

}
