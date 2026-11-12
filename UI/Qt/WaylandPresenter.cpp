/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Format.h>
#include <Compositor/WaylandPresenterClientEndpoint.h>
#include <Compositor/WaylandPresenterServerEndpoint.h>
#include <LibCompositing/FrameFitting.h>
#include <LibCore/EventLoop.h>
#include <LibCore/Notifier.h>
#include <LibCore/ThreadEventQueue.h>
#include <LibIPC/ConnectionFromClient.h>
#include <LibThreading/Thread.h>
#include <UI/Qt/WaylandPresenter.h>
#include <UI/Qt/WaylandRegistry.h>

#include <QCoreApplication>
#include <QGuiApplication>
#include <QMetaObject>

#include <libdrm/drm_fourcc.h>
#include <wayland-client.h>
#include <wayland-version.h>

// NB: The generated listener functions cast the const away from the listeners they pass to libwayland.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wcast-qual"
#include <linux-dmabuf-v1-client-protocol.h>
#include <viewporter-client-protocol.h>
#pragma GCC diagnostic pop

namespace Ladybird {

struct WaylandPresenterBuffer {
    AK_ALLOC_WITH_KMALLOC;

    enum class State : u8 {
        Creating,
        Ready,
        Failed,
    };

    WaylandPresenter* presenter { nullptr };
    u64 compositor_generation { 0 };
    u64 id { 0 };
    Gfx::IntSize size;
    u32 drm_format { 0 };
    u64 modifier { 0 };
    State state { State::Creating };
    zwp_linux_buffer_params_v1* params { nullptr };
    wl_buffer* buffer { nullptr };
    size_t attachments_awaiting_release { 0 };
    bool owes_release_to_compositor { false };
    bool forgotten_by_compositor { false };
};

class WaylandPresenterConnection final
    : public IPC::ConnectionFromClient<WaylandPresenterClientEndpoint, WaylandPresenterServerEndpoint> {
public:
    WaylandPresenterConnection(NonnullOwnPtr<IPC::Transport> transport, WaylandPresenter& presenter)
        : IPC::ConnectionFromClient<WaylandPresenterClientEndpoint, WaylandPresenterServerEndpoint>(*this, move(transport), 1)
        , m_presenter(presenter)
    {
    }

    void detach() { m_detached = true; }

private:
    virtual void die() override
    {
        if (!exchange(m_detached, true))
            m_presenter.compositor_disconnected();
    }

    virtual void create_layer(u64 layer_id) override { m_presenter.create_layer(layer_id); }
    virtual void destroy_layer(u64 layer_id) override { m_presenter.destroy_layer(layer_id); }
    virtual void register_buffer(u64 buffer_id, Gfx::LinuxDmaBufHandle dmabuf) override { m_presenter.register_buffer(buffer_id, move(dmabuf)); }
    virtual void forget_buffer(u64 buffer_id) override { m_presenter.forget_buffer(buffer_id); }
    virtual void show_frame(u64 layer_id, u64 buffer_id, Gfx::IntSize painted_size, double device_pixel_ratio, Optional<Gfx::IntSize> fit_within) override { m_presenter.show_frame(layer_id, buffer_id, painted_size, device_pixel_ratio, fit_within); }
    virtual void place_frame(u64 layer_id, Optional<Gfx::IntSize> fit_within) override { m_presenter.place_frame(layer_id, fit_within); }

    WaylandPresenter& m_presenter;
    bool m_detached { false };
};

// NB: The page is opaque. Telling the compositor so lets it skip what is behind the page, or scan the frame out.
static u32 opaque_drm_format(u32 drm_format)
{
    if (drm_format == DRM_FORMAT_ARGB8888)
        return DRM_FORMAT_XRGB8888;
    if (drm_format == DRM_FORMAT_ABGR8888)
        return DRM_FORMAT_XBGR8888;
    return drm_format;
}

WaylandPresenter* WaylandPresenter::the()
{
    static WaylandPresenter* presenter = []() -> WaylandPresenter* {
        if (QGuiApplication::platformName() != "wayland")
            return nullptr;
        auto* wayland_application = qGuiApp->nativeInterface<QNativeInterface::QWaylandApplication>();
        if (!wayland_application || !wayland_application->display() || !wayland_application->compositor())
            return nullptr;
        auto registry = WaylandRegistry::create();
        if (!registry)
            return nullptr;

        Globals globals {
            .display = wayland_application->display(),
            .compositor = wayland_application->compositor(),
            .viewporter = registry->bind<wp_viewporter>(wp_viewporter_interface, 1),
            .dmabuf = registry->bind<zwp_linux_dmabuf_v1>(zwp_linux_dmabuf_v1_interface, 3),
        };
        if (!globals.viewporter || !globals.dmabuf) {
            if (globals.viewporter)
                wp_viewporter_destroy(globals.viewporter);
            if (globals.dmabuf)
                zwp_linux_dmabuf_v1_destroy(globals.dmabuf);
            return nullptr;
        }
        return new WaylandPresenter(globals);
    }();
    return presenter;
}

static wl_event_queue* create_presenter_queue(wl_display* display)
{
#if WAYLAND_VERSION_MAJOR > 1 || WAYLAND_VERSION_MINOR >= 23
    return wl_display_create_queue_with_name(display, "WaylandPresenter");
#else
    return wl_display_create_queue(display);
#endif
}

WaylandPresenter::WaylandPresenter(Globals globals)
    : m_globals(globals)
    , m_queue(create_presenter_queue(globals.display))
{
    wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(m_globals.viewporter), m_queue);
    wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(m_globals.dmabuf), m_queue);
    m_compositor_on_queue = static_cast<wl_compositor*>(wl_proxy_create_wrapper(m_globals.compositor));
    wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(m_compositor_on_queue), m_queue);

    m_thread = Threading::Thread::construct("WaylandPresenter"sv, [this] { return thread_main(); });
    m_thread->start();
    {
        MutexLocker locker(m_mutex);
        m_thread_started.wait_while([&] { return !m_event_loop; });
    }

    // NB: The presenter thread takes part in every read of the Wayland connection, so it has to be gone before Qt
    //     disconnects from the compositor, or Qt would wait for it.
    QObject::connect(qApp, &QCoreApplication::aboutToQuit, [this] { stop(); });
}

