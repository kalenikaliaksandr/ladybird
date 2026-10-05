/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/BitCast.h>
#include <AK/String.h>
#include <AK/StringBuilder.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <LibGC/Root.h>
#include <LibJS/HostObjectABI.h>
#include <LibJS/Runtime/BigInt.h>
#include <LibJS/Runtime/CommonPropertyNames.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/Object.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyDescriptor.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Symbol.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibTest/TestCase.h>

// The value types of LibJS's API that need no VM: their bits, their header-only behavior and the conversions that only
// look at a value's bits. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

static_assert(sizeof(Value) == sizeof(JSValue));
static_assert(sizeof(Optional<Value>) == sizeof(Value));
static_assert(sizeof(PropertyKey) == sizeof(JSPropertyKey));
static_assert(alignof(PropertyKey) == alignof(JSPropertyKey));
static_assert(sizeof(Optional<PropertyKey>) == sizeof(PropertyKey));
static_assert(sizeof(Optional<Completion>) == sizeof(Completion));

static_assert(sizeof(ThrowCompletionOr<Value>) == sizeof(JSCompletion));
static_assert(alignof(ThrowCompletionOr<Value>) == alignof(JSCompletion));
static_assert(IsTriviallyCopyable<ThrowCompletionOr<Value>>);
static_assert(IsTriviallyDestructible<ThrowCompletionOr<Value>>);

static constexpr u64 shifted_tag(u64 tag)
{
    return tag << GC::TAG_SHIFT;
}

static String formatted(Value value)
{
    return MUST(String::formatted("{}", value));
}

// The word that crosses the embedding ABI as the key's JSPropertyKey.
static uintptr_t abi_bits_of(PropertyKey const& property_key)
{
    JSPropertyKey abi_property_key;
    __builtin_memcpy(&abi_property_key, &property_key, sizeof(abi_property_key));
    return abi_property_key.bits;
}

TEST_CASE(value_bits_are_those_both_runtimes_share)
{
    EXPECT_EQ(js_undefined().encoded(), shifted_tag(0b110 | GC::BASE_TAG));
    EXPECT_EQ(js_null().encoded(), shifted_tag(0b111 | GC::BASE_TAG));
    EXPECT_EQ(js_special_empty_value().encoded(), shifted_tag(0b011 | GC::BASE_TAG));
    EXPECT_EQ(Value(true).encoded(), shifted_tag(0b001 | GC::BASE_TAG) | 1);
    EXPECT_EQ(Value(false).encoded(), shifted_tag(0b001 | GC::BASE_TAG));
    EXPECT_EQ(Value(-7).encoded(), shifted_tag(0b010 | GC::BASE_TAG) | 0xFFFFFFF9u);
    EXPECT_EQ(Value(1.5).encoded(), bit_cast<u64>(1.5));
    EXPECT_EQ(Value().encoded(), js_undefined().encoded());
}

TEST_CASE(value_predicates)
{
    EXPECT(js_undefined().is_undefined());
    EXPECT(js_undefined().is_nullish());
    EXPECT(js_null().is_null());
    EXPECT(js_null().is_nullish());
    EXPECT(!Value(false).is_nullish());
    EXPECT(!Value(0).is_nullish());
    EXPECT(js_special_empty_value().is_special_empty_value());
    EXPECT(!js_undefined().is_special_empty_value());

    EXPECT(Value(true).is_boolean());
    EXPECT(Value(true).as_bool());
    EXPECT(!Value(false).as_bool());

    for (auto value : { js_undefined(), js_null(), Value(true), Value(1), Value(1.5) }) {
        EXPECT(!value.is_cell());
        EXPECT(!value.is_object());
        EXPECT(!value.is_string());
        EXPECT(!value.is_symbol());
        EXPECT(!value.is_bigint());
        EXPECT(!value.is_accessor());
        EXPECT(!value.is_function());
        EXPECT(!value.is_constructor());
    }
}

