/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <AK/String.h>
#include <AK/StringBuilder.h>
#include <AK/Utf16String.h>
#include <AK/Utf16StringBuilder.h>
#include <AK/Vector.h>
#include <LibGC/RootVector.h>
#include <LibJS/Console.h>
#include <LibJS/Print.h>
#include <LibJS/Runtime/ConsoleObject.h>
#include <LibJS/Runtime/Error.h>
#include <LibJS/Runtime/ErrorData.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// The console of a realm, the embedder's console clients that receive what it logs, and printing values the way the
// console shows them, as LibJS's users use them. The same expectations hold for the C++ runtime's LibJS and for the
// facade over the Rust one.

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
    Console& console() { return realm().intrinsics().console_object()->console(); }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

StringView name_of(Console::LogLevel log_level)
{
    switch (log_level) {
    case Console::LogLevel::Assert:
        return "Assert"sv;
    case Console::LogLevel::Count:
        return "Count"sv;
    case Console::LogLevel::CountReset:
        return "CountReset"sv;
    case Console::LogLevel::Debug:
        return "Debug"sv;
    case Console::LogLevel::Dir:
        return "Dir"sv;
    case Console::LogLevel::DirXML:
        return "DirXML"sv;
    case Console::LogLevel::Error:
        return "Error"sv;
    case Console::LogLevel::Group:
        return "Group"sv;
    case Console::LogLevel::GroupCollapsed:
        return "GroupCollapsed"sv;
    case Console::LogLevel::Info:
        return "Info"sv;
    case Console::LogLevel::Log:
        return "Log"sv;
    case Console::LogLevel::TimeEnd:
        return "TimeEnd"sv;
    case Console::LogLevel::TimeLog:
        return "TimeLog"sv;
    case Console::LogLevel::Table:
        return "Table"sv;
    case Console::LogLevel::Trace:
        return "Trace"sv;
    case Console::LogLevel::Warn:
        return "Warn"sv;
    }
    VERIFY_NOT_REACHED();
}

// One of the embedder's console clients, which records each call the console makes, with the values it logs
// formatted the generic way.
class RecordingConsoleClient final : public ConsoleClient {
    GC_CELL(RecordingConsoleClient, ConsoleClient);
    GC_DECLARE_ALLOCATOR(RecordingConsoleClient);

public:
    Vector<String>& events() { return m_events; }

    Console& console() { return *m_console; }

    virtual ThrowCompletionOr<Value> printer(Console::LogLevel log_level, PrinterArguments arguments) override
    {
        if (arguments.has<Console::Group>()) {
            m_events.append(MUST(String::formatted("{}: {}", name_of(log_level), arguments.get<Console::Group>().label)));
            return js_undefined();
        }

        if (arguments.has<Console::Trace>()) {
            auto const& trace = arguments.get<Console::Trace>();
            StringBuilder builder;
            builder.appendff("{}: {}", name_of(log_level), trace.label);
            for (auto const& frame : trace.stack) {
                builder.appendff(" | {}", frame.function_name);
                if (frame.source_file.has_value())
                    builder.appendff(" at {}", *frame.source_file);
                if (frame.line.has_value() && frame.column.has_value())
                    builder.appendff(":{}:{}", *frame.line, *frame.column);
            }
            m_events.append(builder.to_string_without_validation());
            return js_undefined();
        }

        auto formatted = TRY(generically_format_values(arguments.get<GC::RootVector<Value>>()));
        if (formatted == "\"throw from the printer\""sv)
            return throw_completion(Value(7));
        m_events.append(MUST(String::formatted("{}: {}", name_of(log_level), formatted)));
        return js_undefined();
    }

    virtual void add_css_style_to_current_message(Utf16View style) override
    {
        m_events.append(MUST(String::formatted("css: {}", style)));
    }

    virtual void report_exception(Utf16View name, Utf16View message, ErrorData const& error_data, bool in_promise) override
    {
        StringBuilder builder;
        builder.appendff("exception{}: {}: {}", in_promise ? " in promise"sv : ""sv, name, message);
        for (auto const& frame : error_data.traceback()) {
            auto const& source_range = frame.source_range();
            builder.appendff(" | {}@{}:{}:{}", frame.function_name, source_range.filename(), source_range.start.line, source_range.start.column);
        }
        m_events.append(builder.to_string_without_validation());
    }

    virtual void clear() override
    {
        m_events.append("clear"_string);
    }

    virtual void end_group() override
    {
        m_events.append("end group"_string);
    }

private:
    explicit RecordingConsoleClient(Console& console, Vector<String>& events)
        : ConsoleClient(console)
        , m_events(events)
    {
    }

    Vector<String>& m_events;
};

