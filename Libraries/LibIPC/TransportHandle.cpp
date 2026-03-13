/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/Socket.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Transport.h>
#include <LibIPC/TransportHandle.h>
#include <LibIPC/TransportSocket.h>

namespace IPC {

TransportHandle::TransportHandle(File file)
    : m_kind(Kind::File)
    , m_file(move(file))
{
}

ErrorOr<NonnullOwnPtr<TransportSocket>> TransportHandle::create_socket_transport() const
{
    VERIFY(m_kind == Kind::File);
    auto socket = TRY(Core::LocalSocket::adopt_fd(m_file.take_fd()));
    TRY(socket->set_blocking(false));
    return make<TransportSocket>(move(socket));
}

#if defined(AK_OS_MACOS)

TransportHandle::TransportHandle(Core::MachPort receive_right, Core::MachPort send_right)
    : m_kind(Kind::MachPorts)
    , m_receive_right(move(receive_right))
    , m_send_right(move(send_right))
{
}

ErrorOr<NonnullOwnPtr<Transport>> TransportHandle::create_transport() const
{
    VERIFY(m_kind == Kind::MachPorts);
    return make<Transport>(move(m_receive_right), move(m_send_right));
}

template<>
ErrorOr<void> encode(Encoder& encoder, TransportHandle const& handle)
{
    TRY(encoder.encode(static_cast<u8>(handle.m_kind)));
    if (handle.m_kind == TransportHandle::Kind::MachPorts) {
        TRY(encoder.append_attachment(Attachment::from_mach_port(
            Core::MachPort::adopt_right(handle.m_receive_right.release(), Core::MachPort::PortRight::Receive),
            Core::MachPort::MessageRight::MoveReceive)));
        TRY(encoder.append_attachment(Attachment::from_mach_port(
            Core::MachPort::adopt_right(handle.m_send_right.release(), Core::MachPort::PortRight::Send),
            Core::MachPort::MessageRight::MoveSend)));
    } else {
        TRY(encoder.encode(handle.m_file));
    }
    return {};
}

template<>
ErrorOr<TransportHandle> decode(Decoder& decoder)
{
    auto kind = static_cast<TransportHandle::Kind>(TRY(decoder.decode<u8>()));
    if (kind == TransportHandle::Kind::MachPorts) {
        auto& attachments = decoder.attachments();
        if (attachments.size() < 2)
            return Error::from_string_literal("Not enough attachments for TransportHandle decode");
        auto recv_attachment = attachments.dequeue();
        auto send_attachment = attachments.dequeue();
        auto receive_right = recv_attachment.release_mach_port();
        auto send_right = send_attachment.release_mach_port();
        return TransportHandle { move(receive_right), move(send_right) };
    }
    auto file = TRY(decoder.decode<File>());
    return TransportHandle { move(file) };
}

#else // !AK_OS_MACOS

ErrorOr<NonnullOwnPtr<Transport>> TransportHandle::create_transport() const
{
    return create_socket_transport();
}

template<>
ErrorOr<void> encode(Encoder& encoder, TransportHandle const& handle)
{
    TRY(encoder.encode(static_cast<u8>(handle.m_kind)));
    return encoder.encode(handle.m_file);
}

template<>
ErrorOr<TransportHandle> decode(Decoder& decoder)
{
    [[maybe_unused]] auto kind = TRY(decoder.decode<u8>());
    auto file = TRY(decoder.decode<File>());
    return TransportHandle { move(file) };
}

#endif

}
