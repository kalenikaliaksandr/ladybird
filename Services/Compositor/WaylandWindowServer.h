/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/RefPtr.h>
#include <AK/WeakPtr.h>
#include <Compositor/WaylandPresenterClientEndpoint.h>
#include <Compositor/WaylandPresenterServerEndpoint.h>
#include <Compositor/WindowServer.h>
#include <LibIPC/ConnectionToServer.h>

namespace Compositor {

class WaylandWindowServer;

class ConnectionToWaylandPresenter final
    : public IPC::ConnectionToServer<WaylandPresenterClientEndpoint, WaylandPresenterServerEndpoint> {
public:
    ConnectionToWaylandPresenter(NonnullOwnPtr<IPC::Transport>, WaylandWindowServer&);

private:
    virtual void die() override;
    virtual void did_release_buffer(u64 buffer_id) override;

    WeakPtr<WaylandWindowServer> m_window_server;
};

class WaylandPresenterBuffer final : public WindowServerSurface {
public:
    WaylandPresenterBuffer(WaylandWindowServer&, u64 buffer_id);
    virtual ~WaylandPresenterBuffer() override;

    u64 buffer_id() const { return m_buffer_id; }
    void did_hand_to_presenter() { ++m_frames_shown_by_presenter; }
    void did_release()
    {
        if (m_frames_shown_by_presenter > 0)
            --m_frames_shown_by_presenter;
    }

    virtual bool may_be_read_by_window_server() const override { return m_frames_shown_by_presenter > 0; }

private:
    WeakPtr<WaylandWindowServer> m_window_server;
    u64 m_buffer_id { 0 };
    size_t m_frames_shown_by_presenter { 0 };
};

// The desktop's Wayland compositor, which the UI's presenter thread hands the frames that this process sends it. The
// UI places the presenter's surface for each layer in its browser window.
class WaylandWindowServer final : public WindowServer {
public:
    static NonnullRefPtr<WaylandWindowServer> create(NonnullOwnPtr<IPC::Transport>);
    virtual ~WaylandWindowServer() override;

    virtual OwnPtr<RemoteLayer> try_create_remote_layer(u64 remote_layer_id) override;
    virtual ErrorOr<NonnullRefPtr<PresentableSurface>> try_create_presentable_surface(Gfx::IntSize, RefPtr<Gfx::SkiaBackendContext> const&) override;
    virtual FrameHandOff frame_hand_off() const override { return FrameHandOff::OnceGpuWorkIsFinished; }
    virtual void hold_changes_on_screen() override { }
    virtual void release_changes_held_off_screen() override { }

    ConnectionToWaylandPresenter* presenter() { return m_presenter_is_gone ? nullptr : m_presenter.ptr(); }
    void presenter_did_release_buffer(u64 buffer_id);
    void presenter_disconnected();
    void forget_buffer(WaylandPresenterBuffer&);

private:
    WaylandWindowServer() = default;

    RefPtr<ConnectionToWaylandPresenter> m_presenter;
    bool m_presenter_is_gone { false };
    HashMap<u64, WaylandPresenterBuffer*> m_buffers;
    u64 m_next_buffer_id { 1 };
};

class WaylandRemoteLayer final : public RemoteLayer {
public:
    WaylandRemoteLayer(WaylandWindowServer&, u64 layer_id);
    virtual ~WaylandRemoteLayer() override;

    virtual Compositing::RemoteLayerHostHandle host_handle() const override;
    virtual bool can_show(WindowServerPlacement const&, Content const&, double) const override { return false; }
    virtual void show_frame(Frame) override;
    virtual void place_frame(Optional<Gfx::IntSize> fit_within) override;

private:
    WeakPtr<WaylandWindowServer> m_window_server;
    u64 m_layer_id { 0 };
};

}