TEST_CASE(value_numbers)
{
    EXPECT(Value(42).is_int32());
    EXPECT_EQ(Value(42).as_i32(), 42);
    EXPECT_EQ(Value(-42).as_double(), -42.0);
    EXPECT(Value(42).is_number());

    // Integral doubles in the int32 range are stored as int32s, except negative zero.
    EXPECT(Value(3.0).is_int32());
    EXPECT_EQ(Value(3.0).as_i32(), 3);
    EXPECT(!Value(-0.0).is_int32());
    EXPECT(Value(-0.0).is_negative_zero());
    EXPECT(!Value(0.0).is_negative_zero());
    EXPECT(Value(2147483648.0).is_double());
    EXPECT(!Value(2147483648.0).is_int32());

    EXPECT(Value(static_cast<unsigned>(NumericLimits<i32>::max())).is_int32());
    EXPECT(!Value(static_cast<unsigned>(NumericLimits<i32>::max()) + 1u).is_int32());
    EXPECT_EQ(Value(4294967295u).as_double(), 4294967295.0);
    EXPECT(Value(static_cast<u16>(7)).is_int32());
    EXPECT_EQ(Value(static_cast<u64>(1) << 40).as_double(), 1099511627776.0);

    // Every NaN becomes the one canonical NaN.
    EXPECT_EQ(Value(bit_cast<double>(0x7FF0000000000001ull)).encoded(), GC::CANON_NAN_BITS);
    EXPECT_EQ(Value(bit_cast<double>(0xFFF8000000000000ull)).encoded(), GC::CANON_NAN_BITS);
    EXPECT(Value(NAN).is_nan());
    EXPECT(Value(NAN).is_number());

    EXPECT(Value(INFINITY).is_positive_infinity());
    EXPECT(Value(-INFINITY).is_negative_infinity());
    EXPECT(!Value(INFINITY).is_negative_infinity());
    EXPECT(!Value(INFINITY).is_finite_number());
    EXPECT(!Value(NAN).is_finite_number());
    EXPECT(Value(1.5).is_finite_number());
    EXPECT(!js_undefined().is_finite_number());

    EXPECT(Value(7).is_integral_number());
    EXPECT(Value(1e20).is_integral_number());
    EXPECT(!Value(1.5).is_integral_number());
    EXPECT(!Value(INFINITY).is_integral_number());

    EXPECT_EQ(MAX_ARRAY_LIKE_INDEX, 9007199254740991.0);
}

TEST_CASE(value_from_null_pointers_is_null)
{
    EXPECT(Value(static_cast<Object*>(nullptr)).is_null());
    EXPECT(Value(static_cast<PrimitiveString*>(nullptr)).is_null());
    EXPECT(Value(static_cast<Symbol*>(nullptr)).is_null());
    EXPECT(Value(static_cast<BigInt*>(nullptr)).is_null());
    EXPECT(Value(GC::Ptr<Object> {}).is_null());
    EXPECT(Value(GC::Ptr<PrimitiveString> {}).is_null());
}

TEST_CASE(value_casts_of_values_that_hold_no_object)
{
    EXPECT(!Value(1).as_if<Object>());
    EXPECT(!Value(1).as_if<FunctionObject>());
    EXPECT(!js_null().is<FunctionObject>());
    Value const constant_value = js_undefined();
    EXPECT(!constant_value.as_if<FunctionObject>());
}

TEST_CASE(value_to_boolean)
{
    EXPECT(Value(true).to_boolean());
    EXPECT(!Value(false).to_boolean());
    EXPECT(Value(1).to_boolean());
    EXPECT(!Value(0).to_boolean());
    EXPECT(Value(-0.5).to_boolean());
    EXPECT(!Value(-0.0).to_boolean());
    EXPECT(!Value(NAN).to_boolean());
    EXPECT(Value(INFINITY).to_boolean());
    EXPECT(!js_undefined().to_boolean());
    EXPECT(!js_null().to_boolean());
}

