/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StdLibExtras.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Function.h>
#include <LibGC/Heap.h>
#include <LibJS/HostObjectABI.h>

#include "EmbeddingTest.h"

static constexpr u8 all_attributes = JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE;

// A borrowed key for a name that is not an array index: the raw word of its fly string, which must outlive the key.
static JSPropertyKey key_of(Utf16FlyString const& name)
{
    return { name.raw_identity() };
}

template<typename T>
static T* field_at(void const* base, size_t offset)
{
    return *reinterpret_cast<T* const*>(static_cast<u8 const*>(base) + offset);
}

// What a facade's VM::argument() reads: the arguments are the last slots of the running execution context, which
// follow its fixed fields.
static JSValue argument(JSVM* vm, size_t index)
{
    auto const* context = field_at<u8 const>(vm, JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET);
    auto slot_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET);
    auto argument_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
    if (index >= argument_count)
        return js_undefined;
    auto const* slots = reinterpret_cast<JSValue const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_SIZE);
    return slots[slot_count - argument_count + index];
}

static bool evaluates_to_true(EmbeddedVM& embedded_vm, StringView source)
{
    auto completion = embedded_vm.evaluate(source);
    return completion.variant == JS_COMPLETION_NORMAL && completion.payload == js_true;
}

static void define_global(EmbeddedVM& embedded_vm, Utf16FlyString const& name, JSValue value)
{
    auto key = key_of(name);
    js_object_define_direct_property(embedded_vm.vm(), embedded_vm.global_object(), &key, value, all_attributes);
}

// Native functions are written for whichever way the target returns a C++ ThrowCompletionOr<Value>.
template<JSCompletion (*behaviour)(JSVM*)>
static JSNativeFunction native_function()
{
    if constexpr (IsSame<JSNativeFunction, JSCompletion (*)(JSVM*)>)
        return behaviour;
    else
        return [](JSCompletion* result, JSVM* vm) { *result = behaviour(vm); };
}

static JSCompletion add_two_int32_arguments(JSVM* vm)
{
    auto as_int32 = [](JSValue value) { return static_cast<i32>(static_cast<u32>(value)); };
    return { int32_value(as_int32(argument(vm, 0)) + as_int32(argument(vm, 1))), JS_COMPLETION_NORMAL };
}

static JSCompletion throw_first_argument(JSVM* vm)
{
    return { argument(vm, 0), JS_COMPLETION_THROW };
}

static JSCompletion return_forty_two(JSVM*)
{
    return { int32_value(42), JS_COMPLETION_NORMAL };
}

TEST_CASE(property_operations_from_cpp)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto* object_prototype = embedded_vm->object_of("Object.prototype"sv);
    auto* object = js_object_create(vm, embedded_vm->realm(), object_prototype);
    EXPECT_EQ(js_object_prototype(object), object_prototype);

    auto answer_name = "answer"_utf16_fly_string;
    auto created_name = "created"_utf16_fly_string;
    auto to_string_name = "toString"_utf16_fly_string;
    auto answer = key_of(answer_name);
    auto created = key_of(created_name);
    auto to_string = key_of(to_string_name);

    js_object_define_direct_property(vm, object, &answer, int32_value(42), all_attributes);
    EXPECT_EQ(js_object_get(vm, object, &answer).payload, int32_value(42));
    EXPECT_EQ(js_object_set(vm, object, &answer, int32_value(43), true).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_object_get(vm, object, &answer).payload, int32_value(43));
    EXPECT_EQ(js_object_has_property(vm, object, &to_string).payload, 1u);
    EXPECT_EQ(js_object_has_own_property(vm, object, &to_string).payload, 0u);
    EXPECT(js_object_storage_has(object, &answer));

    auto creation = js_object_create_data_property_or_throw(vm, object, &created, int32_value(1));
    EXPECT_EQ(creation.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(creation.payload, 1u);
    define_global(*embedded_vm, "object"_utf16_fly_string, value_of_object(object));
    EXPECT(evaluates_to_true(*embedded_vm, "object.answer === 43 && object.created === 1 && Object.keys(object).join() === 'answer,created'"sv));

    EXPECT_EQ(js_object_delete_property_or_throw(vm, object, &created).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_object_has_own_property(vm, object, &created).payload, 0u);

    auto* array = js_array_create_from(vm, embedded_vm->realm(), nullptr, 0);
    EXPECT_EQ(js_object_class_id(array), JS_LAYOUT_CLASS_ID_ARRAY);
    EXPECT(js_object_is_subclass_of(array, JS_LAYOUT_CLASS_ID_OBJECT));
    EXPECT(!js_object_is_subclass_of(object, JS_LAYOUT_CLASS_ID_ARRAY));
}

