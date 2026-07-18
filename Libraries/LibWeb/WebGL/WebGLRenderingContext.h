/*
 * Copyright (c) 2022, Luke Wilde <lukew@serenityos.org>
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Utf16FlyString.h>
#include <LibGC/Ptr.h>
#include <LibWeb/Bindings/PlatformObject.h>
#include <LibWeb/Forward.h>
#include <LibWeb/HTML/Canvas/CanvasOwner.h>
#include <LibWeb/WebGL/Types.h>
#include <LibWeb/WebGL/WebGLContextAttributes.h>
#include <LibWeb/WebGL/WebGLRenderingContextOverloads.h>

namespace Web::WebGL {

class WebGLRenderingContext final : public WebGLRenderingContextOverloads {
    WEB_PLATFORM_OBJECT(WebGLRenderingContext, WebGLRenderingContextOverloads);
    GC_DECLARE_ALLOCATOR(WebGLRenderingContext);

public:
    static JS::ThrowCompletionOr<GC::Ptr<WebGLRenderingContext>> create(JS::Realm&, HTML::CanvasOwner, JS::Value options);

    virtual ~WebGLRenderingContext() override;

    void prepare_for_compositing() override;
    void commit_frame_for_placeholder(Gfx::IntSize logical_size);
    void did_update_canvas_content() override;

    virtual HTML::CanvasOwner canvas_for_binding() const override;

    Optional<WebGLContextAttributes> get_context_attributes();

    void set_size(Gfx::IntSize const&);
    void reset_to_default_state();

    WebIDL::Long drawing_buffer_width() const;
    WebIDL::Long drawing_buffer_height() const;

private:
    virtual void initialize(JS::Realm&) override;

    WebGLRenderingContext(JS::Realm&, HTML::CanvasOwner, NonnullOwnPtr<WebGLContextProxy> context, WebGLContextAttributes context_creation_parameters, WebGLContextAttributes actual_context_parameters);

    virtual void visit_edges(Cell::Visitor&) override;
    virtual bool reestablish_remote_context() override;

    HTML::CanvasOwner m_canvas_owner;

    // https://www.khronos.org/registry/webgl/specs/latest/1.0/#context-creation-parameters
    // Each WebGLRenderingContext has context creation parameters, set upon creation, in a WebGLContextAttributes object.
    WebGLContextAttributes m_context_creation_parameters {};

    // https://www.khronos.org/registry/webgl/specs/latest/1.0/#actual-context-parameters
    // Each WebGLRenderingContext has actual context parameters, set each time the drawing buffer is created, in a WebGLContextAttributes object.
    WebGLContextAttributes m_actual_context_parameters {};
};

bool fire_webgl_context_event(DOM::EventTarget& canvas, Utf16FlyString const& type);
void fire_webgl_context_creation_error(DOM::EventTarget& canvas);

OwnPtr<WebGLContextProxy> create_webgl_context_proxy(HTML::CanvasOwner const&, WebGLVersion, WebGLContextAttributes const&);
bool restore_webgl_context_proxy(WebGLContextProxy&, HTML::CanvasOwner const&, WebGLVersion, WebGLContextAttributes const&);

// Marks the owner canvas's content dirty and requests the repaint/commit machinery
// appropriate for it: display-list invalidation for a <canvas> element, a frame commit
// plus rendering update for an OffscreenCanvas.
void dispatch_canvas_content_update(HTML::CanvasOwner const&);

Gfx::IntSize canvas_owner_bitmap_size(HTML::CanvasOwner const&);

}
