/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Platform.h>

#if !defined(AK_OS_MACOS)
#    error "TransportMachPort is only available on macOS"
#endif

#include <AK/Atomic.h>
#include <AK/Queue.h>
#include <AK/RefPtr.h>
#include <AK/Vector.h>
#include <LibCore/MachPort.h>
#include <LibCore/Notifier.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/AutoCloseFileDescriptor.h>
#include <LibIPC/TransportHandle.h>
#include <LibThreading/ConditionVariable.h>
#include <LibThreading/Thread.h>

namespace IPC {

// TransportMachPort implements IPC message transport using Mach ports on macOS.
//
// Architecture:
//
//   Main thread                    IO thread
//   ───────────                    ─────────
//   post_message() ─── m_send_mutex ───→ send via mach_msg()
//                                        receive via mach_msg() on m_port_set
//   read hook (fd) ←── notify pipe ────┘
//
// Mach ports are kernel-managed message queues identified by integer names.
// Each port has exactly one "receive right" (the reading end) and zero or more
// "send rights" (the writing ends). Unlike Unix sockets, Mach ports are simplex
// (one-directional), so a bidirectional connection needs two ports:
//
//   Process A                          Process B
//   ┌──────────────────┐               ┌──────────────────┐
//   │ m_receive_port ◄─── send right ──┤ m_send_port      │
//   │ m_send_port ───── send right ──►─┤ m_receive_port   │
//   └──────────────────┘               └──────────────────┘
//
// The IO thread blocks on a "port set" (like epoll for Mach ports) that
// multiplexes the IPC receive port and a wakeup port. The wakeup port is
// a self-pipe trick: the main thread sends a message to it to interrupt the
// IO thread's blocking mach_msg() call (e.g., to flush pending sends or stop).
//
// Peer death is detected via MACH_NOTIFY_NO_SENDERS: the kernel tracks how
// many send rights exist for each receive port and delivers a notification
// when the count drops to zero (i.e., the peer exited or closed the connection).
//
// Since Core::Notifier requires file descriptors, a pipe bridges Mach port
// events to the main thread's event loop: the IO thread writes a byte to the
// pipe when messages arrive or the peer disconnects, and a Notifier on the
// read end triggers the connection's read hook.
class TransportMachPort {
    AK_MAKE_NONCOPYABLE(TransportMachPort);
    AK_MAKE_NONMOVABLE(TransportMachPort);

public:
    struct Paired {
        NonnullOwnPtr<TransportMachPort> local;
        TransportHandle remote_handle;
    };
    static ErrorOr<Paired> create_paired();

    // Bootstrap a Mach port connection over an existing Unix socket.
    // Used when the initial connection is socket-based (e.g., WebDriver).
    // Both sides must call this concurrently on the same socket.
    static ErrorOr<NonnullOwnPtr<TransportMachPort>> from_socket(NonnullOwnPtr<Core::LocalSocket> socket);

    TransportMachPort(Core::MachPort receive_right, Core::MachPort send_right);
    ~TransportMachPort();

    void set_up_read_hook(Function<void()>);
    bool is_open() const;

    void close();
    void close_after_sending_all_pending_messages();

    void wait_until_readable();

    void post_message(Vector<u8> const&, Vector<Attachment>& attachments);

    enum class ShouldShutdown {
        No,
        Yes,
    };
    struct Message {
        Vector<u8> bytes;
        Queue<Attachment> attachments;
    };
    ShouldShutdown read_as_many_messages_as_possible_without_blocking(Function<void(Message&&)>&&);

    // Extract port rights for transfer to another process. Stops the IO
    // thread and clears notifications. The destination process reconstructs
    // a TransportMachPort from the handle via TransportHandle::create_transport().
    ErrorOr<TransportHandle> release_for_transfer();

private:
    Core::MachPort release_receive_right();
    Core::MachPort release_send_right();
    static constexpr unsigned int IPC_DATA_MESSAGE_ID = 0x4950C001;
    static constexpr unsigned int IPC_WAKEUP_MESSAGE_ID = 0x4950C003;

    struct PendingMessage {
        Vector<u8> payload;
        Vector<Attachment> attachments;
    };

    enum class IOThreadState {
        Running,
        SendPendingMessagesAndStop,
        Stopped,
    };

    void initialize();
    intptr_t io_thread_loop();
    void stop_io_thread(IOThreadState desired_state);
    void wake_io_thread();
    void send_mach_message(PendingMessage&);
    void process_received_message(u8* buffer);

    Core::MachPort m_receive_port;
    Core::MachPort m_send_port;
    Core::MachPort m_port_set;
    Core::MachPort m_wakeup_receive_port;
    Core::MachPort m_wakeup_send_port;

    Atomic<bool> m_is_open { true };

    RefPtr<Threading::Thread> m_io_thread;
    Atomic<IOThreadState> m_io_thread_state { IOThreadState::Running };
    Atomic<bool> m_peer_eof { false };

    // Send queue
    Vector<PendingMessage> m_pending_send_messages;
    Threading::Mutex m_send_mutex;
    Vector<u8> m_send_buffer;

    // Incoming messages
    Threading::Mutex m_incoming_mutex;
    Threading::ConditionVariable m_incoming_cv { m_incoming_mutex };
    Vector<NonnullOwnPtr<Message>> m_incoming_messages;

    // Hook notification (uses pipe since Core::Notifier needs fd)
    RefPtr<AutoCloseFileDescriptor> m_notify_hook_read_fd;
    RefPtr<AutoCloseFileDescriptor> m_notify_hook_write_fd;
    RefPtr<Core::Notifier> m_read_hook_notifier;
    Function<void()> m_on_read_hook;
};

}
