/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/NumericLimits.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Web::WebGL {

// Copies `data` to `destination + offset`, zeroing the alignment gap between `cursor`
// and `offset` so it cannot leak the buffer's previous contents; returns the new cursor.
static size_t copy_at(Bytes destination, size_t cursor, size_t offset, ReadonlyBytes data)
{
    __builtin_memset(destination.offset_pointer(cursor), 0, offset - cursor);
    __builtin_memcpy(destination.offset_pointer(offset), data.data(), data.size());
    return offset + data.size();
}

void WebGLCommandList::append_bytes(WebGLCommandType type, ReadonlyBytes payload, ReadonlyBytes inline_data)
{
    VERIFY(m_bytes.size() % command_alignment == 0);

    auto payload_end = payload.size();
    WebGLDataSpan data_span;
    if (!inline_data.is_empty()) {
        data_span = { first_inline_data_offset(payload.size()), static_cast<u32>(inline_data.size()) };
        payload_end = data_span.offset + data_span.size;
    }

    auto record_size = sizeof(WebGLCommandHeader) + payload_end;
    auto padded_record_size = align_up_to(record_size, command_alignment);
    auto padded_payload_size = padded_record_size - sizeof(WebGLCommandHeader);
    VERIFY(padded_payload_size <= NumericLimits<u32>::max());

    WebGLCommandHeader header {
        .type = type,
        .payload_size = static_cast<u32>(padded_payload_size),
    };

    auto record_offset = m_bytes.size();
    m_bytes.resize(record_offset + padded_record_size);
    auto record = m_bytes.bytes().slice(record_offset);
    auto cursor = copy_at(record, 0, 0, { &header, sizeof(header) });
    cursor = copy_at(record, cursor, sizeof(header), payload);
    if (!inline_data.is_empty())
        cursor = copy_at(record, cursor, sizeof(header) + data_span.offset, inline_data);
    __builtin_memset(record.offset_pointer(cursor), 0, record.size() - cursor);
}

ByteBuffer WebGLSyncCall::encode_request_bytes(WebGLSyncCallType type, ReadonlyBytes request, ReadonlyBytes inline_data)
{
    auto payload_end = request.size();
    WebGLDataSpan data_span;
    if (!inline_data.is_empty()) {
        data_span = { WebGLCommandList::first_inline_data_offset(request.size()), static_cast<u32>(inline_data.size()) };
        payload_end = data_span.offset + data_span.size;
    }
    auto padded_payload_size = align_up_to(payload_end, WebGLCommandList::command_alignment);
    VERIFY(padded_payload_size <= NumericLimits<u32>::max());

    WebGLSyncCallHeader header {
        .type = type,
        .payload_size = static_cast<u32>(padded_payload_size),
    };

    auto bytes = MUST(ByteBuffer::create_uninitialized(sizeof(header) + padded_payload_size));
    auto cursor = copy_at(bytes, 0, 0, { &header, sizeof(header) });
    cursor = copy_at(bytes, cursor, sizeof(header), request);
    if (!inline_data.is_empty())
        cursor = copy_at(bytes, cursor, sizeof(header) + data_span.offset, inline_data);
    __builtin_memset(bytes.offset_pointer(cursor), 0, bytes.size() - cursor);
    return bytes;
}

ByteBuffer WebGLSyncCall::encode_reply_bytes(ReadonlyBytes reply, ReadonlyBytes inline_data, ReadonlyBytes more_inline_data)
{
    VERIFY(more_inline_data.is_empty() || !inline_data.is_empty());

    auto reply_end = reply.size();
    WebGLDataSpan first_span;
    if (!inline_data.is_empty()) {
        first_span = { WebGLCommandList::first_inline_data_offset(reply.size()), static_cast<u32>(inline_data.size()) };
        reply_end = first_span.offset + first_span.size;
    }
    WebGLDataSpan second_span;
    if (!more_inline_data.is_empty()) {
        second_span = { WebGLCommandList::next_inline_data_offset(first_span), static_cast<u32>(more_inline_data.size()) };
        reply_end = second_span.offset + second_span.size;
    }

    auto bytes = MUST(ByteBuffer::create_uninitialized(reply_end));
    auto cursor = copy_at(bytes, 0, 0, reply);
    if (!inline_data.is_empty())
        cursor = copy_at(bytes, cursor, first_span.offset, inline_data);
    if (!more_inline_data.is_empty())
        cursor = copy_at(bytes, cursor, second_span.offset, more_inline_data);
    __builtin_memset(bytes.offset_pointer(cursor), 0, bytes.size() - cursor);
    return bytes;
}

}
