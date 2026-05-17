/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWebView/CompositorClient.h>

namespace WebView {

CompositorClient::CompositorClient(NonnullOwnPtr<IPC::Transport> transport)
    : IPC::ConnectionToServer<CompositorClientEndpoint, CompositorServerEndpoint>(*this, move(transport))
{
}

void CompositorClient::die()
{
}

void CompositorClient::did_allocate_backing_stores(u64, i32, Gfx::SharedImage, i32, Gfx::SharedImage)
{
}

void CompositorClient::did_paint(u64, Gfx::IntRect, i32)
{
}

}
