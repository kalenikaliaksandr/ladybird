/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Define GL_GLEXT_PROTOTYPES before OpenGLContext.h pulls in the GL headers without it.
#define GL_GLEXT_PROTOTYPES 1
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
extern "C" {
#include <GLES2/gl2ext_angle.h>
}
#include <GLES3/gl3.h>

#include <EGL/egl.h>
#include <EGL/eglext.h>
#define EGL_EGLEXT_PROTOTYPES 1
extern "C" {
#include <EGL/eglext_angle.h>
}

#include <AK/HashMap.h>
#include <AK/OwnPtr.h>
#include <AK/String.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SharedImageBuffer.h>
#ifdef USE_VULKAN_DMABUF_IMAGES
#    include <LibGfx/VulkanImage.h>
#endif
#include <Compositor/OpenGLContext.h>

// Enable WebGL if we're on MacOS and can use Metal or if we can use shareable Vulkan images
#if defined(AK_OS_MACOS) || defined(USE_VULKAN_DMABUF_IMAGES)
#    define ENABLE_WEBGL 1
#endif

namespace Compositor {

using namespace Web::WebGL;

struct OpenGLContext::Impl {
    EGLDisplay display { EGL_NO_DISPLAY };
    EGLConfig config { EGL_NO_CONFIG_KHR };
    EGLContext context { EGL_NO_CONTEXT };
    EGLSurface surface { EGL_NO_SURFACE };

