/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/NonnullOwnPtr.h>
#include <LibCore/MachPort.h>
#include <LibCore/Notifier.h>
#include <LibCore/Socket.h>
#include <LibCore/System.h>
#include <LibIPC/TransportMachPort.h>
#include <LibThreading/Thread.h>

#include <mach/mach.h>

namespace IPC {

static void set_mach_port_queue_limit(mach_port_t port)
{
    mach_port_limits_t limits { .mpl_qlimit = MACH_PORT_QLIMIT_MAX };
    mach_port_set_attributes(mach_task_self(), port, MACH_PORT_LIMITS_INFO,
        reinterpret_cast<mach_port_info_t>(&limits), MACH_PORT_LIMITS_INFO_COUNT);
}

ErrorOr<NonnullOwnPtr<TransportMachPort>> TransportMachPort::from_socket(NonnullOwnPtr<Core::LocalSocket> socket)
{
    // Bootstrap Mach port transport over a Unix socket.
    // Both sides run this same function concurrently.
    //
    // Protocol:
    // 1. Create a receive port and a send right for it
    // 2. Register the receive port with the bootstrap server using a unique name
    // 3. Send our bootstrap name over the socket
    // 4. Receive the other side's bootstrap name from the socket
    // 5. Look up the other side's port from the bootstrap server
    // 6. Construct TransportMachPort with (our receive, their send)

    auto our_recv = TRY(Core::MachPort::create_with_right(Core::MachPort::PortRight::Receive));
    auto our_send = TRY(our_recv.insert_right(Core::MachPort::MessageRight::MakeSend));

    // Create a unique name for our port
    auto our_name = ByteString::formatted("org.ladybird.ipc.{}.{}", getpid(), our_recv.port());
    TRY(our_recv.register_with_bootstrap_server(our_name));

    // Send our name length + name bytes
    auto name_bytes = our_name.bytes();
    u32 name_len = static_cast<u32>(name_bytes.size());

    TRY(socket->set_blocking(true));
    auto socket_fd = socket->fd().value();
    TRY(Core::System::write(socket_fd, { reinterpret_cast<u8 const*>(&name_len), sizeof(name_len) }));
    TRY(Core::System::write(socket_fd, name_bytes));

    // Read the other side's name
    u32 peer_name_len = 0;
    TRY(Core::System::read(socket_fd, { reinterpret_cast<u8*>(&peer_name_len), sizeof(peer_name_len) }));

    if (peer_name_len == 0 || peer_name_len > 256)
        return Error::from_string_literal("Invalid peer bootstrap name length");

    auto peer_name_buffer = TRY(ByteBuffer::create_uninitialized(peer_name_len));
    TRY(Core::System::read(socket_fd, peer_name_buffer.bytes()));

    auto peer_name = ByteString { peer_name_buffer.bytes() };

    // Look up their port
    auto their_send = TRY(Core::MachPort::look_up_from_bootstrap_server(peer_name));

    // We don't need the local send right since it's registered in the bootstrap server
    // (the other side will look it up). Just drop it.
    (void)our_send;

    return make<TransportMachPort>(move(our_recv), move(their_send));
}

ErrorOr<TransportMachPort::Paired> TransportMachPort::create_paired()
{
    auto port_a_recv = TRY(Core::MachPort::create_with_right(Core::MachPort::PortRight::Receive));
    auto port_a_send = TRY(port_a_recv.insert_right(Core::MachPort::MessageRight::MakeSend));

    auto port_b_recv = TRY(Core::MachPort::create_with_right(Core::MachPort::PortRight::Receive));
    auto port_b_send = TRY(port_b_recv.insert_right(Core::MachPort::MessageRight::MakeSend));

    // Local: receives on A, sends to B
    // Remote: receives on B, sends to A
    return Paired {
        make<TransportMachPort>(move(port_a_recv), move(port_b_send)),
        make<TransportMachPort>(move(port_b_recv), move(port_a_send)),
    };
}

TransportMachPort::TransportMachPort(Core::MachPort receive_right, Core::MachPort send_right)
    : m_receive_port(move(receive_right))
    , m_send_port(move(send_right))
{
    initialize();
}

void TransportMachPort::initialize()
{
    // Create port set for multiplexing receive port + wakeup port
    m_port_set = MUST(Core::MachPort::create_with_right(Core::MachPort::PortRight::PortSet));

    // Add receive port to set
    auto ret = mach_port_insert_member(mach_task_self(), m_receive_port.port(), m_port_set.port());
    VERIFY(ret == KERN_SUCCESS);

    // Create wakeup port pair
    m_wakeup_receive_port = MUST(Core::MachPort::create_with_right(Core::MachPort::PortRight::Receive));
    m_wakeup_send_port = MUST(m_wakeup_receive_port.insert_right(Core::MachPort::MessageRight::MakeSend));

    // Add wakeup receive port to set
    ret = mach_port_insert_member(mach_task_self(), m_wakeup_receive_port.port(), m_port_set.port());
    VERIFY(ret == KERN_SUCCESS);

    // Create pipe for notifying main thread (Core::Notifier needs fd)
    auto fds = MUST(Core::System::pipe2(O_CLOEXEC | O_NONBLOCK));
    m_notify_hook_read_fd = adopt_ref(*new AutoCloseFileDescriptor(fds[0]));
    m_notify_hook_write_fd = adopt_ref(*new AutoCloseFileDescriptor(fds[1]));

    // Increase receive port queue limit
    set_mach_port_queue_limit(m_receive_port.port());

    // Request dead-name notification for the send port. When the peer process
    // exits, its receive port is destroyed, our send right becomes a dead name,
    // and the kernel delivers a notification to m_receive_port (which is in
    // the port set the IO thread blocks on).
    mach_port_t prev = MACH_PORT_NULL;
    mach_port_request_notification(mach_task_self(),
        m_send_port.port(),
        MACH_NOTIFY_DEAD_NAME,
        0,
        m_receive_port.port(),
        MACH_MSG_TYPE_MAKE_SEND_ONCE,
        &prev);

    m_io_thread = Threading::Thread::construct("IPC IO (Mach)"sv, [this] { return io_thread_loop(); });
    m_io_thread->start();
}

TransportMachPort::~TransportMachPort()
{
    stop_io_thread(IOThreadState::Stopped);
    m_read_hook_notifier.clear();
}

void TransportMachPort::stop_io_thread(IOThreadState desired_state)
{
    VERIFY(desired_state == IOThreadState::Stopped || desired_state == IOThreadState::SendPendingMessagesAndStop);
    m_io_thread_state.store(desired_state, AK::MemoryOrder::memory_order_release);
    wake_io_thread();
    if (m_io_thread && m_io_thread->needs_to_be_joined())
        (void)m_io_thread->join();
}

void TransportMachPort::wake_io_thread()
{
    if (!MACH_PORT_VALID(m_wakeup_send_port.port()))
        return;

    mach_msg_header_t header {};
    header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    header.msgh_size = sizeof(header);
    header.msgh_remote_port = m_wakeup_send_port.port();
    header.msgh_local_port = MACH_PORT_NULL;
    header.msgh_id = IPC_WAKEUP_MESSAGE_ID;

    // Non-blocking send with short timeout
    mach_msg(&header, MACH_SEND_MSG | MACH_SEND_TIMEOUT, sizeof(header), 0, MACH_PORT_NULL, 0, MACH_PORT_NULL);
}

intptr_t TransportMachPort::io_thread_loop()
{
    static constexpr size_t RECV_BUFFER_SIZE = 65536;
    auto buffer = Vector<u8>();
    buffer.resize(RECV_BUFFER_SIZE);

    for (;;) {
        auto state = m_io_thread_state.load();
        if (state == IOThreadState::Stopped)
            break;

        // Send pending messages
        {
            Threading::MutexLocker locker(m_send_mutex);
            while (!m_pending_send_messages.is_empty()) {
                auto msg = m_pending_send_messages.take_first();
                locker.unlock();
                send_mach_message(msg);
                locker.lock();
            }

            if (m_io_thread_state.load() == IOThreadState::SendPendingMessagesAndStop) {
                m_io_thread_state = IOThreadState::Stopped;
                break;
            }
        }

        // Receive a message from the port set (blocking)
        auto* header = reinterpret_cast<mach_msg_header_t*>(buffer.data());

        auto const ret = mach_msg(header, MACH_RCV_MSG | MACH_RCV_LARGE, 0, buffer.size(),
            m_port_set.port(), MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL);

        if (ret == MACH_RCV_TOO_LARGE) {
            // Message was too large for our buffer, retry with correct size
            auto needed_size = header->msgh_size + sizeof(mach_msg_trailer_t);
            buffer.resize(needed_size);
            header = reinterpret_cast<mach_msg_header_t*>(buffer.data());
            auto const retry_ret = mach_msg(header, MACH_RCV_MSG, 0, needed_size,
                m_port_set.port(), MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL);
            if (retry_ret != KERN_SUCCESS) {
                dbgln("TransportMachPort: mach_msg retry failed: {}", mach_error_string(retry_ret));
                m_io_thread_state = IOThreadState::Stopped;
                break;
            }
        } else if (ret != KERN_SUCCESS) {
            dbgln("TransportMachPort: mach_msg receive failed: {}", mach_error_string(ret));
            m_io_thread_state = IOThreadState::Stopped;
            break;
        }

        if (header->msgh_local_port == m_wakeup_receive_port.port()) {
            // Wakeup message, loop back to check send queue
            continue;
        }

        if (header->msgh_local_port == m_receive_port.port()) {
            if (header->msgh_id == IPC_DATA_MESSAGE_ID) {
                process_received_message(buffer.data());
            } else if (header->msgh_id == MACH_NOTIFY_DEAD_NAME) {
                // Peer's receive port was destroyed (peer process exited).
                // Deallocate the dead name carried in the notification.
                auto* notif = reinterpret_cast<mach_dead_name_notification_t*>(buffer.data());
                mach_port_deallocate(mach_task_self(), notif->not_port);
                m_io_thread_state = IOThreadState::Stopped;
                break;
            } else {
                dbgln("TransportMachPort: Unknown message id {}", header->msgh_id);
            }
        }
    }

    VERIFY(m_io_thread_state == IOThreadState::Stopped);
    {
        Threading::MutexLocker locker(m_incoming_mutex);
        m_peer_eof = true;
    }
    m_incoming_cv.broadcast();

    // Notify main thread of shutdown
    if (m_notify_hook_write_fd) {
        Array<u8, 1> bytes = { 0 };
        (void)Core::System::write(m_notify_hook_write_fd->value(), bytes);
    }

    return 0;
}

void TransportMachPort::send_mach_message(PendingMessage& msg)
{
    auto const& payload = msg.payload;
    auto& attachments = msg.attachments;
    size_t port_count = attachments.size();
    size_t total_desc_count = port_count + 1; // +1 for OOL payload

    // Calculate message size
    size_t msg_size = sizeof(mach_msg_header_t)
        + sizeof(mach_msg_body_t)
        + (port_count * sizeof(mach_msg_port_descriptor_t))
        + sizeof(mach_msg_ool_descriptor_t);

    auto buffer = Vector<u8>();
    buffer.resize(msg_size);
    memset(buffer.data(), 0, msg_size);

    auto* header = reinterpret_cast<mach_msg_header_t*>(buffer.data());
    header->msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
    header->msgh_size = msg_size;
    header->msgh_remote_port = m_send_port.port();
    header->msgh_local_port = MACH_PORT_NULL;
    header->msgh_id = IPC_DATA_MESSAGE_ID;

    auto* body = reinterpret_cast<mach_msg_body_t*>(header + 1);
    body->msgh_descriptor_count = total_desc_count;

    auto* desc_ptr = reinterpret_cast<mach_msg_port_descriptor_t*>(body + 1);

    // All attachments are already mach ports (FDs were converted to fileports in Attachment::from_fd)
    for (size_t i = 0; i < port_count; ++i) {
        auto disposition = static_cast<mach_msg_type_name_t>(attachments[i].message_right());
        auto port = attachments[i].release_mach_port();
        desc_ptr[i].name = port.release();
        desc_ptr[i].disposition = disposition;
        desc_ptr[i].type = MACH_MSG_PORT_DESCRIPTOR;
    }

    // Add OOL descriptor for payload
    auto* ool_desc = reinterpret_cast<mach_msg_ool_descriptor_t*>(&desc_ptr[port_count]);
    ool_desc->address = const_cast<void*>(static_cast<void const*>(payload.data()));
    ool_desc->size = payload.size();
    ool_desc->deallocate = false;
    ool_desc->copy = MACH_MSG_VIRTUAL_COPY;
    ool_desc->type = MACH_MSG_OOL_DESCRIPTOR;

    auto const ret = mach_msg(header, MACH_SEND_MSG | MACH_SEND_TIMEOUT, msg_size, 0,
        MACH_PORT_NULL, 5000 /* 5 sec timeout */, MACH_PORT_NULL);
    if (ret != KERN_SUCCESS) {
        dbgln("TransportMachPort: send failed: {} (send_port={:x})", mach_error_string(ret), m_send_port.port());
        m_peer_eof = true;
    }
}

void TransportMachPort::process_received_message(u8* buffer)
{
    auto* header = reinterpret_cast<mach_msg_header_t*>(buffer);
    auto message = make<Message>();

    if (!(header->msgh_bits & MACH_MSGH_BITS_COMPLEX)) {
        // Non-complex message shouldn't happen for IPC data
        dbgln("TransportMachPort: received non-complex IPC data message");
        return;
    }

    auto* body = reinterpret_cast<mach_msg_body_t*>(header + 1);
    auto desc_count = body->msgh_descriptor_count;

    if (desc_count < 1) {
        dbgln("TransportMachPort: received complex message with no descriptors");
        return;
    }

    // Walk through descriptors, converting port descriptors directly to Attachments.
    auto* current = reinterpret_cast<u8*>(body + 1);
    mach_msg_ool_descriptor_t const* ool_desc = nullptr;

    for (unsigned int i = 0; i < desc_count; ++i) {
        auto* desc_type = reinterpret_cast<mach_msg_type_descriptor_t*>(current);
        switch (desc_type->type) {
        case MACH_MSG_PORT_DESCRIPTOR: {
            auto* port_desc = reinterpret_cast<mach_msg_port_descriptor_t*>(current);
            Core::MachPort::PortRight right;
            Core::MachPort::MessageRight msg_right;
            switch (port_desc->disposition) {
            case MACH_MSG_TYPE_MOVE_SEND:
                right = Core::MachPort::PortRight::Send;
                msg_right = Core::MachPort::MessageRight::MoveSend;
                break;
            case MACH_MSG_TYPE_MOVE_RECEIVE:
                right = Core::MachPort::PortRight::Receive;
                msg_right = Core::MachPort::MessageRight::MoveReceive;
                break;
            case MACH_MSG_TYPE_MOVE_SEND_ONCE:
                right = Core::MachPort::PortRight::SendOnce;
                msg_right = Core::MachPort::MessageRight::MoveSendOnce;
                break;
            default:
                right = Core::MachPort::PortRight::Send;
                msg_right = Core::MachPort::MessageRight::MoveSend;
                break;
            }
            message->attachments.enqueue(
                Attachment::from_mach_port(Core::MachPort::adopt_right(port_desc->name, right), msg_right));
            current += sizeof(mach_msg_port_descriptor_t);
            break;
        }
        case MACH_MSG_OOL_DESCRIPTOR: {
            ool_desc = reinterpret_cast<mach_msg_ool_descriptor_t const*>(current);
            current += sizeof(mach_msg_ool_descriptor_t);
            break;
        }
        default:
            dbgln("TransportMachPort: unknown descriptor type {}", desc_type->type);
            current += sizeof(mach_msg_descriptor_t);
            break;
        }
    }

    // Extract payload from OOL data
    if (ool_desc && ool_desc->size > 0 && ool_desc->address != nullptr) {
        message->bytes.append(static_cast<u8 const*>(ool_desc->address), ool_desc->size);
        // Deallocate OOL memory allocated by kernel
        vm_deallocate(mach_task_self(), reinterpret_cast<vm_address_t>(ool_desc->address), ool_desc->size);
    }

    if (!message->bytes.is_empty() || !message->attachments.is_empty()) {
        Threading::MutexLocker locker(m_incoming_mutex);
        m_incoming_messages.append(move(message));
        m_incoming_cv.broadcast();

        // Notify main thread
        if (m_notify_hook_write_fd) {
            Array<u8, 1> bytes = { 0 };
            (void)Core::System::write(m_notify_hook_write_fd->value(), bytes);
        }
    }
}

void TransportMachPort::set_up_read_hook(Function<void()> hook)
{
    m_on_read_hook = move(hook);
    m_read_hook_notifier = Core::Notifier::construct(m_notify_hook_read_fd->value(), Core::NotificationType::Read);
    m_read_hook_notifier->on_activation = [this] {
        VERIFY(m_notify_hook_read_fd);
        char buf[64];
        (void)Core::System::read(m_notify_hook_read_fd->value(), { buf, sizeof(buf) });
        if (m_on_read_hook)
            m_on_read_hook();
    };

    {
        Threading::MutexLocker locker(m_incoming_mutex);
        if (!m_incoming_messages.is_empty()) {
            Array<u8, 1> bytes = { 0 };
            MUST(Core::System::write(m_notify_hook_write_fd->value(), bytes));
        }
    }
}

bool TransportMachPort::is_open() const
{
    return m_is_open && !m_peer_eof;
}

void TransportMachPort::close()
{
    m_is_open = false;
    stop_io_thread(IOThreadState::Stopped);
}

void TransportMachPort::close_after_sending_all_pending_messages()
{
    stop_io_thread(IOThreadState::SendPendingMessagesAndStop);
    m_is_open = false;
}

void TransportMachPort::wait_until_readable()
{
    Threading::MutexLocker lock(m_incoming_mutex);
    while (m_incoming_messages.is_empty() && !m_peer_eof) {
        m_incoming_cv.wait();
    }
}

void TransportMachPort::post_message(Vector<u8> const& bytes, Vector<Attachment>& attachments)
{
    PendingMessage pending;
    pending.payload = bytes;
    pending.attachments = move(attachments);

    {
        Threading::MutexLocker locker(m_send_mutex);
        m_pending_send_messages.append(move(pending));
    }
    wake_io_thread();
}

TransportMachPort::ShouldShutdown TransportMachPort::read_as_many_messages_as_possible_without_blocking(Function<void(Message&&)>&& callback)
{
    Vector<NonnullOwnPtr<Message>> messages;
    {
        Threading::MutexLocker locker(m_incoming_mutex);
        messages = move(m_incoming_messages);
    }
    for (auto& message : messages)
        callback(move(*message));
    return m_peer_eof ? ShouldShutdown::Yes : ShouldShutdown::No;
}

Core::MachPort TransportMachPort::release_receive_right()
{
    stop_io_thread(IOThreadState::Stopped);
    // Remove from port set before releasing
    if (MACH_PORT_VALID(m_receive_port.port()) && MACH_PORT_VALID(m_port_set.port()))
        mach_port_extract_member(mach_task_self(), m_receive_port.port(), m_port_set.port());
    return move(m_receive_port);
}

Core::MachPort TransportMachPort::release_send_right()
{
    stop_io_thread(IOThreadState::Stopped);
    return move(m_send_port);
}

}
