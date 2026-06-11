/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringBuilder.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>
#include <LibWeb/WebGL/WebGLContextProxyBase.h>

namespace Web::WebGL {

WebGLContextProxyBase::WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport> transport, Painting::CanvasContextId canvas_context_id, WebGLVersion webgl_version, Vector<String> supported_extensions)
    : m_transport(move(transport))
    , m_canvas_context_id(canvas_context_id)
    , m_webgl_version(webgl_version)
    , m_supported_extensions(move(supported_extensions))
{
}

WebGLContextProxyBase::~WebGLContextProxyBase()
{
    m_transport->destroy_context(m_canvas_context_id);
}

Painting::CanvasContextId WebGLContextProxyBase::allocate_canvas_context_id()
{
    // Shares the process-wide atomic id allocator with the other resource ids, so it stays
    // race-free if WebGL contexts are ever created off the main thread (worker OffscreenCanvas).
    return Painting::allocate_display_list_resource_id<Painting::CanvasContextId>();
}

void WebGLContextProxyBase::restore(NonnullRefPtr<RemoteWebGLTransport> transport, Vector<String> supported_extensions)
{
    m_transport = move(transport);
    m_supported_extensions = move(supported_extensions);
    m_lost = false;
    m_commands.clear_with_capacity();
    m_string_cache.clear();
}

void WebGLContextProxyBase::flush_commands()
{
    if (m_commands.is_empty())
        return;
    m_transport->send_commands(m_canvas_context_id, m_commands.buffer());
    m_commands.clear_with_capacity();
}

ByteBuffer WebGLContextProxyBase::send_sync_call(ByteBuffer request)
{
    if (m_lost)
        return {};
    flush_commands();
    return m_transport->sync_call(m_canvas_context_id, move(request));
}

void WebGLContextProxyBase::set_size(Gfx::IntSize const& size)
{
    record(Commands::SetDrawingBufferSize { .width = size.width(), .height = size.height() });
}

void WebGLContextProxyBase::prepare_canvas_surface_for_compositing(Compositor::CompositorContextId target_context_id, Painting::CanvasId canvas_id, bool preserve_drawing_buffer)
{
    flush_commands();
    m_transport->prepare_canvas_surface(m_canvas_context_id, target_context_id, canvas_id, preserve_drawing_buffer);
}

RefPtr<Gfx::Bitmap> WebGLContextProxyBase::read_back_drawing_buffer(Gfx::IntRect const& rect)
{
    if (m_lost)
        return nullptr;
    flush_commands();
    auto bitmap = m_transport->read_back_drawing_buffer(m_canvas_context_id, rect);
    if (!bitmap.is_valid())
        return nullptr;
    return bitmap.bitmap();
}

void WebGLContextProxyBase::read_pixels_into_pixel_pack_buffer(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, long long offset)
{
    record(Commands::ReadPixelsIntoPixelPackBuffer {
        .x = x,
        .y = y,
        .width = width,
        .height = height,
        .format = format,
        .type = type,
        .offset = static_cast<GLintptr>(offset),
    });
}

void WebGLContextProxyBase::read_buffer_sub_data(GLenum target, long long offset, Bytes destination)
{
    SyncCalls::ReadBufferSubData::Request request {
        .target = target,
        .offset = static_cast<GLintptr>(offset),
        .size = static_cast<GLintptr>(destination.size()),
    };
    auto reply_bytes = send_sync_call(WebGLSyncCall::encode_request<SyncCalls::ReadBufferSubData>(request));
    if (m_lost)
        return;
    auto reply = WebGLSyncCall::decode_reply<SyncCalls::ReadBufferSubData::Reply>(reply_bytes);
    WebGLCommandList::copy_data_span(reply_bytes, reply.data, destination);
}

// --- Custom-handled GL entry points ---------------------------------------------------

void WebGLContextProxy::shader_source(GLuint shader, GLsizei count, GLchar const* const* string, GLint const* length)
{
    // The WebGL implementation always passes a single (concatenated) source string.
    VERIFY(count == 1);
    auto source_length = length ? static_cast<size_t>(length[0]) : __builtin_strlen(string[0]);
    ByteBuffer source_bytes = MUST(ByteBuffer::create_uninitialized(source_length + 1));
    __builtin_memcpy(source_bytes.data(), string[0], source_length);
    source_bytes[source_length] = 0;

    Commands::ShaderSource command { .shader = shader, .source = {} };
    command.source = { WebGLCommandList::first_inline_data_offset(sizeof(command)), static_cast<u32>(source_bytes.size()) };
    record(command, source_bytes);
}

