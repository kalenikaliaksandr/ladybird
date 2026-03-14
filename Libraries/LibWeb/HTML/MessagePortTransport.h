/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/RefCounted.h>
#include <AK/Vector.h>
#include <LibIPC/TransportSocket.h>
#include <LibWeb/HTML/StructuredSerialize.h>

namespace Web::HTML {

class MessagePortTransport {
public:
    virtual ~MessagePortTransport() = default;

    virtual void post_message(SerializedTransferRecord&&) = 0;

    enum class ReadResult : u8 {
        Continue,
        PeerClosed,
    };
    virtual ReadResult drain_incoming_messages(Function<void(SerializedTransferRecord&&)>) = 0;

    virtual void set_up_read_hook(Function<void()>) = 0;
    virtual bool is_open() const = 0;
    virtual void close() = 0;
    virtual void close_after_sending_pending() = 0;
};

class InProcessTransportState;

class InProcessTransport final : public MessagePortTransport {
    AK_MAKE_NONCOPYABLE(InProcessTransport);
    AK_MAKE_NONMOVABLE(InProcessTransport);

public:
    enum class Side : u8 {
        A = 0,
        B = 1
    };

    struct Pair {
        NonnullOwnPtr<InProcessTransport> local;
        NonnullOwnPtr<InProcessTransport> remote;
    };
    static Pair create();
    ~InProcessTransport();

    virtual void post_message(SerializedTransferRecord&&) override;
    virtual ReadResult drain_incoming_messages(Function<void(SerializedTransferRecord&&)>) override;
    virtual void set_up_read_hook(Function<void()>) override;
    virtual bool is_open() const override;
    virtual void close() override;
    virtual void close_after_sending_pending() override;

    Vector<SerializedTransferRecord> take_incoming();

private:
    InProcessTransport(NonnullRefPtr<InProcessTransportState>, Side);

    Side peer_side() const { return m_side == Side::A ? Side::B : Side::A; }

    NonnullRefPtr<InProcessTransportState> m_state;
    Side m_side;
};

class CrossProcessTransport final : public MessagePortTransport {
    AK_MAKE_NONCOPYABLE(CrossProcessTransport);
    AK_MAKE_NONMOVABLE(CrossProcessTransport);

public:
    explicit CrossProcessTransport(NonnullOwnPtr<IPC::TransportSocket>);

    virtual void post_message(SerializedTransferRecord&&) override;
    virtual ReadResult drain_incoming_messages(Function<void(SerializedTransferRecord&&)>) override;
    virtual void set_up_read_hook(Function<void()>) override;
    virtual bool is_open() const override;
    virtual void close() override;
    virtual void close_after_sending_pending() override;

    ErrorOr<IPC::TransportHandle> release_for_transfer();

private:
    NonnullOwnPtr<IPC::TransportSocket> m_socket;
};

}
