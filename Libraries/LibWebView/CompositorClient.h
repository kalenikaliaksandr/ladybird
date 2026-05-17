/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <Compositor/CompositorClientEndpoint.h>
#include <Compositor/CompositorServerEndpoint.h>
#include <LibIPC/ConnectionToServer.h>
#include <LibWebView/Export.h>

namespace WebView {

class WEBVIEW_API CompositorClient final
    : public IPC::ConnectionToServer<CompositorClientEndpoint, CompositorServerEndpoint>
    , public CompositorClientEndpoint {
    C_OBJECT_ABSTRACT(CompositorClient);

public:
    explicit CompositorClient(NonnullOwnPtr<IPC::Transport>);

private:
    virtual void die() override;

    virtual void did_allocate_backing_stores(
        u64, i32, Gfx::SharedImage, i32, Gfx::SharedImage) override;
    virtual void did_paint(u64, Gfx::IntRect, i32) override;
};

}