TEST_CASE(value_same_value)
{
    EXPECT(same_value(Value(NAN), Value(NAN)));
    EXPECT(!same_value(Value(0.0), Value(-0.0)));
    EXPECT(same_value(Value(1), Value(1.0)));
    EXPECT(!same_value(js_undefined(), js_null()));
    EXPECT(!same_value(Value(1), Value(true)));

    EXPECT(same_value_zero(Value(NAN), Value(NAN)));
    EXPECT(same_value_zero(Value(0.0), Value(-0.0)));
    EXPECT(!same_value_zero(Value(1), Value(2)));

    EXPECT(Value(2.5) == Value(2.5));
    EXPECT(!(Value(0.0) == Value(-0.0)));
}

TEST_CASE(value_strings_without_side_effects)
{
    EXPECT_EQ(js_undefined().to_utf16_string_without_side_effects(), "undefined"sv);
    EXPECT_EQ(js_null().to_utf16_string_without_side_effects(), "null"sv);
    EXPECT_EQ(Value(true).to_utf16_string_without_side_effects(), "true"sv);
    EXPECT_EQ(Value(false).to_utf16_string_without_side_effects(), "false"sv);
    EXPECT_EQ(Value(-42).to_utf16_string_without_side_effects(), "-42"sv);
    EXPECT_EQ(Value(1.5).to_utf16_string_without_side_effects(), "1.5"sv);
    EXPECT_EQ(Value(-0.0).to_utf16_string_without_side_effects(), "0"sv);
    EXPECT_EQ(Value(NAN).to_utf16_string_without_side_effects(), "NaN"sv);
    EXPECT_EQ(Value(-INFINITY).to_utf16_string_without_side_effects(), "-Infinity"sv);
    EXPECT_EQ(Value(1e21).to_utf16_string_without_side_effects(), "1e+21"sv);
    EXPECT_EQ(Value(1.25e-7).to_utf16_string_without_side_effects(), "1.25e-7"sv);

    EXPECT_EQ(formatted(Value(3)), "3"sv);
    EXPECT_EQ(formatted(js_undefined()), "undefined"sv);
    EXPECT_EQ(formatted(js_special_empty_value()), "<empty>"sv);
}

TEST_CASE(number_to_string)
{
    struct Case {
        double number;
        StringView with_exponent;
        StringView without_exponent;
    };
    Case const cases[] = {
        { 0.0, "0"sv, "0"sv },
        { -0.0, "0"sv, "0"sv },
        { 123.0, "123"sv, "123"sv },
        { -0.125, "-0.125"sv, "-0.125"sv },
        { 0.000001, "0.000001"sv, "0.000001"sv },
        { 0.0000001, "1e-7"sv, "0.0000001"sv },
        { 123456789012345680000.0, "123456789012345680000"sv, "123456789012345680000"sv },
        { 1e21, "1e+21"sv, "1000000000000000000000"sv },
        { 1.5e25, "1.5e+25"sv, "15000000000000000000000000"sv },
        { NAN, "NaN"sv, "NaN"sv },
        { INFINITY, "Infinity"sv, "Infinity"sv },
        { -INFINITY, "-Infinity"sv, "-Infinity"sv },
    };
    for (auto const& test_case : cases) {
        StringBuilder builder;
        builder.append("x="sv);
        number_to_string(builder, test_case.number);
        EXPECT_EQ(builder.string_view().substring_view(2), test_case.with_exponent);
        EXPECT_EQ(number_to_utf16_string(test_case.number), test_case.with_exponent);

        StringBuilder builder_without_exponent;
        number_to_string(builder_without_exponent, test_case.number, NumberToStringMode::WithoutExponent);
        EXPECT_EQ(builder_without_exponent.string_view(), test_case.without_exponent);
        EXPECT_EQ(number_to_utf16_string(test_case.number, NumberToStringMode::WithoutExponent), test_case.without_exponent);
    }
}

