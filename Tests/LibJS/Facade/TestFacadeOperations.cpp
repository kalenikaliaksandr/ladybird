/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StringBuilder.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibCrypto/BigInt/SignedBigInteger.h>
#include <LibGC/Root.h>
#include <LibGC/RootVector.h>
#include <LibJS/Runtime/AbstractOperations.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/Iterator.h>
#include <LibJS/Runtime/JSONObject.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Symbol.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

#ifndef AK_OS_WINDOWS
#    include <fcntl.h>
#    include <unistd.h>
#endif

// The operations of LibJS's API that run JavaScript: calls, construction, iterators, JSON, the conversions that throw,
// and completions. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

namespace {

struct TestVM {
    TestVM()
        : vm(VM::create())
        , execution_context(MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr)))
        , realm(GC::make_root(*execution_context->realm))
    {
    }

    ~TestVM()
    {
        while (!vm->execution_context_stack().is_empty())
            vm->pop_execution_context();
    }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> execution_context;
    GC::Root<Realm> realm;
};

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm, "operations.js"sv);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static Utf16String string_property_of(VM& vm, Value value, StringView name)
{
    return MUST(property_of(vm, value, name).to_utf16_string(vm));
}

static Utf16String error_name_of(VM& vm, Value error)
{
    VERIFY(error.is_object());
    return string_property_of(vm, error, "name"sv);
}

static Value string_value(VM& vm, StringView string)
{
    return PrimitiveString::create(vm, Utf16String::from_utf8(string));
}

TEST_CASE(calls)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto add = MUST(evaluate(vm, realm, "(function (a, b) { return a + b + (arguments.length > 2 ? arguments[2] : 0); })"sv));
    EXPECT(add.is_function());
    auto& add_function = add.as_function();

    EXPECT_EQ(MUST(call(vm, add, js_undefined(), Value(1), Value(2))).as_double(), 3.0);
    EXPECT_EQ(MUST(call(vm, add_function, js_undefined(), Value(1), Value(2), Value(3))).as_double(), 6.0);
    EXPECT(MUST(call(vm, add_function, js_undefined())).is_nan());

    Value arguments[] = { Value(10), Value(20) };
    EXPECT_EQ(MUST(call(vm, add, js_undefined(), ReadonlySpan<Value> { arguments })).as_double(), 30.0);
    EXPECT_EQ(MUST(call(vm, add, js_undefined(), Span<Value> { arguments })).as_double(), 30.0);
    EXPECT_EQ(MUST(call(vm, add_function, js_undefined(), ReadonlySpan<Value> { arguments })).as_double(), 30.0);
    EXPECT_EQ(MUST(call(vm, add_function, js_undefined(), Span<Value> { arguments })).as_double(), 30.0);

    GC::RootVector<Value> rooted_arguments;
    rooted_arguments.append(string_value(vm, "a"sv));
    rooted_arguments.append(string_value(vm, "b"sv));
    EXPECT_EQ(MUST(MUST(call_impl(vm, add_function, js_undefined(), rooted_arguments)).to_utf16_string(vm)), "ab0"sv);
    EXPECT_EQ(MUST(MUST(call_impl(vm, add, js_undefined(), rooted_arguments.span())).to_utf16_string(vm)), "ab0"sv);

    // The this value reaches the callee as it is in strict code, and as an object in sloppy code.
    auto strict_this = MUST(evaluate(vm, realm, "(function () { 'use strict'; return this; })"sv));
    EXPECT_EQ(MUST(call(vm, strict_this, Value(7))).as_double(), 7.0);
    auto sloppy_this = MUST(evaluate(vm, realm, "(function () { return typeof this; })"sv));
    EXPECT_EQ(MUST(MUST(call(vm, sloppy_this, Value(7))).to_utf16_string(vm)), "object"sv);

    // A built-in function is called the same way.
    auto math_max = MUST(evaluate(vm, realm, "Math.max"sv));
    EXPECT_EQ(MUST(call(vm, math_max, js_undefined(), Value(4), Value(9), Value(2))).as_double(), 9.0);

    // A throw reaches the caller as a throw completion of the thrown value.
    auto thrower = MUST(evaluate(vm, realm, "(function (value) { throw value; })"sv));
    auto thrown = call(vm, thrower, js_undefined(), Value(42));
    EXPECT(thrown.is_throw_completion());
    EXPECT_EQ(thrown.error_value().as_double(), 42.0);
    EXPECT_EQ(thrown.throw_completion().type(), Completion::Type::Throw);

    // A value that is not callable throws a TypeError.
    auto not_callable = call(vm, Value(5), js_undefined());
    EXPECT(not_callable.is_error());
    EXPECT_EQ(error_name_of(vm, not_callable.error_value()), "TypeError"sv);
    auto object_not_callable = call(vm, MUST(evaluate(vm, realm, "({})"sv)), js_undefined(), Value(1));
    EXPECT_EQ(error_name_of(vm, object_not_callable.error_value()), "TypeError"sv);
}

