/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/System.h>
#include <LibCore/SystemHandle.h>

namespace Core {

void SystemHandle::close()
{
    if (!is_valid())
        return;
    (void)System::close(leak());
}

}
