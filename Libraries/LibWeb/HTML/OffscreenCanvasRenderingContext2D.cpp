/*
 * Copyright (c) 2025-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/OffscreenCanvasRenderingContext2D.h>
#include <LibWeb/Bindings/PrincipalHostDefined.h>
#include <LibWeb/CSS/StyleValues/FilterStyleValue.h>
#include <LibWeb/HTML/OffscreenCanvas.h>
#include <LibWeb/HTML/OffscreenCanvasRenderingContext2D.h>
#include <LibWeb/Page/Page.h>

namespace Web::HTML {

GC_DEFINE_ALLOCATOR(OffscreenCanvasRenderingContext2D);

JS::ThrowCompletionOr<GC::Ref<OffscreenCanvasRenderingContext2D>> OffscreenCanvasRenderingContext2D::create(JS::Realm& realm, OffscreenCanvas& canvas, JS::Value options)
{
    auto context_attributes = TRY(Bindings::convert_to_idl_value_for_canvas_rendering_context2d_settings(realm.vm(), options));
    return realm.create<OffscreenCanvasRenderingContext2D>(realm, canvas, context_attributes);
}

OffscreenCanvasRenderingContext2D::OffscreenCanvasRenderingContext2D(JS::Realm& realm, OffscreenCanvas& canvas, Bindings::CanvasRenderingContext2DSettings context_attributes)
    : Canvas2DContextBase(realm, canvas.bitmap_size_for_canvas(), move(context_attributes))
    , m_canvas(canvas)
{
}

OffscreenCanvasRenderingContext2D::~OffscreenCanvasRenderingContext2D() = default;

void OffscreenCanvasRenderingContext2D::initialize(JS::Realm& realm)
{
    Base::initialize(realm);
    set_prototype(&Bindings::ensure_web_prototype<Bindings::OffscreenCanvasRenderingContext2DPrototype>(realm, "OffscreenCanvasRenderingContext2D"_utf16_fly_string));
}

void OffscreenCanvasRenderingContext2D::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_canvas);
}

GC::Ref<OffscreenCanvas> OffscreenCanvasRenderingContext2D::canvas()
{
    return m_canvas;
}

void OffscreenCanvasRenderingContext2D::did_draw_hook()
{
    // FIXME: Once transferControlToOffscreen() placeholder links exist, schedule a
    //        commit so the placeholder canvas element picks up the new frame.
}

Page* OffscreenCanvasRenderingContext2D::page_for_compositor()
{
    return m_canvas->page_for_compositor();
}

DOM::EventTarget& OffscreenCanvasRenderingContext2D::context_event_target()
{
    return m_canvas;
}

Gfx::Color OffscreenCanvasRenderingContext2D::resolve_drop_shadow_color(CSS::DropShadowFilterStyleValue const& drop_shadow) const
{
    // The spec resolves the drop-shadow color against the canvas element's style;
    // an OffscreenCanvas has no element, so only colors that resolve without style
    // context apply, and everything else falls back to black.
    if (drop_shadow.color())
        return drop_shadow.color()->to_color(CSS::ColorResolutionContext {}).value_or(Gfx::Color::Black);
    return Gfx::Color::Black;
}

}
