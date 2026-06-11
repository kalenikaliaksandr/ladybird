/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/Vector.h>
#include <Compositor/WebGLMessageDispatcher.h>

// The wire-specified WebGL messages: operations that do not map 1:1 onto a GL entry
// point (string lists, the two readPixels modes, getBufferSubData as a single read,
// drawing-buffer resizing). Their regular siblings are generated; these are few and
// irregular enough to keep by hand.

namespace Compositor {

using namespace Web::WebGL;

void WebGLMessageDispatcher::webgl_shader_source(u64 webgl_context_id, u32 shader, String source)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return;
    auto shader_or_error = context->objects().lookup(shader);
    if (shader_or_error.is_error()) {
        did_misbehave("WebContent referenced an unknown WebGL object id");
        return;
    }
    auto source_string = source.to_byte_string();
    GLchar const* source_characters = source_string.characters();
    GLint source_length = static_cast<GLint>(source_string.length());
    context->gl_context().shader_source(shader_or_error.value(), 1, &source_characters, &source_length);
}

static ErrorOr<Vector<ByteString>> null_terminated_string_list(Vector<String> const& strings)
{
    if (strings.size() > max_webgl_string_list_entries)
        return Error::from_string_literal("WebGL string list is too long");
    Vector<ByteString> result;
    result.ensure_capacity(strings.size());
    for (auto const& string : strings)
        result.unchecked_append(string.to_byte_string());
    return result;
}

void WebGLMessageDispatcher::webgl_transform_feedback_varyings(u64 webgl_context_id, u32 program, Vector<String> varyings, u32 buffer_mode)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return;
    auto program_or_error = context->objects().lookup(program);
    if (program_or_error.is_error()) {
        did_misbehave("WebContent referenced an unknown WebGL object id");
        return;
    }
    auto varying_strings_or_error = null_terminated_string_list(varyings);
    if (varying_strings_or_error.is_error()) {
        did_misbehave("WebContent sent an oversized WebGL string list");
        return;
    }
    auto varying_strings = varying_strings_or_error.release_value();
    Vector<GLchar const*> varying_characters;
    varying_characters.ensure_capacity(varying_strings.size());
    for (auto const& varying : varying_strings)
        varying_characters.unchecked_append(varying.characters());
    context->gl_context().transform_feedback_varyings(program_or_error.value(), static_cast<GLsizei>(varying_characters.size()), varying_characters.data(), buffer_mode);
}

Messages::CompositorWebContentServer::WebglReadPixelsRobustAngleResponse WebGLMessageDispatcher::webgl_read_pixels_robust_angle(u64 webgl_context_id, i32 x, i32 y, i32 width, i32 height, u32 format, u32 type, i32 buf_size)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return { 0, 0, 0, {} };
    if (buf_size < 0 || static_cast<u64>(buf_size) > max_webgl_readback_size) {
        did_misbehave("WebGL readback is too large");
        return { 0, 0, 0, {} };
    }
    auto pixels_or_error = ByteBuffer::create_zeroed(buf_size);
    if (pixels_or_error.is_error())
        return { 0, 0, 0, {} };
    auto pixels = pixels_or_error.release_value();
    GLsizei length = 0;
    GLsizei columns = 0;
    GLsizei rows = 0;
    context->gl_context().read_pixels_robust_angle(x, y, width, height, format, type, buf_size, &length, &columns, &rows, pixels.data());
    return { length, columns, rows, move(pixels) };
}

Messages::CompositorWebContentServer::WebglGetStringResponse WebGLMessageDispatcher::webgl_get_string(u64 webgl_context_id, u32 name)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return { String {} };
    auto const* value = context->gl_context().get_string(name);
    if (!value)
        return { String {} };
    return { String::from_utf8_with_replacement_character(StringView { reinterpret_cast<char const*>(value), __builtin_strlen(reinterpret_cast<char const*>(value)) }) };
}

Messages::CompositorWebContentServer::WebglGetVertexAttribPointervRobustAngleResponse WebGLMessageDispatcher::webgl_get_vertex_attrib_pointerv_robust_angle(u64 webgl_context_id, u32 index, u32 pname)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return { 0 };
    void* pointer = nullptr;
    GLsizei length = 0;
    context->gl_context().get_vertex_attrib_pointerv_robust_angle(index, pname, 1, &length, &pointer);
    return { static_cast<i64>(reinterpret_cast<uintptr_t>(pointer)) };
}

Messages::CompositorWebContentServer::WebglGetUniformIndicesResponse WebGLMessageDispatcher::webgl_get_uniform_indices(u64 webgl_context_id, u32 program, Vector<String> uniform_names)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return { {} };
    auto program_or_error = context->objects().lookup(program);
    if (program_or_error.is_error()) {
        did_misbehave("WebContent referenced an unknown WebGL object id");
        return { {} };
    }
    auto name_strings_or_error = null_terminated_string_list(uniform_names);
    if (name_strings_or_error.is_error()) {
        did_misbehave("WebContent sent an oversized WebGL string list");
        return { {} };
    }
    auto name_strings = name_strings_or_error.release_value();
    Vector<GLchar const*> name_characters;
    name_characters.ensure_capacity(name_strings.size());
    for (auto const& name : name_strings)
        name_characters.unchecked_append(name.characters());
    Vector<u32> indices;
    indices.resize(name_characters.size());
    context->gl_context().get_uniform_indices(program_or_error.value(), static_cast<GLsizei>(name_characters.size()), name_characters.data(), indices.data());
    return { move(indices) };
}

void WebGLMessageDispatcher::webgl_set_drawing_buffer_size(u64 webgl_context_id, i32 width, i32 height)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return;
    if (context->set_drawing_buffer_size(width, height).is_error())
        did_misbehave("WebContent requested an invalid WebGL drawing buffer size");
}

void WebGLMessageDispatcher::webgl_read_pixels_into_pixel_pack_buffer(u64 webgl_context_id, i32 x, i32 y, i32 width, i32 height, u32 format, u32 type, i64 offset)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return;
    context->gl_context().read_pixels_robust_angle(x, y, width, height, format, type, 0, nullptr, nullptr, nullptr, reinterpret_cast<void*>(static_cast<uintptr_t>(offset)));
}

Messages::CompositorWebContentServer::WebglReadBufferSubDataResponse WebGLMessageDispatcher::webgl_read_buffer_sub_data(u64 webgl_context_id, u32 target, i64 offset, i64 size)
{
    auto* context = webgl_context_for_message(webgl_context_id);
    if (!context)
        return { {} };
    if (offset < 0 || size < 0 || static_cast<u64>(size) > max_webgl_readback_size) {
        did_misbehave("WebGL readback is too large");
        return { {} };
    }
    auto data_or_error = ByteBuffer::create_zeroed(static_cast<size_t>(size));
    if (data_or_error.is_error())
        return { {} };
    auto data = data_or_error.release_value();
    if (auto* mapped = context->gl_context().map_buffer_range(target, offset, size, GL_MAP_READ_BIT)) {
        __builtin_memcpy(data.data(), mapped, data.size());
        context->gl_context().unmap_buffer(target);
    }
    return { move(data) };
}

}
