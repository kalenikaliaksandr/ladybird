/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Span.h>
#include <LibCompositing/DisplayList/DisplayListCommandsGenerated.h>
#include <LibCompositing/Export.h>

namespace Compositing {

// Calls the callback with the type and the payload of each record of a checked tape, in order.
COMPOSITING_API void for_each_canvas_2d_command(ReadonlyBytes tape, Function<void(DisplayListCommandType, ReadonlyBytes payload)> const&);

}
