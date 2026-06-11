/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/HashMap.h>
#include <LibWeb/WebGL/GLFunctions.h>
#include <LibWeb/WebGL/Types.h>

namespace Compositor {

// Maps client-allocated WebGL object ids to the GL names (and GLsync objects) the host
// context created for them. Id 0 and any unknown id translate to the GL default object
// (0 / nullptr): a client may legitimately reference an already-deleted object (double
// delete, use-after-delete and deleteSync(null) are silent no-ops per the WebGL spec), so
// an unknown id must not be treated as misbehavior. Allocating a duplicate id still is.
class WebGLObjectMap {
public:
    ErrorOr<GLuint> lookup(Web::WebGL::WebGLObjectId) const;
    ErrorOr<GLuint> take(Web::WebGL::WebGLObjectId);
    ErrorOr<void> add(Web::WebGL::WebGLObjectId, GLuint);

    ErrorOr<GLsync> lookup_sync(Web::WebGL::WebGLObjectId) const;
    ErrorOr<GLsync> take_sync(Web::WebGL::WebGLObjectId);
    ErrorOr<void> add_sync(Web::WebGL::WebGLObjectId, GLsync);

private:
    HashMap<Web::WebGL::WebGLObjectId, GLuint> m_objects;
    HashMap<Web::WebGL::WebGLObjectId, GLsync> m_syncs;
};

}
