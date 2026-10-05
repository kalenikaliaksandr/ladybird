/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/QuickSort.h>
#include <AK/String.h>
#include <AK/StringBuilder.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibJS/Debugger.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibJS/SourceRange.h>
#include <LibTest/TestCase.h>

// The debugger that WebContent's DevTools attach to the VM: breakpoints, the pause callback with the paused stack and
// its bindings, evaluation in a paused frame, and stepping, as LibJS's users use them. The same expectations hold for
// the C++ runtime's LibJS and for the facade over the Rust one.

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
        if (vm->debugging_enabled())
            vm->disable_debugging();
        while (!vm->execution_context_stack().is_empty())
            vm->pop_execution_context();
    }

    Realm& realm() { return *realm_execution_context->realm; }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source, StringView filename)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm, filename);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static Debugger& enabled_debugger(VM& vm)
{
    vm.enable_debugging();
    VERIFY(vm.debugger());
    return *vm.debugger();
}

static StringView name_of(Debugger::PauseReason reason)
{
    switch (reason) {
    case Debugger::PauseReason::Entry:
        return "Entry"sv;
    case Debugger::PauseReason::Breakpoint:
        return "Breakpoint"sv;
    case Debugger::PauseReason::DebuggerStatement:
        return "DebuggerStatement"sv;
    case Debugger::PauseReason::Exception:
        return "Exception"sv;
    case Debugger::PauseReason::Step:
        return "Step"sv;
    }
    VERIFY_NOT_REACHED();
}

// A pause as "<reason> at <filename>:<line>", or "<reason> nowhere" for a pause without a source position.
static String describe(Debugger::PauseInfo const& pause)
{
    if (!pause.source_range.has_value())
        return MUST(String::formatted("{} nowhere", name_of(pause.reason)));
    return MUST(String::formatted("{} at {}:{}", name_of(pause.reason), pause.source_range->filename(), pause.source_range->start.line));
}

// Each frame of a paused stack as "[<function name>](<passed argument count>) at <filename>:<line>", where the argument
// count is left out for a frame that calls no function, and "without source" replaces the position for a frame that
// runs no code of a script, such as a native function's.
static Vector<String> describe_stack(Debugger::PauseInfo const& pause)
{
    Vector<String> frames;
    for (auto const& frame : pause.stack_trace) {
        VERIFY(frame.execution_context);
        auto const& execution_context = *frame.execution_context;
        StringBuilder builder;
        builder.appendff("[{}]", execution_context.function_name());
        if (execution_context.function)
            builder.appendff("({})", execution_context.passed_argument_count);
        if (frame.source_range.has_value())
            builder.appendff(" at {}:{}", frame.source_range->filename(), frame.source_range->start.line);
        else
            builder.append(" without source"sv);
        frames.append(MUST(builder.to_string()));
    }
    return frames;
}

// The bindings of a frame as "name=value" or "name=value (immutable)", sorted by name.
static Vector<String> describe_bindings(Debugger& debugger, ExecutionContext const& execution_context)
{
    Vector<String> bindings;
    for (auto const& binding : debugger.bindings_for_frame(execution_context)) {
        auto value = binding.value.is_special_empty_value() ? "<uninitialized>"_utf16 : binding.value.to_utf16_string_without_side_effects();
        bindings.append(MUST(String::formatted("{}={}{}", binding.name, value, binding.is_mutable ? ""sv : " (immutable)"sv)));
    }
    quick_sort(bindings);
    return bindings;
}

TEST_CASE(debugging_is_enabled_and_disabled)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    EXPECT(!vm.debugging_enabled());
    EXPECT_EQ(vm.debugger(), nullptr);

    vm.enable_debugging();
    EXPECT(vm.debugging_enabled());
    auto* debugger = vm.debugger();
    EXPECT_NE(debugger, nullptr);
    EXPECT_EQ(static_cast<VM const&>(vm).debugger(), debugger);
    EXPECT(!debugger->is_paused());

    vm.enable_debugging();
    EXPECT_EQ(vm.debugger(), debugger);

    vm.disable_debugging();
    EXPECT(!vm.debugging_enabled());
    EXPECT_EQ(vm.debugger(), nullptr);
}

