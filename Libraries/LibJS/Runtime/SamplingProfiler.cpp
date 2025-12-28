/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringBuilder.h>
#include <LibJS/Bytecode/Executable.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/SamplingProfiler.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/SourceRange.h>

#ifndef AK_OS_WINDOWS
#    include <signal.h>
#    include <sys/time.h>
#endif

namespace JS {

static SamplingProfiler* s_current_profiler = nullptr;

NonnullOwnPtr<SamplingProfiler> SamplingProfiler::create(VM& vm)
{
    return adopt_own(*new SamplingProfiler(vm));
}

SamplingProfiler::SamplingProfiler(VM& vm)
    : m_vm(vm)
{
}

SamplingProfiler::~SamplingProfiler()
{
    if (m_running)
        stop();
}

#ifndef AK_OS_WINDOWS
static void sigprof_handler(int)
{
    if (s_current_profiler)
        s_current_profiler->request_sample();
}
#endif

void SamplingProfiler::start()
{
    if (m_running)
        return;

    m_running = true;
    s_current_profiler = this;
    m_raw_stacks.clear();
    m_total_samples = 0;

#ifndef AK_OS_WINDOWS
    // Set up SIGPROF handler
    struct sigaction sa;
    sa.sa_handler = sigprof_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGPROF, &sa, nullptr);

    // Set up interval timer
    struct itimerval timer;
    timer.it_value.tv_sec = 0;
    timer.it_value.tv_usec = static_cast<suseconds_t>(m_sample_interval_us);
    timer.it_interval = timer.it_value;
    setitimer(ITIMER_PROF, &timer, nullptr);
#endif
}

void SamplingProfiler::stop()
{
    if (!m_running)
        return;

#ifndef AK_OS_WINDOWS
    // Disable the timer
    struct itimerval timer = {};
    setitimer(ITIMER_PROF, &timer, nullptr);

    // Restore default signal handler
    signal(SIGPROF, SIG_DFL);
#endif

    // Process any pending sample
    if (m_sample_pending)
        sample_current_stack();

    m_running = false;
    s_current_profiler = nullptr;
}

void SamplingProfiler::request_sample()
{
    // Called from signal handler context - only set the flag
    // The actual sampling happens in process_pending_sample()
    m_sample_pending = true;
}

void SamplingProfiler::process_pending_sample()
{
    if (!m_sample_pending)
        return;

    m_sample_pending = false;
    sample_current_stack();
}

void SamplingProfiler::sample_current_stack()
{
    auto const& stack = m_vm.execution_context_stack();
    if (stack.is_empty())
        return;

    RawStackTrace trace;

    // Walk from bottom to top (outer -> inner for flamegraph format)
    for (auto* context : stack) {
        // Skip frames without JS function and without executable (native C++ frames)
        if (!context->function && !context->executable)
            continue;

        RawFrame frame;
        frame.executable = context->executable.ptr();
        frame.program_counter = context->program_counter;

        if (context->function)
            frame.function_name = context->function->name_for_call_stack().to_utf8();

        trace.frames.append(move(frame));
    }

    // Don't record empty stacks (all native frames)
    if (trace.frames.is_empty())
        return;

    auto result = m_raw_stacks.get(trace);
    m_raw_stacks.set(move(trace), result.value_or(0) + 1);
    ++m_total_samples;
}

String SamplingProfiler::to_collapsed_stack() const
{
    StringBuilder output;

    // Resolve source locations for each unique stack trace
    for (auto const& [trace, count] : m_raw_stacks) {
        StringBuilder stack_signature;
        bool first = true;

        for (auto const& frame : trace.frames) {
            if (!first)
                stack_signature.append(';');
            first = false;

            if (!frame.function_name.is_empty()) {
                if (frame.executable) {
                    auto source_range = frame.executable->source_range_at(frame.program_counter);
                    if (source_range.source_code) {
                        auto realized = source_range.realize();
                        stack_signature.appendff("({} @ {}:{}:{})", frame.function_name, realized.filename(), realized.start.line, realized.start.column);
                    } else {
                        stack_signature.appendff("({})", frame.function_name);
                    }
                } else {
                    stack_signature.appendff("({})", frame.function_name);
                }
            } else if (frame.executable) {
                // Top-level script execution or anonymous function without name
                stack_signature.append("(script)"sv);
            } else {
                stack_signature.append("(anonymous)"sv);
            }
        }

        output.appendff("{} {}\n", stack_signature.string_view(), count);
    }

    return output.to_string_without_validation();
}

}
