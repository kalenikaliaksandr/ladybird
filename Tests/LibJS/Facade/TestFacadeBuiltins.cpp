/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibCrypto/BigInt/SignedBigInteger.h>
#include <LibGC/Function.h>
#include <LibGC/Root.h>
#include <LibJS/Runtime/BigInt.h>
#include <LibJS/Runtime/BigIntObject.h>
#include <LibJS/Runtime/BooleanObject.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/Date.h>
#include <LibJS/Runtime/Error.h>
#include <LibJS/Runtime/ErrorConstructor.h>
#include <LibJS/Runtime/ErrorData.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/FinalizationRegistry.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/JobCallback.h>
#include <LibJS/Runtime/Map.h>
#include <LibJS/Runtime/MapIterator.h>
#include <LibJS/Runtime/NumberObject.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/Promise.h>
#include <LibJS/Runtime/PromiseCapability.h>
#include <LibJS/Runtime/PromiseConstructor.h>
#include <LibJS/Runtime/PromiseJob.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/RegExpObject.h>
#include <LibJS/Runtime/Set.h>
#include <LibJS/Runtime/SetIterator.h>
#include <LibJS/Runtime/StringObject.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// The built-in objects that LibJS's users create and read from C++. The same expectations hold for the C++ runtime's
// LibJS and for the facade over the Rust one.

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

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source, StringView filename = "builtins.js"sv)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm, filename);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static Utf16String string_of(VM& vm, Value value)
{
    return MUST(value.to_utf16_string(vm));
}

static bool evaluates_to_true(VM& vm, Realm& realm, StringView source)
{
    auto result = evaluate(vm, realm, source);
    return !result.is_error() && result.value().is_boolean() && result.value().as_bool();
}

// The values that JavaScript reads as from_cpp.get(key), from a Map that it created itself, which keeps them alive.
static Map& values_from_cpp(VM& vm, Realm& realm)
{
    return as<Map>(MUST(evaluate(vm, realm, "globalThis.from_cpp ||= new Map()"sv)).as_object());
}

static void pass_to_javascript(VM& vm, Realm& realm, i32 key, Value value)
{
    values_from_cpp(vm, realm).map_set(Value(key), value);
}

// Has the promise jobs that the VM hands to the host queued in `queued_promise_jobs`, which keeps them alive.
static void queue_promise_jobs_in(VM& vm, Vector<GC::Root<GC::Function<void()>>>& queued_promise_jobs)
{
    vm.host_enqueue_promise_job = [&vm, &queued_promise_jobs](PromiseJob job, GC::Ptr<Realm>) {
        queued_promise_jobs.append(GC::make_root(GC::create_function(vm.heap(), [job = move(job)] {
            MUST(job.run());
        })));
    };
    vm.host_promise_job_queue_is_empty = [&queued_promise_jobs] { return queued_promise_jobs.is_empty(); };
}

static void run_queued_promise_jobs(Vector<GC::Root<GC::Function<void()>>>& queued_promise_jobs)
{
    while (!queued_promise_jobs.is_empty()) {
        auto job = queued_promise_jobs.take_first();
        job->function()();
    }
}

// Collects garbage once the stack below the caller no longer holds pointers left over from earlier calls, which the
// conservative scan would treat as roots.
static NEVER_INLINE void collect_garbage(VM& vm)
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    vm.heap().collect_garbage();
}

