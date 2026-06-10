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

}
