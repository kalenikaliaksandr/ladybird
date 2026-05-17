/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Compositor/SerializedPayload.h>

namespace Web::Compositor {

SerializedPayload::SerializedPayload(Core::AnonymousBuffer buffer, size_t payload_size)
    : m_buffer(move(buffer))
    , m_payload_size(payload_size)
{
}

ErrorOr<SerializedPayload> SerializedPayload::create(size_t payload_size)
{
    auto buffer = TRY(Core::AnonymousBuffer::create_with_size(max(payload_size, static_cast<size_t>(1))));
    return SerializedPayload { move(buffer), payload_size };
}

ErrorOr<SerializedPayload> SerializedPayload::copy_from(ReadonlyBytes bytes)
{
    auto payload = TRY(create(bytes.size()));
    bytes.copy_to(payload.bytes());
    return payload;
}

ErrorOr<SerializedPayload> SerializedPayload::adopt(Core::AnonymousBuffer buffer, size_t payload_size)
{
    if (!buffer.is_valid() && payload_size != 0)
        return Error::from_string_literal("Serialized payload has no backing buffer");
    if (payload_size > buffer.size())
        return Error::from_string_literal("Serialized payload size exceeds backing buffer size");
    return SerializedPayload { move(buffer), payload_size };
}

Bytes SerializedPayload::bytes()
{
    if (!m_buffer.is_valid())
        return {};
    return { m_buffer.data<void>(), m_payload_size };
}

ReadonlyBytes SerializedPayload::bytes() const
{
    if (!m_buffer.is_valid())
        return {};
    return { m_buffer.data<void>(), m_payload_size };
}

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedPayload const& payload)
{
    TRY(encoder.encode(payload.buffer()));
    TRY(encoder.encode_size(payload.size()));
    return {};
}

template<>
WEB_API ErrorOr<Web::Compositor::SerializedPayload> decode(Decoder& decoder)
{
    auto buffer = TRY(decoder.decode<Core::AnonymousBuffer>());
    // This payload is backed by the transferred AnonymousBuffer, so it should
    // not be constrained by the generic heap-container decode limit.
    auto payload_size = static_cast<size_t>(TRY(decoder.decode<u32>()));
    return Web::Compositor::SerializedPayload::adopt(move(buffer), payload_size);
}

}
