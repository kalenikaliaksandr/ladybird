/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/RefPtr.h>
#include <LibGfx/Cursor.h>
#include <LibWeb/Compositor/CompositorHost.h>
#include <WebContent/Forward.h>

namespace WebContent {

NonnullOwnPtr<Web::Compositor::CompositorHost> create_web_content_compositor_host();
void set_compositor_process_connection(RefPtr<CompositorConnection>);
void set_compositor_context_destruction_handler(Function<void(Web::Compositor::CompositorContextId)>);
bool request_compositor_process_cursor_change(u64 page_id, Gfx::Cursor const&);

}
