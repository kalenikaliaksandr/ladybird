/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/Error.h>
#include <LibJS/Runtime/ErrorConstructor.h>
#include <LibJS/Runtime/ErrorData.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/Map.h>
#include <LibJS/Runtime/MapIterator.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Set.h>
#include <LibJS/Runtime/SetIterator.h>
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

// Object::PropertyKind, through Object so that this builds before the facade's Object has it.
enum class PropertyKindOfThisTest {
    Key,
    Value,
    KeyAndValue,
};

template<typename ObjectType = Object>
static auto property_kind(PropertyKindOfThisTest kind)
{
    if constexpr (requires { ObjectType::PropertyKind::KeyAndValue; })
        return static_cast<typename ObjectType::PropertyKind>(kind);
    else
        return kind;
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
    pass_to_javascript(vm, realm, 0, MapIterator::create(realm, map, property_kind(PropertyKindOfThisTest::Key)));
    pass_to_javascript(vm, realm, 1, MapIterator::create(realm, map, property_kind(PropertyKindOfThisTest::Value)));
    auto entries = MapIterator::create(realm, map, property_kind(PropertyKindOfThisTest::KeyAndValue));
    pass_to_javascript(vm, realm, 2, entries);
    EXPECT(is<MapIterator>(static_cast<Object&>(*entries)));

    // The iterators see the entries as they are when they step.
    map->map_set(Value(3), Value(30));
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "JSON.stringify([[...from_cpp.get(0)], [...from_cpp.get(1)], [...from_cpp.get(2)]])"sv))), "[[1,2,3],[10,20,30],[[1,10],[2,20],[3,30]]]"sv);
    EXPECT(evaluates_to_true(vm, realm, "Object.getPrototypeOf(from_cpp.get(0)) === Object.getPrototypeOf(new Map().keys())"sv));

    auto set = Set::create(realm);
    set->set_add(Value(1));
    set->set_add(Value(2));
    pass_to_javascript(vm, realm, 3, SetIterator::create(realm, set, property_kind(PropertyKindOfThisTest::Value)));
    auto set_entries = SetIterator::create(realm, set, property_kind(PropertyKindOfThisTest::KeyAndValue));
    pass_to_javascript(vm, realm, 4, set_entries);
    EXPECT(is<SetIterator>(static_cast<Object&>(*set_entries)));
    EXPECT_EQ(string_of(vm, MUST(evaluate(vm, realm, "JSON.stringify([[...from_cpp.get(3)], [...from_cpp.get(4)]])"sv))), "[[1,2],[[1,1],[2,2]]]"sv);

    EXPECT(is<MapIterator>(MUST(evaluate(vm, realm, "new Map().entries()"sv)).as_object()));
    EXPECT(!is<MapIterator>(MUST(evaluate(vm, realm, "new Set().values()"sv)).as_object()));
    EXPECT(is<SetIterator>(MUST(evaluate(vm, realm, "new Set().values()"sv)).as_object()));
}
