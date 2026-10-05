/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/RefCounted.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Root.h>
#include <LibGC/RootVector.h>
#include <LibJS/Runtime/Array.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/ErrorTypes.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/GlobalObject.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/NativeFunction.h>
#include <LibJS/Runtime/Object.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyDescriptor.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Symbol.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// Objects, their properties and internal methods, native functions and arrays, as LibJS's users use them. The same
// expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

namespace {

struct VMWithRealm {
    VMWithRealm()
        : vm(VM::create())
        , realm_execution_context(MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr)))
    {
    }

    ~VMWithRealm()
    {
        while (!vm->execution_context_stack().is_empty())
            vm->pop_execution_context();
    }

    Realm& realm() { return *realm_execution_context->realm; }

    ThrowCompletionOr<Value> evaluate(StringView source)
    {
        auto source_text = Utf16String::from_utf8(source);
        auto script = Script::parse(source_text.utf16_view(), realm());
        VERIFY(!script.is_error());
        return vm->run(script.value());
    }

    // The completion value as a string, or "uncaught <name>: <message>" for an exception that escaped the script.
    Utf16String result_of(StringView source)
    {
        auto result = evaluate(source);
        if (result.is_error())
            return Utf16String::formatted("uncaught {}", error_text(result.error_value()));
        return result.value().to_utf16_string_without_side_effects();
    }

    Utf16String error_text(Value error)
    {
        if (!error.is_object())
            return error.to_utf16_string_without_side_effects();
        auto& object = error.as_object();
        return Utf16String::formatted("{}: {}", object.get_without_side_effects(vm->names.name).to_utf16_string_without_side_effects(), object.get_without_side_effects(vm->names.message).to_utf16_string_without_side_effects());
    }

    void define_global(StringView name, Value value)
    {
        realm().global_object().define_direct_property(Utf16FlyString::from_utf8(name), value, default_attributes);
    }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

PropertyKey key(StringView name)
{
    return PropertyKey { Utf16FlyString::from_utf8(name) };
}

void const* address_of(Value value)
{
    return &value.as_object();
}

// The prototype that the object's shape records, which LibJS's users read through a const object.
Object const* prototype_of(Object const& object)
{
    return object.prototype();
}

bool starts_with(Utf16String const& text, StringView prefix)
{
    return text.to_utf8().starts_with_bytes(prefix);
}

struct RefCountedCounter : public RefCounted<RefCountedCounter> {
    i32 value { 0 };
};

Value string_value(VM& vm, StringView string)
{
    return PrimitiveString::create(vm, Utf16String::from_utf8(string));
}

Utf16String joined_keys(ReadonlySpan<Value> keys)
{
    Vector<Utf16String> strings;
    for (auto key : keys)
        strings.append(key.to_utf16_string_without_side_effects());
    return Utf16String::join(","sv, strings);
}

// Returns its arguments joined with the this value's "tag" property, and throws without arguments.
JS_DEFINE_NATIVE_FUNCTION(join_arguments_with_tag)
{
    if (vm.argument_count() == 0)
        return vm.throw_completion<TypeError>(ErrorType::BadArgCountOne, "joinArgumentsWithTag");
    Vector<Utf16String> parts;
    if (vm.this_value().is_object())
        parts.append(TRY(TRY(vm.this_value().as_object().get(key("tag"sv))).to_utf16_string(vm)));
    for (size_t i = 0; i < vm.argument_count(); ++i)
        parts.append(TRY(vm.argument(i).to_utf16_string(vm)));
    return PrimitiveString::create(vm, Utf16String::join("-"sv, parts));
}

size_t s_raw_getter_calls = 0;

JS_DEFINE_NATIVE_FUNCTION(count_raw_getter_calls)
{
    ++s_raw_getter_calls;
    return Value(static_cast<i32>(s_raw_getter_calls));
}

Value s_last_raw_setter_argument;

JS_DEFINE_NATIVE_FUNCTION(remember_raw_setter_argument)
{
    s_last_raw_setter_argument = vm.argument(0);
    return js_undefined();
}

JS_DEFINE_NATIVE_FUNCTION(return_this_value)
{
    return vm.this_value();
}

size_t s_intrinsic_accessor_calls = 0;

Value create_intrinsic_value(Realm& realm)
{
    ++s_intrinsic_accessor_calls;
    return JS::Array::create_from(realm, { Value(1), Value(2) });
}

}

