/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Bindings/PlatformObject.h>
#include <LibWeb/Forward.h>

namespace Web::WebGL {

class WebGLLoseContext : public Bindings::PlatformObject {
    WEB_PLATFORM_OBJECT(WebGLLoseContext, Bindings::PlatformObject);
    GC_DECLARE_ALLOCATOR(WebGLLoseContext);

public:
    static JS::ThrowCompletionOr<GC::Ref<JS::Object>> create(JS::Realm&, GC::Ref<WebGLRenderingContextBase>);

    void lose_context();
    void restore_context();

protected:
    void initialize(JS::Realm&) override;
    void visit_edges(Visitor&) override;

private:
    WebGLLoseContext(JS::Realm&, GC::Ref<WebGLRenderingContextBase>);

    GC::Ref<WebGLRenderingContextBase> m_context;
};

}
