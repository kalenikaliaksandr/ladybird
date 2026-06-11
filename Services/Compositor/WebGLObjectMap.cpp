/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/WebGLObjectMap.h>

namespace Compositor {

// Id 0 and any unknown id translate to the GL default object (0 / nullptr). A client may
// legitimately reference an object it already deleted: per the WebGL spec deleting an
// object twice, using a deleted object, and deleteSync(null) are all silent no-ops.
// Treating an unknown id as an error would let those spec-legal calls reach did_misbehave()
// and tear down the whole connection (every canvas and WebGL context of that renderer).
template<typename Value>
static Value lookup_or_default(HashMap<Web::WebGL::WebGLObjectId, Value> const& map, Web::WebGL::WebGLObjectId id)
{
    return map.get(id).value_or(Value {});
}

template<typename Value>
static Value take_or_default(HashMap<Web::WebGL::WebGLObjectId, Value>& map, Web::WebGL::WebGLObjectId id)
{
    return map.take(id).value_or(Value {});
}

template<typename Value>
static ErrorOr<void> add_unique(HashMap<Web::WebGL::WebGLObjectId, Value>& map, Web::WebGL::WebGLObjectId id, Value value)
{
    if (id == 0)
        return Error::from_string_literal("WebGL object id 0 is reserved");
    if (map.contains(id))
        return Error::from_string_literal("WebGL object id is already in use");
    map.set(id, value);
    return {};
}

ErrorOr<GLuint> WebGLObjectMap::lookup(Web::WebGL::WebGLObjectId id) const
{
    return lookup_or_default(m_objects, id);
}

ErrorOr<GLuint> WebGLObjectMap::take(Web::WebGL::WebGLObjectId id)
{
    return take_or_default(m_objects, id);
}

ErrorOr<void> WebGLObjectMap::add(Web::WebGL::WebGLObjectId id, GLuint name)
{
    return add_unique(m_objects, id, name);
}

ErrorOr<GLsync> WebGLObjectMap::lookup_sync(Web::WebGL::WebGLObjectId id) const
{
    return lookup_or_default(m_syncs, id);
}

ErrorOr<GLsync> WebGLObjectMap::take_sync(Web::WebGL::WebGLObjectId id)
{
    return take_or_default(m_syncs, id);
}

ErrorOr<void> WebGLObjectMap::add_sync(Web::WebGL::WebGLObjectId id, GLsync sync)
{
    return add_unique(m_syncs, id, sync);
}

}