TEST_CASE(objects_and_their_prototypes)
{
    VMWithRealm vm_with_realm;
    auto& realm = vm_with_realm.realm();
    auto object_prototype = realm.intrinsics().object_prototype();

    auto object = Object::create(realm, object_prototype);
    EXPECT_EQ(prototype_of(*object), object_prototype.ptr());
    EXPECT_EQ(object->shape().prototype(), object_prototype.ptr());
    EXPECT_EQ(&object->shape().realm(), &realm);
    EXPECT_EQ(MUST(object->internal_get_prototype_of()), object_prototype.ptr());
    EXPECT_EQ(MUST(object->ordinary_get_prototype_of()), object_prototype.ptr());
    EXPECT(object->extensible());
    EXPECT(MUST(object->is_extensible()));
    EXPECT(!object->is_function());
    EXPECT(!object->is_platform_object());
    EXPECT(object->eligible_for_own_property_enumeration_fast_path());
    EXPECT_EQ(object->class_name(), "Object"sv);
    EXPECT_EQ(&object->vm(), vm_with_realm.vm.ptr());

    auto without_prototype = Object::create(realm, nullptr);
    EXPECT(!prototype_of(*without_prototype));
    EXPECT(!MUST(without_prototype->internal_get_prototype_of()));

    without_prototype->set_prototype(object);
    EXPECT_EQ(prototype_of(*without_prototype), object.ptr());
    EXPECT(MUST(without_prototype->internal_set_prototype_of(object_prototype.ptr())));
    EXPECT_EQ(prototype_of(*without_prototype), object_prototype.ptr());
    EXPECT(MUST(without_prototype->ordinary_set_prototype_of(nullptr)));
    EXPECT(!prototype_of(*without_prototype));

    // A prototype cycle is refused.
    EXPECT(!MUST(object_prototype->internal_set_prototype_of(object.ptr())));

    EXPECT(MUST(object->internal_prevent_extensions()));
    EXPECT(!object->extensible());
    EXPECT(!MUST(object->internal_is_extensible()));
    EXPECT(!MUST(object->ordinary_is_extensible()));
    EXPECT(!MUST(object->internal_set_prototype_of(nullptr)));
    auto another = Object::create(realm, object_prototype);
    EXPECT(MUST(another->ordinary_prevent_extensions()));
    EXPECT(!another->extensible());

    // An immutable prototype can only be "set" to what it already is.
    auto immutable = Object::create(realm, object_prototype);
    EXPECT(MUST(immutable->set_immutable_prototype(object_prototype.ptr())));
    EXPECT(!MUST(immutable->set_immutable_prototype(nullptr)));
    EXPECT_EQ(prototype_of(*immutable), object_prototype.ptr());

    // Prototype conversion and cache invalidation keep the properties of an object.
    auto prototype = Object::create(realm, object_prototype);
    prototype->define_direct_property(key("inherited"sv), Value(5), default_attributes);
    prototype->convert_to_prototype_if_needed();
    prototype->invalidate_property_lookup_caches();
    auto heir = Object::create(realm, prototype);
    EXPECT_EQ(MUST(heir->get(key("inherited"sv))), Value(5));
}

