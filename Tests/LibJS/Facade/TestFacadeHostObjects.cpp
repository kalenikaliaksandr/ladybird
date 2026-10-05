/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/TypeCasts.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Heap.h>
#include <LibGC/HeapBlock.h>
#include <LibGC/Root.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>
#include <LibJS/HostClassBuilder.h>
#include <LibJS/HostObjectABI.h>
#include <LibJS/Runtime/Array.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/GlobalObject.h>
#include <LibJS/Runtime/HostArray.h>
#include <LibJS/Runtime/HostFunction.h>
#include <LibJS/Runtime/HostObject.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/NativeFunction.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// Objects, functions and arrays whose internal methods an embedder implements through the tables of
// LibJS/HostObjectABI.h, built with LibJS/HostClassBuilder.h, as LibJS's users build them. The same expectations hold
// for the C++ runtime's LibJS and for the facade over the Rust one. A hook defers to the ordinary internal method
// through Object::ordinary_*(), which never dispatches back to the hook.

namespace {

class TestEnvironment {
public:
    explicit TestEnvironment(Function<GC::Ref<JS::Object>(JS::Realm&)> create_global_object = nullptr)
        : m_vm(JS::VM::create())
        , m_execution_context(MUST(JS::Realm::initialize_host_defined_realm(*m_vm, move(create_global_object), nullptr)))
    {
    }

    ~TestEnvironment()
    {
        while (!m_vm->execution_context_stack().is_empty())
            m_vm->pop_execution_context();
    }

    JS::VM& vm() { return *m_vm; }
    JS::Realm& realm() { return *m_execution_context->realm; }

    void define_global(StringView name, JS::Value value)
    {
        realm().global_object().define_direct_property(Utf16FlyString::from_utf8(name), value, JS::default_attributes);
    }

    JS::ThrowCompletionOr<JS::Value> run(StringView source)
    {
        auto source_text = Utf16String::from_utf8(source);
        auto script = JS::Script::parse(source_text.utf16_view(), realm());
        VERIFY(!script.is_error());
        return m_vm->run(script.value());
    }

    // Returns the completion value as a string, or "uncaught <error>" for an exception that escaped the script.
    String evaluate(StringView source)
    {
        auto result = run(source);
        if (result.is_error())
            return MUST(String::formatted("uncaught {}", result.error_value().to_utf16_string_without_side_effects()));
        return result.value().to_utf16_string_without_side_effects().to_utf8();
    }

    // Runs the statements in a function and returns "<name>: <message>" of what they throw, or "no exception". The
    // result comes from a return value, as a catch block's completion value can be wrong after an exception from a
    // nested call.
    String exception_from(StringView statements)
    {
        return evaluate(MUST(String::formatted("(() => {{ try {{ {}; }} catch (error) {{ return `${{error.name}}: ${{error.message}}`; }} return 'no exception'; }})()", statements)));
    }

private:
    NonnullRefPtr<JS::VM> m_vm;
    NonnullOwnPtr<JS::ExecutionContext> m_execution_context;
};

class TestHostData final : public GC::Cell {
    GC_CELL(TestHostData, GC::Cell);
    GC_DECLARE_ALLOCATOR(TestHostData);

public:
    u64 payload { 0 };
};

GC_DEFINE_ALLOCATOR(TestHostData);

class OtherTestHostData final : public GC::Cell {
    GC_CELL(OtherTestHostData, GC::Cell);
    GC_DECLARE_ALLOCATOR(OtherTestHostData);

public:
    u64 payload { 0 };
};

GC_DEFINE_ALLOCATOR(OtherTestHostData);

bool key_is(JS::PropertyKey const& property_key, StringView name)
{
    return property_key.is_string() && property_key.as_string() == Utf16FlyString::from_utf8(name);
}

template<typename HostCell>
JS::Completion hook_error(HostCell const& cell, StringView hook)
{
    return cell.vm().template throw_completion<JS::TypeError>(Utf16String::formatted("{} threw", hook));
}

bool s_keyless_hooks_throw = false;
Optional<JS::PropertyDescriptor> s_last_defined_descriptor;
// Not given, given and empty, or given with a descriptor.
Optional<Optional<JS::PropertyDescriptor>> s_last_precomputed_get_own_property;
JS::Value s_last_intercepted_value;

// Implements every object hook. Hooks taking a key answer some keys themselves, throw for "throwing", and leave the
// rest to the ordinary internal method, the way bindings do.
struct InterceptingTraits {
    static JS::ThrowCompletionOr<JS::Object*> get_prototype_of(JS::HostObject const& object)
    {
        if (s_keyless_hooks_throw)
            return hook_error(object, "get_prototype_of"sv);
        return object.ordinary_get_prototype_of();
    }

    static JS::ThrowCompletionOr<bool> set_prototype_of(JS::HostObject& object, JS::Object* prototype)
    {
        if (s_keyless_hooks_throw)
            return hook_error(object, "set_prototype_of"sv);
        return object.ordinary_set_prototype_of(prototype);
    }

    static JS::ThrowCompletionOr<bool> is_extensible(JS::HostObject const& object)
    {
        if (s_keyless_hooks_throw)
            return hook_error(object, "is_extensible"sv);
        return object.ordinary_is_extensible();
    }

    static JS::ThrowCompletionOr<bool> prevent_extensions(JS::HostObject& object)
    {
        if (s_keyless_hooks_throw)
            return hook_error(object, "prevent_extensions"sv);
        return false;
    }

    static JS::ThrowCompletionOr<Optional<JS::PropertyDescriptor>> get_own_property(JS::HostObject const& object, JS::PropertyKey const& property_key)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "get_own_property"sv);
        if (key_is(property_key, "virtual"sv)) {
            return JS::PropertyDescriptor {
                .value = JS::Value(JS::PrimitiveString::create(object.vm(), "virtual value"_utf16)),
                .writable = false,
                .enumerable = true,
                .configurable = true,
            };
        }
        return object.ordinary_get_own_property(property_key);
    }

    static JS::ThrowCompletionOr<bool> define_own_property(JS::HostObject& object, JS::PropertyKey const& property_key, JS::PropertyDescriptor& descriptor, Optional<JS::PropertyDescriptor>* precomputed_get_own_property)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "define_own_property"sv);
        if (key_is(property_key, "rejected"sv))
            return false;
        s_last_defined_descriptor = descriptor;
        s_last_precomputed_get_own_property.clear();
        if (precomputed_get_own_property)
            s_last_precomputed_get_own_property = *precomputed_get_own_property;
        return object.ordinary_define_own_property(property_key, descriptor, precomputed_get_own_property);
    }

    static JS::ThrowCompletionOr<bool> has_property(JS::HostObject const& object, JS::PropertyKey const& property_key)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "has_property"sv);
        if (key_is(property_key, "magic"sv))
            return true;
        return object.ordinary_has_property(property_key);
    }

    static JS::ThrowCompletionOr<JS::Value> get(JS::HostObject const& object, JS::PropertyKey const& property_key, JS::Value receiver, JS::CacheableGetPropertyMetadata* metadata, JS::Object::PropertyLookupPhase phase)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "get"sv);
        if (key_is(property_key, "answer"sv))
            return JS::Value(42);
        return object.ordinary_get(property_key, receiver, metadata, phase);
    }

    static JS::ThrowCompletionOr<bool> set(JS::HostObject& object, JS::PropertyKey const& property_key, JS::Value value, JS::Value receiver, JS::CacheableSetPropertyMetadata* metadata, JS::Object::PropertyLookupPhase phase)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "set"sv);
        if (key_is(property_key, "intercepted"sv)) {
            s_last_intercepted_value = value;
            return true;
        }
        return object.ordinary_set(property_key, value, receiver, metadata, phase);
    }

    static JS::ThrowCompletionOr<bool> delete_property(JS::HostObject& object, JS::PropertyKey const& property_key)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(object, "delete_property"sv);
        if (key_is(property_key, "undeletable"sv))
            return false;
        return object.ordinary_delete(property_key);
    }

    static JS::ThrowCompletionOr<GC::RootVector<JS::Value>> own_property_keys(JS::HostObject const& object)
    {
        if (s_keyless_hooks_throw)
            return hook_error(object, "own_property_keys"sv);
        auto keys = TRY(object.ordinary_own_property_keys());
        keys.append(JS::PrimitiveString::create(object.vm(), "virtual"_utf16));
        return keys;
    }
};

constexpr JSHostObjectHooks intercepting_hooks = JS::make_host_object_hooks<InterceptingTraits>();

}

// Declared and defined apart, as an embedder's header and source file do.
extern JSHostClass const intercepting_host_class;
constexpr JSHostClass intercepting_host_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "InterceptingHostObject"sv, nullptr, &intercepting_hooks, nullptr, 0);

