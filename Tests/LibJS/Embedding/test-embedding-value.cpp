/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringView.h>
#include <AK/Utf16String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>
#include <LibJS/Embedding/ABI.h>
#include <LibJS/HostObjectABI.h>

#include "EmbeddingTest.h"

namespace {

Utf16View view_of(JSUtf16View view)
{
    if (view.has_ascii_storage)
        return Utf16View { StringView { static_cast<char const*>(view.data), view.length_in_code_units } };
    return Utf16View { static_cast<char16_t const*>(view.data), view.length_in_code_units };
}

JSUtf16View abi_view_of(Utf16View view)
{
    if (view.has_ascii_storage())
        return { view.ascii_span().data(), view.length_in_code_units(), true };
    return { view.utf16_span().data(), view.length_in_code_units(), false };
}

Vector<u32> magnitude_of(JSBigInt* bigint)
{
    Vector<u32> words;
    words.resize(js_bigint_magnitude_word_count(bigint));
    js_bigint_copy_magnitude_words(bigint, words.data(), words.size());
    return words;
}

// A VM with a realm to evaluate scripts in, through the testing part of the ABI.
class TestingRealm {
public:
    TestingRealm()
        : m_testing_realm(js_testing_realm_create())
    {
    }

    ~TestingRealm() { js_testing_realm_destroy(m_testing_realm); }

    JSVM* vm() const { return js_testing_realm_vm(m_testing_realm); }
    JSRealm* realm() const { return js_testing_realm_realm(m_testing_realm); }

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

TEST_CASE(strings_round_trip_without_copies)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto string = Utf16String::from_utf8("αβγδεζηθικλμ, a string with UTF-16 storage"sv);
    auto const* storage = string.utf16_view().utf16_span().data();

    auto* primitive_string = js_string_create_from_owned_utf16_string(vm, move(string).into_raw());
    auto view = js_string_utf16_view(primitive_string);
    EXPECT(!view.has_ascii_storage);
    EXPECT_EQ(view.data, static_cast<void const*>(storage));
    auto read_back = Utf16String::adopt_raw(js_string_utf16_string(primitive_string));
    EXPECT_EQ(read_back.utf16_view().utf16_span().data(), storage);
    EXPECT_EQ(read_back, u"αβγδεζηθικλμ, a string with UTF-16 storage"sv);

    auto* ascii = js_string_create_from_utf16_view(vm, abi_view_of(u"; then ASCII"sv));
    auto ascii_view = js_string_utf16_view(ascii);
    EXPECT(ascii_view.has_ascii_storage);
    EXPECT_EQ(view_of(ascii_view), u"; then ASCII"sv);

    // A concatenation is built lazily, and resolved when it is first read.
    auto* concatenation = pointer_of_payload<JSPrimitiveString>(js_string_create_concatenation(vm, primitive_string, ascii));
    EXPECT_EQ(js_string_length_in_code_units(concatenation), 54u);
    EXPECT_EQ(view_of(js_string_utf16_view(concatenation)), u"αβγδεζηθικλμ, a string with UTF-16 storage; then ASCII"sv);
    EXPECT_EQ(view_of(js_string_utf16_view(js_string_create_substring(vm, concatenation, 44, 4))), u"then"sv);

    // Strings from scripts are the same cells.
    auto* from_script = pointer_of_payload<JSPrimitiveString>(js_value_to_primitive_string(vm, testing_realm.value_of("'; then ' + 'ASCII'"sv)));
    EXPECT(js_string_equals(from_script, ascii));
}

TEST_CASE(bigints_round_trip)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();