    GLuint framebuffer { 0 };
    GLuint color_buffer { 0 };
    GLuint depth_buffer { 0 };
    EGLint texture_target { 0 };

#ifdef USE_VULKAN_DMABUF_IMAGES
    EGLImage egl_image { EGL_NO_IMAGE };
    struct {
        PFNEGLQUERYDMABUFFORMATSEXTPROC query_dma_buf_formats { nullptr };
        PFNEGLQUERYDMABUFMODIFIERSEXTPROC query_dma_buf_modifiers { nullptr };
    } ext_procs;
#endif
};

OpenGLContext::OpenGLContext(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, Impl impl, WebGLVersion webgl_version, DrawingBufferOptions drawing_buffer_options)
    : m_skia_backend_context(move(skia_backend_context))
    , m_impl(make<Impl>(impl))
    , m_webgl_version(webgl_version)
    , m_drawing_buffer_options(drawing_buffer_options)
{
}

OpenGLContext::~OpenGLContext()
{
#ifdef ENABLE_WEBGL
    free_surface_resources();
    eglMakeCurrent(m_impl->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(m_impl->display, m_impl->context);
#endif
}

void OpenGLContext::free_surface_resources()
{
#ifdef ENABLE_WEBGL
    eglMakeCurrent(m_impl->display, EGL_NO_SURFACE, EGL_NO_SURFACE, m_impl->context);

    if (m_impl->framebuffer) {
        glDeleteFramebuffers(1, &m_impl->framebuffer);
        m_impl->framebuffer = 0;
    }

    if (m_impl->color_buffer) {
        glDeleteTextures(1, &m_impl->color_buffer);
        m_impl->color_buffer = 0;
    }

    if (m_impl->depth_buffer) {
        glDeleteRenderbuffers(1, &m_impl->depth_buffer);
        m_impl->depth_buffer = 0;
    }

#    ifdef USE_VULKAN_DMABUF_IMAGES
    if (m_impl->egl_image != EGL_NO_IMAGE) {
        eglDestroyImage(m_impl->display, m_impl->egl_image);
        m_impl->egl_image = EGL_NO_IMAGE;
    }
#    endif

    if (m_impl->surface != EGL_NO_SURFACE) {
#    ifdef AK_OS_MACOS
        eglReleaseTexImage(m_impl->display, m_impl->surface, EGL_BACK_BUFFER);
#    endif
        eglDestroySurface(m_impl->display, m_impl->surface);
        m_impl->surface = EGL_NO_SURFACE;
    }
#endif
}

#ifdef ENABLE_WEBGL
static EGLConfig get_egl_config(EGLDisplay display)
{
    EGLint const config_attribs[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8,
        EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, 8,
        EGL_DEPTH_SIZE, 24,
        EGL_STENCIL_SIZE, 8,
        EGL_NONE
    };

    EGLint number_of_configs;
    eglChooseConfig(display, config_attribs, NULL, 0, &number_of_configs);

    Vector<EGLConfig> configs;
    configs.resize(number_of_configs);
    eglChooseConfig(display, config_attribs, configs.data(), number_of_configs, &number_of_configs);
    return number_of_configs > 0 ? configs[0] : EGL_NO_CONFIG_KHR;
}
#endif

OwnPtr<OpenGLContext> OpenGLContext::create(NonnullRefPtr<Gfx::SkiaBackendContext> skia_backend_context, WebGLVersion webgl_version, [[maybe_unused]] DrawingBufferOptions drawing_buffer_options)
{
#ifdef ENABLE_WEBGL
    EGLAttrib display_attributes[] = {
        EGL_PLATFORM_ANGLE_TYPE_ANGLE,
#    if defined(AK_OS_MACOS)
        EGL_PLATFORM_ANGLE_TYPE_METAL_ANGLE,
#    elif defined(USE_VULKAN_DMABUF_IMAGES)
        EGL_PLATFORM_ANGLE_TYPE_OPENGL_ANGLE,
        EGL_PLATFORM_ANGLE_NATIVE_PLATFORM_TYPE_ANGLE,
        EGL_PLATFORM_SURFACELESS_MESA,
#    endif
        EGL_NONE,
    };

    auto display = eglGetPlatformDisplay(EGL_PLATFORM_ANGLE_ANGLE, reinterpret_cast<void*>(EGL_DEFAULT_DISPLAY), display_attributes);
    if (display == EGL_NO_DISPLAY) {
        dbgln("Failed to get EGL display");
        return {};
    }

    EGLint major, minor;
    if (!eglInitialize(display, &major, &minor)) {
        dbgln("Failed to initialize EGL");
        return {};
    }

    auto* config = get_egl_config(display);
    if (config == EGL_NO_CONFIG_KHR) {
        dbgln("Failed to find EGLConfig");
        return {};
    }

    EGLint texture_target;
#    if defined(AK_OS_MACOS)
    eglGetConfigAttrib(display, config, EGL_BIND_TO_TEXTURE_TARGET_ANGLE, &texture_target);
    VERIFY(texture_target == EGL_TEXTURE_RECTANGLE_ANGLE || texture_target == EGL_TEXTURE_2D);
#    elif defined(USE_VULKAN_DMABUF_IMAGES)
    texture_target = EGL_TEXTURE_2D;
#    endif

    EGLint context_attributes[] = {
        EGL_CONTEXT_CLIENT_VERSION,
        webgl_version == WebGLVersion::WebGL1 ? 2 : 3,
        EGL_CONTEXT_WEBGL_COMPATIBILITY_ANGLE,
        EGL_TRUE,
        EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE,
        EGL_TRUE,
        EGL_CONTEXT_OPENGL_BACKWARDS_COMPATIBLE_ANGLE,
        EGL_FALSE,
#    ifdef USE_VULKAN_DMABUF_IMAGES
        // we need GL_OES_EGL_image
        EGL_EXTENSIONS_ENABLED_ANGLE,
        EGL_TRUE,
#    endif
        EGL_NONE,
        EGL_NONE,
    };
    auto context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
    if (context == EGL_NO_CONTEXT) {
        dbgln("Failed to create EGL context");
        return {};
    }

#    ifdef USE_VULKAN_DMABUF_IMAGES
    auto pfn_egl_query_dma_buf_formats_ext = reinterpret_cast<PFNEGLQUERYDMABUFFORMATSEXTPROC>(eglGetProcAddress("eglQueryDmaBufFormatsEXT"));
    if (!pfn_egl_query_dma_buf_formats_ext) {
        dbgln("eglQueryDmaBufFormatsEXT unavailable");
        return {};
    }

    auto pfn_egl_query_dma_buf_modifiers_ext = reinterpret_cast<PFNEGLQUERYDMABUFMODIFIERSEXTPROC>(eglGetProcAddress("eglQueryDmaBufModifiersEXT"));
    if (!pfn_egl_query_dma_buf_modifiers_ext) {
        dbgln("eglQueryDmaBufModifiersEXT unavailable");
        return {};
    }
#    endif

    return make<OpenGLContext>(skia_backend_context, Impl {
                                                         .display = display,
                                                         .config = config,
                                                         .context = context,
                                                         .texture_target = texture_target,
#    ifdef USE_VULKAN_DMABUF_IMAGES
                                                         .ext_procs = {
                                                             .query_dma_buf_formats = pfn_egl_query_dma_buf_formats_ext,
                                                             .query_dma_buf_modifiers = pfn_egl_query_dma_buf_modifiers_ext,
                                                         },
#    endif
                                                     },
        webgl_version, drawing_buffer_options);
#else
    (void)skia_backend_context;
    (void)webgl_version;
    return {};
#endif
}

void OpenGLContext::notify_content_will_change()
{
#ifdef ENABLE_WEBGL
    m_painting_surface->notify_content_will_change();
#endif
}

void OpenGLContext::clear_buffer_to_default_values()
{
#ifdef ENABLE_WEBGL
    GLint original_framebuffer;
    GLint original_renderbuffer;
    GLenum framebuffer_target = GL_FRAMEBUFFER;
    GLenum framebuffer_binding = GL_FRAMEBUFFER_BINDING;
    if (m_webgl_version == WebGLVersion::WebGL2) {
        framebuffer_target = GL_DRAW_FRAMEBUFFER;
        framebuffer_binding = GL_DRAW_FRAMEBUFFER_BINDING;
    }
    glGetIntegerv(framebuffer_binding, &original_framebuffer);
    glGetIntegerv(GL_RENDERBUFFER_BINDING, &original_renderbuffer);

    glBindFramebuffer(framebuffer_target, default_framebuffer());
    glBindRenderbuffer(GL_RENDERBUFFER, default_renderbuffer());

    Array<GLfloat, 4> current_clear_color;
    glGetFloatv(GL_COLOR_CLEAR_VALUE, current_clear_color.data());

    Array<GLboolean, 4> current_color_mask;
    glGetBooleanv(GL_COLOR_WRITEMASK, current_color_mask.data());

    GLfloat current_clear_depth;
    glGetFloatv(GL_DEPTH_CLEAR_VALUE, &current_clear_depth);

    GLint current_clear_stencil;
    glGetIntegerv(GL_STENCIL_CLEAR_VALUE, &current_clear_stencil);

    GLboolean was_scissor_enabled = glIsEnabled(GL_SCISSOR_TEST);
    if (was_scissor_enabled)
        glDisable(GL_SCISSOR_TEST);

    // The implicit clear value for the color buffer is transparent black
    // unless the context was created without alpha.
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
    glClearColor(0, 0, 0, m_drawing_buffer_options.alpha ? 0 : 1);

    // The implicit clear value for the depth buffer is 1.0.
    glClearDepthf(1.0f);

    // The implicit clear value for the stencil buffer is 0.
    glClearStencil(0);

    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);

    // Restore the clear values.
    glClearColor(current_clear_color[0], current_clear_color[1], current_clear_color[2], current_clear_color[3]);
    glColorMask(current_color_mask[0], current_color_mask[1], current_color_mask[2], current_color_mask[3]);
    glClearDepthf(current_clear_depth);
    glClearStencil(current_clear_stencil);
    if (was_scissor_enabled)
        glEnable(GL_SCISSOR_TEST);

    glBindFramebuffer(framebuffer_target, original_framebuffer);
    glBindRenderbuffer(GL_RENDERBUFFER, original_renderbuffer);
#endif
}

bool OpenGLContext::is_default_draw_framebuffer_bound() const
{
#ifdef ENABLE_WEBGL
    if (!m_impl->framebuffer)
        return false;

    GLenum binding = GL_FRAMEBUFFER_BINDING;
    if (m_webgl_version == WebGLVersion::WebGL2)
        binding = GL_DRAW_FRAMEBUFFER_BINDING;

    GLint framebuffer = 0;
    glGetIntegerv(binding, &framebuffer);
    return static_cast<GLuint>(framebuffer) == m_impl->framebuffer;
#else
    return false;
#endif
}

bool OpenGLContext::is_default_read_framebuffer_bound() const
{
#ifdef ENABLE_WEBGL
    if (!m_impl->framebuffer)
        return false;

    GLenum binding = GL_FRAMEBUFFER_BINDING;
    if (m_webgl_version == WebGLVersion::WebGL2)
        binding = GL_READ_FRAMEBUFFER_BINDING;

    GLint framebuffer = 0;
    glGetIntegerv(binding, &framebuffer);
    return static_cast<GLuint>(framebuffer) == m_impl->framebuffer;
#else
    return false;
#endif
}

void OpenGLContext::force_default_framebuffer_alpha_to_one()
{
#ifdef ENABLE_WEBGL
    if (m_drawing_buffer_options.alpha || !is_default_draw_framebuffer_bound())
        return;

    Array<GLfloat, 4> current_clear_color;
    glGetFloatv(GL_COLOR_CLEAR_VALUE, current_clear_color.data());

    Array<GLboolean, 4> current_color_mask;
    glGetBooleanv(GL_COLOR_WRITEMASK, current_color_mask.data());

    GLboolean was_scissor_enabled = glIsEnabled(GL_SCISSOR_TEST);
    if (was_scissor_enabled)
        glDisable(GL_SCISSOR_TEST);

    glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_TRUE);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);

    glClearColor(current_clear_color[0], current_clear_color[1], current_clear_color[2], current_clear_color[3]);
    glColorMask(current_color_mask[0], current_color_mask[1], current_color_mask[2], current_color_mask[3]);
    if (was_scissor_enabled)
        glEnable(GL_SCISSOR_TEST);
