/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Function.h>
#include <LibGC/Heap.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>
#include <thread>

#include "EmbeddingTest.h"

// One of the embedder's GC cells, like the ClassicScript that LibWeb keeps as the [[HostDefined]] of a script.
class ScriptOfTheEmbedder final : public GC::Cell {
    GC_CELL(ScriptOfTheEmbedder, GC::Cell);
    GC_DECLARE_ALLOCATOR(ScriptOfTheEmbedder);

public:
    explicit ScriptOfTheEmbedder(u64 script_id)
        : m_script_id(script_id)
    {
    }

    u64 script_id() const { return m_script_id; }

private:
    u64 m_script_id { 0 };
};

GC_DEFINE_ALLOCATOR(ScriptOfTheEmbedder);

namespace {

Utf16String to_string(EmbeddedVM& embedded_vm, JSValue value)
{
    JSOwnedUtf16String string {};
    VERIFY(js_value_to_utf16_string(embedded_vm.vm(), value, &string).variant == JS_COMPLETION_NORMAL);
    return Utf16String::adopt_raw(string);
}

Utf16String string_of_normal_completion(EmbeddedVM& embedded_vm, JSCompletion completion)
{
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return to_string(embedded_vm, completion.payload);
}

void define_global(EmbeddedVM& embedded_vm, Utf16FlyString const& name, JSValue value)
{
    JSPropertyKey key { name.raw_identity() };
    js_object_define_direct_property(embedded_vm.vm(), embedded_vm.global_object(), &key, value, JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_CONFIGURABLE);
}

struct ParserError {
    Utf16String message;
    u32 line { 0 };
    u32 column { 0 };
};

// Collects the syntax errors that a sink receives, and runs a script and collects garbage for each of them, as an
// embedder that reports errors through JavaScript may.
struct ReenteringParserErrorCollector {
    EmbeddedVM& embedded_vm;
    Vector<ParserError> errors;
    Vector<Utf16String> results_of_scripts_run_while_appending;

    JSParserErrorSink sink()
    {
        return { this, [](void* context, JSOwnedUtf16String message, u32 line, u32 column) {
                    auto& collector = *static_cast<ReenteringParserErrorCollector*>(context);
                    collector.embedded_vm.collect_garbage();
                    auto result = collector.embedded_vm.evaluate("[1, 2].map(x => x * 2).join()"sv);
                    collector.results_of_scripts_run_while_appending.append(string_of_normal_completion(collector.embedded_vm, result));
                    collector.errors.append({ Utf16String::adopt_raw(message), line, column });
                } };
    }
};

// Collects garbage once the stack below the caller no longer holds pointers left over from earlier calls, which the
// conservative scan would treat as roots.
NEVER_INLINE void collect_garbage()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    GC::Heap::the().collect_garbage();
}

NEVER_INLINE JSScript* parse_script_whose_host_defined_cell_only_it_keeps(EmbeddedVM& embedded_vm, StringView source, GC::Weak<ScriptOfTheEmbedder>& host_defined_cell)
{
    auto host_defined = GC::Heap::the().allocate<ScriptOfTheEmbedder>(7u);
    host_defined_cell = host_defined;
    return js_script_parse(embedded_vm.vm(), embedded_vm.realm(), ascii_view(source), ascii_view("https://example.com/script.js"sv), ascii_view("display.js"sv), host_defined.ptr(), 1, nullptr);
}

JSSourceCode const* create_source_code(StringView filename, StringView code)
{
    return js_source_code_create(Utf16String::from_utf8(filename).into_raw(), Utf16String::from_utf8(code).into_raw());
}

struct ProgramFromAnotherThread {
    JSCompiledProgram* compiled { nullptr };
    JSParsedProgram* parsed_with_errors { nullptr };
};

// Parses and compiles the code of the source code on another thread, as LibWeb does while it fetches a script. The
// worker owns the code it parses, which it releases when it is done.
ProgramFromAnotherThread parse_and_compile_on_another_thread(JSSourceCode const* source_code, JSProgramType program_type, size_t line_number_offset)
{
    ProgramFromAnotherThread program;
    std::thread worker([&program, code = Utf16String::adopt_raw(js_source_code_code(source_code)), program_type, line_number_offset] {
        auto* parsed = js_compile_parse(abi_view_of(code.utf16_view()), program_type, line_number_offset);
        if (js_compile_parsed_program_has_errors(parsed))
            program.parsed_with_errors = parsed;
        else
            program.compiled = js_compile_parsed_program(parsed);
    });
    worker.join();
    return program;
}