TEST_CASE(errors_of_each_kind)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto expect_error = [&](Object& error, StringView constructor_name, StringView name_and_message) {
        EXPECT(is<JS::Error>(error));
        EXPECT(same_value(property_of(vm, &error, "constructor"sv), MUST(evaluate(vm, realm, constructor_name))));
        EXPECT_EQ(string_of(vm, &error), Utf16String::from_utf8(name_and_message));
    };

    expect_error(JS::Error::create(realm), "Error"sv, "Error"sv);
    expect_error(JS::Error::create(realm, "owned"_utf16), "Error"sv, "Error: owned"sv);
    expect_error(JS::Error::create(realm, u"viewed"sv), "Error"sv, "Error: viewed"sv);
    expect_error(EvalError::create(realm), "EvalError"sv, "EvalError"sv);
    expect_error(InternalError::create(realm, "internal"_utf16), "InternalError"sv, "InternalError: internal"sv);
    expect_error(RangeError::create(realm, "range"_utf16), "RangeError"sv, "RangeError: range"sv);
    expect_error(ReferenceError::create(realm, u"reference"sv), "ReferenceError"sv, "ReferenceError: reference"sv);
    expect_error(SyntaxError::create(realm, "syntax"_utf16), "SyntaxError"sv, "SyntaxError: syntax"sv);
    expect_error(TypeError::create(realm, u"type"sv), "TypeError"sv, "TypeError: type"sv);
    expect_error(URIError::create(realm), "URIError"sv, "URIError"sv);

    auto type_error = TypeError::create(realm, "Ünïcödé \U0001F600"_utf16);
    EXPECT_EQ(property_of(vm, type_error, "message"sv).as_string().utf16_string(), Utf16String::from_utf8("Ünïcödé \U0001F600"sv));
    EXPECT(property_of(vm, JS::Error::create(realm), "message"sv).as_string().utf16_string().is_empty());

    type_error->set_message("replaced"_utf16);
    EXPECT_EQ(string_of(vm, type_error), "TypeError: replaced"sv);
    type_error->set_message(u"replaced again"sv);
    EXPECT_EQ(string_of(vm, type_error), "TypeError: replaced again"sv);

    // The kinds tell each other apart, and an Error is an error of none of them.
    Object& type_error_object = *type_error;
    EXPECT(is<TypeError>(type_error_object));
    EXPECT(!is<RangeError>(type_error_object));
    EXPECT(!is<TypeError>(*JS::Error::create(realm)));
    EXPECT(!is<JS::Error>(MUST(evaluate(vm, realm, "Error.prototype"sv)).as_object()));
    EXPECT(!is<JS::Error>(MUST(evaluate(vm, realm, "({ message: 'not an error' })"sv)).as_object()));

    // Errors that JavaScript creates are of the same kinds, those of subclasses included.
    auto from_javascript = MUST(evaluate(vm, realm, "class MyRangeError extends RangeError {}; [new TypeError('t'), new MyRangeError('r'), new AggregateError([]), new Error('e')]"sv));
    EXPECT(is<TypeError>(property_of(vm, from_javascript, "0"sv).as_object()));
    EXPECT(is<RangeError>(property_of(vm, from_javascript, "1"sv).as_object()));
    EXPECT(is<JS::Error>(property_of(vm, from_javascript, "2"sv).as_object()));
    EXPECT(!is<TypeError>(property_of(vm, from_javascript, "2"sv).as_object()));
    EXPECT(is<JS::Error>(property_of(vm, from_javascript, "3"sv).as_object()));
    EXPECT(!is<RangeError>(property_of(vm, from_javascript, "3"sv).as_object()));

    EXPECT(same_value(realm.intrinsics().error_constructor(), MUST(evaluate(vm, realm, "Error"sv))));
}

TEST_CASE(errors_of_an_embedder_error_type)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto prototype = MUST(evaluate(vm, realm, "Object.create(Error.prototype, { name: { value: 'WebAssembly.LinkError' } })"sv));
    auto& prototype_object = prototype.as_object();

    auto error = JS::Error::create(realm, prototype_object);
    EXPECT(is<JS::Error>(static_cast<Object&>(*error)));
    EXPECT_EQ(string_of(vm, error), "WebAssembly.LinkError"sv);
    auto error_with_message = JS::Error::create(realm, prototype_object, "import failed"_utf16);
    EXPECT_EQ(string_of(vm, error_with_message), "WebAssembly.LinkError: import failed"sv);
    EXPECT(same_value(property_of(vm, error_with_message, "name"sv), property_of(vm, prototype, "name"sv)));
}

TEST_CASE(install_error_cause)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto error = JS::Error::create(realm, "with a cause"_utf16);
    MUST(error->install_error_cause(MUST(evaluate(vm, realm, "({ cause: 42 })"sv))));
    EXPECT_EQ(property_of(vm, error, "cause"sv).as_i32(), 42);

    auto without_cause = JS::Error::create(realm);
    MUST(without_cause->install_error_cause(js_undefined()));
    MUST(without_cause->install_error_cause(MUST(evaluate(vm, realm, "({})"sv))));
    EXPECT(property_of(vm, without_cause, "cause"sv).is_undefined());

    auto thrown = without_cause->install_error_cause(MUST(evaluate(vm, realm, "({ get cause() { throw 7; } })"sv)));
    EXPECT(thrown.is_throw_completion());
    EXPECT_EQ(thrown.throw_completion().value().as_i32(), 7);
}

