/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// The scenarios of Tests/LibJS/test-host-object.cpp, run on the Rust runtime through the embedding ABI alone. The
// hooks are plain C functions written against LibJS/HostObjectABI.h, as HostClassBuilder.h needs the C++ runtime's
// types, and they leave what they do not answer themselves to the js_object_ordinary_* operations.

#include <AK/BitCast.h>
#include <AK/ByteString.h>
#include <AK/QuickSort.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/CAPI.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Heap.h>
#include <LibGC/HeapBlock.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>
#include <LibJS/HostObjectABI.h>

#include "EmbeddingTest.h"

namespace {

// Hooks take no VM, as a process has one, so they reach the test's VM through this.
EmbeddedVM* s_embedded_vm = nullptr;

JSVM* hook_vm()
{
    VERIFY(s_embedded_vm);
    return s_embedded_vm->vm();
}

class TestEnvironment {
    AK_MAKE_NONCOPYABLE(TestEnvironment);
    AK_MAKE_NONMOVABLE(TestEnvironment);

public:
    TestEnvironment()
        : m_embedded_vm(EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options))
    {
        s_embedded_vm = m_embedded_vm.ptr();
    }

    ~TestEnvironment()
    {
        s_embedded_vm = nullptr;
    }

    JSVM* vm() { return m_embedded_vm->vm(); }
    JSRealm* realm() { return m_embedded_vm->realm(); }
    JSObject* object_prototype() { return m_embedded_vm->intrinsic(JS_INTRINSIC_OBJECT_PROTOTYPE); }
    JSObject* object_of(StringView source) { return m_embedded_vm->object_of(source); }

    void define_global(StringView name, JSObject* object)
    {
        auto key_name = Utf16FlyString::from_utf8(name);
        JSPropertyKey key { key_name.raw_identity() };
        js_object_define_direct_property(vm(), m_embedded_vm->global_object(), &key, value_of_object(object), JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE);
    }

    // Returns the completion value as a string, or "uncaught <error>" for an exception that escaped the script.
    ByteString evaluate(StringView source)
    {
        auto completion = m_embedded_vm->evaluate(source);
        auto string = Utf16String::adopt_raw(js_value_to_utf16_string_without_side_effects(completion.payload)).to_byte_string();
        if (completion.variant == JS_COMPLETION_THROW)
            return ByteString::formatted("uncaught {}", string);
        return string;
    }

    // Runs the statements in a function and returns "<name>: <message>" of what they throw, or "no exception".
    ByteString exception_from(StringView statements)
    {
        return evaluate(ByteString::formatted("(() => {{ try {{ {}; }} catch (error) {{ return `${{error.name}}: ${{error.message}}`; }} return 'no exception'; }})()", statements));
    }

    JSObject* create_host_object(JSHostClass const& host_class, JSObject* prototype, void* wrappable = nullptr, void* host_data = nullptr)
    {
        return js_host_object_create(vm(), realm(), &host_class, prototype, wrappable, host_data);
    }

private:
    NonnullOwnPtr<EmbeddedVM> m_embedded_vm;
};

// A table of object hooks with only the hooks that `set_hooks` sets, as designated initializers have to name every
// field.
template<typename Callback>
consteval JSHostObjectHooks object_hooks_with(Callback set_hooks)
{
    JSHostObjectHooks hooks {};
    set_hooks(hooks);
    return hooks;
}

constexpr JSHostClass make_host_class(u8 kind, StringView name, JSHostClass const* parent, void const* hooks, u32 flags)
{
    return JSHostClass {
        .abi_version = JS_HOST_ABI_VERSION,
        .kind = kind,
        .reserved = 0,
        .flags = flags,
        .name = name.characters_without_null_termination(),
        .name_length = name.length(),
        .parent = parent,
        .hooks = hooks,
        .user_data = nullptr,
    };
}

// The encodings of JSValues and JSPropertyKeys that the hooks need besides those of EmbeddingTest.h.
constexpr u64 string_value_tag = 0b010 | GC::IS_CELL_BIT;
constexpr u64 property_key_number_flag = 3;

JSValue string_value(StringView ascii)
{
    auto* string = js_string_create_from_utf16_view(hook_vm(), ascii_view(ascii));
    return (string_value_tag << GC::TAG_SHIFT) | GC::NanBoxedValue::encode_pointer_bits(string);
}

bool is_number(JSValue value)
{
    auto tag = value >> GC::TAG_SHIFT;
    return tag == int32_value_tag || (tag & GC::BASE_TAG) != GC::BASE_TAG || value == GC::CANON_NAN_BITS;
}

double as_double(JSValue value)
{
    if ((value >> GC::TAG_SHIFT) == int32_value_tag)
        return static_cast<i32>(static_cast<u32>(value));
    return bit_cast<double>(value);
}

JSValue number_value(double number)
{
    return bit_cast<JSValue>(number);
}

bool key_is(JSPropertyKey key, StringView name)
{
    return key.bits == Utf16FlyString::from_utf8(name).raw_identity();
}

bool is_index_key(JSPropertyKey key)
{
    return (key.bits & property_key_number_flag) == property_key_number_flag;
}

u32 index_of_key(JSPropertyKey key)
{
    return static_cast<u32>(key.bits >> 2);
}

JSCompletion normal_completion(u64 payload)
{
    return { payload, JS_COMPLETION_NORMAL };
}

JSCompletion hook_error(StringView hook)
{
    auto message = ByteString::formatted("{} threw", hook);
    return js_error_throw(hook_vm(), JS_ERROR_KIND_TYPE_ERROR, ascii_view(message));
}

// What a facade's VM::argument() reads: the arguments are the last slots of the running execution context, which
// follow its fixed fields.
JSValue argument(JSVM* vm, size_t index)
{
    u8 const* context = nullptr;
    __builtin_memcpy(&context, reinterpret_cast<u8 const*>(vm) + JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET, sizeof(context));
    auto slot_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET);
    auto argument_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
    if (index >= argument_count)
        return js_undefined;
    auto const* slots = reinterpret_cast<JSValue const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_SIZE);
    return slots[slot_count - argument_count + index];
}

size_t argument_count(JSVM* vm)
{
    u8 const* context = nullptr;
    __builtin_memcpy(&context, reinterpret_cast<u8 const*>(vm) + JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET, sizeof(context));
    return *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
}

u16 object_flags(JSObject const* object)
{
    u16 flags = 0;
    static_assert(JS_LAYOUT_OBJECT_FLAGS_SIZE == sizeof(flags));
    __builtin_memcpy(&flags, reinterpret_cast<u8 const*>(object) + JS_LAYOUT_OBJECT_FLAGS_OFFSET, sizeof(flags));
    return flags;
}

// The name LibGC reports for a cell, which is that of its host class.
StringView class_name_of(JSObject const* object)
{
    auto const* cell = reinterpret_cast<GC::Cell const*>(object);
    size_t length = 0;
    auto const* name = cell->type_info().class_name(cell, &length);
    return { name, length };
}