// The embedder's side of compiling lazy functions off thread, as LibWeb's thread pool and event loop provide it. It
// runs each task right away on the thread that hands it over, or queues it for the test to run on a thread of its
// choice.
struct OffThreadCompilationHost {
    bool runs_tasks_right_away { true };
    Vector<JSOffThreadTask> submitted_work;
    Vector<JSOffThreadTask> tasks_posted_to_the_vms_thread;
    size_t submit_count { 0 };
    Atomic<size_t> release_count { 0 };
    // A script that the first submit_work and every post_to_main_thread on the VM's thread run, with garbage collected
    // around it, before they hand the task over.
    EmbeddedVM* vm_to_reenter { nullptr };
    StringView script_to_reenter_the_vm_with;

    void reenter_the_vm()
    {
        if (!vm_to_reenter)
            return;
        collect_garbage();
        VERIFY(vm_to_reenter->run(script_to_reenter_the_vm_with));
        collect_garbage();
    }

    JSOffThreadCompilationCallbacks callbacks()
    {
        return {
            .context = this,
            .submit_work = [](void* context, JSOffThreadTask task) {
                auto& host = *static_cast<OffThreadCompilationHost*>(context);
                if (++host.submit_count == 1)
                    host.reenter_the_vm();
                if (host.runs_tasks_right_away)
                    task.run(task.data);
                else
                    host.submitted_work.append(task); },
            .post_to_main_thread = [](void* context, JSOffThreadTask task) {
                auto& host = *static_cast<OffThreadCompilationHost*>(context);
                if (host.runs_tasks_right_away) {
                    host.reenter_the_vm();
                    task.run(task.data);
                } else {
                    host.tasks_posted_to_the_vms_thread.append(task);
                } },
            .release = [](void* context) { ++static_cast<OffThreadCompilationHost*>(context)->release_count; },
        };
    }

    void run_submitted_work_on_a_worker()
    {
        std::thread worker([work = move(submitted_work)] {
            for (auto task : work)
                task.run(task.data);
        });
        worker.join();
    }

    void run_tasks_posted_to_the_vms_thread()
    {
        for (auto task : exchange(tasks_posted_to_the_vms_thread, {}))
            task.run(task.data);
    }
};

// The top-level function declaration is not one of the functions that the top-level code creates: declaration
// instantiation creates it, so, as in C++, it stays lazy.
constexpr StringView script_with_lazy_functions = "function declared() { return 'declared' }\nvar plain = function plain() { return 'plain' };\nvar outer = function outer() { function inner() { return /i+/.exec('ii')[0] } return inner };\nvar arrow = () => 'arrow';"sv;

JSScript* parse_and_run_script_with_lazy_functions(EmbeddedVM& embedded_vm)
{
    auto* script = js_script_parse(embedded_vm.vm(), embedded_vm.realm(), ascii_view(script_with_lazy_functions), ascii_view("lazy.js"sv), ascii_view(""sv), nullptr, 1, nullptr);
    VERIFY(js_script_run(embedded_vm.vm(), script, nullptr).variant == JS_COMPLETION_NORMAL);
    return script;
}

}

