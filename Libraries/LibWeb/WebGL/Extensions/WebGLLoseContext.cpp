/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Runtime/Realm.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/WebGLLoseContext.h>
#include <LibWeb/WebGL/Extensions/WebGLLoseContext.h>
#include <LibWeb/WebGL/WebGLRenderingContextBase.h>

namespace Web::WebGL {

GC_DEFINE_ALLOCATOR(WebGLLoseContext);

JS::ThrowCompletionOr<GC::Ref<JS::Object>> WebGLLoseContext::create(JS::Realm& realm, GC::Ref<WebGLRenderingContextBase> context)
{
    return realm.create<WebGLLoseContext>(realm, context);
}

WebGLLoseContext::WebGLLoseContext(JS::Realm& realm, GC::Ref<WebGLRenderingContextBase> context)
    : PlatformObject(realm)
    , m_context(context)
{
}

void WebGLLoseContext::lose_context()
{
    m_context->lose_context_from_extension();
}

void WebGLLoseContext::restore_context()
{
    m_context->restore_context_from_extension();
}

void WebGLLoseContext::initialize(JS::Realm& realm)
{
    WEB_SET_PROTOTYPE_FOR_INTERFACE(WebGLLoseContext);
    Base::initialize(realm);
}

void WebGLLoseContext::visit_edges(Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_context);
}

}
