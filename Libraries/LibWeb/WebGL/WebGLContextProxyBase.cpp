/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/WebGL/WebGLContextProxyBase.h>

namespace Web::WebGL {

WebGLContextProxyBase::WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport> transport, WebGLContextId webgl_context_id)
    : m_transport(move(transport))
    , m_webgl_context_id(webgl_context_id)
{
}

WebGLContextProxyBase::~WebGLContextProxyBase()
{
    m_transport->destroy_context(m_webgl_context_id);
}

WebGLContextId WebGLContextProxyBase::allocate_webgl_context_id()
{
    static u64 s_next_webgl_context_id = 1;
    return WebGLContextId { s_next_webgl_context_id++ };
}

void WebGLContextProxyBase::flush_commands()
{
    if (m_commands.is_empty())
        return;
    m_transport->send_commands(m_webgl_context_id, m_commands.buffer());
    m_commands.clear_with_capacity();
}

ByteBuffer WebGLContextProxyBase::send_sync_call(ByteBuffer request)
{
    if (m_lost)
        return {};
    flush_commands();
    return m_transport->sync_call(m_webgl_context_id, move(request));
}

}