namespace {

size_t s_finalized_host_objects = 0;

struct CountingFinalizerTraits {
    static void finalize(JS::HostObject&) { ++s_finalized_host_objects; }
};

constexpr JSHostObjectHooks counting_finalizer_hooks = JS::make_host_object_hooks<CountingFinalizerTraits>();
constexpr JSHostClass counting_finalizer_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "CountingFinalizer"sv, nullptr, &counting_finalizer_hooks, nullptr, 0);

// The [[ErrorData]] that host objects of this class expose is that of the error object in their host data slot.
struct ErrorDataTraits {
    static JS::ErrorData* error_data(JS::HostObject& object)
    {
        return GC::static_cell_cast<JS::Object>(object.host_data().ptr())->error_data();
    }
};

constexpr JSHostObjectHooks error_data_hooks = JS::make_host_object_hooks<ErrorDataTraits>();
constexpr JSHostClass error_data_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "ErrorDataHostObject"sv, nullptr, &error_data_hooks, nullptr, 0);

bool s_inherited_property_is_cacheable = true;
bool s_inherited_property_is_shadowed = false;
size_t s_own_lookups_of_inherited_property = 0;

// Can start answering for "inherited" itself, which shadows the property of the same name on its prototype without
// changing its shape, as a legacy platform object's named properties do.
struct InheritedCacheabilityTraits {
    static JS::ThrowCompletionOr<Optional<JS::PropertyDescriptor>> get_own_property(JS::HostObject const& object, JS::PropertyKey const& property_key)
    {
        if (key_is(property_key, "inherited"sv)) {
            ++s_own_lookups_of_inherited_property;
            if (s_inherited_property_is_shadowed)
                return JS::PropertyDescriptor { .value = JS::Value(2), .writable = true, .enumerable = true, .configurable = true };
        }
        return object.ordinary_get_own_property(property_key);
    }

    static bool is_cacheable_for_inherited_property(JS::HostObject const&) { return s_inherited_property_is_cacheable; }
};

JS::ThrowCompletionOr<JS::Value> start_shadowing_inherited_property(JS::VM&)
{
    s_inherited_property_is_shadowed = true;
    return JS::js_undefined();
}

constexpr JSHostObjectHooks inherited_cacheability_hooks = JS::make_host_object_hooks<InheritedCacheabilityTraits>();
constexpr JSHostClass inherited_cacheability_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "InheritedCacheability"sv, nullptr, &inherited_cacheability_hooks, nullptr, JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE);

// Only a [[Get]] hook, which doubles the numbers that the ordinary [[Get]] finds. It passes no cache metadata on, so that
// inline caches never answer for it.
struct NumberDoublingTraits {
    static JS::ThrowCompletionOr<JS::Value> get(JS::HostObject const& object, JS::PropertyKey const& property_key, JS::Value receiver, JS::CacheableGetPropertyMetadata*, JS::Object::PropertyLookupPhase phase)
    {
        auto value = TRY(object.ordinary_get(property_key, receiver, nullptr, phase));
        if (value.is_number())
            return JS::Value(value.as_double() * 2);
        return value;
    }
};

constexpr JSHostObjectHooks number_doubling_hooks = JS::make_host_object_hooks<NumberDoublingTraits>();
constexpr JSHostClass number_doubling_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "NumberDoubling"sv, nullptr, &number_doubling_hooks, nullptr, 0);

// Hooks written against HostObjectABI.h alone, the way a table not made by HostClassBuilder.h would be. The first leaves
// the descriptor it is given zeroed, and the second accepts every definition but zeroes the descriptor it writes back.
JSCompletion report_every_property_absent(JSObject*, JSPropertyKey, JSPropertyDescriptor*)
{
    return { .payload = 0, .variant = JS_COMPLETION_NORMAL };
}

JSCompletion accept_definition_and_zero_descriptor(JSObject*, JSPropertyKey, JSPropertyDescriptor* descriptor, JSPropertyDescriptor const*)
{
    *descriptor = {};
    return { .payload = 1, .variant = JS_COMPLETION_NORMAL };
}

constexpr JSHostObjectHooks hand_written_hooks = [] {
    JSHostObjectHooks hooks {};
    hooks.get_own_property = report_every_property_absent;
    hooks.define_own_property = accept_definition_and_zero_descriptor;
    return hooks;
}();
constexpr JSHostClass hand_written_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "HandWritten"sv, nullptr, &hand_written_hooks, nullptr, 0);

// A collection with the internal methods of a WebIDL legacy platform object: its supported property indices are the
// indices of the numbers in its host data, which an indexed property setter changes, and which it lists before its own
// properties. Its [[GetOwnProperty]] and [[OwnPropertyKeys]] write straight into the engine's descriptor and key list,
// as LibWeb's do.
class NumberList final : public GC::Cell {
    GC_CELL(NumberList, GC::Cell);
    GC_DECLARE_ALLOCATOR(NumberList);

public:
    Vector<i32> numbers;
};

GC_DEFINE_ALLOCATOR(NumberList);

NumberList& number_list_of(JS::HostObject const& object)
{
    auto* number_list = JS::host_data_if<NumberList>(object);
    VERIFY(number_list);
    return *number_list;
}

Optional<size_t> supported_index_of(JS::HostObject const& object, JS::PropertyKey const& property_key)
{
    if (!property_key.is_number() || property_key.as_number() >= number_list_of(object).numbers.size())
        return {};
    return property_key.as_number();
}

struct NumberListTraits {
    static JS::ThrowCompletionOr<bool> define_own_property(JS::HostObject& object, JS::PropertyKey const& property_key, JS::PropertyDescriptor& descriptor, Optional<JS::PropertyDescriptor>* precomputed_get_own_property)
    {
        if (property_key.is_number())
            return false;
        return object.ordinary_define_own_property(property_key, descriptor, precomputed_get_own_property);
    }

    static JS::ThrowCompletionOr<bool> set(JS::HostObject& object, JS::PropertyKey const& property_key, JS::Value value, JS::Value receiver, JS::CacheableSetPropertyMetadata*, JS::Object::PropertyLookupPhase)
    {
        if (receiver.as_if<JS::Object>() == GC::Ptr<JS::Object> { object } && property_key.is_number()) {
            if (!value.is_number())
                return object.vm().throw_completion<JS::TypeError>("not a number"sv);
            auto& numbers = number_list_of(object).numbers;
            auto index = property_key.as_number();
            if (index > numbers.size())
                return object.vm().throw_completion<JS::RangeError>("not contiguous"sv);
            if (index == numbers.size())
                numbers.append(value.as_i32());
            else
                numbers[index] = value.as_i32();
            return true;
        }
        auto own_descriptor = TRY(object.ordinary_get_own_property(property_key));
        return object.ordinary_set_with_own_descriptor(property_key, value, receiver, own_descriptor);
    }

    static JS::ThrowCompletionOr<bool> delete_property(JS::HostObject& object, JS::PropertyKey const& property_key)
    {
        if (supported_index_of(object, property_key).has_value())
            return false;
        return object.ordinary_delete(property_key);
    }
};

JS::HostObject const& number_list_from_abi(JSObject* object)
{
    return static_cast<JS::HostObject const&>(*JS::HostABI::object_from_abi(object));
}

JS::ThrowCompletionOr<void> number_list_get_own_property(JS::HostObject const& object, JS::PropertyKey const& property_key, JSPropertyDescriptor& descriptor)
{
    if (auto index = supported_index_of(object, property_key); index.has_value()) {
        descriptor.value = JS::HostABI::value_to_abi(JS::Value(number_list_of(object).numbers[*index]));
        descriptor.flags = JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE | JS_PD_HAS_CONFIGURABLE | JS_PD_CONFIGURABLE;
        return {};
    }
    descriptor = JS::HostABI::property_descriptor_to_abi(TRY(object.ordinary_get_own_property(property_key)));
    return {};
}

JS::ThrowCompletionOr<void> number_list_own_property_keys(JS::HostObject const& object, JSValueSink& keys)
{
    auto& vm = object.vm();
    for (size_t index = 0; index < number_list_of(object).numbers.size(); ++index)
        keys.append(keys.context, JS::HostABI::value_to_abi(JS::PrimitiveString::create_from_unsigned_integer(vm, index)));
    for (auto key : TRY(object.ordinary_own_property_keys()))
        keys.append(keys.context, JS::HostABI::value_to_abi(key));
    return {};
}