TEST_CASE(error_data_of_errors_and_error_data_cells)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto error_value = MUST(evaluate(vm, realm, "function outer() {\n    return new TypeError('from outer');\n}\nouter()"sv));
    auto& error = as<JS::Error>(error_value.as_object());

    // The frames go from the innermost, the constructor that created the error, out to the realm's execution context.
    ErrorData const& error_data = error;
    auto traceback = error_data.traceback();
    EXPECT_EQ(traceback.size(), 4u);
    auto expect_frame = [&](size_t index, StringView function_name, StringView filename, u32 line, u32 column) {
        EXPECT_EQ(traceback[index].function_name, function_name);
        EXPECT_EQ(traceback[index].source_range().filename(), filename);
        EXPECT_EQ(traceback[index].source_range().start.line, line);
        EXPECT_EQ(traceback[index].source_range().start.column, column);
    };
    expect_frame(0, "TypeError"sv, ""sv, 0, 0);
    expect_frame(1, "outer"sv, "builtins.js"sv, 2, 12);
    expect_frame(2, ""sv, "builtins.js"sv, 4, 6);
    expect_frame(3, ""sv, ""sv, 0, 0);

    // The source range of a frame is in the source code that the frame ran, which it keeps alive.
    auto outer_source_range = traceback[1].source_range();
    EXPECT_EQ(outer_source_range.code.ptr(), traceback[2].source_range().code.ptr());
    traceback.clear();
    EXPECT_EQ(outer_source_range.code->code(), "function outer() {\n    return new TypeError('from outer');\n}\nouter()"sv);

    auto expected_stack_string = "    at TypeError\n    at outer (builtins.js:2:12)\n    at builtins.js:4:6\n"sv;
    EXPECT_EQ(error_data.stack_string(), expected_stack_string);
    EXPECT_EQ(error.stack_string(), expected_stack_string);
    EXPECT_EQ(error.stack_string(CompactTraceback::Yes), expected_stack_string);
    EXPECT_EQ(string_of(vm, property_of(vm, error_value, "stack"sv)), Utf16String::formatted("TypeError: from outer\n{}", expected_stack_string));

    // Compacting shows more than five consecutive frames of the same function as one with a count.
    auto deep_error = MUST(evaluate(vm, realm, "function recurse(depth) {\n    return depth === 0 ? new Error() : recurse(depth - 1);\n}\nrecurse(7)"sv));
    ErrorData const& deep_error_data = as<JS::Error>(deep_error.as_object());
    EXPECT_EQ(deep_error_data.traceback().size(), 11u);
    EXPECT_EQ(deep_error_data.stack_string(CompactTraceback::Yes), "    at Error\n    at recurse (builtins.js:2:40)\n    7 more calls\n    at builtins.js:4:8\n"sv);

    // An error that C++ creates has the call stack it was created on, of which the outermost frame shows in no
    // stack string.
    auto created_in_cpp = TypeError::create(realm);
    ErrorData const& created_in_cpp_data = *created_in_cpp;
    EXPECT_EQ(created_in_cpp_data.traceback().size(), 1u);
    EXPECT(created_in_cpp_data.traceback()[0].function_name.is_empty());
    EXPECT(created_in_cpp_data.traceback()[0].source_range().filename().is_empty());
    EXPECT_EQ(created_in_cpp_data.traceback()[0].source_range().start.line, 0u);
    EXPECT(created_in_cpp->stack_string().is_empty());

    // An error data cell captures the call stack for a host object that is not an Error, and is its error data.
    auto cell = ErrorDataCell::capture(vm);
    ErrorData* cell_error_data = cell.ptr();
    EXPECT_EQ(cell_error_data->traceback().size(), 1u);
    EXPECT(cell_error_data->stack_string().is_empty());
    EXPECT_EQ(cell->traceback().size(), 1u);
}

TEST_CASE(promises_settled_from_cpp)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
    queue_promise_jobs_in(vm, queued_promise_jobs);

    auto pending = Promise::create(realm);
    EXPECT_EQ(pending->state(), Promise::State::Pending);
    EXPECT(pending->result().is_undefined());
    EXPECT(!pending->is_handled());
    pending->set_is_handled();
    EXPECT(pending->is_handled());
    pass_to_javascript(vm, realm, 0, pending);
    EXPECT(evaluates_to_true(vm, realm, "Object.getPrototypeOf(from_cpp.get(0)) === Promise.prototype"sv));

    auto fulfilled = Promise::create(realm);
    fulfilled->fulfill(Value(42));
    EXPECT_EQ(fulfilled->state(), Promise::State::Fulfilled);
    EXPECT_EQ(fulfilled->result().as_i32(), 42);

    auto rejected = Promise::create(realm);
    rejected->set_is_handled();
    rejected->reject(Value(7));
    EXPECT_EQ(rejected->state(), Promise::State::Rejected);
    EXPECT_EQ(rejected->result().as_i32(), 7);

    // Reactions run as promise jobs, which the host runs.
    MUST(evaluate(vm, realm, "globalThis.log = []"sv));
    auto on_fulfilled = MUST(evaluate(vm, realm, "value => { log.push('fulfilled ' + value); return value + 1; }"sv));
    auto on_rejected = MUST(evaluate(vm, realm, "reason => { log.push('rejected ' + reason); }"sv));
    auto capability = MUST(new_promise_capability(vm, realm.intrinsics().promise_constructor()));
    auto source = Promise::create(realm);
    EXPECT(same_value(source->perform_then(on_fulfilled, on_rejected, capability), capability->promise()));
    EXPECT(source->is_handled());
    EXPECT(source->perform_then(on_fulfilled, on_rejected, nullptr).is_undefined());
    source->fulfill(Value(1));
    EXPECT_EQ(queued_promise_jobs.size(), 2u);
    run_queued_promise_jobs(queued_promise_jobs);
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "log.join()"sv))), "fulfilled 1,fulfilled 1"sv);
    auto& chained = as<Promise>(*capability->promise());
    EXPECT_EQ(chained.state(), Promise::State::Fulfilled);
    EXPECT_EQ(chained.result().as_i32(), 2);

    // The resolving functions of a promise share whether it is resolved: the first call settles it.
    auto target = Promise::create(realm);
    auto resolving_functions = target->create_resolving_functions();
    auto settled_first = Promise::create(realm);
    settled_first->perform_then(resolving_functions.reject, resolving_functions.resolve, nullptr);
    auto settled_second = Promise::create(realm);
    settled_second->perform_then(resolving_functions.resolve, resolving_functions.resolve, nullptr);
    settled_first->fulfill(Value(3));
    settled_second->fulfill(Value(4));
    target->set_is_handled();
    run_queued_promise_jobs(queued_promise_jobs);
    EXPECT_EQ(target->state(), Promise::State::Rejected);
    EXPECT_EQ(target->result().as_i32(), 3);

    // Resolving a promise with a thenable adopts its state.
    auto adopting = Promise::create(realm);
    auto adopting_functions = adopting->create_resolving_functions();
    auto resolved_with_promise = Promise::create(realm);
    resolved_with_promise->perform_then(adopting_functions.resolve, adopting_functions.reject, nullptr);
    resolved_with_promise->fulfill(MUST(evaluate(vm, realm, "Promise.resolve('adopted')"sv)));
    run_queued_promise_jobs(queued_promise_jobs);
    EXPECT_EQ(adopting->state(), Promise::State::Fulfilled);
    EXPECT_EQ(string_of(vm, adopting->result()), "adopted"sv);
}