intptr_t WaylandPresenter::thread_main()
{
    Core::EventLoop event_loop;
    m_display_notifier = Core::Notifier::construct(wl_display_get_fd(m_globals.display), Core::Notifier::Type::Read);
    m_display_notifier->on_activation = [this] { read_and_dispatch_events(); };
    prepare_to_read_events();
    {
        MutexLocker locker(m_mutex);
        m_event_loop = Core::EventLoop::current_weak();
        m_thread_started.broadcast();
    }

    auto result = event_loop.exec();

    if (exchange(m_prepared_to_read, false))
        wl_display_cancel_read(m_globals.display);
    m_display_notifier = nullptr;
    if (auto connection = move(m_connection)) {
        connection->detach();
        connection->shutdown();
    }
    while (Core::ThreadEventQueue::current().process() > 0) { }

    MutexLocker locker(m_mutex);
    m_event_loop = nullptr;
    return result;
}

void WaylandPresenter::invoke_on_presenter_thread(Function<void()> function)
{
    RefPtr<Core::WeakEventLoopReference> event_loop;
    {
        MutexLocker locker(m_mutex);
        event_loop = m_event_loop;
    }
    if (!event_loop)
        return;
    auto strong_event_loop = event_loop->take();
    if (!strong_event_loop)
        return;
    strong_event_loop->deferred_invoke(move(function));
    // NB: A deferred invocation wakes the main thread's event loop under Qt, not this one.
    strong_event_loop->wake();
}

void WaylandPresenter::stop()
{
    if (!m_thread)
        return;
    invoke_on_presenter_thread([] { Core::EventLoop::current().quit(0); });
    (void)m_thread->join();
    m_thread = nullptr;
}

// NB: Qt reads the Wayland connection on a thread of its own. Events for this thread's queue are only read while this
//     thread is prepared to read too, and every reader that prepared has to read or cancel before any read happens.
void WaylandPresenter::prepare_to_read_events()
{
    while (wl_display_prepare_read_queue(m_globals.display, m_queue) != 0)
        wl_display_dispatch_queue_pending(m_globals.display, m_queue);
    wl_display_flush(m_globals.display);
    m_prepared_to_read = true;
}

