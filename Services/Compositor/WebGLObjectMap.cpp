/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/WebGLObjectMap.h>

namespace Compositor {

ErrorOr<GLuint> WebGLObjectMap::lookup(Web::WebGL::WebGLObjectId id) const
{
    if (id == 0)
        return 0;
    if (auto name = m_objects.get(id); name.has_value())
        return name.value();
    return Error::from_string_literal("Unknown WebGL object id");
}

ErrorOr<GLuint> WebGLObjectMap::take(Web::WebGL::WebGLObjectId id)
{
    if (id == 0)
        return 0;
    auto name = m_objects.take(id);
    if (!name.has_value())
        return Error::from_string_literal("Unknown WebGL object id");
    return name.value();
}

ErrorOr<void> WebGLObjectMap::add(Web::WebGL::WebGLObjectId id, GLuint name)
{
    if (id == 0)
        return Error::from_string_literal("WebGL object id 0 is reserved");
    if (m_objects.contains(id))
        return Error::from_string_literal("WebGL object id is already in use");
    m_objects.set(id, name);
    return {};
}

ErrorOr<GLsync> WebGLObjectMap::lookup_sync(Web::WebGL::WebGLObjectId id) const
{
    if (auto sync = m_syncs.get(id); sync.has_value())
        return sync.value();
    return Error::from_string_literal("Unknown WebGL sync object id");
}

ErrorOr<GLsync> WebGLObjectMap::take_sync(Web::WebGL::WebGLObjectId id)
{
    auto sync = m_syncs.take(id);
    if (!sync.has_value())
        return Error::from_string_literal("Unknown WebGL sync object id");
    return sync.value();
}

ErrorOr<void> WebGLObjectMap::add_sync(Web::WebGL::WebGLObjectId id, GLsync sync)
{
    if (id == 0)
        return Error::from_string_literal("WebGL object id 0 is reserved");
    if (m_syncs.contains(id))
        return Error::from_string_literal("WebGL object id is already in use");
    m_syncs.set(id, sync);
    return {};
}

}