consteval JSHostObjectHooks make_number_list_hooks()
{
    auto hooks = JS::make_host_object_hooks<NumberListTraits>();
    hooks.get_own_property = [](JSObject* object, JSPropertyKey property_key, JSPropertyDescriptor* descriptor) {
        return JS::HostABI::completion_to_abi(number_list_get_own_property(number_list_from_abi(object), JS::HostABI::property_key_from_abi(property_key), *descriptor));
    };
    hooks.own_property_keys = [](JSObject* object, JSValueSink* keys) {
        return JS::HostABI::completion_to_abi(number_list_own_property_keys(number_list_from_abi(object), *keys));
    };
    return hooks;
}

constexpr JSHostObjectHooks number_list_hooks = make_number_list_hooks();
constexpr u32 legacy_platform_object_flags = JS_HOST_CLASS_IS_PLATFORM_OBJECT
    | JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY
    | JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS
    | JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE
    | JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH;
constexpr JSHostClass number_list_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "NumberList"sv, nullptr, &number_list_hooks, nullptr, legacy_platform_object_flags);

constexpr u32 all_object_flags = JS_HOST_CLASS_IS_PLATFORM_OBJECT
    | JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY
    | JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS
    | JS_HOST_CLASS_IS_HTMLDDA
    | JS_HOST_CLASS_IS_GLOBAL_OBJECT
    | JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE
    | JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH;

int s_user_data_marker = 0;

constexpr JSHostClass all_flags_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllFlags"sv, nullptr, nullptr, nullptr, all_object_flags);
constexpr JSHostClass no_flags_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "NoFlags"sv, nullptr, nullptr, &s_user_data_marker, 0);
constexpr JSHostClass immutable_prototype_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "ImmutablePrototype"sv, nullptr, nullptr, nullptr, JS_HOST_CLASS_IMMUTABLE_PROTOTYPE);
constexpr JSHostClass base_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "Base"sv, nullptr, nullptr, nullptr, 0);
constexpr JSHostClass derived_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "Derived"sv, &base_class, nullptr, nullptr, 0);
constexpr JSHostClass allocator_a_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorA"sv, nullptr, nullptr, nullptr, 0);
constexpr JSHostClass allocator_b_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorB"sv, nullptr, nullptr, nullptr, 0);
constexpr JSHostClass allocator_a_sharing_child_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorASharingChild"sv, &allocator_a_class, nullptr, nullptr, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT);
constexpr JSHostClass allocator_a_sharing_grandchild_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorASharingGrandchild"sv, &allocator_a_sharing_child_class, nullptr, nullptr, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT);
constexpr JSHostClass allocator_a_isolated_child_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorAIsolatedChild"sv, &allocator_a_class, nullptr, nullptr, 0);

GC::CellAllocator& allocator_of(JS::Object const& object)
{
    return GC::HeapBlock::from_cell(GC::as_cell(&object))->cell_allocator();
}

GC::Ref<JS::HostObject> create_intercepting_object(TestEnvironment& environment)
{
    auto& realm = environment.realm();
    auto object = JS::HostObject::create(realm, intercepting_host_class, realm.intrinsics().object_prototype());
    environment.define_global("host"sv, object);
    return object;
}

NEVER_INLINE void allocate_unreachable_host_objects(JS::Realm& realm, size_t count)
{
    for (size_t i = 0; i < count; ++i)
        (void)JS::HostObject::create(realm, counting_finalizer_class, nullptr);
}

NEVER_INLINE GC::Root<JS::HostObject> allocate_host_object_owning_its_cells(JS::Realm& realm, GC::Weak<TestHostData>& wrappable, GC::Weak<TestHostData>& host_data)
{
    auto new_wrappable = realm.heap().allocate<TestHostData>();
    auto new_host_data = realm.heap().allocate<TestHostData>();
    wrappable = new_wrappable;
    host_data = new_host_data;
    return GC::make_root(JS::HostObject::create(realm, no_flags_class, nullptr, new_wrappable, new_host_data));
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

JS::ThrowCompletionOr<JS::Value> return_undefined(JS::VM&)
{
    return JS::js_undefined();
}

void expect_same_descriptor(JS::PropertyDescriptor const& actual, JS::PropertyDescriptor const& expected)
{
    EXPECT_EQ(actual.value.has_value(), expected.value.has_value());
    if (actual.value.has_value() && expected.value.has_value())
        EXPECT_EQ(actual.value->encoded(), expected.value->encoded());
    EXPECT(actual.get == expected.get);
    EXPECT(actual.set == expected.set);
    EXPECT_EQ(actual.writable, expected.writable);
    EXPECT_EQ(actual.enumerable, expected.enumerable);
    EXPECT_EQ(actual.configurable, expected.configurable);
    EXPECT_EQ(actual.property_offset, expected.property_offset);
}

bool starts_with(String const& text, StringView prefix)
{
    return text.starts_with_bytes(prefix);
}

}

