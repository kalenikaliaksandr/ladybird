/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <LibWeb/Export.h>
#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Web::WebGL {

// State and transport shared by every generated WebGLContextProxy method: the pending
// command batch, the client-side object id allocator, and the lost flag. When the
// context is lost (the compositor went away), recording becomes a no-op and sync calls
// return zeroed replies.
class WEB_API WebGLContextProxyBase {
    AK_MAKE_NONCOPYABLE(WebGLContextProxyBase);
    AK_MAKE_NONMOVABLE(WebGLContextProxyBase);

public:
    WebGLContextProxyBase(NonnullRefPtr<RemoteWebGLTransport>, Painting::CanvasContextId);
    ~WebGLContextProxyBase();

    static Painting::CanvasContextId allocate_canvas_context_id();

    void flush_commands();
    void set_lost() { m_lost = true; }

protected:
    // A single batch never approaches IPC's message size limit; oversized uploads are
    // chunked by the recorder before they get here.
    static constexpr size_t max_pending_command_bytes = 4 * MiB;

    WebGLObjectId allocate_object_id() { return m_next_object_id++; }

    template<typename Command>
    void record(Command const& command, ReadonlyBytes inline_data = {})
    {
        if (m_lost)
            return;
        m_commands.append(command, inline_data);
        if (m_commands.size_in_bytes() >= max_pending_command_bytes)
            flush_commands();
    }

    ByteBuffer send_sync_call(ByteBuffer request);
    bool is_lost() const { return m_lost; }

private:
    NonnullRefPtr<RemoteWebGLTransport> m_transport;
    Painting::CanvasContextId m_canvas_context_id { 0 };
    WebGLCommandList m_commands;
    u32 m_next_object_id { 1 };
    bool m_lost { false };
};

}