TEST_CASE(construction)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto point = MUST(evaluate(vm, realm, "class Point { constructor(x, y) { this.x = x; this.y = y; this.target = new.target; } }; Point"sv));
    auto& point_class = point.as_function();
    EXPECT(point.is_constructor());

    auto constructed = MUST(construct(vm, point_class, Value(1), Value(2)));
    Value constructed_value = constructed;
    EXPECT_EQ(property_of(vm, constructed_value, "x"sv).as_double(), 1.0);
    EXPECT_EQ(property_of(vm, constructed_value, "y"sv).as_double(), 2.0);
    EXPECT(same_value(property_of(vm, constructed_value, "target"sv), point));
    EXPECT(MUST(call(vm, MUST(evaluate(vm, realm, "(object => object instanceof Point)"sv)), js_undefined(), constructed_value)).to_boolean());

    auto without_arguments = MUST(construct(vm, point_class));
    EXPECT(property_of(vm, without_arguments, "x"sv).is_undefined());

    // A new target other than the constructor itself is new.target, and its prototype is the new object's.
    auto derived = MUST(evaluate(vm, realm, "class Derived extends Point {}; Derived"sv));
    Value span_arguments[] = { Value(3), Value(4) };
    auto with_new_target = MUST(construct(vm, point_class, ReadonlySpan<Value> { span_arguments }, &derived.as_function()));
    EXPECT(same_value(property_of(vm, with_new_target, "target"sv), derived));
    EXPECT(MUST(call(vm, MUST(evaluate(vm, realm, "(object => Object.getPrototypeOf(object) === Derived.prototype)"sv)), js_undefined(), Value(with_new_target))).to_boolean());
    auto from_mutable_span = MUST(construct(vm, point_class, Span<Value> { span_arguments }));
    EXPECT_EQ(property_of(vm, from_mutable_span, "y"sv).as_double(), 4.0);

    auto array_constructor = MUST(evaluate(vm, realm, "Array"sv));
    auto array = MUST(construct(vm, array_constructor.as_function(), Value(3)));
    EXPECT(MUST(Value(array).is_array(vm)));
    EXPECT_EQ(MUST(length_of_array_like(vm, *array)), 3u);

    auto throwing_constructor = MUST(evaluate(vm, realm, "(function () { throw new RangeError('no'); })"sv));
    auto thrown = construct(vm, throwing_constructor.as_function());
    EXPECT(thrown.is_error());
    EXPECT_EQ(error_name_of(vm, thrown.error_value()), "RangeError"sv);
    EXPECT_EQ(string_property_of(vm, thrown.error_value(), "message"sv), "no"sv);
}

TEST_CASE(length_of_array_likes)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto length_of = [&](StringView source) {
        auto object = MUST(evaluate(vm, realm, source));
        return length_of_array_like(vm, object.as_object());
    };

    EXPECT_EQ(MUST(length_of("[1, 2, 3]"sv)), 3u);
    EXPECT_EQ(MUST(length_of("var holey = [1, 2]; holey[9] = 0; holey"sv)), 10u);
    EXPECT_EQ(MUST(length_of("({ length: '4' })"sv)), 4u);
    EXPECT_EQ(MUST(length_of("({ length: -1 })"sv)), 0u);
    EXPECT_EQ(MUST(length_of("({ length: 2.9 })"sv)), 2u);
    EXPECT_EQ(MUST(length_of("({ length: Infinity })"sv)), 9007199254740991u);
    EXPECT_EQ(MUST(length_of("({})"sv)), 0u);
    EXPECT_EQ(MUST(length_of("(function (a, b) {})"sv)), 2u);

    auto throwing_getter = length_of("({ get length() { throw new SyntaxError('length'); } })"sv);
    EXPECT(throwing_getter.is_error());
    EXPECT_EQ(error_name_of(vm, throwing_getter.error_value()), "SyntaxError"sv);
    auto symbol_length = length_of("({ length: Symbol() })"sv);
    EXPECT_EQ(error_name_of(vm, symbol_length.error_value()), "TypeError"sv);
}

TEST_CASE(function_realms_and_weak_references)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto function = MUST(evaluate(vm, realm, "(function () {})"sv));
    EXPECT_EQ(MUST(get_function_realm(vm, function.as_function())), &realm);
    auto bound = MUST(evaluate(vm, realm, "(function () {}).bind(null)"sv));
    EXPECT_EQ(MUST(get_function_realm(vm, bound.as_function())), &realm);
    auto proxy = MUST(evaluate(vm, realm, "new Proxy(function () {}, {})"sv));
    EXPECT_EQ(MUST(get_function_realm(vm, proxy.as_function())), &realm);

    auto revoked = MUST(evaluate(vm, realm, "var revocable = Proxy.revocable(function () {}, {}); revocable.revoke(); revocable.proxy"sv));
    auto from_revoked = get_function_realm(vm, revoked.as_function());
    EXPECT(from_revoked.is_error());
    EXPECT_EQ(error_name_of(vm, from_revoked.error_value()), "TypeError"sv);

    EXPECT(can_be_held_weakly(MUST(evaluate(vm, realm, "({})"sv))));
    EXPECT(can_be_held_weakly(Symbol::create(vm, "unique"_utf16)));
    EXPECT(can_be_held_weakly(MUST(evaluate(vm, realm, "Symbol.iterator"sv))));
    EXPECT(!can_be_held_weakly(MUST(evaluate(vm, realm, "Symbol.for('registered')"sv))));
    EXPECT(!can_be_held_weakly(Value(1)));
    EXPECT(!can_be_held_weakly(string_value(vm, "string"sv)));
    EXPECT(!can_be_held_weakly(js_undefined()));

    auto object = MUST(evaluate(vm, realm, "({ answer: 42 })"sv));
    auto environment = new_object_environment(object.as_object(), true, nullptr);
    EXPECT(environment.ptr());
}

TEST_CASE(modulo_of_numbers_and_big_integers)
{
    EXPECT_EQ(modulo(7, 3), 1);
    EXPECT_EQ(modulo(-7, 3), 2);
    EXPECT_EQ(modulo(7, -3), -2);
    EXPECT_EQ(modulo(-7.5, 2.0), 0.5);
    EXPECT_EQ(modulo(7.5, 2), 1.5);
    EXPECT_EQ(modulo(Crypto::SignedBigInteger { -7 }, Crypto::SignedBigInteger { 3 }), Crypto::SignedBigInteger { 2 });
    EXPECT_EQ(modulo(Crypto::SignedBigInteger { 7 }, Crypto::SignedBigInteger { 3 }), Crypto::SignedBigInteger { 1 });
}

static Vector<double> numbers_of(GC::RootVector<Value> const& values)
{
    Vector<double> numbers;
    for (auto value : values)
        numbers.append(value.as_double());
    return numbers;
}

