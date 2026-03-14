/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Message.h>
#include <LibWeb/HTML/MessagePortTransport.h>

namespace Web::HTML {

class InProcessTransportState : public RefCounted<InProcessTransportState> {
public:
    struct Side {
        Vector<SerializedTransferRecord> incoming;
        Function<void()> read_hook;
        bool closed { false };
    };

    Side& side(InProcessTransport::Side s)
    {
        return m_sides[to_underlying(s)];
    }

    Side const& side(InProcessTransport::Side s) const
    {
        return m_sides[to_underlying(s)];
    }

private:
    Array<Side, 2> m_sides;
};

InProcessTransport::Pair InProcessTransport::create()
{
    auto state = adopt_ref(*new InProcessTransportState);
    return {
        adopt_own(*new InProcessTransport(state, Side::A)),
        adopt_own(*new InProcessTransport(state, Side::B)),
    };
}

InProcessTransport::~InProcessTransport()
{
    // The shared state can outlive this transport (the peer still holds a ref).
    // Clear our hook so it doesn't retain a dangling pointer to the destroyed port.
    m_state->side(m_side).read_hook = nullptr;
}

InProcessTransport::InProcessTransport(NonnullRefPtr<InProcessTransportState> state, Side side)
    : m_state(move(state))
    , m_side(side)
{
}

void InProcessTransport::post_message(SerializedTransferRecord&& record)
{
    auto& peer = m_state->side(peer_side());
    if (peer.closed)
        return;
    peer.incoming.append(move(record));
    if (peer.read_hook)
        peer.read_hook();
}

MessagePortTransport::ReadResult InProcessTransport::drain_incoming_messages(Function<void(SerializedTransferRecord&&)> callback)
{
    auto& mine = m_state->side(m_side);
    auto messages = move(mine.incoming);
    for (auto& message : messages)
        callback(move(message));
    return m_state->side(peer_side()).closed ? ReadResult::PeerClosed : ReadResult::Continue;
}

void InProcessTransport::set_up_read_hook(Function<void()> hook)
{
    auto& mine = m_state->side(m_side);
    mine.read_hook = move(hook);
    if (!mine.incoming.is_empty() && mine.read_hook)
        mine.read_hook();
}

bool InProcessTransport::is_open() const
{
    return !m_state->side(peer_side()).closed;
}

void InProcessTransport::close()
{
    m_state->side(m_side).closed = true;
    // Wake the peer so it can observe PeerClosed (mirrors socket EOF behavior).
    auto& peer = m_state->side(peer_side());
    if (peer.read_hook)
        peer.read_hook();
}

void InProcessTransport::close_after_sending_pending()
{
    close();
}

Vector<SerializedTransferRecord> InProcessTransport::take_incoming()
{
    return move(m_state->side(m_side).incoming);
}

CrossProcessTransport::CrossProcessTransport(NonnullOwnPtr<IPC::TransportSocket> socket)
    : m_socket(move(socket))
{
}

void CrossProcessTransport::post_message(SerializedTransferRecord&& record)
{
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder(buffer);
    MUST(encoder.encode(record));
    MUST(buffer.transfer_message(*m_socket));
}

MessagePortTransport::ReadResult CrossProcessTransport::drain_incoming_messages(Function<void(SerializedTransferRecord&&)> callback)
{
    auto should_shutdown = m_socket->read_as_many_messages_as_possible_without_blocking([&](IPC::TransportSocket::Message&& raw) {
        FixedMemoryStream stream { raw.bytes.span(), FixedMemoryStream::Mode::ReadOnly };
        IPC::Decoder decoder { stream, raw.attachments };
        auto record = MUST(decoder.decode<SerializedTransferRecord>());
        callback(move(record));
    });
    return should_shutdown == IPC::TransportSocket::ShouldShutdown::Yes
        ? ReadResult::PeerClosed
        : ReadResult::Continue;
}

void CrossProcessTransport::set_up_read_hook(Function<void()> hook)
{
    m_socket->set_up_read_hook(move(hook));
}

bool CrossProcessTransport::is_open() const
{
    return m_socket->is_open();
}

void CrossProcessTransport::close()
{
    m_socket->close();
}

void CrossProcessTransport::close_after_sending_pending()
{
    m_socket->close_after_sending_all_pending_messages();
}

ErrorOr<IPC::TransportHandle> CrossProcessTransport::release_for_transfer()
{
    return m_socket->release_for_transfer();
}

}