TEST_CASE(promises_of_javascript)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
    queue_promise_jobs_in(vm, queued_promise_jobs);

    auto results = MUST(evaluate(vm, realm, "class SubPromise extends Promise {}; [Promise.resolve(1), (async () => { throw 2; })(), new SubPromise(() => {}), { then() {} }, Promise.prototype]"sv));
    auto& resolved = as<Promise>(property_of(vm, results, "0"sv).as_object());
    EXPECT_EQ(resolved.state(), Promise::State::Fulfilled);
    EXPECT_EQ(resolved.result().as_i32(), 1);
    auto& thrown_in_async_function = as<Promise>(property_of(vm, results, "1"sv).as_object());
    EXPECT_EQ(thrown_in_async_function.state(), Promise::State::Rejected);
    EXPECT_EQ(thrown_in_async_function.result().as_i32(), 2);
    EXPECT(!thrown_in_async_function.is_handled());
    EXPECT(is<Promise>(property_of(vm, results, "2"sv).as_object()));
    EXPECT(!is<Promise>(property_of(vm, results, "3"sv).as_object()));
    EXPECT(!is<Promise>(property_of(vm, results, "4"sv).as_object()));

    auto& caught = as<Promise>(MUST(evaluate(vm, realm, "globalThis.caught = Promise.reject(5); caught.catch(() => {}); caught"sv)).as_object());
    EXPECT(caught.is_handled());
    run_queued_promise_jobs(queued_promise_jobs);

    EXPECT(same_value(realm.intrinsics().promise_constructor(), MUST(evaluate(vm, realm, "Promise"sv))));
}

TEST_CASE(promise_capabilities)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
    queue_promise_jobs_in(vm, queued_promise_jobs);

    auto capability = MUST(new_promise_capability(vm, realm.intrinsics().promise_constructor()));
    auto& promise = as<Promise>(*capability->promise());
    EXPECT_EQ(promise.state(), Promise::State::Pending);
    pass_to_javascript(vm, realm, 0, capability->promise());
    pass_to_javascript(vm, realm, 1, capability->resolve());
    pass_to_javascript(vm, realm, 2, capability->reject());
    MUST(evaluate(vm, realm, "from_cpp.get(1)('resolved'); from_cpp.get(2)('ignored')"sv));
    EXPECT_EQ(promise.state(), Promise::State::Fulfilled);
    EXPECT_EQ(string_of(vm, promise.result()), "resolved"sv);

    // A capability of a subclass constructs the subclass.
    auto subclass = MUST(evaluate(vm, realm, "globalThis.constructed = 0; class SubPromise extends Promise { constructor(executor) { super(executor); ++constructed; } }; SubPromise"sv));
    auto subclass_capability = MUST(new_promise_capability(vm, subclass));
    EXPECT(is<Promise>(*subclass_capability->promise()));
    pass_to_javascript(vm, realm, 3, subclass_capability->promise());
    EXPECT(evaluates_to_true(vm, realm, "constructed === 1 && from_cpp.get(3) instanceof SubPromise"sv));

    // Something that is not a constructor makes a TypeError, and a throwing constructor's exception passes through.
    auto not_a_constructor = new_promise_capability(vm, Value(1));
    EXPECT(not_a_constructor.is_throw_completion());
    EXPECT(is<TypeError>(not_a_constructor.throw_completion().value().as_object()));
    auto throwing_constructor = new_promise_capability(vm, MUST(evaluate(vm, realm, "(function (executor) { throw 'from constructor'; })"sv)));
    EXPECT(throwing_constructor.is_throw_completion());
    EXPECT_EQ(string_of(vm, throwing_constructor.throw_completion().value()), "from constructor"sv);

    // A capability of any object and two functions.
    auto resolve = MUST(evaluate(vm, realm, "(() => {})"sv));
    auto reject = MUST(evaluate(vm, realm, "(() => {})"sv));
    auto object = MUST(evaluate(vm, realm, "({})"sv));
    auto record = PromiseCapability::create(vm, object.as_object(), as<FunctionObject>(resolve.as_object()), as<FunctionObject>(reject.as_object()));
    EXPECT(same_value(record->promise(), object));
    EXPECT(same_value(record->resolve(), resolve));
    EXPECT(same_value(record->reject(), reject));
}

