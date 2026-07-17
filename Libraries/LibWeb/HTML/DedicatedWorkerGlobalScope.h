/*
 * Copyright (c) 2024, Andrew Kaster <akaster@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Bindings/DedicatedWorkerGlobalScope.h>
#include <LibWeb/Bindings/WorkerGlobalScope.h>
#include <LibWeb/Export.h>
#include <LibWeb/HTML/WorkerGlobalScope.h>

namespace WebWorker {
class WorkerHost;
}

namespace Web::HTML {

class WEB_API DedicatedWorkerGlobalScope
    : public WorkerGlobalScope
    , public Bindings::DedicatedWorkerGlobalScopeGlobalMixin {
    WEB_PLATFORM_OBJECT(DedicatedWorkerGlobalScope, WorkerGlobalScope);
    GC_DECLARE_ALLOCATOR(DedicatedWorkerGlobalScope);

public:
    static constexpr bool OVERRIDES_FINALIZE = true;

    virtual ~DedicatedWorkerGlobalScope() override;

    WebIDL::ExceptionOr<void> post_message(JS::Value message, Bindings::StructuredSerializeOptions const&);
    WebIDL::ExceptionOr<void> post_message(JS::Value message, GC::RootVector<GC::Ref<JS::Object>> const& transfer);

    void close();

    WebIDL::ExceptionOr<WebIDL::UnsignedLong> request_animation_frame(GC::Ref<WebIDL::CallbackType>);
    WebIDL::ExceptionOr<void> cancel_animation_frame(WebIDL::UnsignedLong handle);

    // https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#the-animationframeprovider-interface
    // Whether this scope's owner set reaches a Document through supported
    // dedicated workers; computed by the spawning realm and handed to the
    // worker process at startup.
    bool is_supported_animation_frame_provider() const { return m_is_supported_animation_frame_provider; }
    void set_is_supported_animation_frame_provider(Badge<WebWorker::WorkerHost>, bool supported) { m_is_supported_animation_frame_provider = supported; }

    WebIDL::CallbackType* onmessage();
    void set_onmessage(WebIDL::CallbackType* callback);

    WebIDL::CallbackType* onmessageerror();
    void set_onmessageerror(WebIDL::CallbackType* callback);

    virtual void finalize() override;

private:
    DedicatedWorkerGlobalScope(JS::Realm&, GC::Ref<Web::Page>);

    virtual void initialize_web_interfaces_impl() override;
    virtual void visit_edges(Cell::Visitor&) override;

    virtual void run_animation_frame_callbacks_for_rendering_update(double now) override;
    virtual bool has_pending_animation_frame_callbacks() const override;

    AnimationFrameCallbackDriver& animation_frame_callback_driver();

    GC::Ptr<AnimationFrameCallbackDriver> m_animation_frame_callback_driver;
    bool m_is_supported_animation_frame_provider { false };
};

}
