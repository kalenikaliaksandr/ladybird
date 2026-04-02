/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Queue.h>
#include <AK/Variant.h>

namespace WebView {

// UI-side traversal queue — determines WHEN traversal steps execute.
// Step bodies are executed in WebContent via IPC; this queue only manages ordering and completion.
class UITraversalQueue {
public:
    UITraversalQueue() = default;

    // A traversal step that the queue will execute by calling the provided function.
    // The function should initiate the step (e.g., send IPC to WebContent).
    // Call did_complete_current_step() when the step finishes.
    using StepFunction = Function<void()>;

    void append(StepFunction step);
    void did_complete_current_step();

    bool is_processing() const { return m_processing; }

private:
    void process_next();

    Queue<StepFunction> m_queue;
    bool m_processing { false };
};

}