void WaylandPresenter::read_and_dispatch_events()
{
    if (exchange(m_prepared_to_read, false) && wl_display_read_events(m_globals.display) < 0) {
        dbgln("WaylandPresenter: Unable to read from the Wayland connection");
        m_display_notifier->set_enabled(false);
        return;
    }
    wl_display_dispatch_queue_pending(m_globals.display, m_queue);
    prepare_to_read_events();
}

void WaylandPresenter::connect_to_compositor(IPC::TransportHandle handle)
{
    invoke_on_presenter_thread([this, handle = move(handle)]() mutable {
        if (auto connection = move(m_connection)) {
            connection->detach();
            connection->shutdown();
            compositor_disconnected();
        }
        auto transport = handle.create_transport();
        if (transport.is_error()) {
            dbgln("WaylandPresenter: Unable to connect to the Compositor: {}", transport.error());
            return;
        }
        {
            MutexLocker locker(m_mutex);
            ++m_compositor_generation;
        }
        m_connection = adopt_ref(*new WaylandPresenterConnection(transport.release_value(), *this));
    });
}

void WaylandPresenter::compositor_disconnected()
{
    {
        MutexLocker locker(m_mutex);
        for (auto& [_, layer] : m_layers)
            layer->m_latest_buffer = nullptr;
        m_layers.clear();
        for (auto& buffer : m_buffers) {
            if (buffer->compositor_generation != m_compositor_generation)
                continue;
            buffer->forgotten_by_compositor = true;
            buffer->owes_release_to_compositor = false;
        }
        for (size_t i = m_buffers.size(); i-- > 0;)
            release_or_destroy_buffer_if_unused(*m_buffers[i]);
    }
    // NB: The connection is still on the stack of whatever noticed that it is gone.
    Core::deferred_invoke([this, connection = m_connection] {
        if (m_connection == connection)
            m_connection = nullptr;
    });
}

NonnullRefPtr<WaylandPresenterLayer> WaylandPresenter::layer(u64 layer_id)
{
    MutexLocker locker(m_mutex);
    return ensure_layer(layer_id);
}

WaylandPresenterLayer& WaylandPresenter::ensure_layer(u64 layer_id)
{
    return *m_layers.ensure(layer_id, [&] {
        auto* surface = wl_compositor_create_surface(m_compositor_on_queue);
        auto* viewport = wp_viewporter_get_viewport(m_globals.viewporter, surface);
        // NB: Input goes to the browser window underneath, where Qt delivers it to the view.
        auto* empty_region = wl_compositor_create_region(m_compositor_on_queue);
        wl_surface_set_input_region(surface, empty_region);
        wl_region_destroy(empty_region);
        return adopt_ref(*new WaylandPresenterLayer(*this, layer_id, surface, viewport));
    });
}

void WaylandPresenter::create_layer(u64 layer_id)
{
    MutexLocker locker(m_mutex);
    (void)ensure_layer(layer_id);
}

void WaylandPresenter::destroy_layer(u64 layer_id)
{
    MutexLocker locker(m_mutex);
    auto layer = m_layers.take(layer_id);
    if (!layer.has_value())
        return;
    if (auto* buffer = exchange((*layer)->m_latest_buffer, nullptr))
        release_or_destroy_buffer_if_unused(*buffer);
}

WaylandPresenterBuffer* WaylandPresenter::buffer_of_connected_compositor(u64 buffer_id)
{
    for (auto& buffer : m_buffers) {
        if (buffer->compositor_generation == m_compositor_generation && buffer->id == buffer_id && !buffer->forgotten_by_compositor)
            return buffer.ptr();
    }
    return nullptr;
}

