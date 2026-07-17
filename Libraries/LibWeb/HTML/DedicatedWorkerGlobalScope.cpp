/*
 * Copyright (c) 2024, Andrew Kaster <akaster@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Bindings/DedicatedWorkerExposedInterfaces.h>
#include <LibWeb/Bindings/DedicatedWorkerGlobalScope.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/HTML/AnimationFrameCallbackDriver.h>
#include <LibWeb/HTML/DedicatedWorkerGlobalScope.h>
#include <LibWeb/HTML/EventHandler.h>
#include <LibWeb/HTML/EventNames.h>
#include <LibWeb/HTML/MessageEvent.h>
#include <LibWeb/HTML/MessagePort.h>
#include <LibWeb/HTML/Scripting/ExceptionReporter.h>
#include <LibWeb/HighResolutionTime/TimeOrigin.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Platform/Timer.h>
#include <LibWeb/WebIDL/AbstractOperations.h>
#include <LibWeb/WebIDL/DOMException.h>

namespace Web::HTML {

GC_DEFINE_ALLOCATOR(DedicatedWorkerGlobalScope);

DedicatedWorkerGlobalScope::DedicatedWorkerGlobalScope(JS::Realm& realm, GC::Ref<Web::Page> page)
    : WorkerGlobalScope(realm, page)
{
    m_legacy_platform_object_flags = LegacyPlatformObjectFlags { .has_global_interface_extended_attribute = true };
}

DedicatedWorkerGlobalScope::~DedicatedWorkerGlobalScope() = default;

void DedicatedWorkerGlobalScope::initialize_web_interfaces_impl()
{
    auto& realm = this->realm();
    add_dedicated_worker_exposed_interfaces(*this);

    DedicatedWorkerGlobalScopeGlobalMixin::initialize(realm, *this);

    Base::initialize_web_interfaces_impl();
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close
void DedicatedWorkerGlobalScope::close()
{
    // The close() method steps are to close a worker given this.
    close_a_worker();
}

// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-animationframeprovider-requestanimationframe
WebIDL::ExceptionOr<WebIDL::UnsignedLong> DedicatedWorkerGlobalScope::request_animation_frame(GC::Ref<WebIDL::CallbackType> callback)
{
    // 1. If this is not supported, then throw a "NotSupportedError" DOMException.
    if (!is_supported_animation_frame_provider())
        return WebIDL::NotSupportedError::create(realm(), "Worker's owner set does not reach a Document"_utf16);

    auto handle = animation_frame_callback_driver().add_idl_callback(callback);
    schedule_rendering_update();
    return handle;
}

// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#animationframeprovider-cancelanimationframe
WebIDL::ExceptionOr<void> DedicatedWorkerGlobalScope::cancel_animation_frame(WebIDL::UnsignedLong handle)
{
    // 1. If this is not supported, then throw a "NotSupportedError" DOMException.
    if (!is_supported_animation_frame_provider())
        return WebIDL::NotSupportedError::create(realm(), "Worker's owner set does not reach a Document"_utf16);

    // 2. Let callbacks be this's target object's map of animation frame callbacks.
    // 3. Remove callbacks[handle].
    if (m_animation_frame_callback_driver)
        (void)m_animation_frame_callback_driver->remove(handle);
    return {};
}

AnimationFrameCallbackDriver& DedicatedWorkerGlobalScope::animation_frame_callback_driver()
{
    if (!m_animation_frame_callback_driver)
        m_animation_frame_callback_driver = realm().create<AnimationFrameCallbackDriver>();
    return *m_animation_frame_callback_driver;
}

void DedicatedWorkerGlobalScope::run_animation_frame_callbacks_for_rendering_update(double now)
{
    if (m_animation_frame_callback_driver)
        m_animation_frame_callback_driver->run(now);
}

bool DedicatedWorkerGlobalScope::has_pending_animation_frame_callbacks() const
{
    return m_animation_frame_callback_driver && m_animation_frame_callback_driver->has_callbacks();
}

void DedicatedWorkerGlobalScope::finalize()
{
    Base::finalize();
    WindowOrWorkerGlobalScopeMixin::finalize();
}

void DedicatedWorkerGlobalScope::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_animation_frame_callback_driver);
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-postmessage-options
WebIDL::ExceptionOr<void> DedicatedWorkerGlobalScope::post_message(JS::Value message, Bindings::StructuredSerializeOptions const& options)
{
    // The postMessage(message, transfer) and postMessage(message, options) methods on DedicatedWorkerGlobalScope objects act as if,
    // when invoked, it immediately invoked the respective postMessage(message, transfer) and postMessage(message, options)
    // on the port, with the same arguments, and returned the same return value.
    return m_internal_port->post_message(message, options);
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-postmessage
WebIDL::ExceptionOr<void> DedicatedWorkerGlobalScope::post_message(JS::Value message, GC::RootVector<GC::Ref<JS::Object>> const& transfer)
{
    // The postMessage(message, transfer) and postMessage(message, options) methods on DedicatedWorkerGlobalScope objects act as if,
    // when invoked, it immediately invoked the respective postMessage(message, transfer) and postMessage(message, options)
    // on the port, with the same arguments, and returned the same return value.
    return m_internal_port->post_message(message, transfer);
}

WebIDL::CallbackType* DedicatedWorkerGlobalScope::onmessage()
{
    return event_handler_attribute(EventNames::message);
}

void DedicatedWorkerGlobalScope::set_onmessage(WebIDL::CallbackType* callback)
{
    set_event_handler_attribute(EventNames::message, callback);

    // NOTE: This onmessage attribute setter implicitly sets worker's underlying MessagePort's onmessage attribute, so this
    //       spec behavior also applies here:
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-ports:handler-messageeventtarget-onmessage
    // The first time a MessagePort object's onmessage IDL attribute is set, the port's port message queue must be enabled,
    // as if the start() method had been called.
    m_internal_port->start();
}

void DedicatedWorkerGlobalScope::set_onmessageerror(WebIDL::CallbackType* callback)
{
    set_event_handler_attribute(EventNames::messageerror, callback);
}

WebIDL::CallbackType* DedicatedWorkerGlobalScope::onmessageerror()
{
    return event_handler_attribute(EventNames::messageerror);
}

}