GC_DEFINE_ALLOCATOR(RecordingConsoleClient);

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source, StringView filename = "console.js"sv)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm, filename);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static GC::Ref<RecordingConsoleClient> install_recording_client(VMWithRealm& vm_with_realm, Vector<String>& events)
{
    auto& console = vm_with_realm.console();
    auto client = vm_with_realm.vm->heap().allocate<RecordingConsoleClient>(console, events);
    console.set_client(*client);
    return client;
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

TEST_CASE(the_console_of_a_realm)
{
    VMWithRealm vm_with_realm;
    auto& realm = vm_with_realm.realm();
    auto& console = vm_with_realm.console();
    EXPECT_EQ(&console.realm(), &realm);

    auto console_object = MUST(Value(&realm.global_object()).get(*vm_with_realm.vm, PropertyKey { "console"_utf16_fly_string }));
    EXPECT(is<ConsoleObject>(console_object.as_object()));
    EXPECT_EQ(&as<ConsoleObject>(console_object.as_object()).console(), &console);
    EXPECT(!is<ConsoleObject>(realm.global_object()));

    // These write to the debug log only.
    console.output_debug_message(Console::LogLevel::Log, "from a StringView"sv);
    console.output_debug_message(Console::LogLevel::Warn, u"from a Utf16View"sv);
}

TEST_CASE(a_console_client_receives_what_the_console_logs)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    install_recording_client(vm_with_realm, events);

    auto source = R"(
console.log("answer", 42);
console.log();
console.warn({ a: 1 }, [1, "two"]);
console.error("error %d of %s", 3, "four", "extra");
console.info("info");
console.debug("debug");
console.dir({ b: 2 });
console.dirxml("xml");
console.table([1]);
console.assert(true, "not logged");
console.assert(false);
console.assert(false, "with %s", "message");
console.assert(false, 5);
console.count();
console.count("label");
console.count();
console.countReset();
console.count();
console.countReset("unknown");
console.group("group %s", "label");
console.groupEnd();
console.groupCollapsed();
console.groupEnd();
console.clear();
console.log("%cstyled", "color: red");
)"sv;
    EXPECT(!evaluate(vm, realm, source).is_error());

    // console.dirxml() prints like console.dir(), as both runtimes define it with dir's native function.
    Vector<String> expected_events {
        "Log: \"answer\" 42"_string,
        "Warn: Object{ \"a\": 1 } [ 1, \"two\" ]"_string,
        "Error: \"error 3 of four\" \"extra\""_string,
        "Info: \"info\""_string,
        "Debug: \"debug\""_string,
        "Dir: Object{ \"b\": 2 }"_string,
        "Dir: \"xml\""_string,
        "Table: Object{ \"rows\": [ Object{ \"(index)\": 0, \"Value\": 1 } ], \"columns\": [ \"(index)\", \"Value\" ] }"_string,
        "Assert: \"Assertion failed\""_string,
        "Assert: \"Assertion failed: with message\""_string,
        "Assert: \"Assertion failed\" 5"_string,
        "Count: \"default: 1\""_string,
        "Count: \"label: 1\""_string,
        "Count: \"default: 2\""_string,
        "Count: \"default: 1\""_string,
        "CountReset: \"\"unknown\" doesn't have a count\""_string,
        "Group: group label"_string,
        "end group"_string,
        "GroupCollapsed: Group"_string,
        "end group"_string,
        "clear"_string,
        "css: color: red"_string,
        "Log: \"styled\""_string,
    };
    EXPECT_EQ(events, expected_events);
}

TEST_CASE(a_console_client_receives_reported_exceptions)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    install_recording_client(vm_with_realm, events);

    // The client reads the error data of an Error and of an error data cell, which the console passes on as it is.
    auto error = MUST(evaluate(vm, realm, "function make() {\n    return new TypeError('boom');\n}\nmake()"sv));
    vm_with_realm.console().report_exception(u"TypeError"sv, u"boom"sv, as<JS::Error>(error.as_object()), false);
    auto cell = ErrorDataCell::capture(vm);
    vm_with_realm.console().report_exception(u"DOMException"sv, u"from a cell"sv, *cell, true);

    Vector<String> expected_events {
        "exception: TypeError: boom | TypeError@:0:0 | make@console.js:2:12 | @console.js:4:5 | @:0:0"_string,
        "exception in promise: DOMException: from a cell | @:0:0"_string,
    };
    EXPECT_EQ(events, expected_events);
}