TEST_CASE(optional_value_and_traits)
{
    Optional<Value> empty;
    EXPECT(!empty.has_value());
    Optional<Value> undefined = js_undefined();
    EXPECT(undefined.has_value());
    EXPECT(undefined->is_undefined());

    EXPECT_EQ(Traits<Value>::hash(Value(5)), Traits<u64>::hash(Value(5).encoded()));
    EXPECT(Traits<Value>::is_trivial());
}

TEST_CASE(roots_of_values_that_hold_no_cell)
{
    auto root = GC::make_root(Value(5));
    EXPECT(!root.is_null());
    EXPECT_EQ(root.value().as_i32(), 5);
    EXPECT(!root.cell());
    EXPECT(GC::Root<Value> {}.is_null());
}

static ThrowCompletionOr<int> add_one_unless_thrown(ThrowCompletionOr<int> input)
{
    auto number = TRY(input);
    return number + 1;
}

TEST_CASE(completion)
{
    Completion normal;
    EXPECT_EQ(normal.type(), Completion::Type::Normal);
    EXPECT(normal.value().is_undefined());
    EXPECT(!normal.is_abrupt());
    EXPECT(!normal.is_error());

    Completion from_value = Value(3);
    EXPECT_EQ(from_value.type(), Completion::Type::Normal);
    EXPECT_EQ(from_value.release_value().as_i32(), 3);

    auto thrown = throw_completion(Value(4));
    EXPECT_EQ(thrown.type(), Completion::Type::Throw);
    EXPECT(thrown.is_abrupt());
    EXPECT(thrown.is_error());
    EXPECT_EQ(thrown.value().as_i32(), 4);
    auto released = thrown.release_error();
    EXPECT_EQ(released.type(), Completion::Type::Throw);
    EXPECT_EQ(released.value().as_i32(), 4);

    auto explicit_normal = normal_completion(Value(5));
    EXPECT_EQ(explicit_normal.type(), Completion::Type::Normal);
    EXPECT_EQ(explicit_normal.value().as_i32(), 5);

    Completion copy = thrown;
    EXPECT(copy.is_error());
    Completion moved = move(copy);
    EXPECT_EQ(moved.value().as_i32(), 4);

    Optional<Completion> no_completion;
    EXPECT(!no_completion.has_value());
    no_completion = moved;
    EXPECT(no_completion.has_value());
    EXPECT(no_completion->is_error());
}

TEST_CASE(throw_completion_or)
{
    ThrowCompletionOr<Value> value_result = Value(6);
    EXPECT(!value_result.is_error());
    EXPECT(!value_result.is_throw_completion());
    EXPECT_EQ(value_result.value().as_i32(), 6);
    EXPECT_EQ(value_result.release_value().as_i32(), 6);

    ThrowCompletionOr<Value> thrown_result = throw_completion(Value(7));
    EXPECT(thrown_result.is_error());
    EXPECT(thrown_result.is_throw_completion());
    EXPECT_EQ(thrown_result.error_value().as_i32(), 7);
    EXPECT_EQ(thrown_result.throw_completion().value().as_i32(), 7);
    EXPECT_EQ(thrown_result.error().type(), Completion::Type::Throw);
    EXPECT_EQ(thrown_result.release_error().value().as_i32(), 7);

    Completion from_thrown_result = ThrowCompletionOr<Value> { throw_completion(Value(8)) };
    EXPECT(from_thrown_result.is_error());
    EXPECT_EQ(from_thrown_result.value().as_i32(), 8);
    Completion from_value_result = ThrowCompletionOr<Value> { Value(9) };
    EXPECT_EQ(from_value_result.type(), Completion::Type::Normal);
    EXPECT_EQ(from_value_result.value().as_i32(), 9);

    ThrowCompletionOr<void> void_result;
    EXPECT(!void_result.is_error());
    ThrowCompletionOr<void> thrown_void_result = throw_completion(js_null());
    EXPECT(thrown_void_result.is_error());
    EXPECT(thrown_void_result.error_value().is_null());

    ThrowCompletionOr<Optional<int>> none = OptionalNone {};
    EXPECT(!none.value().has_value());

    EXPECT_EQ(add_one_unless_thrown(1).value(), 2);
    auto propagated = add_one_unless_thrown(throw_completion(Value(10)));
    EXPECT(propagated.is_error());
    EXPECT_EQ(propagated.error_value().as_i32(), 10);
}

