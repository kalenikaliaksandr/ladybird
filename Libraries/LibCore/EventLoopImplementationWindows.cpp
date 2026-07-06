/*
 * Copyright (c) 2023, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2024-2025, stasoid <stasoid@yahoo.com>
 * Copyright (c) 2025, ayeteadoe <ayeteadoe@gmail.com>
 * Copyright (c) 2025, Ryszard Goc <ryszardgoc@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <AK/Diagnostics.h>
#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/Windows.h>
#include <LibCore/EventLoop.h>
#include <LibCore/EventLoopImplementationWindows.h>
#include <LibCore/Notifier.h>
#include <LibCore/System.h>
#include <LibCore/ThreadEventQueue.h>
#include <LibCore/Timer.h>
#include <LibSync/Mutex.h>
#include <LibSync/MutexProtected.h>
#include <signal.h>

struct OwnHandle {
    HANDLE handle = NULL;

    explicit OwnHandle(HANDLE h = NULL)
        : handle(h)
    {
    }

    OwnHandle(OwnHandle&& h)
    {
        handle = h.handle;
        h.handle = NULL;
    }

    // This operation can only be done when handle is NULL
    OwnHandle& operator=(OwnHandle&& other)
    {
        VERIFY(!handle);
        if (this == &other)
            return *this;
        handle = other.handle;
        other.handle = NULL;
        return *this;
    }

    ~OwnHandle()
    {
        if (handle)
            CloseHandle(handle);
    }

    bool operator==(OwnHandle const& h) const { return handle == h.handle; }
    bool operator==(HANDLE h) const { return handle == h; }
};

template<>
struct Traits<OwnHandle> : DefaultTraits<OwnHandle> {
    static unsigned hash(OwnHandle const& h) { return Traits<HANDLE>::hash(h.handle); }
};
template<>
constexpr bool IsHashCompatible<HANDLE, OwnHandle> = true;

namespace Core {

enum class CompletionType : u8 {
    Wake,
    Timer,
    Notifer,
    SocketNotifiers,
    Process,
    Signal,
};

struct CompletionPacket {
    CompletionType type;
};

struct EventLoopWake final : CompletionPacket {
    OwnHandle wait_packet;
    OwnHandle wait_event;
};

struct EventLoopTimer final : CompletionPacket {

    ~EventLoopTimer()
    {
        CancelWaitableTimer(timer.handle);
    }

    OwnHandle timer;
    OwnHandle wait_packet;
    bool is_periodic;
    WeakPtr<EventReceiver> owner;
};

// A pipe notifier keeps a zero-byte overlapped read pending on the pipe; its completion signals wait_event once
// the pipe has data to read (or is broken).
struct EventLoopNotifier final : CompletionPacket {

    ~EventLoopNotifier()
    {
    }

    Notifier* notifier;
    OwnHandle wait_packet;
    OwnHandle wait_event;

    bool zero_read_pending = false;
    OVERLAPPED overlapped = {};
};

// All notifiers watching one socket. WSAEventSelect() supports only a single event registration per socket (a new
// call replaces the previous selection), so every notifier for a socket shares one registration selecting the union
// of their events, and WSAEnumNetworkEvents() decides which notifiers to activate.
struct EventLoopSocketNotifiers final : CompletionPacket {
    SOCKET socket { INVALID_SOCKET };
    OwnHandle wait_packet;
    OwnHandle wait_event;
    Vector<Notifier*> notifiers;
};

struct EventLoopProcess final : CompletionPacket {
    ~EventLoopProcess() = default;

    OwnHandle process;
    pid_t pid;
    Function<void(pid_t)> exit_handler;
    OwnHandle jobobject;
};

// Signals are emulated with console control events: the console control handler (which runs on a dedicated thread)
// records the signal number and signals wait_event, and the event loop thread then runs the registered handlers.
struct EventLoopSignal final : CompletionPacket {
    OwnHandle wait_packet;
    OwnHandle wait_event;
};

struct SignalHandler {
    int signal_number = 0;
    Function<void(int)> handler;
};

struct ThreadData;

// Which threads want to be woken for which signals. Written by event loop threads, read by the console control
// handler thread.
struct SignalWakeTarget {
    int signal_number = 0;
    ThreadData* thread_data = nullptr;
};

static Sync::MutexProtected<Vector<SignalWakeTarget>>& signal_wake_targets()
{
    static Sync::MutexProtected<Vector<SignalWakeTarget>> targets;
    return targets;
}

struct ThreadData {
    static ThreadData* the()
    {
        thread_local OwnPtr<ThreadData> thread_data = make<ThreadData>();
        if (thread_data)
            return &*thread_data;
        return nullptr;
    }

    ThreadData()
        : wake_data(make<EventLoopWake>())
    {
        wake_data->type = CompletionType::Wake;
        wake_data->wait_event.handle = CreateEvent(NULL, FALSE, FALSE, NULL);

        // Consider a way for different event loops to have a different number of threads
        iocp.handle = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
        VERIFY(iocp.handle);

        NTSTATUS status = g_system.NtCreateWaitCompletionPacket(&wake_data->wait_packet.handle, GENERIC_READ | GENERIC_WRITE, NULL);
        VERIFY(NT_SUCCESS(status));
        status = g_system.NtAssociateWaitCompletionPacket(wake_data->wait_packet.handle, iocp.handle, wake_data->wait_event.handle, wake_data.ptr(), NULL, 0, 0, NULL);
        VERIFY(NT_SUCCESS(status));
    }
    ~ThreadData()
    {
        signal_wake_targets().with_locked([this](auto& targets) {
            targets.remove_all_matching([this](auto const& target) { return target.thread_data == this; });
        });

        NTSTATUS status = g_system.NtCancelWaitCompletionPacket(wake_data->wait_packet.handle, TRUE);
        VERIFY(NT_SUCCESS(status));
    }

    OwnHandle iocp;

    // These are only used to register and unregister. The event loop doesn't access these.
    HashMap<intptr_t, NonnullOwnPtr<EventLoopTimer>> timers;
    HashMap<Notifier*, NonnullOwnPtr<EventLoopNotifier>> notifiers;
    HashMap<SOCKET, NonnullOwnPtr<EventLoopSocketNotifiers>> socket_notifiers;

    // Signal handlers registered on this thread; only accessed from this thread.
    HashMap<int, SignalHandler> signal_handlers_by_id;
    int next_signal_handler_id = 1;
    OwnPtr<EventLoopSignal> signal_data;

    // Signal numbers delivered by the console control handler thread, pending dispatch on this thread.
    Sync::MutexProtected<Vector<int>> pending_signals;

    // The wake completion packet is posted to the thread's event loop to wake it.
    NonnullOwnPtr<EventLoopWake> wake_data;
};

static Sync::MutexProtected<HashMap<pid_t, NonnullOwnPtr<EventLoopProcess>>> s_processes;

static void arm_pipe_notifier(EventLoopNotifier& notifier_data);
static int notifier_type_to_network_event(NotificationType type);

EventLoopImplementationWindows::EventLoopImplementationWindows()
    : m_wake_event(ThreadData::the()->wake_data->wait_event.handle)
{
    VERIFY(m_wake_event);
}

EventLoopImplementationWindows::~EventLoopImplementationWindows()
{
}

int EventLoopImplementationWindows::exec()
{
    for (;;) {
        if (m_exit_requested)
            return m_exit_code;
        pump(PumpMode::WaitForEvents);
    }
    VERIFY_NOT_REACHED();
}

static constexpr bool debug_event_loop = false;

size_t EventLoopImplementationWindows::pump(PumpMode pump_mode)
{
    auto& event_queue = ThreadEventQueue::current();
    auto* thread_data = ThreadData::the();

    // NOTE: The number of entries to dequeue is to be optimized. Ideally we always dequeue all outstanding packets,
    // but we don't want to increase the cost of each pump unnecessarily. If more than one entry is never dequeued
    // at once, we could switch to using GetQueuedCompletionStatus which directly returns the values.
    constexpr ULONG entry_count = 32;
    OVERLAPPED_ENTRY entries[entry_count];
    ULONG entries_removed = 0;

    bool has_pending_events = event_queue.has_pending_events();
    DWORD timeout = 0;
    if (!has_pending_events && pump_mode == PumpMode::WaitForEvents)
        timeout = INFINITE;

    BOOL success = GetQueuedCompletionStatusEx(thread_data->iocp.handle, entries, entry_count, &entries_removed, timeout, FALSE);
    dbgln_if(debug_event_loop, "Event loop dequed {} events", entries_removed);

    if (success) {
        for (ULONG i = 0; i < entries_removed; i++) {
            auto& entry = entries[i];
            auto* packet = reinterpret_cast<CompletionPacket*>(entry.lpCompletionKey);

            if (packet->type == CompletionType::Wake) {
                auto* wake_data = static_cast<EventLoopWake*>(packet);
                NTSTATUS status = g_system.NtAssociateWaitCompletionPacket(wake_data->wait_packet.handle, thread_data->iocp.handle, wake_data->wait_event.handle, wake_data, NULL, 0, 0, NULL);
                VERIFY(NT_SUCCESS(status));
                continue;
            }
            if (packet->type == CompletionType::Timer) {
                auto* timer = static_cast<EventLoopTimer*>(packet);
                if (auto owner = timer->owner.strong_ref())
                    event_queue.post_event(owner, Event::Type::Timer);
                if (timer->is_periodic) {
                    NTSTATUS status = g_system.NtAssociateWaitCompletionPacket(timer->wait_packet.handle, thread_data->iocp.handle, timer->timer.handle, timer, NULL, 0, 0, NULL);
                    VERIFY(NT_SUCCESS(status));
                }
                continue;
            }
            if (packet->type == CompletionType::Notifer) {
                auto* notifier_data = static_cast<EventLoopNotifier*>(packet);

                // The probe that signaled us has completed.
                notifier_data->zero_read_pending = false;

                bool should_post_activation = true;
                bool should_arm_probe = true;

                // Only deliver an activation if the pipe actually has data or reached EOF: the probe may have
                // completed for data the notifier's owner has since read, and a spurious activation would make
                // the owner's blocking read stall the event loop.
                DWORD bytes_available = 0;
                if (PeekNamedPipe(notifier_data->notifier->handle().windows_handle(), NULL, 0, NULL, &bytes_available, NULL)) {
                    should_post_activation = bytes_available > 0;
                } else {
                    // The pipe is broken and drained; deliver so the owner observes EOF, and stop probing.
                    should_arm_probe = false;
                }

                if (should_post_activation)
                    event_queue.post_event(notifier_data->notifier, Core::Event::Type::NotifierActivation);
                if (should_arm_probe)
                    arm_pipe_notifier(*notifier_data);

                NTSTATUS status = g_system.NtAssociateWaitCompletionPacket(notifier_data->wait_packet.handle, thread_data->iocp.handle, notifier_data->wait_event.handle, notifier_data, NULL, 0, 0, NULL);
                VERIFY(NT_SUCCESS(status));
                continue;
            }
            if (packet->type == CompletionType::SocketNotifiers) {
                auto* socket_data = static_cast<EventLoopSocketNotifiers*>(packet);

                // WSAEnumNetworkEvents() reports which selected events fired and resets them (and the event object),
                // so each notifier watching this socket is activated only for its own events.
                WSANETWORKEVENTS network_events {};
                if (WSAEnumNetworkEvents(socket_data->socket, socket_data->wait_event.handle, &network_events) == 0) {
                    for (auto* notifier : socket_data->notifiers) {
                        if (network_events.lNetworkEvents & notifier_type_to_network_event(notifier->type()))
                            event_queue.post_event(notifier, Core::Event::Type::NotifierActivation);
                    }
                }

                NTSTATUS status = g_system.NtAssociateWaitCompletionPacket(socket_data->wait_packet.handle, thread_data->iocp.handle, socket_data->wait_event.handle, socket_data, NULL, 0, 0, NULL);
                VERIFY(NT_SUCCESS(status));
                continue;
            }
            if (packet->type == CompletionType::Signal) {
                auto* signal_data = static_cast<EventLoopSignal*>(packet);

                auto pending_signals = thread_data->pending_signals.with_locked([](auto& signals) { return move(signals); });
                for (auto signal_number : pending_signals) {
                    // Collect ids first: a handler may register or unregister handlers while it runs.
                    Vector<int> matching_handler_ids;
                    for (auto& [id, handler] : thread_data->signal_handlers_by_id) {
                        if (handler.signal_number == signal_number)
                            matching_handler_ids.append(id);
                    }
                    for (auto id : matching_handler_ids) {
                        if (auto it = thread_data->signal_handlers_by_id.find(id); it != thread_data->signal_handlers_by_id.end())
                            it->value.handler(signal_number);
                    }
                }

                NTSTATUS status = g_system.NtAssociateWaitCompletionPacket(signal_data->wait_packet.handle, thread_data->iocp.handle, signal_data->wait_event.handle, signal_data, NULL, 0, 0, NULL);
                VERIFY(NT_SUCCESS(status));
                continue;
            }
            if (packet->type == CompletionType::Process) {
                auto* process_data = static_cast<EventLoopProcess*>(packet);
                pid_t const process_id = process_data->pid;
                // NOTE: This may seem like the incorrect parameter, but https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_associate_completion_port
                // states that this field represents the event type indicator
                DWORD const event_type = entry.dwNumberOfBytesTransferred;
                if (reinterpret_cast<intptr_t>(entry.lpOverlapped) == process_id && (event_type == JOB_OBJECT_MSG_EXIT_PROCESS || event_type == JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS)) {
                    Optional<NonnullOwnPtr<EventLoopProcess>> owned_process = s_processes.with_locked([&](auto& processes) {
                        return processes.take(process_id);
                    });
                    if (owned_process.has_value()) {
                        // Run the handler from the event queue rather than inline: it may unregister notifiers whose
                        // completion packets were dequeued into this very batch and are not yet re-associated, which
                        // would fail the packet cancellation (and leave dangling pointers in the batch).
                        deferred_invoke([process = owned_process.release_value(), process_id] {
                            process->exit_handler(process_id);
                        });
                    }
                }
                continue;
            }
            VERIFY_NOT_REACHED();
        }
    } else {
        DWORD error = GetLastError();
        switch (error) {
        case WAIT_TIMEOUT:
            break;
        default:
            dbgln("GetQueuedCompletionStatusEx failed with unexpected error: {}", Error::from_windows_error(error));
            VERIFY_NOT_REACHED();
        }
    }

    return event_queue.process();
}

void EventLoopImplementationWindows::quit(int code)
{
    m_exit_requested = true;
    m_exit_code = code;
}

void EventLoopImplementationWindows::wake()
{
    SetEvent(m_wake_event);
}

static int notifier_type_to_network_event(NotificationType type)
{
    // NotificationType is a bitmask (e.g. HangUp | Error), so translate each set bit.
    int events = 0;
    if (has_flag(type, NotificationType::Read))
        events |= FD_READ | FD_CLOSE | FD_ACCEPT;
    if (has_flag(type, NotificationType::Write))
        events |= FD_WRITE | FD_CONNECT;
    // WSAEventSelect has no separate error event; socket errors surface as FD_CLOSE.
    if (has_flag(type, NotificationType::HangUp) || has_flag(type, NotificationType::Error))
        events |= FD_CLOSE;

    if (!events) {
        dbgln("This notification type is not implemented: {}", (int)type);
        VERIFY_NOT_REACHED();
    }

    return events;
}

// Starts the readability probe of a pipe notifier: a zero-byte overlapped read that completes, signalling the
// notifier's event, once the pipe has data to read or the write end is closed.
static void arm_pipe_notifier(EventLoopNotifier& notifier_data)
{
    HANDLE pipe = notifier_data.notifier->handle().windows_handle();
    notifier_data.zero_read_pending = false;

    DWORD bytes_read = 0;
    if (ReadFile(pipe, nullptr, 0, &bytes_read, &notifier_data.overlapped))
        return; // Data is already available; the completion has signaled the event.

    if (GetLastError() == ERROR_IO_PENDING) {
        notifier_data.zero_read_pending = true;
        return;
    }

    // The pipe is broken or otherwise unusable; signal the event so the notifier's owner gets to observe EOF.
    SetEvent(notifier_data.overlapped.hEvent);
}

// Reselects the union of all notifiers' events for a socket. WSAEventSelect() replaces any previous selection for
// the socket, so this must consider every notifier watching it.
static void update_socket_event_selection(EventLoopSocketNotifiers& socket_data)
{
    int events = 0;
    for (auto* notifier : socket_data.notifiers)
        events |= notifier_type_to_network_event(notifier->type());

    int rc = WSAEventSelect(socket_data.socket, socket_data.wait_event.handle, events);
    VERIFY(!rc);
}

void EventLoopManagerWindows::register_notifier(Notifier& notifier)
{
    auto* thread_data = ThreadData::the();

    if (notifier.handle().kind() == HandleKind::Socket) {
        auto socket = static_cast<SOCKET>(notifier.handle().socket());
        auto& socket_data = *thread_data->socket_notifiers.ensure(socket, [&] {
            auto socket_data = make<EventLoopSocketNotifiers>();
            socket_data->type = CompletionType::SocketNotifiers;
            socket_data->socket = socket;
            socket_data->wait_event.handle = CreateEvent(NULL, FALSE, FALSE, NULL);
            VERIFY(socket_data->wait_event.handle);

            NTSTATUS status = g_system.NtCreateWaitCompletionPacket(&socket_data->wait_packet.handle, GENERIC_READ | GENERIC_WRITE, NULL);
            VERIFY(NT_SUCCESS(status));
            status = g_system.NtAssociateWaitCompletionPacket(socket_data->wait_packet.handle, thread_data->iocp.handle, socket_data->wait_event.handle, socket_data.ptr(), NULL, 0, 0, NULL);
            VERIFY(NT_SUCCESS(status));

            return socket_data;
        });

        if (socket_data.notifiers.contains_slow(&notifier))
            return;
        socket_data.notifiers.append(&notifier);
        update_socket_event_selection(socket_data);
        return;
    }

    VERIFY(notifier.handle().kind() == HandleKind::Pipe);

    auto& notifiers = thread_data->notifiers;
    if (notifiers.contains(&notifier))
        return;

    // Pipes only support read notifications.
    VERIFY(notifier.type() == NotificationType::Read);

    HANDLE event = CreateEvent(NULL, FALSE, FALSE, NULL);
    VERIFY(event);

    auto notifier_data = make<EventLoopNotifier>();
    notifier_data->type = CompletionType::Notifer;
    notifier_data->notifier = &notifier;
    notifier_data->wait_event.handle = event;
    notifier_data->overlapped.hEvent = event;
    arm_pipe_notifier(*notifier_data);

    NTSTATUS status = g_system.NtCreateWaitCompletionPacket(&notifier_data->wait_packet.handle, GENERIC_READ | GENERIC_WRITE, NULL);
    VERIFY(NT_SUCCESS(status));
    status = g_system.NtAssociateWaitCompletionPacket(notifier_data->wait_packet.handle, thread_data->iocp.handle, event, notifier_data.ptr(), NULL, 0, 0, NULL);
    VERIFY(NT_SUCCESS(status));
    notifiers.set(&notifier, move(notifier_data));
}

void EventLoopManagerWindows::unregister_notifier(Notifier& notifier)
{
    auto* thread_data = ThreadData::the();
    VERIFY(thread_data);

    if (notifier.handle().kind() == HandleKind::Socket) {
        auto it = thread_data->socket_notifiers.find(static_cast<SOCKET>(notifier.handle().socket()));
        if (it == thread_data->socket_notifiers.end())
            return;
        auto& socket_data = *it->value;
        socket_data.notifiers.remove_all_matching([&](auto* it) { return it == &notifier; });

        if (!socket_data.notifiers.is_empty()) {
            update_socket_event_selection(socket_data);
            return;
        }

        // Undo the event selection; the caller may keep using the socket (WSAEventSelect puts it in non-blocking
        // mode, which is left as-is, matching what a lone WSAEventSelect registration did before).
        (void)WSAEventSelect(socket_data.socket, NULL, 0);

        NTSTATUS status = g_system.NtCancelWaitCompletionPacket(socket_data.wait_packet.handle, TRUE);
        VERIFY(NT_SUCCESS(status));
        thread_data->socket_notifiers.remove(it);
        return;
    }

    auto maybe_notifier_data = thread_data->notifiers.take(&notifier);
    if (!maybe_notifier_data.has_value())
        return;
    auto notifier_data = move(maybe_notifier_data.value());

    if (notifier_data->zero_read_pending) {
        // The pending probe writes into notifier_data->overlapped when it completes, so it must be fully cancelled
        // before notifier_data is destroyed. Notifier guarantees the fd is still open at this point.
        HANDLE pipe = notifier.handle().windows_handle();
        if (CancelIoEx(pipe, &notifier_data->overlapped) || GetLastError() != ERROR_NOT_FOUND) {
            DWORD bytes_read = 0;
            GetOverlappedResult(pipe, &notifier_data->overlapped, &bytes_read, TRUE);
        }
    }

    // We are removing the signalled packets since the caller no longer expects them
    NTSTATUS status = g_system.NtCancelWaitCompletionPacket(notifier_data->wait_packet.handle, TRUE);
    VERIFY(NT_SUCCESS(status));
    // TODO: Reuse the data structure
}

intptr_t EventLoopManagerWindows::register_timer(EventReceiver& object, int milliseconds, bool should_reload)
{
    VERIFY(milliseconds >= 0);
    auto* thread_data = ThreadData::the();
    VERIFY(thread_data);
    auto& timers = thread_data->timers;

    // FIXME: This is a temporary fix for issue #3641
    bool manual_reset = static_cast<Timer&>(object).is_single_shot();
    HANDLE timer = CreateWaitableTimer(NULL, manual_reset, NULL);
    VERIFY(timer);

    auto timer_data = make<EventLoopTimer>();
    timer_data->type = CompletionType::Timer;
    timer_data->timer.handle = timer;
    timer_data->owner = object.make_weak_ptr();
    timer_data->is_periodic = should_reload;
    VERIFY(timer_data->timer.handle);

    NTSTATUS status = g_system.NtCreateWaitCompletionPacket(&timer_data->wait_packet.handle, GENERIC_READ | GENERIC_WRITE, NULL);
    VERIFY(NT_SUCCESS(status));

    LARGE_INTEGER first_time = {};
    // Measured in 0.1μs intervals, negative means starting from now
    first_time.QuadPart = -10'000LL * milliseconds;
    BOOL succeeded = SetWaitableTimer(timer_data->timer.handle, &first_time, should_reload ? milliseconds : 0, NULL, NULL, FALSE);
    VERIFY(succeeded);

    status = g_system.NtAssociateWaitCompletionPacket(timer_data->wait_packet.handle, thread_data->iocp.handle, timer_data->timer.handle, timer_data.ptr(), NULL, 0, 0, NULL);
    VERIFY(NT_SUCCESS(status));

    auto timer_id = reinterpret_cast<intptr_t>(timer_data.ptr());
    VERIFY(!timers.get(timer_id).has_value());
    timers.set(timer_id, move(timer_data));
    return timer_id;
}

void EventLoopManagerWindows::unregister_timer(intptr_t timer_id)
{
    if (auto* thread_data = ThreadData::the()) {
        auto maybe_timer = thread_data->timers.take(timer_id);
        if (!maybe_timer.has_value())
            return;
        auto timer = move(maybe_timer.value());
        NTSTATUS status = g_system.NtCancelWaitCompletionPacket(timer->wait_packet.handle, TRUE);
        VERIFY(NT_SUCCESS(status));
    }
}

static BOOL WINAPI console_ctrl_handler(DWORD ctrl_type)
{
    int signal_number = 0;
    switch (ctrl_type) {
    case CTRL_C_EVENT:
        signal_number = SIGINT;
        break;
    case CTRL_BREAK_EVENT:
        signal_number = SIGBREAK;
        break;
    case CTRL_CLOSE_EVENT:
    case CTRL_LOGOFF_EVENT:
    case CTRL_SHUTDOWN_EVENT:
        signal_number = SIGTERM;
        break;
    default:
        return FALSE;
    }

    bool handled = false;
    signal_wake_targets().with_locked([&](auto& targets) {
        for (auto& target : targets) {
            if (target.signal_number != signal_number)
                continue;

            target.thread_data->pending_signals.with_locked([&](auto& signals) { signals.append(signal_number); });
            SetEvent(target.thread_data->signal_data->wait_event.handle);
            handled = true;
        }
    });

    return handled ? TRUE : FALSE;
}

int EventLoopManagerWindows::register_signal(int signal_number, Function<void(int)> handler)
{
    auto* thread_data = ThreadData::the();
    VERIFY(thread_data);

    if (!thread_data->signal_data) {
        auto signal_data = make<EventLoopSignal>();
        signal_data->type = CompletionType::Signal;
        signal_data->wait_event.handle = CreateEvent(NULL, FALSE, FALSE, NULL);
        VERIFY(signal_data->wait_event.handle);

        NTSTATUS status = g_system.NtCreateWaitCompletionPacket(&signal_data->wait_packet.handle, GENERIC_READ | GENERIC_WRITE, NULL);
        VERIFY(NT_SUCCESS(status));
        status = g_system.NtAssociateWaitCompletionPacket(signal_data->wait_packet.handle, thread_data->iocp.handle, signal_data->wait_event.handle, signal_data.ptr(), NULL, 0, 0, NULL);
        VERIFY(NT_SUCCESS(status));

        thread_data->signal_data = move(signal_data);
    }

    int handler_id = thread_data->next_signal_handler_id++;
    thread_data->signal_handlers_by_id.set(handler_id, { signal_number, move(handler) });

    signal_wake_targets().with_locked([&](auto& targets) {
        static bool s_console_ctrl_handler_installed = false;
        if (!s_console_ctrl_handler_installed) {
            SetConsoleCtrlHandler(console_ctrl_handler, TRUE);
            s_console_ctrl_handler_installed = true;
        }

        targets.append({ signal_number, thread_data });
    });

    return handler_id;
}

void EventLoopManagerWindows::unregister_signal(int handler_id)
{
    auto* thread_data = ThreadData::the();
    if (!thread_data)
        return;

    auto maybe_handler = thread_data->signal_handlers_by_id.take(handler_id);
    if (!maybe_handler.has_value())
        return;

    signal_wake_targets().with_locked([&](auto& targets) {
        for (size_t i = 0; i < targets.size(); ++i) {
            if (targets[i].signal_number == maybe_handler->signal_number && targets[i].thread_data == thread_data) {
                targets.remove(i);
                break;
            }
        }
    });
}

void EventLoopManagerWindows::register_process(pid_t pid, ESCAPING Function<void(pid_t)> exit_handler)
{
    auto* thread_data = ThreadData::the();
    VERIFY(thread_data);

    s_processes.with_locked([&](auto& processes) {
        if (processes.contains(pid))
            return;

        HANDLE process_handle = OpenProcess(PROCESS_ALL_ACCESS, FALSE, pid);
        VERIFY(process_handle);

        HANDLE job_object_handle = CreateJobObject(nullptr, nullptr);
        VERIFY(job_object_handle);

        BOOL succeeded = AssignProcessToJobObject(job_object_handle, process_handle);
        VERIFY(succeeded);

        auto process_data = make<EventLoopProcess>();
        process_data->type = CompletionType::Process;
        process_data->process.handle = process_handle;
        process_data->pid = pid;
        process_data->exit_handler = move(exit_handler);
        process_data->jobobject.handle = job_object_handle;

        JOBOBJECT_ASSOCIATE_COMPLETION_PORT joacp = { .CompletionKey = process_data.ptr(), .CompletionPort = thread_data->iocp.handle };
        succeeded = SetInformationJobObject(job_object_handle, JobObjectAssociateCompletionPortInformation, &joacp, sizeof(JOBOBJECT_ASSOCIATE_COMPLETION_PORT));
        VERIFY(succeeded);

        processes.set(pid, move(process_data));
    });
}

void EventLoopManagerWindows::unregister_process(pid_t pid)
{
    auto maybe_process = s_processes.with_locked([&](auto& processes) {
        return processes.take(pid);
    });
    if (!maybe_process.has_value())
        return;

    auto process_data = maybe_process.release_value();
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT joacp = { .CompletionKey = process_data, .CompletionPort = nullptr };
    BOOL succeeded = SetInformationJobObject(process_data->jobobject.handle, JobObjectAssociateCompletionPortInformation, &joacp, sizeof(JOBOBJECT_ASSOCIATE_COMPLETION_PORT));
    VERIFY(succeeded);
}

void EventLoopManagerWindows::did_post_event()
{
}

NonnullOwnPtr<EventLoopImplementation> EventLoopManagerWindows::make_implementation()
{
    return make<EventLoopImplementationWindows>();
}

}