TEST_CASE(cpp_raw_natives_are_called_from_javascript)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto add_name = "add"_utf16_fly_string;
    auto thrower_name = "thrower"_utf16_fly_string;
    auto computed_name = "computed"_utf16_fly_string;
    auto add = key_of(add_name);
    auto thrower = key_of(thrower_name);
    auto computed = key_of(computed_name);

    js_object_define_native_function(vm, embedded_vm->global_object(), embedded_vm->realm(), &add, native_function<add_two_int32_arguments>(), 2, all_attributes);
    auto* thrower_function = js_function_create_native(vm, native_function<throw_first_argument>(), 1, &thrower, nullptr, nullptr, 0);
    define_global(*embedded_vm, thrower_name, value_of_object(thrower_function));
    js_object_define_native_accessor(vm, embedded_vm->global_object(), embedded_vm->realm(), &computed, native_function<return_forty_two>(), nullptr, JS_ATTRIBUTE_CONFIGURABLE);

    EXPECT(evaluates_to_true(*embedded_vm, "add(2, 3) === 5 && add.length === 2 && add.name === 'add'"sv));
    EXPECT(evaluates_to_true(*embedded_vm, "try { thrower(7); false } catch (e) { e === 7 }"sv));
    EXPECT(evaluates_to_true(*embedded_vm, "computed === 42 && Object.getOwnPropertyDescriptor(globalThis, 'computed').get.name === 'get computed'"sv));
}

using ClosureFunction = GC::Function<JSCompletion(JSVM*)>;

// The facade's thunk for every closure: the context is the GC::Function that wraps the AK::Function.
static JSCompletion call_closure_function(void* context, JSVM* vm)
{
    return static_cast<ClosureFunction*>(context)->function()(vm);
}

TEST_CASE(cpp_closures_are_called_from_javascript_and_can_call_back)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto base_name = "base"_utf16_fly_string;
    auto closure_name = "closure"_utf16_fly_string;
    define_global(*embedded_vm, base_name, int32_value(100));

    size_t calls = 0;
    auto* global_object = embedded_vm->global_object();
    auto function = GC::create_function(GC::Heap::the(), [&calls, global_object, &base_name](JSVM* vm) -> JSCompletion {
        ++calls;
        // The closure calls back into the VM while the VM runs it.
        auto base = key_of(base_name);
        auto base_value = js_object_get(vm, global_object, &base);
        if (base_value.variant != JS_COMPLETION_NORMAL)
            return base_value;
        auto sum = static_cast<i32>(static_cast<u32>(base_value.payload)) + static_cast<i32>(static_cast<u32>(argument(vm, 0)));
        return { int32_value(sum), JS_COMPLETION_NORMAL };
    });
    auto closure_key = key_of(closure_name);
    auto* closure = js_function_create_closure(vm, call_closure_function, function.ptr(), 1, &closure_key, nullptr, nullptr, 0);
    define_global(*embedded_vm, closure_name, value_of_object(closure));

    EXPECT(evaluates_to_true(*embedded_vm, "closure(1) + closure(2) === 203 && closure.name === 'closure' && closure.length === 1"sv));
    EXPECT_EQ(calls, 2u);
}

static constexpr size_t closure_count = 64;
static bool s_closure_context_destroyed[closure_count];

class DestructionRecorder {
    AK_MAKE_NONCOPYABLE(DestructionRecorder);

public:
    explicit DestructionRecorder(size_t index)
        : m_index(index)
    {
    }

    DestructionRecorder(DestructionRecorder&& other)
        : m_index(exchange(other.m_index, NumericLimits<size_t>::max()))
    {
    }

    ~DestructionRecorder()
    {
        if (m_index != NumericLimits<size_t>::max())
            s_closure_context_destroyed[m_index] = true;
    }

private:
    size_t m_index;
};