TEST_CASE(property_operations)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto object = Object::create(realm, realm.intrinsics().object_prototype());
    object->define_direct_property(key("direct"sv), Value(1), default_attributes);
    object->define_direct_property(key("hidden"sv), Value(2), Attribute::Writable | Attribute::Configurable);
    object->define_direct_property(vm.well_known_symbol_to_string_tag(), string_value(vm, "Tagged"sv), Attribute::Configurable);
    vm_with_realm.define_global("object"sv, object);

    EXPECT_EQ(MUST(object->get(key("direct"sv))), Value(1));
    EXPECT_EQ(MUST(object->get(PropertyKey { 7u })), js_undefined());
    EXPECT(MUST(object->get(key("toString"sv))).is_function());
    EXPECT_EQ(vm_with_realm.result_of("String(object)"sv), "[object Tagged]"sv);

    EXPECT(MUST(object->has_property(key("toString"sv))));
    EXPECT(!MUST(object->has_own_property(key("toString"sv))));
    EXPECT(MUST(object->has_own_property(key("hidden"sv))));
    EXPECT(MUST(object->internal_has_property(key("direct"sv))));
    EXPECT(MUST(object->ordinary_has_property(key("direct"sv))));
    EXPECT(object->storage_has(key("direct"sv)));
    EXPECT(!object->storage_has(key("toString"sv)));

    MUST(object->set(key("direct"sv), Value(10), Object::ShouldThrowExceptions::Yes));
    EXPECT_EQ(vm_with_realm.result_of("object.direct"sv), "10"sv);
    EXPECT(MUST(object->internal_set(key("set"sv), Value(11), object)));
    EXPECT(MUST(object->ordinary_set(key("ordinarySet"sv), Value(12), object)));
    EXPECT(MUST(object->ordinary_set_with_own_descriptor(key("withOwnDescriptor"sv), Value(13), object, {})));
    EXPECT_EQ(vm_with_realm.result_of("[object.set, object.ordinarySet, object.withOwnDescriptor].join()"sv), "11,12,13"sv);

    EXPECT(MUST(object->create_data_property(key("created"sv), Value(3))));
    EXPECT(MUST(object->create_data_property_or_throw(key("createdOrThrown"sv), Value(4))));
    object->create_non_enumerable_data_property_or_throw(key("nonEnumerable"sv), Value(5));
    EXPECT_EQ(vm_with_realm.result_of("JSON.stringify(Object.getOwnPropertyDescriptor(object, 'nonEnumerable'))"sv), R"({"value":5,"writable":true,"enumerable":false,"configurable":true})"sv);

    // CreateDataProperty reports where a new property is stored.
    Optional<u32> new_property_offset;
    EXPECT(MUST(object->create_data_property(key("withOffset"sv), Value(6), &new_property_offset)));
    EXPECT(new_property_offset.has_value());
    auto own_descriptor = MUST(object->ordinary_get_own_property(key("withOffset"sv)));
    VERIFY(own_descriptor.has_value());
    EXPECT_EQ(own_descriptor->property_offset, new_property_offset);
    EXPECT_EQ(own_descriptor->value, Value(6));

    PropertyDescriptor defined { .value = Value(7), .writable = false, .enumerable = true, .configurable = false };
    MUST(object->define_property_or_throw(key("defined"sv), defined));
    EXPECT(defined.property_offset.has_value());
    auto descriptor = MUST(object->internal_get_own_property(key("defined"sv)));
    VERIFY(descriptor.has_value());
    EXPECT(descriptor->is_data_descriptor());
    EXPECT(!descriptor->is_accessor_descriptor());
    EXPECT_EQ(descriptor->value, Value(7));
    EXPECT_EQ(descriptor->writable, false);
    EXPECT_EQ(descriptor->enumerable, true);
    EXPECT_EQ(descriptor->configurable, false);
    EXPECT(!MUST(object->internal_get_own_property(key("missing"sv))).has_value());

    // Redefining a non-configurable property fails, which DefinePropertyOrThrow turns into a TypeError.
    PropertyDescriptor redefinition { .value = Value(8) };
    EXPECT(!MUST(object->internal_define_own_property(key("defined"sv), redefinition)));
    EXPECT(!MUST(object->ordinary_define_own_property(key("defined"sv), redefinition)));
    auto define_or_throw = object->define_property_or_throw(key("defined"sv), redefinition);
    VERIFY(define_or_throw.is_error());
    EXPECT(starts_with(vm_with_realm.error_text(define_or_throw.error_value()), "TypeError: "sv));

    // A precomputed [[GetOwnProperty]] result stands in for the one the definition would look up.
    Optional<PropertyDescriptor> no_existing_property;
    PropertyDescriptor fresh { .value = Value(9), .writable = true, .enumerable = true, .configurable = true };
    EXPECT(MUST(object->internal_define_own_property(key("fresh"sv), fresh, &no_existing_property)));
    EXPECT(fresh.property_offset.has_value());
    EXPECT_EQ(MUST(object->get(key("fresh"sv))), Value(9));

    EXPECT(MUST(object->internal_delete(key("fresh"sv))));
    EXPECT(MUST(object->ordinary_delete(key("created"sv))));
    MUST(object->delete_property_or_throw(key("createdOrThrown"sv)));
    EXPECT(!MUST(object->internal_delete(key("defined"sv))));
    EXPECT(object->delete_property_or_throw(key("defined"sv)).is_error());
    object->storage_delete(key("withOffset"sv));
    EXPECT(!object->storage_has(key("withOffset"sv)));

    EXPECT_EQ(joined_keys(MUST(object->internal_own_property_keys())), "direct,hidden,set,ordinarySet,withOwnDescriptor,nonEnumerable,defined,Symbol(Symbol.toStringTag)"sv);
    EXPECT_EQ(joined_keys(MUST(object->ordinary_own_property_keys())), "direct,hidden,set,ordinarySet,withOwnDescriptor,nonEnumerable,defined,Symbol(Symbol.toStringTag)"sv);
    EXPECT_EQ(joined_keys(MUST(object->enumerable_own_property_names(Object::PropertyKind::Key))), "direct,set,ordinarySet,withOwnDescriptor,defined"sv);
    EXPECT_EQ(joined_keys(MUST(object->enumerable_own_property_names(Object::PropertyKind::Value))), "10,11,12,13,7"sv);
    auto entries = MUST(object->enumerable_own_property_names(Object::PropertyKind::KeyAndValue));
    EXPECT_EQ(entries.size(), 5u);
    EXPECT_EQ(MUST(entries[0].to_utf16_string(vm)), "direct,10"sv);

    EXPECT_EQ(object->get_without_side_effects(key("direct"sv)), Value(10));
    EXPECT(object->get_without_side_effects(key("toString"sv)).is_function());
    EXPECT(object->get_without_side_effects(key("missing"sv)).is_undefined());
}

