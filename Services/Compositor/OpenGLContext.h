/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/OwnPtr.h>
#include <AK/RefPtr.h>
#include <AK/Vector.h>
#include <LibGfx/Forward.h>
#include <LibGfx/Size.h>
#include <LibWeb/WebGL/GLFunctions.h>

namespace Compositor {

class OpenGLContext : public Web::WebGL::GLFunctions {
public:
    using WebGLVersion = Web::WebGL::WebGLVersion;

    struct DrawingBufferOptions {
        bool alpha;
        bool depth;
        bool stencil;
        bool antialias;
        bool premultiplied_alpha;
    };

    static OwnPtr<OpenGLContext> create(NonnullRefPtr<Gfx::SkiaBackendContext>, WebGLVersion, DrawingBufferOptions);

    void notify_content_will_change();
    void clear_buffer_to_default_values();
    void allocate_painting_surface_if_needed();

    struct Impl;
    OpenGLContext(NonnullRefPtr<Gfx::SkiaBackendContext>, Impl, WebGLVersion, DrawingBufferOptions);

    ~OpenGLContext();

    void make_current();

    void present(bool preserve_drawing_buffer);
    DrawingBufferOptions const& drawing_buffer_options() const { return m_drawing_buffer_options; }

    void set_size(Gfx::IntSize const&);

    RefPtr<Gfx::PaintingSurface> surface();

    u32 default_framebuffer() const;
    u32 default_renderbuffer() const;

    Vector<String> get_supported_opengl_extensions();

    void clear(GLbitfield mask);
    void clear_bufferfv(Web::WebGL::GLenum buffer, Web::WebGL::GLint drawbuffer, GLfloat const* value);
    void clear_bufferiv(Web::WebGL::GLenum buffer, Web::WebGL::GLint drawbuffer, Web::WebGL::GLint const* value);
    void clear_bufferuiv(Web::WebGL::GLenum buffer, Web::WebGL::GLint drawbuffer, Web::WebGL::GLuint const* value);
    void draw_arrays(Web::WebGL::GLenum mode, Web::WebGL::GLint first, Web::WebGL::GLsizei count);
    void draw_arrays_instanced(Web::WebGL::GLenum mode, Web::WebGL::GLint first, Web::WebGL::GLsizei count, Web::WebGL::GLsizei instancecount);
    void draw_arrays_instanced_angle(Web::WebGL::GLenum mode, Web::WebGL::GLint first, Web::WebGL::GLsizei count, Web::WebGL::GLsizei primcount);
    void draw_elements(Web::WebGL::GLenum mode, Web::WebGL::GLsizei count, Web::WebGL::GLenum type, void const* indices);
    void draw_elements_instanced(Web::WebGL::GLenum mode, Web::WebGL::GLsizei count, Web::WebGL::GLenum type, void const* indices, Web::WebGL::GLsizei instancecount);
    void draw_elements_instanced_angle(Web::WebGL::GLenum mode, Web::WebGL::GLsizei count, Web::WebGL::GLenum type, void const* indices, Web::WebGL::GLsizei primcount);
    void draw_range_elements(Web::WebGL::GLenum mode, Web::WebGL::GLuint start, Web::WebGL::GLuint end, Web::WebGL::GLsizei count, Web::WebGL::GLenum type, void const* indices);
    void blit_framebuffer(Web::WebGL::GLint src_x0, Web::WebGL::GLint src_y0, Web::WebGL::GLint src_x1, Web::WebGL::GLint src_y1, Web::WebGL::GLint dst_x0, Web::WebGL::GLint dst_y0, Web::WebGL::GLint dst_x1, Web::WebGL::GLint dst_y1, GLbitfield mask, Web::WebGL::GLenum filter);
    void get_integerv_robust_angle(Web::WebGL::GLenum pname, Web::WebGL::GLsizei buf_size, Web::WebGL::GLsizei* length, Web::WebGL::GLint* data);
    void read_pixels_robust_angle(Web::WebGL::GLint x, Web::WebGL::GLint y, Web::WebGL::GLsizei width, Web::WebGL::GLsizei height, Web::WebGL::GLenum format, Web::WebGL::GLenum type, Web::WebGL::GLsizei buf_size, Web::WebGL::GLsizei* length, Web::WebGL::GLsizei* columns, Web::WebGL::GLsizei* rows, void* pixels);

private:
    NonnullRefPtr<Gfx::SkiaBackendContext> m_skia_backend_context;
    Gfx::IntSize m_size;
    RefPtr<Gfx::PaintingSurface> m_painting_surface;
#ifdef AK_OS_MACOS
    OwnPtr<Gfx::SharedImageBuffer> m_shared_image_buffer;
#endif
    NonnullOwnPtr<Impl> m_impl;
    Optional<Vector<String>> m_requestable_extensions;
    WebGLVersion m_webgl_version;
    [[maybe_unused]] DrawingBufferOptions m_drawing_buffer_options;

    void free_surface_resources();
    bool is_default_draw_framebuffer_bound() const;
    bool is_default_read_framebuffer_bound() const;
    void force_default_framebuffer_alpha_to_one();
#if defined(AK_OS_MACOS)
    void allocate_iosurface_painting_surface();
#elif defined(USE_VULKAN_DMABUF_IMAGES)
    void allocate_vkimage_painting_surface();
#endif
};

}
