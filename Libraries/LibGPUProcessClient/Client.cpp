/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGPUProcessClient/Client.h>

namespace GPUProcessClient {

Client::Client(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionToServer<GPUProcessClientEndpoint, GPUProcessServerEndpoint>(*this, move(transport))
{
}

void Client::die()
{
    if (on_death)
        on_death();
}

void Client::did_paint(u64 page_id, Gfx::IntRect content_rect, i32 bitmap_id)
{
    if (on_paint_complete)
        on_paint_complete(page_id, content_rect, bitmap_id);
}

void Client::ready_for_next_frame(u64)
{
    // FIXME: Signal backpressure release to the appropriate page
}

}
