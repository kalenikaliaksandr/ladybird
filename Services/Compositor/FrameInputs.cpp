/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StdLibExtras.h>
#include <Compositor/FrameInputs.h>

namespace Compositor {

FrameInputs::FrameInputs(RustFFI::FrameInputs* frame_inputs, Compositing::AccumulatedVisualContextTree visual_context_tree)
    : m_frame_inputs(frame_inputs)
    , m_visual_context_tree(move(visual_context_tree))
{
}

FrameInputs FrameInputs::adopt(RustFFI::FrameInputs* frame_inputs)
{
    VERIFY(frame_inputs);
    return { frame_inputs, Compositing::AccumulatedVisualContextTree::adopt_rust_handle(RustFFI::compositor_frame_inputs_retain_visual_context_tree(frame_inputs)) };
}

FrameInputs::FrameInputs(FrameInputs&& other)
    : m_frame_inputs(exchange(other.m_frame_inputs, nullptr))
    , m_visual_context_tree(move(other.m_visual_context_tree))
{
}

FrameInputs& FrameInputs::operator=(FrameInputs&& other)
{
    if (this == &other)
        return *this;
    if (m_frame_inputs)
        RustFFI::compositor_frame_inputs_destroy(m_frame_inputs);
    m_frame_inputs = exchange(other.m_frame_inputs, nullptr);
    m_visual_context_tree = move(other.m_visual_context_tree);
    return *this;
}

FrameInputs::~FrameInputs()
{
    if (m_frame_inputs)
        RustFFI::compositor_frame_inputs_destroy(m_frame_inputs);
}

ReadonlySpan<ScrollbarOfFrame> FrameInputs::scrollbars() const
{
    size_t count = 0;
    auto const* scrollbars = RustFFI::compositor_frame_inputs_scrollbars(m_frame_inputs, &count);
    return { scrollbars, count };
}

void FrameInputs::replay(Compositing::DisplayList const& display_list, Compositing::RustFFI::FfiDisplayListReplayCallbacks const& callbacks, ReplaySelection const& selection) const
{
    VERIFY(display_list.compatible_visual_context_tree_structural_epoch() == m_visual_context_tree.structural_epoch());
    RustFFI::compositor_frame_inputs_replay(m_frame_inputs, display_list.rust_handle(), display_list.root_group_color(), &selection, &callbacks);
}

void FrameInputs::replay_records(ReadonlyBytes records, Compositing::RustFFI::FfiDisplayListReplayCallbacks const& callbacks) const
{
    RustFFI::compositor_frame_inputs_replay_records(m_frame_inputs, records.data(), records.size(), &callbacks);
}

}