TEST_CASE(object_hooks_answer_scripts)
{
    TestEnvironment environment;
    create_intercepting_object(environment);

    EXPECT_EQ(environment.evaluate("host.answer"sv), "42"sv);
    EXPECT_EQ(environment.evaluate("'magic' in host"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify(Object.getOwnPropertyDescriptor(host, 'virtual'))"sv),
        R"({"value":"virtual value","writable":false,"enumerable":true,"configurable":true})"sv);
    EXPECT_EQ(environment.evaluate("host.intercepted = 7; host.intercepted"sv), "undefined"sv);
    EXPECT_EQ(s_last_intercepted_value, JS::Value(7));
    EXPECT_EQ(environment.evaluate("host.undeletable = 1; delete host.undeletable"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("Reflect.defineProperty(host, 'rejected', { value: 1 })"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("Object.defineProperty(host, 'defined', { value: 1, enumerable: true }); host.defined"sv), "1"sv);
    EXPECT_EQ(environment.evaluate("host.expando = 2; delete host.expando"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("Reflect.ownKeys(host).join()"sv), "undeletable,defined,virtual"sv);
    EXPECT_EQ(environment.evaluate("Reflect.preventExtensions(host)"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("Object.isExtensible(host)"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(host) === Object.prototype"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("const proto = { inherited: 3 }; Reflect.setPrototypeOf(host, proto) && host.inherited"sv), "3"sv);
}

TEST_CASE(object_hooks_answer_the_embedder)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto host = create_intercepting_object(environment);

    // The internal methods dispatch to the hooks, and the ordinary ones never do.
    EXPECT_EQ(MUST(host->internal_get(JS::PropertyKey { "answer"_utf16_fly_string }, host)), JS::Value(42));
    EXPECT_EQ(MUST(host->get(JS::PropertyKey { "answer"_utf16_fly_string })), JS::Value(42));
    EXPECT(MUST(host->ordinary_get(JS::PropertyKey { "answer"_utf16_fly_string }, host)).is_undefined());
    EXPECT(MUST(host->has_property(JS::PropertyKey { "magic"_utf16_fly_string })));
    EXPECT(!MUST(host->ordinary_has_property(JS::PropertyKey { "magic"_utf16_fly_string })));
    EXPECT(MUST(host->internal_get_own_property(JS::PropertyKey { "virtual"_utf16_fly_string })).has_value());
    EXPECT(!MUST(host->ordinary_get_own_property(JS::PropertyKey { "virtual"_utf16_fly_string })).has_value());
    EXPECT(MUST(host->internal_set(JS::PropertyKey { "intercepted"_utf16_fly_string }, JS::Value(8), host)));
    EXPECT_EQ(s_last_intercepted_value, JS::Value(8));
    EXPECT(!MUST(host->internal_delete(JS::PropertyKey { "undeletable"_utf16_fly_string })));
    EXPECT(!MUST(host->internal_prevent_extensions()));
    EXPECT(MUST(host->ordinary_prevent_extensions()));
    EXPECT(!host->extensible());
    EXPECT_EQ(MUST(host->internal_own_property_keys()).size(), 1u);
    EXPECT_EQ(MUST(host->ordinary_own_property_keys()).size(), 0u);
    EXPECT_EQ(MUST(host->internal_get_prototype_of()), realm.intrinsics().object_prototype().ptr());

    auto failed_get = host->get(JS::PropertyKey { "throwing"_utf16_fly_string });
    VERIFY(failed_get.is_error());
    EXPECT_EQ(failed_get.error_value().as_object().get_without_side_effects(environment.vm().names.message).to_utf16_string_without_side_effects(), "get threw"sv);
}

TEST_CASE(object_hooks_throw_into_scripts)
{
    TestEnvironment environment;
    create_intercepting_object(environment);

    EXPECT_EQ(environment.evaluate(R"(
        [
            () => host.throwing,
            () => "throwing" in host,
            () => Object.getOwnPropertyDescriptor(host, "throwing"),
            () => { host.throwing = 1; },
            () => delete host.throwing,
            () => Object.defineProperty(host, "throwing", { value: 1 }),
        ].map(operation => {
            try {
                operation();
                return "no exception";
            } catch (error) {
                return error.message;
            }
        }).join()
    )"sv),
        "get threw,has_property threw,get_own_property threw,set threw,delete_property threw,define_own_property threw"sv);

    s_keyless_hooks_throw = true;
    auto result = environment.evaluate(R"(
        [
            () => Object.getPrototypeOf(host),
            () => Reflect.setPrototypeOf(host, null),
            () => Reflect.isExtensible(host),
            () => Reflect.preventExtensions(host),
            () => Reflect.ownKeys(host),
        ].map(operation => {
            try {
                operation();
                return "no exception";
            } catch (error) {
                return error.message;
            }
        }).join()
    )"sv);
    s_keyless_hooks_throw = false;
    EXPECT_EQ(result, "get_prototype_of threw,set_prototype_of threw,is_extensible threw,prevent_extensions threw,own_property_keys threw"sv);
}

TEST_CASE(enumeration_goes_through_the_hooks)
{
    TestEnvironment environment;
    auto& realm = environment.realm();

    auto intercepting = create_intercepting_object(environment);
    EXPECT(!intercepting->eligible_for_own_property_enumeration_fast_path());
    EXPECT_EQ(environment.evaluate("host.plain = 1"sv), "1"sv);
    EXPECT_EQ(environment.evaluate("Object.keys(host).join()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("(() => { const keys = []; for (const key in host) keys.push(key); return keys.join(); })()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify(host)"sv), R"({"plain":1,"virtual":"virtual value"})"sv);
    EXPECT_EQ(environment.evaluate("Object.keys(Object.assign({}, host)).join()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("Object.keys({ ...host }).join()"sv), "plain,virtual"sv);

    auto doubling = JS::HostObject::create(realm, number_doubling_class, realm.intrinsics().object_prototype());
    EXPECT(!doubling->eligible_for_own_property_enumeration_fast_path());
    environment.define_global("doubling"sv, doubling);
    EXPECT_EQ(environment.evaluate("doubling.number = 2; doubling.number"sv), "4"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify(doubling)"sv), R"({"number":4})"sv);
    EXPECT_EQ(environment.evaluate("Object.values(doubling).join()"sv), "4"sv);
    EXPECT_EQ(environment.evaluate("Object.assign({}, doubling).number"sv), "4"sv);
    EXPECT_EQ(environment.evaluate("({ ...doubling }).number"sv), "4"sv);
}

TEST_CASE(hand_written_descriptor_hooks)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    environment.define_global("handWritten"sv, JS::HostObject::create(realm, hand_written_class, realm.intrinsics().object_prototype()));

    EXPECT_EQ(environment.evaluate("Reflect.defineProperty(handWritten, 'defined', { value: 1 })"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("handWritten.assigned = 2"sv), "2"sv);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyDescriptor(handWritten, 'defined')"sv), "undefined"sv);
    EXPECT_EQ(environment.evaluate("'assigned' in handWritten"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("handWritten.toString === Object.prototype.toString"sv), "true"sv);
}

TEST_CASE(property_descriptors_round_trip_through_the_abi)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto getter = JS::NativeFunction::create(realm, return_undefined, 0);

    auto round_trip = [](JS::PropertyDescriptor const& descriptor) {
        return JS::HostABI::property_descriptor_from_abi(JS::HostABI::property_descriptor_to_abi(descriptor)).release_value();
    };

    JS::PropertyDescriptor data_descriptor {
        .value = JS::Value(1.5),
        .writable = true,
        .enumerable = false,
        .configurable = true,
        .property_offset = 5,
    };
    expect_same_descriptor(round_trip(data_descriptor), data_descriptor);
    EXPECT_EQ(JS::HostABI::property_descriptor_to_abi(data_descriptor).flags,
        JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_HAS_CONFIGURABLE | JS_PD_CONFIGURABLE | JS_PD_HAS_PROPERTY_OFFSET);

    JS::PropertyDescriptor accessor_descriptor {
        .get = GC::Ptr<JS::FunctionObject> { getter },
        .set = GC::Ptr<JS::FunctionObject> {},
        .enumerable = true,
    };
    auto accessor_round_trip = round_trip(accessor_descriptor);
    expect_same_descriptor(accessor_round_trip, accessor_descriptor);
    EXPECT(accessor_round_trip.set.has_value());

    JS::PropertyDescriptor empty_descriptor;
    expect_same_descriptor(round_trip(empty_descriptor), empty_descriptor);

    EXPECT_EQ(JS::HostABI::property_descriptor_to_abi(Optional<JS::PropertyDescriptor> {}).flags, 0);
    EXPECT(!JS::HostABI::property_descriptor_from_abi(JSPropertyDescriptor {}).has_value());

    // A descriptor that a hook hands to the engine reaches scripts intact.
    auto object = JS::HostObject::create(realm, no_flags_class, realm.intrinsics().object_prototype());
    environment.define_global("object"sv, object);
    MUST(object->define_property_or_throw(JS::PropertyKey { "accessor"_utf16_fly_string }, accessor_descriptor));
    EXPECT_EQ(environment.evaluate("const accessor = Object.getOwnPropertyDescriptor(object, 'accessor'); [typeof accessor.get, accessor.set, accessor.enumerable, accessor.configurable].join()"sv), "function,,true,false"sv);
}

TEST_CASE(property_offsets_pass_through_hooks)
{
    TestEnvironment environment;
    auto object = create_intercepting_object(environment);
    auto key = JS::PropertyKey { "fresh"_utf16_fly_string };

    s_last_defined_descriptor.clear();
    Optional<u32> new_property_offset;
    EXPECT(MUST(object->create_data_property(key, JS::Value(3), &new_property_offset)));
    EXPECT(new_property_offset.has_value());
    auto ordinary_entry = MUST(object->ordinary_get_own_property(key));
    VERIFY(ordinary_entry.has_value());
    EXPECT_EQ(new_property_offset, ordinary_entry->property_offset);
    VERIFY(s_last_defined_descriptor.has_value());
    EXPECT_EQ(s_last_defined_descriptor->value->encoded(), JS::Value(3).encoded());
    EXPECT_EQ(s_last_defined_descriptor->writable, true);
    EXPECT(!s_last_precomputed_get_own_property.has_value());

    // An empty precomputed [[GetOwnProperty]] result must stay distinct from none at all.
    EXPECT_EQ(environment.evaluate("host.assigned = 1"sv), "1"sv);
    VERIFY(s_last_precomputed_get_own_property.has_value());
    EXPECT(!s_last_precomputed_get_own_property->has_value());

    Optional<JS::PropertyDescriptor> precomputed_get_own_property = MUST(object->internal_get_own_property(key));
    JS::PropertyDescriptor redefinition { .value = JS::Value(5) };
    EXPECT(MUST(object->internal_define_own_property(key, redefinition, &precomputed_get_own_property)));
    VERIFY(s_last_precomputed_get_own_property.has_value());
    VERIFY(s_last_precomputed_get_own_property->has_value());
    expect_same_descriptor(**s_last_precomputed_get_own_property, *precomputed_get_own_property);
    EXPECT_EQ(MUST(object->get(key)), JS::Value(5));
    MUST(object->set(key, JS::Value(3), JS::Object::ShouldThrowExceptions::Yes));

    auto own_descriptor = MUST(object->internal_get_own_property(key));
    VERIFY(own_descriptor.has_value());
    EXPECT_EQ(own_descriptor->property_offset, new_property_offset);

    EXPECT_EQ(MUST(object->internal_get(key, object)), JS::Value(3));
    EXPECT(MUST(object->internal_set(key, JS::Value(4), object)));
    EXPECT_EQ(MUST(object->get(key)), JS::Value(4));
}

TEST_CASE(table_flags_become_object_flags)
{
    TestEnvironment environment;
    auto& realm = environment.realm();

    auto flagged = JS::HostObject::create(realm, all_flags_class, realm.intrinsics().object_prototype());
    EXPECT(flagged->is_platform_object());
    EXPECT(flagged->requires_slow_add_own_property());
    EXPECT(flagged->may_interfere_with_indexed_property_access());
    EXPECT(flagged->is_htmldda());
    EXPECT(flagged->has_global_object_flag());
    EXPECT(!flagged->is_cacheable_for_property_absence());
    EXPECT(!flagged->eligible_for_own_property_enumeration_fast_path());

    flagged->clear_requires_slow_add_own_property();
    EXPECT(!flagged->requires_slow_add_own_property());
    EXPECT(flagged->is_platform_object());

    auto plain = JS::HostObject::create(realm, no_flags_class, realm.intrinsics().object_prototype());
    EXPECT(!plain->is_platform_object());
    EXPECT(!plain->requires_slow_add_own_property());
    EXPECT(!plain->may_interfere_with_indexed_property_access());
    EXPECT(!plain->is_htmldda());
    EXPECT(!plain->has_global_object_flag());
    EXPECT(plain->is_cacheable_for_property_absence());
    EXPECT(plain->eligible_for_own_property_enumeration_fast_path());
    EXPECT(plain->extensible());
    EXPECT_EQ(plain->host_class().user_data, &s_user_data_marker);

    environment.define_global("flagged"sv, flagged);
    EXPECT_EQ(environment.evaluate("typeof flagged"sv), "undefined"sv);
    EXPECT_EQ(environment.evaluate("flagged == null"sv), "true"sv);
}

TEST_CASE(immutable_prototype_flag)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    environment.define_global("immutable"sv, JS::HostObject::create(realm, immutable_prototype_class, realm.intrinsics().object_prototype()));

    EXPECT_EQ(environment.evaluate("Reflect.setPrototypeOf(immutable, {})"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("Reflect.setPrototypeOf(immutable, Object.prototype)"sv), "true"sv);
    EXPECT(starts_with(environment.exception_from("Object.setPrototypeOf(immutable, null)"sv), "TypeError: "sv));
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(immutable) === Object.prototype"sv), "true"sv);
}

TEST_CASE(inherited_property_cacheability_comes_from_the_hook)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto key = JS::PropertyKey { "inherited"_utf16_fly_string };
    auto prototype = JS::Object::create(realm, realm.intrinsics().object_prototype());
    prototype->define_direct_property(key, JS::Value(1), JS::default_attributes);
    auto object = JS::HostObject::create(realm, inherited_cacheability_class, prototype);
    environment.define_global("object"sv, object);

    s_inherited_property_is_cacheable = true;
    EXPECT(object->is_cacheable_for_inherited_property());
    EXPECT_EQ(environment.evaluate("let sum = 0; for (let i = 0; i < 4; ++i) sum += object.inherited; sum"sv), "4"sv);

    // Inline caches never answer for an object that cannot vouch for inherited properties, so its hook sees every
    // lookup, and a property it starts to answer for shadows the inherited one at once.
    s_inherited_property_is_cacheable = false;
    EXPECT(!object->is_cacheable_for_inherited_property());
    EXPECT_EQ(MUST(object->get(key)), JS::Value(1));
    environment.define_global("startShadowing"sv, JS::NativeFunction::create(realm, start_shadowing_inherited_property, 0));
    s_own_lookups_of_inherited_property = 0;
    EXPECT_EQ(environment.evaluate("const seen = []; for (let i = 0; i < 6; ++i) { if (i === 3) startShadowing(); seen.push(object.inherited); } seen.join()"sv), "1,1,1,2,2,2"sv);
    EXPECT_EQ(s_own_lookups_of_inherited_property, 6u);
    s_inherited_property_is_shadowed = false;
    s_inherited_property_is_cacheable = true;

    EXPECT(JS::Object::create(realm, nullptr)->is_cacheable_for_inherited_property());
}

TEST_CASE(error_data_hook)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto error = MUST(environment.run("new Error('boom')"sv));
    auto object = JS::HostObject::create(realm, error_data_class, realm.intrinsics().object_prototype(), nullptr, GC::Ptr<GC::Cell> { error.as_object() });

    EXPECT(object->has_error_data());
    EXPECT_EQ(object->error_data(), error.as_object().error_data());
    EXPECT(!JS::HostObject::create(realm, no_flags_class, nullptr)->has_error_data());

    environment.define_global("errorish"sv, object);
    EXPECT_EQ(environment.evaluate("Object.prototype.toString.call(errorish)"sv), "[object Error]"sv);
}

TEST_CASE(finalize_hook_runs_for_collected_objects)
{
    TestEnvironment environment;
    environment.vm().heap().set_incremental_sweep_enabled(false);

    s_finalized_host_objects = 0;
    allocate_unreachable_host_objects(environment.realm(), 32);
    scrub_stack();
    environment.vm().heap().collect_garbage();
    EXPECT(s_finalized_host_objects > 0);
}

TEST_CASE(host_objects_keep_their_cells_alive)
{
    TestEnvironment environment;
    environment.vm().heap().set_incremental_sweep_enabled(false);

    GC::Weak<TestHostData> wrappable;
    GC::Weak<TestHostData> host_data;
    auto object = allocate_host_object_owning_its_cells(environment.realm(), wrappable, host_data);
    scrub_stack();
    environment.vm().heap().collect_garbage();

    EXPECT(wrappable.ptr());
    EXPECT(host_data.ptr());
    EXPECT_EQ(object->wrappable().ptr(), wrappable.ptr().ptr());
    EXPECT_EQ(object->host_data().ptr(), host_data.ptr().ptr());
}

TEST_CASE(host_object_layout)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto wrappable = realm.heap().allocate<TestHostData>();
    auto host_data = realm.heap().allocate<TestHostData>();
    auto object = JS::HostObject::create(realm, no_flags_class, nullptr, wrappable, host_data);

    static_assert(JS::HostObject::wrappable_offset() == JS_HOST_OBJECT_WRAPPABLE_OFFSET);
    auto const* bytes = reinterpret_cast<u8 const*>(object.ptr());
    EXPECT_EQ(*reinterpret_cast<JSHostClass const* const*>(bytes + JS_HOST_OBJECT_HOST_CLASS_OFFSET), &no_flags_class);
    EXPECT_EQ(*reinterpret_cast<GC::Cell* const*>(bytes + JS_HOST_OBJECT_WRAPPABLE_OFFSET), static_cast<GC::Cell*>(wrappable.ptr()));
    EXPECT_EQ(*reinterpret_cast<GC::Cell* const*>(bytes + JS_HOST_OBJECT_HOST_DATA_OFFSET), static_cast<GC::Cell*>(host_data.ptr()));
    EXPECT_EQ(&object->host_class(), &no_flags_class);
    EXPECT_EQ(object->wrappable().ptr(), static_cast<GC::Cell*>(wrappable.ptr()));
    EXPECT_EQ(object->host_data().ptr(), static_cast<GC::Cell*>(host_data.ptr()));
}

TEST_CASE(host_class_identity)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto base = JS::HostObject::create(realm, base_class, nullptr);
    auto derived = JS::HostObject::create(realm, derived_class, nullptr);
    auto ordinary = JS::Object::create(realm, nullptr);

    EXPECT_EQ(base->class_name(), "Base"sv);
    EXPECT_EQ(derived->class_name(), "Derived"sv);
    EXPECT_EQ(JS::host_class_of(*derived), &derived_class);
    EXPECT_EQ(JS::host_class_of(*ordinary), nullptr);
    EXPECT(&derived->host_class() == &derived_class);

    EXPECT(JS::is_host_instance_of(*derived, derived_class));
    EXPECT(JS::is_host_instance_of(*derived, base_class));
    EXPECT(!JS::is_host_instance_of(*base, derived_class));
    EXPECT(!JS::is_host_instance_of(*ordinary, base_class));

    EXPECT(is<JS::HostObject>(static_cast<JS::Object&>(*derived)));
    EXPECT(!is<JS::HostObject>(*ordinary));
    EXPECT_EQ(as_if<JS::HostObject>(static_cast<JS::Object&>(*base)), base.ptr());
    EXPECT(!as_if<JS::HostObject>(*ordinary));
}

TEST_CASE(host_data_is_identified_by_its_type)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto host_data = realm.heap().allocate<TestHostData>();
    auto object = JS::HostObject::create(realm, no_flags_class, nullptr, nullptr, host_data);

    EXPECT_EQ(JS::host_data_if<TestHostData>(*object), host_data.ptr());
    EXPECT_EQ(JS::host_data_if<OtherTestHostData>(*object), nullptr);
    EXPECT_EQ(JS::host_data_if<TestHostData>(*JS::HostObject::create(realm, no_flags_class, nullptr)), nullptr);
    EXPECT_EQ(JS::host_data_if<TestHostData>(*JS::Object::create(realm, nullptr)), nullptr);
    EXPECT_EQ(JS::host_data_of(*object).ptr(), static_cast<GC::Cell*>(host_data.ptr()));
    EXPECT(!JS::host_data_of(*JS::Object::create(realm, nullptr)));

    auto other_host_data = realm.heap().allocate<OtherTestHostData>();
    object->set_host_data(other_host_data);
    EXPECT_EQ(JS::host_data_if<TestHostData>(*object), nullptr);
    EXPECT_EQ(JS::host_data_if<OtherTestHostData>(*object), other_host_data.ptr());
    EXPECT_EQ(object->host_data().ptr(), static_cast<GC::Cell*>(other_host_data.ptr()));
}

TEST_CASE(each_host_class_has_its_own_allocator)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto first_a = JS::HostObject::create(realm, allocator_a_class, nullptr);
    auto second_a = JS::HostObject::create(realm, allocator_a_class, nullptr);
    auto b = JS::HostObject::create(realm, allocator_b_class, nullptr);
    auto ordinary = JS::Object::create(realm, nullptr);

    EXPECT_EQ(&allocator_of(*first_a), &allocator_of(*second_a));
    EXPECT_NE(&allocator_of(*first_a), &allocator_of(*b));
    EXPECT_NE(&allocator_of(*first_a), &allocator_of(*ordinary));
    EXPECT_EQ(allocator_of(*first_a).class_name(), "AllocatorA"sv);
    EXPECT_EQ(allocator_of(*b).class_name(), "AllocatorB"sv);
}

TEST_CASE(host_classes_can_share_their_parents_allocator)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto a = JS::HostObject::create(realm, allocator_a_class, nullptr);
    auto sharing_child = JS::HostObject::create(realm, allocator_a_sharing_child_class, nullptr);
    auto sharing_grandchild = JS::HostObject::create(realm, allocator_a_sharing_grandchild_class, nullptr);
    auto isolated_child = JS::HostObject::create(realm, allocator_a_isolated_child_class, nullptr);

    EXPECT_EQ(&allocator_of(*sharing_child), &allocator_of(*a));
    EXPECT_EQ(&allocator_of(*sharing_grandchild), &allocator_of(*a));
    EXPECT_NE(&allocator_of(*isolated_child), &allocator_of(*a));
    EXPECT_EQ(allocator_of(*isolated_child).class_name(), "AllocatorAIsolatedChild"sv);

    EXPECT_EQ(JS::host_class_of(*sharing_grandchild), &allocator_a_sharing_grandchild_class);
    EXPECT_EQ(sharing_grandchild->class_name(), "AllocatorASharingGrandchild"sv);
}

TEST_CASE(get_as_prototype_of_runs_the_get_of_the_object)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto own_key = JS::PropertyKey { "own"_utf16_fly_string };
    auto inherited_key = JS::PropertyKey { "inherited"_utf16_fly_string };
    auto missing_key = JS::PropertyKey { "missing"_utf16_fly_string };

    auto prototype = JS::Object::create(realm, realm.intrinsics().object_prototype());
    prototype->define_direct_property(inherited_key, JS::Value(1), JS::default_attributes);
    auto holder = JS::Object::create(realm, prototype);
    holder->define_direct_property(own_key, JS::Value(2), JS::default_attributes);
    holder->convert_to_prototype_if_needed();
    auto receiver = JS::Object::create(realm, nullptr);

    EXPECT_EQ(MUST(holder->internal_get_as_prototype_of(own_key, receiver, nullptr)), JS::Value(2));
    EXPECT_EQ(MUST(holder->internal_get_as_prototype_of(inherited_key, receiver, nullptr)), JS::Value(1));
    EXPECT_EQ(MUST(holder->internal_get_as_prototype_of(missing_key, receiver, nullptr)), JS::js_undefined());

    auto host = create_intercepting_object(environment);
    EXPECT_EQ(MUST(host->internal_get_as_prototype_of(JS::PropertyKey { "answer"_utf16_fly_string }, receiver, nullptr)), JS::Value(42));
}

TEST_CASE(legacy_platform_object)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto number_list = realm.heap().allocate<NumberList>();
    number_list->numbers = { 10, 20 };
    auto object = JS::HostObject::create(realm, number_list_class, realm.intrinsics().object_prototype(), nullptr, number_list);
    environment.define_global("list"sv, object);

    EXPECT(object->is_platform_object());
    EXPECT(object->may_interfere_with_indexed_property_access());
    EXPECT(!object->eligible_for_own_property_enumeration_fast_path());

    EXPECT_EQ(environment.evaluate("[list[0], list[1], list[2], 1 in list, 2 in list].join()"sv), "10,20,,true,false"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify(Object.getOwnPropertyDescriptor(list, 1))"sv), R"({"value":20,"writable":true,"enumerable":true,"configurable":true})"sv);
    EXPECT_EQ(environment.evaluate("list[2] = 30; list.expando = 'e'; Reflect.ownKeys(list).join()"sv), "0,1,2,expando"sv);
    EXPECT_EQ(number_list->numbers, (Vector<i32> { 10, 20, 30 }));
    EXPECT_EQ(environment.evaluate("list[0] = 11; list[0]"sv), "11"sv);
    EXPECT_EQ(environment.exception_from("list[1] = 'twenty'"sv), "TypeError: not a number"sv);
    EXPECT_EQ(environment.exception_from("list[9] = 90"sv), "RangeError: not contiguous"sv);
    EXPECT_EQ(environment.evaluate("[Reflect.defineProperty(list, 0, { value: 1 }), delete list[0], delete list.expando, Object.keys(list).join()].join()"sv), "false,false,true,0,1,2"sv);
    EXPECT_EQ(environment.evaluate("(() => { let sum = 0; for (const key in list) sum += list[key]; return sum; })()"sv), "61"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify({ ...list })"sv), R"({"0":11,"1":20,"2":30})"sv);

    // A receiver other than the list itself, such as an object that inherits from it, gets an ordinary property.
    EXPECT_EQ(environment.evaluate("const heir = Object.create(list); heir[0] = 'own'; [heir[0], list[0], Object.keys(heir).join()].join()"sv), "own,11,0"sv);
}

namespace {

Vector<StringView> s_named_property_names;

// The named properties of a host's global object, as HTML's WindowProperties object, which sits on the prototype chain
// of Window, provides them: they are not in its shape, and they come and go.
struct NamedPropertiesTraits {
    static JS::ThrowCompletionOr<Optional<JS::PropertyDescriptor>> get_own_property(JS::HostObject const& object, JS::PropertyKey const& property_key)
    {
        for (auto name : s_named_property_names) {
            if (key_is(property_key, name))
                return JS::PropertyDescriptor { .value = JS::Value(JS::PrimitiveString::create(object.vm(), Utf16String::formatted("named {}", name))), .writable = true, .enumerable = false, .configurable = true };
        }
        return object.ordinary_get_own_property(property_key);
    }
};

constexpr JSHostObjectHooks named_properties_hooks = JS::make_host_object_hooks<NamedPropertiesTraits>();
constexpr JSHostClass named_properties_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "NamedProperties"sv, nullptr, &named_properties_hooks, nullptr, JS_HOST_CLASS_IS_PLATFORM_OBJECT | JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE);
constexpr JSHostClass host_global_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "HostGlobal"sv, nullptr, nullptr, nullptr, JS_HOST_CLASS_IS_PLATFORM_OBJECT | JS_HOST_CLASS_IS_GLOBAL_OBJECT);

// The implementation object of a wrapper, as LibWeb's wrappables are: it reaches its main-world wrapper through a weak
// reference, and its parent is another implementation object, whose wrapper a direct getter returns.
class TestNode final : public GC::Cell {
    GC_CELL(TestNode, GC::Cell);
    GC_DECLARE_ALLOCATOR(TestNode);

public:
    static constexpr size_t parent_offset() { return offsetof(TestNode, parent); }
    static constexpr size_t main_world_wrapper_offset() { return offsetof(TestNode, main_world_wrapper); }

    GC::Ptr<TestNode> parent;
    GC::Weak<JS::HostObject> main_world_wrapper;

private:
    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(parent);
    }
};

GC_DEFINE_ALLOCATOR(TestNode);

constexpr JSHostClass test_node_wrapper_class = JS::make_host_class(JS_HOST_CLASS_OBJECT, "TestNode"sv, nullptr, nullptr, nullptr, JS_HOST_CLASS_IS_PLATFORM_OBJECT);

size_t s_parent_getter_calls = 0;

// What the interpreter falls back to when it cannot follow a direct getter's offsets itself.
JS::ThrowCompletionOr<JS::Value> parent_getter(JS::VM& vm)
{
    ++s_parent_getter_calls;
    auto this_value = vm.this_value();
    auto* wrapper = this_value.is_object() ? as_if<JS::HostObject>(this_value.as_object()) : nullptr;
    if (!wrapper || !JS::is_host_instance_of(*wrapper, test_node_wrapper_class))
        return vm.throw_completion<JS::TypeError>("not a node"sv);
    auto& node = static_cast<TestNode&>(*wrapper->wrappable());
    if (!node.parent)
        return JS::js_null();
    return JS::Value(node.parent->main_world_wrapper.ptr());
}

}

TEST_CASE(host_global_object)
{
    GC::Ptr<JS::HostObject> global;
    TestEnvironment environment([&](JS::Realm& realm) -> GC::Ref<JS::Object> {
        auto named_properties = JS::HostObject::create(realm, named_properties_class, realm.intrinsics().object_prototype());
        global = JS::HostObject::create(realm, host_global_class, named_properties);
        return *global;
    });
    auto& realm = environment.realm();

    EXPECT_EQ(&realm.global_object(), global.ptr());
    EXPECT_EQ(&global->shape().realm(), &realm);
    EXPECT_EQ(&global->host_class(), &host_global_class);
    EXPECT_EQ(global->class_name(), "HostGlobal"sv);
    EXPECT(global->has_global_object_flag());
    EXPECT(global->is_platform_object());
    EXPECT(is<JS::HostObject>(realm.global_object()));
    EXPECT(!is<JS::GlobalObject>(realm.global_object()));
    EXPECT_EQ(&MUST(environment.run("globalThis"sv)).as_object(), global.ptr());
    EXPECT_EQ(&MUST(environment.run("this"sv)).as_object(), global.ptr());

    // The realm gives the host's global object the default global bindings, and global var and function declarations
    // become its properties.
    EXPECT_EQ(environment.evaluate("[typeof Object, typeof globalThis.Array, JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, 'Array'))].join()"sv), R"(function,function,{"writable":true,"enumerable":false,"configurable":true})"sv);
    EXPECT_EQ(environment.evaluate("var declared = 1; let lexical = 2; function declaredFunction() { return this; } [globalThis.declared, 'lexical' in globalThis, declaredFunction() === globalThis].join()"sv), "1,false,true"sv);

    // Unqualified names resolve through the global object's prototype chain, where the host's named properties are.
    EXPECT(starts_with(environment.exception_from("namedThing"sv), "ReferenceError: "sv));
    s_named_property_names.append("namedThing"sv);
    EXPECT_EQ(environment.evaluate("[namedThing, 'namedThing' in globalThis, Object.hasOwn(globalThis, 'namedThing')].join()"sv), "named namedThing,true,false"sv);
    s_named_property_names.clear();
    EXPECT(starts_with(environment.exception_from("namedThing"sv), "ReferenceError: "sv));
}

TEST_CASE(direct_getter_follows_the_offsets_of_a_wrapper)
{
    TestEnvironment environment;
    auto& realm = environment.realm();

    auto getter = JS::DirectGetterFunction::create(realm, parent_getter, 0, "parent"_utf16_fly_string,
        JS::DirectGetterConfiguration {
            .wrapper_implementation_offset = JS::HostObject::wrappable_offset(),
            .implementation_value_offset = TestNode::parent_offset(),
            .main_world_wrapper_offset = TestNode::main_world_wrapper_offset(),
            .weak_impl_value_offset = GC::WeakImpl::value_offset(),
        },
        "get"sv);
    auto prototype = JS::Object::create(realm, realm.intrinsics().object_prototype());
    prototype->define_direct_accessor("parent"_utf16_fly_string, getter, nullptr, JS::default_attributes);
    environment.define_global("nodePrototype"sv, prototype);

    auto create_node = [&](GC::Ptr<TestNode> parent, StringView wrapper_name) {
        auto node = realm.heap().allocate<TestNode>();
        node->parent = parent;
        auto wrapper = JS::HostObject::create(realm, test_node_wrapper_class, prototype, node);
        node->main_world_wrapper = wrapper;
        environment.define_global(wrapper_name, wrapper);
        return node;
    };
    auto root = create_node(nullptr, "root"sv);
    auto child = create_node(root, "child"sv);

    // Once the interpreter has checked a cache entry, it reads the parent's main-world wrapper, or null for no parent,
    // straight from the implementation objects, without calling the getter.
    s_parent_getter_calls = 0;
    EXPECT_EQ(environment.evaluate("const parents = []; for (let i = 0; i < 8; ++i) parents.push(child.parent === root); parents.join()"sv), "true,true,true,true,true,true,true,true"sv);
    EXPECT(s_parent_getter_calls < 8);
    s_parent_getter_calls = 0;
    EXPECT_EQ(environment.evaluate("const nulls = []; for (let i = 0; i < 8; ++i) nulls.push(root.parent === null); nulls.join()"sv), "true,true,true,true,true,true,true,true"sv);
    EXPECT(s_parent_getter_calls < 8);

    // A wrapper that is not its implementation object's main-world wrapper, as one of another world, always gets the
    // getter, and so does an object that only inherits from the wrappers' prototype.
    environment.define_global("otherWorldChild"sv, JS::HostObject::create(realm, test_node_wrapper_class, prototype, child));
    s_parent_getter_calls = 0;
    EXPECT_EQ(environment.evaluate("const otherWorldParents = []; for (let i = 0; i < 8; ++i) otherWorldParents.push(otherWorldChild.parent === root); otherWorldParents.join()"sv), "true,true,true,true,true,true,true,true"sv);
    EXPECT_EQ(s_parent_getter_calls, 8u);
    EXPECT_EQ(environment.exception_from("function parentOf(node) { return node.parent; } for (let i = 0; i < 4; ++i) parentOf(child); parentOf(Object.create(nodePrototype))"sv), "TypeError: not a node"sv);
}

namespace {

size_t s_finalized_host_functions = 0;

// Sums its arguments, and throws when there are none.
struct AdderTraits {
    static JS::ThrowCompletionOr<JS::Value> call(JS::HostFunction& function, JS::VM& vm)
    {
        if (vm.argument_count() == 0)
            return hook_error(function, "call"sv);
        double sum = 0;
        for (size_t i = 0; i < vm.argument_count(); ++i)
            sum += TRY(vm.argument(i).to_double(vm));
        return JS::Value(sum);
    }

    static void finalize(JS::HostFunction&) { ++s_finalized_host_functions; }
};

// Makes { value } objects from new.target's prototype, and throws without an argument.
struct MakerTraits {
    static JS::ThrowCompletionOr<JS::Value> call(JS::HostFunction& function, JS::VM&)
    {
        return hook_error(function, "call"sv);
    }

    static JS::ThrowCompletionOr<GC::Ref<JS::Object>> construct(JS::HostFunction& function, JS::VM& vm, JS::FunctionObject& new_target)
    {
        if (vm.argument_count() == 0)
            return hook_error(function, "construct"sv);
        auto& realm = *vm.current_realm();
        auto prototype = TRY(new_target.get(vm.names.prototype));
        auto object = JS::Object::create(realm, prototype.is_object() ? &prototype.as_object() : realm.intrinsics().object_prototype().ptr());
        object->define_direct_property("value"_utf16_fly_string, vm.argument(0), JS::default_attributes);
        return object;
    }
};

constexpr JSHostFunctionHooks adder_hooks = JS::make_host_function_hooks<AdderTraits>();
constexpr JSHostClass adder_class = JS::make_host_class(JS_HOST_CLASS_FUNCTION, "Adder"sv, nullptr, &adder_hooks, nullptr, 0);
constexpr JSHostFunctionHooks maker_hooks = JS::make_host_function_hooks<MakerTraits>();
constexpr JSHostClass maker_class = JS::make_host_class(JS_HOST_CLASS_FUNCTION, "Maker"sv, nullptr, &maker_hooks, nullptr, JS_HOST_CLASS_HAS_CONSTRUCTOR);

Optional<JS::Value> s_last_deleted_element;

// Doubles numbers stored at indices and rejects negative ones, then lets the array store them.
struct DoublingArrayTraits {
    static JS::ThrowCompletionOr<bool> set(JS::HostArray& array, JS::PropertyKey const& property_key, JS::Value value, JS::Value receiver, JS::CacheableSetPropertyMetadata* metadata, JS::Object::PropertyLookupPhase phase)
    {
        if (property_key.is_number() && value.is_number()) {
            if (value.as_double() < 0)
                return hook_error(array, "set"sv);
            value = JS::Value(value.as_double() * 2);
        }
        return array.array_set(property_key, value, receiver, metadata, phase);
    }

    static JS::ThrowCompletionOr<bool> delete_property(JS::HostArray& array, JS::PropertyKey const& property_key)
    {
        if (key_is(property_key, "throwing"sv))
            return hook_error(array, "delete_property"sv);
        if (property_key.is_number()) {
            if (auto element = array.indexed_get(property_key.as_number()); element.has_value())
                s_last_deleted_element = element->value;
        }
        return array.array_delete(property_key);
    }
};

constexpr JSHostArrayHooks doubling_array_hooks = JS::make_host_array_hooks<DoublingArrayTraits>();
constexpr JSHostClass doubling_array_class = JS::make_host_class(JS_HOST_CLASS_ARRAY, "DoublingArray"sv, nullptr, &doubling_array_hooks, nullptr, JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS);
constexpr JSHostClass hookless_array_class = JS::make_host_class(JS_HOST_CLASS_ARRAY, "HooklessArray"sv, nullptr, nullptr, nullptr, 0);

NEVER_INLINE void allocate_unreachable_host_functions(JS::Realm& realm, size_t count)
{
    for (size_t i = 0; i < count; ++i)
        (void)JS::HostFunction::create(realm, adder_class, "add"_utf16_fly_string, 2);
}

}

TEST_CASE(host_function_calls)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto host_data = realm.heap().allocate<TestHostData>();
    auto adder = JS::HostFunction::create(realm, adder_class, "add"_utf16_fly_string, 2, nullptr, host_data);
    environment.define_global("add"sv, adder);

    EXPECT_EQ(environment.evaluate("add(1, 2, 3)"sv), "6"sv);
    EXPECT_EQ(environment.evaluate("typeof add"sv), "function"sv);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(add).join()"sv), "length,name"sv);
    EXPECT_EQ(environment.evaluate("add.name + add.length"sv), "add2"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(add) === Function.prototype"sv), "true"sv);
    EXPECT_EQ(environment.exception_from("add()"sv), "TypeError: call threw"sv);
    EXPECT(starts_with(environment.exception_from("new add(1)"sv), "TypeError: "sv));
    EXPECT_EQ(environment.evaluate("[1, 2].map(number => add(number, 1)).join()"sv), "2,3"sv);

    EXPECT_EQ(adder->class_name(), "Adder"sv);
    EXPECT_EQ(adder->name_for_call_stack(), "add"sv);
    EXPECT_EQ(adder->realm(), &realm);
    EXPECT_EQ(&adder->host_class(), &adder_class);
    EXPECT_EQ(adder->host_data().ptr(), static_cast<GC::Cell*>(host_data.ptr()));
    EXPECT_EQ(JS::host_class_of(*adder), &adder_class);
    EXPECT(is<JS::HostFunction>(static_cast<JS::Object&>(*adder)));
    EXPECT(is<JS::NativeFunction>(static_cast<JS::Object&>(*adder)));
    EXPECT(is<JS::FunctionObject>(static_cast<JS::Object&>(*adder)));
    EXPECT(!is<JS::HostObject>(static_cast<JS::Object&>(*adder)));
    EXPECT_EQ(JS::host_data_if<TestHostData>(*adder), host_data.ptr());

    auto other_host_data = realm.heap().allocate<OtherTestHostData>();
    adder->set_host_data(other_host_data);
    EXPECT_EQ(JS::host_data_if<OtherTestHostData>(*adder), other_host_data.ptr());
}

TEST_CASE(host_function_constructs)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto& vm = environment.vm();
    auto maker = JS::HostFunction::create(realm, maker_class, "Maker"_utf16_fly_string, 1);
    maker->define_direct_property(vm.names.prototype, JS::Object::create(realm, realm.intrinsics().object_prototype()), 0);
    environment.define_global("Maker"sv, maker);

    EXPECT_EQ(environment.evaluate("const made = new Maker(5); made.value + ' ' + (made instanceof Maker)"sv), "5 true"sv);
    EXPECT_EQ(environment.evaluate("class Derived extends Maker {} const derived = new Derived(3); derived.value + ' ' + (derived instanceof Derived)"sv), "3 true"sv);
    EXPECT_EQ(environment.evaluate("Reflect.construct(Maker, [7], Derived) instanceof Derived"sv), "true"sv);
    EXPECT_EQ(environment.exception_from("new Maker()"sv), "TypeError: construct threw"sv);
    EXPECT_EQ(environment.exception_from("Maker(1)"sv), "TypeError: call threw"sv);
}

