/*
 * Copyright (c) 2024, Andrew Kaster <akaster@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Platform.h>

#if !defined(AK_OS_MACH)
#    error "This file is only available on Mach platforms"
#endif

#include <LibCore/Export.h>
#include <LibCore/MachPort.h>
#include <LibCore/Platform/ProcessStatistics.h>
#include <mach/mach.h>

namespace Core::Platform {

struct MachServerRegistration {
    MachPort server_port;
    MachPort ipc_receive_right;
    MachPort ipc_send_right;
};

CORE_API MachServerRegistration register_with_mach_server(ByteString const& server_name);

}
