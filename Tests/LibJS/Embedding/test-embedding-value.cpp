/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringView.h>
#include <AK/Utf16String.h>
#include <LibJS/Embedding/ABI.h>
#include <LibJS/HostObjectABI.h>

#include "EmbeddingTest.h"

namespace {

// A VM with a realm to evaluate scripts in, through the testing part of the ABI.
class TestingRealm {
public:
    TestingRealm()
        : m_testing_realm(js_testing_realm_create())
    {
    }

    ~TestingRealm() { js_testing_realm_destroy(m_testing_realm); }

    JSVM* vm() const { return js_testing_realm_vm(m_testing_realm); }

    JSCompletion evaluate(StringView source) const
    {
        return js_testing_realm_evaluate(m_testing_realm, reinterpret_cast<u8 const*>(source.characters_without_null_termination()), source.length());
    }

    JSValue value_of(StringView source) const
    {
        auto completion = evaluate(source);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        return completion.payload;
    }

    Utf16String to_string(JSValue value) const
    {
        JSOwnedUtf16String string {};
        auto completion = js_value_to_utf16_string(vm(), value, &string);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        return Utf16String::adopt_raw(string);
    }

    // "Name: message" of a thrown error, through Error.prototype.toString.
    Utf16String thrown_error_text(JSCompletion completion) const
    {
        VERIFY(completion.variant == JS_COMPLETION_THROW);
        return to_string(completion.payload);
    }

private:
    JSTestingRealm* m_testing_realm { nullptr };
};

}

TEST_CASE(conversions_run_value_of_and_report_what_it_throws)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto seven_and_a_half = testing_realm.value_of("({ valueOf() { return 7.5 } })"sv);
    auto throwing = testing_realm.value_of("({ valueOf() { throw new RangeError('from valueOf') } })"sv);

    double number = 0;
    EXPECT_EQ(js_value_to_double(vm, seven_and_a_half, &number).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(number, 7.5);
    EXPECT_EQ(testing_realm.thrown_error_text(js_value_to_double(vm, throwing, &number)), u"RangeError: from valueOf"sv);
    EXPECT_EQ(number, 7.5);

    u32 index_like = 0;
    EXPECT_EQ(js_value_to_u32(vm, seven_and_a_half, &index_like).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(index_like, 7u);
    EXPECT_EQ(js_value_to_u32(vm, throwing, &index_like).variant, JS_COMPLETION_THROW);

    auto primitive = js_value_to_primitive(vm, seven_and_a_half, JS_PREFERRED_TYPE_NUMBER);
    EXPECT_EQ(primitive.variant, JS_COMPLETION_NORMAL);
    EXPECT(js_value_same_value(primitive.payload, testing_realm.value_of("7.5"sv)));
    // With a string hint, OrdinaryToPrimitive tries toString first.
    primitive = js_value_to_primitive(vm, seven_and_a_half, JS_PREFERRED_TYPE_STRING);
    EXPECT_EQ(primitive.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(testing_realm.to_string(primitive.payload), u"[object Object]"sv);

    u64 index = 0;
    EXPECT_EQ(testing_realm.thrown_error_text(js_value_to_index(vm, testing_realm.value_of("-1"sv), &index)), u"RangeError: Index must be a positive integer no greater than 2^53-1"sv);
}

TEST_CASE(strings_and_keys_come_out_owned)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto object = testing_realm.value_of("({ toString() { return 'a string longer than a short one' } })"sv);

    EXPECT_EQ(testing_realm.to_string(object), u"a string longer than a short one"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_value_to_utf16_string_without_side_effects(object)), u"[object Object]"sv);

    JSPropertyKey key {};
    EXPECT_EQ(js_value_to_property_key(vm, testing_realm.value_of("'12'"sv), &key).variant, JS_COMPLETION_NORMAL);
    // Array index keys are numbers shifted left by two, with both low bits set.
    EXPECT_EQ(key.bits, (static_cast<uintptr_t>(12) << 2) | 3);

    auto symbol = testing_realm.value_of("Symbol('description')"sv);
    JSOwnedUtf16String string {};
    EXPECT_EQ(testing_realm.thrown_error_text(js_value_to_utf16_string(vm, symbol, &string)), u"TypeError: Cannot convert symbol to string"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_value_to_utf16_string_without_side_effects(symbol)), u"Symbol(description)"sv);
}

TEST_CASE(bigint_conversions_wrap_to_64_bits)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();

    u64 unsigned_result = 0;
    EXPECT_EQ(js_value_to_bigint_uint64(vm, testing_realm.value_of("2n ** 64n + 5n"sv), &unsigned_result).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(unsigned_result, 5u);
    i64 signed_result = 0;
    EXPECT_EQ(js_value_to_bigint_int64(vm, testing_realm.value_of("2n ** 63n"sv), &signed_result).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(signed_result, NumericLimits<i64>::min());
    EXPECT_EQ(testing_realm.thrown_error_text(js_value_to_bigint_int64(vm, testing_realm.value_of("1"sv), &signed_result)), u"TypeError: Cannot convert number to BigInt"sv);
}

TEST_CASE(equality_and_predicates)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto nan = testing_realm.value_of("NaN"sv);

    EXPECT(!js_value_is_strictly_equal(nan, nan));
    EXPECT(js_value_same_value(nan, nan));
    EXPECT(js_value_same_value_zero(testing_realm.value_of("0"sv), testing_realm.value_of("-0"sv)));
    EXPECT(!js_value_same_value(testing_realm.value_of("0"sv), testing_realm.value_of("-0"sv)));

    // A bool result is the payload of the normal completion, as 0 or 1.
    auto loosely_equal = js_value_is_loosely_equal(vm, testing_realm.value_of("'1'"sv), testing_realm.value_of("1"sv));
    EXPECT_EQ(loosely_equal.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(loosely_equal.payload, 1u);

    auto is_array = js_value_is_array(vm, testing_realm.value_of("[]"sv));
    EXPECT_EQ(is_array.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(is_array.payload, 1u);
    EXPECT_EQ(js_value_is_array(vm, testing_realm.value_of("({})"sv)).payload, 0u);
    EXPECT_EQ(js_value_is_array(vm, testing_realm.value_of("var p = Proxy.revocable([], {}); p.revoke(); p.proxy"sv)).variant, JS_COMPLETION_THROW);

    EXPECT(js_value_is_constructor(testing_realm.value_of("(class {})"sv)));
    EXPECT(!js_value_is_constructor(testing_realm.value_of("(() => {})"sv)));
    EXPECT(js_value_is_function(testing_realm.value_of("(() => {})"sv)));
    EXPECT(!js_value_to_boolean(testing_realm.value_of("''"sv)));
}
