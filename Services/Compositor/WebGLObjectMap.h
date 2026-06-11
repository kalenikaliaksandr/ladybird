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
// context created for them. Id 0 translates to GL object 0 (unbind semantics); any
// other unknown id is an error, since it can only come from a misbehaving WebContent.
// The reverse direction exists for the few getters that report a bound object back to
// the client.
class WebGLObjectMap {
public:
    ErrorOr<GLuint> lookup(Web::WebGL::WebGLObjectId) const;
    ErrorOr<GLuint> take(Web::WebGL::WebGLObjectId);
    ErrorOr<void> add(Web::WebGL::WebGLObjectId, GLuint);
    Web::WebGL::WebGLObjectId reverse_lookup(GLuint) const;

    ErrorOr<GLsync> lookup_sync(Web::WebGL::WebGLObjectId) const;
    ErrorOr<GLsync> take_sync(Web::WebGL::WebGLObjectId);
    ErrorOr<void> add_sync(Web::WebGL::WebGLObjectId, GLsync);

private:
    HashMap<Web::WebGL::WebGLObjectId, GLuint> m_objects;
    HashMap<GLuint, Web::WebGL::WebGLObjectId> m_reverse;
    HashMap<Web::WebGL::WebGLObjectId, GLsync> m_syncs;
};

}
