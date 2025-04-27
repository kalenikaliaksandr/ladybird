/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>

namespace JS {

struct StackFrameLayout {
    u32 constants_count { 0 };
    u32 registers_count { 0 };
    u32 locals_count { 0 };
    u32 arguments_count { 0 };
};

}
