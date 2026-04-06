/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/IDAllocator.h>
#include <GPUProcess/ConnectionFromClient.h>
#include <LibCore/EventLoop.h>
#include <LibCore/System.h>
#include <LibIPC/TransportHandle.h>

namespace GPUProcess {

static HashMap<int, RefPtr<ConnectionFromClient>> s_connections;
static IDAllocator s_client_ids;

ConnectionFromClient::ConnectionFromClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionFromClient<GPUProcessClientEndpoint, GPUProcessServerEndpoint>(*this, move(transport), s_client_ids.allocate())
{
    s_connections.set(client_id(), *this);
}

void ConnectionFromClient::die()
{
    auto client_id = this->client_id();
    s_connections.remove(client_id);
    s_client_ids.deallocate(client_id);

    if (s_connections.is_empty())
        Core::EventLoop::current().quit(0);
}

Messages::GPUProcessServer::InitTransportResponse ConnectionFromClient::init_transport([[maybe_unused]] int peer_pid)
{
#ifdef AK_OS_WINDOWS
    m_transport->set_peer_pid(peer_pid);
    return Core::System::getpid();
#endif
    VERIFY_NOT_REACHED();
}

ErrorOr<IPC::TransportHandle> ConnectionFromClient::connect_new_client()
{
    auto paired = TRY(IPC::Transport::create_paired());
    auto handle = move(paired.remote_handle);

    auto client = adopt_ref(*new ConnectionFromClient(move(paired.local)));

    return handle;
}

Messages::GPUProcessServer::ConnectNewClientsResponse ConnectionFromClient::connect_new_clients(size_t count)
{
    Vector<IPC::TransportHandle> handles;
    handles.ensure_capacity(count);
    for (size_t i = 0; i < count; ++i) {
        auto handle_or_error = connect_new_client();
        if (handle_or_error.is_error()) {
            dbgln("GPUProcess: Failed to connect new client: {}", handle_or_error.error());
            return Vector<IPC::TransportHandle> {};
        }
        handles.unchecked_append(handle_or_error.release_value());
    }
    return handles;
}

}
