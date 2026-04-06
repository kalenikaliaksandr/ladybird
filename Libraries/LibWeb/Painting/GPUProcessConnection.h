/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibGPUProcessClient/Client.h>
#include <LibWeb/Export.h>

namespace Web::Painting {

class GPUProcessConnection {
public:
    WEB_API static void install(NonnullRefPtr<GPUProcessClient::Client>);
    WEB_API static bool is_initialized();
    WEB_API static GPUProcessClient::Client& the();

private:
    static RefPtr<GPUProcessClient::Client> s_client;
};

}
