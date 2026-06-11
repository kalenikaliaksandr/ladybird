/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/WebGL/WebGLContextProxyBase.h>

namespace Web::WebGL {

WebGLContextProxyBase::WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport> transport, Painting::CanvasContextId canvas_context_id)
    : m_transport(move(transport))
    , m_canvas_context_id(canvas_context_id)
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

}