TEST_CASE(maps)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto map = Map::create(realm);
    EXPECT_EQ(map->map_size(), 0u);
    map->map_set(Value(1), Value(10));
    map->map_set(PrimitiveString::create(vm, "two"_utf16), Value(20));
    map->map_set(Value(-0.0), Value(30));
    map->map_set(Value(1), Value(11));
    EXPECT_EQ(map->map_size(), 3u);
    EXPECT_EQ(map->map_get(Value(1.0))->as_i32(), 11);
    EXPECT_EQ(map->map_get(PrimitiveString::create(vm, "two"_utf16))->as_i32(), 20);
    EXPECT(!map->map_get(Value(2)).has_value());
    EXPECT(!map->map_has(js_undefined()));

    // Unlike the JavaScript methods, which turn a key of -0 into +0, these compare keys with SameValue.
    EXPECT(map->map_has(Value(-0.0)));
    EXPECT(!map->map_has(Value(0)));

    // JavaScript sees the same entries.
    pass_to_javascript(vm, realm, 0, map);
    EXPECT(evaluates_to_true(vm, realm, "const map = from_cpp.get(0); map instanceof Map && map.size === 3 && map.get('two') === 20"sv));
    EXPECT(evaluates_to_true(vm, realm, "Object.is([...from_cpp.get(0).keys()][2], -0)"sv));
    MUST(evaluate(vm, realm, "from_cpp.get(0).set('from javascript', 40)"sv));
    EXPECT_EQ(map->map_size(), 4u);

    EXPECT(map->map_remove(Value(1)));
    EXPECT(!map->map_remove(Value(1)));
    EXPECT_EQ(map->map_size(), 3u);
    map->map_clear();
    EXPECT_EQ(map->map_size(), 0u);
    EXPECT(evaluates_to_true(vm, realm, "from_cpp.get(0).size === 0"sv));

    EXPECT(is<Map>(MUST(evaluate(vm, realm, "new Map()"sv)).as_object()));
    EXPECT(!is<Map>(MUST(evaluate(vm, realm, "Map.prototype"sv)).as_object()));
    EXPECT(!is<Map>(MUST(evaluate(vm, realm, "new WeakMap()"sv)).as_object()));
}

TEST_CASE(for_each_entry_visits_a_map_as_for_each_does)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto map = Map::create(realm);
    for (i32 key = 1; key <= 4; ++key)
        map->map_set(Value(key), Value(key * 10));

    // Entries that the callback removes before they are reached are skipped, and entries it adds are visited.
    Vector<i32> visited;
    MUST(map->for_each_entry([&](Value key, Value value) -> ThrowCompletionOr<void> {
        visited.append(key.as_i32());
        EXPECT_EQ(value.as_i32(), key.as_i32() * 10);
        if (key.as_i32() == 1) {
            map->map_remove(Value(2));
            map->map_set(Value(5), Value(50));
        }
        return {};
    }));
    EXPECT_EQ(visited, (Vector<i32> { 1, 3, 4, 5 }));

    // The first error of the callback stops the visit.
    visited.clear();
    auto result = map->for_each_entry([&](Value key, Value) -> ThrowCompletionOr<void> {
        visited.append(key.as_i32());
        if (key.as_i32() == 3)
            return vm.throw_completion<RangeError>("stop"sv);
        return {};
    });
    EXPECT(result.is_throw_completion());
    EXPECT(is<RangeError>(result.throw_completion().value().as_object()));
    EXPECT_EQ(visited, (Vector<i32> { 1, 3 }));

    visited.clear();
    MUST(Map::create(realm)->for_each_entry([&](Value, Value) -> ThrowCompletionOr<void> {
        visited.append(0);
        return {};
    }));
    EXPECT(visited.is_empty());
}

