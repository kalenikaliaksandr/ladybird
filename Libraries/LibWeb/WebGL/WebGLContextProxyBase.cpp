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

WebGLContextProxyBase::WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport> transport, u64 webgl_context_id, WebGLVersion webgl_version, Vector<String> supported_extensions)
    : m_transport(move(transport))
    , m_webgl_context_id(webgl_context_id)
    , m_webgl_version(webgl_version)
    , m_supported_extensions(move(supported_extensions))
{
}

WebGLContextProxyBase::~WebGLContextProxyBase()
{
    m_transport->destroy_context(m_webgl_context_id);
}

u64 WebGLContextProxyBase::allocate_webgl_context_id()
{
    static u64 s_next_webgl_context_id = 1;
    return s_next_webgl_context_id++;
}

RefPtr<Gfx::PaintingSurface> WebGLContextProxyBase::surface()
{
    // The drawing buffer lives in the Compositor; there is nothing to sample locally.
    return nullptr;
}

void WebGLContextProxyBase::set_size(Gfx::IntSize const& size)
{
    if (m_lost)
        return;
    m_transport->webgl_set_drawing_buffer_size(m_webgl_context_id, size.width(), size.height());
}

void WebGLContextProxyBase::present_to_compositor(u64 target_context_id, u64 surface_id, bool preserve_drawing_buffer)
{
    if (m_lost)
        return;
    m_transport->webgl_present_to_compositor(m_webgl_context_id, target_context_id, surface_id, preserve_drawing_buffer);
}

RefPtr<Gfx::Bitmap> WebGLContextProxyBase::read_back_drawing_buffer()
{
    if (m_lost)
        return nullptr;
    auto bitmap = m_transport->read_back_drawing_buffer(m_webgl_context_id);
    if (!bitmap.is_valid())
        return nullptr;
    return bitmap.bitmap();
}

void WebGLContextProxyBase::read_pixels_into_pixel_pack_buffer(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, long long offset)
{
    if (m_lost)
        return;
    m_transport->webgl_read_pixels_into_pixel_pack_buffer(m_webgl_context_id, x, y, width, height, format, type, static_cast<i64>(offset));
}

void WebGLContextProxyBase::read_buffer_sub_data(GLenum target, long long offset, Bytes destination)
{
    if (m_lost)
        return;
    auto data = m_transport->webgl_read_buffer_sub_data(m_webgl_context_id, target, static_cast<i64>(offset), static_cast<i64>(destination.size()));
    __builtin_memcpy(destination.data(), data.data(), min(data.size(), destination.size()));
}

// --- Custom-handled GL entry points ---------------------------------------------------

void WebGLContextProxy::shader_source(GLuint shader, GLsizei count, GLchar const* const* string, GLint const* length)
{
    // The WebGL implementation always passes a single (concatenated) source string.
    VERIFY(count == 1);
    if (is_lost())
        return;
    auto source_length = length ? static_cast<size_t>(length[0]) : __builtin_strlen(string[0]);
    transport().webgl_shader_source(webgl_context_id(), shader, StringView { string[0], source_length });
}

static Vector<String> string_list(GLsizei count, GLchar const* const* strings)
{
    Vector<String> result;
    result.ensure_capacity(count);
    for (GLsizei i = 0; i < count; ++i)
        result.unchecked_append(String::from_utf8_with_replacement_character(StringView { strings[i], __builtin_strlen(strings[i]) }));
    return result;
}

void WebGLContextProxy::transform_feedback_varyings(GLuint program, GLsizei count, GLchar const* const* varyings, GLenum bufferMode)
{
    if (is_lost())
        return;
    transport().webgl_transform_feedback_varyings(webgl_context_id(), program, string_list(count, varyings), bufferMode);
}

void WebGLContextProxy::read_pixels_robust_angle(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, GLsizei bufSize, GLsizei* length, GLsizei* columns, GLsizei* rows, void* pixels)
{
    if (is_lost())
        return;
    i32 reply_length = 0;
    i32 reply_columns = 0;
    i32 reply_rows = 0;
    ByteBuffer reply_pixels;
    transport().webgl_read_pixels_robust_angle(webgl_context_id(), x, y, width, height, format, type, bufSize, reply_length, reply_columns, reply_rows, reply_pixels);
    if (length)
        *length = reply_length;
    if (columns)
        *columns = reply_columns;
    if (rows)
        *rows = reply_rows;
    if (pixels)
        __builtin_memcpy(pixels, reply_pixels.data(), min(reply_pixels.size(), static_cast<size_t>(bufSize)));
}

GLubyte const* WebGLContextProxy::get_string(GLenum name)
{
    if (auto cached = m_string_cache.get(name); cached.has_value())
        return reinterpret_cast<GLubyte const*>(cached.value().characters());
    if (is_lost())
        return reinterpret_cast<GLubyte const*>("");
    auto value = transport().webgl_get_string(webgl_context_id(), name);
    m_string_cache.set(name, value.to_byte_string());
    return reinterpret_cast<GLubyte const*>(m_string_cache.get(name).value().characters());
}

void WebGLContextProxy::get_vertex_attrib_pointerv_robust_angle(GLuint index, GLenum pname, GLsizei bufSize, GLsizei* length, void** pointer)
{
    (void)bufSize;
    if (is_lost())
        return;
    auto reply_pointer = transport().webgl_get_vertex_attrib_pointerv_robust_angle(webgl_context_id(), index, pname);
    if (length)
        *length = 1;
    if (pointer)
        *pointer = reinterpret_cast<void*>(static_cast<uintptr_t>(reply_pointer));
}

void WebGLContextProxy::get_uniform_indices(GLuint program, GLsizei uniformCount, GLchar const* const* uniformNames, GLuint* uniformIndices)
{
    if (is_lost())
        return;
    auto indices = transport().webgl_get_uniform_indices(webgl_context_id(), program, string_list(uniformCount, uniformNames));
    if (uniformIndices)
        __builtin_memcpy(uniformIndices, indices.data(), min(indices.size() * sizeof(u32), static_cast<size_t>(uniformCount) * sizeof(GLuint)));
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