TEST_CASE(scripts_keep_their_host_defined_cell_and_run_with_an_overriding_lexical_environment)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    EXPECT(embedded_vm->run("var shadowed = 1"sv));

    // The console of DevTools runs its input with an override, a scope over the global environment.
    auto* override_environment = js_environment_new_declarative_environment(vm, embedded_vm->global_environment());
    EXPECT_EQ(js_environment_create_mutable_binding(vm, override_environment, ascii_view("shadowed"sv), false).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_environment_initialize_binding(vm, override_environment, ascii_view("shadowed"sv), embedded_vm->value_of("'override'"sv), JS_INITIALIZE_BINDING_HINT_NORMAL).variant, JS_COMPLETION_NORMAL);

    GC::Weak<ScriptOfTheEmbedder> host_defined_cell;
    auto source = "[typeof shadowed, String(shadowed), eval('shadowed'), new Error().stack.includes('display.js:1:')].join()"sv;
    auto* script = parse_script_whose_host_defined_cell_only_it_keeps(*embedded_vm, source, host_defined_cell);
    VERIFY(script);
    collect_garbage();
    EXPECT(host_defined_cell.ptr());
    EXPECT_EQ(js_script_host_defined(script), static_cast<void*>(host_defined_cell.ptr().ptr()));
    EXPECT_EQ(static_cast<ScriptOfTheEmbedder*>(js_script_host_defined(script))->script_id(), 7u);
    EXPECT_EQ(js_script_realm(script), embedded_vm->realm());

    // As in C++, the override is what typeof and eval resolve through, but the global variable access that the
    // script compiles a plain reference to reads the global environment.
    EXPECT_EQ(string_of_normal_completion(*embedded_vm, js_script_run(vm, script, override_environment)), u"string,1,override,true"sv);
    EXPECT_EQ(string_of_normal_completion(*embedded_vm, js_script_run(vm, script, nullptr)), u"number,1,1,true"sv);

    auto* throwing = js_script_parse(vm, embedded_vm->realm(), ascii_view("throw 42"sv), ascii_view("throwing.js"sv), ascii_view(""sv), nullptr, 1, nullptr);
    EXPECT(!js_script_host_defined(throwing));
    auto completion = js_script_run(vm, throwing, nullptr);
    EXPECT_EQ(completion.variant, JS_COMPLETION_THROW);
    EXPECT_EQ(completion.payload, int32_value(42));
}

TEST_CASE(syntax_errors_reach_the_sink_with_lines_counted_from_the_offset)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    ReenteringParserErrorCollector collector { *embedded_vm, {}, {} };
    auto sink = collector.sink();
    auto* script = js_script_parse(embedded_vm->vm(), embedded_vm->realm(), ascii_view("let a = 1;\nlet b = ;"sv), ascii_view("broken.js"sv), ascii_view(""sv), nullptr, 10, &sink);
    EXPECT(!script);
    EXPECT_EQ(collector.errors.size(), 1u);
    EXPECT(!collector.errors[0].message.is_empty());
    EXPECT_EQ(collector.errors[0].line, 11u);
    EXPECT_EQ(collector.errors[0].column, 9u);
    EXPECT_EQ(collector.results_of_scripts_run_while_appending, Vector<Utf16String> { "2,4"_utf16 });

    // Without a sink, the errors are dropped.
    EXPECT(!js_script_parse(embedded_vm->vm(), embedded_vm->realm(), ascii_view("("sv), ascii_view(""sv), ascii_view(""sv), nullptr, 1, nullptr));
}

TEST_CASE(source_code_is_shared_by_reference_counting)
{
    auto code = Utf16String::from_utf16(u"let α = 1;"sv);
    auto const* code_units = code.utf16_view().utf16_span().data();
    auto const* source_code = js_source_code_create(move("file.js"_utf16).into_raw(), move(code).into_raw());
    EXPECT_EQ(Utf16String::adopt_raw(js_source_code_filename(source_code)), u"file.js"sv);
    auto shared_code = Utf16String::adopt_raw(js_source_code_code(source_code));
    EXPECT_EQ(shared_code, u"let α = 1;"sv);
    EXPECT_EQ(shared_code.utf16_view().utf16_span().data(), code_units);
    EXPECT_EQ(js_source_code_length_in_code_units(source_code), 10u);

    js_source_code_retain(source_code);
    js_source_code_release(source_code);
    EXPECT_EQ(js_source_code_length_in_code_units(source_code), 10u);
    js_source_code_release(source_code);
}

TEST_CASE(execution_contexts_report_the_source_code_of_their_bytecode)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    JSSourceCode const* source_code_of_the_closure = nullptr;
    JSSourceCode const* source_code_of_its_caller = nullptr;
    auto function = GC::create_function(GC::Heap::the(), [&](JSVM* vm) -> JSCompletion {
        source_code_of_the_closure = js_source_code_of_execution_context(js_execution_context_running(vm));
        auto* caller = js_execution_context_last_matching(
            vm, [](void*, JSExecutionContext* execution_context) {
                return js_source_code_of_execution_context(execution_context) != nullptr;
            },
            nullptr);
        source_code_of_its_caller = js_source_code_of_execution_context(caller);
        js_source_code_retain(source_code_of_its_caller);
        return { js_undefined, JS_COMPLETION_NORMAL };
    });
    auto* closure = js_function_create_closure_with_name(embedded_vm->vm(), embedded_vm->realm(), ascii_view("capture"sv), [](void* context, JSVM* vm) { return static_cast<GC::Function<JSCompletion(JSVM*)>*>(context)->function()(vm); }, function.ptr());
    define_global(*embedded_vm, "capture"_utf16_fly_string, value_of_object(closure));

    EXPECT_EQ(embedded_vm->evaluate("(function caller() { capture(); })()"sv, "caller.js"sv).variant, JS_COMPLETION_NORMAL);
    EXPECT(!source_code_of_the_closure);
    VERIFY(source_code_of_its_caller);
    EXPECT_EQ(Utf16String::adopt_raw(js_source_code_filename(source_code_of_its_caller)), u"caller.js"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_source_code_code(source_code_of_its_caller)), u"(function caller() { capture(); })()"sv);
    js_source_code_release(source_code_of_its_caller);
}