// Every even closure is reachable from the global object; nothing holds the odd ones.
static NEVER_INLINE void create_closures_whose_contexts_record_their_destruction(EmbeddedVM& embedded_vm)
{
    auto* vm = embedded_vm.vm();
    auto* holder = js_array_create_from(vm, embedded_vm.realm(), nullptr, 0);
    define_global(embedded_vm, "holder"_utf16_fly_string, value_of_object(holder));
    for (size_t index = 0; index < closure_count; ++index) {
        auto function = GC::create_function(GC::Heap::the(), [recorder = DestructionRecorder { index }, index](JSVM*) -> JSCompletion {
            (void)recorder;
            return { int32_value(static_cast<i32>(index)), JS_COMPLETION_NORMAL };
        });
        auto* closure = js_function_create_closure_with_name(vm, embedded_vm.realm(), ascii_view("recorder"sv), call_closure_function, function.ptr());
        if (index % 2 == 0)
            js_array_indexed_append(holder, value_of_object(closure));
    }
}

TEST_CASE(a_closure_keeps_its_cpp_context_alive)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    for (auto& destroyed : s_closure_context_destroyed)
        destroyed = false;
    create_closures_whose_contexts_record_their_destruction(*embedded_vm);
    embedded_vm->collect_garbage();

    size_t destroyed_held_contexts = 0;
    size_t destroyed_unheld_contexts = 0;
    for (size_t index = 0; index < closure_count; ++index) {
        if (s_closure_context_destroyed[index])
            ++(index % 2 == 0 ? destroyed_held_contexts : destroyed_unheld_contexts);
    }
    EXPECT_EQ(destroyed_held_contexts, 0u);
    EXPECT(destroyed_unheld_contexts >= closure_count / 4);
    EXPECT(evaluates_to_true(*embedded_vm, "holder[0]() === 0 && holder[31]() === 62 && holder.length === 32"sv));
}