TEST_CASE(iterators_over_arrays)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto array = MUST(evaluate(vm, realm, "[1, 2, 3]"sv));
    auto iterator_record = MUST(get_iterator(vm, array, IteratorHint::Sync));
    EXPECT(!iterator_record->done);
    EXPECT(iterator_record->iterator);
    EXPECT(iterator_record->next_method.is_function());

    EXPECT_EQ(MUST(iterator_step_value(vm, iterator_record))->as_double(), 1.0);
    EXPECT_EQ(MUST(iterator_step_value(vm, *iterator_record))->as_double(), 2.0);
    EXPECT_EQ(MUST(iterator_step_value(vm, iterator_record))->as_double(), 3.0);
    EXPECT(!MUST(iterator_step_value(vm, iterator_record)).has_value());
    EXPECT(iterator_record->done);

    auto listed = MUST(iterator_to_list(vm, MUST(get_iterator(vm, array, IteratorHint::Sync))));
    EXPECT_EQ(numbers_of(listed), (Vector<double> { 1, 2, 3 }));

    // The method of GetIteratorFromMethod is the one the caller found, here the array's own @@iterator.
    auto iterator_method = MUST(array.get_method(vm, vm.well_known_symbol_iterator()));
    EXPECT(iterator_method);
    auto from_method = MUST(get_iterator_from_method(vm, array, *iterator_method));
    EXPECT_EQ(numbers_of(MUST(iterator_to_list(vm, from_method))), (Vector<double> { 1, 2, 3 }));
    EXPECT(from_method->done);

    auto values_method = MUST(evaluate(vm, realm, "Array.prototype.keys"sv));
    auto keys = MUST(get_iterator_from_method(vm, array, values_method.as_function()));
    EXPECT_EQ(numbers_of(MUST(iterator_to_list(vm, keys))), (Vector<double> { 0, 1, 2 }));

    // A string iterates its code points.
    auto string_record = MUST(get_iterator(vm, string_value(vm, "a\U0001F600"sv), IteratorHint::Sync));
    EXPECT_EQ(MUST(MUST(iterator_step_value(vm, string_record))->to_utf16_string(vm)), "a"sv);
    EXPECT_EQ(MUST(iterator_step_value(vm, string_record))->as_string().length_in_utf16_code_units(), 2u);
    EXPECT(!MUST(iterator_step_value(vm, string_record)).has_value());

    auto not_iterable = get_iterator(vm, Value(5), IteratorHint::Sync);
    EXPECT(not_iterable.is_error());
    EXPECT_EQ(error_name_of(vm, not_iterable.error_value()), "TypeError"sv);
    auto object_not_iterable = get_iterator(vm, MUST(evaluate(vm, realm, "({})"sv)), IteratorHint::Sync);
    EXPECT_EQ(error_name_of(vm, object_not_iterable.error_value()), "TypeError"sv);
}

TEST_CASE(iterators_over_generators)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    MUST(evaluate(vm, realm, R"~~~(
        var log = [];
        function* generator() {
            try {
                const sent = yield 1;
                log.push("sent " + sent);
                yield 2;
                yield 3;
            } finally {
                log.push("closed");
            }
        }
    )~~~"sv));

    auto generator = MUST(evaluate(vm, realm, "generator()"sv));
    auto iterator_record = MUST(get_iterator(vm, generator, IteratorHint::Sync));
    EXPECT_EQ(iterator_record->iterator.ptr(), &generator.as_object());

    EXPECT_EQ(MUST(iterator_step_value(vm, iterator_record))->as_double(), 1.0);

    // IteratorNext passes its value to the generator, and the result object is the generator's own.
    auto result = MUST(iterator_next(vm, iterator_record, Value(42)));
    EXPECT(!MUST(iterator_complete(vm, result)));
    EXPECT_EQ(MUST(iterator_value(vm, result)).as_double(), 2.0);
    EXPECT_EQ(MUST(MUST(evaluate(vm, realm, "log.join()"sv)).to_utf16_string(vm)), "sent 42"sv);

    // The generator's finally block runs when it returns, which the record does not see.
    auto return_method = MUST(Value(iterator_record->iterator).get_method(vm, vm.names.return_));
    auto returned = MUST(call(vm, *return_method, iterator_record->iterator, Value(9)));
    EXPECT(MUST(iterator_complete(vm, returned.as_object())));
    EXPECT_EQ(MUST(iterator_value(vm, returned.as_object())).as_double(), 9.0);
    EXPECT_EQ(MUST(MUST(evaluate(vm, realm, "log.join()"sv)).to_utf16_string(vm)), "sent 42,closed"sv);
    EXPECT(!iterator_record->done);

    // The returned generator is done, and stepping it marks the record done.
    auto after_return = MUST(iterator_next(vm, iterator_record));
    EXPECT(MUST(iterator_complete(vm, after_return)));
    EXPECT(MUST(iterator_value(vm, after_return)).is_undefined());
    EXPECT(!iterator_record->done);
    EXPECT(!MUST(iterator_step_value(vm, iterator_record)).has_value());
    EXPECT(iterator_record->done);

    // A generator that throws midway passes the throw on, and the record is done.
    auto throwing = MUST(evaluate(vm, realm, "(function* () { yield 1; throw new EvalError('midway'); })()"sv));
    auto throwing_record = MUST(get_iterator(vm, throwing, IteratorHint::Sync));
    auto to_list = iterator_to_list(vm, throwing_record);
    EXPECT(to_list.is_error());
    EXPECT_EQ(error_name_of(vm, to_list.error_value()), "EvalError"sv);
    EXPECT(throwing_record->done);
}

static ThrowCompletionOr<double> sum_of(VM& vm, IteratorRecord& iterator_record)
{
    double sum = 0;
    while (true) {
        auto next = TRY(iterator_step_value(vm, iterator_record));
        if (!next.has_value())
            return sum;
        sum += TRY(next->to_double(vm));
    }
}