TEST_CASE(sets)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto set = Set::create(realm);
    EXPECT_EQ(set->set_size(), 0u);
    set->set_add(Value(1));
    set->set_add(PrimitiveString::create(vm, "two"_utf16));
    set->set_add(Value(1.0));
    set->set_add(Value(-0.0));
    EXPECT_EQ(set->set_size(), 3u);
    EXPECT(set->set_has(Value(1)));
    EXPECT(set->set_has(Value(-0.0)));
    EXPECT(!set->set_has(Value(0)));
    EXPECT(set->set_has(PrimitiveString::create(vm, "two"_utf16)));
    EXPECT(!set->set_has(Value(2)));

    pass_to_javascript(vm, realm, 0, set);
    EXPECT(evaluates_to_true(vm, realm, "const set = from_cpp.get(0); set instanceof Set && set.size === 3 && set.has('two') && Object.is([...set][2], -0)"sv));

    EXPECT(set->set_remove(Value(1)));
    EXPECT(!set->set_remove(Value(1)));
    EXPECT_EQ(set->set_size(), 2u);

    // Values that the callback removes before they are reached are skipped, and values it adds are visited.
    set->set_clear();
    for (i32 value = 1; value <= 4; ++value)
        set->set_add(Value(value));
    Vector<i32> visited;
    MUST(set->for_each_value([&](Value value) -> ThrowCompletionOr<void> {
        visited.append(value.as_i32());
        if (value.as_i32() == 1) {
            set->set_remove(Value(3));
            set->set_add(Value(5));
        }
        return {};
    }));
    EXPECT_EQ(visited, (Vector<i32> { 1, 2, 4, 5 }));

    visited.clear();
    auto result = set->for_each_value([&](Value value) -> ThrowCompletionOr<void> {
        visited.append(value.as_i32());
        return vm.throw_completion<TypeError>("stop"sv);
    });
    EXPECT(result.is_throw_completion());
    EXPECT_EQ(visited, (Vector<i32> { 1 }));

    set->set_clear();
    EXPECT_EQ(set->set_size(), 0u);
    EXPECT(evaluates_to_true(vm, realm, "from_cpp.get(0).size === 0"sv));

    EXPECT(is<Set>(MUST(evaluate(vm, realm, "new Set()"sv)).as_object()));
    EXPECT(!is<Set>(MUST(evaluate(vm, realm, "Set.prototype"sv)).as_object()));
    EXPECT(!is<Set>(MUST(evaluate(vm, realm, "new Map()"sv)).as_object()));
}

TEST_CASE(map_and_set_iterators)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto map = Map::create(realm);
    map->map_set(Value(1), Value(10));
    map->map_set(Value(2), Value(20));
    pass_to_javascript(vm, realm, 0, MapIterator::create(realm, map, Object::PropertyKind::Key));
    pass_to_javascript(vm, realm, 1, MapIterator::create(realm, map, Object::PropertyKind::Value));
    auto entries = MapIterator::create(realm, map, Object::PropertyKind::KeyAndValue);
    pass_to_javascript(vm, realm, 2, entries);
    EXPECT(is<MapIterator>(static_cast<Object&>(*entries)));

    // The iterators see the entries as they are when they step.
    map->map_set(Value(3), Value(30));
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "JSON.stringify([[...from_cpp.get(0)], [...from_cpp.get(1)], [...from_cpp.get(2)]])"sv))), "[[1,2,3],[10,20,30],[[1,10],[2,20],[3,30]]]"sv);
    EXPECT(evaluates_to_true(vm, realm, "Object.getPrototypeOf(from_cpp.get(0)) === Object.getPrototypeOf(new Map().keys())"sv));

    auto set = Set::create(realm);
    set->set_add(Value(1));
    set->set_add(Value(2));
    pass_to_javascript(vm, realm, 3, SetIterator::create(realm, set, Object::PropertyKind::Value));
    auto set_entries = SetIterator::create(realm, set, Object::PropertyKind::KeyAndValue);
    pass_to_javascript(vm, realm, 4, set_entries);
    EXPECT(is<SetIterator>(static_cast<Object&>(*set_entries)));
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "JSON.stringify([[...from_cpp.get(3)], [...from_cpp.get(4)]])"sv))), "[[1,2],[[1,1],[2,2]]]"sv);

    EXPECT(is<MapIterator>(MUST(evaluate(vm, realm, "new Map().entries()"sv)).as_object()));
    EXPECT(!is<MapIterator>(MUST(evaluate(vm, realm, "new Set().values()"sv)).as_object()));
    EXPECT(is<SetIterator>(MUST(evaluate(vm, realm, "new Set().values()"sv)).as_object()));
}

