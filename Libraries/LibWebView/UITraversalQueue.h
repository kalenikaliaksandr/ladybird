/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Queue.h>

namespace WebView {

class SessionHistoryManager;

// UI-side traversal queue replacing WebContent's SessionHistoryTraversalQueue.
// Manages ordering of traversal steps and sync nav queue-jumping.
// Step bodies execute in WebContent via IPC — this queue only schedules.
class UITraversalQueue {
public:
    UITraversalQueue() = default;

    using StepFunction = Function<void()>;

    // Enqueue an async traversal step.
    void append(StepFunction step);

    // Enqueue a sync navigation step associated with a navigable.
    // May be queue-jumped during an active traversal.
    void append_sync(StepFunction step, u64 navigable_id);

    // Called when the currently executing step completes.
    void did_complete_current_step();

    bool is_processing() const { return m_processing; }

    // Used by the traversal executor (ApplyHistoryStepState) to jump the queue.
    // Returns and removes the first eligible sync nav step, or nullptr.
    struct SyncNavEntry {
        StepFunction step;
        u64 navigable_id;
    };
    SyncNavEntry* take_first_eligible_sync_nav_step(
        SessionHistoryManager const&,
        Function<bool(u64 navigable_id)> const& should_skip);

private:
    void schedule_processing();
    void process_next();

    struct QueueEntry {
        StepFunction step;
        u64 navigable_id { 0 }; // 0 = async, non-zero = sync nav step
    };

    Queue<QueueEntry> m_queue;
    bool m_processing { false };
    bool m_processing_scheduled { false };
};

}