TEST_CASE(property_operations_that_throw)
{
    VMWithRealm vm_with_realm;
    auto& realm = vm_with_realm.realm();

    auto throwing = MUST(vm_with_realm.evaluate(R"(
        new Proxy({}, {
            get() { throw new RangeError("get trap"); },
            has() { throw new RangeError("has trap"); },
            ownKeys() { throw new RangeError("ownKeys trap"); },
        }))"sv));
    auto& proxy = throwing.as_object();
    EXPECT(!is<FunctionObject>(proxy));

    auto get = proxy.get(key("anything"sv));
    VERIFY(get.is_error());
    EXPECT_EQ(vm_with_realm.error_text(get.error_value()), "RangeError: get trap"sv);
    auto has = proxy.has_property(key("anything"sv));
    VERIFY(has.is_error());
    EXPECT_EQ(vm_with_realm.error_text(has.error_value()), "RangeError: has trap"sv);
    auto keys = proxy.internal_own_property_keys();
    VERIFY(keys.is_error());
    EXPECT_EQ(vm_with_realm.error_text(keys.error_value()), "RangeError: ownKeys trap"sv);
    auto names = proxy.enumerable_own_property_names(Object::PropertyKind::Key);
    VERIFY(names.is_error());
    EXPECT_EQ(vm_with_realm.error_text(names.error_value()), "RangeError: ownKeys trap"sv);

    // Integrity levels.
    auto object = Object::create(realm, realm.intrinsics().object_prototype());
    object->define_direct_property(key("value"sv), Value(1), default_attributes);
    EXPECT(!MUST(object->test_integrity_level(Object::IntegrityLevel::Frozen)));
    EXPECT(MUST(object->set_integrity_level(Object::IntegrityLevel::Frozen)));
    EXPECT(MUST(object->test_integrity_level(Object::IntegrityLevel::Frozen)));
    EXPECT(MUST(object->test_integrity_level(Object::IntegrityLevel::Sealed)));
    EXPECT(!MUST(object->is_extensible()));

    EXPECT(!MUST(object->create_data_property(key("added"sv), Value(2))));
    auto create_or_throw = object->create_data_property_or_throw(key("added"sv), Value(2));
    VERIFY(create_or_throw.is_error());
    EXPECT(starts_with(vm_with_realm.error_text(create_or_throw.error_value()), "TypeError: "sv));

    MUST(object->set(key("value"sv), Value(3), Object::ShouldThrowExceptions::No));
    EXPECT_EQ(MUST(object->get(key("value"sv))), Value(1));
    auto set_or_throw = object->set(key("value"sv), Value(3), Object::ShouldThrowExceptions::Yes);
    VERIFY(set_or_throw.is_error());
    EXPECT(starts_with(vm_with_realm.error_text(set_or_throw.error_value()), "TypeError: "sv));

    auto sealed = Object::create(realm, nullptr);
    sealed->define_direct_property(key("value"sv), Value(1), default_attributes);
    EXPECT(MUST(sealed->set_integrity_level(Object::IntegrityLevel::Sealed)));
    EXPECT(MUST(sealed->test_integrity_level(Object::IntegrityLevel::Sealed)));
    EXPECT(!MUST(sealed->test_integrity_level(Object::IntegrityLevel::Frozen)));
    MUST(sealed->set(key("value"sv), Value(4), Object::ShouldThrowExceptions::Yes));
    EXPECT_EQ(MUST(sealed->get(key("value"sv))), Value(4));
}

TEST_CASE(internal_methods_dispatch_to_exotic_objects)
{
    VMWithRealm vm_with_realm;

    auto proxy_value = MUST(vm_with_realm.evaluate(R"(
        globalThis.log = [];
        globalThis.target = { own: 1 };
        new Proxy(target, {
            get(target, key, receiver) { log.push("get " + String(key)); return key === "answer" ? 42 : Reflect.get(target, key, receiver); },
            set(target, key, value, receiver) { log.push("set " + String(key)); return Reflect.set(target, key, value); },
            has(target, key) { log.push("has " + String(key)); return key === "magic" || Reflect.has(target, key); },
            deleteProperty(target, key) { log.push("delete " + String(key)); return Reflect.deleteProperty(target, key); },
            defineProperty(target, key, descriptor) { log.push("define " + String(key)); return Reflect.defineProperty(target, key, descriptor); },
            getOwnPropertyDescriptor(target, key) { log.push("getOwnProperty " + String(key)); return Reflect.getOwnPropertyDescriptor(target, key); },
            ownKeys(target) { log.push("ownKeys"); return Reflect.ownKeys(target); },
            getPrototypeOf(target) { log.push("getPrototypeOf"); return null; },
            setPrototypeOf(target, prototype) { log.push("setPrototypeOf"); return false; },
            isExtensible(target) { log.push("isExtensible"); return Reflect.isExtensible(target); },
            preventExtensions(target) { log.push("preventExtensions"); return false; },
        }))"sv));
    auto& proxy = proxy_value.as_object();

    EXPECT_EQ(MUST(proxy.internal_get(key("answer"sv), proxy_value)), Value(42));
    EXPECT_EQ(MUST(proxy.internal_get(key("own"sv), proxy_value)), Value(1));
    EXPECT(MUST(proxy.internal_has_property(key("magic"sv))));
    EXPECT(MUST(proxy.internal_set(key("own"sv), Value(2), proxy_value)));
    PropertyDescriptor descriptor { .value = Value(3), .writable = true, .enumerable = true, .configurable = true };
    EXPECT(MUST(proxy.internal_define_own_property(key("defined"sv), descriptor)));
    EXPECT_EQ(MUST(proxy.internal_get_own_property(key("defined"sv)))->value, Value(3));
    EXPECT(MUST(proxy.internal_delete(key("defined"sv))));
    EXPECT_EQ(joined_keys(MUST(proxy.internal_own_property_keys())), "own"sv);
    EXPECT(!MUST(proxy.internal_get_prototype_of()));
    EXPECT(!MUST(proxy.internal_set_prototype_of(nullptr)));
    EXPECT(MUST(proxy.internal_is_extensible()));
    EXPECT(!MUST(proxy.internal_prevent_extensions()));
    EXPECT_EQ(vm_with_realm.result_of("log.join()"sv),
        "get answer,get own,has magic,set own,define defined,getOwnProperty defined,delete defined,ownKeys,getPrototypeOf,setPrototypeOf,isExtensible,preventExtensions"sv);
    EXPECT_EQ(vm_with_realm.result_of("target.own"sv), "2"sv);
}

TEST_CASE(for_in_enumeration)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;

    auto object = MUST(vm_with_realm.evaluate("const prototype = { inherited: 1, shadowed: 2 }; Object.defineProperty(prototype, 'hidden', { value: 3 }); const object = Object.create(prototype); object.own = 4; object.shadowed = 5; object"sv));

    Vector<Utf16String> keys;
    auto stopped_with = object.as_object().enumerate_object_properties([&](Value key) -> Optional<Completion> {
        keys.append(MUST(key.to_utf16_string(vm)));
        return {};
    });
    EXPECT(!stopped_with.has_value());
    EXPECT_EQ(Utf16String::join(","sv, keys), "own,shadowed,inherited"sv);

    keys.clear();
    stopped_with = object.as_object().enumerate_object_properties([&](Value key) -> Optional<Completion> {
        keys.append(MUST(key.to_utf16_string(vm)));
        return normal_completion(Value(keys.size()));
    });
    VERIFY(stopped_with.has_value());
    EXPECT_EQ(stopped_with->type(), Completion::Type::Normal);
    EXPECT_EQ(stopped_with->value(), Value(1));
    EXPECT_EQ(keys.size(), 1u);

    auto throwing = MUST(vm_with_realm.evaluate("new Proxy({}, { ownKeys() { throw new Error('no keys'); } })"sv));
    stopped_with = throwing.as_object().enumerate_object_properties([&](Value) -> Optional<Completion> {
        VERIFY_NOT_REACHED();
    });
    VERIFY(stopped_with.has_value());
    EXPECT_EQ(stopped_with->type(), Completion::Type::Throw);
    EXPECT_EQ(vm_with_realm.error_text(stopped_with->value()), "Error: no keys"sv);
}

TEST_CASE(accessors)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto object = Object::create(realm, realm.intrinsics().object_prototype());
    vm_with_realm.define_global("object"sv, object);

    s_raw_getter_calls = 0;
    object->define_native_accessor(realm, key("raw"sv), count_raw_getter_calls, remember_raw_setter_argument, Attribute::Configurable);
    EXPECT_EQ(vm_with_realm.result_of("object.raw = 'assigned'; object.raw + object.raw"sv), "3"sv);
    EXPECT_EQ(s_last_raw_setter_argument.to_utf16_string_without_side_effects(), "assigned"sv);
    EXPECT_EQ(vm_with_realm.result_of("const raw = Object.getOwnPropertyDescriptor(object, 'raw'); [raw.get.name, raw.get.length, raw.set.name, raw.set.length, raw.enumerable].join()"sv), "get raw,0,set raw,1,false"sv);
    EXPECT(object->get_without_side_effects(key("raw"sv)).is_accessor());

    object->define_native_accessor(realm, key("getterOnly"sv), return_this_value, nullptr, default_attributes);
    EXPECT_EQ(vm_with_realm.result_of("object.getterOnly === object && Object.getOwnPropertyDescriptor(object, 'getterOnly').set === undefined"sv), "true"sv);

    i32 closure_value = 0;
    object->define_native_accessor(
        realm, key("closure"sv),
        [&](VM&) -> ThrowCompletionOr<Value> { return Value(closure_value); },
        [&](VM& vm) -> ThrowCompletionOr<Value> {
            closure_value = TRY(vm.argument(0).to_i32(vm));
            return js_undefined();
        },
        default_attributes);
    EXPECT_EQ(vm_with_realm.result_of("object.closure = 21; object.closure * 2"sv), "42"sv);
    EXPECT_EQ(closure_value, 21);
    EXPECT_EQ(vm_with_realm.result_of("Object.getOwnPropertyDescriptor(object, 'closure').get.name"sv), "get closure"sv);

    // A direct accessor whose getter or setter is absent keeps the other one when it is defined again.
    auto getter = NativeFunction::create(realm, count_raw_getter_calls, 0, key("direct"sv), &realm, "get"sv);
    auto setter = NativeFunction::create(realm, remember_raw_setter_argument, 1, key("direct"sv), &realm, "set"sv);
    object->define_direct_accessor(key("direct"sv), getter, nullptr, Attribute::Configurable | Attribute::Enumerable);
    object->define_direct_accessor(key("direct"sv), nullptr, setter, Attribute::Configurable | Attribute::Enumerable);
    auto direct = MUST(object->internal_get_own_property(key("direct"sv)));
    VERIFY(direct.has_value());
    EXPECT(direct->is_accessor_descriptor());
    EXPECT_EQ(direct->get->ptr(), static_cast<FunctionObject*>(getter.ptr()));
    EXPECT_EQ(direct->set->ptr(), static_cast<FunctionObject*>(setter.ptr()));
    EXPECT_EQ(direct->enumerable, true);

    // A cached accessor runs its getter once until the cached value is cleared.
    s_raw_getter_calls = 0;
    object->define_direct_cached_accessor(key("cached"sv), getter, nullptr, Attribute::Configurable);
    EXPECT_EQ(vm_with_realm.result_of("[object.cached, object.cached].join()"sv), "1,1"sv);
    object->clear_cached_accessor_value(key("cached"sv));
    EXPECT_EQ(vm_with_realm.result_of("[object.cached, object.cached].join()"sv), "2,2"sv);

    // An intrinsic accessor computes its value the first time the property is read.
    s_intrinsic_accessor_calls = 0;
    object->define_intrinsic_accessor(key("intrinsic"sv), Attribute::Writable | Attribute::Configurable, create_intrinsic_value);
    EXPECT_EQ(s_intrinsic_accessor_calls, 0u);
    EXPECT_EQ(vm_with_realm.result_of("object.intrinsic === object.intrinsic && object.intrinsic.join()"sv), "1,2"sv);
    EXPECT_EQ(s_intrinsic_accessor_calls, 1u);
    EXPECT_EQ(vm_with_realm.result_of("JSON.stringify(Object.getOwnPropertyDescriptor(object, 'intrinsic'))"sv), R"({"value":[1,2],"writable":true,"enumerable":false,"configurable":true})"sv);

    // Engine-private properties are invisible to scripts.
    auto private_symbol = Symbol::create_private(vm);
    object->set_engine_private_property(private_symbol, Value(99));
    EXPECT_EQ(vm_with_realm.result_of("Reflect.ownKeys(object).length"sv), "6"sv);
}