TEST_CASE(a_pause_at_a_breakpoint_reports_the_stack_and_its_bindings)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    auto source = "function answer(argument) {\n    let value = 41;\n    const fixed = 'constant';\n    let sum = value + argument;\n    return sum;\n}\nanswer(0);\n"sv;
    EXPECT(!evaluate(vm, realm, source, "pause.js"sv).is_error());
    auto breakpoint_id = MUST(debugger.add_breakpoint(u"pause.js"sv, 5));

    size_t pause_count = 0;
    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        ++pause_count;
        EXPECT_EQ(describe(pause), "Breakpoint at pause.js:5"sv);
        EXPECT_EQ(pause.breakpoint_ids, Vector<BreakpointID> { breakpoint_id });
        EXPECT(!pause.exception.has_value());
        EXPECT(!pause.exception_will_be_caught);
        EXPECT(debugger.is_paused());

        Vector<String> expected_stack { "[answer](1) at pause.js:5"_string, "[] at call.js:1"_string, "[] without source"_string };
        EXPECT_EQ(describe_stack(pause), expected_stack);
        if (pause.stack_trace.size() != expected_stack.size()) {
            debugger.continue_execution();
            return;
        }
        auto const& paused_frame = pause.stack_trace[0];
        auto const& calling_frame = pause.stack_trace[1];
        EXPECT_EQ(paused_frame.source_range->code.ptr(), pause.source_range->code.ptr());
        EXPECT_NE(calling_frame.source_range->code.ptr(), pause.source_range->code.ptr());
        EXPECT_EQ(paused_frame.execution_context->argument(0), Value(1));

        Vector<String> expected_bindings { "argument=1"_string, "fixed=constant (immutable)"_string, "sum=42"_string, "value=41"_string };
        EXPECT_EQ(describe_bindings(debugger, *paused_frame.execution_context), expected_bindings);

        EXPECT_EQ(debugger.evaluate_in_frame(*paused_frame.execution_context, u"sum = value + argument + 8"sv).value(), Value(50));
        auto thrown = debugger.evaluate_in_frame(*paused_frame.execution_context, u"fixed = 1"sv);
        EXPECT(thrown.is_error());
        EXPECT(thrown.error_value().is_object());
        debugger.continue_execution();
    });

    EXPECT_EQ(MUST(evaluate(vm, realm, "answer(1);"sv, "call.js"sv)), Value(50));
    EXPECT_EQ(pause_count, 1u);
    EXPECT(!debugger.is_paused());
}

TEST_CASE(a_paused_stack_includes_native_frames_and_the_realm_frame)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    Vector<Vector<String>> paused_stacks;
    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        paused_stacks.append(describe_stack(pause));
        debugger.continue_execution();
    });

    auto source = "function inner() {\n    debugger;\n}\nfunction outer() {\n    [1].map(inner);\n}\nouter();\n"sv;
    EXPECT(!evaluate(vm, realm, source, "stack.js"sv).is_error());

    Vector<Vector<String>> expected_stacks { {
        "[inner](3) at stack.js:2"_string,
        "[](1) without source"_string,
        "[outer](0) at stack.js:5"_string,
        "[] at stack.js:7"_string,
        "[] without source"_string,
    } };
    EXPECT_EQ(paused_stacks, expected_stacks);
}

TEST_CASE(breakpoints_are_added_resolved_and_removed)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    auto by_filename = MUST(debugger.add_breakpoint(u"list.js"sv, 6));
    EXPECT(!debugger.is_breakpoint_resolved(by_filename));
    EXPECT_EQ(MUST(debugger.add_breakpoint(u"list.js"sv, 6)), by_filename);
    auto with_a_column = MUST(debugger.add_breakpoint(u"list.js"sv, 6, 1u));
    EXPECT_NE(with_a_column, by_filename);
    EXPECT(debugger.remove_breakpoint(with_a_column));

    auto refused = debugger.add_breakpoint(u"list.js"sv, 0);
    EXPECT(refused.is_error());
    EXPECT_EQ(refused.error().string_literal(), "Breakpoint line must be greater than zero"sv);

    Vector<String> pauses;
    Optional<NonnullRefPtr<SourceCode const>> paused_source_code;
    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        pauses.append(describe(pause));
        if (!paused_source_code.has_value())
            paused_source_code = pause.source_range->code;
        debugger.continue_execution();
    });

    auto source = "function list() {\n    let first = 1;\n    let second = 2;\n    return first + second;\n}\nlist();\n"sv;
    EXPECT(!evaluate(vm, realm, source, "list.js"sv).is_error());
    EXPECT(debugger.is_breakpoint_resolved(by_filename));
    Vector<String> expected_pauses { "Breakpoint at list.js:6"_string };
    EXPECT_EQ(pauses, expected_pauses);
    pauses.clear();

    EXPECT(debugger.remove_breakpoint(by_filename));
    EXPECT(!debugger.remove_breakpoint(by_filename));
    EXPECT(!debugger.is_breakpoint_resolved(by_filename));
    EXPECT(!evaluate(vm, realm, "list();"sv, "call.js"sv).is_error());
    EXPECT(pauses.is_empty());

    // The function has been compiled, so a breakpoint in its source code resolves in it right away.
    VERIFY(paused_source_code.has_value());
    auto in_source_code = MUST(debugger.add_breakpoint(*paused_source_code, 4));
    EXPECT(debugger.is_breakpoint_resolved(in_source_code));
    EXPECT_EQ(MUST(debugger.add_breakpoint(*paused_source_code, 4)), in_source_code);
    EXPECT(!evaluate(vm, realm, "list();"sv, "call.js"sv).is_error());
    expected_pauses = { "Breakpoint at list.js:4"_string };
    EXPECT_EQ(pauses, expected_pauses);
    pauses.clear();

    // Source code with the same filename and text is other source code, which the breakpoint leaves alone.
    EXPECT(!evaluate(vm, realm, source, "list.js"sv).is_error());
    EXPECT(pauses.is_empty());
}