#endif
}

#ifdef AK_OS_MACOS
void OpenGLContext::allocate_iosurface_painting_surface()
{
    m_shared_image_buffer = make<Gfx::SharedImageBuffer>(Gfx::SharedImageBuffer::create(m_size));
    m_painting_surface = Gfx::PaintingSurface::create_from_shared_image_buffer(*m_shared_image_buffer, m_skia_backend_context, Gfx::PaintingSurface::Origin::BottomLeft);

    EGLint const surface_attributes[] = {
        EGL_WIDTH,
        m_size.width(),
        EGL_HEIGHT,
        m_size.height(),
        EGL_IOSURFACE_PLANE_ANGLE,
        0,
        EGL_TEXTURE_TARGET,
        m_impl->texture_target,
        EGL_TEXTURE_INTERNAL_FORMAT_ANGLE,
        GL_BGRA_EXT,
        EGL_TEXTURE_FORMAT,
        EGL_TEXTURE_RGBA,
        EGL_TEXTURE_TYPE_ANGLE,
        GL_UNSIGNED_BYTE,
        EGL_NONE,
        EGL_NONE,
    };
    m_impl->surface = eglCreatePbufferFromClientBuffer(m_impl->display, EGL_IOSURFACE_ANGLE, m_shared_image_buffer->iosurface_handle().core_foundation_pointer(), m_impl->config, surface_attributes);

    eglMakeCurrent(m_impl->display, EGL_NO_SURFACE, EGL_NO_SURFACE, m_impl->context);

    glGenTextures(1, &m_impl->color_buffer);
    glBindTexture(m_impl->texture_target == EGL_TEXTURE_RECTANGLE_ANGLE ? GL_TEXTURE_RECTANGLE_ANGLE : GL_TEXTURE_2D, m_impl->color_buffer);
    auto result = eglBindTexImage(m_impl->display, m_impl->surface, EGL_BACK_BUFFER);
    VERIFY(result == EGL_TRUE);

    glViewport(0, 0, m_size.width(), m_size.height());
}
#endif

