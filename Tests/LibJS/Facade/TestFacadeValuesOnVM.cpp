/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <LibCrypto/BigInt/SignedBigInteger.h>
#include <LibJS/Runtime/BigInt.h>
#include <LibJS/Runtime/CommonPropertyNames.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/Object.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Symbol.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibTest/TestCase.h>

// The value types of LibJS's API that need a VM: strings, symbols and BigInts, and the conversions that can run
// JavaScript or throw. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

namespace {

struct TestVM {
    TestVM()
        : vm(VM::create())
        , execution_context(MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr)))
    {
    }

    ~TestVM()
    {
        vm->pop_execution_context();
    }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> execution_context;
};

}

static Value string_value(VM& vm, StringView string)
{
    return PrimitiveString::create(vm, Utf16String::from_utf8(string));
}

TEST_CASE(primitive_strings)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    auto hello = PrimitiveString::create(vm, "hello"_utf16);
    EXPECT_EQ(hello->utf16_string(), "hello"sv);
    EXPECT(hello->utf16_string_view() == "hello"sv);
    EXPECT_EQ(hello->length_in_utf16_code_units(), 5u);

    auto non_ascii = Utf16String::from_utf8("café \U0001F600"sv);
    auto from_view = PrimitiveString::create(vm, non_ascii.utf16_view());
    EXPECT_EQ(from_view->utf16_string(), non_ascii);
    EXPECT_EQ(from_view->length_in_utf16_code_units(), 7u);

    auto from_fly_string = PrimitiveString::create(vm, "hello"_utf16_fly_string);
    EXPECT(*from_fly_string == *hello);
    EXPECT(!(*from_fly_string == *from_view));

    auto world = PrimitiveString::create(vm, "world"_utf16);
    auto concatenation = MUST(PrimitiveString::create(vm, *hello, *world));
    EXPECT_EQ(concatenation->utf16_string(), "helloworld"sv);
    EXPECT_EQ(concatenation->length_in_utf16_code_units(), 10u);

    auto substring = PrimitiveString::create(vm, *concatenation, 3, 4);
    EXPECT(substring->utf16_string_view() == "lowo"sv);

    EXPECT_EQ(PrimitiveString::create_from_unsigned_integer(vm, 4294967296)->utf16_string(), "4294967296"sv);

    Value value = hello;
    EXPECT(value.is_string());
    EXPECT_EQ(&value.as_string(), hello.ptr());
    EXPECT(value.to_boolean());
    EXPECT(!Value(PrimitiveString::create(vm, Utf16String {})).to_boolean());
    EXPECT_EQ(value.to_utf16_string_without_side_effects(), "hello"sv);
}

TEST_CASE(conversions_of_strings)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    EXPECT_EQ(MUST(string_value(vm, "42"sv).to_utf16_string(vm)), "42"sv);
    EXPECT_EQ(MUST(string_value(vm, "42"sv).to_number(vm)).as_double(), 42.0);
    EXPECT(MUST(string_value(vm, "nope"sv).to_number(vm)).is_nan());
    EXPECT_EQ(MUST(string_value(vm, " 1.5 "sv).to_double(vm)), 1.5);
    EXPECT_EQ(MUST(string_value(vm, "-3"sv).to_i32(vm)), -3);
    EXPECT_EQ(MUST(string_value(vm, "-1"sv).to_u32(vm)), 4294967295u);
    EXPECT_EQ(MUST(string_value(vm, "65537"sv).to_u16(vm)), 1u);
    EXPECT_EQ(MUST(string_value(vm, "257"sv).to_u8(vm)), 1u);
    EXPECT_EQ(MUST(string_value(vm, "-5"sv).to_length(vm)), 0u);
    EXPECT_EQ(MUST(string_value(vm, "3"sv).to_index(vm)), 3u);
}