void WaylandPresenter::register_buffer(u64 buffer_id, Gfx::LinuxDmaBufHandle dmabuf)
{
    if (dmabuf.objects.is_empty() || dmabuf.planes.is_empty())
        return;
    for (auto const& plane : dmabuf.planes) {
        if (plane.object_index >= dmabuf.objects.size())
            return;
    }

    auto buffer = make<WaylandPresenterBuffer>();
    buffer->presenter = this;
    buffer->id = buffer_id;
    buffer->size = dmabuf.size;
    buffer->drm_format = opaque_drm_format(dmabuf.drm_format);
    buffer->modifier = dmabuf.modifier;
    buffer->params = zwp_linux_dmabuf_v1_create_params(m_globals.dmabuf);
    for (size_t plane_index = 0; plane_index < dmabuf.planes.size(); ++plane_index) {
        auto const& plane = dmabuf.planes[plane_index];
        zwp_linux_buffer_params_v1_add(buffer->params, dmabuf.objects[plane.object_index].fd(), plane_index, plane.offset, plane.pitch,
            static_cast<u32>(dmabuf.modifier >> 32), static_cast<u32>(dmabuf.modifier & 0xffffffff));
    }
    static zwp_linux_buffer_params_v1_listener const params_listener {
        .created = buffer_created,
        .failed = buffer_failed,
    };
    zwp_linux_buffer_params_v1_add_listener(buffer->params, &params_listener, buffer.ptr());
    zwp_linux_buffer_params_v1_create(buffer->params, dmabuf.size.width(), dmabuf.size.height(), buffer->drm_format, 0);
    wl_display_flush(m_globals.display);

    MutexLocker locker(m_mutex);
    buffer->compositor_generation = m_compositor_generation;
    m_buffers.append(move(buffer));
}

void WaylandPresenter::forget_buffer(u64 buffer_id)
{
    MutexLocker locker(m_mutex);
    auto* buffer = buffer_of_connected_compositor(buffer_id);
    if (!buffer)
        return;
    buffer->forgotten_by_compositor = true;
    buffer->owes_release_to_compositor = false;
    release_or_destroy_buffer_if_unused(*buffer);
}

void WaylandPresenter::show_frame(u64 layer_id, u64 buffer_id, Gfx::IntSize painted_size, double device_pixel_ratio, Optional<Gfx::IntSize> fit_within)
{
    MutexLocker locker(m_mutex);
    auto* buffer = buffer_of_connected_compositor(buffer_id);
    auto layer = m_layers.get(layer_id);
    if (!buffer || !layer.has_value()) {
        if (m_connection)
            m_connection->async_did_release_buffer(buffer_id);
        return;
    }
    auto& shown_layer = **layer;
    buffer->owes_release_to_compositor = true;
    auto* replaced_buffer = exchange(shown_layer.m_latest_buffer, buffer);
    shown_layer.m_latest_frame = { painted_size, device_pixel_ratio, fit_within };
    commit_latest_frame(shown_layer);
    if (replaced_buffer && replaced_buffer != buffer)
        release_or_destroy_buffer_if_unused(*replaced_buffer);
    if (buffer->state == WaylandPresenterBuffer::State::Failed) {
        layer_cannot_show_frames(shown_layer);
        release_or_destroy_buffer_if_unused(*buffer);
    }
}

void WaylandPresenter::layer_cannot_show_frames(WaylandPresenterLayer& layer)
{
    if (!exchange(layer.m_frames_cannot_be_shown, true))
        notify_main_thread(layer, &WaylandPresenterLayer::on_frames_cannot_be_shown);
}

void WaylandPresenter::place_frame(u64 layer_id, Optional<Gfx::IntSize> fit_within)
{
    MutexLocker locker(m_mutex);
    auto layer = m_layers.get(layer_id);
    if (!layer.has_value())
        return;
    (*layer)->m_latest_frame.fit_within = fit_within;
    commit_latest_frame(**layer);
}

void WaylandPresenter::notify_main_thread(WaylandPresenterLayer& layer, Function<void()> WaylandPresenterLayer::* callback)
{
    auto call_callback = [layer = NonnullRefPtr { layer }, callback] {
        if (auto& function = layer.ptr()->*callback)
            function();
    };
    QMetaObject::invokeMethod(qApp, move(call_callback), Qt::QueuedConnection);
}