#ifdef USE_VULKAN_DMABUF_IMAGES
void OpenGLContext::allocate_vkimage_painting_surface()
{
    VkFormat vulkan_format = VK_FORMAT_B8G8R8A8_UNORM;
    uint32_t drm_format = Gfx::vk_format_to_drm_format(vulkan_format);

    // Ensure that our format is supported by the implementation.
    // FIXME: try other formats if not?
    EGLint num_formats = 0;
    m_impl->ext_procs.query_dma_buf_formats(m_impl->display, 0, nullptr, &num_formats);
    Vector<EGLint> egl_formats;
    egl_formats.resize(num_formats);
    m_impl->ext_procs.query_dma_buf_formats(m_impl->display, num_formats, egl_formats.data(), &num_formats);
    VERIFY(egl_formats.find(drm_format) != egl_formats.end());

    EGLint num_modifiers = 0;
    m_impl->ext_procs.query_dma_buf_modifiers(m_impl->display, drm_format, 0, nullptr, nullptr, &num_modifiers);
    Vector<uint64_t> egl_modifiers;
    egl_modifiers.resize(num_modifiers);
    Vector<EGLBoolean> external_only;
    external_only.resize(num_modifiers);
    m_impl->ext_procs.query_dma_buf_modifiers(m_impl->display, drm_format, num_modifiers, egl_modifiers.data(), external_only.data(), &num_modifiers);
    Vector<uint64_t> renderable_modifiers;
    for (int i = 0; i < num_modifiers; ++i) {
        if (!external_only[i]) {
            renderable_modifiers.append(egl_modifiers[i]);
        }
    }

    auto vulkan_image = MUST(Gfx::create_shared_vulkan_image(m_skia_backend_context->vulkan_context(), m_size.width(), m_size.height(), vulkan_format, renderable_modifiers));
    m_painting_surface = Gfx::PaintingSurface::create_from_vkimage(m_skia_backend_context, vulkan_image, Gfx::PaintingSurface::Origin::BottomLeft);

    EGLAttrib attribs[] = {
        EGL_WIDTH,
        m_size.width(),
        EGL_HEIGHT,
        m_size.height(),
        EGL_LINUX_DRM_FOURCC_EXT,
        drm_format,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        vulkan_image->get_dma_buf_fd(), // EGL takes ownership of the fd
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        static_cast<uint32_t>(vulkan_image->info.row_pitch),
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
        static_cast<uint32_t>(vulkan_image->info.modifier & 0xffffffff),
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
        static_cast<uint32_t>(vulkan_image->info.modifier >> 32),
        EGL_NONE,
    };
    m_impl->egl_image = eglCreateImage(m_impl->display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, nullptr, attribs);
    VERIFY(m_impl->egl_image != EGL_NO_IMAGE);

    m_impl->surface = EGL_NO_SURFACE;
    eglMakeCurrent(m_impl->display, m_impl->surface, m_impl->surface, m_impl->context);

    glGenTextures(1, &m_impl->color_buffer);
    glBindTexture(GL_TEXTURE_2D, m_impl->color_buffer);
    glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, m_impl->egl_image);

    glViewport(0, 0, m_size.width(), m_size.height());
}
#endif

