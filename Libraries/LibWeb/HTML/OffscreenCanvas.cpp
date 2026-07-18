/*
 * Copyright (c) 2025-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Checked.h>
#include <AK/NumericLimits.h>
#include <AK/Tuple.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/CanvasCommandList.h>
#include <LibWeb/Bindings/OffscreenCanvas.h>
#include <LibWeb/Bindings/PrincipalHostDefined.h>
#include <LibWeb/CSS/ComputedValues.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/HTML/Canvas/SerializeBitmap.h>
#include <LibWeb/HTML/OffscreenCanvas.h>
#include <LibWeb/HTML/OffscreenCanvasRenderingContext2D.h>
#include <LibWeb/HTML/Scripting/TemporaryExecutionContext.h>
#include <LibWeb/HTML/Window.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/HTML/WorkerGlobalScope.h>
#include <LibWeb/Platform/EventLoopPlugin.h>
#include <LibWeb/Platform/FontPlugin.h>
#include <LibWeb/WebGL/WebGL2RenderingContext.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>
#include <LibWeb/WebGL/WebGLRenderingContext.h>

namespace Web::HTML {

GC_DEFINE_ALLOCATOR(OffscreenCanvas);

// Bitmap-facing sizes are ints; the logical unsigned long long dimensions are
// preserved as-is and only clamped where a Gfx size is needed. Drawing to a
// canvas whose area exceeds the maximum still no-ops through bitmap_size_for_canvas().
static int clamp_canvas_dimension(WebIDL::UnsignedLongLong dimension)
{
    return static_cast<int>(min<WebIDL::UnsignedLongLong>(dimension, NumericLimits<int>::max()));
}

GC::Ref<OffscreenCanvas> OffscreenCanvas::create(JS::Realm& realm, WebIDL::UnsignedLongLong width,
    WebIDL::UnsignedLongLong height)
{
    return MUST(OffscreenCanvas::construct_impl(realm, width, height));
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas
WebIDL::ExceptionOr<GC::Ref<OffscreenCanvas>> OffscreenCanvas::construct_impl(
    JS::Realm& realm,
    WebIDL::UnsignedLongLong width,
    WebIDL::UnsignedLongLong height)
{
    // The new OffscreenCanvas(width, height) constructor steps are:

    // 1. Initialize the bitmap of this to a rectangular array of transparent black pixels of the dimensions specified by width and height.
    // NOTE: The bitmap is allocated lazily in the Compositor process when a rendering context is created;
    //       until then only the dimensions exist, and readbacks produce transparent black.

    // 2. Initialize the width of this to width.
    // 3. Initialize the height of this to height.

    // FIXME: 4. Set this's inherited language to explicitly unknown.

    // FIXME: 5. Set this's inherited direction to "ltr".

    // 6. Let global be the relevant global object of this.
    auto& global = realm.global_object();

    // 7. If global is a Window object:
    if (is<HTML::Window>(global)) {
        auto& window = as<HTML::Window>(global);
        // 1.Let element be the document element of global's associated Document.
        auto* element = window.associated_document().document_element();
        // 2. If element is not null :
        if (element) {
            // FIXME: 1. Set the inherited language of this to element's language.
            // FIXME: 2. Set the inherited direction of this to element's directionality.
        }
    }

    return realm.create<OffscreenCanvas>(realm, width, height);
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas
OffscreenCanvas::OffscreenCanvas(JS::Realm& realm, WebIDL::UnsignedLongLong width, WebIDL::UnsignedLongLong height)
    : EventTarget(realm)
    , m_width(width)
    , m_height(height)
{
}

OffscreenCanvas::~OffscreenCanvas() = default;

WebIDL::ExceptionOr<void> OffscreenCanvas::transfer_steps(HTML::TransferDataEncoder&)
{
    // FIXME: Implement this
    dbgln("(STUBBED) OffscreenCanvas::transfer_steps(HTML::TransferDataEncoder&)");
    return {};
}

WebIDL::ExceptionOr<void> OffscreenCanvas::transfer_receiving_steps(HTML::TransferDataDecoder&)
{
    // FIXME: Implement this
    dbgln("(STUBBED) OffscreenCanvas::transfer_receiving_steps(HTML::TransferDataDecoder&)");
    return {};
}

HTML::TransferType OffscreenCanvas::primary_interface() const
{
    // FIXME: Implement this
    dbgln("(STUBBED) OffscreenCanvas::primary_interface()");
    return {};
}

WebIDL::UnsignedLongLong OffscreenCanvas::width() const
{
    return m_width;
}

WebIDL::UnsignedLongLong OffscreenCanvas::height() const
{
    return m_height;
}

void OffscreenCanvas::reset_context_to_default_state()
{
    m_context.visit(
        [](GC::Ref<OffscreenCanvasRenderingContext2D>& context) {
            context->reset_to_default_state();
        },
        [](GC::Ref<WebGL::WebGLRenderingContext>& context) {
            context->reset_to_default_state();
        },
        [](GC::Ref<WebGL::WebGL2RenderingContext>& context) {
            context->reset_to_default_state();
        },
        [](Empty) {
            // Do nothing.
        });
}

void OffscreenCanvas::update_context_bitmap_size()
{
    // The context works on the clamped size: a canvas whose area exceeds the
    // maximum has no backing storage at all rather than a failing remote context,
    // matching HTMLCanvasElement::notify_context_about_canvas_size_change.
    auto bitmap_size = bitmap_size_for_canvas();
    m_context.visit(
        [&](GC::Ref<OffscreenCanvasRenderingContext2D>& context) {
            // Setting a dimension always replaces the bitmap, even when the value
            // did not change.
            context->replace_bitmap(bitmap_size);
        },
        [&](GC::Ref<WebGL::WebGLRenderingContext>& context) {
            context->set_size(bitmap_size);
        },
        [&](GC::Ref<WebGL::WebGL2RenderingContext>& context) {
            context->set_size(bitmap_size);
        },
        [](Empty) {
            // Do nothing.
        });
}

RefPtr<Gfx::Bitmap> OffscreenCanvas::read_back_bitmap()
{
    auto size = bitmap_size_for_canvas();
    if (size.is_empty())
        return nullptr;

    RefPtr<Gfx::Bitmap> bitmap;
    if (auto* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>())
        bitmap = (*context)->read_pixels({ {}, size });
    // FIXME: Read back the WebGL drawing buffer once OffscreenCanvas supports WebGL contexts.

    if (!bitmap) {
        // No rendering context, or no Compositor to read from: the bitmap is
        // transparent black. (A 2D context synthesizes its clear color in
        // read_pixels() itself.)
        auto bitmap_or_error = Gfx::Bitmap::create(Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, size);
        if (bitmap_or_error.is_error())
            return nullptr;
        bitmap = bitmap_or_error.release_value();
    }
    return bitmap;
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-width
WebIDL::ExceptionOr<void> OffscreenCanvas::set_width(WebIDL::UnsignedLongLong value)
{
    // On setting, if this OffscreenCanvas object's [[Detached]] internal slot value
    // is true, then throw an "InvalidStateError" DOMException.
    if (is_detached())
        return WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas is detached"_utf16);

    m_width = value;

    update_context_bitmap_size();
    reset_context_to_default_state();
    return {};
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-height
WebIDL::ExceptionOr<void> OffscreenCanvas::set_height(WebIDL::UnsignedLongLong value)
{
    // On setting, if this OffscreenCanvas object's [[Detached]] internal slot value
    // is true, then throw an "InvalidStateError" DOMException.
    if (is_detached())
        return WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas is detached"_utf16);

    m_height = value;

    update_context_bitmap_size();
    reset_context_to_default_state();
    return {};
}

template<typename ContextType>
static Optional<OffscreenCanvas::CanvasDrawSource> prepare_webgl_context_as_canvas_draw_source(ContextType& context)
{
    if (context.is_context_lost())
        return {};
    context.prepare_for_compositing();
    auto canvas_id = context.context().canvas_id();
    if (!canvas_id.has_value())
        return {};
    return OffscreenCanvas::CanvasDrawSource { .canvas_id = *canvas_id, .is_2d = false };
}

Page* OffscreenCanvas::page_for_compositor()
{
    auto& page = Bindings::principal_host_defined_page(HTML::relevant_realm(*this));
    page.ensure_compositor_host();
    return &page;
}

// https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-origin-clean
bool OffscreenCanvas::is_origin_clean() const
{
    if (auto const* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>())
        return (*context)->origin_clean();
    // FIXME: WebGL and WebGL2 contexts do not track the origin-clean flag yet.
    return true;
}

Optional<OffscreenCanvas::CanvasDrawSource> OffscreenCanvas::prepare_as_canvas_draw_source()
{
    return m_context.visit(
        [](GC::Ref<OffscreenCanvasRenderingContext2D>& context) -> Optional<CanvasDrawSource> {
            if (context->is_context_lost())
                return {};
            context->ensure_backing_storage();
            auto canvas_id = context->canvas_id();
            if (!canvas_id.has_value())
                return {};
            return CanvasDrawSource { .canvas_id = *canvas_id, .is_2d = true };
        },
        [](GC::Ref<WebGL::WebGLRenderingContext>& context) -> Optional<CanvasDrawSource> {
            return prepare_webgl_context_as_canvas_draw_source(*context);
        },
        [](GC::Ref<WebGL::WebGL2RenderingContext>& context) -> Optional<CanvasDrawSource> {
            return prepare_webgl_context_as_canvas_draw_source(*context);
        },
        [](Empty) -> Optional<CanvasDrawSource> {
            return {};
        });
}

Gfx::IntSize OffscreenCanvas::clamped_logical_size() const
{
    return { clamp_canvas_dimension(m_width), clamp_canvas_dimension(m_height) };
}

Gfx::IntSize OffscreenCanvas::bitmap_size_for_canvas() const
{
    Checked<u64> area = m_width;
    area *= m_height;
    if (area.has_overflow() || area.value() > static_cast<u64>(Gfx::max_canvas_area))
        return {};
    return clamped_logical_size();
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-getcontext
JS::ThrowCompletionOr<OffscreenRenderingContext> OffscreenCanvas::get_context(Bindings::OffscreenRenderingContextId contextId, JS::Value options)
{
    // 1. If options is not an object, then set options to null.
    if (!options.is_object())
        options = JS::js_null();

    // 2. Set options to the result of converting options to a JavaScript value.
    // NOTE: No-op.

    // 3. Run the steps in the cell of the following table whose column header matches this OffscreenCanvas object's context mode and whose row header matches contextId:
    // NOTE: See the spec for the full table.
    if (contextId == Bindings::OffscreenRenderingContextId::_2d) {
        if (TRY(create_2d_context(options)) == HasOrCreatedContext::Yes)
            return *m_context.get<GC::Ref<HTML::OffscreenCanvasRenderingContext2D>>();

        return Empty {};
    }

    if (contextId == Bindings::OffscreenRenderingContextId::Webgl) {
        dbgln("(STUBBED) OffscreenCanvas::get_context(Webgl)");

        return Empty {};
    }

    if (contextId == Bindings::OffscreenRenderingContextId::Webgl2) {
        dbgln("(STUBBED) OffscreenCanvas::get_context(Webgl2)");

        return Empty {};
    }

    return Empty {};
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-transfertoimagebitmap
WebIDL::ExceptionOr<GC::Ref<ImageBitmap>> OffscreenCanvas::transfer_to_image_bitmap()
{
    // The transferToImageBitmap() method, when invoked, must run the following steps :

    // 1. If the value of this OffscreenCanvas object's [[Detached]] internal slot is set to true, then throw an "InvalidStateError" DOMException.
    if (is_detached())
        return WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas is detached"_utf16);

    // 2. If this OffscreenCanvas object's context mode is set to none, then throw an "InvalidStateError" DOMException.
    if (m_context.has<Empty>()) {
        return WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas has no context"_utf16);
    }

    // 3. Let image be a newly created ImageBitmap object that references the same underlying bitmap data as this OffscreenCanvas object's bitmap.
    // AD-HOC: The bitmap lives in the Compositor process, so the ImageBitmap gets a read-back
    //         copy. This is observably equivalent to a reference because the next step replaces
    //         this canvas' bitmap anyway.
    auto image = ImageBitmap::create(realm());
    image->set_bitmap(read_back_bitmap());

    // 4. Set this OffscreenCanvas object's bitmap to reference a newly created bitmap of the same dimensions and color space as the previous bitmap, and with its pixels initialized to transparent black, or opaque black if the rendering context' s alpha is false.
    if (auto* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>())
        (*context)->clear_entire_bitmap();

    // 5. Return image.
    return image;
}

static Tuple<Utf16String, Optional<double>> options_convert_or_default(Optional<Bindings::ImageEncodeOptions> options)
{

    if (!options.has_value()) {
        return { "image/png"_utf16, {} };
    }

    return { options->type, options->quality };
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-converttoblob
GC::Ref<WebIDL::Promise> OffscreenCanvas::convert_to_blob(Optional<Bindings::ImageEncodeOptions> maybe_options)
{
    // 1. If the value of this's [[Detached]] internal slot is true, then return a promise rejected with an "InvalidStateError" DOMException.
    if (is_detached()) {
        auto error = WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas is detached"_utf16);
        return WebIDL::create_rejected_promise_from_exception(realm(), error);
    }

    // 2. If this's context mode is 2d and the rendering context's output bitmap's origin-clean flag is set to false, then return a promise rejected with a "SecurityError" DOMException.
    if (auto* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>(); context && !(*context)->origin_clean()) {
        auto error = WebIDL::SecurityError::create(realm(), "OffscreenCanvas is not origin-clean"_utf16);
        return WebIDL::create_rejected_promise_from_exception(realm(), error);
    }

    auto size = bitmap_size_for_canvas();

    // 3. If this's bitmap has no pixels (i.e., either its horizontal dimension or its vertical dimension is zero), then return a promise rejected with an "IndexSizeError" DOMException.
    if (size.height() == 0 or size.width() == 0) {
        auto error = WebIDL::IndexSizeError::create(realm(), "OffscreenCanvas has invalid dimensions. The bitmap has no pixels"_utf16);

        return WebIDL::create_rejected_promise_from_exception(realm(), error);
    }

    // 4. Let bitmap be a copy of this's bitmap.
    // NOTE: The copy must be taken before the steps below run in parallel, and reading back
    //       from the Compositor produces an independent copy already.
    RefPtr<Gfx::Bitmap> bitmap = read_back_bitmap();

    // 5. Let result be a new promise object.
    auto result_promise = WebIDL::create_promise(realm());

    // 6. Let global be this's relevant global object.
    auto& global = HTML::relevant_global_object(*this);

    // 7. Run these steps in parallel:
    Platform::EventLoopPlugin::the().deferred_invoke(GC::create_function(heap(), [this, &global, result_promise, bitmap, maybe_options] {
        // 1. Let file be a serialization of bitmap as a file, with options's type and quality if present.
        Optional<SerializeBitmapResult> file_result {};
        auto options = options_convert_or_default(maybe_options);

        if (bitmap) {
            if (auto result = serialize_bitmap(*bitmap, options.get<0>(), options.get<1>()); !result.is_error())
                file_result = result.release_value();
        }

        // 2. Queue a global task on the canvas blob serialization task source given global to run these steps:
        HTML::queue_global_task(Task::Source::CanvasBlobSerializationTask, global, GC::create_function(heap(), [this, result_promise, file_result = move(file_result)] -> void {
            HTML::TemporaryExecutionContext context(realm(), HTML::TemporaryExecutionContext::CallbacksEnabled::Yes);

            // 1. If file is null, then reject result with an "EncodingError" DOMException.
            if (!file_result.has_value()) {
                auto error = WebIDL::EncodingError::create(realm(), "Failed to convert OffscreenCanvas to Blob"_utf16);
                WebIDL::reject_promise(realm(), result_promise, error);
            }
            // 2. Otherwise, resolve result with a new Blob object, created in global's relevant realm, representing file. [FILEAPI]
            else {
                auto blob = FileAPI::Blob::create(realm(), file_result->buffer, serialized_bitmap_mime_type_to_utf16_view(file_result->mime_type));
                WebIDL::resolve_promise(realm(), result_promise, blob);
            }
        }));
    }));

    // 8. Return result.
    return result_promise;
}
void OffscreenCanvas::set_oncontextlost(GC::Ptr<WebIDL::CallbackType> event_handler)
{
    set_event_handler_attribute(HTML::EventNames::contextlost, event_handler);
}

GC::Ptr<WebIDL::CallbackType> OffscreenCanvas::oncontextlost()
{
    return event_handler_attribute(HTML::EventNames::contextlost);
}

void OffscreenCanvas::set_oncontextrestored(GC::Ptr<WebIDL::CallbackType> event_handler)
{
    set_event_handler_attribute(HTML::EventNames::contextrestored, event_handler);
}

GC::Ptr<WebIDL::CallbackType> OffscreenCanvas::oncontextrestored()
{
    return event_handler_attribute(HTML::EventNames::contextrestored);
}

CSS::ComputationContext OffscreenCanvas::canvas_font_computation_context() const
{
    // NB: The default font for a canvas is 10px sans-serif so we use a point size of 8 here.
    CSS::Length::FontMetrics font_metrics { 10, Platform::FontPlugin::the().default_font(8)->pixel_metrics(), CSS::InitialValues::line_height() };

    return CSS::ComputationContext {
        .length_resolution_context = {
            .viewport_rect = { 0, 0, 0, 0 },
            .font_metrics = font_metrics,
            .root_font_metrics = font_metrics },

        // NB: We don't require an abstract element because tree counting and random() functions aren't allowed in
        //     offscreen canvas context values
        .abstract_element = {},

        // NB: We don't require a color scheme since this is only used for resolving font values, not colors
        .color_scheme = {}
    };
}

void OffscreenCanvas::initialize(JS::Realm& realm)
{
    Base::initialize(realm);
    WEB_SET_PROTOTYPE_FOR_INTERFACE(OffscreenCanvas);
}

void OffscreenCanvas::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_context);
}

JS::ThrowCompletionOr<OffscreenCanvas::HasOrCreatedContext> OffscreenCanvas::create_2d_context(JS::Value options)
{
    if (!m_context.has<Empty>())
        return m_context.has<GC::Ref<OffscreenCanvasRenderingContext2D>>() ? HasOrCreatedContext::Yes : HasOrCreatedContext::No;

    m_context = TRY(OffscreenCanvasRenderingContext2D::create(realm(), *this, options));
    return HasOrCreatedContext::Yes;
}

}