// The Rust interpreter reads what a native function returns as a JSCompletion.
TEST_CASE(throw_completion_or_value_has_the_layout_of_a_native_function_result)
{
    ThrowCompletionOr<Value> value_result = Value(11);
    auto abi_value_result = bit_cast<JSCompletion>(value_result);
    EXPECT_EQ(abi_value_result.payload, Value(11).encoded());
    EXPECT_EQ(abi_value_result.variant, JS_COMPLETION_NORMAL);

    ThrowCompletionOr<Value> thrown_result = throw_completion(Value(1.5));
    auto abi_thrown_result = bit_cast<JSCompletion>(thrown_result);
    EXPECT_EQ(abi_thrown_result.payload, Value(1.5).encoded());
    EXPECT_EQ(abi_thrown_result.variant, JS_COMPLETION_THROW);
}

TEST_CASE(property_key_numbers)
{
    PropertyKey zero { 0 };
    EXPECT(zero.is_number());
    EXPECT_EQ(zero.as_number(), 0u);

    PropertyKey index { 4294967294u };
    EXPECT(index.is_number());
    EXPECT_EQ(index.as_number(), 4294967294u);

    // 2^32 - 1 is not an array index, so it is a string key.
    PropertyKey not_an_index { 4294967295u };
    EXPECT(not_an_index.is_string());
    EXPECT_EQ(not_an_index.as_string(), "4294967295"_utf16_fly_string);

    PropertyKey large { static_cast<u64>(1) << 40 };
    EXPECT(large.is_string());
    EXPECT_EQ(large.to_utf16_string(), "1099511627776"sv);

    EXPECT_EQ(abi_bits_of(PropertyKey { 5u }), (static_cast<uintptr_t>(5) << 2) | PropertyKey::NUMBER_FLAG);
}

TEST_CASE(property_key_strings)
{
    PropertyKey name { "name"_utf16_fly_string };
    EXPECT(name.is_string());
    EXPECT(!name.is_number());
    EXPECT(!name.is_symbol());
    EXPECT_EQ(name.as_string(), "name"_utf16_fly_string);
    EXPECT_EQ(name.to_utf16_string(), "name"sv);

    PropertyKey canonical_number { "123"_utf16_fly_string };
    EXPECT(canonical_number.is_number());
    EXPECT_EQ(canonical_number.as_number(), 123u);

    PropertyKey zero { "0"_utf16_fly_string };
    EXPECT(zero.is_number());

    PropertyKey leading_zero { "0123"_utf16_fly_string };
    EXPECT(leading_zero.is_string());

    PropertyKey too_large { "4294967295"_utf16_fly_string };
    EXPECT(too_large.is_string());

    PropertyKey not_parsed { "123"_utf16_fly_string, PropertyKey::StringMayBeNumber::No };
    EXPECT(not_parsed.is_string());
    EXPECT(not_parsed != canonical_number);

    PropertyKey from_string { "abc"_utf16 };
    EXPECT(from_string.is_string());
    EXPECT_EQ(from_string, PropertyKey { "abc"_utf16_fly_string });

    // A string key is the word of its fly string, which AK interns process-wide.
    auto fly_string = "a property name that is long enough to live on the heap"_utf16_fly_string;
    EXPECT_EQ(abi_bits_of(PropertyKey { fly_string }), fly_string.raw_identity());
}