GC::CellAllocator& allocator_of(void const* cell)
{
    return GC::HeapBlock::from_cell(static_cast<GC::Cell const*>(cell))->cell_allocator();
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

// A script for hooks to run while they run, and the hooks that ran it. Each hook runs it, and collects garbage, only
// when no hook is running it already, so that the script can reach the hooks again without running itself forever.
Optional<StringView> s_script_for_hooks_to_reenter_with;
bool s_hook_is_reentering = false;
Vector<StringView> s_hooks_that_reentered;

void reenter(StringView hook)
{
    if (!s_script_for_hooks_to_reenter_with.has_value() || s_hook_is_reentering)
        return;
    s_hook_is_reentering = true;
    auto completion = s_embedded_vm->evaluate(*s_script_for_hooks_to_reenter_with);
    js_vm_collect_garbage(hook_vm());
    s_hook_is_reentering = false;
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    if (!s_hooks_that_reentered.contains_slow(hook))
        s_hooks_that_reentered.append(hook);
}

bool s_keyless_hooks_throw = false;
Optional<JSPropertyDescriptor> s_last_defined_descriptor;
// Not given, given and absent, or given with a descriptor.
Optional<Optional<JSPropertyDescriptor>> s_last_precomputed_get_own_property;
JSValue s_last_intercepted_value = 0;

// Implements every object hook. Hooks taking a key answer some keys themselves, throw for "throwing", and leave the
// rest to the ordinary internal method, the way bindings do.
JSCompletion intercepting_get_prototype_of(JSObject* object)
{
    reenter("get_prototype_of"sv);
    if (s_keyless_hooks_throw)
        return hook_error("get_prototype_of"sv);
    return js_object_ordinary_get_prototype_of(hook_vm(), object);
}

JSCompletion intercepting_set_prototype_of(JSObject* object, JSObject* prototype)
{
    reenter("set_prototype_of"sv);
    if (s_keyless_hooks_throw)
        return hook_error("set_prototype_of"sv);
    return js_object_ordinary_set_prototype_of(hook_vm(), object, prototype);
}

JSCompletion intercepting_is_extensible(JSObject* object)
{
    reenter("is_extensible"sv);
    if (s_keyless_hooks_throw)
        return hook_error("is_extensible"sv);
    return js_object_ordinary_is_extensible(hook_vm(), object);
}

JSCompletion intercepting_prevent_extensions(JSObject*)
{
    reenter("prevent_extensions"sv);
    if (s_keyless_hooks_throw)
        return hook_error("prevent_extensions"sv);
    return normal_completion(0);
}

JSCompletion intercepting_get_own_property(JSObject* object, JSPropertyKey key, JSPropertyDescriptor* out)
{
    reenter("get_own_property"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("get_own_property"sv);
    if (key_is(key, "virtual"sv)) {
        *out = JSPropertyDescriptor {
            .value = string_value("virtual value"sv),
            .get = nullptr,
            .set = nullptr,
            .property_offset = 0,
            .flags = JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE | JS_PD_HAS_CONFIGURABLE | JS_PD_CONFIGURABLE,
        };
        return normal_completion(0);
    }
    return js_object_ordinary_get_own_property(hook_vm(), object, &key, out);
}

JSCompletion intercepting_define_own_property(JSObject* object, JSPropertyKey key, JSPropertyDescriptor* descriptor, JSPropertyDescriptor const* precomputed_get_own_property)
{
    reenter("define_own_property"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("define_own_property"sv);
    if (key_is(key, "rejected"sv))
        return normal_completion(0);
    s_last_defined_descriptor = *descriptor;
    s_last_precomputed_get_own_property.emplace();
    if (precomputed_get_own_property)
        s_last_precomputed_get_own_property.emplace(*precomputed_get_own_property);
    return js_object_ordinary_define_own_property(hook_vm(), object, &key, descriptor, precomputed_get_own_property);
}

JSCompletion intercepting_has_property(JSObject* object, JSPropertyKey key)
{
    reenter("has_property"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("has_property"sv);
    if (key_is(key, "magic"sv))
        return normal_completion(1);
    return js_object_ordinary_has_property(hook_vm(), object, &key);
}

JSCompletion intercepting_get(JSObject* object, JSPropertyKey key, JSValue receiver, JSGetCacheMetadata* metadata, u8 phase)
{
    reenter("get"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("get"sv);
    if (key_is(key, "answer"sv))
        return normal_completion(int32_value(42));
    return js_object_ordinary_get(hook_vm(), object, &key, receiver, metadata, phase);
}

JSCompletion intercepting_set(JSObject* object, JSPropertyKey key, JSValue value, JSValue receiver, JSSetCacheMetadata* metadata, u8 phase)
{
    reenter("set"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("set"sv);
    if (key_is(key, "intercepted"sv)) {
        s_last_intercepted_value = value;
        return normal_completion(1);
    }
    return js_object_ordinary_set(hook_vm(), object, &key, value, receiver, metadata, phase);
}

JSCompletion intercepting_delete_property(JSObject* object, JSPropertyKey key)
{
    reenter("delete_property"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("delete_property"sv);
    if (key_is(key, "undeletable"sv))
        return normal_completion(0);
    return js_object_ordinary_delete(hook_vm(), object, &key);
}

JSCompletion intercepting_own_property_keys(JSObject* object, JSValueSink* keys)
{
    reenter("own_property_keys"sv);
    if (s_keyless_hooks_throw)
        return hook_error("own_property_keys"sv);
    auto completion = js_object_ordinary_own_property_keys(hook_vm(), object, keys);
    if (completion.variant == JS_COMPLETION_NORMAL)
        keys->append(keys->context, string_value("virtual"sv));
    return completion;
}

constexpr JSHostObjectHooks intercepting_hooks {
    .get_prototype_of = intercepting_get_prototype_of,
    .set_prototype_of = intercepting_set_prototype_of,
    .is_extensible = intercepting_is_extensible,
    .prevent_extensions = intercepting_prevent_extensions,
    .get_own_property = intercepting_get_own_property,
    .define_own_property = intercepting_define_own_property,
    .has_property = intercepting_has_property,
    .get = intercepting_get,
    .set = intercepting_set,
    .delete_property = intercepting_delete_property,
    .own_property_keys = intercepting_own_property_keys,
    .is_cacheable_for_inherited_property = nullptr,
    .error_data = nullptr,
    .finalize = nullptr,
};

}

// Declared and defined apart, as an embedder's header and source file do.
extern JSHostClass const intercepting_host_class;
constexpr JSHostClass intercepting_host_class = make_host_class(JS_HOST_CLASS_OBJECT, "InterceptingHostObject"sv, nullptr, &intercepting_hooks, 0);

namespace {

size_t s_finalized_host_objects = 0;

void count_finalized_host_object(JSObject*)
{
    ++s_finalized_host_objects;
}

constexpr JSHostObjectHooks counting_finalizer_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.finalize = count_finalized_host_object; });
constexpr JSHostClass counting_finalizer_class = make_host_class(JS_HOST_CLASS_OBJECT, "CountingFinalizer"sv, nullptr, &counting_finalizer_hooks, 0);

void* error_data_of_host_data(JSObject* object)
{
    reenter("error_data"sv);
    auto* error = static_cast<JSObject*>(js_host_object_host_data_of(object));
    return const_cast<JSErrorData*>(js_error_data_of(error));
}

constexpr JSHostObjectHooks error_data_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.error_data = error_data_of_host_data; });
constexpr JSHostClass error_data_class = make_host_class(JS_HOST_CLASS_OBJECT, "ErrorDataHostObject"sv, nullptr, &error_data_hooks, 0);

bool s_inherited_property_is_cacheable = true;

bool inherited_property_cacheability(JSObject*)
{
    reenter("is_cacheable_for_inherited_property"sv);
    return s_inherited_property_is_cacheable;
}

constexpr JSHostObjectHooks inherited_cacheability_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.is_cacheable_for_inherited_property = inherited_property_cacheability; });
constexpr JSHostClass inherited_cacheability_class = make_host_class(JS_HOST_CLASS_OBJECT, "InheritedCacheability"sv, nullptr, &inherited_cacheability_hooks, 0);

// Only a [[Get]] hook, which doubles the numbers that the ordinary [[Get]] finds. It passes no cache metadata on, so that
// inline caches never answer for it.
JSCompletion doubling_get(JSObject* object, JSPropertyKey key, JSValue receiver, JSGetCacheMetadata*, u8 phase)
{
    auto completion = js_object_ordinary_get(hook_vm(), object, &key, receiver, nullptr, phase);
    if (completion.variant != JS_COMPLETION_NORMAL || !is_number(completion.payload))
        return completion;
    return normal_completion(number_value(as_double(completion.payload) * 2));
}

constexpr JSHostObjectHooks number_doubling_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.get = doubling_get; });
constexpr JSHostClass number_doubling_class = make_host_class(JS_HOST_CLASS_OBJECT, "NumberDoubling"sv, nullptr, &number_doubling_hooks, 0);