TEST_CASE(scripts_and_modules_compiled_on_another_thread_become_records_on_the_vms_thread)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    auto const* source_code = create_source_code("https://example.com/worker.js"sv, "function twice(x) { return 2 * x }\n(function () { return twice(21) })()"sv);
    auto program = parse_and_compile_on_another_thread(source_code, JS_PROGRAM_TYPE_SCRIPT, 1);
    VERIFY(program.compiled);
    auto host_defined = GC::Heap::the().allocate<ScriptOfTheEmbedder>(1u);
    auto* script = js_compile_create_script_from_compiled_program(vm, program.compiled, source_code, embedded_vm->realm(), ascii_view("https://example.com/worker.js"sv), host_defined.ptr());
    EXPECT_EQ(js_compile_top_level_source_code_of_script(script), source_code);
    EXPECT_EQ(js_script_host_defined(script), static_cast<void*>(host_defined.ptr()));
    // The script keeps the source code alive once the embedder lets go of it.
    js_source_code_release(source_code);
    auto completion = js_script_run(vm, script, nullptr);
    EXPECT_EQ(completion.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(completion.payload, int32_value(42));
    EXPECT_EQ(string_of_normal_completion(*embedded_vm, embedded_vm->evaluate("twice.toString()"sv)), u"function twice(x) { return 2 * x }"sv);

    auto const* module_source_code = create_source_code("https://example.com/module.mjs"sv, "export default function twice(x) { return 2 * x }"sv);
    auto module_program = parse_and_compile_on_another_thread(module_source_code, JS_PROGRAM_TYPE_MODULE, 1);
    VERIFY(module_program.compiled);
    auto* module = js_compile_create_module_from_compiled_program(vm, module_program.compiled, module_source_code, embedded_vm->realm(), ascii_view("https://example.com/module.mjs"sv), host_defined.ptr());
    EXPECT(module);
    EXPECT_EQ(js_compile_top_level_source_code_of_module(module), module_source_code);

    // A module with top-level await compiles its body as an async function, and C++ reports no top-level source code
    // for it.
    auto const* awaiting_source_code = create_source_code("https://example.com/await.mjs"sv, "await null;"sv);
    auto awaiting_program = parse_and_compile_on_another_thread(awaiting_source_code, JS_PROGRAM_TYPE_MODULE, 1);
    VERIFY(awaiting_program.compiled);
    auto* awaiting_module = js_compile_create_module_from_compiled_program(vm, awaiting_program.compiled, awaiting_source_code, embedded_vm->realm(), ascii_view("https://example.com/await.mjs"sv), nullptr);
    EXPECT(!js_compile_top_level_source_code_of_module(awaiting_module));
    js_source_code_release(module_source_code);
    js_source_code_release(awaiting_source_code);
}

