/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <Compositor/CompositorClientEndpoint.h>
#include <Compositor/CompositorServerEndpoint.h>
#include <LibIPC/ConnectionFromClient.h>

namespace Compositor {

class ConnectionFromClient final
    : public IPC::ConnectionFromClient<CompositorClientEndpoint, CompositorServerEndpoint> {
    C_OBJECT(ConnectionFromClient);

public:
    virtual void die() override;

private:
    explicit ConnectionFromClient(NonnullOwnPtr<IPC::Transport>);

    virtual void ping() override;
    virtual Messages::CompositorServer::AsyncScrollByResponse async_scroll_by(u64 page_id, Gfx::FloatPoint position, Gfx::FloatPoint delta_in_device_pixels) override;
    virtual Messages::CompositorServer::MouseEventResponse mouse_event(u64 page_id, Web::MouseEvent event) override;
    virtual void ready_to_paint(u64 page_id, i32 bitmap_id) override;
};

}