TEST_CASE(iterators_over_custom_iterables)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    MUST(evaluate(vm, realm, R"~~~(
        var events = [];
        function counter(values) {
            return {
                [Symbol.iterator]() {
                    events.push("iterator");
                    let index = 0;
                    return {
                        next(sent) {
                            events.push("next " + sent);
                            if (index < values.length)
                                return { value: values[index++], done: false };
                            return { value: "ignored", done: true };
                        },
                        return() {
                            events.push("return");
                            return {};
                        },
                    };
                },
            };
        }
    )~~~"sv));
    auto events = [&] { return MUST(MUST(evaluate(vm, realm, "events.join()"sv)).to_utf16_string(vm)); };

    auto counter = MUST(evaluate(vm, realm, "counter([1, 2, 3])"sv));
    auto iterator_record = MUST(get_iterator(vm, counter, IteratorHint::Sync));
    EXPECT_EQ(MUST(sum_of(vm, iterator_record)), 6.0);
    EXPECT_EQ(events(), "iterator,next undefined,next undefined,next undefined,next undefined"sv);
    EXPECT(iterator_record->done);

    // A step's work that throws leaves the iterator open, as the record's user decides whether to close it.
    MUST(evaluate(vm, realm, "events = []"sv));
    auto with_symbol = MUST(get_iterator(vm, MUST(evaluate(vm, realm, "counter([1, Symbol()])"sv)), IteratorHint::Sync));
    auto symbol_sum = sum_of(vm, with_symbol);
    EXPECT(symbol_sum.is_error());
    EXPECT_EQ(error_name_of(vm, symbol_sum.error_value()), "TypeError"sv);
    EXPECT_EQ(events(), "iterator,next undefined,next undefined"sv);
    EXPECT(!with_symbol->done);

    // The value the result object has when it is done is not a step's value.
    auto done_result_record = MUST(get_iterator(vm, MUST(evaluate(vm, realm, "counter([])"sv)), IteratorHint::Sync));
    auto done_result = MUST(iterator_next(vm, done_result_record, string_value(vm, "sent"sv)));
    EXPECT(MUST(iterator_complete(vm, done_result)));
    EXPECT_EQ(MUST(MUST(iterator_value(vm, done_result)).to_utf16_string(vm)), "ignored"sv);

    // A next method that returns something other than an object throws a TypeError and leaves the record done.
    auto bad_next = MUST(evaluate(vm, realm, "({ [Symbol.iterator]() { return { next() { return 1; } }; } })"sv));
    auto bad_next_record = MUST(get_iterator(vm, bad_next, IteratorHint::Sync));
    auto bad_step = iterator_step_value(vm, bad_next_record);
    EXPECT(bad_step.is_error());
    EXPECT_EQ(error_name_of(vm, bad_step.error_value()), "TypeError"sv);
    EXPECT(bad_next_record->done);

    // A next method that throws leaves the record done.
    auto throwing_next = MUST(evaluate(vm, realm, "({ [Symbol.iterator]() { return { next() { throw 'next'; } }; } })"sv));
    auto throwing_next_record = MUST(get_iterator(vm, throwing_next, IteratorHint::Sync));
    auto throwing_step = iterator_next(vm, throwing_next_record);
    EXPECT_EQ(MUST(throwing_step.error_value().to_utf16_string(vm)), "next"sv);
    EXPECT(throwing_next_record->done);

    // An @@iterator method that returns something other than an object throws a TypeError.
    auto bad_iterator = get_iterator(vm, MUST(evaluate(vm, realm, "({ [Symbol.iterator]() { return 1; } })"sv)), IteratorHint::Sync);
    EXPECT_EQ(error_name_of(vm, bad_iterator.error_value()), "TypeError"sv);

    // A result without a "value" steps to undefined, and the iterator's other methods are the record's iterator's.
    auto with_return = MUST(evaluate(vm, realm, "({ [Symbol.iterator]() { return { next() { return { done: false }; }, return() { return 1; } }; } })"sv));
    auto with_return_record = MUST(get_iterator(vm, with_return, IteratorHint::Sync));
    EXPECT(MUST(iterator_step_value(vm, with_return_record))->is_undefined());
    auto return_method = MUST(Value(with_return_record->iterator).get_method(vm, vm.names.return_));
    EXPECT(return_method);
    EXPECT_EQ(MUST(call(vm, *return_method, with_return_record->iterator)).as_double(), 1.0);
}

TEST_CASE(async_iterators)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    // An object with @@asyncIterator iterates with it.
    auto async_iterable = MUST(evaluate(vm, realm, "var asyncIterator = { next() { return Promise.resolve({ value: 'async', done: false }); } }; ({ [Symbol.asyncIterator]() { return asyncIterator; } })"sv));
    auto async_record = MUST(get_iterator(vm, async_iterable, IteratorHint::Async));
    EXPECT(same_value(async_record->iterator, MUST(evaluate(vm, realm, "asyncIterator"sv))));
    auto async_result = MUST(iterator_next(vm, async_record));
    EXPECT(same_value(MUST(call(vm, MUST(evaluate(vm, realm, "(result => result instanceof Promise)"sv)), js_undefined(), Value(async_result))), Value(true)));

    // A sync iterable without @@asyncIterator is wrapped, so its steps are promises of results.
    auto array = MUST(evaluate(vm, realm, "[5]"sv));
    auto wrapped_record = MUST(get_iterator(vm, array, IteratorHint::Async));
    EXPECT(!same_value(wrapped_record->iterator, array));
    auto promise = MUST(iterator_next(vm, wrapped_record));
    EXPECT(same_value(MUST(call(vm, MUST(evaluate(vm, realm, "(result => result instanceof Promise)"sv)), js_undefined(), Value(promise))), Value(true)));

    auto not_async_iterable = get_iterator(vm, Value(1), IteratorHint::Async);
    EXPECT_EQ(error_name_of(vm, not_async_iterable.error_value()), "TypeError"sv);
}