TEST_CASE(host_function_without_own_properties)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto& vm = environment.vm();
    auto error_constructor = MUST(environment.run("Error"sv));
    auto function = JS::HostFunction::create_without_own_properties(realm, adder_class, "bare"_utf16_fly_string, &error_constructor.as_object());
    environment.define_global("bare"sv, function);

    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(bare).length"sv), "0"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(bare) === Error"sv), "true"sv);

    function->define_direct_property(vm.names.prototype, JS::Object::create(realm, nullptr), 0);
    function->define_direct_property(vm.names.name, JS::PrimitiveString::create(vm, "bare"_utf16), JS::Attribute::Configurable);
    function->define_direct_property(vm.names.length, JS::Value(1), JS::Attribute::Configurable);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(bare).join()"sv), "prototype,name,length"sv);
    EXPECT_EQ(environment.evaluate("bare(4, 5)"sv), "9"sv);
}

TEST_CASE(host_function_finalize_hook)
{
    TestEnvironment environment;
    environment.vm().heap().set_incremental_sweep_enabled(false);

    s_finalized_host_functions = 0;
    allocate_unreachable_host_functions(environment.realm(), 32);
    scrub_stack();
    environment.vm().heap().collect_garbage();
    EXPECT(s_finalized_host_functions > 0);
}

