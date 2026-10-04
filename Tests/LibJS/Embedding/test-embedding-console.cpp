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
