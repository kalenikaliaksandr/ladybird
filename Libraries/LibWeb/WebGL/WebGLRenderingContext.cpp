/*
 * Copyright (c) 2022, Luke Wilde <lukew@serenityos.org>
 * Copyright (c) 2023, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Runtime/ArrayBuffer.h>
#include <LibJS/Runtime/TypedArray.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/WebGLContextEvent.h>
#include <LibWeb/Bindings/WebGLRenderingContext.h>
#include <LibWeb/Compositor/CompositorHost.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/HTML/HTMLCanvasElement.h>
#include <LibWeb/HTML/Navigable.h>
#include <LibWeb/Infra/Strings.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/WebGL/EventNames.h>
#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <LibWeb/WebGL/WebGLContextEvent.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>
#include <LibWeb/WebGL/WebGLRenderingContext.h>
#include <LibWeb/WebGL/WebGLShader.h>
#include <LibWeb/WebIDL/Buffers.h>

#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

namespace Web::WebGL {

GC_DEFINE_ALLOCATOR(WebGLRenderingContext);

// https://www.khronos.org/registry/webgl/specs/latest/1.0/#fire-a-webgl-context-event
void fire_webgl_context_event(HTML::HTMLCanvasElement& canvas_element, FlyString const& type)
{
    // To fire a WebGL context event named e means that an event using the WebGLContextEvent interface, with its type attribute [DOM4] initialized to e, its cancelable attribute initialized to true, and its isTrusted attribute [DOM4] initialized to true, is to be dispatched at the given object.
    // FIXME: Consider setting a status message.
    auto event = WebGLContextEvent::create(canvas_element.realm(), type, Bindings::WebGLContextEventInit {});
    event->set_is_trusted(true);
    event->set_cancelable(true);
    canvas_element.dispatch_event(*event);
}

// https://www.khronos.org/registry/webgl/specs/latest/1.0/#fire-a-webgl-context-creation-error
void fire_webgl_context_creation_error(HTML::HTMLCanvasElement& canvas_element)
{
    // 1. Fire a WebGL context event named "webglcontextcreationerror" at canvas, optionally with its statusMessage attribute set to a platform dependent string about the nature of the failure.
    fire_webgl_context_event(canvas_element, EventNames::webglcontextcreationerror);
}

OwnPtr<WebGLContextProxy> create_webgl_context_proxy(HTML::HTMLCanvasElement& canvas_element, WebGLVersion webgl_version, WebGLContextAttributes const& context_attributes)
{
    auto& page = canvas_element.document().page();
    if (!page.has_compositor_host())
        return {};
    auto transport = page.compositor_host().create_webgl_transport();
    if (!transport)
        return {};

    auto webgl_context_id = WebGLContextProxyBase::allocate_webgl_context_id();
    auto result = transport->create_context(
        webgl_context_id,
        webgl_version,
        context_attributes.depth,
        context_attributes.stencil,
        context_attributes.antialias);
    if (!result.success)
        return {};

    return make<WebGLContextProxy>(transport.release_nonnull(), webgl_context_id, webgl_version, move(result.supported_extensions));
}

void present_remote_context(WebGLContextProxy& context, HTML::HTMLCanvasElement& canvas_element, bool preserve_drawing_buffer)
{
    // Without a compositor context (for example mid-teardown) there is no slot to
    // publish into; flush whatever is recorded so the host stays current.
    if (auto navigable = canvas_element.document().navigable(); navigable && navigable->has_compositor_context()) {
        context.present_to_compositor(
            navigable->compositor_context().id(),
            canvas_element.ensure_compositor_surface_id(),
            preserve_drawing_buffer);
        return;
    }
    context.flush_commands();
}

JS::ThrowCompletionOr<GC::Ptr<WebGLRenderingContext>> WebGLRenderingContext::create(JS::Realm& realm, HTML::HTMLCanvasElement& canvas_element, JS::Value options)
{
    // We should be coming here from getContext being called on a wrapped <canvas> element.
    auto context_attributes = TRY(convert_value_to_context_attributes_dictionary(canvas_element.vm(), options));

    auto context = create_webgl_context_proxy(canvas_element, WebGLVersion::WebGL1, context_attributes);
    if (!context) {
        fire_webgl_context_creation_error(canvas_element);
        return GC::Ptr<WebGLRenderingContext> { nullptr };
    }

    context->set_size(canvas_element.bitmap_size_for_canvas(1, 1));

    return realm.create<WebGLRenderingContext>(realm, canvas_element, context.release_nonnull(), context_attributes, context_attributes);
}

WebGLRenderingContext::WebGLRenderingContext(JS::Realm& realm, HTML::HTMLCanvasElement& canvas_element, NonnullOwnPtr<WebGLContextProxy> context, WebGLContextAttributes context_creation_parameters, WebGLContextAttributes actual_context_parameters)
    : WebGLRenderingContextOverloads(realm, move(context))
    , m_canvas_element(canvas_element)
    , m_context_creation_parameters(context_creation_parameters)
    , m_actual_context_parameters(actual_context_parameters)
{
}

WebGLRenderingContext::~WebGLRenderingContext() = default;

void WebGLRenderingContext::initialize(JS::Realm& realm)
{
    WEB_SET_PROTOTYPE_FOR_INTERFACE(WebGLRenderingContext);
    Base::initialize(realm);
}

void WebGLRenderingContext::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    WebGLRenderingContextImpl::visit_edges(visitor);
    visitor.visit(m_canvas_element);
}

void WebGLRenderingContext::present()
{
    present_remote_context(context(), *m_canvas_element, m_context_creation_parameters.preserve_drawing_buffer);
}

GC::Ref<HTML::HTMLCanvasElement> WebGLRenderingContext::canvas_for_binding() const
{
    return *m_canvas_element;
}

void WebGLRenderingContext::needs_to_present()
{
    m_canvas_element->set_canvas_content_dirty();

    m_canvas_element->set_needs_repaint();
}

Optional<WebGLContextAttributes> WebGLRenderingContext::get_context_attributes()
{
    if (is_context_lost())
        return {};
    return m_actual_context_parameters;
}

void WebGLRenderingContext::set_size(Gfx::IntSize const& size)
{
    Gfx::IntSize final_size;
    final_size.set_width(max(size.width(), 1));
    final_size.set_height(max(size.height(), 1));
    context().set_size(final_size);
}

void WebGLRenderingContext::reset_to_default_state()
{
}

RefPtr<Gfx::PaintingSurface> WebGLRenderingContext::surface()
{
    return context().surface();
}

void WebGLRenderingContext::allocate_painting_surface_if_needed()
{
    context().allocate_painting_surface_if_needed();
}

WebIDL::Long WebGLRenderingContext::drawing_buffer_width() const
{
    auto size = canvas_for_binding()->bitmap_size_for_canvas();
    return size.width();
}

WebIDL::Long WebGLRenderingContext::drawing_buffer_height() const
{
    auto size = canvas_for_binding()->bitmap_size_for_canvas();
    return size.height();
}

}
