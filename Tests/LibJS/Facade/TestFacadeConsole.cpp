/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/String.h>
#include <AK/StringBuilder.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/RootVector.h>
#include <LibJS/Console.h>
#include <LibJS/Runtime/ConsoleObject.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// The console of a realm and the embedder's console clients that receive what it logs, as LibJS's users use them. The
// same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

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
