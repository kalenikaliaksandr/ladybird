/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Vector.h>
#include <Compositor/WebGLCommandReplayer.h>
#include <Compositor/WebGLHost.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Compositor {

using namespace Web::WebGL;

// A page-controlled readback (readPixels, getBufferSubData) never allocates more than
// this on the host; the client chunks larger reads.
static constexpr size_t max_webgl_readback_size = 16 * MiB;

// Packed-string lists (shader varyings, uniform names) are bounded well above anything
// a real program produces.
static constexpr GLsizei max_webgl_string_list_entries = 16384;

static constexpr int max_webgl_drawing_buffer_dimension = 16384;

HostWebGLContext::HostWebGLContext(NonnullOwnPtr<Web::WebGL::OpenGLContext> gl_context)
    : m_gl_context(move(gl_context))
{
}

OwnPtr<HostWebGLContext> HostWebGLContext::create(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    auto gl_context = Web::WebGL::OpenGLContext::create(skia_backend_context, version, options);
    if (!gl_context)
        return {};
    // The drawing buffer is allocated lazily on first use and needs a non-empty size;
    // the client follows up with its real size before issuing any drawing commands.
    gl_context->set_size({ 1, 1 });
    return adopt_own(*new HostWebGLContext(gl_context.release_nonnull()));
}

ErrorOr<void> HostWebGLContext::execute_commands(ReadonlyBytes bytes)
{
    m_gl_context->make_current();

    // A non-preserving context's drawing buffer is cleared after being prepared for
    // compositing, but the clear is deferred to here (the start of the next frame's
    // commands) so a readback taken before then still sees the rendered frame.
    if (m_needs_clear_before_next_frame) {
        m_gl_context->clear_buffer_to_default_values();
        m_needs_clear_before_next_frame = false;
    }

    return WebGLCommandList::for_each_command(bytes, [&]<typename Command>(Command const& command, ReadonlyBytes payload) -> ErrorOr<void> {
        if constexpr (IsSame<Command, Commands::SetDrawingBufferSize>) {
            (void)payload;
            return set_drawing_buffer_size(command.width, command.height);
        } else if constexpr (IsSame<Command, Commands::ReadPixelsIntoPixelPackBuffer>) {
            (void)payload;
            m_gl_context->read_pixels_robust_angle(command.x, command.y, command.width, command.height, command.format, command.type, 0, nullptr, nullptr, nullptr, reinterpret_cast<void*>(static_cast<uintptr_t>(command.offset)));
            return {};
        } else {
            return replay_webgl_command(*m_gl_context, m_objects, command, payload);
        }
    });
}

ErrorOr<ByteBuffer> HostWebGLContext::execute_sync_call(ReadonlyBytes request)
{
    m_gl_context->make_current();
    return handle_webgl_sync_call(*m_gl_context, m_objects, request);
}

ErrorOr<NonnullRefPtr<Gfx::PaintingSurface>> HostWebGLContext::prepare_for_compositing(bool preserve_drawing_buffer)
{
    // Flush all pending GL work so Skia samples the finished drawing buffer. The
    // default framebuffer was written behind Skia's back, so discard cached snapshots
    // before the display-list player asks Skia for an image.
    m_gl_context->present(/* preserve_drawing_buffer= */ true);

    auto drawing_surface = m_gl_context->surface();
    if (!drawing_surface)
        return Error::from_string_literal("WebGL context has no drawing buffer");
    m_gl_context->notify_content_will_change();

    // Defer the clear (see execute_commands) so a readback before the next frame still sees
    // this frame.
    if (!preserve_drawing_buffer)
        m_needs_clear_before_next_frame = true;

    return drawing_surface.release_nonnull();
}

ErrorOr<void> HostWebGLContext::set_drawing_buffer_size(int width, int height)
{
    if (width < 1 || height < 1 || width > max_webgl_drawing_buffer_dimension || height > max_webgl_drawing_buffer_dimension)
        return Error::from_string_literal("Invalid WebGL drawing buffer size");
    m_gl_context->set_size({ width, height });
    m_gl_context->make_current();
    return {};
}

