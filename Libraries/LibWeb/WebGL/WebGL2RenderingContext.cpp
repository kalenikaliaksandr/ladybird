/*
 * Copyright (c) 2022, Luke Wilde <lukew@serenityos.org>
 * Copyright (c) 2024, Andrew Kaster <andrew@ladybird.org>
 * Copyright (c) 2023, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Runtime/ArrayBuffer.h>
#include <LibJS/Runtime/TypedArray.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/WebGL2RenderingContext.h>
#include <LibWeb/HTML/HTMLCanvasElement.h>
#include <LibWeb/HTML/OffscreenCanvas.h>
#include <LibWeb/Infra/Strings.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/WebGL/EventNames.h>
#include <LibWeb/WebGL/WebGL2RenderingContext.h>
#include <LibWeb/WebGL/WebGLContextEvent.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>
#include <LibWeb/WebGL/WebGLRenderingContext.h>
#include <LibWeb/WebGL/WebGLShader.h>
#include <LibWeb/WebIDL/Buffers.h>

#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

namespace Web::WebGL {

GC_DEFINE_ALLOCATOR(WebGL2RenderingContext);

JS::ThrowCompletionOr<GC::Ptr<WebGL2RenderingContext>> WebGL2RenderingContext::create(JS::Realm& realm, HTML::CanvasOwner canvas_owner, JS::Value options)
{
    // We should be coming here from getContext being called on a wrapped <canvas> element or OffscreenCanvas.
    auto context_attributes = TRY(convert_value_to_context_attributes_dictionary(realm.vm(), options));

    auto context = create_webgl_context_proxy(canvas_owner, WebGLVersion::WebGL2, context_attributes);
    if (!context) {
        canvas_owner.visit([](auto const& canvas) { fire_webgl_context_creation_error(*canvas); });
        return GC::Ptr<WebGL2RenderingContext> { nullptr };
    }

    return realm.create<WebGL2RenderingContext>(realm, move(canvas_owner), context.release_nonnull(), context_attributes, context_attributes);
}

WebGL2RenderingContext::WebGL2RenderingContext(JS::Realm& realm, HTML::CanvasOwner canvas_owner, NonnullOwnPtr<WebGLContextProxy> context, WebGLContextAttributes context_creation_parameters, WebGLContextAttributes actual_context_parameters)
    : WebGL2RenderingContextOverloads(realm, move(context))
    , m_canvas_owner(move(canvas_owner))
    , m_context_creation_parameters(context_creation_parameters)
    , m_actual_context_parameters(actual_context_parameters)
{
}

WebGL2RenderingContext::~WebGL2RenderingContext() = default;

void WebGL2RenderingContext::initialize(JS::Realm& realm)
{
    WEB_SET_PROTOTYPE_FOR_INTERFACE(WebGL2RenderingContext);
    Base::initialize(realm);
}

void WebGL2RenderingContext::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    WebGL2RenderingContextImpl::visit_edges(visitor);
    visitor.visit(m_canvas_owner);
}

void WebGL2RenderingContext::prepare_for_compositing()
{
    context().present_canvas_for_compositing(m_context_creation_parameters.preserve_drawing_buffer);
}

void WebGL2RenderingContext::commit_frame_for_placeholder(Gfx::IntSize logical_size)
{
    context().present_canvas_for_compositing(m_context_creation_parameters.preserve_drawing_buffer, logical_size);
}

bool WebGL2RenderingContext::reestablish_remote_context()
{
    return restore_webgl_context_proxy(context(), m_canvas_owner, WebGLVersion::WebGL2, m_actual_context_parameters);
}

HTML::CanvasOwner WebGL2RenderingContext::canvas_for_binding() const
{
    return m_canvas_owner;
}

void WebGL2RenderingContext::did_update_canvas_content()
{
    dispatch_canvas_content_update(m_canvas_owner);
}

Optional<WebGLContextAttributes> WebGL2RenderingContext::get_context_attributes()
{
    if (is_context_lost())
        return {};
    return m_actual_context_parameters;
}

void WebGL2RenderingContext::set_size(Gfx::IntSize const& size)
{
    Gfx::IntSize final_size;
    final_size.set_width(clamp(size.width(), 1, max_webgl_drawing_buffer_dimension));
    final_size.set_height(clamp(size.height(), 1, max_webgl_drawing_buffer_dimension));
    context().set_size(final_size);
}

void WebGL2RenderingContext::reset_to_default_state()
{
}

WebIDL::Long WebGL2RenderingContext::drawing_buffer_width() const
{
    // The actual drawing buffer is clamped to at least 1x1 at creation and
    // resize; a zero-size canvas must report that, not its bitmap dimension.
    auto size = canvas_owner_bitmap_size(m_canvas_owner);
    return clamp(size.width(), 1, max_webgl_drawing_buffer_dimension);
}

WebIDL::Long WebGL2RenderingContext::drawing_buffer_height() const
{
    auto size = canvas_owner_bitmap_size(m_canvas_owner);
    return clamp(size.height(), 1, max_webgl_drawing_buffer_dimension);
}

}
