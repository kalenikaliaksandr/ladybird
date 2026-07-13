/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/Checked.h>
#include <AK/NumericLimits.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Web::WebGL {

ReadonlyBytes WebGLCommandList::command_bytes(Core::AnonymousBuffer const& buffer, size_t size)
{
    VERIFY(buffer.is_valid());
    VERIFY(buffer.size() >= buffer_header_size);
    VERIFY(size <= buffer.size() - buffer_header_size);
    return { buffer.data<u8>() + buffer_header_size, size };
}

ReadonlyBytes WebGLCommandList::bytes() const
{
    if (!m_buffer.is_valid())
        return {};
    return command_bytes(m_buffer, m_size);
}

Bytes WebGLCommandList::writable_bytes()
{
    VERIFY(m_buffer.is_valid());
    VERIFY(m_buffer.size() >= buffer_header_size);
    return { m_buffer.data<u8>() + buffer_header_size, m_buffer.size() - buffer_header_size };
}

WebGLCommandBufferState WebGLCommandList::command_buffer_state(Core::AnonymousBuffer const& buffer)
{
    VERIFY(buffer.is_valid());
    VERIFY(buffer.size() >= buffer_header_size);
    auto* header = const_cast<WebGLCommandBufferHeader*>(buffer.data<WebGLCommandBufferHeader>());
    VERIFY(AK::atomic_is_lock_free(&header->state));
    return static_cast<WebGLCommandBufferState>(AK::atomic_load(&header->state, AK::MemoryOrder::memory_order_acquire));
}

void WebGLCommandList::set_command_buffer_state(Core::AnonymousBuffer& buffer, WebGLCommandBufferState state)
{
    VERIFY(buffer.is_valid());
    VERIFY(buffer.size() >= buffer_header_size);
    auto* header = buffer.data<WebGLCommandBufferHeader>();
    VERIFY(AK::atomic_is_lock_free(&header->state));
    AK::atomic_store(&header->state, to_underlying(state), AK::MemoryOrder::memory_order_release);
}

bool WebGLCommandList::is_available() const
{
    return !m_buffer.is_valid() || command_buffer_state(m_buffer) == WebGLCommandBufferState::Free;
}

void WebGLCommandList::mark_submitted()
{
    VERIFY(!is_empty());
    VERIFY(is_available());
    set_command_buffer_state(m_buffer, WebGLCommandBufferState::Submitted);
}

void WebGLCommandList::reset_after_context_loss()
{
    m_size = 0;
    if (!m_buffer.is_valid())
        return;
    set_command_buffer_state(m_buffer, WebGLCommandBufferState::Free);
    m_needs_registration = true;
}

Core::AnonymousBuffer WebGLCommandList::take_registration_buffer()
{
    if (!m_needs_registration)
        return {};
    m_needs_registration = false;
    return m_buffer;
}

void WebGLCommandList::ensure_capacity(size_t required_capacity)
{
    auto current_capacity = m_buffer.is_valid() ? m_buffer.size() - buffer_header_size : 0;
    if (current_capacity >= required_capacity)
        return;

    Checked<size_t> new_capacity = current_capacity;
    if (current_capacity == 0)
        new_capacity = initial_buffer_capacity;
    else
        new_capacity *= 2;
    if (new_capacity.has_overflow() || new_capacity.value() < required_capacity)
        new_capacity = required_capacity;
    new_capacity += 4 * KiB - 1;
    VERIFY(!new_capacity.has_overflow());
    new_capacity = (new_capacity.value() / (4 * KiB)) * (4 * KiB);

    Checked<size_t> allocation_size = new_capacity;
    allocation_size += buffer_header_size;
    VERIFY(!allocation_size.has_overflow());
    VERIFY(allocation_size.value() <= NumericLimits<u32>::max());

    auto new_buffer = MUST(Core::AnonymousBuffer::create_with_size(allocation_size.value()));
    __builtin_memset(new_buffer.data<u8>(), 0, buffer_header_size);
    set_command_buffer_state(new_buffer, WebGLCommandBufferState::Free);
    if (m_size > 0)
        bytes().copy_to({ new_buffer.data<u8>() + buffer_header_size, m_size });
    m_buffer = move(new_buffer);
    m_needs_registration = true;
}

static size_t payload_layout_size(ReadonlyBytes payload, ReadonlyBytes inline_data, ReadonlyBytes more_inline_data = {})
{
    auto size = payload.size();
    if (!inline_data.is_empty())
        size = align_up_to(size, WebGLCommandList::command_alignment) + inline_data.size();
    if (!more_inline_data.is_empty())
        size = align_up_to(size, WebGLCommandList::command_alignment) + more_inline_data.size();
    return size;
}