void OpenGLContext::allocate_painting_surface_if_needed()
{
#ifdef ENABLE_WEBGL
    if (m_painting_surface)
        return;

    free_surface_resources();

    VERIFY(!m_size.is_empty());

#    if defined(AK_OS_MACOS)
    allocate_iosurface_painting_surface();
#    elif defined(USE_VULKAN_DMABUF_IMAGES)
    allocate_vkimage_painting_surface();
#    endif
    VERIFY(m_painting_surface);
    VERIFY(eglGetCurrentContext() == m_impl->context);

    glGenFramebuffers(1, &m_impl->framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, m_impl->framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, m_impl->texture_target == EGL_TEXTURE_RECTANGLE_ANGLE ? GL_TEXTURE_RECTANGLE_ANGLE : GL_TEXTURE_2D, m_impl->color_buffer, 0);

    if (m_drawing_buffer_options.depth || m_drawing_buffer_options.stencil) {
        glGenRenderbuffers(1, &m_impl->depth_buffer);
        glBindRenderbuffer(GL_RENDERBUFFER, m_impl->depth_buffer);

        if (m_drawing_buffer_options.depth && m_drawing_buffer_options.stencil) {
            glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH24_STENCIL8, m_size.width(), m_size.height());
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT, GL_RENDERBUFFER, m_impl->depth_buffer);
        } else if (m_drawing_buffer_options.depth) {
            glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH_COMPONENT24, m_size.width(), m_size.height());
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, m_impl->depth_buffer);
        } else {
            VERIFY(m_drawing_buffer_options.stencil);
            glRenderbufferStorage(GL_RENDERBUFFER, GL_STENCIL_INDEX8, m_size.width(), m_size.height());
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_STENCIL_ATTACHMENT, GL_RENDERBUFFER, m_impl->depth_buffer);
        }
    }

    VERIFY(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE);

    if (!m_initialized_default_scissor_box) {
        glScissor(0, 0, m_size.width(), m_size.height());
        m_initialized_default_scissor_box = true;
    }
#endif
}

void OpenGLContext::set_size(Gfx::IntSize const& size)
{
    if (m_size != size) {
        m_painting_surface = nullptr;
    }
    m_size = size;
}

void OpenGLContext::make_current()
{
#ifdef ENABLE_WEBGL
    allocate_painting_surface_if_needed();
    eglMakeCurrent(m_impl->display, EGL_NO_SURFACE, EGL_NO_SURFACE, m_impl->context);
#endif
}

void OpenGLContext::clear(GLbitfield mask)
{
#ifdef ENABLE_WEBGL
    GLFunctions::clear(mask);
    if (mask & GL_COLOR_BUFFER_BIT)
        force_default_framebuffer_alpha_to_one();
#else
    (void)mask;
#endif
}