TEST_CASE(dates)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    // 2026-10-04T12:34:56.789Z
    auto time_value = make_date(make_day(2026, 9, 4), make_time(12, 34, 56, 789));
    EXPECT_EQ(time_value, 1791117296789.0);
    auto date = Date::create(realm, time_value);
    EXPECT_EQ(date->date_value(), time_value);
    pass_to_javascript(vm, realm, 0, date);
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "from_cpp.get(0).toISOString()"sv))), "2026-10-04T12:34:56.789Z"sv);
    EXPECT(evaluates_to_true(vm, realm, "from_cpp.get(0) instanceof Date"sv));

    EXPECT(isnan(Date::create(realm, NAN)->date_value()));
    auto& from_javascript = as<Date>(MUST(evaluate(vm, realm, "new Date(Date.UTC(1969, 11, 31, 23, 59, 58, 7))"sv)).as_object());
    auto time = from_javascript.date_value();
    EXPECT_EQ(year_from_time(time), 1969);
    EXPECT_EQ(month_from_time(time), 11);
    EXPECT_EQ(date_from_time(time), 31);
    EXPECT_EQ(hour_from_time(time), 23);
    EXPECT_EQ(min_from_time(time), 59);
    EXPECT_EQ(sec_from_time(time), 58);
    EXPECT_EQ(ms_from_time(time), 7);
    EXPECT_EQ(make_day(1970, 0, 1), 0.0);
    EXPECT_EQ(make_day(2024, 1, 29) * ms_per_day, MUST(evaluate(vm, realm, "Date.UTC(2024, 1, 29)"sv)).as_double());
    EXPECT(isnan(make_day(INFINITY, 0, 1)));
    EXPECT(isnan(make_time(NAN, 0, 0, 0)));
    EXPECT_EQ(make_time(1, 2, 3, 4), ms_per_hour + 2 * ms_per_minute + 3 * ms_per_second + 4);
    EXPECT_EQ(max_time_value, 8.64E15);
    EXPECT_EQ(ms_per_day, hours_per_day * minutes_per_hour * seconds_per_minute * ms_per_second);

    clear_system_time_zone_cache();
    EXPECT(evaluates_to_true(vm, realm, "typeof new Date(0).getTimezoneOffset() === 'number'"sv));

    EXPECT(!is<Date>(MUST(evaluate(vm, realm, "Date.prototype"sv)).as_object()));
}

TEST_CASE(regexps)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto regexp = MUST(regexp_create(vm, PrimitiveString::create(vm, "a+b"_utf16), PrimitiveString::create(vm, "gi"_utf16)));
    EXPECT_EQ(regexp->pattern(), "a+b"sv);
    EXPECT_EQ(regexp->flags(), "gi"sv);
    pass_to_javascript(vm, realm, 0, regexp);
    EXPECT(evaluates_to_true(vm, realm, "const regexp = from_cpp.get(0); regexp instanceof RegExp && regexp.test('xAAB') && regexp.lastIndex === 4"sv));

    // undefined is the empty pattern and no flags, and other values become strings.
    auto empty = MUST(regexp_create(vm, js_undefined(), js_undefined()));
    EXPECT(empty->pattern().is_empty());
    EXPECT(empty->flags().is_empty());
    EXPECT_EQ(MUST(regexp_create(vm, Value(12), js_undefined()))->pattern(), "12"sv);

    auto invalid_pattern = regexp_create(vm, PrimitiveString::create(vm, "("_utf16), js_undefined());
    EXPECT(invalid_pattern.is_throw_completion());
    EXPECT(is<SyntaxError>(invalid_pattern.throw_completion().value().as_object()));
    auto invalid_flags = regexp_create(vm, js_undefined(), PrimitiveString::create(vm, "gg"_utf16));
    EXPECT(invalid_flags.is_throw_completion());
    EXPECT(is<SyntaxError>(invalid_flags.throw_completion().value().as_object()));

    auto& from_javascript = as<RegExpObject>(MUST(evaluate(vm, realm, "/[a-z]\\//dgimsy"sv)).as_object());
    EXPECT_EQ(from_javascript.pattern(), "[a-z]\\/"sv);
    EXPECT_EQ(from_javascript.flags(), "dgimsy"sv);

    EXPECT(!is<RegExpObject>(MUST(evaluate(vm, realm, "RegExp.prototype"sv)).as_object()));
}

