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
#include <LibWeb/Compositor/CompositorHost.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/HTML/Canvas/SerializeBitmap.h>
#include <LibWeb/HTML/DedicatedWorkerGlobalScope.h>
#include <LibWeb/HTML/EventLoop/EventLoop.h>
#include <LibWeb/HTML/OffscreenCanvas.h>
#include <LibWeb/HTML/OffscreenCanvasRenderingContext2D.h>
#include <LibWeb/HTML/Scripting/TemporaryExecutionContext.h>
#include <LibWeb/HTML/StructuredSerialize.h>
#include <LibWeb/HTML/Window.h>
#include <LibWeb/HTML/WorkerGlobalScope.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Platform/EventLoopPlugin.h>
#include <LibWeb/Platform/FontPlugin.h>
#include <LibWeb/WebGL/WebGL2RenderingContext.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>
#include <LibWeb/WebGL/WebGLRenderingContext.h>
#include <LibWeb/WebGL/WebGLRenderingContextBase.h>

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

// Presenting without preserve-drawing-buffer flags the Compositor-side deferred
// clear-to-default-values; the (possibly same-size) resize forms the command
// batch that executes it, so readbacks and the next frame observe a fresh
// buffer instead of the old pixels.
template<typename ContextType>
static void replace_webgl_drawing_buffer(ContextType& context, Gfx::IntSize bitmap_size)
{
    if (!context.is_context_lost())
        context.context().present_canvas_for_compositing(/* preserve_drawing_buffer= */ false);
    // A lost context still tracks the logical size for its restore path.
    context.set_size(bitmap_size);
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas
OffscreenCanvas::OffscreenCanvas(JS::Realm& realm, WebIDL::UnsignedLongLong width, WebIDL::UnsignedLongLong height)
    : EventTarget(realm)
    , m_width(width)
    , m_height(height)
{
}

OffscreenCanvas::~OffscreenCanvas() = default;

Page* OffscreenCanvas::page_of_relevant_global_object() const
{
    return &Bindings::principal_host_defined_page(HTML::relevant_realm(*this));
}

Page* OffscreenCanvas::page_for_compositor()
{
    auto* page = page_of_relevant_global_object();
    if (page)
        page->ensure_compositor_host();
    return page;
}

void OffscreenCanvas::set_placeholder_link(Badge<HTMLCanvasElement>, Painting::OffscreenCanvasPlaceholderLink placeholder_link)
{
    m_placeholder_link = placeholder_link;
    register_with_page_for_compositor_notifications();
}

// Registered canvases get frame commits swept by the Page's canvas-compositing pass
// (placeholder-linked ones) and compositor loss/reconnect notifications (any canvas
// with a rendering context, whose remote state lives in the Compositor process).
void OffscreenCanvas::register_with_page_for_compositor_notifications()
{
    bool has_rendering_context = !m_context.has<Empty>();
    if (m_is_registered_with_page || (!m_placeholder_link.has_value() && !has_rendering_context))
        return;
    if (auto* page = page_of_relevant_global_object()) {
        page->register_offscreen_canvas({}, *this);
        m_is_registered_with_page = true;
    }
}

void OffscreenCanvas::notify_context_did_draw()
{
    if (!m_placeholder_link.has_value()) {
        return;
    }
    // One pending commit marker is enough; the rendering update is already
    // scheduled, and re-queuing it for every drawing primitive would rescan the
    // task queue each time.
    if (m_needs_frame_commit)
        return;
    m_needs_frame_commit = true;

    // A committed frame only reaches the placeholder after the canvas-compositing
    // sweep presents it, so make sure a rendering update actually happens: nothing
    // else marks documents dirty when script only draws to an OffscreenCanvas.
    auto& global = relevant_global_object(*this);
    if (is<Window>(global))
        main_thread_event_loop().queue_task_to_update_the_rendering();
    else if (auto* worker_scope = as_if<WorkerGlobalScope>(global))
        worker_scope->schedule_rendering_update();
}

void OffscreenCanvas::commit_frame_if_needed()
{
    if (!m_needs_frame_commit || !m_placeholder_link.has_value())
        return;
    m_needs_frame_commit = false;

    // Recording the commit marker makes the Compositor copy the live surface into
    // the presented one and notify the placeholder's watcher with the committed
    // logical size and origin-clean state; a 2D commit travels in the shared
    // command stream the caller flushes afterwards, a WebGL commit flushes its
    // own command stream.
    m_context.visit(
        [&](GC::Ref<OffscreenCanvasRenderingContext2D>& context) {
            // A resize or context creation leaves no backing storage until the next
            // draw; presenting the initial (cleared) bitmap requires creating it.
            context->ensure_backing_storage();
            if (context->commit_frame_for_placeholder(clamped_logical_size()))
                return;
            // No bitmap can back the current dimensions (zero, or beyond the
            // maximum area): commit the logical size alone so the placeholder
            // still follows it instead of keeping the old frame and dimensions.
            if (auto* page = page_of_relevant_global_object(); page && page->has_compositor_host())
                page->compositor_host().commit_offscreen_canvas_size(*m_placeholder_link, clamped_logical_size());
        },
        [&](GC::Ref<WebGL::WebGLRenderingContext>& context) {
            context->commit_frame_for_placeholder(clamped_logical_size());
        },
        [&](GC::Ref<WebGL::WebGL2RenderingContext>& context) {
            context->commit_frame_for_placeholder(clamped_logical_size());
        },
        [&](Empty) {
            // No context yet: a resize before getContext() must still commit the
            // logical size so the placeholder follows it.
            if (auto* page = page_of_relevant_global_object(); page && page->has_compositor_host())
                page->compositor_host().commit_offscreen_canvas_size(*m_placeholder_link, clamped_logical_size());
        });
}

WebGL::WebGLRenderingContextBase* OffscreenCanvas::webgl_context() const
{
    return m_context.visit(
        [](GC::Ref<WebGL::WebGLRenderingContext> const& context) -> WebGL::WebGLRenderingContextBase* { return context.ptr(); },
        [](GC::Ref<WebGL::WebGL2RenderingContext> const& context) -> WebGL::WebGLRenderingContextBase* { return context.ptr(); },
        [](auto const&) -> WebGL::WebGLRenderingContextBase* { return nullptr; });
}

void OffscreenCanvas::notify_compositor_connection_lost()
{
    if (auto* webgl_context = this->webgl_context())
        webgl_context->lose_context_from_compositor_loss();
}

void OffscreenCanvas::notify_compositor_backing_storage_lost()
{
    if (auto* webgl_context = this->webgl_context()) {
        webgl_context->restore_context_after_compositor_reconnect();
        return;
    }
    if (auto* context_2d = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>())
        (*context_2d)->notify_backing_storage_lost();
}

// https://html.spec.whatwg.org/multipage/canvas.html#the-offscreencanvas-interface:transfer-steps
WebIDL::ExceptionOr<void> OffscreenCanvas::transfer_steps(HTML::TransferDataEncoder& data_holder)
{
    // 1. If value's context mode is not equal to none, then throw an "InvalidStateError" DOMException.
    if (!m_context.has<Empty>())
        return WebIDL::InvalidStateError::create(realm(), "Cannot transfer an OffscreenCanvas with a rendering context"_utf16);

    // 2. Set dataHolder.[[Width]] to value's width, and dataHolder.[[Height]] to value's height.
    MUST(data_holder.encode(m_width));
    MUST(data_holder.encode(m_height));

    // 3. Set dataHolder.[[PlaceholderCanvas]] to be a weak reference to value's placeholder canvas
    //    element, if value has one, or null if it does not.
    // AD-HOC: The placeholder may live in another process; what identifies it is the pre-allocated
    //         compositor canvas id, which travels as plain data through any number of transfers.
    MUST(data_holder.encode(m_placeholder_link));

    // FIXME: Also transfer the inherited language and inherited direction.

    if (m_is_registered_with_page) {
        if (auto* page = page_of_relevant_global_object())
            page->unregister_offscreen_canvas({}, *this);
        m_is_registered_with_page = false;
    }
    m_placeholder_link.clear();
    m_needs_frame_commit = false;

    // 4. Unset value's width and height: the detached source must report 0x0 while
    //    the receiver carries the encoded dimensions.
    m_width = 0;
    m_height = 0;
    return {};
}

// https://html.spec.whatwg.org/multipage/canvas.html#the-offscreencanvas-interface:transfer-receiving-steps
WebIDL::ExceptionOr<void> OffscreenCanvas::transfer_receiving_steps(HTML::TransferDataDecoder& data_holder)
{
    // 1. Initialize value's bitmap to a rectangular array of transparent black pixels with width equal
    //    to dataHolder.[[Width]] and height equal to dataHolder.[[Height]].
    m_width = TRY(decode_or_throw_data_clone_error<WebIDL::UnsignedLongLong>(realm(), data_holder));
    m_height = TRY(decode_or_throw_data_clone_error<WebIDL::UnsignedLongLong>(realm(), data_holder));

    // 2. If dataHolder.[[PlaceholderCanvas]] is not null, set value's placeholder canvas element to
    //    dataHolder.[[PlaceholderCanvas]] (while maintaining the weak reference semantics).
    m_placeholder_link = TRY(decode_or_throw_data_clone_error<Optional<Painting::OffscreenCanvasPlaceholderLink>>(realm(), data_holder));
    register_with_page_for_compositor_notifications();

    // FIXME: Also receive the inherited language and inherited direction.

    return {};
}

HTML::TransferType OffscreenCanvas::primary_interface() const
{
    return HTML::TransferType::OffscreenCanvas;
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
            // Setting a dimension always replaces the bitmap even when the
            // value did not change.
            context->replace_bitmap(bitmap_size);
        },
        [&](GC::Ref<WebGL::WebGLRenderingContext>& context) {
            replace_webgl_drawing_buffer(*context, bitmap_size);
        },
        [&](GC::Ref<WebGL::WebGL2RenderingContext>& context) {
            replace_webgl_drawing_buffer(*context, bitmap_size);
        },
        [](Empty) {
            // Do nothing.
        });

    // Resizing replaces the bitmap with a cleared one; a placeholder must be shown
    // that new bitmap even when nothing is drawn afterwards.
    notify_context_did_draw();
}

