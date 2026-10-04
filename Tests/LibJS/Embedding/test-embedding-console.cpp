/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/StringBuilder.h>
#include <AK/StringView.h>
#include <AK/Utf16String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>

#include "EmbeddingTest.h"

namespace {

class TestHarness {
public:
    TestHarness()
        : m_embedded_vm(EmbeddedVM::create())
    {
        m_embedded_vm->initialize_realm();
    }

    JSVM* vm() const { return m_embedded_vm->vm(); }

    JSConsole* console() const { return js_console_object_console(js_realm_intrinsic(vm(), m_embedded_vm->realm(), JS_INTRINSIC_CONSOLE_OBJECT)); }

    JSCompletion run_script(StringView source, StringView filename) const { return m_embedded_vm->evaluate(source, filename); }

private:
    NonnullOwnPtr<EmbeddedVM> m_embedded_vm;
};

ByteString byte_string_of(JSUtf16View view)
{
    if (view.length_in_code_units == 0)
        return {};
    if (view.has_ascii_storage)
        return ByteString { static_cast<char const*>(view.data), view.length_in_code_units };
    return MUST((Utf16View { static_cast<char16_t const*>(view.data), view.length_in_code_units }.to_byte_string()));
}

struct ConsoleOutput {
    JSConsoleClient* client { nullptr };
    Vector<ByteString> messages;
};

ConsoleOutput s_console_output;

JSCompletion print_to_console_output(void*, JSVM* vm, uint8_t log_level, JSConsolePrinterArguments const* arguments)
{
    if (arguments->kind == JS_CONSOLE_PRINTER_ARGUMENTS_TRACE) {
        StringBuilder builder;
        builder.appendff("trace {}:", byte_string_of(arguments->label));
        for (size_t i = 0; i < arguments->trace_frame_count; ++i) {
            auto const& frame = arguments->trace_frames[i];
            builder.appendff(" {}", byte_string_of(frame.function_name));
            if (frame.has_source_file && frame.has_line && frame.has_column)
                builder.appendff("@{}:{}:{}", byte_string_of(frame.source_file), frame.line, frame.column);
        }
        s_console_output.messages.append(builder.to_byte_string());
        return { .payload = 0, .variant = JS_COMPLETION_NORMAL };
    }

    VERIFY(arguments->kind == JS_CONSOLE_PRINTER_ARGUMENTS_VALUES);
    VERIFY(log_level == JS_CONSOLE_LOG_LEVEL_LOG);
    size_t formatted = 0;
    auto completion = js_console_client_generically_format_values(vm, s_console_output.client, arguments->values, arguments->value_count, &formatted);
    if (completion.variant != JS_COMPLETION_NORMAL)
        return completion;
    auto message = Utf16String::adopt_raw(formatted);
    s_console_output.messages.append(ByteString::formatted("log {}", message));
    return completion;
}

constexpr JSConsoleClientMethods s_console_output_methods {
    .printer = print_to_console_output,
    .add_css_style_to_current_message = nullptr,
    .report_exception = nullptr,
    .clear = nullptr,
    .end_group = nullptr,
};

JSUtf16View view_of(StringView ascii)
{
    return { .data = ascii.characters_without_null_termination(), .length_in_code_units = ascii.length(), .has_ascii_storage = true };
}

ByteString printed(JSVM* vm, JSValue value)
{
    StringBuilder builder;
    JSByteSink sink {
        .context = &builder,
        .append = [](void* context, u8 const* bytes, size_t length) {
            static_cast<StringBuilder*>(context)->append(ReadonlyBytes { bytes, length });
            return true;
        },
    };
    VERIFY(js_console_print_value(vm, value, &sink, true, false));
    return builder.to_byte_string();
}

struct DebuggerPause {
    uint8_t reason { 0 };
    Vector<u32> breakpoint_ids;
    u32 line { 0 };
    Vector<ByteString> bindings;
    ByteString evaluated;
};

Vector<DebuggerPause> s_debugger_pauses;

struct FrameBindings {
    JSVM* vm { nullptr };
    Vector<ByteString> descriptions;
};

void record_debugger_pause(void*, JSVM* vm, JSDebuggerPauseInfo const* pause_info)
{
    DebuggerPause pause { .reason = pause_info->reason, .breakpoint_ids = {}, .line = pause_info->source_range.line, .bindings = {}, .evaluated = {} };
    for (size_t i = 0; i < pause_info->breakpoint_id_count; ++i)
        pause.breakpoint_ids.append(pause_info->breakpoint_ids[i]);

    VERIFY(pause_info->stack_frame_count > 0);
    auto* paused_execution_context = pause_info->stack_frames[0].execution_context;

    FrameBindings frame_bindings { .vm = vm, .descriptions = {} };
    JSDebuggerFrameBindingSink sink {
        .context = &frame_bindings,
        .append = [](void* context, JSDebuggerFrameBinding const* binding) {
            auto& frame_bindings = *static_cast<FrameBindings*>(context);
            frame_bindings.descriptions.append(ByteString::formatted("{}={}", byte_string_of(binding->name), printed(frame_bindings.vm, binding->value)));
        },
    };
    js_debugger_bindings_for_frame(vm, paused_execution_context, &sink);
    pause.bindings = move(frame_bindings.descriptions);

    auto completion = js_debugger_evaluate_in_frame(vm, paused_execution_context, view_of("sum = value + argument + 8"sv));
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    pause.evaluated = printed(vm, completion.payload);

    s_debugger_pauses.append(move(pause));
    js_debugger_continue_execution(vm, JS_RESUME_MODE_CONTINUE);
}

}

