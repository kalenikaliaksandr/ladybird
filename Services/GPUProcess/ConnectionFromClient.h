/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <GPUProcess/Forward.h>
#include <GPUProcess/GPUProcessClientEndpoint.h>
#include <GPUProcess/GPUProcessServerEndpoint.h>
#include <LibIPC/ConnectionFromClient.h>

namespace GPUProcess {

class ConnectionFromClient final
    : public IPC::ConnectionFromClient<GPUProcessClientEndpoint, GPUProcessServerEndpoint> {
    C_OBJECT(ConnectionFromClient);

public:
    ~ConnectionFromClient() override = default;

    virtual void die() override;

private:
    explicit ConnectionFromClient(NonnullOwnPtr<IPC::Transport>);

    virtual Messages::GPUProcessServer::InitTransportResponse init_transport(int peer_pid) override;
    virtual Messages::GPUProcessServer::ConnectNewClientsResponse connect_new_clients(size_t count) override;

    ErrorOr<IPC::TransportHandle> connect_new_client();
};

}
