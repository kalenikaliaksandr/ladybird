/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/ConnectionFromClient.h>
#include <LibCore/EventLoop.h>

namespace Compositor {

ConnectionFromClient::ConnectionFromClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionFromClient<CompositorClientEndpoint, CompositorServerEndpoint>(*this, move(transport), 1)
{
}

void ConnectionFromClient::die()
{
    Core::EventLoop::current().quit(0);
}

void ConnectionFromClient::ping()
{
}

Messages::CompositorServer::AsyncScrollByResponse ConnectionFromClient::async_scroll_by(u64, Gfx::FloatPoint, Gfx::FloatPoint)
{
    return false;
}

Messages::CompositorServer::MouseEventResponse ConnectionFromClient::mouse_event(u64, Web::MouseEvent)
{
    return false;
}

void ConnectionFromClient::ready_to_paint(u64, i32)
{
}

}