TEST_CASE(native_functions)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto raw = NativeFunction::create(realm, join_arguments_with_tag, 2, key("joinArgumentsWithTag"sv));
    vm_with_realm.define_global("raw"sv, raw);
    EXPECT(is<FunctionObject>(*raw));
    EXPECT(is<NativeFunction>(static_cast<Object&>(*raw)));
    EXPECT(raw->is_function());
    EXPECT(!raw->is_ecmascript_function_object());
    EXPECT_EQ(raw->realm(), &realm);
    EXPECT_EQ(prototype_of(*raw), realm.intrinsics().function_prototype().ptr());
    EXPECT_EQ(vm_with_realm.result_of("[raw.name, raw.length, typeof raw].join()"sv), "joinArgumentsWithTag,2,function"sv);
    EXPECT_EQ(vm_with_realm.result_of("raw.call({ tag: 'T' }, 1, 'b')"sv), "T-1-b"sv);
    EXPECT_EQ(vm_with_realm.result_of("try { raw(); } catch (error) { error instanceof TypeError }"sv), "true"sv);
    EXPECT_EQ(vm_with_realm.result_of("try { new raw(1); } catch (error) { error instanceof TypeError }"sv), "true"sv);
    EXPECT(raw->name_for_call_stack().is_empty());

    auto prefixed = NativeFunction::create(realm, join_arguments_with_tag, 0, vm.well_known_symbol_iterator(), {}, "get"sv);
    vm_with_realm.define_global("prefixed"sv, prefixed);
    EXPECT_EQ(vm_with_realm.result_of("prefixed.name"sv), "get [Symbol.iterator]"sv);

    // A closure captures what it needs, and its captures stay alive while the function does.
    auto counter = make_ref_counted<RefCountedCounter>();
    auto closure = NativeFunction::create(realm, [counter, &realm](VM& vm) -> ThrowCompletionOr<Value> {
        counter->value += TRY(vm.argument(0).to_i32(vm));
        if (vm.current_realm() != &realm)
            return vm.throw_completion<TypeError>("another realm"sv);
        return Value(counter->value); }, 1, key("count"sv));
    vm_with_realm.define_global("count"sv, closure);
    EXPECT_EQ(vm_with_realm.result_of("count(2); count(3)"sv), "5"sv);
    EXPECT_EQ(counter->value, 5);
    EXPECT_EQ(vm_with_realm.result_of("count.name + count.length"sv), "count1"sv);
    EXPECT(is<NativeFunction>(static_cast<Object&>(*closure)));

    // Named functions have no "length" or "name" of their own.
    auto named_raw = NativeFunction::create(realm, "namedRaw"_utf16_fly_string, return_this_value);
    i32 named_closure_result = 7;
    auto named_closure = NativeFunction::create(realm, "namedClosure"_utf16_fly_string, [named_closure_result](VM&) -> ThrowCompletionOr<Value> { return Value(named_closure_result); });
    vm_with_realm.define_global("namedRaw"sv, named_raw);
    vm_with_realm.define_global("namedClosure"sv, named_closure);
    EXPECT_EQ(vm_with_realm.result_of("[Object.getOwnPropertyNames(namedRaw).length, Object.getOwnPropertyNames(namedClosure).length, namedClosure()].join()"sv), "0,0,7"sv);
    EXPECT_EQ(named_raw->name_for_call_stack(), "namedRaw"sv);
    EXPECT_EQ(named_closure->name_for_call_stack(), "namedClosure"sv);

    // Methods of objects.
    auto object = Object::create(realm, realm.intrinsics().object_prototype());
    vm_with_realm.define_global("object"sv, object);
    MUST(object->set(key("tag"sv), string_value(vm, "O"sv), Object::ShouldThrowExceptions::Yes));
    object->define_native_function(realm, key("join"sv), join_arguments_with_tag, 1, Attribute::Writable | Attribute::Configurable);
    double factor = 2;
    object->define_native_function(realm, key("twice"sv), [factor](VM& vm) -> ThrowCompletionOr<Value> { return Value(TRY(vm.argument(0).to_double(vm)) * factor); }, 1, default_attributes);
    EXPECT_EQ(vm_with_realm.result_of("[object.join('x'), object.twice(4), object.join.name, object.twice.length, Object.keys(object).join()].join()"sv), "O-x,8,join,1,tag,twice"sv);

    // A direct getter function is a raw native function that also tells the interpreter where to find its result.
    auto direct_getter = DirectGetterFunction::create(realm, return_this_value, 0, key("direct"sv), DirectGetterConfiguration { .wrapper_implementation_offset = 8, .implementation_value_offset = 8, .main_world_wrapper_offset = 8, .weak_impl_value_offset = 8 }, "get"sv);
    vm_with_realm.define_global("directGetter"sv, direct_getter);
    EXPECT(is<DirectGetterFunction>(static_cast<Object&>(*direct_getter)));
    EXPECT(is<NativeFunction>(static_cast<Object&>(*direct_getter)));
    EXPECT(!is<DirectGetterFunction>(static_cast<Object&>(*raw)));
    EXPECT_EQ(vm_with_realm.result_of("[directGetter.name, directGetter.length, directGetter.call(object) === object].join()"sv), "get direct,0,true"sv);

    // Script functions are functions, but no native ones.
    auto script_function = MUST(vm_with_realm.evaluate("(function scriptFunction() {})"sv));
    EXPECT(is<FunctionObject>(script_function.as_object()));
    EXPECT(!is<NativeFunction>(script_function.as_object()));
    EXPECT(script_function.as_object().is_ecmascript_function_object());
    EXPECT_EQ(script_function.as_function().realm(), &realm);
    EXPECT_EQ(script_function.as_function().name_for_call_stack(), "scriptFunction"sv);
    EXPECT(!is<FunctionObject>(*Object::create(realm, nullptr)));
}