// NB: Wayland does not clip a subsurface to its parent, so the frame is cropped to the part of the view that the main
//     thread placed the surface over. A surface that is not placed is not shown, and shows the whole frame.
void WaylandPresenter::commit_latest_frame(WaylandPresenterLayer& layer)
{
    auto* buffer = layer.m_latest_buffer;
    if (!buffer || buffer->state != WaylandPresenterBuffer::State::Ready)
        return;

    auto const& frame = layer.m_latest_frame;
    QRect source { 0, 0, min(frame.painted_size.width(), buffer->size.width()), min(frame.painted_size.height(), buffer->size.height()) };
    QRect covered = source;
    auto device_pixel_ratio = frame.device_pixel_ratio;
    if (layer.m_placement.has_value()) {
        auto const& placement = *layer.m_placement;
        device_pixel_ratio = placement.device_pixel_ratio;
        if (placement.scales_frame_to_fit) {
            covered = placement.rect_the_frame_may_cover;
            if (frame.fit_within.has_value()) {
                auto fitted = Compositing::rect_fitting_frame(frame.painted_size, *frame.fit_within);
                if (QRect { fitted.x(), fitted.y(), fitted.width(), fitted.height() } != covered)
                    notify_main_thread(layer, &WaylandPresenterLayer::on_placement_out_of_date);
            }
        } else {
            source &= placement.rect_the_frame_may_cover;
            covered = source;
        }
    }

    if (source.isEmpty() || covered.isEmpty()) {
        if (layer.m_attached_buffer) {
            wl_surface_attach(layer.m_surface, nullptr, 0, 0);
            wl_surface_commit(layer.m_surface);
            wl_display_flush(m_globals.display);
            layer.m_attached_buffer = nullptr;
        }
        covered = {};
    } else {
        auto to_logical = [&](int device_pixels) { return max(1, qRound(device_pixels / device_pixel_ratio)); };
        auto destination_width = to_logical(covered.width());
        auto destination_height = to_logical(covered.height());
        wp_viewport_set_source(layer.m_viewport, wl_fixed_from_int(source.x()), wl_fixed_from_int(source.y()), wl_fixed_from_int(source.width()), wl_fixed_from_int(source.height()));
        wp_viewport_set_destination(layer.m_viewport, destination_width, destination_height);

        auto* opaque_region = wl_compositor_create_region(m_compositor_on_queue);
        wl_region_add(opaque_region, 0, 0, destination_width, destination_height);
        wl_surface_set_opaque_region(layer.m_surface, opaque_region);
        wl_region_destroy(opaque_region);

        if (layer.m_attached_buffer != buffer) {
            wl_surface_attach(layer.m_surface, buffer->buffer, 0, 0);
            wl_surface_damage_buffer(layer.m_surface, 0, 0, INT32_MAX, INT32_MAX);
            ++buffer->attachments_awaiting_release;
            layer.m_attached_buffer = buffer;
        }
        wl_surface_commit(layer.m_surface);
        // NB: Requests from this thread would otherwise wait for Qt's main thread to flush the connection.
        wl_display_flush(m_globals.display);
    }

    if (layer.m_placement.has_value() && !covered.contains(layer.m_covered_rect))
        notify_main_thread(layer, &WaylandPresenterLayer::on_covered_rect_shrank);
    layer.m_covered_rect = layer.m_placement.has_value() ? covered : QRect {};
}

bool WaylandPresenter::is_latest_buffer_of_a_layer(WaylandPresenterBuffer const& buffer) const
{
    for (auto const& [_, layer] : m_layers) {
        if (layer->m_latest_buffer == &buffer)
            return true;
    }
    return false;
}

// NB: The desktop's compositor may wait for the GPU to stop writing a dma-buf before it finishes importing it, so a
//     buffer is not handed back to be painted into again while it is still being created.
void WaylandPresenter::release_or_destroy_buffer_if_unused(WaylandPresenterBuffer& buffer)
{
    if (buffer.attachments_awaiting_release > 0 || buffer.state == WaylandPresenterBuffer::State::Creating)
        return;
    if (buffer.state != WaylandPresenterBuffer::State::Failed && is_latest_buffer_of_a_layer(buffer))
        return;
    if (exchange(buffer.owes_release_to_compositor, false) && m_connection && buffer.compositor_generation == m_compositor_generation)
        m_connection->async_did_release_buffer(buffer.id);
    if (!buffer.forgotten_by_compositor || is_latest_buffer_of_a_layer(buffer))
        return;
    if (buffer.buffer)
        wl_buffer_destroy(buffer.buffer);
    m_buffers.remove_first_matching([&](auto const& candidate) { return candidate.ptr() == &buffer; });
}

