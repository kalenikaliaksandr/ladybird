/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/ConditionVariable.h>
#include <AK/Function.h>
#include <AK/HashMap.h>
#include <AK/Mutex.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <AK/Vector.h>
#include <AK/kmalloc.h>
#include <LibCore/Forward.h>
#include <LibGfx/LinuxDmaBufHandle.h>
#include <LibGfx/Size.h>
#include <LibIPC/TransportHandle.h>
#include <LibThreading/Forward.h>

#include <QRect>

struct wl_buffer;
struct wl_compositor;
struct wl_display;
struct wl_event_queue;
struct wl_surface;
struct wp_viewport;
struct wp_viewporter;
struct zwp_linux_buffer_params_v1;
struct zwp_linux_dmabuf_v1;

namespace Ladybird {

class WaylandPresenter;
class WaylandPresenterConnection;
struct WaylandPresenterBuffer;

// A surface that shows the frames of one of the Compositor's remote layers. The presenter thread commits each frame to
// it as the frame arrives, and the main thread makes it a subsurface of the browser window and places it there.
class WaylandPresenterLayer final : public AtomicRefCounted<WaylandPresenterLayer> {
public:
    AK_ALLOC_WITH_KMALLOC;

    ~WaylandPresenterLayer();

    u64 id() const { return m_id; }
    wl_surface* surface() const { return m_surface; }

    struct Placement {
        // In the device pixels of the view.
        QRect rect_the_frame_may_cover;
        bool scales_frame_to_fit { false };
        double device_pixel_ratio { 1.0 };

        bool operator==(Placement const&) const = default;
    };
    Optional<Placement> placement() const;
    // Answers whether the surface committed the frame it shows placed anew. A surface that is shown must stop being
    // shown before it is unplaced, as it would show the whole frame otherwise.
    bool place(Optional<Placement>);

    QRect covered_rect() const;
    Optional<Gfx::IntSize> painted_size_of_latest_frame() const;
    bool frames_cannot_be_shown() const;

    // These are called on the main thread.
    Function<void()> on_frames_cannot_be_shown;
    Function<void()> on_placement_out_of_date;
    Function<void()> on_covered_rect_shrank;

private:
    friend class WaylandPresenter;

    struct Frame {
        Gfx::IntSize painted_size;
        double device_pixel_ratio { 1.0 };
        Optional<Gfx::IntSize> fit_within;
    };

    WaylandPresenterLayer(WaylandPresenter&, u64 id, wl_surface*, wp_viewport*);

    WaylandPresenter& m_presenter;
    u64 m_id { 0 };
    wl_surface* m_surface { nullptr };
    wp_viewport* m_viewport { nullptr };

    Optional<Placement> m_placement;
    Frame m_latest_frame;
    WaylandPresenterBuffer* m_latest_buffer { nullptr };
    WaylandPresenterBuffer* m_attached_buffer { nullptr };
    QRect m_covered_rect;
    bool m_frames_cannot_be_shown { false };
};

// Shows the frames that the Compositor sends it straight from its own process, on a thread of its own, so that they
// reach the screen without waiting for the main thread.
class WaylandPresenter {
public:
    AK_ALLOC_WITH_KMALLOC;

    // Null off Wayland, or where the desktop's compositor lacks what the presenter needs.
    static WaylandPresenter* the();

    void connect_to_compositor(IPC::TransportHandle);
    NonnullRefPtr<WaylandPresenterLayer> layer(u64 layer_id);

    void create_layer(u64 layer_id);
    void destroy_layer(u64 layer_id);
    void register_buffer(u64 buffer_id, Gfx::LinuxDmaBufHandle);
    void forget_buffer(u64 buffer_id);
    void show_frame(u64 layer_id, u64 buffer_id, Gfx::IntSize painted_size, double device_pixel_ratio, Optional<Gfx::IntSize> fit_within);
    void place_frame(u64 layer_id, Optional<Gfx::IntSize> fit_within);
    void compositor_disconnected();

private:
    friend class WaylandPresenterLayer;

    struct Globals {
        wl_display* display { nullptr };
        wl_compositor* compositor { nullptr };
        wp_viewporter* viewporter { nullptr };
        zwp_linux_dmabuf_v1* dmabuf { nullptr };
    };

    explicit WaylandPresenter(Globals);

    intptr_t thread_main();
    void invoke_on_presenter_thread(Function<void()>);
    void stop();
    void prepare_to_read_events();
    void read_and_dispatch_events();

    WaylandPresenterLayer& ensure_layer(u64 layer_id);
    WaylandPresenterBuffer* buffer_of_connected_compositor(u64 buffer_id);
    void commit_latest_frame(WaylandPresenterLayer&);
    bool is_latest_buffer_of_a_layer(WaylandPresenterBuffer const&) const;
    void release_or_destroy_buffer_if_unused(WaylandPresenterBuffer&);
    void notify_main_thread(WaylandPresenterLayer&, Function<void()> WaylandPresenterLayer::* callback);
    void layer_cannot_show_frames(WaylandPresenterLayer&);

    static void buffer_created(void*, zwp_linux_buffer_params_v1*, wl_buffer*);
    static void buffer_failed(void*, zwp_linux_buffer_params_v1*);
    static void buffer_released(void*, wl_buffer*);

    Globals m_globals;
    wl_event_queue* m_queue { nullptr };
    wl_compositor* m_compositor_on_queue { nullptr };

    RefPtr<Threading::Thread> m_thread;
    mutable Mutex m_mutex;
    ConditionVariable m_thread_started { m_mutex };
    RefPtr<Core::WeakEventLoopReference> m_event_loop;

    // Only the presenter thread touches these.
    RefPtr<Core::Notifier> m_display_notifier;
    bool m_prepared_to_read { false };
    RefPtr<WaylandPresenterConnection> m_connection;

    // The mutex guards these, and what every layer shows.
    u64 m_compositor_generation { 0 };
    HashMap<u64, NonnullRefPtr<WaylandPresenterLayer>> m_layers;
    Vector<NonnullOwnPtr<WaylandPresenterBuffer>> m_buffers;
};

}
