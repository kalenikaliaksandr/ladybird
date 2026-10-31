/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>

namespace Web::Painting {

enum class PaintCommandCacheMode : u8 {
    ReadOnly,
    ReadWrite,
};

// Which part of its document a recording records.
enum class RecordingCoverage : u8 {
    WholeDocument,
    // What lies around the part of each scroll node that its scroll position shows.
    AroundScrollPositions,
    // Like AroundScrollPositions, but keeping only what an earlier testing recording aimed for, so that what a test
    // records never depends on recordings made outside it.
    AroundScrollPositionsForTesting,
    // What the recording published last records: a recording that only rebuilds the hit-test list records no more.
    AsSourceRecording,
};

enum class SelectionState : u8 {
    None,
    Start,
    End,
    StartAndEnd,
    Full,
};

}
