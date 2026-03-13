/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Transport.h>
#include <LibIPC/TransportHandle.h>

#if !defined(AK_OS_MACOS)
#    include <LibCore/Socket.h>
#    include <LibIPC/File.h>
#endif

namespace IPC {

#if defined(AK_OS_MACOS)

TransportHandle::TransportHandle(Core::MachPort receive_right, Core::MachPort send_right)
    : m_receive_right(move(receive_right))
    , m_send_right(move(send_right))
{
}

ErrorOr<TransportHandle> TransportHandle::from_transport(Transport& transport)
{
    auto receive_right = transport.release_receive_right();
    auto send_right = transport.release_send_right();
    return TransportHandle { move(receive_right), move(send_right) };
}

ErrorOr<NonnullOwnPtr<Transport>> TransportHandle::create_transport() const
{
    return make<Transport>(move(m_receive_right), move(m_send_right));
}

template<>
ErrorOr<void> encode(Encoder& encoder, TransportHandle const& handle)
{
    TRY(encoder.append_attachment(Attachment::from_mach_port(
        Core::MachPort::adopt_right(handle.m_receive_right.release(), Core::MachPort::PortRight::Receive),
        Core::MachPort::MessageRight::MoveReceive)));
    TRY(encoder.append_attachment(Attachment::from_mach_port(
        Core::MachPort::adopt_right(handle.m_send_right.release(), Core::MachPort::PortRight::Send),
        Core::MachPort::MessageRight::MoveSend)));
    return {};
}

template<>
ErrorOr<TransportHandle> decode(Decoder& decoder)
{
    auto& attachments = decoder.attachments();
    if (attachments.size() < 2)
        return Error::from_string_literal("Not enough attachments for TransportHandle decode");

    auto recv_attachment = attachments.dequeue();
    auto send_attachment = attachments.dequeue();

    auto receive_right = recv_attachment.release_mach_port();
    auto send_right = send_attachment.release_mach_port();

    return TransportHandle { move(receive_right), move(send_right) };
}

#else // !AK_OS_MACOS

TransportHandle::TransportHandle(File file)
    : m_file(move(file))
{
}

ErrorOr<TransportHandle> TransportHandle::from_transport(Transport& transport)
{
    auto fd = TRY(transport.release_underlying_transport_for_transfer());
    return TransportHandle { File::adopt_fd(fd) };
}

ErrorOr<NonnullOwnPtr<Transport>> TransportHandle::create_transport() const
{
    auto socket = TRY(Core::LocalSocket::adopt_fd(m_file.take_fd()));
    TRY(socket->set_blocking(true));
    return make<Transport>(move(socket));
}

template<>
ErrorOr<void> encode(Encoder& encoder, TransportHandle const& handle)
{
    return encoder.encode(handle.m_file);
}

template<>
ErrorOr<TransportHandle> decode(Decoder& decoder)
{
    auto file = TRY(decoder.decode<File>());
    return TransportHandle { move(file) };
}

#endif

}
