/*
 * Copyright (c) 2025-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/HTML/Canvas/Canvas2DContextBase.h>
#include <LibWeb/HTML/Canvas/CanvasTextDrawingStyles.h>

namespace Web::HTML {

class OffscreenCanvasRenderingContext2D
    : public Canvas2DContextBase
    , public CanvasTextDrawingStyles<OffscreenCanvas> {

    WEB_PLATFORM_OBJECT(OffscreenCanvasRenderingContext2D, Canvas2DContextBase);
    GC_DECLARE_ALLOCATOR(OffscreenCanvasRenderingContext2D);

public:
    [[nodiscard]] static JS::ThrowCompletionOr<GC::Ref<OffscreenCanvasRenderingContext2D>> create(JS::Realm&, OffscreenCanvas&, JS::Value options);

    virtual ~OffscreenCanvasRenderingContext2D() override;

    GC::Ref<OffscreenCanvas> canvas();

protected:
    Variant<GC::Ref<HTMLCanvasElement>, GC::Ref<OffscreenCanvas>> canvas_element() override { return m_canvas; }
    Variant<GC::Ref<HTMLCanvasElement>, GC::Ref<OffscreenCanvas>> canvas_element() const override { return m_canvas; }

private:
    OffscreenCanvasRenderingContext2D(JS::Realm&, OffscreenCanvas&, Bindings::CanvasRenderingContext2DSettings);

    virtual void initialize(JS::Realm&) override;
    virtual void visit_edges(Cell::Visitor&) override;

    virtual void did_draw_hook() override;
    virtual Page* page_for_compositor() override;
    virtual DOM::EventTarget& context_event_target() override;
    virtual Gfx::Color resolve_drop_shadow_color(CSS::DropShadowFilterStyleValue const&) const override;

    GC::Ref<OffscreenCanvas> m_canvas;
};

}