void WaylandPresenter::buffer_created(void* data, zwp_linux_buffer_params_v1*, wl_buffer* wayland_buffer)
{
    auto& buffer = *static_cast<WaylandPresenterBuffer*>(data);
    auto& presenter = *buffer.presenter;
    MutexLocker locker(presenter.m_mutex);
    zwp_linux_buffer_params_v1_destroy(buffer.params);
    buffer.params = nullptr;
    buffer.buffer = wayland_buffer;
    buffer.state = WaylandPresenterBuffer::State::Ready;
    static wl_buffer_listener const buffer_listener {
        .release = buffer_released,
    };
    wl_buffer_add_listener(wayland_buffer, &buffer_listener, &buffer);

    for (auto& [_, layer] : presenter.m_layers) {
        if (layer->m_latest_buffer == &buffer)
            presenter.commit_latest_frame(*layer);
    }
    presenter.release_or_destroy_buffer_if_unused(buffer);
}

void WaylandPresenter::buffer_failed(void* data, zwp_linux_buffer_params_v1*)
{
    auto& buffer = *static_cast<WaylandPresenterBuffer*>(data);
    auto& presenter = *buffer.presenter;
    MutexLocker locker(presenter.m_mutex);
    zwp_linux_buffer_params_v1_destroy(buffer.params);
    buffer.params = nullptr;
    buffer.state = WaylandPresenterBuffer::State::Failed;
    dbgln("WaylandPresenter: The compositor cannot show a page frame in a dma-buf of format {:#x} with modifier {:#x}", buffer.drm_format, buffer.modifier);

    for (auto& [_, layer] : presenter.m_layers) {
        if (layer->m_latest_buffer == &buffer)
            presenter.layer_cannot_show_frames(*layer);
    }
    presenter.release_or_destroy_buffer_if_unused(buffer);
}

void WaylandPresenter::buffer_released(void* data, wl_buffer*)
{
    auto& buffer = *static_cast<WaylandPresenterBuffer*>(data);
    auto& presenter = *buffer.presenter;
    MutexLocker locker(presenter.m_mutex);
    if (buffer.attachments_awaiting_release > 0)
        --buffer.attachments_awaiting_release;
    presenter.release_or_destroy_buffer_if_unused(buffer);
}

WaylandPresenterLayer::WaylandPresenterLayer(WaylandPresenter& presenter, u64 id, wl_surface* surface, wp_viewport* viewport)
    : m_presenter(presenter)
    , m_id(id)
    , m_surface(surface)
    , m_viewport(viewport)
{
}

WaylandPresenterLayer::~WaylandPresenterLayer()
{
    wp_viewport_destroy(m_viewport);
    wl_surface_destroy(m_surface);
    wl_display_flush(m_presenter.m_globals.display);
}

Optional<WaylandPresenterLayer::Placement> WaylandPresenterLayer::placement() const
{
    MutexLocker locker(m_presenter.m_mutex);
    return m_placement;
}

bool WaylandPresenterLayer::place(Optional<Placement> placement)
{
    MutexLocker locker(m_presenter.m_mutex);
    if (m_placement == placement)
        return false;
    m_placement = placement;
    if (!placement.has_value()) {
        m_covered_rect = {};
        return false;
    }
    if (!m_latest_buffer || m_latest_buffer->state != WaylandPresenterBuffer::State::Ready)
        return false;
    m_presenter.commit_latest_frame(*this);
    return true;
}

QRect WaylandPresenterLayer::covered_rect() const
{
    MutexLocker locker(m_presenter.m_mutex);
    return m_covered_rect;
}

bool WaylandPresenterLayer::frames_cannot_be_shown() const
{
    MutexLocker locker(m_presenter.m_mutex);
    return m_frames_cannot_be_shown;
}

Optional<Gfx::IntSize> WaylandPresenterLayer::painted_size_of_latest_frame() const
{
    MutexLocker locker(m_presenter.m_mutex);
    if (!m_latest_buffer)
        return {};
    return m_latest_frame.painted_size;
}

}