// The first leaves the descriptor it is given zeroed, and the second accepts every definition but zeroes the descriptor
// it writes back.
JSCompletion report_every_property_absent(JSObject*, JSPropertyKey, JSPropertyDescriptor*)
{
    return normal_completion(0);
}

JSCompletion accept_definition_and_zero_descriptor(JSObject*, JSPropertyKey, JSPropertyDescriptor* descriptor, JSPropertyDescriptor const*)
{
    *descriptor = {};
    return normal_completion(1);
}

constexpr JSHostObjectHooks hand_written_hooks = object_hooks_with([](JSHostObjectHooks& hooks) {
    hooks.get_own_property = report_every_property_absent;
    hooks.define_own_property = accept_definition_and_zero_descriptor;
});
constexpr JSHostClass hand_written_class = make_host_class(JS_HOST_CLASS_OBJECT, "HandWritten"sv, nullptr, &hand_written_hooks, 0);

JSPropertyDescriptor s_recorded_descriptor {};

JSCompletion recording_define_own_property(JSObject* object, JSPropertyKey key, JSPropertyDescriptor* descriptor, JSPropertyDescriptor const* precomputed_get_own_property)
{
    s_recorded_descriptor = *descriptor;
    return js_object_ordinary_define_own_property(hook_vm(), object, &key, descriptor, precomputed_get_own_property);
}

constexpr JSHostObjectHooks recording_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.define_own_property = recording_define_own_property; });
constexpr JSHostClass recording_class = make_host_class(JS_HOST_CLASS_OBJECT, "Recording"sv, nullptr, &recording_hooks, 0);

size_t s_forwarded_lookups = 0;
bool s_forwarding_passes_metadata_on = true;

// Forwards every [[Get]] to the object in its host data, as a lookup in that object's prototype chain, the way
// WindowProxy forwards to its Window.
JSCompletion forwarding_get(JSObject* object, JSPropertyKey key, JSValue receiver, JSGetCacheMetadata* metadata, u8)
{
    ++s_forwarded_lookups;
    auto* target = static_cast<JSObject*>(js_host_object_host_data_of(object));
    return js_object_internal_get_as_prototype_of(hook_vm(), target, &key, receiver, s_forwarding_passes_metadata_on ? metadata : nullptr);
}

constexpr JSHostObjectHooks forwarding_hooks = object_hooks_with([](JSHostObjectHooks& hooks) { hooks.get = forwarding_get; });
constexpr JSHostClass forwarding_class = make_host_class(JS_HOST_CLASS_OBJECT, "Forwarding"sv, nullptr, &forwarding_hooks, 0);

constexpr u32 all_object_flags = JS_HOST_CLASS_IS_PLATFORM_OBJECT
    | JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY
    | JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS
    | JS_HOST_CLASS_IS_HTMLDDA
    | JS_HOST_CLASS_IS_GLOBAL_OBJECT
    | JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE
    | JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH;

constexpr JSHostClass all_flags_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllFlags"sv, nullptr, nullptr, all_object_flags);
constexpr JSHostClass no_flags_class = make_host_class(JS_HOST_CLASS_OBJECT, "NoFlags"sv, nullptr, nullptr, 0);
constexpr JSHostClass immutable_prototype_class = make_host_class(JS_HOST_CLASS_OBJECT, "ImmutablePrototype"sv, nullptr, nullptr, JS_HOST_CLASS_IMMUTABLE_PROTOTYPE);
constexpr JSHostClass base_class = make_host_class(JS_HOST_CLASS_OBJECT, "Base"sv, nullptr, nullptr, 0);
constexpr JSHostClass derived_class = make_host_class(JS_HOST_CLASS_OBJECT, "Derived"sv, &base_class, nullptr, 0);
constexpr JSHostClass allocator_a_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorA"sv, nullptr, nullptr, 0);
constexpr JSHostClass allocator_b_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorB"sv, nullptr, nullptr, 0);
constexpr JSHostClass allocator_a_sharing_child_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorASharingChild"sv, &allocator_a_class, nullptr, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT);
constexpr JSHostClass allocator_a_sharing_grandchild_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorASharingGrandchild"sv, &allocator_a_sharing_child_class, nullptr, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT);
constexpr JSHostClass allocator_a_isolated_child_class = make_host_class(JS_HOST_CLASS_OBJECT, "AllocatorAIsolatedChild"sv, &allocator_a_class, nullptr, 0);

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

// host_data_if<T>() of the facade: the companion cell if it is exactly a T.
template<typename T>
T* host_data_if(JSObject* object)
{
    auto* host_data = static_cast<GC::Cell*>(js_host_object_host_data_of(object));
    if (!host_data || !GC::cell_was_allocated_from(*host_data, T::cell_allocator))
        return nullptr;
    return static_cast<T*>(host_data);
}