TEST_CASE(iterator_result_objects)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto from_realm = create_iterator_result_object(realm, Value(1), false);
    EXPECT(!MUST(iterator_complete(vm, from_realm)));
    EXPECT_EQ(MUST(iterator_value(vm, from_realm)).as_double(), 1.0);

    auto from_vm = create_iterator_result_object(vm, string_value(vm, "last"sv), true);
    EXPECT(MUST(iterator_complete(vm, from_vm)));
    EXPECT_EQ(MUST(MUST(iterator_value(vm, from_vm)).to_utf16_string(vm)), "last"sv);

    auto describe = MUST(evaluate(vm, realm, "(result => Object.getPrototypeOf(result) === Object.prototype && Object.keys(result).join())"sv));
    EXPECT_EQ(MUST(MUST(call(vm, describe, js_undefined(), Value(from_vm))).to_utf16_string(vm)), "value,done"sv);

    // IteratorComplete and IteratorValue read the properties of any object, getters included.
    auto with_getters = MUST(evaluate(vm, realm, "({ get done() { return 1; }, get value() { throw new URIError('value'); } })"sv));
    EXPECT(MUST(iterator_complete(vm, with_getters.as_object())));
    auto throwing_value = iterator_value(vm, with_getters.as_object());
    EXPECT_EQ(error_name_of(vm, throwing_value.error_value()), "URIError"sv);
}

TEST_CASE(json)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto text = Utf16String::from_utf8(R"~~~({"a":[1,2,{"b":null}],"c":"α","d":true})~~~"sv);
    auto parsed = MUST(JSONObject::parse_json(vm, text.utf16_view()));
    EXPECT(parsed.is_object());
    EXPECT(MUST(property_of(vm, parsed, "a"sv).is_array(vm)));
    EXPECT(property_of(vm, property_of(vm, property_of(vm, parsed, "a"sv), "2"sv), "b"sv).is_null());
    EXPECT_EQ(string_property_of(vm, parsed, "c"sv), Utf16String::from_utf8("α"sv));
    EXPECT(property_of(vm, parsed, "d"sv).as_bool());

    EXPECT_EQ(MUST(JSONObject::parse_json(vm, u"12.5"sv)).as_double(), 12.5);
    EXPECT_EQ(MUST(MUST(JSONObject::parse_json(vm, u"\"text\""sv)).to_utf16_string(vm)), "text"sv);

    auto malformed = JSONObject::parse_json(vm, u"{\"a\":}"sv);
    EXPECT(malformed.is_error());
    EXPECT_EQ(error_name_of(vm, malformed.error_value()), "SyntaxError"sv);

    auto stringified = MUST(JSONObject::stringify_impl(vm, parsed, js_undefined(), js_undefined()));
    EXPECT(stringified.has_value());
    EXPECT_EQ(stringified.value(), Utf16String::from_utf8(R"~~~({"a":[1,2,{"b":null}],"c":"α","d":true})~~~"sv));

    auto indented = MUST(JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "({ a: [1] })"sv)), js_undefined(), Value(2)));
    EXPECT_EQ(indented.value(), "{\n  \"a\": [\n    1\n  ]\n}"sv);
    auto with_string_space = MUST(JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "({ a: 1 })"sv)), js_undefined(), string_value(vm, "--"sv)));
    EXPECT_EQ(with_string_space.value(), "{\n--\"a\": 1\n}"sv);

    auto doubling_replacer = MUST(evaluate(vm, realm, "((key, value) => typeof value === 'number' ? value * 2 : value)"sv));
    EXPECT_EQ(MUST(JSONObject::stringify_impl(vm, parsed, doubling_replacer, js_undefined())).value(), Utf16String::from_utf8(R"~~~({"a":[2,4,{"b":null}],"c":"α","d":true})~~~"sv));
    auto key_list = MUST(evaluate(vm, realm, "['d']"sv));
    EXPECT_EQ(MUST(JSONObject::stringify_impl(vm, parsed, key_list, js_undefined())).value(), "{\"d\":true}"sv);

    EXPECT(!MUST(JSONObject::stringify_impl(vm, js_undefined(), js_undefined(), js_undefined())).has_value());
    EXPECT(!MUST(JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "(function () {})"sv)), js_undefined(), js_undefined())).has_value());
    EXPECT_EQ(MUST(JSONObject::stringify_impl(vm, string_value(vm, "quote\""sv), js_undefined(), js_undefined())).value(), "\"quote\\\"\""sv);
    EXPECT_EQ(MUST(JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "({ toJSON() { return 'custom'; } })"sv)), js_undefined(), js_undefined())).value(), "\"custom\""sv);

    auto throwing_to_json = JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "({ toJSON() { throw new ReferenceError('toJSON'); } })"sv)), js_undefined(), js_undefined());
    EXPECT_EQ(error_name_of(vm, throwing_to_json.error_value()), "ReferenceError"sv);
    auto cyclic = JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "var cyclic = {}; cyclic.self = cyclic; cyclic"sv)), js_undefined(), js_undefined());
    EXPECT_EQ(error_name_of(vm, cyclic.error_value()), "TypeError"sv);
    auto bigint = JSONObject::stringify_impl(vm, MUST(evaluate(vm, realm, "1n"sv)), js_undefined(), js_undefined());
    EXPECT_EQ(error_name_of(vm, bigint.error_value()), "TypeError"sv);
}

