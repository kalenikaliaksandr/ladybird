/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Vector.h>
#include <LibJS/HostObjectABI.h>

#include "EmbeddingTest.h"

// The NaN-boxed encoding that JSValue shares with JS::Value.
static constexpr JSValue js_true = (0x7FF9ull << 48) | 1;

static constexpr JSValue int32_value(i32 value)
{
    return (0x7FFAull << 48) | static_cast<u32>(value);
}

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

// A VM whose heap is the process default, so that the test can allocate C++ GC cells in it, with a realm.
class Harness {
public:
    Harness()
        : m_embedded_vm(EmbeddedVM::create({ .become_process_default_heap = true, .shared_memory_shared_array_buffers = false }))
    {
        m_embedded_vm->initialize_realm();
    }

    JSVM* vm() const { return m_embedded_vm->vm(); }
    JSRealm* realm() const { return m_embedded_vm->realm(); }
    JSObject* global_object() const { return field_at<JSObject>(realm(), JS_LAYOUT_REALM_GLOBAL_OBJECT_OFFSET); }

    JSCompletion evaluate(StringView source) const { return m_embedded_vm->evaluate(source); }

    JSObject* evaluate_to_object(StringView source) const
    {
        auto completion = evaluate(source);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        return object_of_value(completion.payload);
    }

    bool evaluates_to_true(StringView source) const
    {
        auto completion = evaluate(source);
        return completion.variant == JS_COMPLETION_NORMAL && completion.payload == js_true;
    }

    void define_global(Utf16FlyString const& name, JSValue value) const
    {
        auto key = key_of(name);
        js_object_define_direct_property(vm(), global_object(), &key, value, all_attributes);
    }

    void collect_garbage() const { js_vm_collect_garbage(vm()); }

private:
    NonnullOwnPtr<EmbeddedVM> m_embedded_vm;
};

TEST_CASE(property_operations_from_cpp)
{
    Harness harness;
    auto* vm = harness.vm();
    auto* object_prototype = harness.evaluate_to_object("Object.prototype"sv);
    auto* object = js_object_create(vm, harness.realm(), object_prototype);
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
    harness.define_global("object"_utf16_fly_string, value_of_object(object));
    EXPECT(harness.evaluates_to_true("object.answer === 43 && object.created === 1 && Object.keys(object).join() === 'answer,created'"sv));

    EXPECT_EQ(js_object_delete_property_or_throw(vm, object, &created).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_object_has_own_property(vm, object, &created).payload, 0u);

    auto* array = js_array_create_from(vm, harness.realm(), nullptr, 0);
    EXPECT_EQ(js_object_class_id(array), JS_LAYOUT_CLASS_ID_ARRAY);
    EXPECT(js_object_is_subclass_of(array, JS_LAYOUT_CLASS_ID_OBJECT));
    EXPECT(!js_object_is_subclass_of(object, JS_LAYOUT_CLASS_ID_ARRAY));
}

TEST_CASE(descriptors_round_trip_with_their_property_offsets)
{
    Harness harness;
    auto* vm = harness.vm();
    auto* object = js_object_create(vm, harness.realm(), nullptr);
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

    auto* accessor_holder = harness.evaluate_to_object("({ get value() { return 1; } })"sv);
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
    Harness harness;
    auto* vm = harness.vm();
    auto* object = harness.evaluate_to_object("({ b: 1, a: 2, 1: 3, [Symbol.iterator]: 4 })"sv);
    Vector<JSValue> keys;
    JSValueSink sink { &keys, append_to_vector };
    EXPECT_EQ(js_object_internal_own_property_keys(vm, object, &sink).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(keys.size(), 4u);

    auto* keys_array = js_array_create_from(vm, harness.realm(), keys.data(), keys.size());
    harness.define_global("keys"_utf16_fly_string, value_of_object(keys_array));
    EXPECT(harness.evaluates_to_true("keys.slice(0, 3).join() === '1,b,a' && keys[3] === Symbol.iterator"sv));
}

TEST_CASE(integrity_levels)
{
    Harness harness;
    auto* vm = harness.vm();
    auto* object = harness.evaluate_to_object("({ x: 1 })"sv);
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
