/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Painting/GPUProcessConnection.h>

namespace Web::Painting {

RefPtr<GPUProcessClient::Client> GPUProcessConnection::s_client;

void GPUProcessConnection::install(NonnullRefPtr<GPUProcessClient::Client> client)
{
    s_client = move(client);
}

bool GPUProcessConnection::is_initialized()
{
    return s_client != nullptr;
}

GPUProcessClient::Client& GPUProcessConnection::the()
{
    VERIFY(s_client);
    return *s_client;
}

}