TEST_CASE(a_console_client_receives_traces_and_timers)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    install_recording_client(vm_with_realm, events);

    auto source = "function traced() {\n    console.trace('label %d', 5);\n}\ntraced();\n"sv;
    EXPECT(!evaluate(vm, realm, source, "trace.js"sv).is_error());
    Vector<String> expected_trace {
        "Trace: label 5 | traced at trace.js:2:18 | <anonymous> at trace.js:4:7 | <anonymous>"_string,
    };
    EXPECT_EQ(events, expected_trace);
    events.clear();

    EXPECT(!evaluate(vm, realm, "console.time('timer'); console.timeLog('timer', 'extra'); console.timeEnd('timer'); console.timeEnd('timer');"sv).is_error());
    EXPECT_EQ(events.size(), 3u);
    if (events.size() == 3u) {
        EXPECT(events[0].starts_with_bytes("TimeLog: \"timer: "sv));
        EXPECT(events[0].ends_with_bytes(" seconds\" \"extra\""sv));
        EXPECT(events[1].starts_with_bytes("TimeEnd: \"timer: "sv));
        EXPECT(events[1].ends_with_bytes(" seconds\""sv));
        EXPECT_EQ(events[2], "Warn: \"Timer 'timer' does not exist.\""_string);
    }
}

TEST_CASE(a_printer_that_throws_throws_from_the_console_method)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    install_recording_client(vm_with_realm, events);

    auto result = evaluate(vm, realm, "try { console.log('throw from the printer'); 0; } catch (error) { error; }"sv);
    EXPECT_EQ(result.value(), Value(7));

    result = evaluate(vm, realm, "console.log('%s', { toString() { throw 8; } })"sv);
    EXPECT(result.is_error());
    EXPECT_EQ(result.error_value(), Value(8));
    EXPECT(events.is_empty());
}

TEST_CASE(generically_format_values_prints_each_value)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    auto client = install_recording_client(vm_with_realm, events);

    GC::RootVector<Value> values;
    values.append(Value(1));
    values.append(js_undefined());
    values.append(MUST(evaluate(vm, realm, "'two'"sv)));
    values.append(MUST(evaluate(vm, realm, "[3, 4]"sv)));
    values.append(MUST(evaluate(vm, realm, "({ toString() { throw 9; } })"sv)));
    EXPECT_EQ(MUST(client->generically_format_values(values)), "1 undefined \"two\" [ 3, 4 ] Object{ \"toString\": [Function] toString }"sv);
    EXPECT_EQ(MUST(client->generically_format_values({})), Utf16String {});
}

static NEVER_INLINE void install_a_client_that_only_the_console_holds(VMWithRealm& vm_with_realm, Vector<String>& events)
{
    install_recording_client(vm_with_realm, events);
}

TEST_CASE(the_console_keeps_its_client_alive)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> events;
    install_a_client_that_only_the_console_holds(vm_with_realm, events);
    collect_garbage(vm);

    EXPECT(!evaluate(vm, realm, "console.log('after', 'collecting', 'garbage')"sv).is_error());
    Vector<String> expected_events { "Log: \"after\" \"collecting\" \"garbage\""_string };
    EXPECT_EQ(events, expected_events);
}

TEST_CASE(the_last_client_set_receives_what_the_console_logs)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    Vector<String> first_events;
    Vector<String> second_events;
    auto first_client = install_recording_client(vm_with_realm, first_events);
    install_recording_client(vm_with_realm, second_events);
    EXPECT_EQ(&first_client->console(), &vm_with_realm.console());

    EXPECT(!evaluate(vm, realm, "console.log('to the second client')"sv).is_error());
    EXPECT(first_events.is_empty());
    Vector<String> expected_events { "Log: \"to the second client\""_string };
    EXPECT_EQ(second_events, expected_events);

    vm_with_realm.console().set_client(*first_client);
    EXPECT(!evaluate(vm, realm, "console.log('to the first client')"sv).is_error());
    Vector<String> expected_first_events { "Log: \"to the first client\""_string };
    EXPECT_EQ(first_events, expected_first_events);
}

static String printed(VM& vm, Value value, bool strip_ansi = true, bool raw_strings = false)
{
    AllocatingMemoryStream stream;
    PrintContext print_context { .vm = vm, .stream = &stream, .strip_ansi = strip_ansi, .raw_strings = raw_strings };
    MUST(print(value, print_context));
    return MUST(String::from_stream(stream, stream.used_buffer_size()));
}

