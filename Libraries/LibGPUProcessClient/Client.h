/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <GPUProcess/GPUProcessClientEndpoint.h>
#include <GPUProcess/GPUProcessServerEndpoint.h>
#include <LibIPC/ConnectionToServer.h>

namespace GPUProcessClient {

class Client final
    : public IPC::ConnectionToServer<GPUProcessClientEndpoint, GPUProcessServerEndpoint>
    , public GPUProcessClientEndpoint {
    C_OBJECT_ABSTRACT(Client);

public:
    using InitTransport = Messages::GPUProcessServer::InitTransport;

    Client(NonnullOwnPtr<IPC::Transport>);

    Function<void()> on_death;

private:
    virtual void die() override;

    virtual void did_paint(u64 page_id, Gfx::IntRect content_rect, i32 bitmap_id) override;
    virtual void ready_for_next_frame(u64 page_id) override;
};

}