TEST_CASE(descriptors_round_trip_with_their_property_offsets)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto* object = js_object_create(vm, embedded_vm->realm(), nullptr);
    auto name = "x"_utf16_fly_string;
    auto key = key_of(name);

    JSPropertyDescriptor descriptor {};
    descriptor.value = int32_value(7);
    descriptor.flags = JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE | JS_PD_HAS_CONFIGURABLE;
    auto definition = js_object_internal_define_own_property(vm, object, &key, &descriptor, nullptr);
    EXPECT_EQ(definition.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(definition.payload, 1u);
    EXPECT(descriptor.flags & JS_PD_HAS_PROPERTY_OFFSET);

    JSPropertyDescriptor own_property {};
    EXPECT_EQ(js_object_internal_get_own_property(vm, object, &key, &own_property).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(own_property.value, int32_value(7));
    EXPECT_EQ(own_property.flags & ~JS_PD_HAS_PROPERTY_OFFSET, descriptor.flags & ~JS_PD_HAS_PROPERTY_OFFSET);
    EXPECT(own_property.flags & JS_PD_HAS_PROPERTY_OFFSET);
    EXPECT_EQ(own_property.property_offset, descriptor.property_offset);

    // The ordinary [[DefineOwnProperty]] rejects changing a non-configurable, non-writable property, without throwing.
    JSPropertyDescriptor change {};
    change.value = int32_value(8);
    change.flags = JS_PD_PRESENT | JS_PD_HAS_VALUE;
    auto rejected = js_object_ordinary_define_own_property(vm, object, &key, &change, &own_property);
    EXPECT_EQ(rejected.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(rejected.payload, 0u);

    auto missing_name = "missing"_utf16_fly_string;
    auto missing = key_of(missing_name);
    JSPropertyDescriptor absent {};
    absent.flags = JS_PD_PRESENT;
    js_object_ordinary_get_own_property(vm, object, &missing, &absent);
    EXPECT_EQ(absent.flags, 0);

    auto* accessor_holder = embedded_vm->object_of("({ get value() { return 1; } })"sv);
    auto value_name = "value"_utf16_fly_string;
    auto value = key_of(value_name);
    JSPropertyDescriptor accessor {};
    js_object_internal_get_own_property(vm, accessor_holder, &value, &accessor);
    EXPECT(accessor.flags & JS_PD_HAS_GET);
    EXPECT(accessor.get != nullptr);
    EXPECT(accessor.flags & JS_PD_HAS_SET);
    EXPECT(accessor.set == nullptr);
    EXPECT(!(accessor.flags & JS_PD_HAS_VALUE));
}

static void append_to_vector(void* context, JSValue value)
{
    static_cast<Vector<JSValue>*>(context)->append(value);
}

TEST_CASE(own_keys_reach_a_value_sink)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto* object = embedded_vm->object_of("({ b: 1, a: 2, 1: 3, [Symbol.iterator]: 4 })"sv);
    Vector<JSValue> keys;
    JSValueSink sink { &keys, append_to_vector };
    EXPECT_EQ(js_object_internal_own_property_keys(vm, object, &sink).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(keys.size(), 4u);

    auto* keys_array = js_array_create_from(vm, embedded_vm->realm(), keys.data(), keys.size());
    define_global(*embedded_vm, "keys"_utf16_fly_string, value_of_object(keys_array));
    EXPECT(evaluates_to_true(*embedded_vm, "keys.slice(0, 3).join() === '1,b,a' && keys[3] === Symbol.iterator"sv));
}

TEST_CASE(integrity_levels)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto* object = embedded_vm->object_of("({ x: 1 })"sv);
    auto name = "x"_utf16_fly_string;
    auto key = key_of(name);

    EXPECT_EQ(js_object_test_integrity_level(vm, object, JS_INTEGRITY_LEVEL_SEALED).payload, 0u);
    EXPECT_EQ(js_object_set_integrity_level(vm, object, JS_INTEGRITY_LEVEL_FROZEN).payload, 1u);
    EXPECT_EQ(js_object_test_integrity_level(vm, object, JS_INTEGRITY_LEVEL_SEALED).payload, 1u);
    EXPECT_EQ(js_object_test_integrity_level(vm, object, JS_INTEGRITY_LEVEL_FROZEN).payload, 1u);
    EXPECT_EQ(js_object_is_extensible(vm, object).payload, 0u);

    EXPECT_EQ(js_object_set(vm, object, &key, int32_value(2), false).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_object_set(vm, object, &key, int32_value(2), true).variant, JS_COMPLETION_THROW);
    EXPECT_EQ(js_object_get(vm, object, &key).payload, int32_value(1));
}

TEST_CASE(dynamic_functions_close_over_an_object_environment)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto source_text = u"function onclick(event) {\nreturn event + scoped\n}"sv;
    auto body = u"\nreturn event + scoped\n"sv;
    JSOwnedUtf16String error_message = 0;
    auto* function_data = js_function_compile_dynamic(vm, abi_view_of(source_text), ascii_view("event"sv), abi_view_of(body), JS_FUNCTION_KIND_NORMAL, &error_message);
    EXPECT(function_data != nullptr);

    auto* scope_object = embedded_vm->object_of("({ scoped: 40 })"sv);
    auto* scope = js_environment_new_object_environment(vm, scope_object, true, embedded_vm->global_environment());
    EXPECT_EQ(js_environment_kind(scope), JS_ENVIRONMENT_KIND_OBJECT);
    EXPECT_EQ(js_environment_outer(scope), embedded_vm->global_environment());

    auto* function = js_function_instantiate_dynamic(vm, embedded_vm->realm(), function_data, scope, nullptr, { .tag = JS_LAYOUT_SCRIPT_OR_MODULE_TAG_EMPTY, .cell = nullptr });
    JSValue arguments[] = { int32_value(2) };
    auto result = js_function_call(vm, value_of_object(function), js_undefined, arguments, 1);
    EXPECT_EQ(result.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(result.payload, int32_value(42));
    define_global(*embedded_vm, "handler"_utf16_fly_string, value_of_object(function));
    EXPECT(evaluates_to_true(*embedded_vm, "handler.name === 'onclick' && handler.length === 1 && String(handler).startsWith('function onclick(event)')"sv));

    auto broken_source = u"function f() {\n}}\n}"sv;
    auto* broken = js_function_compile_dynamic(vm, abi_view_of(broken_source), ascii_view(""sv), ascii_view("\n}}\n"sv), JS_FUNCTION_KIND_NORMAL, &error_message);
    EXPECT(broken == nullptr);
    auto message = Utf16String::adopt_raw(error_message);
    EXPECT(message.contains(u"(line: "sv));
}
