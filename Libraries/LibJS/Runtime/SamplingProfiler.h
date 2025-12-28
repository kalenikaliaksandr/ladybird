/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashFunctions.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/String.h>
#include <AK/Traits.h>
#include <AK/Vector.h>
#include <LibJS/Bytecode/Executable.h>
#include <LibJS/Export.h>
#include <LibJS/Forward.h>

namespace JS {

class JS_API SamplingProfiler {
public:
    static NonnullOwnPtr<SamplingProfiler> create(VM&);
    ~SamplingProfiler();

    void start();
    void stop();
    bool is_running() const { return m_running; }

    // Called from timer/signal handler context to request a sample
    void request_sample();

    // Called from safe points to actually collect the sample
    void process_pending_sample();

    // Output in collapsed stack format (compatible with flamegraph.pl)
    String to_collapsed_stack() const;

    // Configuration
    void set_sample_interval_us(u64 microseconds) { m_sample_interval_us = microseconds; }
    u64 sample_interval_us() const { return m_sample_interval_us; }

    u64 total_samples() const { return m_total_samples; }

private:
    explicit SamplingProfiler(VM&);

    void sample_current_stack();

    // Lightweight frame data collected during sampling (no source resolution)
    struct RawFrame {
        Bytecode::Executable const* executable { nullptr };
        size_t program_counter { 0 };
        String function_name;

        bool operator==(RawFrame const&) const = default;
    };

    // A complete stack trace as a sequence of raw frames
    struct RawStackTrace {
        Vector<RawFrame> frames;

        bool operator==(RawStackTrace const&) const = default;
    };

    struct RawStackTraceTraits : public AK::DefaultTraits<RawStackTrace> {
        static unsigned hash(RawStackTrace const& trace)
        {
            unsigned h = 0;
            for (auto const& frame : trace.frames) {
                h = pair_int_hash(h, ptr_hash(frame.executable));
                h = pair_int_hash(h, static_cast<unsigned>(frame.program_counter));
            }
            return h;
        }
    };

    VM& m_vm;
    bool m_running { false };
    u64 m_sample_interval_us { 1000 }; // Default: 1ms (1000Hz)

    // Sample storage: raw stack trace -> count (source resolution deferred)
    HashMap<RawStackTrace, u64, RawStackTraceTraits> m_raw_stacks;
    u64 m_total_samples { 0 };

    // For deferred sampling from signal handler
    bool volatile m_sample_pending { false };
};

}
