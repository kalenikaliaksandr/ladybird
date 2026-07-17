/*
 * Copyright (c) 2025-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibGfx/Forward.h>
#include <LibGfx/Size.h>
#include <LibWeb/Bindings/PlatformObject.h>
#include <LibWeb/Bindings/Transferable.h>
#include <LibWeb/DOM/EventTarget.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/OffscreenCanvasLink.h>
#include <LibWeb/WebIDL/Types.h>

namespace Web::HTML {

// https://html.spec.whatwg.org/multipage/canvas.html#offscreenrenderingcontext
// NOTE: This is the Variant created by the IDL wrapper generator, and needs to be updated accordingly.
using OffscreenRenderingContext = Variant<GC::Ref<OffscreenCanvasRenderingContext2D>, GC::Ref<WebGL::WebGLRenderingContext>, GC::Ref<WebGL::WebGL2RenderingContext>, Empty>;

// https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas
class OffscreenCanvas : public DOM::EventTarget
    , public Web::Bindings::Transferable {
    WEB_PLATFORM_OBJECT(OffscreenCanvas, DOM::EventTarget);
    GC_DECLARE_ALLOCATOR(OffscreenCanvas);

public:
    static constexpr bool OVERRIDES_FINALIZE = true;

    static GC::Ref<OffscreenCanvas> create(JS::Realm&, WebIDL::UnsignedLongLong width, WebIDL::UnsignedLongLong height);

    static WebIDL::ExceptionOr<GC::Ref<OffscreenCanvas>> construct_impl(
        JS::Realm&,
        WebIDL::UnsignedLongLong width,
        WebIDL::UnsignedLongLong height);

    virtual ~OffscreenCanvas() override;

    // ^Web::Bindings::Transferable
    virtual WebIDL::ExceptionOr<void> transfer_steps(HTML::TransferDataEncoder&) override;
    virtual WebIDL::ExceptionOr<void> transfer_receiving_steps(HTML::TransferDataDecoder&) override;
    virtual HTML::TransferType primary_interface() const override;

    WebIDL::UnsignedLongLong width() const;
    WebIDL::UnsignedLongLong height() const;

    // The canvas pixels live in the Compositor process; this synchronously reads
    // them back, or returns the context's initial bitmap (transparent black, or
    // opaque black for an alpha:false 2D context) when there is no backing
    // storage to read from. Returns null for zero-area canvases.
    RefPtr<Gfx::Bitmap> read_back_bitmap();

    WebIDL::ExceptionOr<void> set_width(WebIDL::UnsignedLongLong);
    WebIDL::ExceptionOr<void> set_height(WebIDL::UnsignedLongLong);

    struct CanvasDrawSource {
        Painting::CanvasId canvas_id;
        bool is_2d { false };
    };
    // Presents the current WebGL frame or materializes 2D backing storage so a
    // DrawCanvas command referencing this canvas resolves its live pixels;
    // returns the id to reference, or nothing when no compositor context exists.
    Optional<CanvasDrawSource> prepare_as_canvas_draw_source();

    // https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-origin-clean
    bool is_origin_clean() const;

    // The Page whose compositor host backs this canvas's rendering, with the
    // host ensured: both Window and Worker realms carry a Page through their
    // PrincipalHostDefined, and in worker processes that page's client provides
    // a compositor host over the worker's own connection.
    Page* page_for_compositor();

    Gfx::IntSize bitmap_size_for_canvas() const;

    JS::ThrowCompletionOr<OffscreenRenderingContext> get_context(Bindings::OffscreenRenderingContextId contextId, JS::Value options);

    WebIDL::ExceptionOr<GC::Ref<ImageBitmap>> transfer_to_image_bitmap();

    GC::Ref<WebIDL::Promise> convert_to_blob(Optional<Bindings::ImageEncodeOptions> options);

    void set_oncontextlost(GC::Ptr<WebIDL::CallbackType>);
    GC::Ptr<WebIDL::CallbackType> oncontextlost();
    void set_oncontextrestored(GC::Ptr<WebIDL::CallbackType>);
    GC::Ptr<WebIDL::CallbackType> oncontextrestored();

    CSS::ComputationContext canvas_font_computation_context() const;

    // Links this OffscreenCanvas to a placeholder canvas element via a pre-allocated
    // compositor canvas id; the first rendering context claims that id so the
    // placeholder's DrawCanvas resolves the frames committed here.
    void set_placeholder_link(Badge<HTMLCanvasElement>, Painting::OffscreenCanvasPlaceholderLink);
    Optional<Painting::OffscreenCanvasPlaceholderLink> placeholder_link() const { return m_placeholder_link; }

    void notify_context_did_draw();
    void commit_frame_if_needed();

private:
    OffscreenCanvas(JS::Realm&, WebIDL::UnsignedLongLong width, WebIDL::UnsignedLongLong height);

    virtual void initialize(JS::Realm&) override;
    virtual void finalize() override;
    virtual void visit_edges(Cell::Visitor&) override;

    Page* page_of_relevant_global_object() const;
    void register_with_page_for_frame_commits();

    enum class HasOrCreatedContext {
        No,
        Yes,
    };
    JS::ThrowCompletionOr<HasOrCreatedContext> create_2d_context(JS::Value options);

    void reset_context_to_default_state();
    void update_context_bitmap_size();

    // The logical dimensions clamped into bitmap range; commits and paint sizes
    // use this, while the IDL attributes report the full unsigned long long values.
    Gfx::IntSize clamped_logical_size() const;

    Variant<GC::Ref<HTML::OffscreenCanvasRenderingContext2D>, GC::Ref<WebGL::WebGLRenderingContext>, GC::Ref<WebGL::WebGL2RenderingContext>, Empty> m_context;

    // Logical canvas dimensions: unsigned long long per IDL, preserved even when
    // no bitmap of this size can be allocated (bitmap_size_for_canvas() is then
    // empty and drawing no-ops).
    WebIDL::UnsignedLongLong m_width { 0 };
    WebIDL::UnsignedLongLong m_height { 0 };

    Optional<Painting::OffscreenCanvasPlaceholderLink> m_placeholder_link;
    bool m_needs_frame_commit { false };
    bool m_is_registered_with_page { false };
};

}