TEST_CASE(finalization_registries)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    Vector<GC::Root<FinalizationRegistry>> registries_to_clean_up;
    vm.host_enqueue_finalization_registry_cleanup_job = [&](FinalizationRegistry& registry) {
        registries_to_clean_up.append(GC::make_root(registry));
    };

    auto callback = MUST(evaluate(vm, realm, "globalThis.cleaned_up = []; held => cleaned_up.push(held)"sv));
    pass_to_javascript(vm, realm, 0, callback);
    auto& registry = as<FinalizationRegistry>(MUST(evaluate(vm, realm, "globalThis.registry = new FinalizationRegistry(from_cpp.get(0)); registry"sv)).as_object());
    EXPECT_EQ(&registry.realm(), &realm);
    EXPECT(same_value(&registry.cleanup_callback().callback(), callback));

    // Without collected targets, a cleanup calls nothing.
    MUST(registry.cleanup());
    EXPECT(evaluates_to_true(vm, realm, "cleaned_up.length === 0"sv));

    MUST(evaluate(vm, realm, "(() => { for (let i = 0; i < 4; ++i) registry.register({}, 'held ' + i); })()"sv));
    collect_garbage(vm);
    EXPECT_EQ(registries_to_clean_up.size(), 1u);
    EXPECT_EQ(registries_to_clean_up.first().ptr(), &registry);
    MUST(registry.cleanup());
    EXPECT(evaluates_to_true(vm, realm, "cleaned_up.sort().join() === 'held 0,held 1,held 2,held 3'"sv));

    // A cleanup with a callback of its own calls it in place of the registry's.
    MUST(evaluate(vm, realm, "(() => registry.register({}, 'held again'))()"sv));
    collect_garbage(vm);
    auto other_callback = MUST(evaluate(vm, realm, "globalThis.cleaned_up_by_other = []; held => cleaned_up_by_other.push(held)"sv));
    MUST(registry.cleanup(JobCallback::create(vm, as<FunctionObject>(other_callback.as_object()), nullptr)));
    EXPECT(evaluates_to_true(vm, realm, "cleaned_up.length === 4 && cleaned_up_by_other.join() === 'held again'"sv));

    // A throwing callback stops the cleanup.
    MUST(evaluate(vm, realm, "(() => registry.register({}, 'held by a throwing cleanup'))()"sv));
    collect_garbage(vm);
    auto throwing_callback = MUST(evaluate(vm, realm, "(() => { throw 'from cleanup'; })"sv));
    auto thrown = registry.cleanup(JobCallback::create(vm, as<FunctionObject>(throwing_callback.as_object()), nullptr));
    EXPECT(thrown.is_throw_completion());
    EXPECT_EQ(string_of(vm, thrown.throw_completion().value()), "from cleanup"sv);
}

TEST_CASE(objects_that_wrap_primitives)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto boolean_object = BooleanObject::create(realm, true);
    EXPECT(boolean_object->boolean());
    auto number_object = NumberObject::create(realm, 1.5);
    EXPECT_EQ(number_object->number(), 1.5);
    auto bigint = BigInt::create(vm, Crypto::SignedBigInteger { -12345 });
    auto bigint_object = BigIntObject::create(realm, bigint);
    EXPECT_EQ(&bigint_object->bigint(), bigint.ptr());
    auto string = PrimitiveString::create(vm, "abc"_utf16);
    auto string_object = StringObject::create(realm, string, realm.intrinsics().string_prototype());
    EXPECT_EQ(&string_object->primitive_string(), string.ptr());
    StringObject const& const_string_object = *string_object;
    EXPECT_EQ(&const_string_object.primitive_string(), string.ptr());

    pass_to_javascript(vm, realm, 0, boolean_object);
    pass_to_javascript(vm, realm, 1, number_object);
    pass_to_javascript(vm, realm, 2, bigint_object);
    pass_to_javascript(vm, realm, 3, string_object);
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "[0, 1, 2, 3].map(key => typeof from_cpp.get(key) + ' ' + from_cpp.get(key).valueOf()).join()"sv))), "object true,object 1.5,object -12345,object abc"sv);
    EXPECT(evaluates_to_true(vm, realm, "const string_object = from_cpp.get(3); string_object.length === 3 && string_object[1] === 'b' && Object.keys(string_object).join() === '0,1,2'"sv));

    // The objects that JavaScript creates are of the same kinds, and so are the prototypes that wrap a primitive.
    auto objects = MUST(evaluate(vm, realm, "[Object(false), new Number(2), Object(3n), new String('x'), Boolean.prototype, Number.prototype, String.prototype, BigInt.prototype]"sv));
    auto object_at = [&](StringView index) -> Object& { return property_of(vm, objects, index).as_object(); };
    EXPECT(!as<BooleanObject>(object_at("0"sv)).boolean());
    EXPECT_EQ(as<NumberObject>(object_at("1"sv)).number(), 2);
    EXPECT_EQ(as<BigIntObject>(object_at("2"sv)).bigint().big_integer(), Crypto::SignedBigInteger { 3 });
    EXPECT_EQ(as<StringObject>(object_at("3"sv)).primitive_string().utf16_string(), "x"sv);
    EXPECT(!as<BooleanObject>(object_at("4"sv)).boolean());
    EXPECT_EQ(as<NumberObject>(object_at("5"sv)).number(), 0);
    EXPECT(as<StringObject>(object_at("6"sv)).primitive_string().utf16_string().is_empty());
    EXPECT(!is<BigIntObject>(object_at("7"sv)));
    EXPECT(!is<NumberObject>(object_at("0"sv)));
    EXPECT(!is<StringObject>(object_at("1"sv)));
    EXPECT(!is<BooleanObject>(object_at("3"sv)));
}
