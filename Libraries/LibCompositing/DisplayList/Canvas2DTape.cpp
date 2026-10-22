/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCompositing/DisplayList/Canvas2DTape.h>
#include <LibCompositing/RustFFI.h>

namespace Compositing {

void for_each_canvas_2d_command(ReadonlyBytes tape, Function<void(DisplayListCommandType, ReadonlyBytes payload)> const& callback)
{
    RustFFI::canvas_2d_tape_for_each_command(tape, const_cast<void*>(static_cast<void const*>(&callback)), [](void* context, DisplayListCommandType command_type, ReadonlyBytes payload) {
        (*static_cast<Function<void(DisplayListCommandType, ReadonlyBytes)> const*>(context))(command_type, payload);
    });
}

}