TEST_CASE(property_key_copies_moves_and_traits)
{
    PropertyKey original { "copied"_utf16_fly_string };
    PropertyKey copy = original;
    EXPECT_EQ(copy, original);
    PropertyKey moved = move(copy);
    EXPECT_EQ(moved, original);

    PropertyKey number { 9u };
    moved = number;
    EXPECT(moved.is_number());
    moved = PropertyKey { "again"_utf16_fly_string };
    EXPECT(moved.is_string());

    EXPECT(Traits<PropertyKey>::equals(PropertyKey { 3u }, PropertyKey { "3"_utf16_fly_string }));
    EXPECT(!Traits<PropertyKey>::equals(PropertyKey { 3u }, PropertyKey { "3"_utf16_fly_string, PropertyKey::StringMayBeNumber::No }));
    EXPECT_EQ(Traits<PropertyKey>::hash(PropertyKey { "hashed"_utf16_fly_string }), Traits<PropertyKey>::hash(PropertyKey { "hashed"_utf16 }));

    EXPECT_EQ(MUST(String::formatted("{}", PropertyKey { 12u })), "12"sv);
    EXPECT_EQ(MUST(String::formatted("{}", PropertyKey { "key"_utf16_fly_string })), "key"sv);

    Optional<PropertyKey> no_key;
    EXPECT(!no_key.has_value());
    no_key = PropertyKey { 1u };
    EXPECT(no_key.has_value());

    auto* allocated_key = new PropertyKey { "allocated"_utf16_fly_string };
    EXPECT(allocated_key->is_string());
    delete allocated_key;
}

TEST_CASE(property_descriptor)
{
    PropertyDescriptor empty;
    EXPECT(empty.is_empty());
    EXPECT(empty.is_generic_descriptor());
    EXPECT(!empty.is_data_descriptor());
    EXPECT(!empty.is_accessor_descriptor());

    PropertyDescriptor data { .value = Value(1), .writable = true };
    EXPECT(!data.is_empty());
    EXPECT(data.is_data_descriptor());
    EXPECT(!data.is_accessor_descriptor());
    EXPECT_EQ(MUST(String::formatted("{}", data)), "PropertyDescriptor { [[Value]]: 1, [[Writable]]: true }"sv);

    PropertyDescriptor writable_only { .writable = false };
    EXPECT(writable_only.is_data_descriptor());

    PropertyDescriptor accessor { .get = GC::Ptr<FunctionObject> {}, .enumerable = false, .configurable = true };
    EXPECT(accessor.is_accessor_descriptor());
    EXPECT(!accessor.is_data_descriptor());
    EXPECT(!accessor.is_generic_descriptor());

    PropertyDescriptor generic { .enumerable = true };
    EXPECT(generic.is_generic_descriptor());
}

TEST_CASE(common_property_names_are_interned_keys)
{
    CommonPropertyNames names;

    EXPECT(names.length.is_string());
    EXPECT_EQ(names.length, PropertyKey { "length"_utf16_fly_string });
    EXPECT_EQ(names.length.as_string().raw_identity(), "length"_utf16_fly_string.raw_identity());
    EXPECT_EQ(names.prototype.as_string(), "prototype"_utf16_fly_string);
    EXPECT_EQ(names.delete_.as_string(), "delete"_utf16_fly_string);
    EXPECT_EQ(names.return_.as_string(), "return"_utf16_fly_string);
    EXPECT_EQ(names.inputAlias.as_string(), "$_"_utf16_fly_string);
    EXPECT_EQ(names.iterator.as_string(), "iterator"_utf16_fly_string);
    EXPECT_EQ(names.TypeError.as_string(), "TypeError"_utf16_fly_string);

    CommonPropertyNames other_names;
    EXPECT_EQ(abi_bits_of(names.toJSON), abi_bits_of(other_names.toJSON));
}
