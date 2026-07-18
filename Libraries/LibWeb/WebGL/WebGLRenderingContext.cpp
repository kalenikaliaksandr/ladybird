/*
 * Copyright (c) 2022, Luke Wilde <lukew@serenityos.org>
 * Copyright (c) 2023, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Runtime/ArrayBuffer.h>
#include <LibJS/Runtime/TypedArray.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/PrincipalHostDefined.h>
#include <LibWeb/Bindings/WebGLContextEvent.h>
#include <LibWeb/Bindings/WebGLRenderingContext.h>
#include <LibWeb/Compositor/CompositorHost.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/EventTarget.h>
#include <LibWeb/HTML/HTMLCanvasElement.h>
#include <LibWeb/HTML/LocalNavigable.h>
#include <LibWeb/HTML/OffscreenCanvas.h>
#include <LibWeb/HTML/Scripting/Environments.h>
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
// Returns false if the event was canceled (the page called preventDefault), which is how
// webglcontextlost signals that the page wants the context restored.
bool fire_webgl_context_event(DOM::EventTarget& canvas, Utf16FlyString const& type)
{
    // To fire a WebGL context event named e means that an event using the WebGLContextEvent interface, with its type attribute [DOM4] initialized to e, its cancelable attribute initialized to true, and its isTrusted attribute [DOM4] initialized to true, is to be dispatched at the given object.
    // FIXME: Consider setting a status message.
    auto event = WebGLContextEvent::create(canvas.realm(), type, Bindings::WebGLContextEventInit {});
    event->set_is_trusted(true);
    event->set_cancelable(true);
    return canvas.dispatch_event(*event);
}

// https://www.khronos.org/registry/webgl/specs/latest/1.0/#fire-a-webgl-context-creation-error
void fire_webgl_context_creation_error(DOM::EventTarget& canvas)
{
    // 1. Fire a WebGL context event named "webglcontextcreationerror" at canvas, optionally with its statusMessage attribute set to a platform dependent string about the nature of the failure.
    fire_webgl_context_event(canvas, EventNames::webglcontextcreationerror);
}

Gfx::IntSize canvas_owner_bitmap_size(HTML::CanvasOwner const& canvas_owner)
{
    return canvas_owner.visit([](auto const& canvas) { return canvas->bitmap_size_for_canvas(); });
}

void dispatch_canvas_content_update(HTML::CanvasOwner const& canvas_owner)
{
    canvas_owner.visit(
        [](GC::Ref<HTML::HTMLCanvasElement> const& canvas_element) {
            canvas_element->set_canvas_content_dirty();

            // NB: Invalidate the cached DrawCanvas command so that if another change causes the display list to be
            //     recorded, it contains the new content generation and damages the canvas. Don't request a display list
            //     recording here: the new content reaches the compositor through the canvas surface registry when the
            //     canvas is presented.
            if (auto paintable = canvas_element->unsafe_paintable())
                paintable->invalidate_paint_cache();
            canvas_element->set_needs_repaint(InvalidateDisplayList::No);
        },
        [](GC::Ref<HTML::OffscreenCanvas> const& offscreen_canvas) {
            // Queues a frame commit towards the placeholder canvas (if any) and schedules a
            // rendering update in Window and dedicated worker realms so it actually happens.
            offscreen_canvas->notify_context_did_draw();
        });
}

// The drawing buffer's creation-time size, clamped like set_size() clamps resizes;
// later resizes travel as SetDrawingBufferSize commands.
static Gfx::IntSize initial_drawing_buffer_size(HTML::CanvasOwner const& canvas_owner)
{
    auto size = canvas_owner.visit(
        [](GC::Ref<HTML::HTMLCanvasElement> const& canvas_element) { return canvas_element->bitmap_size_for_canvas(1, 1); },
        [](GC::Ref<HTML::OffscreenCanvas> const& offscreen_canvas) { return offscreen_canvas->bitmap_size_for_canvas(); });
    return {
        clamp(size.width(), 1, max_webgl_drawing_buffer_dimension),
        clamp(size.height(), 1, max_webgl_drawing_buffer_dimension),
    };
}

namespace {

struct RemoteWebGLContext {
    NonnullRefPtr<RemoteWebGLTransport> transport;
    RemoteWebGLTransport::CreateResult result;
};

Page* page_for_canvas_owner_compositor(HTML::CanvasOwner const& canvas_owner)
{
    return canvas_owner.visit(
        [](GC::Ref<HTML::HTMLCanvasElement> const& canvas_element) -> Page* {
            return &canvas_element->document().page();
        },
        [](GC::Ref<HTML::OffscreenCanvas> const& offscreen_canvas) -> Page* {
            return offscreen_canvas->page_for_compositor();
        });
}

Optional<RemoteWebGLContext> create_remote_webgl_context(HTML::CanvasOwner const& canvas_owner, WebGLVersion webgl_version, WebGLContextAttributes const& context_attributes)
{
    auto* page = page_for_canvas_owner_compositor(canvas_owner);
    if (!page->has_compositor_host())
        return {};
    auto transport = page->compositor_host().create_webgl_transport();
    if (!transport)
        return {};

    // A placeholder-linked OffscreenCanvas claims its pre-allocated compositor canvas id
    // so the placeholder element's DrawCanvas resolves the frames committed here.
    Optional<Painting::OffscreenCanvasPlaceholderLink> placeholder_link;
    if (auto const* offscreen_canvas = canvas_owner.get_pointer<GC::Ref<HTML::OffscreenCanvas>>())
        placeholder_link = (*offscreen_canvas)->placeholder_link();

    auto result = transport->create_context(
        webgl_version,
        initial_drawing_buffer_size(canvas_owner),
        context_attributes.depth,
        context_attributes.stencil,
        context_attributes.antialias,
        placeholder_link);
    if (!result.success)
        return {};

    canvas_owner.visit(
        [](GC::Ref<HTML::HTMLCanvasElement> const& canvas_element) {
            // NB: The display list must be re-recorded so its DrawCanvas command refers to the new remote context's
            //     canvas id. Content updates alone don't invalidate the display list, so do it here.
            canvas_element->set_needs_repaint();
        },
        [](GC::Ref<HTML::OffscreenCanvas> const& offscreen_canvas) {
            // A placeholder-linked canvas commits the fresh (cleared) drawing buffer so the
            // placeholder repopulates after a compositor restoration; without a link this is
            // a no-op.
            offscreen_canvas->notify_context_did_draw();
        });

    return RemoteWebGLContext { transport.release_nonnull(), move(result) };
}

}

OwnPtr<WebGLContextProxy> create_webgl_context_proxy(HTML::CanvasOwner const& canvas_owner, WebGLVersion webgl_version, WebGLContextAttributes const& context_attributes)
{
    auto remote = create_remote_webgl_context(canvas_owner, webgl_version, context_attributes);
    if (!remote.has_value())
        return {};

    return make<WebGLContextProxy>(move(remote->transport), webgl_version, move(remote->result.supported_extensions));
}

bool restore_webgl_context_proxy(WebGLContextProxy& context, HTML::CanvasOwner const& canvas_owner, WebGLVersion webgl_version, WebGLContextAttributes const& context_attributes)
{
    auto remote = create_remote_webgl_context(canvas_owner, webgl_version, context_attributes);
    if (!remote.has_value())
        return false;

    context.restore(move(remote->transport), move(remote->result.supported_extensions));
    return true;
}

JS::ThrowCompletionOr<GC::Ptr<WebGLRenderingContext>> WebGLRenderingContext::create(JS::Realm& realm, HTML::CanvasOwner canvas_owner, JS::Value options)
{
    // We should be coming here from getContext being called on a wrapped <canvas> element or OffscreenCanvas.
    auto context_attributes = TRY(convert_value_to_context_attributes_dictionary(realm.vm(), options));

    auto context = create_webgl_context_proxy(canvas_owner, WebGLVersion::WebGL1, context_attributes);
    if (!context) {
        canvas_owner.visit([](auto const& canvas) { fire_webgl_context_creation_error(*canvas); });
        return GC::Ptr<WebGLRenderingContext> { nullptr };
    }

    return realm.create<WebGLRenderingContext>(realm, move(canvas_owner), context.release_nonnull(), context_attributes, context_attributes);
}

WebGLRenderingContext::WebGLRenderingContext(JS::Realm& realm, HTML::CanvasOwner canvas_owner, NonnullOwnPtr<WebGLContextProxy> context, WebGLContextAttributes context_creation_parameters, WebGLContextAttributes actual_context_parameters)
    : WebGLRenderingContextOverloads(realm, move(context))
    , m_canvas_owner(move(canvas_owner))
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
    visitor.visit(m_canvas_owner);
}

void WebGLRenderingContext::prepare_for_compositing()
{
    context().present_canvas_for_compositing(m_context_creation_parameters.preserve_drawing_buffer);
}

void WebGLRenderingContext::commit_frame_for_placeholder(Gfx::IntSize logical_size)
{
    context().present_canvas_for_compositing(m_context_creation_parameters.preserve_drawing_buffer, logical_size);
}

bool WebGLRenderingContext::reestablish_remote_context()
{
    return restore_webgl_context_proxy(context(), m_canvas_owner, WebGLVersion::WebGL1, m_actual_context_parameters);
}

HTML::CanvasOwner WebGLRenderingContext::canvas_for_binding() const
{
    return m_canvas_owner;
}

void WebGLRenderingContext::did_update_canvas_content()
{
    dispatch_canvas_content_update(m_canvas_owner);
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
    final_size.set_width(clamp(size.width(), 1, max_webgl_drawing_buffer_dimension));
    final_size.set_height(clamp(size.height(), 1, max_webgl_drawing_buffer_dimension));
    context().set_size(final_size);
}

void WebGLRenderingContext::reset_to_default_state()
{
}

WebIDL::Long WebGLRenderingContext::drawing_buffer_width() const
{
    // The actual drawing buffer is clamped to at least 1x1 at creation and
    // resize; a zero-size canvas must report that, not its bitmap dimension.
    auto size = canvas_owner_bitmap_size(m_canvas_owner);
    return clamp(size.width(), 1, max_webgl_drawing_buffer_dimension);
}

WebIDL::Long WebGLRenderingContext::drawing_buffer_height() const
{
    auto size = canvas_owner_bitmap_size(m_canvas_owner);
    return clamp(size.height(), 1, max_webgl_drawing_buffer_dimension);
}

}
