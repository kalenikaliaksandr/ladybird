/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/EventLoop.h>
#include <LibWebView/SessionHistoryManager.h>
#include <LibWebView/UITraversalQueue.h>

namespace WebView {

void UITraversalQueue::append(StepFunction step)
{
    m_queue.enqueue({ .step = move(step), .navigable_id = 0 });
    schedule_processing();
}

void UITraversalQueue::append_sync(StepFunction step, u64 navigable_id)
{
    m_queue.enqueue({ .step = move(step), .navigable_id = navigable_id });
    schedule_processing();
}

void UITraversalQueue::did_complete_current_step()
{
    VERIFY(m_processing);
    m_processing = false;
    schedule_processing();
}

UITraversalQueue::SyncNavEntry* UITraversalQueue::take_first_eligible_sync_nav_step(
    SessionHistoryManager const& manager,
    Function<bool(u64 navigable_id)> const& should_skip)
{
    (void)manager;
    (void)should_skip;
    // FIXME: Implement queue-jumping in UI.
    return nullptr;
}

void UITraversalQueue::schedule_processing()
{
    if (!m_processing_scheduled && !m_processing) {
        m_processing_scheduled = true;
        Core::deferred_invoke([this] {
            m_processing_scheduled = false;
            process_next();
        });
    }
}

void UITraversalQueue::process_next()
{
    if (m_processing || m_queue.is_empty())
        return;

    m_processing = true;
    auto entry = m_queue.dequeue();
    entry.step();
}

}