TEST_CASE(syntax_errors_found_on_another_thread_reach_the_vms_thread)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto const* source_code = create_source_code("broken.js"sv, "let ok = 1;\nlet broken = ;"sv);
    auto program = parse_and_compile_on_another_thread(source_code, JS_PROGRAM_TYPE_SCRIPT, 1);
    VERIFY(program.parsed_with_errors);

    ReenteringParserErrorCollector collector { *embedded_vm, {}, {} };
    auto sink = collector.sink();
    EXPECT(!js_compile_create_script_from_parsed_program(vm, program.parsed_with_errors, source_code, embedded_vm->realm(), ascii_view("broken.js"sv), nullptr, &sink));
    EXPECT_EQ(collector.errors.size(), 1u);
    EXPECT_EQ(collector.errors[0].line, 2u);
    EXPECT_EQ(collector.errors[0].column, 14u);
    EXPECT_EQ(collector.results_of_scripts_run_while_appending.size(), 1u);
    js_source_code_release(source_code);

    // A program that parsed without errors on the VM's thread is compiled when it becomes a script.
    auto const* valid_source_code = create_source_code("valid.js"sv, "6 * 7"sv);
    auto* parsed = js_compile_parse(ascii_view("6 * 7"sv), JS_PROGRAM_TYPE_SCRIPT, 1);
    auto* script = js_compile_create_script_from_parsed_program(vm, parsed, valid_source_code, embedded_vm->realm(), ascii_view("valid.js"sv), nullptr, &sink);
    js_source_code_release(valid_source_code);
    EXPECT_EQ(js_script_run(vm, script, nullptr).payload, int32_value(42));

    // Programs that will not run are destroyed on any thread.
    std::thread worker([] {
        js_compile_compiled_program_destroy(js_compile_parsed_program(js_compile_parse(ascii_view("/a+/.test('aa')"sv), JS_PROGRAM_TYPE_SCRIPT, 1)));
        js_compile_parsed_program_destroy(js_compile_parse(ascii_view("let"sv), JS_PROGRAM_TYPE_MODULE, 1));
    });
    worker.join();
}

TEST_CASE(lazy_functions_compile_with_callbacks_that_run_the_work_right_away)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* script = parse_and_run_script_with_lazy_functions(*embedded_vm);

    // The script that runs while the work is submitted calls outer(), which then compiles on the VM's thread, so the
    // functions it creates are compiled off thread in a second round. It runs again whenever a task is posted.
    OffThreadCompilationHost host;
    host.vm_to_reenter = embedded_vm.ptr();
    host.script_to_reenter_the_vm_with = "typeof outer() === 'function'"sv;
    auto callbacks = host.callbacks();
    js_compile_remaining_functions_of_script_off_thread(embedded_vm->vm(), script, &callbacks);
    EXPECT_EQ(host.submit_count, 2u);
    EXPECT_EQ(host.release_count.load(), 1u);
    EXPECT_EQ(string_of_normal_completion(*embedded_vm, embedded_vm->evaluate("[declared(), plain(), outer()(), arrow()].join()"sv)), u"declared,plain,ii,arrow"sv);
}

TEST_CASE(lazy_functions_compile_on_a_worker_thread_and_install_on_the_vms_thread)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* script = parse_and_run_script_with_lazy_functions(*embedded_vm);

    OffThreadCompilationHost host;
    host.runs_tasks_right_away = false;
    auto callbacks = host.callbacks();
    js_compile_remaining_functions_of_script_off_thread(embedded_vm->vm(), script, &callbacks);
    EXPECT_EQ(host.submit_count, 1u);

    // outer() starts running before the worker is done, so the worker compiles the functions it creates next.
    EXPECT(embedded_vm->run("typeof outer() === 'function'"sv));
    collect_garbage();
    host.run_submitted_work_on_a_worker();
    collect_garbage();
    EXPECT_EQ(host.release_count.load(), 0u);
    host.run_tasks_posted_to_the_vms_thread();
    EXPECT_EQ(host.submit_count, 2u);
    host.run_submitted_work_on_a_worker();
    host.run_tasks_posted_to_the_vms_thread();
    EXPECT_EQ(host.release_count.load(), 1u);
    EXPECT_EQ(string_of_normal_completion(*embedded_vm, embedded_vm->evaluate("[declared(), plain(), outer()(), arrow()].join()"sv)), u"declared,plain,ii,arrow"sv);

    // A module whose functions compile off thread, here one with top-level await, whose body is an async function.
    auto const* source_code = create_source_code("https://example.com/await.mjs"sv, "await null;\nglobalThis.later = () => 'later';"sv);
    auto program = parse_and_compile_on_another_thread(source_code, JS_PROGRAM_TYPE_MODULE, 1);
    auto* module = js_compile_create_module_from_compiled_program(embedded_vm->vm(), program.compiled, source_code, embedded_vm->realm(), ascii_view("https://example.com/await.mjs"sv), nullptr);
    js_source_code_release(source_code);
    OffThreadCompilationHost module_host;
    auto module_callbacks = module_host.callbacks();
    js_compile_remaining_functions_of_module_off_thread(embedded_vm->vm(), module, &module_callbacks);
    EXPECT_EQ(module_host.submit_count, 1u);
    EXPECT_EQ(module_host.release_count.load(), 1u);
}