TEST_CASE(host_array_hooks)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto host_data = realm.heap().allocate<TestHostData>();
    auto array = JS::HostArray::create(realm, doubling_array_class, nullptr, host_data);
    environment.define_global("doubling"sv, array);

    EXPECT_EQ(environment.evaluate("doubling[0] = 2; doubling[1] = 5; doubling.push(7); doubling.join()"sv), "4,10,14"sv);
    EXPECT_EQ(environment.evaluate("doubling.length + ' ' + Array.isArray(doubling)"sv), "3 true"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(doubling) === Array.prototype"sv), "true"sv);

    s_last_deleted_element.clear();
    EXPECT_EQ(environment.evaluate("delete doubling[1]"sv), "true"sv);
    EXPECT_EQ(s_last_deleted_element, JS::Value(10));
    EXPECT_EQ(environment.evaluate("doubling.length + ' ' + (1 in doubling)"sv), "3 false"sv);

    EXPECT_EQ(environment.exception_from("doubling[0] = -1"sv), "TypeError: set threw"sv);
    EXPECT_EQ(environment.exception_from("delete doubling.throwing"sv), "TypeError: delete_property threw"sv);
    EXPECT_EQ(environment.evaluate("doubling[0]"sv), "4"sv);

    // The embedder's own [[Set]] and [[Delete]] of the array run the hooks, and array_set() and array_delete() do not.
    EXPECT(MUST(array->internal_set(JS::PropertyKey { 5u }, JS::Value(1), array)));
    EXPECT(MUST(array->array_set(JS::PropertyKey { 6u }, JS::Value(1), array)));
    EXPECT_EQ(environment.evaluate("doubling[5] + ' ' + doubling[6]"sv), "2 1"sv);
    s_last_deleted_element.clear();
    EXPECT(MUST(array->array_delete(JS::PropertyKey { 5u })));
    EXPECT(!s_last_deleted_element.has_value());

    EXPECT(array->may_interfere_with_indexed_property_access());
    EXPECT_EQ(array->class_name(), "DoublingArray"sv);
    EXPECT_EQ(JS::host_class_of(*array), &doubling_array_class);
    EXPECT_EQ(&array->host_class(), &doubling_array_class);
    EXPECT(is<JS::HostArray>(static_cast<JS::Object&>(*array)));
    EXPECT(is<JS::Array>(static_cast<JS::Object&>(*array)));
    EXPECT(!is<JS::HostObject>(static_cast<JS::Object&>(*array)));
    EXPECT_EQ(JS::host_data_if<TestHostData>(*array), host_data.ptr());
    EXPECT_EQ(array->host_data().ptr(), static_cast<GC::Cell*>(host_data.ptr()));

    auto other_host_data = realm.heap().allocate<OtherTestHostData>();
    array->set_host_data(other_host_data);
    EXPECT_EQ(JS::host_data_if<OtherTestHostData>(*array), other_host_data.ptr());
}

TEST_CASE(host_array_without_hooks_is_an_array)
{
    TestEnvironment environment;
    auto& realm = environment.realm();
    auto array = JS::HostArray::create(realm, hookless_array_class);
    environment.define_global("hookless"sv, array);

    EXPECT_EQ(environment.evaluate("hookless.push(1, 2); hookless[5] = 3; delete hookless[0]; hookless.length + ' ' + JSON.stringify(hookless)"sv), "6 [null,2,null,null,null,3]"sv);
    EXPECT(!array->may_interfere_with_indexed_property_access());

    auto with_prototype = JS::HostArray::create(realm, hookless_array_class, realm.intrinsics().object_prototype());
    environment.define_global("withPrototype"sv, with_prototype);
    EXPECT_EQ(environment.evaluate("[Object.getPrototypeOf(withPrototype) === Object.prototype, Array.isArray(withPrototype)].join()"sv), "true,true"sv);
}