TEST_CASE(pauses_at_exceptions_debugger_statements_steps_and_entries)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    Vector<String> pauses;
    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        auto description = describe(pause);
        if (pause.exception.has_value())
            description = MUST(String::formatted("{} with {}{}", description, pause.exception->to_utf16_string_without_side_effects(), pause.exception_will_be_caught ? " (caught)"sv : ""sv));
        pauses.append(move(description));
        debugger.continue_execution(pause.reason == Debugger::PauseReason::DebuggerStatement ? Debugger::ResumeMode::StepOver : Debugger::ResumeMode::Continue);
    });

    debugger.set_pause_on_exceptions(Debugger::PauseOnExceptions::All);
    EXPECT(!evaluate(vm, realm, "try {\n    throw 42;\n} catch {}\n"sv, "caught.js"sv).is_error());
    debugger.did_finish_exception_propagation(Value(42));

    debugger.set_pause_on_exceptions(Debugger::PauseOnExceptions::Uncaught);
    EXPECT(!evaluate(vm, realm, "try {\n    throw 43;\n} catch {}\n"sv, "ignored.js"sv).is_error());
    auto uncaught = evaluate(vm, realm, "\nthrow 44;\n"sv, "uncaught.js"sv);
    EXPECT_EQ(uncaught.error_value(), Value(44));
    debugger.did_finish_exception_propagation(Value(44));

    debugger.set_pause_on_exceptions(Debugger::PauseOnExceptions::None);
    EXPECT(evaluate(vm, realm, "throw 45;"sv, "unpaused.js"sv).is_error());

    EXPECT(!evaluate(vm, realm, "debugger;\nlet after = 1;\n"sv, "step.js"sv).is_error());

    debugger.request_pause_on_next_bytecode_execution();
    EXPECT(!evaluate(vm, realm, "\nlet entered = 1;\n"sv, "entry.js"sv).is_error());
    EXPECT(!evaluate(vm, realm, "let not_entered = 1;\n"sv, "after-entry.js"sv).is_error());

    Vector<String> expected_pauses {
        "Exception at caught.js:2 with 42 (caught)"_string,
        "Exception at uncaught.js:2 with 44"_string,
        "DebuggerStatement at step.js:1"_string,
        "Step at step.js:2"_string,
        "Entry at entry.js:2"_string,
    };
    EXPECT_EQ(pauses, expected_pauses);
}

TEST_CASE(stepping_into_over_and_out_of_calls)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    auto source = R"(function inner() {
    let x = 1;
    return x;
}
function outer() {
    debugger;
    inner();
    inner();
    return 0;
}
outer();
var done = 1;
)"sv;

    auto run_with_resume_modes = [&](Vector<Debugger::ResumeMode> resume_modes) {
        Vector<String> pauses;
        size_t pause_index = 0;
        debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
            pauses.append(MUST(String::formatted("{} in {}", describe(pause), pause.stack_trace.first().execution_context->function_name())));
            auto resume_mode = pause_index < resume_modes.size() ? resume_modes[pause_index] : Debugger::ResumeMode::Continue;
            ++pause_index;
            debugger.continue_execution(resume_mode);
        });
        EXPECT(!evaluate(vm, realm, source, "steps.js"sv).is_error());
        return pauses;
    };

    Vector<String> expected_pauses {
        "DebuggerStatement at steps.js:6 in outer"_string,
        "Step at steps.js:7 in outer"_string,
        "Step at steps.js:2 in inner"_string,
        "Step at steps.js:3 in inner"_string,
        "Step at steps.js:8 in outer"_string,
    };
    EXPECT_EQ(run_with_resume_modes({ Debugger::ResumeMode::StepOver, Debugger::ResumeMode::StepInto, Debugger::ResumeMode::StepOver, Debugger::ResumeMode::StepOut }), expected_pauses);

    expected_pauses = {
        "DebuggerStatement at steps.js:6 in outer"_string,
        "Step at steps.js:7 in outer"_string,
        "Step at steps.js:8 in outer"_string,
        "Step at steps.js:9 in outer"_string,
    };
    EXPECT_EQ(run_with_resume_modes({ Debugger::ResumeMode::StepOver, Debugger::ResumeMode::StepOver, Debugger::ResumeMode::StepOver }), expected_pauses);

    expected_pauses = {
        "DebuggerStatement at steps.js:6 in outer"_string,
        "Step at steps.js:12 in "_string,
    };
    EXPECT_EQ(run_with_resume_modes({ Debugger::ResumeMode::StepOut }), expected_pauses);
}

