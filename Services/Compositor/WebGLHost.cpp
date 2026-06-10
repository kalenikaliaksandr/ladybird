/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/WebGLCommandReplayer.h>
#include <Compositor/WebGLHost.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Compositor {

HostWebGLContext::HostWebGLContext(NonnullOwnPtr<Web::WebGL::OpenGLContext> gl_context)
    : m_gl_context(move(gl_context))
{
}

OwnPtr<HostWebGLContext> HostWebGLContext::create(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    auto gl_context = Web::WebGL::OpenGLContext::create(move(skia_backend_context), version, options);
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
    return Web::WebGL::WebGLCommandList::for_each_command(bytes, [&](auto const& command, ReadonlyBytes payload) -> ErrorOr<void> {
        return replay_webgl_command(*m_gl_context, m_objects, command, payload);
    });
}

ErrorOr<ByteBuffer> HostWebGLContext::execute_sync_call(ReadonlyBytes request)
{
    m_gl_context->make_current();
    return handle_webgl_sync_call(*m_gl_context, m_objects, request);
}

WebGLHost::WebGLHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context)
    : m_skia_backend_context(move(skia_backend_context))
{
}

HostWebGLContext* WebGLHost::create_context(Web::WebGL::WebGLContextId webgl_context_id, Web::WebGL::OpenGLContext::WebGLVersion version, Web::WebGL::OpenGLContext::DrawingBufferOptions options)
{
    if (!m_skia_backend_context)
        return nullptr;
    if (m_contexts.contains(webgl_context_id))
        return nullptr;
    auto context = HostWebGLContext::create(*m_skia_backend_context, version, options);
    if (!context)
        return nullptr;
    auto* context_ptr = context.ptr();
    m_contexts.set(webgl_context_id, context.release_nonnull());
    return context_ptr;
}

void WebGLHost::destroy_context(Web::WebGL::WebGLContextId webgl_context_id)
{
    m_contexts.remove(webgl_context_id);
}

HostWebGLContext* WebGLHost::context(Web::WebGL::WebGLContextId webgl_context_id)
{
    return m_contexts.get(webgl_context_id).value_or(nullptr);
}

}