TEST_CASE(arrays)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto empty = MUST(JS::Array::create(realm, 3));
    vm_with_realm.define_global("empty"sv, empty);
    EXPECT(is<JS::Array>(static_cast<Object&>(*empty)));
    EXPECT(!is<JS::Array>(*Object::create(realm, realm.intrinsics().array_prototype())));
    EXPECT_EQ(prototype_of(*empty), realm.intrinsics().array_prototype().ptr());

    // Only Array exotic objects are arrays, those of classes that extend Array included.
    auto is_array_from_script = [&](StringView source) { return is<JS::Array>(MUST(vm_with_realm.evaluate(source)).as_object()); };
    EXPECT(is_array_from_script("[1, 2]"sv));
    EXPECT(is_array_from_script("new (class extends Array {})(3)"sv));
    EXPECT(!is_array_from_script("({ length: 1, 0: 'a' })"sv));
    EXPECT(!is_array_from_script("(function () { return arguments; })(1)"sv));
    EXPECT(!is_array_from_script("new Proxy([], {})"sv));
    EXPECT(!is_array_from_script("new Uint8Array(2)"sv));
    EXPECT_EQ(empty->indexed_array_like_size(), 3u);
    EXPECT(!empty->indexed_has(0));
    EXPECT_EQ(vm_with_realm.result_of("[empty.length, Array.isArray(empty), 0 in empty].join()"sv), "3,true,false"sv);
    EXPECT_EQ(empty->class_name(), "Array"sv);

    auto with_prototype = MUST(JS::Array::create(realm, 0, realm.intrinsics().object_prototype()));
    EXPECT_EQ(prototype_of(*with_prototype), realm.intrinsics().object_prototype().ptr());

    auto too_long = JS::Array::create(realm, 1ull << 32);
    VERIFY(too_long.is_error());
    EXPECT(starts_with(vm_with_realm.error_text(too_long.error_value()), "RangeError: "sv));

    GC::RootVector<Value> elements;
    elements.append(Value(1));
    elements.append(string_value(vm, "two"sv));
    elements.append(js_null());
    auto from_span = JS::Array::create_from(realm, elements.span());
    vm_with_realm.define_global("fromSpan"sv, from_span);
    EXPECT_EQ(vm_with_realm.result_of("JSON.stringify(fromSpan)"sv), R"([1,"two",null])"sv);

    auto from_list = JS::Array::create_from(realm, { Value(4), Value(5) });
    vm_with_realm.define_global("fromList"sv, from_list);
    EXPECT_EQ(vm_with_realm.result_of("fromList.join()"sv), "4,5"sv);

    Vector<int> numbers { 1, 2, 3 };
    auto mapped = JS::Array::create_from<int>(realm, numbers.span(), [](int const& number) { return Value(number * 10); });
    vm_with_realm.define_global("mapped"sv, mapped);
    EXPECT_EQ(vm_with_realm.result_of("mapped.join()"sv), "10,20,30"sv);

    // The indexed storage of an array.
    auto first = from_span->indexed_get(0);
    VERIFY(first.has_value());
    EXPECT_EQ(first->value, Value(1));
    EXPECT(first->attributes == default_attributes);
    EXPECT(!from_span->indexed_get(5).has_value());
    from_span->indexed_append(Value(6));
    EXPECT_EQ(from_span->indexed_array_like_size(), 4u);
    EXPECT_EQ(from_span->indexed_take_first().value, Value(1));
    EXPECT_EQ(vm_with_realm.result_of("JSON.stringify(fromSpan)"sv), R"(["two",null,6])"sv);

    auto frozen_element = MUST(vm_with_realm.evaluate("const frozen = [1]; Object.freeze(frozen); frozen"sv));
    auto frozen = frozen_element.as_object().indexed_get(0);
    VERIFY(frozen.has_value());
    EXPECT(!frozen->attributes.is_writable());
    EXPECT(!frozen->attributes.is_configurable());
    EXPECT(frozen->attributes.is_enumerable());
}