// --- Wire-specified commands ---------------------------------------------------------

ErrorOr<void> replay_webgl_command(Web::WebGL::OpenGLContext& gl, WebGLObjectMap& objects, Commands::ShaderSource const& command, ReadonlyBytes payload)
{
    auto source_bytes = WebGLCommandList::resolve_string_span(payload, command.source);
    auto shader = TRY(objects.lookup(command.shader));
    GLchar const* source = reinterpret_cast<GLchar const*>(source_bytes.data());
    GLint length = static_cast<GLint>(source_bytes.size() - 1);
    gl.shader_source(shader, 1, &source, &length);
    return {};
}

// Splits a payload of `count` packed NUL-terminated strings into pointers.
static ErrorOr<Vector<GLchar const*>> split_packed_strings(ReadonlyBytes bytes, GLsizei count)
{
    if (count < 0 || count > max_webgl_string_list_entries)
        return Error::from_string_literal("WebGL string list is too long");
    Vector<GLchar const*> strings;
    strings.ensure_capacity(count);
    size_t cursor = 0;
    for (GLsizei i = 0; i < count; ++i) {
        auto start = cursor;
        while (cursor < bytes.size() && bytes[cursor] != 0)
            ++cursor;
        if (cursor >= bytes.size())
            return Error::from_string_literal("WebGL string is not NUL-terminated");
        strings.unchecked_append(reinterpret_cast<GLchar const*>(bytes.data() + start));
        ++cursor;
    }
    return strings;
}

ErrorOr<void> replay_webgl_command(Web::WebGL::OpenGLContext& gl, WebGLObjectMap& objects, Commands::TransformFeedbackVaryings const& command, ReadonlyBytes payload)
{
    auto varyings_bytes = WebGLCommandList::resolve_data_span(payload, command.varyings);
    auto varyings = TRY(split_packed_strings(varyings_bytes, command.count));
    auto program = TRY(objects.lookup(command.program));
    gl.transform_feedback_varyings(program, command.count, varyings.data(), command.buffer_mode);
    return {};
}

// --- Wire-specified synchronous calls ------------------------------------------------

ErrorOr<ByteBuffer> handle_one(Web::WebGL::OpenGLContext& gl, WebGLObjectMap&, SyncCalls::GetString::Request const& request, ReadonlyBytes)
{
    auto const* value = gl.get_string(request.name);
    ReadonlyBytes value_bytes { value, value ? __builtin_strlen(reinterpret_cast<char const*>(value)) + 1 : 0 };
    SyncCalls::GetString::Reply reply {
        .value = { WebGLCommandList::first_inline_data_offset(sizeof(SyncCalls::GetString::Reply)), static_cast<u32>(value_bytes.size()) },
    };
    return WebGLSyncCall::encode_reply(reply, value_bytes);
}

ErrorOr<ByteBuffer> handle_one(Web::WebGL::OpenGLContext& gl, WebGLObjectMap&, SyncCalls::ReadPixelsRobustANGLE::Request const& request, ReadonlyBytes)
{
    if (request.buf_size < 0 || static_cast<u64>(request.buf_size) > max_webgl_readback_size)
        return Error::from_string_literal("WebGL readback is too large");
    auto pixels = TRY(ByteBuffer::create_zeroed(request.buf_size));
    GLsizei length = 0;
    GLsizei columns = 0;
    GLsizei rows = 0;
    gl.read_pixels_robust_angle(request.x, request.y, request.width, request.height, request.format, request.type, request.buf_size, &length, &columns, &rows, pixels.data());
    SyncCalls::ReadPixelsRobustANGLE::Reply reply {
        .length = length,
        .columns = columns,
        .rows = rows,
        .pixels = { WebGLCommandList::first_inline_data_offset(sizeof(SyncCalls::ReadPixelsRobustANGLE::Reply)), static_cast<u32>(pixels.size()) },
    };
    return WebGLSyncCall::encode_reply(reply, pixels);
}

