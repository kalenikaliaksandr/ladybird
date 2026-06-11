/*
 * Copyright (c) 2022, Luke Wilde <lukew@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>
#include <LibGC/Root.h>
#include <LibWeb/Forward.h>

namespace Web::WebGL {

using GLenum = unsigned int;
using GLuint = unsigned int;
using GLint = int;
using GLsizei = int;
using GLintptr = long long;
using GLchar = char;

// FIXME: This should really be "struct __GLsync*", but the linker doesn't recognise it.
//        Since this conflicts with the original definition of GLsync, the suffix "Internal" has been added.
using GLsyncInternal = void*;

enum class WebGLVersion {
    WebGL1,
    WebGL2,
};

// Maximum drawing-buffer dimension. The client clamps each side to this before requesting
// a size (so drawingBufferWidth/Height report the real buffer), and the host clamps again
// defensively so an out-of-range request can never tear down the connection.
static constexpr int max_webgl_drawing_buffer_dimension = 16384;

// Client-allocated id of a GL object living in a remote WebGL context. The remote side
// maps ids to real GL names; 0 is reserved (unbind, or the default framebuffer and
// renderbuffer where those have bind-0 semantics).
using WebGLObjectId = u32;

// Region of a WebGL command's inline payload; offset is relative to the start of the
// command's payload (the command struct itself sits at offset 0).
struct WebGLDataSpan {
    u32 offset { 0 };
    u32 size { 0 };
};

}