TEST_CASE(a_filtered_pause_keeps_the_step_in_progress)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    // The function is compiled before the breakpoint in it is set.
    EXPECT(!evaluate(vm, realm, "function called() {\n    return 1;\n}\ncalled();\n"sv, "called.js"sv).is_error());
    MUST(debugger.add_breakpoint(u"called.js"sv, 2));

    auto pauses_when_breakpoints_resume_with = [&](Function<void()> resume_from_a_breakpoint) {
        Vector<String> pauses;
        debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
            pauses.append(describe(pause));
            if (pause.reason == Debugger::PauseReason::Breakpoint)
                resume_from_a_breakpoint();
            else
                debugger.continue_execution(Debugger::ResumeMode::StepOver);
        });
        EXPECT(!evaluate(vm, realm, "debugger;\ncalled();\nvar after = 2;\n"sv, "filtered.js"sv).is_error());
        return pauses;
    };

    Vector<String> expected_pauses {
        "DebuggerStatement at filtered.js:1"_string,
        "Step at filtered.js:2"_string,
        "Breakpoint at called.js:2"_string,
        "Step at filtered.js:3"_string,
    };
    EXPECT_EQ(pauses_when_breakpoints_resume_with([&] { debugger.continue_execution_preserving_step_state(); }), expected_pauses);

    expected_pauses = {
        "DebuggerStatement at filtered.js:1"_string,
        "Step at filtered.js:2"_string,
        "Breakpoint at called.js:2"_string,
    };
    EXPECT_EQ(pauses_when_breakpoints_resume_with([&] { debugger.continue_execution(); }), expected_pauses);
}

TEST_CASE(the_pause_callback_runs_javascript_and_is_replaced)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    Vector<String> pauses;
    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        pauses.append(describe(pause));
        // A script that runs while paused runs to its end, without pausing at its own debugger statement.
        EXPECT_EQ(MUST(evaluate(vm, realm, "debugger; 6 * 7;"sv, "nested.js"sv)), Value(42));
        EXPECT_EQ(debugger.evaluate_in_frame(*pause.stack_trace.first().execution_context, u"first + 1"sv).value(), Value(2));
        debugger.continue_execution();
    });
    EXPECT(!evaluate(vm, realm, "let first = 1;\ndebugger;\n"sv, "first.js"sv).is_error());

    debugger.set_pause_callback([&](Debugger::PauseInfo const& pause) {
        pauses.append(MUST(String::formatted("replacement: {}", describe(pause))));
        debugger.continue_execution();
    });
    EXPECT(!evaluate(vm, realm, "debugger;\n"sv, "second.js"sv).is_error());

    Vector<String> expected_pauses {
        "DebuggerStatement at first.js:2"_string,
        "replacement: DebuggerStatement at second.js:1"_string,
    };
    EXPECT_EQ(pauses, expected_pauses);

    debugger.set_pause_callback({});
    EXPECT(!evaluate(vm, realm, "debugger;"sv, "unreported.js"sv).is_error());
    EXPECT_EQ(pauses.size(), 2u);
}

// Installs a pause callback that only the debugger holds.
static NEVER_INLINE void install_pause_callback_that_counts(Debugger& debugger, size_t& pause_count)
{
    debugger.set_pause_callback([&debugger, &pause_count](Debugger::PauseInfo const&) {
        ++pause_count;
        debugger.continue_execution();
    });
}

static NEVER_INLINE void collect_garbage(VM& vm)
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    vm.heap().collect_garbage();
}

TEST_CASE(the_debugger_keeps_its_pause_callback)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& debugger = enabled_debugger(vm);

    size_t pause_count = 0;
    install_pause_callback_that_counts(debugger, pause_count);
    collect_garbage(vm);
    EXPECT(!evaluate(vm, realm, "debugger;"sv, "kept.js"sv).is_error());
    EXPECT_EQ(pause_count, 1u);
}
