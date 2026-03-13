/*
 * Copyright (c) 2021, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Function.h>
#include <LibCore/LocalServer.h>
#include <LibCore/Socket.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibIPC/Transport.h>

namespace IPC {

template<typename ConnectionFromClientType>
class MultiServer {
public:
    static ErrorOr<NonnullOwnPtr<MultiServer>> try_create(NonnullRefPtr<Core::LocalServer> server)
    {
        return adopt_nonnull_own_or_enomem(new (nothrow) MultiServer(move(server)));
    }

    Function<void(ConnectionFromClientType&)> on_new_client;

private:
    explicit MultiServer(NonnullRefPtr<Core::LocalServer> server)
        : m_server(move(server))
    {
        m_server->on_accept = [&](auto client_socket) {
            auto client_id = ++m_next_client_id;

            auto transport = MUST(IPC::Transport::from_socket(move(client_socket)));
            auto client = IPC::new_client_connection<ConnectionFromClientType>(move(transport), client_id);
            if (on_new_client)
                on_new_client(*client);
        };
    }

    int m_next_client_id { 0 };
    RefPtr<Core::LocalServer> m_server;
};

}