void OpenGLContext::clear_bufferfv(GLenum buffer, GLint drawbuffer, GLfloat const* value)
{
#ifdef ENABLE_WEBGL
    GLFunctions::clear_bufferfv(buffer, drawbuffer, value);
    if (buffer == GL_COLOR)
        force_default_framebuffer_alpha_to_one();
#else
    (void)buffer;
    (void)drawbuffer;
    (void)value;
#endif
}

void OpenGLContext::clear_bufferiv(GLenum buffer, GLint drawbuffer, GLint const* value)
{
#ifdef ENABLE_WEBGL
    GLFunctions::clear_bufferiv(buffer, drawbuffer, value);
    if (buffer == GL_COLOR)
        force_default_framebuffer_alpha_to_one();
#else
    (void)buffer;
    (void)drawbuffer;
    (void)value;
#endif
}

void OpenGLContext::clear_bufferuiv(GLenum buffer, GLint drawbuffer, GLuint const* value)
{
#ifdef ENABLE_WEBGL
    GLFunctions::clear_bufferuiv(buffer, drawbuffer, value);
    if (buffer == GL_COLOR)
        force_default_framebuffer_alpha_to_one();
#else
    (void)buffer;
    (void)drawbuffer;
    (void)value;
#endif
}

void OpenGLContext::draw_arrays(GLenum mode, GLint first, GLsizei count)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_arrays(mode, first, count);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)first;
    (void)count;
#endif
}

void OpenGLContext::draw_arrays_instanced(GLenum mode, GLint first, GLsizei count, GLsizei instancecount)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_arrays_instanced(mode, first, count, instancecount);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)first;
    (void)count;
    (void)instancecount;
#endif
}

void OpenGLContext::draw_arrays_instanced_angle(GLenum mode, GLint first, GLsizei count, GLsizei primcount)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_arrays_instanced_angle(mode, first, count, primcount);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)first;
    (void)count;
    (void)primcount;
#endif
}

void OpenGLContext::draw_elements(GLenum mode, GLsizei count, GLenum type, void const* indices)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_elements(mode, count, type, indices);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)count;
    (void)type;
    (void)indices;
#endif
}

void OpenGLContext::draw_elements_instanced(GLenum mode, GLsizei count, GLenum type, void const* indices, GLsizei instancecount)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_elements_instanced(mode, count, type, indices, instancecount);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)count;
    (void)type;
    (void)indices;
    (void)instancecount;
#endif
}

void OpenGLContext::draw_elements_instanced_angle(GLenum mode, GLsizei count, GLenum type, void const* indices, GLsizei primcount)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_elements_instanced_angle(mode, count, type, indices, primcount);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)count;
    (void)type;
    (void)indices;
    (void)primcount;
#endif
}

void OpenGLContext::draw_range_elements(GLenum mode, GLuint start, GLuint end, GLsizei count, GLenum type, void const* indices)
{
#ifdef ENABLE_WEBGL
    GLFunctions::draw_range_elements(mode, start, end, count, type, indices);
    force_default_framebuffer_alpha_to_one();
#else
    (void)mode;
    (void)start;
    (void)end;
    (void)count;
    (void)type;
    (void)indices;
#endif
}

void OpenGLContext::blit_framebuffer(GLint src_x0, GLint src_y0, GLint src_x1, GLint src_y1, GLint dst_x0, GLint dst_y0, GLint dst_x1, GLint dst_y1, GLbitfield mask, GLenum filter)
{
#ifdef ENABLE_WEBGL
    GLFunctions::blit_framebuffer(src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter);
    if (mask & GL_COLOR_BUFFER_BIT)
        force_default_framebuffer_alpha_to_one();
#else
    (void)src_x0;
    (void)src_y0;
    (void)src_x1;
    (void)src_y1;
    (void)dst_x0;
    (void)dst_y0;
    (void)dst_x1;
    (void)dst_y1;
    (void)mask;
    (void)filter;
#endif
}

void OpenGLContext::get_integerv_robust_angle(GLenum pname, GLsizei buf_size, GLsizei* length, GLint* data)
{
#ifdef ENABLE_WEBGL
    if (pname == GL_ALPHA_BITS && !m_drawing_buffer_options.alpha && is_default_read_framebuffer_bound()) {
        if (length)
            *length = 1;
        if (data && buf_size > 0)
            data[0] = 0;
        return;
    }

    GLFunctions::get_integerv_robust_angle(pname, buf_size, length, data);
#else
    (void)pname;
    (void)buf_size;
    (void)length;
    (void)data;
#endif
}