static void write_payload(Bytes destination, ReadonlyBytes payload, ReadonlyBytes inline_data, ReadonlyBytes more_inline_data = {})
{
    __builtin_memcpy(destination.data(), payload.data(), payload.size());
    auto cursor = payload.size();
    for (auto blob : { inline_data, more_inline_data }) {
        if (blob.is_empty())
            continue;
        auto offset = align_up_to(cursor, WebGLCommandList::command_alignment);
        __builtin_memset(destination.offset_pointer(cursor), 0, offset - cursor);
        __builtin_memcpy(destination.offset_pointer(offset), blob.data(), blob.size());
        cursor = offset + blob.size();
    }
    __builtin_memset(destination.offset_pointer(cursor), 0, destination.size() - cursor);
}

void WebGLCommandList::append_bytes(WebGLCommandType type, ReadonlyBytes payload, ReadonlyBytes inline_data)
{
    auto destination = append_with_uninitialized_inline_data_bytes(type, payload, inline_data.size());
    inline_data.copy_to(destination);
}

Bytes WebGLCommandList::append_with_uninitialized_inline_data_bytes(WebGLCommandType type, ReadonlyBytes payload, size_t inline_data_size)
{
    VERIFY(m_size % command_alignment == 0);

    auto payload_size = payload.size();
    if (inline_data_size > 0)
        payload_size = align_up_to(payload_size, command_alignment) + inline_data_size;
    auto record_size = sizeof(WebGLCommandHeader) + payload_size;
    auto padded_record_size = align_up_to(record_size, command_alignment);
    auto padded_payload_size = padded_record_size - sizeof(WebGLCommandHeader);
    VERIFY(padded_payload_size <= NumericLimits<u32>::max());

    WebGLCommandHeader header {
        .type = type,
        .payload_size = static_cast<u32>(padded_payload_size),
    };

    Checked<size_t> required_capacity = m_size;
    required_capacity += padded_record_size;
    VERIFY(!required_capacity.has_overflow());
    ensure_capacity(required_capacity.value());

    auto record_offset = m_size;
    m_size = required_capacity.value();
    auto record = writable_bytes().slice(record_offset, padded_record_size);
    __builtin_memcpy(record.data(), &header, sizeof(header));
    auto destination = record.slice(sizeof(header));
    __builtin_memcpy(destination.data(), payload.data(), payload.size());

    if (inline_data_size == 0) {
        __builtin_memset(destination.offset_pointer(payload.size()), 0, destination.size() - payload.size());
        return {};
    }

    auto inline_data_offset = align_up_to(payload.size(), command_alignment);
    __builtin_memset(destination.offset_pointer(payload.size()), 0, inline_data_offset - payload.size());
    __builtin_memset(destination.offset_pointer(inline_data_offset + inline_data_size), 0, destination.size() - inline_data_offset - inline_data_size);
    return destination.slice(inline_data_offset, inline_data_size);
}

ByteBuffer WebGLSyncCall::encode_request_bytes(WebGLSyncCallType type, ReadonlyBytes request, ReadonlyBytes inline_data)
{
    auto padded_payload_size = align_up_to(payload_layout_size(request, inline_data), WebGLCommandList::command_alignment);
    VERIFY(padded_payload_size <= NumericLimits<u32>::max());

    WebGLSyncCallHeader header {
        .type = type,
        .payload_size = static_cast<u32>(padded_payload_size),
    };

    auto bytes = MUST(ByteBuffer::create_uninitialized(sizeof(header) + padded_payload_size));
    __builtin_memcpy(bytes.data(), &header, sizeof(header));
    write_payload(bytes.bytes().slice(sizeof(header)), request, inline_data);
    return bytes;
}

ByteBuffer WebGLSyncCall::encode_reply_bytes(ReadonlyBytes reply, ReadonlyBytes inline_data, ReadonlyBytes more_inline_data)
{
    VERIFY(more_inline_data.is_empty() || !inline_data.is_empty());

    auto reply_size = align_up_to(payload_layout_size(reply, inline_data, more_inline_data), WebGLCommandList::command_alignment);
    auto bytes = MUST(ByteBuffer::create_uninitialized(reply_size));
    write_payload(bytes, reply, inline_data, more_inline_data);
    return bytes;
}

}
