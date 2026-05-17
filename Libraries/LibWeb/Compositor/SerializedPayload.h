/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Span.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Export.h>

namespace Web::Compositor {

class WEB_API SerializedPayload {
public:
    static ErrorOr<SerializedPayload> create(size_t);
    static ErrorOr<SerializedPayload> copy_from(ReadonlyBytes);
    static ErrorOr<SerializedPayload> adopt(Core::AnonymousBuffer, size_t payload_size);

    size_t size() const { return m_payload_size; }
    bool is_empty() const { return m_payload_size == 0; }

    Core::AnonymousBuffer const& buffer() const { return m_buffer; }
    Core::AnonymousBuffer& buffer() { return m_buffer; }

    Bytes bytes();
    ReadonlyBytes bytes() const;

private:
    SerializedPayload(Core::AnonymousBuffer, size_t payload_size);

    Core::AnonymousBuffer m_buffer;
    size_t m_payload_size { 0 };
};

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SerializedPayload const&);

template<>
WEB_API ErrorOr<Web::Compositor::SerializedPayload> decode(Decoder&);

}