static ByteBuffer pack_strings(GLsizei count, GLchar const* const* strings)
{
    StringBuilder builder;
    for (GLsizei i = 0; i < count; ++i) {
        builder.append({ strings[i], __builtin_strlen(strings[i]) });
        builder.append('\0');
    }
    return MUST(builder.to_byte_buffer());
}

void WebGLContextProxy::transform_feedback_varyings(GLuint program, GLsizei count, GLchar const* const* varyings, GLenum bufferMode)
{
    auto varyings_bytes = pack_strings(count, varyings);
    Commands::TransformFeedbackVaryings command { .program = program, .count = count, .varyings = {}, .buffer_mode = bufferMode };
    command.varyings = { WebGLCommandList::first_inline_data_offset(sizeof(command)), static_cast<u32>(varyings_bytes.size()) };
    record(command, varyings_bytes);
}

void WebGLContextProxy::read_pixels_robust_angle(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, GLsizei bufSize, GLsizei* length, GLsizei* columns, GLsizei* rows, void* pixels)
{
    SyncCalls::ReadPixelsRobustANGLE::Request request {
        .x = x,
        .y = y,
        .width = width,
        .height = height,
        .format = format,
        .type = type,
        .buf_size = bufSize,
    };
    auto reply_bytes = send_sync_call(WebGLSyncCall::encode_request<SyncCalls::ReadPixelsRobustANGLE>(request));
    if (is_lost())
        return;
    auto reply = WebGLSyncCall::decode_reply<SyncCalls::ReadPixelsRobustANGLE::Reply>(reply_bytes);
    if (length)
        *length = reply.length;
    if (columns)
        *columns = reply.columns;
    if (rows)
        *rows = reply.rows;
    if (pixels)
        WebGLCommandList::copy_data_span(reply_bytes, reply.pixels, { pixels, static_cast<size_t>(bufSize) });
}

GLubyte const* WebGLContextProxy::get_string(GLenum name)
{
    if (auto cached = m_string_cache.get(name); cached.has_value())
        return cached.value()->data();

    SyncCalls::GetString::Request request { .name = name };
    auto reply_bytes = send_sync_call(WebGLSyncCall::encode_request<SyncCalls::GetString>(request));
    if (is_lost())
        return reinterpret_cast<GLubyte const*>("");
    auto reply = WebGLSyncCall::decode_reply<SyncCalls::GetString::Reply>(reply_bytes);
    auto resolved = WebGLCommandList::resolve_string_span(reply_bytes, reply.value);
    auto value = make<ByteBuffer>(MUST(ByteBuffer::copy(resolved)));
    auto const* data = value->data();
    m_string_cache.set(name, move(value));
    return data;
}

void WebGLContextProxy::get_vertex_attrib_pointerv_robust_angle(GLuint index, GLenum pname, GLsizei bufSize, GLsizei* length, void** pointer)
{
    (void)bufSize;
    SyncCalls::GetVertexAttribPointervRobustANGLE::Request request { .index = index, .pname = pname };
    auto reply_bytes = send_sync_call(WebGLSyncCall::encode_request<SyncCalls::GetVertexAttribPointervRobustANGLE>(request));
    if (is_lost())
        return;
    auto reply = WebGLSyncCall::decode_reply<SyncCalls::GetVertexAttribPointervRobustANGLE::Reply>(reply_bytes);
    if (length)
        *length = 1;
    if (pointer)
        *pointer = reinterpret_cast<void*>(static_cast<uintptr_t>(reply.pointer));
}

void WebGLContextProxy::get_uniform_indices(GLuint program, GLsizei uniformCount, GLchar const* const* uniformNames, GLuint* uniformIndices)
{
    auto names_bytes = pack_strings(uniformCount, uniformNames);
    SyncCalls::GetUniformIndices::Request request { .program = program, .uniform_count = uniformCount, .uniform_names = {} };
    request.uniform_names = { WebGLCommandList::first_inline_data_offset(sizeof(request)), static_cast<u32>(names_bytes.size()) };
    auto reply_bytes = send_sync_call(WebGLSyncCall::encode_request<SyncCalls::GetUniformIndices>(request, names_bytes));
    if (is_lost())
        return;
    auto reply = WebGLSyncCall::decode_reply<SyncCalls::GetUniformIndices::Reply>(reply_bytes);
    if (uniformIndices)
        WebGLCommandList::copy_data_span(reply_bytes, reply.uniform_indices, { uniformIndices, static_cast<size_t>(uniformCount) * sizeof(GLuint) });
}

void* WebGLContextProxy::map_buffer_range(GLenum, GLintptr, GLsizeiptr, GLbitfield)
{
    // getBufferSubData() goes through read_buffer_sub_data() instead; nothing else maps.
    VERIFY_NOT_REACHED();
}

GLboolean WebGLContextProxy::unmap_buffer(GLenum)
{
    VERIFY_NOT_REACHED();
}

}
