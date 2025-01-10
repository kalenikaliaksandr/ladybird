/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Bindings/ExceptionOrUtils.h>
#include <LibWeb/WebIDL/ObservableArray.h>

namespace Web::WebIDL {

GC_DEFINE_ALLOCATOR(ObservableArray);

GC::Ref<ObservableArray> ObservableArray::create(JS::Realm& realm)
{
    auto prototype = realm.intrinsics().array_prototype();
    return realm.create<ObservableArray>(prototype);
}

ObservableArray::ObservableArray(Object& prototype)
    : JS::Array(prototype)
{
}

void ObservableArray::visit_edges(JS::Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_on_set_an_indexed_value);
    visitor.visit(m_on_delete_an_indexed_value);
}

void ObservableArray::set_on_set_an_indexed_value_callback(SetAnIndexedValueCallbackFunction&& callback)
{
    m_on_set_an_indexed_value = GC::create_function(heap(), move(callback));
}

void ObservableArray::set_on_delete_an_indexed_value_callback(DeleteAnIndexedValueCallbackFunction&& callback)
{
    m_on_delete_an_indexed_value = GC::create_function(heap(), move(callback));
}

JS::ThrowCompletionOr<bool> ObservableArray::internal_set(JS::PropertyKey const& property_key, JS::Value value, JS::Value receiver, JS::CacheablePropertyMetadata* metadata)
{
    auto result = TRY(Base::internal_set(property_key, value, receiver, metadata));
    if (property_key.is_number() && m_on_set_an_indexed_value)
        MUST(Bindings::throw_dom_exception_if_needed(vm(), [&] { return m_on_set_an_indexed_value->function()(value); }));
    return result;
}

JS::ThrowCompletionOr<bool> ObservableArray::internal_delete(JS::PropertyKey const& property_key)
{
    JS::Value value;
    auto maybe_property_descriptor = internal_get_own_property(property_key);
    if (!maybe_property_descriptor.is_error()) {
        auto property_descriptor = maybe_property_descriptor.value();
        if (property_descriptor.has_value()) {
            if (property_descriptor.value().value.has_value()) {
                value = property_descriptor.value().value.value();
            }
        }
    }

    auto result = JS::Array::internal_delete(property_key);
    if (property_key.is_number() && m_on_delete_an_indexed_value)
        MUST(Bindings::throw_dom_exception_if_needed(vm(), [&] { return m_on_delete_an_indexed_value->function()(value); }));
    return result;
}

JS::ThrowCompletionOr<void> ObservableArray::append(JS::Value value)
{
    indexed_properties().append(value);
    if (m_on_set_an_indexed_value) {
        auto maybe_exception = Bindings::throw_dom_exception_if_needed(vm(), [&] { return m_on_set_an_indexed_value->function()(value); });
        if (maybe_exception.is_error()) {
            indexed_properties().remove(indexed_properties().array_like_size() - 1);
            return maybe_exception.release_error();
        }
    }
    return {};
}

void ObservableArray::clear()
{
    while (!indexed_properties().is_empty()) {
        indexed_properties().storage()->take_first();
    }
}

}
