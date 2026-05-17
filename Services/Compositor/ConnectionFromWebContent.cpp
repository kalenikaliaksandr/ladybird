/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/HashMap.h>
#include <AK/IDAllocator.h>
#include <AK/NeverDestroyed.h>
#include <AK/NonnullRefPtr.h>
#include <Compositor/ConnectionFromWebContent.h>

namespace Compositor {

static IDAllocator s_connection_ids;

static HashMap<Web::Compositor::WebContentConnectionId, NonnullRefPtr<ConnectionFromWebContent>>& web_content_connections()
{
    static NeverDestroyed<HashMap<Web::Compositor::WebContentConnectionId, NonnullRefPtr<ConnectionFromWebContent>>> connections;
    return *connections;
}

ErrorOr<Web::Compositor::WebContentConnectionId> ConnectionFromWebContent::connect(IPC::TransportHandle handle)
{
    auto raw_connection_id = s_connection_ids.allocate();
    auto connection_id = Web::Compositor::WebContentConnectionId { static_cast<u64>(raw_connection_id) };

    auto transport = TRY(handle.create_transport());
    auto connection = ConnectionFromWebContent::construct(move(transport), connection_id);
    web_content_connections().set(connection_id, connection);

    return connection_id;
}

ConnectionFromWebContent::ConnectionFromWebContent(NonnullOwnPtr<IPC::Transport> transport, Web::Compositor::WebContentConnectionId connection_id)
    : IPC::ConnectionFromClient<WebContentCompositorClientEndpoint, WebContentCompositorServerEndpoint>(*this, move(transport), static_cast<int>(connection_id.value()))
    , m_connection_id(connection_id)
{
}

void ConnectionFromWebContent::die()
{
    web_content_connections().remove(m_connection_id);
    s_connection_ids.deallocate(static_cast<int>(m_connection_id.value()));
}

void ConnectionFromWebContent::create_context(u64, u64, Web::Compositor::SerializedPresentationModeKind, u64, u64, u64, u64, Web::DisplayListPlayerType)
{
}

void ConnectionFromWebContent::destroy_context(u64)
{
}

void ConnectionFromWebContent::set_presentation_mode(u64, Web::Compositor::SerializedPresentationModeKind, u64, u64, u64, u64)
{
}

void ConnectionFromWebContent::stop_presenting_to_client(u64)
{
}

void ConnectionFromWebContent::viewport_size_updated(u64, Gfx::IntSize, bool, Web::Compositor::WindowResizingInProgress)
{
}

}