RefPtr<Gfx::Bitmap> OffscreenCanvas::read_back_bitmap()
{
    auto size = bitmap_size_for_canvas();
    if (size.is_empty())
        return nullptr;

    RefPtr<Gfx::Bitmap> bitmap;
    if (auto* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>())
        bitmap = (*context)->read_pixels({ {}, size });
    else if (auto* webgl_context = this->webgl_context())
        bitmap = webgl_context->context().read_back_drawing_buffer({ {}, size });

    if (!bitmap) {
        // No rendering context, or a WebGL context with no Compositor to read
        // from: the bitmap is transparent black. (A 2D context synthesizes its
        // clear color in read_pixels() itself.)
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

    // detached column: every context type throws an "InvalidStateError" DOMException.
    if (is_detached())
        return JS::throw_completion(WebIDL::InvalidStateError::create(realm(), "OffscreenCanvas is detached"_utf16));

    if (contextId == Bindings::OffscreenRenderingContextId::_2d) {
        if (TRY(create_2d_context(options)) == HasOrCreatedContext::Yes)
            return *m_context.get<GC::Ref<HTML::OffscreenCanvasRenderingContext2D>>();

        return Empty {};
    }

    if (contextId == Bindings::OffscreenRenderingContextId::Webgl) {
        if (TRY(create_webgl_context<WebGL::WebGLRenderingContext>(options)) == HasOrCreatedContext::Yes)
            return *m_context.get<GC::Ref<WebGL::WebGLRenderingContext>>();

        return Empty {};
    }

    if (contextId == Bindings::OffscreenRenderingContextId::Webgl2) {
        if (TRY(create_webgl_context<WebGL::WebGL2RenderingContext>(options)) == HasOrCreatedContext::Yes)
            return *m_context.get<GC::Ref<WebGL::WebGL2RenderingContext>>();

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
    auto reset_webgl_drawing_buffer = [&](auto& context) {
        // A lost context has no drawing buffer to reset (and must not commit).
        if (context.is_context_lost())
            return;
        replace_webgl_drawing_buffer(context, bitmap_size_for_canvas());
        notify_context_did_draw();
    };
    m_context.visit(
        [](GC::Ref<OffscreenCanvasRenderingContext2D>& context) {
            context->clear_entire_bitmap();
        },
        [&](GC::Ref<WebGL::WebGLRenderingContext>& context) {
            reset_webgl_drawing_buffer(*context);
        },
        [&](GC::Ref<WebGL::WebGL2RenderingContext>& context) {
            reset_webgl_drawing_buffer(*context);
        },
        [](Empty) {
            // Unreachable: step 2 threw for context mode none.
        });

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

    // The readback can reveal taint composited compositor-side; re-check.
    if (auto* context = m_context.get_pointer<GC::Ref<OffscreenCanvasRenderingContext2D>>(); context && !(*context)->origin_clean()) {
        auto error = WebIDL::SecurityError::create(realm(), "OffscreenCanvas is not origin-clean"_utf16);
        return WebIDL::create_rejected_promise_from_exception(realm(), error);
    }

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
    // NB: The own prototype must be claimed before Base::initialize, or EventTarget's
    //     initialize takes the prototype slot and this macro becomes a no-op. Only
    //     objects created outside the generated constructor (like transfer-receiving)
    //     would notice, because the constructor overrides the prototype afterwards.
    WEB_SET_PROTOTYPE_FOR_INTERFACE(OffscreenCanvas);
    Base::initialize(realm);
}

void OffscreenCanvas::finalize()
{
    Base::finalize();
    if (m_is_registered_with_page) {
        if (auto* page = page_of_relevant_global_object())
            page->unregister_offscreen_canvas({}, *this);
    }
    // This link holder dies, so no claim (or resize re-claim) can arrive from it
    // anymore; telling the Compositor lets it drop the reservation once the
    // placeholder element has released it, instead of waiting for this whole
    // process to disconnect.
    if (m_placeholder_link.has_value()) {
        if (auto* page = page_of_relevant_global_object(); page && page->has_compositor_host())
            page->compositor_host().decline_offscreen_canvas_claim(*m_placeholder_link);
    }
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
    register_with_page_for_compositor_notifications();

    // A placeholder must show the context's initial output bitmap even before any
    // drawing call: transparent black, or opaque black for alpha=false contexts.
    notify_context_did_draw();
    return HasOrCreatedContext::Yes;
}

template<typename ContextType>
JS::ThrowCompletionOr<OffscreenCanvas::HasOrCreatedContext> OffscreenCanvas::create_webgl_context(JS::Value options)
{
    if (!m_context.has<Empty>())
        return m_context.has<GC::Ref<ContextType>>() ? HasOrCreatedContext::Yes : HasOrCreatedContext::No;

    auto maybe_context = TRY(ContextType::create(realm(), GC::Ref { *this }, options));
    if (!maybe_context)
        return HasOrCreatedContext::No;

    m_context = GC::Ref<ContextType>(*maybe_context);
    register_with_page_for_compositor_notifications();
    return HasOrCreatedContext::Yes;
}

}