TEST_CASE(conversions_of_numbers)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    EXPECT_EQ(MUST(Value(1.5).to_utf16_string(vm)), "1.5"sv);
    EXPECT_EQ(MUST(Value(-1).to_u32(vm)), 4294967295u);
    EXPECT_EQ(MUST(Value(4294967301.0).to_i32(vm)), 5);
    EXPECT_EQ(MUST(Value(2147483648.0).to_i32(vm)), NumericLimits<i32>::min());
    EXPECT_EQ(MUST(Value(-0.5).to_u8(vm)), 0u);
    EXPECT_EQ(MUST(Value(INFINITY).to_length(vm)), 9007199254740991u);
    EXPECT_EQ(MUST(Value(2.9).to_index(vm)), 2u);
    EXPECT_EQ(MUST(Value(true).to_double(vm)), 1.0);
    EXPECT(MUST(js_undefined().to_double(vm)) != MUST(js_undefined().to_double(vm)));
    EXPECT_EQ(MUST(js_null().to_number(vm)).as_double(), 0.0);

    auto out_of_range_index = Value(-1).to_index(vm);
    EXPECT(out_of_range_index.is_error());
    EXPECT(out_of_range_index.error_value().is_object());

    EXPECT(!MUST(Value(5).is_array(vm)));
}

TEST_CASE(conversions_to_objects)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    auto number_object = MUST(Value(5).to_object(vm));
    Value number_object_value = number_object;
    EXPECT(number_object_value.is_object());
    EXPECT_EQ(&number_object_value.as_object(), number_object.ptr());
    EXPECT_EQ(number_object_value.as_if<Object>().ptr(), number_object.ptr());
    EXPECT(!number_object_value.is_function());
    EXPECT(!number_object_value.is<FunctionObject>());
    EXPECT(number_object_value.to_boolean());
    EXPECT_EQ(MUST(number_object_value.to_object(vm)).ptr(), number_object.ptr());
    EXPECT_EQ(MUST(number_object_value.to_number(vm)).as_double(), 5.0);

    auto from_undefined = js_undefined().to_object(vm);
    EXPECT(from_undefined.is_error());
    EXPECT(from_undefined.error_value().is_object());
}

TEST_CASE(get_and_get_method)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    EXPECT_EQ(MUST(string_value(vm, "abc"sv).get(vm, PropertyKey { "length"_utf16_fly_string })).as_double(), 3.0);
    EXPECT(MUST(Value(5).get(vm, PropertyKey { "missing"_utf16_fly_string })).is_undefined());

    auto to_string = MUST(Value(5).get_method(vm, PropertyKey { "toString"_utf16_fly_string }));
    EXPECT(to_string);
    Value function_value = to_string;
    EXPECT(function_value.is_function());
    EXPECT(!function_value.is_constructor());
    EXPECT_EQ(&function_value.as_function(), to_string.ptr());
    EXPECT_EQ(function_value.as_if<FunctionObject>().ptr(), to_string.ptr());
    EXPECT(function_value.is<FunctionObject>());

    EXPECT(!MUST(Value(5).get_method(vm, PropertyKey { "missing"_utf16_fly_string })));

    auto from_null = js_null().get_method(vm, PropertyKey { "toString"_utf16_fly_string });
    EXPECT(from_null.is_error());
}

TEST_CASE(symbols)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    auto described = Symbol::create(vm, "described"_utf16);
    EXPECT_EQ(described->descriptive_string(), "Symbol(described)"sv);
    EXPECT_EQ(Symbol::create(vm)->descriptive_string(), "Symbol()"sv);
    EXPECT_NE(Symbol::create(vm).ptr(), Symbol::create(vm).ptr());
    auto private_symbol = Symbol::create_private(vm);
    EXPECT_NE(private_symbol.ptr(), described.ptr());

    Value value = described;
    EXPECT(value.is_symbol());
    EXPECT_EQ(&value.as_symbol(), described.ptr());
    EXPECT(value.to_boolean());
    EXPECT_EQ(value.to_utf16_string_without_side_effects(), "Symbol(described)"sv);
    EXPECT(value.to_utf16_string(vm).is_error());
    EXPECT(value.to_number(vm).is_error());

    PropertyKey key { described };
    EXPECT(key.is_symbol());
    EXPECT_EQ(key.as_symbol(), described.ptr());
    EXPECT_EQ(key.to_utf16_string(), "Symbol(described)"sv);
    EXPECT(same_value(key.to_value(vm), value));
}