TEST_CASE(what_objects_are)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    EXPECT(MUST(vm_with_realm.evaluate("new Date(0)"sv)).as_object().is_date());
    EXPECT(!Object::create(realm, nullptr)->is_date());
    EXPECT(MUST(vm_with_realm.evaluate("new Uint8Array(2)"sv)).as_object().is_typed_array());
    EXPECT(!MUST(vm_with_realm.evaluate("[1, 2]"sv)).as_object().is_typed_array());
    EXPECT(MUST(vm_with_realm.evaluate("(function (a) { return arguments; })(1)"sv)).as_object().has_parameter_map());
    EXPECT(MUST(vm_with_realm.evaluate("(function (a) { 'use strict'; return arguments; })(1)"sv)).as_object().has_parameter_map());
    EXPECT(!Object::create(realm, nullptr)->has_parameter_map());
    EXPECT(is<GlobalObject>(realm.global_object()));
    EXPECT(!is<GlobalObject>(*Object::create(realm, nullptr)));

    auto error = MUST(vm_with_realm.evaluate("new TypeError('wrong')"sv));
    EXPECT(error.as_object().has_error_data());
    EXPECT(error.as_object().error_data());
    EXPECT(!Object::create(realm, nullptr)->has_error_data());
    EXPECT(!Object::create(realm, nullptr)->error_data());

    // An unimplemented property reads as absent, and tells the VM's host.
    Vector<Utf16String> unimplemented_accesses;
    vm.on_unimplemented_property_access = [&](Object const& object, PropertyKey const& property_key) {
        unimplemented_accesses.append(Utf16String::formatted("{} {}", object.class_name(), property_key.to_utf16_string()));
    };
    auto object = Object::create(realm, realm.intrinsics().object_prototype());
    object->define_unimplemented_property("notYet"_utf16_fly_string);
    vm_with_realm.define_global("object"sv, object);
    EXPECT_EQ(vm_with_realm.result_of("[object.notYet, 'notYet' in object, Object.keys(object).length].join()"sv), ",false,0"sv);
    EXPECT(!unimplemented_accesses.is_empty());
    EXPECT_EQ(unimplemented_accesses.first(), "Object notYet"sv);
    vm.on_unimplemented_property_access = nullptr;
}

