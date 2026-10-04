/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>
#include <LibTest/TestCase.h>

// The pointer that the payload of a normal completion carries, such as the cell that an operation creates.
template<typename T>
T* pointer_of_payload(JSCompletion completion)
{
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return reinterpret_cast<T*>(static_cast<uintptr_t>(completion.payload));
}