TEST_CASE(bigints)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    auto big_integer = MUST(Crypto::SignedBigInteger::from_base(10, "-123456789012345678901234567890"sv));
    auto bigint = BigInt::create(vm, big_integer);
    EXPECT_EQ(bigint->big_integer(), big_integer);
    EXPECT_EQ(bigint->to_utf16_string(), "-123456789012345678901234567890n"sv);

    Value value = bigint;
    EXPECT(value.is_bigint());
    EXPECT_EQ(&value.as_bigint(), bigint.ptr());
    EXPECT(value.to_boolean());
    EXPECT(!Value(BigInt::create(vm, Crypto::SignedBigInteger { 0 })).to_boolean());
    EXPECT(value.to_number(vm).is_error());

    auto two_to_the_64_plus_5 = MUST(Crypto::SignedBigInteger::from_base(10, "18446744073709551621"sv));
    EXPECT_EQ(MUST(Value(BigInt::create(vm, two_to_the_64_plus_5)).to_bigint_int64(vm)), 5);
    EXPECT_EQ(MUST(Value(BigInt::create(vm, Crypto::SignedBigInteger { -1 })).to_bigint_uint64(vm)), NumericLimits<u64>::max());
    EXPECT_EQ(MUST(Value(BigInt::create(vm, Crypto::SignedBigInteger { -1 })).to_bigint_int64(vm)), -1);

    auto from_string = MUST(string_value(vm, "42"sv).to_bigint(vm));
    EXPECT_EQ(from_string->big_integer(), Crypto::SignedBigInteger { 42 });
    EXPECT(Value(1).to_bigint(vm).is_error());
}

TEST_CASE(property_keys_from_and_to_values)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    auto from_integer = MUST(PropertyKey::from_value(vm, Value(5)));
    EXPECT(from_integer.is_number());
    EXPECT_EQ(from_integer.as_number(), 5u);

    auto from_fraction = MUST(PropertyKey::from_value(vm, Value(1.5)));
    EXPECT(from_fraction.is_string());
    EXPECT_EQ(from_fraction.as_string(), "1.5"_utf16_fly_string);

    auto from_negative = MUST(PropertyKey::from_value(vm, Value(-1)));
    EXPECT_EQ(from_negative.as_string(), "-1"_utf16_fly_string);

    auto from_numeric_string = MUST(PropertyKey::from_value(vm, string_value(vm, "7"sv)));
    EXPECT(from_numeric_string.is_number());
    EXPECT_EQ(from_numeric_string.as_number(), 7u);

    auto from_string = MUST(PropertyKey::from_value(vm, string_value(vm, "seven"sv)));
    EXPECT_EQ(from_string.as_string(), "seven"_utf16_fly_string);

    auto symbol = Symbol::create(vm, "key"_utf16);
    auto from_symbol = MUST(PropertyKey::from_value(vm, symbol));
    EXPECT_EQ(from_symbol.as_symbol(), symbol.ptr());

    auto number_key_value = PropertyKey { 5u }.to_value(vm);
    EXPECT(number_key_value.is_string());
    EXPECT_EQ(number_key_value.as_string().utf16_string(), "5"sv);
    auto string_key_value = PropertyKey { "name"_utf16_fly_string }.to_value(vm);
    EXPECT_EQ(string_key_value.as_string().utf16_string(), "name"sv);
}

TEST_CASE(names_of_the_vm_are_interned_keys)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;

    CommonPropertyNames names;
    EXPECT_EQ(vm.names.length, names.length);
    EXPECT_EQ(vm.names.length.as_string().raw_identity(), names.length.as_string().raw_identity());
    EXPECT_EQ(MUST(string_value(vm, "abc"sv).get(vm, vm.names.length)).as_double(), 3.0);
}