TEST_CASE(print_writes_values_as_the_console_shows_them)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto printed_result_of = [&](StringView source) {
        return printed(vm, MUST(evaluate(vm, realm, source)));
    };

    EXPECT_EQ(printed(vm, Value(42)), "42"sv);
    EXPECT_EQ(printed(vm, Value(-0.0)), "-0"sv);
    EXPECT_EQ(printed(vm, Value(1.5)), "1.5"sv);
    EXPECT_EQ(printed_result_of("NaN"sv), "NaN"sv);
    EXPECT_EQ(printed(vm, js_undefined()), "undefined"sv);
    EXPECT_EQ(printed(vm, js_null()), "null"sv);
    EXPECT_EQ(printed(vm, Value(true)), "true"sv);
    EXPECT_EQ(printed_result_of("'say \"hi\"'"sv), "\"say \"hi\"\""sv);
    EXPECT_EQ(printed_result_of("10n"sv), "10n"sv);
    EXPECT_EQ(printed_result_of("Symbol('description')"sv), "Symbol(description)"sv);
    EXPECT_EQ(printed_result_of("[1, 'two', [3]]"sv), "[ 1, \"two\", [ 3 ] ]"sv);
    EXPECT_EQ(printed_result_of("({ list: [1, 'two'], nested: { map: new Map([[3, 4]]) } })"sv),
        "Object{ \"list\": [ 1, \"two\" ], \"nested\": Object{ \"map\": [Map] { 3 => 4 } } }"sv);
    EXPECT_EQ(printed_result_of("new Set([1, 'two'])"sv), "[Set] { 1, \"two\" }"sv);
    EXPECT_EQ(printed_result_of("(function named() {})"sv), "[Function] named"sv);
    auto typed_array = printed_result_of("new Uint8Array([1, 2])"sv);
    EXPECT(typed_array.starts_with_bytes("[Uint8Array]\n  buffer: [ArrayBuffer] @ 0x"sv));
    EXPECT(typed_array.ends_with_bytes("\n  length: 2\n  byteLength: 2\n[ 1, 2 ]"sv));
    EXPECT_EQ(printed_result_of("new TypeError('boom')"sv), "[TypeError] boom"sv);
    EXPECT_EQ(printed_result_of("Promise.resolve(5)"sv), "[Promise]\n  state: Fulfilled\n  result: 5"sv);
    EXPECT(printed_result_of("const cyclic = {}; cyclic.self = cyclic; cyclic"sv).starts_with_bytes("Object{ \"self\": <already printed Object 0x"sv));
}

TEST_CASE(print_honors_the_options_of_its_context)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto string = MUST(evaluate(vm, realm, "'say \"hi\"'"sv));
    EXPECT_EQ(printed(vm, string, true, true), "say \"hi\""sv);

    auto object = MUST(evaluate(vm, realm, "({ a: [1] })"sv));
    auto in_color = printed(vm, object, false);
    EXPECT(in_color.contains('\x1b'));
    EXPECT_NE(in_color, printed(vm, object));

    Utf16StringBuilder builder;
    PrintContext print_context { .vm = vm, .builder = &builder, .strip_ansi = true };
    MUST(print(object, print_context));
    EXPECT_EQ(builder.to_string(), Utf16String::from_utf8(printed(vm, object)));

    Utf16StringBuilder builder_with_a_lone_surrogate;
    PrintContext print_context_with_a_lone_surrogate { .vm = vm, .builder = &builder_with_a_lone_surrogate, .strip_ansi = true, .raw_strings = true };
    MUST(print(MUST(evaluate(vm, realm, "'a\\ud800b'"sv)), print_context_with_a_lone_surrogate));
    auto printed_with_a_lone_surrogate = builder_with_a_lone_surrogate.to_string();
    EXPECT_EQ(printed_with_a_lone_surrogate.length_in_code_units(), 3u);
    EXPECT_EQ(printed_with_a_lone_surrogate.code_unit_at(1), 0xd800u);
}

namespace {

// A stream that takes `capacity` bytes and then fails.
class StreamThatFills final : public Stream {
public:
    explicit StreamThatFills(size_t capacity)
        : m_capacity(capacity)
    {
    }

    virtual ErrorOr<Bytes> read_some(Bytes) override { return AK::Error::from_errno(EBADF); }

    virtual ErrorOr<size_t> write_some(ReadonlyBytes bytes) override
    {
        if (m_written + bytes.size() > m_capacity)
            return AK::Error::from_errno(ENOSPC);
        m_written += bytes.size();
        return bytes.size();
    }

    virtual bool is_eof() const override { return true; }
    virtual bool is_open() const override { return true; }
    virtual void close() override { }

private:
    size_t m_capacity { 0 };
    size_t m_written { 0 };
};

}

TEST_CASE(print_returns_the_error_of_its_stream)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    StreamThatFills stream { 4 };
    PrintContext print_context { .vm = vm, .stream = &stream, .strip_ansi = true };
    auto result = print(MUST(evaluate(vm, realm, "[1, 2, 3, 4, 5, 6]"sv)), print_context);
    EXPECT(result.is_error());
    EXPECT_EQ(result.error().code(), ENOSPC);
}