void OpenGLContext::read_pixels_robust_angle(GLint x, GLint y, GLsizei width, GLsizei height, GLenum format, GLenum type, GLsizei buf_size, GLsizei* length, GLsizei* columns, GLsizei* rows, void* pixels)
{
#ifdef ENABLE_WEBGL
    auto should_force_alpha = !m_drawing_buffer_options.alpha && is_default_read_framebuffer_bound();
    if (should_force_alpha)
        force_default_framebuffer_alpha_to_one();

    GLFunctions::read_pixels_robust_angle(x, y, width, height, format, type, buf_size, length, columns, rows, pixels);

    if (should_force_alpha && format == GL_RGBA && type == GL_UNSIGNED_BYTE && pixels && length) {
        auto* pixel_bytes = static_cast<u8*>(pixels);
        for (GLsizei i = 3; i < *length; i += 4)
            pixel_bytes[i] = 255;
    }
#else
    (void)x;
    (void)y;
    (void)width;
    (void)height;
    (void)format;
    (void)type;
    (void)buf_size;
    (void)length;
    (void)columns;
    (void)rows;
    (void)pixels;
#endif
}

void OpenGLContext::present(bool preserve_drawing_buffer)
{
#ifdef ENABLE_WEBGL
    make_current();
    force_default_framebuffer_alpha_to_one();

    // "Before the drawing buffer is presented for compositing the implementation shall ensure that all rendering operations have been flushed to the drawing buffer."
    // With Metal, glFlush flushes the command buffer, but without waiting for it to be scheduled or completed.
    // eglWaitUntilWorkScheduledANGLE flushes the command buffer, and waits until it has been scheduled, hence the name.
    // eglWaitUntilWorkScheduledANGLE only has an effect on CGL and Metal backends, so we only use it on macOS.
#    if defined(AK_OS_MACOS)
    eglWaitUntilWorkScheduledANGLE(m_impl->display);
#    elif defined(USE_VULKAN_DMABUF_IMAGES)
    // FIXME: CPU sync for now, but it would be better to export a fence and have Skia wait for it before reading from the surface
    glFinish();
#    endif

    // "By default, after compositing the contents of the drawing buffer shall be cleared to their default values, as shown in the table above.
    // This default behavior can be changed by setting the preserveDrawingBuffer attribute of the WebGLContextAttributes object.
    // If this flag is true, the contents of the drawing buffer shall be preserved until the author either clears or overwrites them."
    if (!preserve_drawing_buffer) {
        // FIXME: we're assuming the clear operation won't actually be submitted to the GPU
        clear_buffer_to_default_values();
    }
#else
    (void)preserve_drawing_buffer;
#endif
}

RefPtr<Gfx::PaintingSurface> OpenGLContext::surface()
{
    return m_painting_surface;
}

u32 OpenGLContext::default_renderbuffer() const
{
    return m_impl->depth_buffer;
}

u32 OpenGLContext::default_framebuffer() const
{
    return m_impl->framebuffer;
}

Vector<String> OpenGLContext::get_supported_opengl_extensions()
{
#ifdef ENABLE_WEBGL
    if (m_requestable_extensions.has_value())
        return m_requestable_extensions.value();

    make_current();

    Vector<String> extensions;

    auto const* extensions_string = reinterpret_cast<char const*>(glGetString(GL_EXTENSIONS));
    StringView extensions_view(extensions_string, strlen(extensions_string));
    for (auto extension : extensions_view.split_view(' ')) {
        extensions.append(MUST(String::from_utf8(extension)));
    }

    auto const* requestable_extensions_string = reinterpret_cast<char const*>(glGetString(GL_REQUESTABLE_EXTENSIONS_ANGLE));
    StringView requestable_extensions_view(requestable_extensions_string, strlen(requestable_extensions_string));
    for (auto extension : requestable_extensions_view.split_view(' ')) {
        extensions.append(MUST(String::from_utf8(extension)));
    }

    // We must cache this, because once extensions have been requested, they're no longer requestable extensions and would
    // not appear in this list. However, we must always report every supported extension, regardless of what has already
    // been requested.
    m_requestable_extensions = extensions;
    return extensions;
#else
    (void)m_webgl_version;
    return {};
#endif
}

}