TEST_CASE(realm_and_intrinsics)
{
    VMWithRealm vm_with_realm;
    auto& realm = vm_with_realm.realm();
    auto& intrinsics = realm.intrinsics();

    EXPECT_EQ(&realm.global_object(), address_of(MUST(vm_with_realm.evaluate("globalThis"sv))));
    EXPECT_EQ(&realm.global_object().shape().realm(), &realm);
    EXPECT_EQ(intrinsics.object_prototype().ptr(), address_of(MUST(vm_with_realm.evaluate("Object.prototype"sv))));
    EXPECT_EQ(intrinsics.function_prototype().ptr(), address_of(MUST(vm_with_realm.evaluate("Function.prototype"sv))));
    EXPECT_EQ(intrinsics.array_prototype().ptr(), address_of(MUST(vm_with_realm.evaluate("Array.prototype"sv))));
    EXPECT_EQ(intrinsics.json_stringify_function().ptr(), address_of(MUST(vm_with_realm.evaluate("JSON.stringify"sv))));
    EXPECT_EQ(intrinsics.object_prototype()->shape().prototype(), nullptr);
    EXPECT_EQ(prototype_of(*intrinsics.array_prototype()), intrinsics.object_prototype().ptr());
    EXPECT_EQ(intrinsics.json_stringify_function()->realm(), &realm);
}