TEST_CASE(tokens_cover_the_source_and_end_with_its_trailing_trivia)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    struct TokenCollector {
        EmbeddedVM& embedded_vm;
        Vector<JSToken> tokens;
    } collector { *embedded_vm, {} };
    JSTokenSink sink {
        .context = &collector,
        .append = [](void* context, JSToken const* token) {
            auto& collector = *static_cast<TokenCollector*>(context);
            // The sink may run JavaScript while the tokenizer waits for it.
            collector.embedded_vm.collect_garbage();
            VERIFY(collector.embedded_vm.run("[1, 2].includes(2)"sv));
            collector.tokens.append(*token);
        },
    };
    auto source = "let x = 1; // c"sv;
    js_compile_tokenize(ascii_view(source), &sink);

    u8 const expected_categories[] = { JS_TOKEN_CATEGORY_KEYWORD, JS_TOKEN_CATEGORY_IDENTIFIER, JS_TOKEN_CATEGORY_OPERATOR, JS_TOKEN_CATEGORY_NUMBER, JS_TOKEN_CATEGORY_PUNCTUATION, JS_TOKEN_CATEGORY_INVALID };
    VERIFY(collector.tokens.size() == array_size(expected_categories));
    u32 covered_up_to = 0;
    for (size_t i = 0; i < collector.tokens.size(); ++i) {
        auto const& token = collector.tokens[i];
        EXPECT_EQ(token.category, expected_categories[i]);
        EXPECT_EQ(token.trivia_offset, covered_up_to);
        EXPECT_EQ(token.offset, token.trivia_offset + token.trivia_length);
        covered_up_to = token.offset + token.length;
    }
    EXPECT_EQ(covered_up_to, source.length());
    auto const& end_of_file = collector.tokens.last();
    EXPECT_EQ(end_of_file.length, 0u);
    EXPECT_EQ(source.substring_view(end_of_file.trivia_offset, end_of_file.trivia_length), " // c"sv);
}

TEST_CASE(breakpoint_positions_match_the_cpp_runtime)
{
    struct PositionCollector {
        size_t calls { 0 };
        Vector<JSPosition> positions;
    } collector;
    JSPositionSink sink {
        .context = &collector,
        .append = [](void* context, JSPosition const* positions, size_t count) {
            auto& collector = *static_cast<PositionCollector*>(context);
            ++collector.calls;
            collector.positions.append(positions, count);
        },
    };
    auto positions_of = [&](StringView source, JSProgramType program_type, size_t line_number_offset) {
        collector.positions.clear();
        js_compile_breakpoint_positions_for_source(ascii_view(source), program_type, line_number_offset, &sink);
        Vector<u32> lines_and_columns;
        for (auto position : collector.positions)
            lines_and_columns.extend({ position.line, position.column });
        return lines_and_columns;
    };

    // The positions that C++ JS::breakpoint_positions_for_source() returns for the same sources. The one at line 3 is
    // inside a function that has not been compiled.
    auto source = "var a = 1;\nfunction f() {\n  return a;\n}\nf();"sv;
    EXPECT_EQ(positions_of(source, JS_PROGRAM_TYPE_SCRIPT, 1), (Vector<u32> { 1, 1, 3, 3, 3, 10, 5, 1, 5, 2 }));
    EXPECT_EQ(positions_of(source, JS_PROGRAM_TYPE_SCRIPT, 10), (Vector<u32> { 10, 1, 12, 3, 12, 10, 14, 1, 14, 2 }));
    EXPECT_EQ(positions_of("export function g() { return 1 }\nawait g();"sv, JS_PROGRAM_TYPE_MODULE, 0), (Vector<u32> { 1, 23, 2, 1, 2, 8 }));
    EXPECT_EQ(collector.calls, 3u);

    // A source with syntax errors has none, and the sink hears nothing.
    EXPECT(positions_of("function ("sv, JS_PROGRAM_TYPE_SCRIPT, 1).is_empty());
    EXPECT_EQ(collector.calls, 3u);

    // Like the tokenizer, the search needs no VM, so it runs on any thread.
    Vector<u32> positions_found_on_another_thread;
    std::thread worker([&] { positions_found_on_another_thread = positions_of(source, JS_PROGRAM_TYPE_SCRIPT, 1); });
    worker.join();
    EXPECT_EQ(positions_found_on_another_thread.size(), 10u);
}