    auto* minimum = js_bigint_create_from_i64(vm, NumericLimits<i64>::min());
    EXPECT(js_bigint_is_negative(minimum));
    EXPECT_EQ(js_bigint_to_i64(minimum), NumericLimits<i64>::min());
    EXPECT_EQ(magnitude_of(minimum), (Vector<u32> { 0, 0x8000'0000 }));

    auto* maximum = js_bigint_create_from_u64(vm, NumericLimits<u64>::max());
    EXPECT_EQ(js_bigint_to_u64(maximum), NumericLimits<u64>::max());
    EXPECT_EQ(Utf16String::adopt_raw(js_bigint_to_string(maximum, 16)), u"ffffffffffffffff"sv);

    u32 const words[] = { 0x1234'5678, 0, 1 };
    auto* large = js_bigint_create_from_magnitude(vm, true, words, array_size(words));
    EXPECT(js_bigint_is_negative(large));
    EXPECT_EQ(magnitude_of(large), (Vector<u32> { 0x1234'5678, 0, 1 }));
    EXPECT_EQ(Utf16String::adopt_raw(js_bigint_to_string(large, 10)), u"-18446744074014971512"sv);

    auto* from_script = pointer_of_payload<JSBigInt>(js_value_to_bigint(vm, testing_realm.value_of("-(2n ** 70n)"sv)));
    EXPECT(js_bigint_is_negative(from_script));
    EXPECT_EQ(magnitude_of(from_script), (Vector<u32> { 0, 0, 64 }));
}

TEST_CASE(symbols_have_descriptions)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();

    auto* symbol = js_symbol_create(vm, abi_view_of(u"ünïque"sv));
    JSUtf16View description {};
    EXPECT(js_symbol_description(symbol, &description));
    EXPECT_EQ(view_of(description), u"ünïque"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_symbol_descriptive_string(symbol)), u"Symbol(ünïque)"sv);
    EXPECT(!js_symbol_description(js_symbol_create_without_description(vm), &description));

    auto* iterator = js_symbol_well_known(vm, JS_WELL_KNOWN_SYMBOL_ITERATOR);
    EXPECT(js_symbol_description(iterator, &description));
    EXPECT_EQ(view_of(description), u"Symbol.iterator"sv);
    EXPECT_EQ(iterator, js_symbol_well_known(vm, JS_WELL_KNOWN_SYMBOL_ITERATOR));
}

TEST_CASE(thrown_errors_have_their_kind_and_message)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    struct ErrorKindAndName {
        JSErrorKind kind;
        Utf16View name;
    };
    ErrorKindAndName const kinds[] = {
        { JS_ERROR_KIND_ERROR, u"Error"sv },
        { JS_ERROR_KIND_EVAL_ERROR, u"EvalError"sv },
        { JS_ERROR_KIND_INTERNAL_ERROR, u"InternalError"sv },
        { JS_ERROR_KIND_RANGE_ERROR, u"RangeError"sv },
        { JS_ERROR_KIND_REFERENCE_ERROR, u"ReferenceError"sv },
        { JS_ERROR_KIND_SYNTAX_ERROR, u"SyntaxError"sv },
        { JS_ERROR_KIND_TYPE_ERROR, u"TypeError"sv },
        { JS_ERROR_KIND_URI_ERROR, u"URIError"sv },
        { JS_ERROR_KIND_AGGREGATE_ERROR, u"AggregateError"sv },
        { JS_ERROR_KIND_SUPPRESSED_ERROR, u"SuppressedError"sv },
    };
    for (auto const& [kind, name] : kinds) {
        auto completion = js_error_throw(vm, kind, abi_view_of(u"formatted by the embedder"sv));
        EXPECT_EQ(testing_realm.thrown_error_text(completion), Utf16String::formatted("{}: formatted by the embedder", name));
        auto* error = pointer_of_payload<JSObject>(js_value_to_object(vm, completion.payload));
        EXPECT(js_error_is_error(error));
        EXPECT_NE(js_error_data_of(error), nullptr);
    }

    auto message = Utf16String::from_utf8("ünïcode message, long enough for an allocation of its own"sv);
    auto completion = js_error_throw_with_owned_message(vm, JS_ERROR_KIND_TYPE_ERROR, move(message).into_raw());
    EXPECT_EQ(testing_realm.thrown_error_text(completion), u"TypeError: ünïcode message, long enough for an allocation of its own"sv);

    // With one realm, a TypeError realm scope changes nothing but what it restores.
    auto* realm = testing_realm.realm();
    auto outer_scope = js_error_type_error_realm_scope_enter(vm, realm);
    EXPECT_EQ(outer_scope.previous_realm, nullptr);
    auto inner_scope = js_error_type_error_realm_scope_enter(vm, realm);
    EXPECT_EQ(inner_scope.previous_realm, realm);
    EXPECT_EQ(testing_realm.thrown_error_text(js_error_throw(vm, JS_ERROR_KIND_TYPE_ERROR, abi_view_of(u"in scope"sv))), u"TypeError: in scope"sv);
    js_error_type_error_realm_scope_exit(vm, inner_scope);
    js_error_type_error_realm_scope_exit(vm, outer_scope);
}

TEST_CASE(error_data_describes_the_call_stack)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto* error = pointer_of_payload<JSObject>(js_value_to_object(vm, testing_realm.value_of("function make() { return new Error('x') }\nmake()"sv)));
    auto const* error_data = js_error_data_of(error);
    EXPECT_NE(error_data, nullptr);

    // The frames are the Error constructor, make(), the script, and the realm's own execution context.
    EXPECT_EQ(js_error_data_traceback_length(error_data), 4u);
    JSTracebackFrame frame {};
    js_error_data_traceback_frame(error_data, 0, &frame);
    EXPECT_EQ(view_of(frame.function_name), u"Error"sv);
    EXPECT(!frame.has_source_range);
    js_error_data_traceback_frame(error_data, 1, &frame);
    EXPECT_EQ(view_of(frame.function_name), u"make"sv);
    EXPECT(frame.has_source_range);
    EXPECT_EQ(frame.line, 1u);
    js_error_data_traceback_frame(error_data, 2, &frame);
    EXPECT_EQ(frame.line, 2u);
    auto stack = Utf16String::adopt_raw(js_error_data_stack_string(error_data, true));
    EXPECT(stack.starts_with(u"    at Error\n    at make ("sv));

    // Error data captured outside of any script has only the bottom frame, which the stack leaves out.
    auto* cell = js_error_data_cell_capture(vm);
    auto const* cell_error_data = js_error_data_cell_error_data(cell);
    EXPECT_EQ(js_error_data_traceback_length(cell_error_data), 1u);
    EXPECT(Utf16String::adopt_raw(js_error_data_stack_string(cell_error_data, false)).is_empty());
}

TEST_CASE(errors_of_embedder_defined_classes)
{
    TestingRealm testing_realm;
    auto* vm = testing_realm.vm();
    auto* prototype = pointer_of_payload<JSObject>(js_value_to_object(vm, testing_realm.value_of("Object.create(Error.prototype, { name: { value: 'WebAssembly.LinkError' } })"sv)));

    auto* error = js_error_create_with_prototype(vm, testing_realm.realm(), prototype);
    js_error_set_owned_message(vm, error, Utf16String::from_utf8("import failed"sv).into_raw());
    EXPECT(js_error_is_error(error));
    EXPECT(!js_error_is_error(prototype));
    EXPECT_NE(js_error_data_of(error), nullptr);
    EXPECT_EQ(js_error_data_of(prototype), nullptr);

    auto* range_error = js_error_create(vm, testing_realm.realm(), JS_ERROR_KIND_RANGE_ERROR);
    js_error_set_message(vm, range_error, abi_view_of(u"out of range"sv));
    EXPECT(js_error_is_error(range_error));
}