ErrorOr<ByteBuffer> handle_one(Web::WebGL::OpenGLContext& gl, WebGLObjectMap&, SyncCalls::GetVertexAttribPointervRobustANGLE::Request const& request, ReadonlyBytes)
{
    void* pointer = nullptr;
    GLsizei length = 0;
    gl.get_vertex_attrib_pointerv_robust_angle(request.index, request.pname, 1, &length, &pointer);
    SyncCalls::GetVertexAttribPointervRobustANGLE::Reply reply {
        .pointer = static_cast<Web::WebGL::GLintptr>(reinterpret_cast<uintptr_t>(pointer)),
    };
    return WebGLSyncCall::encode_reply(reply);
}

ErrorOr<ByteBuffer> handle_one(Web::WebGL::OpenGLContext& gl, WebGLObjectMap& objects, SyncCalls::GetUniformIndices::Request const& request, ReadonlyBytes payload)
{
    auto names_bytes = WebGLCommandList::resolve_data_span(payload, request.uniform_names);
    auto names = TRY(split_packed_strings(names_bytes, request.uniform_count));
    auto program = TRY(objects.lookup(request.program));
    Vector<GLuint> indices;
    indices.resize(request.uniform_count);
    gl.get_uniform_indices(program, request.uniform_count, names.data(), indices.data());
    ReadonlyBytes indices_bytes { indices.data(), indices.size() * sizeof(GLuint) };
    SyncCalls::GetUniformIndices::Reply reply {
        .uniform_indices = { WebGLCommandList::first_inline_data_offset(sizeof(SyncCalls::GetUniformIndices::Reply)), static_cast<u32>(indices_bytes.size()) },
    };
    return WebGLSyncCall::encode_reply(reply, indices_bytes);
}

ErrorOr<ByteBuffer> handle_one(Web::WebGL::OpenGLContext& gl, WebGLObjectMap&, SyncCalls::ReadBufferSubData::Request const& request, ReadonlyBytes)
{
    if (request.offset < 0 || request.size < 0 || static_cast<u64>(request.size) > max_webgl_readback_size)
        return Error::from_string_literal("WebGL readback is too large");
    auto data = TRY(ByteBuffer::create_zeroed(static_cast<size_t>(request.size)));
    if (auto* mapped = gl.map_buffer_range(request.target, request.offset, request.size, GL_MAP_READ_BIT)) {
        __builtin_memcpy(data.data(), mapped, data.size());
        gl.unmap_buffer(request.target);
    }
    SyncCalls::ReadBufferSubData::Reply reply {
        .data = { WebGLCommandList::first_inline_data_offset(sizeof(SyncCalls::ReadBufferSubData::Reply)), static_cast<u32>(data.size()) },
    };
    return WebGLSyncCall::encode_reply(reply, data);
}

// --------------------------------------------------------------------------------------

WebGLHost::WebGLHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

HostWebGLContext* WebGLHost::create_context(Web::Painting::CanvasContextId canvas_context_id, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    if (!m_skia_backend_context)
        return nullptr;
    if (m_contexts.contains(canvas_context_id))
        return nullptr;
    auto context = HostWebGLContext::create(*m_skia_backend_context, version, options);
    if (!context)
        return nullptr;
    auto* context_ptr = context.ptr();
    m_contexts.set(canvas_context_id, context.release_nonnull());
    return context_ptr;
}

void WebGLHost::destroy_context(Web::Painting::CanvasContextId canvas_context_id)
{
    m_contexts.remove(canvas_context_id);
}

HostWebGLContext* WebGLHost::context(Web::Painting::CanvasContextId canvas_context_id)
{
    return m_contexts.get(canvas_context_id).value_or(nullptr);
}

}
