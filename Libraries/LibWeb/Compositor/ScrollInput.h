/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>

namespace Web {

// Discrete wheel deltas come from stepwise input such as mouse wheel notches; precise wheel deltas come from input
// that reports exact pixel distances, such as touchpad panning gestures.
enum class WheelDeltaPrecision : u8 {
    Discrete,
    Precise,
};

// Input that scrolls with a gesture, such as a touchpad, reports whether the user is still making that gesture,
// whether a flick has handed the scrolling over to momentum, and when it ends.
enum class ScrollGesturePhase : u8 {
    None,
    Ongoing,
    Momentum,
    Ended,
};

namespace Compositor {

struct AsyncScrollInput {
    WheelDeltaPrecision precision { WheelDeltaPrecision::Precise };
    ScrollGesturePhase phase { ScrollGesturePhase::None };
};

}

}
