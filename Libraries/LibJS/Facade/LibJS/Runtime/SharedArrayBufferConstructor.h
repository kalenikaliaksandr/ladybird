/*
 * Copyright (c) 2023, Shannon Booth <shannon@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibJS/Runtime/FunctionObject.h>

namespace JS {

// FIXME: Derive from NativeFunction, as in the C++ runtime, once the facade has it.
class SharedArrayBufferConstructor final : public FunctionObject {
};

}