TEST_CASE(host_console_client_receives_log_arguments_and_a_trace)
{
    TestHarness harness;
    auto* console = harness.console();
    VERIFY(console);
    s_console_output = { .client = js_console_client_create(harness.vm(), console, &s_console_output_methods, nullptr), .messages = {} };
    js_console_set_client(console, s_console_output.client);

    auto source = "console.log('answer', 42, [1, 2]);\n"
                  "function traced() {\n"
                  "    console.trace('label %d', 5);\n"
                  "}\n"
                  "traced();\n"sv;
    EXPECT_EQ(harness.run_script(source, "console-test.js"sv).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(s_console_output.messages, (Vector<ByteString> { "log \"answer\" 42 [ 1, 2 ]"sv, "trace label 5: traced@console-test.js:3:18 <anonymous>@console-test.js:5:7 <anonymous>"sv }));
}

TEST_CASE(debugger_pauses_at_a_breakpoint_and_reads_the_bindings_of_the_frame)
{
    TestHarness harness;
    auto* vm = harness.vm();
    js_debugger_enable(vm);
    js_debugger_set_pause_callback(vm, record_debugger_pause, nullptr);
    s_debugger_pauses.clear();

    // The function is compiled before the breakpoint is set, so the breakpoint resolves inside it.
    auto source = "function answer(argument) {\n"
                  "    let value = 41;\n"
                  "    let sum = value + argument;\n"
                  "    return sum;\n"
                  "}\n"
                  "answer(0);\n"sv;
    EXPECT_EQ(harness.run_script(source, "debugger-test.js"sv).variant, JS_COMPLETION_NORMAL);
    auto breakpoint = js_debugger_add_breakpoint(vm, view_of("debugger-test.js"sv), 4, false, 0);
    EXPECT(breakpoint.error_message == nullptr);
    EXPECT(js_debugger_is_breakpoint_resolved(vm, breakpoint.breakpoint_id));

    auto completion = harness.run_script("answer(1);"sv, "call.js"sv);
    EXPECT_EQ(completion.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(printed(vm, completion.payload), "50"sv);

    EXPECT_EQ(s_debugger_pauses.size(), 1u);
    auto const& pause = s_debugger_pauses.first();
    EXPECT_EQ(pause.reason, JS_PAUSE_REASON_BREAKPOINT);
    EXPECT_EQ(pause.breakpoint_ids, Vector<u32> { breakpoint.breakpoint_id });
    EXPECT_EQ(pause.line, 4u);
    EXPECT_EQ(pause.bindings, (Vector<ByteString> { "argument=1"sv, "value=41"sv, "sum=42"sv }));
    EXPECT_EQ(pause.evaluated, "50"sv);

    js_debugger_disable(vm);
    EXPECT(!js_debugger_is_enabled(vm));
}
