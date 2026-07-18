/*
 * Copyright (c) 2022, the SerenityOS developers.
 * Copyright (c) 2024, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2024, Tim Flynn <trflynn89@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ScopeGuard.h>
#include <LibWeb/HTML/AnimationFrameCallbackDriver.h>

namespace Web::HTML {

GC_DEFINE_ALLOCATOR(AnimationFrameCallbackDriver);

void AnimationFrameCallbackDriver::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_callbacks);
    visitor.visit(m_executing_callbacks);
}

WebIDL::UnsignedLong AnimationFrameCallbackDriver::add(Callback handler)
{
    auto id = ++m_animation_frame_callback_identifier;
    m_callbacks.set(id, handler);
    return id;
}

bool AnimationFrameCallbackDriver::remove(WebIDL::UnsignedLong id)
{
    // A callback can cancel another one queued for the same rendering update,
    // whose batch has already been moved into m_executing_callbacks.
    return m_callbacks.remove(id) || m_executing_callbacks.remove(id);
}

bool AnimationFrameCallbackDriver::has_callbacks() const
{
    return !m_callbacks.is_empty();
}

void AnimationFrameCallbackDriver::run(double now)
{
    AK::ScopeGuard guard { [&]() { m_executing_callbacks.clear(); } };
    m_executing_callbacks = move(m_callbacks);

    // Take entries one at a time instead of iterating: an invoked callback may
    // cancel a later callback of this same batch through remove().
    while (!m_executing_callbacks.is_empty()) {
        auto it = m_executing_callbacks.begin();
        auto callback = it->value;
        m_executing_callbacks.remove(it);
        callback->function()(now);
    }
}

}
