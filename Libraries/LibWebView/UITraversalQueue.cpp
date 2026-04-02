/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/EventLoop.h>
#include <LibWebView/UITraversalQueue.h>

namespace WebView {

void UITraversalQueue::append(StepFunction step)
{
    m_queue.enqueue(move(step));
    if (!m_processing) {
        Core::deferred_invoke([this] {
            process_next();
        });
    }
}

void UITraversalQueue::did_complete_current_step()
{
    VERIFY(m_processing);
    m_processing = false;
    if (!m_queue.is_empty()) {
        Core::deferred_invoke([this] {
            process_next();
        });
    }
}

void UITraversalQueue::process_next()
{
    if (m_processing || m_queue.is_empty())
        return;

    m_processing = true;
    auto step = m_queue.dequeue();
    step();
}

}