TEST_CASE(conversions_that_throw)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto symbol = Symbol::create(vm, "symbol"_utf16);
    EXPECT_EQ(error_name_of(vm, Value(symbol).to_utf16_string(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(symbol).to_number(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(symbol).to_double(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(symbol).to_i32(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(symbol).to_bigint(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, js_null().to_object(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(1.5).to_bigint(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, string_value(vm, "1.5"sv).to_bigint(vm).error_value()), "SyntaxError"sv);
    EXPECT_EQ(error_name_of(vm, Value(-1).to_index(vm).error_value()), "RangeError"sv);
    EXPECT_EQ(error_name_of(vm, Value(9007199254740992.0).to_index(vm).error_value()), "RangeError"sv);
    EXPECT_EQ(error_name_of(vm, MUST(evaluate(vm, realm, "1n"sv)).to_number(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, MUST(evaluate(vm, realm, "1n"sv)).to_u32(vm).error_value()), "TypeError"sv);
    EXPECT_EQ(error_name_of(vm, js_undefined().get(vm, vm.names.length).error_value()), "TypeError"sv);

    // ToPrimitive runs the object's own conversions, and their throws reach the caller unchanged.
    auto throwing_value_of = MUST(evaluate(vm, realm, "({ valueOf() { throw 'valueOf'; }, toString() { throw 'toString'; } })"sv));
    EXPECT_EQ(MUST(throwing_value_of.to_number(vm).error_value().to_utf16_string(vm)), "valueOf"sv);
    EXPECT_EQ(MUST(throwing_value_of.to_u16(vm).error_value().to_utf16_string(vm)), "valueOf"sv);
    EXPECT_EQ(MUST(throwing_value_of.to_length(vm).error_value().to_utf16_string(vm)), "valueOf"sv);
    EXPECT_EQ(MUST(throwing_value_of.to_utf16_string(vm).error_value().to_utf16_string(vm)), "toString"sv);
    EXPECT_EQ(MUST(PropertyKey::from_value(vm, throwing_value_of).error_value().to_utf16_string(vm)), "toString"sv);
    EXPECT_EQ(MUST(throwing_value_of.to_bigint_int64(vm).error_value().to_utf16_string(vm)), "valueOf"sv);
    EXPECT_EQ(MUST(throwing_value_of.to_bigint_uint64(vm).error_value().to_utf16_string(vm)), "valueOf"sv);

    auto primitive_from_object = MUST(evaluate(vm, realm, "({ valueOf() { return 6; }, toString() { return 'six'; }, [Symbol.toPrimitive]: undefined })"sv));
    EXPECT_EQ(MUST(primitive_from_object.to_i32(vm)), 6);
    EXPECT_EQ(MUST(primitive_from_object.to_u8(vm)), 6u);
    EXPECT_EQ(MUST(primitive_from_object.to_utf16_string(vm)), "six"sv);
    EXPECT_EQ(MUST(PropertyKey::from_value(vm, primitive_from_object)).as_string(), "six"_utf16_fly_string);

    auto bad_to_primitive = MUST(evaluate(vm, realm, "({ [Symbol.toPrimitive]() { return {}; } })"sv));
    EXPECT_EQ(error_name_of(vm, bad_to_primitive.to_number(vm).error_value()), "TypeError"sv);

    // The getters that a property read runs throw through it.
    auto throwing_getter = MUST(evaluate(vm, realm, "({ get value() { throw new RangeError('getter'); } })"sv));
    EXPECT_EQ(error_name_of(vm, throwing_getter.get(vm, vm.names.value).error_value()), "RangeError"sv);
    auto get_method_of_non_function = MUST(evaluate(vm, realm, "({ method: 1 })"sv)).get_method(vm, PropertyKey { "method"_utf16_fly_string });
    EXPECT_EQ(error_name_of(vm, get_method_of_non_function.error_value()), "TypeError"sv);

    auto revoked_proxy = MUST(evaluate(vm, realm, "var revocable_array = Proxy.revocable([], {}); revocable_array.revoke(); revocable_array.proxy"sv));
    EXPECT_EQ(error_name_of(vm, revoked_proxy.is_array(vm).error_value()), "TypeError"sv);
    EXPECT(MUST(MUST(evaluate(vm, realm, "new Proxy([], {})"sv)).is_array(vm)));
}

static ThrowCompletionOr<double> doubled(VM& vm, Value value)
{
    auto number = TRY(value.to_double(vm));
    return number * 2;
}

static ThrowCompletionOr<void> throws_if_negative(VM& vm, double number)
{
    if (number < 0)
        return vm.throw_completion<RangeError>(ErrorType::InvalidLength, "array");
    return {};
}

TEST_CASE(completions)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    auto normal = normal_completion(Value(1));
    EXPECT_EQ(normal.type(), Completion::Type::Normal);
    EXPECT(!normal.is_abrupt());
    EXPECT(!normal.is_error());
    EXPECT_EQ(normal.value().as_double(), 1.0);

    Completion implicit = Value(2);
    EXPECT_EQ(implicit.type(), Completion::Type::Normal);
    Completion default_completion;
    EXPECT(default_completion.value().is_undefined());

    auto thrown = throw_completion(string_value(vm, "thrown"sv));
    EXPECT_EQ(thrown.type(), Completion::Type::Throw);
    EXPECT(thrown.is_abrupt());
    EXPECT(thrown.is_error());
    EXPECT_EQ(MUST(thrown.release_error().value().to_utf16_string(vm)), "thrown"sv);

    ThrowCompletionOr<Value> value_or_throw = thrown;
    EXPECT(value_or_throw.is_throw_completion());
    Completion from_value_or_throw { value_or_throw };
    EXPECT_EQ(from_value_or_throw.type(), Completion::Type::Throw);
    Completion from_value { ThrowCompletionOr<Value> { Value(3) } };
    EXPECT_EQ(from_value.type(), Completion::Type::Normal);
    EXPECT_EQ(from_value.value().as_double(), 3.0);

    Optional<Completion> no_completion;
    EXPECT(!no_completion.has_value());
    no_completion = normal;
    EXPECT(no_completion.has_value());

    EXPECT_EQ(MUST(doubled(vm, Value(4))), 8.0);
    auto doubled_symbol = doubled(vm, Symbol::create(vm));
    EXPECT(doubled_symbol.is_error());
    EXPECT_EQ(error_name_of(vm, doubled_symbol.release_error().value()), "TypeError"sv);

    EXPECT(!throws_if_negative(vm, 1).is_error());
    auto negative = throws_if_negative(vm, -1);
    EXPECT(negative.is_throw_completion());
    EXPECT_EQ(error_name_of(vm, negative.error_value()), "RangeError"sv);
    EXPECT_EQ(string_property_of(vm, negative.error_value(), "message"sv), "Invalid array length"sv);

    // Errors that C++ throws are created in the current realm, with the message of their error type.
    auto type_error = vm.throw_completion<TypeError>(ErrorType::NotAFunction, "thing");
    EXPECT_EQ(type_error.type(), Completion::Type::Throw);
    EXPECT_EQ(error_name_of(vm, type_error.value()), "TypeError"sv);
    EXPECT_EQ(string_property_of(vm, type_error.value(), "message"sv), "thing is not a function"sv);
    auto is_type_error = MUST(evaluate(vm, realm, "(error => error instanceof TypeError && error.stack !== undefined)"sv));
    EXPECT(MUST(call(vm, is_type_error, js_undefined(), type_error.value())).to_boolean());

    // A thrown value that JavaScript catches is the value C++ threw.
    auto catcher = MUST(evaluate(vm, realm, "(function (callback) { try { callback(); } catch (error) { return error; } })"sv));
    auto native_thrower = MUST(evaluate(vm, realm, "(function () { throw 'from JS'; })"sv));
    EXPECT_EQ(MUST(MUST(call(vm, catcher, js_undefined(), native_thrower)).to_utf16_string(vm)), "from JS"sv);
}

#ifndef AK_OS_WINDOWS

namespace {

// Collects what the process writes to its standard error while it is alive.
class StandardErrorCapture {
public:
    StandardErrorCapture()
    {
        VERIFY(pipe(m_pipe) == 0);
        VERIFY(fcntl(m_pipe[0], F_SETFL, O_NONBLOCK) == 0);
        m_saved_standard_error = dup(STDERR_FILENO);
        VERIFY(m_saved_standard_error >= 0);
        VERIFY(dup2(m_pipe[1], STDERR_FILENO) >= 0);
    }

    ~StandardErrorCapture()
    {
        finish();
    }

    ByteString finish()
    {
        if (m_saved_standard_error >= 0) {
            dup2(m_saved_standard_error, STDERR_FILENO);
            close(m_saved_standard_error);
            m_saved_standard_error = -1;
            close(m_pipe[1]);

            char buffer[4096];
            while (true) {
                auto read_size = read(m_pipe[0], buffer, sizeof(buffer));
                if (read_size <= 0)
                    break;
                m_output.append(buffer, read_size);
            }
            close(m_pipe[0]);
        }
        return m_output.to_byte_string();
    }

private:
    int m_pipe[2] {};
    int m_saved_standard_error { -1 };
    StringBuilder m_output;
};

}

TEST_CASE(logging_all_exceptions)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    ByteString output;
    {
        StandardErrorCapture capture;
        set_log_all_js_exceptions(true);
        (void)evaluate(vm, realm, "function thrower() { throw new TypeError('first failure'); }\nthrower();"sv);
        (void)evaluate(vm, realm, "null.second_failure"sv);
        (void)evaluate(vm, realm, "throw 'third failure'"sv);
        (void)vm.throw_completion<TypeError>("fourth failure"_utf16);
        (void)throw_completion(string_value(vm, "fifth failure"sv));
        set_log_all_js_exceptions(false);
        (void)evaluate(vm, realm, "throw 'unlogged failure'"sv);
        (void)throw_completion(string_value(vm, "unlogged C++ failure"sv));
        output = capture.finish();
    }

    // A thrown object shows its message and the call stack, and other values show alone. A throw is logged where the
    // engine or C++ code creates it, and again each time it leaves the bytecode of a function or a script.
    EXPECT_EQ(output, "\033[31;1mTHROW!\033[0m first failure\n"
                      "-> thrower @ operations.js:1,22\n"
                      "->  @ operations.js:2,8\n"
                      "-> \n"
                      "\033[31;1mTHROW!\033[0m first failure\n"
                      "->  @ operations.js:2,8\n"
                      "-> \n"
                      "\033[31;1mTHROW!\033[0m Cannot access property \"second_failure\" on null object\n"
                      "->  @ operations.js:1,5\n"
                      "-> \n"
                      "\033[31;1mTHROW!\033[0m Cannot access property \"second_failure\" on null object\n"
                      "->  @ operations.js:1,5\n"
                      "-> \n"
                      "\033[31;1mTHROW!\033[0m third failure\n"
                      "\033[31;1mTHROW!\033[0m fourth failure\n"
                      "-> \n"
                      "\033[31;1mTHROW!\033[0m fifth failure\n"sv);
}

static ByteString exception_log_of_script(VM& vm, Realm& realm, StringView source)
{
    StandardErrorCapture capture;
    set_log_all_js_exceptions(true);
    (void)evaluate(vm, realm, source);
    set_log_all_js_exceptions(false);
    return capture.finish();
}

TEST_CASE(logging_messages_that_getters_return)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    // Exceptions such as DOMException have their message on a built-in accessor of their prototype.
    EXPECT_EQ(exception_log_of_script(vm, realm, "var regexp = /from a built-in getter/;\n"
                                                 "var source_getter = Object.getOwnPropertyDescriptor(RegExp.prototype, 'source').get;\n"
                                                 "Object.defineProperty(regexp, 'message', { get: source_getter });\n"
                                                 "throw regexp;"sv),
        "\033[31;1mTHROW!\033[0m from a built-in getter\n"
        "->  @ operations.js:4,1\n"
        "-> \n"sv);

    EXPECT_EQ(exception_log_of_script(vm, realm, "class Failure { get message() { return 'from a getter'; } }\n"
                                                 "throw new Failure();"sv),
        "\033[31;1mTHROW!\033[0m from a getter\n"
        "->  @ operations.js:2,1\n"
        "-> \n"sv);
}

TEST_CASE(logging_values_that_the_runtime_throws_again)
{
    TestVM test_vm;
    auto& vm = *test_vm.vm;
    auto& realm = *test_vm.realm;

    // A rejection is logged where it resumes an async function, whether the function suspended or not, and where it
    // resumes an async generator, which then throws it out of its bytecode again.
    EXPECT_EQ(exception_log_of_script(vm, realm, "(async function awaiter() {\n"
                                                 "    try { await Promise.reject(new Error('awaited')); } catch {}\n"
                                                 "})();"sv),
        "\033[31;1mTHROW!\033[0m awaited\n"
        "-> awaiter @ operations.js:0,0\n"
        "->  @ operations.js:3,3\n"
        "-> \n"sv);
    EXPECT_EQ(exception_log_of_script(vm, realm, "(async function awaiter() {\n"
                                                 "    await null;\n"
                                                 "    try { await Promise.reject(new Error('awaited without suspending')); } catch {}\n"
                                                 "})();"sv),
        "\033[31;1mTHROW!\033[0m awaited without suspending\n"
        "-> awaiter @ operations.js:0,0\n"
        "->  @ operations.js:4,3\n"
        "-> \n"sv);
    auto const awaited_in_a_generator_log = "\033[31;1mTHROW!\033[0m awaited in a generator\n"
                                            "-> generator @ operations.js:1,31\n"
                                            "-> \n"
                                            "->  @ operations.js:2,25\n"
                                            "-> \n"sv;
    EXPECT_EQ(exception_log_of_script(vm, realm, "async function* generator() { await Promise.reject(new Error('awaited in a generator')); }\n"
                                                 "generator().next().catch(() => {});"sv),
        ByteString::formatted("{}{}", awaited_in_a_generator_log, awaited_in_a_generator_log));

    // So is a rejection that an async generator returns, and one that for await reads from a sync iterator, which
    // closes the iterator with it before it resumes the async function.
    EXPECT_EQ(exception_log_of_script(vm, realm, "async function* generator() {}\n"
                                                 "generator().return(Promise.reject(new Error('returned'))).catch(() => {});"sv),
        "\033[31;1mTHROW!\033[0m returned\n"
        "-> \n"
        "->  @ operations.js:2,64\n"
        "-> \n"sv);
    EXPECT_EQ(exception_log_of_script(vm, realm, "(async function () {\n"
                                                 "    try { for await (var value of [Promise.reject(new Error('iterated'))]) {} } catch {}\n"
                                                 "})();"sv),
        "\033[31;1mTHROW!\033[0m iterated\n"
        "-> \n"
        "->  @ operations.js:3,3\n"
        "-> \n"
        "\033[31;1mTHROW!\033[0m iterated\n"
        "->  @ operations.js:0,0\n"
        "-> \n"
        "->  @ operations.js:3,3\n"
        "-> \n"sv);

    // Promise reactions pass a rejection on as a throw where they have no handler for it, and Promise.any() of nothing
    // rejects with an AggregateError that it throws.
    EXPECT_EQ(exception_log_of_script(vm, realm, "Promise.reject(new Error('passed on')).then(() => {}).catch(() => {});"sv),
        "\033[31;1mTHROW!\033[0m passed on\n"
        "->  @ operations.js:1,60\n"
        "-> \n"sv);
    EXPECT_EQ(exception_log_of_script(vm, realm, "Promise.reject(new Error('through finally')).finally(() => {}).catch(() => {});"sv),
        "\033[31;1mTHROW!\033[0m through finally\n"
        "-> \n"
        "->  @ operations.js:1,69\n"
        "-> \n"sv);
    EXPECT_EQ(exception_log_of_script(vm, realm, "Promise.any([]).catch(() => {});"sv),
        "\033[31;1mTHROW!\033[0m \n"
        "-> \n"
        "->  @ operations.js:1,12\n"
        "-> \n"sv);

    // The throw() of generators and async generators throws its argument into the generator, which throws it out
    // again.
    EXPECT_EQ(exception_log_of_script(vm, realm, "function* generator() { yield 1; }\n"
                                                 "var iterator = generator();\n"
                                                 "iterator.next();\n"
                                                 "try { iterator.throw(new Error('thrown in')); } catch {}"sv),
        "\033[31;1mTHROW!\033[0m thrown in\n"
        "-> \n"
        "->  @ operations.js:4,21\n"
        "-> \n"
        "\033[31;1mTHROW!\033[0m thrown in\n"
        "-> generator @ operations.js:1,25\n"
        "-> \n"
        "->  @ operations.js:4,21\n"
        "-> \n"sv);

    EXPECT_EQ(exception_log_of_script(vm, realm, "async function* generator() { yield 1; }\n"
                                                 "var iterator = generator();\n"
                                                 "iterator.next();\n"
                                                 "iterator.throw(new Error('thrown into an async generator')).catch(() => {});"sv),
        "\033[31;1mTHROW!\033[0m thrown into an async generator\n"
        "-> \n"
        "->  @ operations.js:4,15\n"
        "-> \n"
        "\033[31;1mTHROW!\033[0m thrown into an async generator\n"
        "-> generator @ operations.js:1,31\n"
        "-> \n"
        "->  @ operations.js:4,66\n"
        "-> \n"sv);

    // Disposing resources throws a SuppressedError of two failures.
    EXPECT_EQ(exception_log_of_script(vm, realm, "var stack = new DisposableStack();\n"
                                                 "stack.defer(() => { throw 'first disposal'; });\n"
                                                 "stack.defer(() => { throw 'second disposal'; });\n"
                                                 "try { stack.dispose(); } catch {}"sv),
        "\033[31;1mTHROW!\033[0m second disposal\n"
        "\033[31;1mTHROW!\033[0m first disposal\n"
        "\033[31;1mTHROW!\033[0m \n"
        "-> \n"
        "->  @ operations.js:4,20\n"
        "-> \n"sv);
}

#endif
