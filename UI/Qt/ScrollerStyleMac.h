/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

namespace Ladybird {

// Hands the application the scroller style AppKit prefers, now and whenever it changes. With "Automatically based on
// mouse or trackpad", it changes when a mouse is plugged in or removed.
void install_preferred_scroller_style_observer();

}
