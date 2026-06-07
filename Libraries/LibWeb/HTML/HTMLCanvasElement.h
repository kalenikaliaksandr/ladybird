/*
 * Copyright (c) 2020, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/OwnPtr.h>
#include <LibGfx/Forward.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/HTML/HTMLElement.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/WebIDL/Types.h>

namespace Web::Compositor {
class CompositorContextHandle;
}

namespace Web::HTML {

class HTMLCanvasElement final : public HTMLElement {
    WEB_PLATFORM_OBJECT(HTMLCanvasElement, HTMLElement);
    GC_DECLARE_ALLOCATOR(HTMLCanvasElement);

public:
    static constexpr bool OVERRIDES_FINALIZE = true;

    using RenderingContext = Variant<GC::Ref<CanvasRenderingContext2D>, GC::Ref<WebGL::WebGLRenderingContext>, GC::Ref<WebGL::WebGL2RenderingContext>, Empty>;

    virtual ~HTMLCanvasElement() override;

    Gfx::IntSize bitmap_size_for_canvas(size_t minimum_width = 0, size_t minimum_height = 0) const;

    JS::ThrowCompletionOr<RenderingContext> get_context(String const& type, JS::Value options);
    enum class HasOrCreatedContext {
        No,
        Yes,
    };
    JS::ThrowCompletionOr<HasOrCreatedContext> create_2d_context(JS::Value options);

    WebIDL::UnsignedLong width() const;
    WebIDL::UnsignedLong height() const;

    void set_width(WebIDL::UnsignedLong);
    void set_height(WebIDL::UnsignedLong);

    virtual void attribute_changed(FlyString const& local_name, Optional<String> const& old_value, Optional<String> const& value, Optional<FlyString> const& namespace_) override;

    String to_data_url(StringView type, Optional<JS::Value> quality);
    WebIDL::ExceptionOr<void> to_blob(GC::Ref<WebIDL::CallbackType> callback, StringView type, Optional<JS::Value> quality);
    RefPtr<Gfx::Bitmap> get_bitmap_from_surface();

    void present();
    void republish_compositor_surface();
    void set_canvas_content_dirty();
    void release_canvas_surface_buffer(Painting::CanvasSurfaceId, u64 generation, u32 buffer_index);

    RefPtr<Gfx::PaintingSurface> surface() const;
    void allocate_painting_surface_if_needed();

    Painting::CanvasSurfaceId ensure_canvas_surface_id();

    CSS::ComputationContext canvas_font_computation_context();

private:
    HTMLCanvasElement(DOM::Document&, DOM::QualifiedName);

    virtual void initialize(JS::Realm&) override;
    virtual void finalize() override;
    virtual void visit_edges(Cell::Visitor&) override;

    virtual bool is_presentational_hint(FlyString const&) const override;
    virtual void apply_presentational_hints(Vector<CSS::StyleProperty>&) const override;

    virtual RefPtr<Layout::Node> create_layout_node(CSS::ComputedProperties const&) override;
    virtual void adjust_computed_style(CSS::ComputedProperties&) override;

    template<typename ContextType>
    JS::ThrowCompletionOr<HasOrCreatedContext> create_webgl_context(JS::Value options);
    template<typename ContextType>
    bool present_webgl_canvas(ContextType&);
    void reset_context_to_default_state();
    void notify_context_about_canvas_size_change();
    bool ensure_canvas_presentation_buffer(Gfx::PaintingSurface&, Web::Compositor::CompositorContextHandle&);
    void clear_canvas_surface();
    bool update_canvas_surface();

    Variant<GC::Ref<HTML::CanvasRenderingContext2D>, GC::Ref<WebGL::WebGLRenderingContext>, GC::Ref<WebGL::WebGL2RenderingContext>, Empty> m_context;
    Optional<Painting::CanvasSurfaceId> m_canvas_surface_id;
    OwnPtr<Gfx::SharedImageBuffer> m_canvas_presentation_buffer;
    u64 m_canvas_presentation_buffer_generation { 0 };
    bool m_canvas_presentation_buffer_is_available { false };
    bool m_canvas_presentation_surface_is_registered { false };
    bool m_canvas_content_dirty { false };
};

}