JSObject* create_intercepting_object(TestEnvironment& environment)
{
    auto* object = environment.create_host_object(intercepting_host_class, environment.object_prototype());
    environment.define_global("host"sv, object);
    return object;
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
    EXPECT_EQ(s_last_intercepted_value, int32_value(7));
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

    auto* intercepting = create_intercepting_object(environment);
    EXPECT(!js_object_eligible_for_own_property_enumeration_fast_path(intercepting));
    EXPECT_EQ(environment.evaluate("host.plain = 1"sv), "1"sv);
    EXPECT_EQ(environment.evaluate("Object.keys(host).join()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("(() => { const keys = []; for (const key in host) keys.push(key); return keys.join(); })()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("JSON.stringify(host)"sv), R"({"plain":1,"virtual":"virtual value"})"sv);
    EXPECT_EQ(environment.evaluate("Object.keys(Object.assign({}, host)).join()"sv), "plain,virtual"sv);
    EXPECT_EQ(environment.evaluate("Object.keys({ ...host }).join()"sv), "plain,virtual"sv);

    auto* doubling = environment.create_host_object(number_doubling_class, environment.object_prototype());
    EXPECT(!js_object_eligible_for_own_property_enumeration_fast_path(doubling));
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
    environment.define_global("handWritten"sv, environment.create_host_object(hand_written_class, environment.object_prototype()));

    EXPECT_EQ(environment.evaluate("Reflect.defineProperty(handWritten, 'defined', { value: 1 })"sv), "true"sv);
    EXPECT_EQ(environment.evaluate("handWritten.assigned = 2"sv), "2"sv);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyDescriptor(handWritten, 'defined')"sv), "undefined"sv);
    EXPECT_EQ(environment.evaluate("'assigned' in handWritten"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("handWritten.toString === Object.prototype.toString"sv), "true"sv);
}

TEST_CASE(property_descriptors_round_trip_through_the_abi)
{
    TestEnvironment environment;
    auto* vm = environment.vm();
    auto* object = environment.create_host_object(recording_class, environment.object_prototype());
    auto* getter = environment.object_of("(function getter() {})"sv);
    auto data_name = "data"_utf16_fly_string;
    auto accessor_name = "accessor"_utf16_fly_string;
    auto empty_name = "empty"_utf16_fly_string;
    JSPropertyKey data_key { data_name.raw_identity() };
    JSPropertyKey accessor_key { accessor_name.raw_identity() };
    JSPropertyKey empty_key { empty_name.raw_identity() };

    // Each descriptor reaches the hook as the engine's own descriptor converted back, which must lose nothing.
    constexpr u16 data_flags = JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_HAS_CONFIGURABLE | JS_PD_CONFIGURABLE;
    JSPropertyDescriptor data_descriptor { .value = number_value(1.5), .get = nullptr, .set = nullptr, .property_offset = 0, .flags = data_flags };
    auto definition = js_object_internal_define_own_property(vm, object, &data_key, &data_descriptor, nullptr);
    EXPECT_EQ(definition.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(definition.payload, 1u);
    EXPECT_EQ(s_recorded_descriptor.flags, data_flags);
    EXPECT_EQ(s_recorded_descriptor.value, number_value(1.5));

    // The engine reports where it stored the new property, and the hook passes that on unchanged.
    EXPECT_EQ(data_descriptor.flags, data_flags | JS_PD_HAS_PROPERTY_OFFSET);
    JSPropertyDescriptor stored {};
    EXPECT_EQ(js_object_ordinary_get_own_property(vm, object, &data_key, &stored).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(stored.flags, data_flags | JS_PD_HAS_PROPERTY_OFFSET);
    EXPECT_EQ(stored.property_offset, data_descriptor.property_offset);

    JSPropertyDescriptor redefinition { .value = number_value(2.5), .get = nullptr, .set = nullptr, .property_offset = stored.property_offset, .flags = JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_PROPERTY_OFFSET };
    EXPECT_EQ(js_object_internal_define_own_property(vm, object, &data_key, &redefinition, nullptr).payload, 1u);
    EXPECT_EQ(s_recorded_descriptor.flags, JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_PROPERTY_OFFSET);
    EXPECT_EQ(s_recorded_descriptor.property_offset, stored.property_offset);

    constexpr u16 accessor_flags = JS_PD_PRESENT | JS_PD_HAS_GET | JS_PD_HAS_SET | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE;
    JSPropertyDescriptor accessor_descriptor { .value = 0, .get = getter, .set = nullptr, .property_offset = 0, .flags = accessor_flags };
    EXPECT_EQ(js_object_internal_define_own_property(vm, object, &accessor_key, &accessor_descriptor, nullptr).payload, 1u);
    EXPECT_EQ(s_recorded_descriptor.flags, accessor_flags);
    EXPECT_EQ(s_recorded_descriptor.get, getter);
    EXPECT(s_recorded_descriptor.set == nullptr);

    JSPropertyDescriptor empty_descriptor { .value = 0, .get = nullptr, .set = nullptr, .property_offset = 0, .flags = JS_PD_PRESENT };
    EXPECT_EQ(js_object_internal_define_own_property(vm, object, &empty_key, &empty_descriptor, nullptr).payload, 1u);
    EXPECT_EQ(s_recorded_descriptor.flags, JS_PD_PRESENT);

    JSPropertyDescriptor absent {};
    auto missing_name = "missing"_utf16_fly_string;
    JSPropertyKey missing_key { missing_name.raw_identity() };
    EXPECT_EQ(js_object_ordinary_get_own_property(vm, object, &missing_key, &absent).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(absent.flags, 0);
}

TEST_CASE(property_offsets_and_precomputed_descriptors_pass_through_hooks)
{
    TestEnvironment environment;
    auto* vm = environment.vm();
    auto* object = create_intercepting_object(environment);
    auto fresh_name = "fresh"_utf16_fly_string;
    JSPropertyKey fresh { fresh_name.raw_identity() };

    s_last_defined_descriptor.clear();
    EXPECT_EQ(js_object_create_data_property(vm, object, &fresh, int32_value(3)).payload, 1u);
    VERIFY(s_last_defined_descriptor.has_value());
    EXPECT_EQ(s_last_defined_descriptor->value, int32_value(3));
    EXPECT_EQ(s_last_defined_descriptor->flags, JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE | JS_PD_HAS_CONFIGURABLE | JS_PD_CONFIGURABLE);
    VERIFY(s_last_precomputed_get_own_property.has_value());
    EXPECT(!s_last_precomputed_get_own_property->has_value());

    // An empty precomputed [[GetOwnProperty]] result must stay distinct from none at all.
    EXPECT_EQ(environment.evaluate("host.assigned = 1"sv), "1"sv);
    VERIFY(s_last_precomputed_get_own_property.has_value());
    VERIFY(s_last_precomputed_get_own_property->has_value());
    EXPECT_EQ((*s_last_precomputed_get_own_property)->flags, 0);

    JSPropertyDescriptor precomputed_get_own_property {};
    EXPECT_EQ(js_object_internal_get_own_property(vm, object, &fresh, &precomputed_get_own_property).variant, JS_COMPLETION_NORMAL);
    EXPECT(precomputed_get_own_property.flags & JS_PD_HAS_PROPERTY_OFFSET);
    JSPropertyDescriptor redefinition { .value = int32_value(5), .get = nullptr, .set = nullptr, .property_offset = 0, .flags = JS_PD_PRESENT | JS_PD_HAS_VALUE };
    EXPECT_EQ(js_object_internal_define_own_property(vm, object, &fresh, &redefinition, &precomputed_get_own_property).payload, 1u);
    VERIFY(s_last_precomputed_get_own_property.has_value());
    VERIFY(s_last_precomputed_get_own_property->has_value());
    auto const& precomputed_seen_by_hook = **s_last_precomputed_get_own_property;
    EXPECT_EQ(precomputed_seen_by_hook.flags, precomputed_get_own_property.flags);
    EXPECT_EQ(precomputed_seen_by_hook.value, precomputed_get_own_property.value);
    EXPECT_EQ(precomputed_seen_by_hook.property_offset, precomputed_get_own_property.property_offset);

    EXPECT_EQ(js_object_get(vm, object, &fresh).payload, int32_value(5));
    EXPECT_EQ(js_object_set(vm, object, &fresh, int32_value(3), true).variant, JS_COMPLETION_NORMAL);
    JSPropertyDescriptor own_descriptor {};
    EXPECT_EQ(js_object_internal_get_own_property(vm, object, &fresh, &own_descriptor).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(own_descriptor.property_offset, precomputed_get_own_property.property_offset);
    EXPECT_EQ(own_descriptor.value, int32_value(3));
}

TEST_CASE(table_flags_become_object_flags)
{
    TestEnvironment environment;

    auto* flagged = environment.create_host_object(all_flags_class, environment.object_prototype());
    auto flags = object_flags(flagged);
    EXPECT(flags & JS_LAYOUT_OBJECT_FLAG_IS_PLATFORM_OBJECT);
    EXPECT(flags & JS_LAYOUT_OBJECT_FLAG_REQUIRES_SLOW_ADD_OWN_PROPERTY);
    EXPECT(flags & JS_LAYOUT_OBJECT_FLAG_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS);
    EXPECT(flags & JS_LAYOUT_OBJECT_FLAG_IS_HTMLDDA);
    EXPECT(flags & JS_LAYOUT_OBJECT_FLAG_IS_GLOBAL_OBJECT);
    EXPECT(!js_object_is_cacheable_for_property_absence(flagged));
    EXPECT(!js_object_eligible_for_own_property_enumeration_fast_path(flagged));

    auto* plain = environment.create_host_object(no_flags_class, environment.object_prototype());
    auto plain_flags = object_flags(plain);
    EXPECT(!(plain_flags & JS_LAYOUT_OBJECT_FLAG_IS_PLATFORM_OBJECT));
    EXPECT(!(plain_flags & JS_LAYOUT_OBJECT_FLAG_REQUIRES_SLOW_ADD_OWN_PROPERTY));
    EXPECT(!(plain_flags & JS_LAYOUT_OBJECT_FLAG_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS));
    EXPECT(!(plain_flags & JS_LAYOUT_OBJECT_FLAG_IS_HTMLDDA));
    EXPECT(!(plain_flags & JS_LAYOUT_OBJECT_FLAG_IS_GLOBAL_OBJECT));
    EXPECT(js_object_is_cacheable_for_property_absence(plain));
    EXPECT(js_object_eligible_for_own_property_enumeration_fast_path(plain));
    EXPECT(plain_flags & JS_LAYOUT_OBJECT_FLAG_IS_EXTENSIBLE);

    environment.define_global("flagged"sv, flagged);
    EXPECT_EQ(environment.evaluate("typeof flagged"sv), "undefined"sv);
    EXPECT_EQ(environment.evaluate("flagged == null"sv), "true"sv);
}

TEST_CASE(immutable_prototype_flag)
{
    TestEnvironment environment;
    environment.define_global("immutable"sv, environment.create_host_object(immutable_prototype_class, environment.object_prototype()));

    EXPECT_EQ(environment.evaluate("Reflect.setPrototypeOf(immutable, {})"sv), "false"sv);
    EXPECT_EQ(environment.evaluate("Reflect.setPrototypeOf(immutable, Object.prototype)"sv), "true"sv);
    EXPECT(environment.exception_from("Object.setPrototypeOf(immutable, null)"sv).starts_with("TypeError: "sv));
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(immutable) === Object.prototype"sv), "true"sv);
}

TEST_CASE(inherited_property_cacheability_comes_from_the_hook)
{
    TestEnvironment environment;
    auto* prototype = environment.object_of("({ inherited: 1 })"sv);
    auto* object = environment.create_host_object(inherited_cacheability_class, prototype);
    environment.define_global("inheriting"sv, object);

    s_inherited_property_is_cacheable = true;
    EXPECT(js_object_is_cacheable_for_inherited_property(object));
    EXPECT_EQ(environment.evaluate("inheriting.inherited"sv), "1"sv);
    s_inherited_property_is_cacheable = false;
    EXPECT(!js_object_is_cacheable_for_inherited_property(object));
    EXPECT_EQ(environment.evaluate("inheriting.inherited"sv), "1"sv);
    s_inherited_property_is_cacheable = true;

    EXPECT(js_object_is_cacheable_for_inherited_property(environment.create_host_object(no_flags_class, nullptr)));
}

TEST_CASE(error_data_hook)
{
    TestEnvironment environment;
    auto* error = js_error_create(environment.vm(), environment.realm(), JS_ERROR_KIND_ERROR);
    auto* object = environment.create_host_object(error_data_class, environment.object_prototype(), nullptr, error);

    EXPECT(js_error_data_of(object) != nullptr);
    EXPECT_EQ(js_error_data_of(object), js_error_data_of(error));
    EXPECT_EQ(js_error_data_of(environment.create_host_object(no_flags_class, nullptr)), nullptr);

    environment.define_global("errorish"sv, object);
    EXPECT_EQ(environment.evaluate("Object.prototype.toString.call(errorish)"sv), "[object Error]"sv);
}

static NEVER_INLINE void allocate_unreachable_host_objects(TestEnvironment& environment, size_t count)
{
    for (size_t i = 0; i < count; ++i)
        (void)environment.create_host_object(counting_finalizer_class, nullptr);
}

TEST_CASE(finalize_hook_runs_for_collected_objects)
{
    TestEnvironment environment;

    s_finalized_host_objects = 0;
    allocate_unreachable_host_objects(environment, 32);
    scrub_stack();
    js_vm_collect_garbage(environment.vm());
    EXPECT(s_finalized_host_objects > 0);
}

static NEVER_INLINE GCRoot* allocate_host_object_owning_its_cells(TestEnvironment& environment, GC::Weak<TestHostData>& wrappable, GC::Weak<TestHostData>& host_data)
{
    auto new_wrappable = GC::Heap::the().allocate<TestHostData>();
    auto new_host_data = GC::Heap::the().allocate<TestHostData>();
    wrappable = new_wrappable;
    host_data = new_host_data;
    auto* object = environment.create_host_object(no_flags_class, nullptr, new_wrappable.ptr(), new_host_data.ptr());
    return gc_root_create(reinterpret_cast<GCCell*>(object));
}

TEST_CASE(host_objects_keep_their_cells_alive)
{
    TestEnvironment environment;

    GC::Weak<TestHostData> wrappable;
    GC::Weak<TestHostData> host_data;
    auto* root = allocate_host_object_owning_its_cells(environment, wrappable, host_data);
    scrub_stack();
    js_vm_collect_garbage(environment.vm());

    auto* object = reinterpret_cast<JSObject*>(gc_root_cell(root));
    EXPECT(wrappable.ptr());
    EXPECT(host_data.ptr());
    EXPECT_EQ(js_host_object_wrappable(object), static_cast<void*>(wrappable.ptr().ptr()));
    EXPECT_EQ(js_host_object_host_data_of(object), static_cast<void*>(host_data.ptr().ptr()));
    gc_root_destroy(root);
}

TEST_CASE(host_object_layout)
{
    TestEnvironment environment;
    auto wrappable = GC::Heap::the().allocate<TestHostData>();
    auto host_data = GC::Heap::the().allocate<TestHostData>();
    auto* object = environment.create_host_object(no_flags_class, nullptr, wrappable.ptr(), host_data.ptr());

    auto const* bytes = reinterpret_cast<u8 const*>(object);
    EXPECT_EQ(*reinterpret_cast<JSHostClass const* const*>(bytes + JS_HOST_OBJECT_HOST_CLASS_OFFSET), &no_flags_class);
    EXPECT_EQ(*reinterpret_cast<GC::Cell* const*>(bytes + JS_HOST_OBJECT_WRAPPABLE_OFFSET), static_cast<GC::Cell*>(wrappable.ptr()));
    EXPECT_EQ(*reinterpret_cast<GC::Cell* const*>(bytes + JS_HOST_OBJECT_HOST_DATA_OFFSET), static_cast<GC::Cell*>(host_data.ptr()));
    EXPECT_EQ(static_cast<size_t>(JS_LAYOUT_HOST_OBJECT_SIZE), static_cast<size_t>(JS_HOST_OBJECT_SIZE));
    EXPECT_EQ(js_host_object_wrappable(object), static_cast<void*>(wrappable.ptr()));
}

TEST_CASE(host_class_identity)
{
    TestEnvironment environment;
    auto* base = environment.create_host_object(base_class, nullptr);
    auto* derived = environment.create_host_object(derived_class, nullptr);
    auto* ordinary = js_object_create(environment.vm(), environment.realm(), nullptr);

    EXPECT_EQ(class_name_of(base), "Base"sv);
    EXPECT_EQ(class_name_of(derived), "Derived"sv);
    EXPECT_EQ(js_host_object_host_class_of(derived), &derived_class);
    EXPECT_EQ(js_host_object_host_class_of(ordinary), nullptr);

    EXPECT(js_host_object_is_host_instance_of(derived, &derived_class));
    EXPECT(js_host_object_is_host_instance_of(derived, &base_class));
    EXPECT(!js_host_object_is_host_instance_of(base, &derived_class));
    EXPECT(!js_host_object_is_host_instance_of(ordinary, &base_class));

    EXPECT_EQ(js_object_class_id(derived), JS_LAYOUT_CLASS_ID_HOST_OBJECT);
    EXPECT(js_object_is_subclass_of(derived, JS_LAYOUT_CLASS_ID_HOST_OBJECT));
    EXPECT(!js_object_is_subclass_of(ordinary, JS_LAYOUT_CLASS_ID_HOST_OBJECT));
}

TEST_CASE(host_data_is_identified_by_its_type)
{
    TestEnvironment environment;
    auto host_data = GC::Heap::the().allocate<TestHostData>();
    auto* object = environment.create_host_object(no_flags_class, nullptr, nullptr, host_data.ptr());

    EXPECT_EQ(host_data_if<TestHostData>(object), host_data.ptr());
    EXPECT_EQ(host_data_if<OtherTestHostData>(object), nullptr);
    EXPECT_EQ(host_data_if<TestHostData>(environment.create_host_object(no_flags_class, nullptr)), nullptr);
    EXPECT_EQ(host_data_if<TestHostData>(js_object_create(environment.vm(), environment.realm(), nullptr)), nullptr);

    auto other_host_data = GC::Heap::the().allocate<OtherTestHostData>();
    js_host_object_set_host_data(object, other_host_data.ptr());
    EXPECT_EQ(host_data_if<TestHostData>(object), nullptr);
    EXPECT_EQ(host_data_if<OtherTestHostData>(object), other_host_data.ptr());
}

TEST_CASE(each_host_class_has_its_own_allocator)
{
    TestEnvironment environment;
    auto* first_a = environment.create_host_object(allocator_a_class, nullptr);
    auto* second_a = environment.create_host_object(allocator_a_class, nullptr);
    auto* b = environment.create_host_object(allocator_b_class, nullptr);
    auto* ordinary = js_object_create(environment.vm(), environment.realm(), nullptr);

    EXPECT_EQ(&allocator_of(first_a), &allocator_of(second_a));
    EXPECT_NE(&allocator_of(first_a), &allocator_of(b));
    EXPECT_NE(&allocator_of(first_a), &allocator_of(ordinary));
    EXPECT_EQ(allocator_of(first_a).class_name(), "AllocatorA"sv);
    EXPECT_EQ(allocator_of(b).class_name(), "AllocatorB"sv);
}

TEST_CASE(host_classes_can_share_their_parents_allocator)
{
    TestEnvironment environment;
    auto* a = environment.create_host_object(allocator_a_class, nullptr);
    auto* sharing_child = environment.create_host_object(allocator_a_sharing_child_class, nullptr);
    auto* sharing_grandchild = environment.create_host_object(allocator_a_sharing_grandchild_class, nullptr);
    auto* isolated_child = environment.create_host_object(allocator_a_isolated_child_class, nullptr);

    EXPECT_EQ(&allocator_of(sharing_child), &allocator_of(a));
    EXPECT_EQ(&allocator_of(sharing_grandchild), &allocator_of(a));
    EXPECT_NE(&allocator_of(isolated_child), &allocator_of(a));
    EXPECT_EQ(allocator_of(isolated_child).class_name(), "AllocatorAIsolatedChild"sv);

    EXPECT_EQ(js_host_object_host_class_of(sharing_grandchild), &allocator_a_sharing_grandchild_class);
    EXPECT_EQ(class_name_of(sharing_grandchild), "AllocatorASharingGrandchild"sv);
}

TEST_CASE(get_as_prototype_of_passes_on_only_cacheable_prototype_chain_hits)
{
    TestEnvironment environment;
    auto* vm = environment.vm();
    auto own_name = "own"_utf16_fly_string;
    auto inherited_name = "inherited"_utf16_fly_string;
    auto missing_name = "missing"_utf16_fly_string;
    JSPropertyKey own { own_name.raw_identity() };
    JSPropertyKey inherited { inherited_name.raw_identity() };
    JSPropertyKey missing { missing_name.raw_identity() };

    auto* holder = environment.object_of("Object.create({ inherited: 1 }, { own: { value: 2, writable: true, enumerable: true, configurable: true } })"sv);
    js_object_convert_to_prototype_if_needed(vm, holder);
    auto receiver = value_of_object(js_object_create(vm, environment.realm(), nullptr));
    EXPECT_EQ(js_object_internal_get_as_prototype_of(vm, holder, &own, receiver, nullptr).payload, int32_value(2));
    EXPECT_EQ(js_object_internal_get_as_prototype_of(vm, holder, &inherited, receiver, nullptr).payload, int32_value(1));
    EXPECT_EQ(js_object_internal_get_as_prototype_of(vm, holder, &missing, receiver, nullptr).payload, js_undefined);

    // A hook that forwards to the holder that way, with the metadata of the caller, gets its lookups cached, so that
    // bytecode stops calling it once the lookup is cached. Without the metadata, every lookup calls it.
    auto* forwarding = environment.create_host_object(forwarding_class, environment.object_prototype(), nullptr, holder);
    environment.define_global("forwarding"sv, forwarding);
    constexpr auto sum_of_ten_lookups = "(() => { let sum = 0; for (let i = 0; i < 10; ++i) sum += forwarding.own; return sum; })()"sv;

    s_forwarded_lookups = 0;
    s_forwarding_passes_metadata_on = true;
    EXPECT_EQ(environment.evaluate(sum_of_ten_lookups), "20"sv);
    EXPECT_EQ(s_forwarded_lookups, 1u);

    s_forwarded_lookups = 0;
    s_forwarding_passes_metadata_on = false;
    EXPECT_EQ(environment.evaluate(sum_of_ten_lookups), "20"sv);
    EXPECT_EQ(s_forwarded_lookups, 10u);
    s_forwarding_passes_metadata_on = true;

    // A value that a host hook produced without filling the metadata is not cached either.
    auto* host = create_intercepting_object(environment);
    auto answer_name = "answer"_utf16_fly_string;
    JSPropertyKey answer { answer_name.raw_identity() };
    EXPECT_EQ(js_object_internal_get_as_prototype_of(vm, host, &answer, receiver, nullptr).payload, int32_value(42));
}

namespace {

size_t s_finalized_host_functions = 0;

// Sums its arguments, and throws when there are none.
JSCompletion adder_call(JSObject*, JSVM* vm)
{
    reenter("call"sv);
    if (argument_count(vm) == 0)
        return hook_error("call"sv);
    double sum = 0;
    for (size_t i = 0; i < argument_count(vm); ++i) {
        double number = 0;
        auto completion = js_value_to_double(vm, argument(vm, i), &number);
        if (completion.variant != JS_COMPLETION_NORMAL)
            return completion;
        sum += number;
    }
    return normal_completion(number_value(sum));
}

void count_finalized_host_function(JSObject*)
{
    ++s_finalized_host_functions;
}

JSCompletion throwing_call(JSObject*, JSVM*)
{
    return hook_error("call"sv);
}

// Makes { value } objects from new.target's prototype, and throws without an argument.
JSCompletion maker_construct(JSObject*, JSVM* vm, JSObject* new_target)
{
    reenter("construct"sv);
    if (argument_count(vm) == 0)
        return hook_error("construct"sv);
    auto prototype_name = "prototype"_utf16_fly_string;
    JSPropertyKey prototype_key { prototype_name.raw_identity() };
    auto prototype = js_object_get(vm, new_target, &prototype_key);
    if (prototype.variant != JS_COMPLETION_NORMAL)
        return prototype;
    auto* prototype_object = object_of_value(prototype.payload);
    if (!prototype_object)
        prototype_object = js_realm_intrinsic(vm, s_embedded_vm->realm(), JS_INTRINSIC_OBJECT_PROTOTYPE);
    auto* object = js_object_create(vm, s_embedded_vm->realm(), prototype_object);
    auto value_name = "value"_utf16_fly_string;
    JSPropertyKey value_key { value_name.raw_identity() };
    js_object_define_direct_property(vm, object, &value_key, argument(vm, 0), JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE);
    return normal_completion(reinterpret_cast<uintptr_t>(object));
}

constexpr JSHostFunctionHooks adder_hooks { .call = adder_call, .construct = nullptr, .finalize = count_finalized_host_function };
constexpr JSHostClass adder_class = make_host_class(JS_HOST_CLASS_FUNCTION, "Adder"sv, nullptr, &adder_hooks, 0);
constexpr JSHostFunctionHooks maker_hooks { .call = throwing_call, .construct = maker_construct, .finalize = nullptr };
constexpr JSHostClass maker_class = make_host_class(JS_HOST_CLASS_FUNCTION, "Maker"sv, nullptr, &maker_hooks, JS_HOST_CLASS_HAS_CONSTRUCTOR);

Optional<JSValue> s_last_deleted_element;

// Doubles numbers stored at indices and rejects negative ones, then lets the array store them.
JSCompletion doubling_array_set(JSObject* array, JSPropertyKey key, JSValue value, JSValue receiver, JSSetCacheMetadata* metadata, u8 phase)
{
    reenter("array set"sv);
    if (is_index_key(key) && is_number(value)) {
        if (as_double(value) < 0)
            return hook_error("set"sv);
        value = number_value(as_double(value) * 2);
    }
    return js_host_array_array_set(hook_vm(), array, &key, value, receiver, metadata, phase);
}

JSCompletion recording_array_delete(JSObject* array, JSPropertyKey key)
{
    reenter("array delete"sv);
    if (key_is(key, "throwing"sv))
        return hook_error("delete_property"sv);
    if (is_index_key(key)) {
        JSValue element = 0;
        if (js_array_indexed_get(array, index_of_key(key), &element, nullptr))
            s_last_deleted_element = element;
    }
    return js_host_array_array_delete(hook_vm(), array, &key);
}

constexpr JSHostArrayHooks doubling_array_hooks { .set = doubling_array_set, .delete_property = recording_array_delete };
constexpr JSHostClass doubling_array_class = make_host_class(JS_HOST_CLASS_ARRAY, "DoublingArray"sv, nullptr, &doubling_array_hooks, JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS);
constexpr JSHostClass hookless_array_class = make_host_class(JS_HOST_CLASS_ARRAY, "HooklessArray"sv, nullptr, nullptr, 0);

JSObject* create_host_function(TestEnvironment& environment, JSHostClass const& host_class, StringView name, i32 length, void* host_data = nullptr)
{
    return js_host_function_create(environment.vm(), environment.realm(), &host_class, ascii_view(name), length, nullptr, host_data);
}

}

TEST_CASE(host_function_calls)
{
    TestEnvironment environment;
    auto host_data = GC::Heap::the().allocate<TestHostData>();
    auto* adder = create_host_function(environment, adder_class, "add"sv, 2, host_data.ptr());
    environment.define_global("add"sv, adder);

    EXPECT_EQ(environment.evaluate("add(1, 2, 3)"sv), "6"sv);
    EXPECT_EQ(environment.evaluate("typeof add"sv), "function"sv);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(add).join()"sv), "length,name"sv);
    EXPECT_EQ(environment.evaluate("add.name + add.length"sv), "add2"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(add) === Function.prototype"sv), "true"sv);
    EXPECT_EQ(environment.exception_from("add()"sv), "TypeError: call threw"sv);
    EXPECT(environment.exception_from("new add(1)"sv).starts_with("TypeError: "sv));

    EXPECT_EQ(class_name_of(adder), "Adder"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_function_name_for_call_stack(adder)), u"add"sv);
    EXPECT(!js_value_is_constructor(value_of_object(adder)));
    EXPECT_EQ(js_function_realm(adder), environment.realm());
    EXPECT_EQ(js_host_object_host_class_of(adder), &adder_class);
    EXPECT(js_object_is_subclass_of(adder, JS_LAYOUT_CLASS_ID_HOST_FUNCTION));
    EXPECT(!js_object_is_subclass_of(adder, JS_LAYOUT_CLASS_ID_HOST_OBJECT));
    EXPECT_EQ(host_data_if<TestHostData>(adder), host_data.ptr());
}

TEST_CASE(host_function_constructs)
{
    TestEnvironment environment;
    auto* vm = environment.vm();
    auto* maker = create_host_function(environment, maker_class, "Maker"sv, 1);
    auto prototype_name = "prototype"_utf16_fly_string;
    JSPropertyKey prototype_key { prototype_name.raw_identity() };
    js_object_define_direct_property(vm, maker, &prototype_key, value_of_object(js_object_create(vm, environment.realm(), environment.object_prototype())), 0);
    environment.define_global("Maker"sv, maker);

    EXPECT(js_value_is_constructor(value_of_object(maker)));
    EXPECT_EQ(environment.evaluate("const made = new Maker(5); made.value + ' ' + (made instanceof Maker)"sv), "5 true"sv);
    EXPECT_EQ(environment.evaluate("class Derived extends Maker {} const derived = new Derived(3); derived.value + ' ' + (derived instanceof Derived)"sv), "3 true"sv);
    EXPECT_EQ(environment.exception_from("new Maker()"sv), "TypeError: construct threw"sv);
    EXPECT_EQ(environment.exception_from("Maker(1)"sv), "TypeError: call threw"sv);
}

TEST_CASE(host_function_without_own_properties)
{
    TestEnvironment environment;
    auto* vm = environment.vm();
    auto* error_constructor = js_realm_intrinsic(vm, environment.realm(), JS_INTRINSIC_ERROR_CONSTRUCTOR);
    auto* function = js_host_function_create_without_own_properties(vm, environment.realm(), &adder_class, ascii_view("bare"sv), error_constructor, nullptr);
    environment.define_global("bare"sv, function);

    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(bare).length"sv), "0"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(bare) === Error"sv), "true"sv);

    auto prototype_name = "prototype"_utf16_fly_string;
    auto name_name = "name"_utf16_fly_string;
    auto length_name = "length"_utf16_fly_string;
    JSPropertyKey prototype_key { prototype_name.raw_identity() };
    JSPropertyKey name_key { name_name.raw_identity() };
    JSPropertyKey length_key { length_name.raw_identity() };
    js_object_define_direct_property(vm, function, &prototype_key, value_of_object(js_object_create(vm, environment.realm(), nullptr)), 0);
    js_object_define_direct_property(vm, function, &name_key, string_value("bare"sv), JS_ATTRIBUTE_CONFIGURABLE);
    js_object_define_direct_property(vm, function, &length_key, int32_value(1), JS_ATTRIBUTE_CONFIGURABLE);
    EXPECT_EQ(environment.evaluate("Object.getOwnPropertyNames(bare).join()"sv), "prototype,name,length"sv);
}

static NEVER_INLINE void allocate_unreachable_host_functions(TestEnvironment& environment, size_t count)
{
    for (size_t i = 0; i < count; ++i)
        (void)create_host_function(environment, adder_class, "add"sv, 2);
}

TEST_CASE(host_function_finalize_hook)
{
    TestEnvironment environment;

    s_finalized_host_functions = 0;
    allocate_unreachable_host_functions(environment, 32);
    scrub_stack();
    js_vm_collect_garbage(environment.vm());
    EXPECT(s_finalized_host_functions > 0);
}

TEST_CASE(host_array_hooks)
{
    TestEnvironment environment;
    auto host_data = GC::Heap::the().allocate<TestHostData>();
    auto* array = js_host_array_create(environment.vm(), environment.realm(), &doubling_array_class, nullptr, host_data.ptr());
    environment.define_global("doubling"sv, array);

    EXPECT_EQ(environment.evaluate("doubling[0] = 2; doubling[1] = 5; doubling.push(7); doubling.join()"sv), "4,10,14"sv);
    EXPECT_EQ(environment.evaluate("doubling.length + ' ' + Array.isArray(doubling)"sv), "3 true"sv);
    EXPECT_EQ(environment.evaluate("Object.getPrototypeOf(doubling) === Array.prototype"sv), "true"sv);

    s_last_deleted_element.clear();
    EXPECT_EQ(environment.evaluate("delete doubling[1]"sv), "true"sv);
    VERIFY(s_last_deleted_element.has_value());
    EXPECT_EQ(as_double(*s_last_deleted_element), 10.0);
    EXPECT_EQ(environment.evaluate("doubling.length + ' ' + (1 in doubling)"sv), "3 false"sv);

    EXPECT_EQ(environment.exception_from("doubling[0] = -1"sv), "TypeError: set threw"sv);
    EXPECT_EQ(environment.exception_from("delete doubling.throwing"sv), "TypeError: delete_property threw"sv);
    EXPECT_EQ(environment.evaluate("doubling[0]"sv), "4"sv);

    EXPECT(object_flags(array) & JS_LAYOUT_OBJECT_FLAG_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS);
    EXPECT_EQ(class_name_of(array), "DoublingArray"sv);
    EXPECT_EQ(js_host_object_host_class_of(array), &doubling_array_class);
    EXPECT(js_object_is_subclass_of(array, JS_LAYOUT_CLASS_ID_HOST_ARRAY));
    EXPECT(js_object_is_subclass_of(array, JS_LAYOUT_CLASS_ID_ARRAY));
    EXPECT_EQ(host_data_if<TestHostData>(array), host_data.ptr());
}

TEST_CASE(host_array_without_hooks_is_an_array)
{
    TestEnvironment environment;
    auto* array = js_host_array_create(environment.vm(), environment.realm(), &hookless_array_class, nullptr, nullptr);
    environment.define_global("hookless"sv, array);

    EXPECT_EQ(environment.evaluate("hookless.push(1, 2); hookless[5] = 3; delete hookless[0]; hookless.length + ' ' + JSON.stringify(hookless)"sv), "6 [null,2,null,null,null,3]"sv);
    EXPECT(!(object_flags(array) & JS_LAYOUT_OBJECT_FLAG_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS));
}

TEST_CASE(every_hook_may_reenter_the_vm)
{
    TestEnvironment environment;
    auto* host = create_intercepting_object(environment);
    environment.define_global("inheriting"sv, environment.create_host_object(inherited_cacheability_class, host));
    auto* error = js_error_create(environment.vm(), environment.realm(), JS_ERROR_KIND_ERROR);
    environment.define_global("errorish"sv, environment.create_host_object(error_data_class, nullptr, nullptr, error));
    environment.define_global("add"sv, create_host_function(environment, adder_class, "add"sv, 2));
    environment.define_global("Maker"sv, create_host_function(environment, maker_class, "Maker"sv, 1));
    environment.define_global("doubling"sv, js_host_array_create(environment.vm(), environment.realm(), &doubling_array_class, nullptr, nullptr));

    // Each hook runs a script that reaches the hooks again and makes garbage, then collects it, before it answers.
    s_hooks_that_reentered.clear();
    s_script_for_hooks_to_reenter_with = "globalThis.reentered = (globalThis.reentered | 0) + 1; host.answer + Object.keys(host).length + [1, 2, 3].map(String).join()"sv;
    auto result = environment.evaluate(
        "Object.getPrototypeOf(host); Reflect.setPrototypeOf(host, Object.prototype); Reflect.isExtensible(host); "
        "Reflect.preventExtensions(host); Object.getOwnPropertyDescriptor(host, 'x'); Object.defineProperty(host, 'x', { value: 1, configurable: true }); "
        "'x' in host; host.x; host.y = 2; delete host.y; Reflect.ownKeys(host); host.toString; inheriting.toString; "
        "Object.prototype.toString.call(errorish) + ' ' + add(1, 2) + ' ' + new Maker(3).value + ' ' + (doubling[0] = 1) + ' ' + delete doubling[0]"sv);
    s_script_for_hooks_to_reenter_with.clear();
    EXPECT_EQ(result, "[object Error] 3 3 1 true"sv);

    quick_sort(s_hooks_that_reentered);
    Vector<StringView> expected_hooks {
        "array delete"sv,
        "array set"sv,
        "call"sv,
        "construct"sv,
        "define_own_property"sv,
        "delete_property"sv,
        "error_data"sv,
        "get"sv,
        "get_own_property"sv,
        "get_prototype_of"sv,
        "has_property"sv,
        "is_cacheable_for_inherited_property"sv,
        "is_extensible"sv,
        "own_property_keys"sv,
        "prevent_extensions"sv,
        "set"sv,
        "set_prototype_of"sv,
    };
    EXPECT_EQ(s_hooks_that_reentered, expected_hooks);
    EXPECT_EQ(environment.evaluate("host.x"sv), "1"sv);
}
