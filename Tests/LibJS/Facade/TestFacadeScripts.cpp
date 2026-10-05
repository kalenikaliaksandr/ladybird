/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Array.h>
#include <AK/ByteBuffer.h>
#include <AK/HashMap.h>
#include <AK/Mutex.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibCore/EventLoop.h>
#include <LibCore/ImmutableBytes.h>
#include <LibGC/Function.h>
#include <LibGC/Root.h>
#include <LibJS/CyclicModule.h>
#include <LibJS/DecodedBytecodeCache.h>
#include <LibJS/Heap/Cell.h>
#include <LibJS/HostObjectABI.h>
#include <LibJS/Module.h>
#include <LibJS/ModuleLoading.h>
#include <LibJS/ParserError.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/FunctionKind.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/HostModule.h>
#include <LibJS/Runtime/JobCallback.h>
#include <LibJS/Runtime/ModuleRequest.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PromiseJob.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibJS/ScriptCompilation.h>
#include <LibJS/SourceCode.h>
#include <LibJS/SourceRange.h>
#include <LibJS/SourceTextModule.h>
#include <LibJS/SyntheticModule.h>
#include <LibTest/TestCase.h>
#include <LibThreading/Thread.h>

// Classic scripts, modules that a host loads, programs parsed and compiled on other threads, and the bytecode cache,
// as LibJS's users use them. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust
// one.

using namespace JS;

namespace {

struct VMWithRealm {
    VMWithRealm()
        : vm(VM::create())
        , realm_execution_context(MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr)))
    {
        vm->host_enqueue_promise_job = [this](PromiseJob job, GC::Ptr<Realm>) {
            queued_promise_jobs.append(GC::make_root(GC::create_function(vm->heap(), [job = move(job)] {
                MUST(job.run());
            })));
        };
        vm->host_promise_job_queue_is_empty = [this] { return queued_promise_jobs.is_empty(); };
    }

    ~VMWithRealm()
    {
        while (!vm->execution_context_stack().is_empty())
            vm->pop_execution_context();
    }

    Realm& realm() { return *realm_execution_context->realm; }

    void run_queued_promise_jobs()
    {
        while (!queued_promise_jobs.is_empty()) {
            auto job = queued_promise_jobs.take_first();
            job->function()();
        }
    }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
};

// One of the embedder's cells, such as the script or module that a record's [[HostDefined]] names.
class HostDefinedCell final : public Cell {
    GC_CELL(HostDefinedCell, Cell);
    GC_DECLARE_ALLOCATOR(HostDefinedCell);

public:
    u64 identifier { 0 };
};

GC_DEFINE_ALLOCATOR(HostDefinedCell);

// The state of a host module of its own, which host_data_if() finds.
class HostModuleData final : public GC::Cell {
    GC_CELL(HostModuleData, GC::Cell);
    GC_DECLARE_ALLOCATOR(HostModuleData);

public:
    size_t execution_count { 0 };
};

GC_DEFINE_ALLOCATOR(HostModuleData);

class OtherHostModuleData final : public GC::Cell {
    GC_CELL(OtherHostModuleData, GC::Cell);
    GC_DECLARE_ALLOCATOR(OtherHostModuleData);

public:
    u64 identifier { 0 };
};

GC_DEFINE_ALLOCATOR(OtherHostModuleData);

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static String utf8_of(VM& vm, Value value)
{
    return MUST(value.to_utf16_string(vm)).to_utf8_but_should_be_ported_to_utf16();
}

static String evaluate_to_utf8(VM& vm, Realm& realm, StringView source)
{
    return utf8_of(vm, MUST(evaluate(vm, realm, source)));
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static ReadonlySpan<u16> utf16_code_units_of(SourceCode const& source_code)
{
    return { source_code.utf16_data(), source_code.length_in_code_units() };
}

static NonnullRefPtr<SourceCode const> source_code_from_utf8(StringView filename, StringView code)
{
    return SourceCode::create(Utf16String::from_utf8(filename), Utf16String::from_utf8(code));
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

TEST_CASE(classic_scripts)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto host_defined = realm.create<HostDefinedCell>();
    auto source_text = "var parsedAnswer = 6 * 7; parsedAnswer"_utf16;
    auto script = MUST(Script::parse(source_text.utf16_view(), realm, "https://example.com/answer.js"sv, {}, host_defined.ptr()));
    EXPECT_EQ(&script->realm(), &realm);
    EXPECT_EQ(script->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(MUST(vm.run(script)).as_i32(), 42);
    EXPECT_EQ(MUST(evaluate(vm, realm, "parsedAnswer + 1"sv)).as_i32(), 43);

    // The code reports its display filename, and its filename without one.
    auto top_level = top_level_source_code(*script);
    EXPECT(top_level);
    EXPECT_EQ(top_level->filename(), "https://example.com/answer.js"sv);
    EXPECT_EQ(top_level->code(), source_text);
    auto displayed_script = MUST(Script::parse(source_text.utf16_view(), realm, "https://example.com/answer.js"sv, u"inline script"sv));
    EXPECT_EQ(top_level_source_code(*displayed_script)->filename(), "inline script"sv);
    EXPECT(!displayed_script->host_defined());

    // Lines count from the offset, which is 1 by default.
    auto broken_source_text = "let a = 1;\nlet b = ;"_utf16;
    auto broken_script = Script::parse(broken_source_text.utf16_view(), realm, "broken.js"sv, {}, nullptr, 10);
    EXPECT(broken_script.is_error());
    EXPECT_EQ(broken_script.error().size(), 1u);
    auto const& parser_error = broken_script.error().first();
    EXPECT(!parser_error.message.is_empty());
    EXPECT_EQ(parser_error.position->line, 11u);
    EXPECT_EQ(parser_error.position->column, 9u);
    EXPECT_EQ(parser_error.to_utf16_string(), Utf16String::formatted("{} (line: 11, column: 9)", parser_error.message));
    auto unshifted_errors = Script::parse(broken_source_text.utf16_view(), realm).release_error();
    EXPECT_EQ(unshifted_errors.first().position->line, 2u);
    EXPECT_EQ(unshifted_errors.first().message, parser_error.message);
}

TEST_CASE(source_code)
{
    VMWithRealm vm_with_realm;

    auto ascii = source_code_from_utf8("ascii.js"sv, "let a = 1;"sv);
    EXPECT_EQ(ascii->filename(), "ascii.js"sv);
    EXPECT_EQ(ascii->code(), "let a = 1;"sv);
    EXPECT_EQ(ascii->length_in_code_units(), 10u);
    EXPECT_EQ(&ascii->code(), &ascii->code());
    auto ascii_code_units = utf16_code_units_of(*ascii);
    EXPECT_EQ(ascii_code_units.size(), 10u);
    EXPECT_EQ(ascii_code_units[4], static_cast<u16>('a'));
    EXPECT_EQ(ascii->utf16_data(), ascii_code_units.data());

    auto non_ascii = source_code_from_utf8("utf16.js"sv, "let α = 1;"sv);
    EXPECT_EQ(non_ascii->length_in_code_units(), 10u);
    EXPECT_EQ(utf16_code_units_of(*non_ascii)[4], static_cast<u16>(0x3b1));
    EXPECT_EQ(non_ascii->source_text_from_offsets(4, 5).to_utf8(), "α = 1"sv);
    EXPECT(non_ascii->source_text_from_offsets(4, 0).is_empty());

    // Source code is shared by reference.
    RefPtr<SourceCode const> another_reference = ascii;
    EXPECT_EQ(another_reference.ptr(), ascii.ptr());
    another_reference = nullptr;
    EXPECT_EQ(ascii->code(), "let a = 1;"sv);

    SourceRange range { ascii, Position { 1, 5 } };
    EXPECT_EQ(range.filename(), "ascii.js"sv);
    EXPECT_EQ(range.start.column, 5u);

    // Source code made from the bytes of a response decodes them as the C++ runtime does.
    auto from_bytes = [](Vector<u8> bytes, size_t length_in_code_units, StringView encoding) {
        return SourceCode::create("bytes.js"_utf16, length_in_code_units, encoding, MUST(Core::ImmutableBytes::copy(bytes.span())));
    };
    EXPECT_EQ(from_bytes({ 'l', 'e', 't' }, 3, "UTF-8"sv)->code(), "let"sv);
    EXPECT_EQ(from_bytes({ 0xed, 0xa0, 0x80 }, 3, "UTF-8"sv)->code().to_utf8(), "\xef\xbf\xbd\xef\xbf\xbd\xef\xbf\xbd"sv);
    EXPECT_EQ(from_bytes({ 0xc0, 0x80 }, 2, "UTF-8"sv)->code().to_utf8(), "\xef\xbf\xbd\xef\xbf\xbd"sv);
    EXPECT_EQ(from_bytes({ 'A', 0x00, 0xff }, 2, "UTF-16LE"sv)->code().to_utf8(), "A\xef\xbf\xbd"sv);
    EXPECT_EQ(from_bytes({ 0x80, 'A' }, 2, "windows-1252"sv)->code().to_utf8(), "\xe2\x82\xac"
                                                                                "A"sv);
    EXPECT_EQ(from_bytes({ 0xef, 0xbb, 0xbf, 0xce, 0xb1 }, 1, "windows-1252"sv)->code().to_utf8(), "α"sv);
    auto decoded = from_bytes({ 0xce, 0xb1, '=', '1' }, 3, "UTF-8"sv);
    EXPECT_EQ(decoded->length_in_code_units(), 3u);
    EXPECT_EQ(utf16_code_units_of(*decoded)[0], static_cast<u16>(0x3b1));
    EXPECT_EQ(decoded->filename(), "bytes.js"sv);
}

TEST_CASE(programs_parsed_and_compiled_on_another_thread)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto source_code = source_code_from_utf8("off-thread.js"sv, "function twice(x) { return 2 * x; } twice(21)"sv);
    auto source_text = utf16_code_units_of(*source_code);
    ParsedProgram parsed;
    CompiledProgram compiled;
    ParsedProgram copy_for_the_cache;
    EXPECT(!parsed);
    EXPECT(!compiled);
    auto thread = Threading::Thread::construct("ParseAndCompile"sv, [&] -> intptr_t {
        parsed = ParsedProgram::parse(source_text, ProgramType::Script, 1);
        if (parsed && !parsed.has_errors()) {
            copy_for_the_cache = parsed.clone();
            compiled = CompiledProgram::compile(move(parsed));
        }
        return 0;
    });
    thread->start();
    (void)thread->join();
    EXPECT(!parsed);
    EXPECT(compiled);
    EXPECT(copy_for_the_cache);
    EXPECT(!copy_for_the_cache.has_errors());

    auto host_defined = realm.create<HostDefinedCell>();
    auto script = MUST(create_script(move(compiled), source_code, realm, "https://example.com/off-thread.js"sv, host_defined.ptr()));
    EXPECT(!compiled);
    EXPECT_EQ(script->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(top_level_source_code(*script).ptr(), source_code.ptr());
    EXPECT_EQ(MUST(vm.run(script)).as_i32(), 42);
    EXPECT_EQ(MUST(evaluate(vm, realm, "twice(4)"sv)).as_i32(), 8);

    // The copy compiles on its own once the original ran.
    auto copy_script = MUST(create_script(move(copy_for_the_cache), source_code, realm, "copy.js"sv, nullptr));
    EXPECT_EQ(MUST(vm.run(copy_script)).as_i32(), 42);

    // A parsed program keeps its syntax errors until the main thread turns it into a record.
    auto broken_source_code = source_code_from_utf8("broken.js"sv, "let a = 1;\nlet b = ;"sv);
    auto broken = ParsedProgram::parse(utf16_code_units_of(*broken_source_code), ProgramType::Script, 1);
    EXPECT(broken);
    EXPECT(broken.has_errors());
    auto errors = create_script(move(broken), broken_source_code, realm, "broken.js"sv, nullptr).release_error();
    EXPECT_EQ(errors.size(), 1u);
    EXPECT_EQ(errors.first().position->line, 2u);
    EXPECT_EQ(errors.first().position->column, 9u);

    auto broken_module = ParsedProgram::parse(utf16_code_units_of(*broken_source_code), ProgramType::Module, 1);
    EXPECT(broken_module.has_errors());
    EXPECT_EQ(create_module(move(broken_module), broken_source_code, realm, "broken.mjs"sv, nullptr).release_error().size(), 1u);

    // Modules go the same way.
    auto module_source_code = source_code_from_utf8("module.mjs"sv, "export const answer = 42;"sv);
    auto parsed_module = ParsedProgram::parse(utf16_code_units_of(*module_source_code), ProgramType::Module, 1);
    auto copy_of_parsed_module = parsed_module.clone();
    auto compiled_module = MUST(create_module(CompiledProgram::compile(move(parsed_module)), module_source_code, realm, "module.mjs"sv, host_defined.ptr()));
    EXPECT_EQ(compiled_module->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(top_level_source_code(*compiled_module).ptr(), module_source_code.ptr());
    auto module_from_parse = MUST(create_module(move(copy_of_parsed_module), module_source_code, realm, "module.mjs"sv, nullptr));
    EXPECT(!module_from_parse->host_defined());
    EXPECT_EQ(top_level_source_code(*module_from_parse).ptr(), module_source_code.ptr());

    // A program that is no longer needed may be destroyed on any thread.
    auto discarded = ParsedProgram::parse(source_text, ProgramType::Script, 1);
    auto discarded_compiled = CompiledProgram::compile(discarded.clone());
    auto destroyer = Threading::Thread::construct("DestroyPrograms"sv, [discarded = move(discarded), discarded_compiled = move(discarded_compiled)] mutable -> intptr_t {
        discarded = {};
        discarded_compiled = {};
        return 0;
    });
    destroyer->start();
    (void)destroyer->join();
}

namespace {

// A host that compiles on worker threads and runs what they post back when the test drains it.
class OffThreadCompilationHost {
public:
    OffThreadCompilationCallbacks callbacks()
    {
        auto destruction_counter = make<DestructionCounter>(callback_destruction_count);
        return {
            .submit_work = [this, destruction_counter = move(destruction_counter)](Function<void()> work) {
                ++submitted_work_count;
                auto worker = Threading::Thread::construct("CompileOffThread"sv, [work = move(work)] mutable -> intptr_t {
                    work();
                    return 0;
                });
                worker->start();
                m_workers.append(move(worker)); },
            .post_to_main_thread = [this](Function<void()> task) {
                MutexLocker locker { m_mutex };
                m_tasks_for_the_main_thread.append(move(task)); },
        };
    }

    void finish_all_work()
    {
        while (true) {
            while (!m_workers.is_empty()) {
                auto workers = move(m_workers);
                for (auto& worker : workers)
                    (void)worker->join();
            }
            Vector<Function<void()>> tasks;
            {
                MutexLocker locker { m_mutex };
                tasks = move(m_tasks_for_the_main_thread);
            }
            if (tasks.is_empty())
                return;
            for (auto& task : tasks)
                task();
        }
    }

    size_t submitted_work_count { 0 };
    size_t callback_destruction_count { 0 };

private:
    struct DestructionCounter {
        AK_MAKE_NONCOPYABLE(DestructionCounter);
        AK_MAKE_NONMOVABLE(DestructionCounter);

    public:
        AK_ALLOC_WITH_KMALLOC;

        explicit DestructionCounter(size_t& count)
            : count(count)
        {
        }

        ~DestructionCounter() { ++count; }

        size_t& count;
    };

    Vector<NonnullRefPtr<Threading::Thread>> m_workers;
    Mutex m_mutex;
    Vector<Function<void()>> m_tasks_for_the_main_thread;
};

}

TEST_CASE(remaining_functions_compile_off_thread)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto source_text = "function outer(x) { function inner(y) { return y * 2; } return inner(x) + 1; }\n"
                       "var arrow = x => x - 1;\n"
                       "outer.length"_utf16;
    auto script = MUST(Script::parse(source_text.utf16_view(), realm, "lazy.js"sv));
    EXPECT_EQ(MUST(vm.run(script)).as_i32(), 1);

    OffThreadCompilationHost host;
    compile_remaining_functions_off_thread(*script, top_level_source_code(*script).release_nonnull(), host.callbacks());
    EXPECT(host.submitted_work_count >= 1u);
    host.finish_all_work();
    EXPECT_EQ(host.callback_destruction_count, 1u);
    EXPECT_EQ(MUST(evaluate(vm, realm, "outer(20) + arrow(2)"sv)).as_i32(), 42);

    // Functions that ran before their off-thread compilation was installed keep working.
    auto racing_source_text = "function first() { function nested() { return 'nested'; } return nested(); }"_utf16;
    auto racing_script = MUST(Script::parse(racing_source_text.utf16_view(), realm, "racing.js"sv));
    MUST(vm.run(racing_script));
    OffThreadCompilationHost racing_host;
    compile_remaining_functions_off_thread(*racing_script, top_level_source_code(*racing_script).release_nonnull(), racing_host.callbacks());
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "first()"sv), "nested"sv);
    racing_host.finish_all_work();
    EXPECT_EQ(racing_host.callback_destruction_count, 1u);
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "first()"sv), "nested"sv);

    // A record without functions to compile gives its callbacks up right away.
    auto plain_script = MUST(Script::parse(u"1 + 1"sv, realm, "plain.js"sv));
    OffThreadCompilationHost idle_host;
    compile_remaining_functions_off_thread(*plain_script, top_level_source_code(*plain_script).release_nonnull(), idle_host.callbacks());
    EXPECT_EQ(idle_host.submitted_work_count, 0u);
    EXPECT_EQ(idle_host.callback_destruction_count, 1u);

    // The functions of modules compile off thread as well, with or without top-level await.
    for (auto module_source : { "export function f() { return 40; } globalThis.fromModule = () => f() + 2;"sv,
             "export function g() { return 2; } await 0; globalThis.fromAsyncModule = () => g() + 40;"sv }) {
        auto module_source_text = Utf16String::from_utf8(module_source);
        auto module = MUST(SourceTextModule::parse(module_source_text.utf16_view(), realm, "lazy.mjs"sv));
        OffThreadCompilationHost module_host;
        compile_remaining_functions_off_thread(*module, source_code_from_utf8("lazy.mjs"sv, module_source), module_host.callbacks());
        module_host.finish_all_work();
        EXPECT_EQ(module_host.callback_destruction_count, 1u);
        module->load_requested_modules(nullptr);
        MUST(module->link(vm));
        MUST(module->evaluate(vm));
        vm_with_realm.run_queued_promise_jobs();
    }
    EXPECT_EQ(MUST(evaluate(vm, realm, "fromModule() + fromAsyncModule()"sv)).as_i32(), 84);
}

TEST_CASE(breakpoint_positions)
{
    VMWithRealm vm_with_realm;

    auto source_code = source_code_from_utf8("breakpoints.js"sv, "let a = 1;\nfunction f() {\n    return a;\n}\n"sv);
    auto positions = breakpoint_positions_for_source(*source_code, ProgramType::Script, 1);
    EXPECT(!positions.is_empty());
    for (size_t i = 1; i < positions.size(); ++i)
        EXPECT(positions[i - 1].line < positions[i].line || (positions[i - 1].line == positions[i].line && positions[i - 1].column < positions[i].column));
    EXPECT(any_of(positions, [](auto const& position) { return position.line == 1; }));
    EXPECT(any_of(positions, [](auto const& position) { return position.line == 3; }));

    auto shifted_positions = breakpoint_positions_for_source(*source_code, ProgramType::Script, 5);
    EXPECT_EQ(shifted_positions.size(), positions.size());
    EXPECT_EQ(shifted_positions.first().line, positions.first().line + 4);

    EXPECT(breakpoint_positions_for_source(*source_code_from_utf8("broken.js"sv, "let = ;"sv), ProgramType::Script, 1).is_empty());
}

namespace {

constexpr size_t bytecode_cache_source_hash_size = 32;

using SourceHash = AK::Array<u8, bytecode_cache_source_hash_size>;

SourceHash source_hash_filled_with(u8 byte)
{
    SourceHash hash;
    hash.fill(byte);
    return hash;
}

ByteBuffer serialize_fully_compiled(SourceCode const& source_code, ProgramType program_type, SourceHash const& source_hash)
{
    auto parsed = ParsedProgram::parse(utf16_code_units_of(source_code), program_type, 1);
    VERIFY(parsed && !parsed.has_errors());
    return CompiledProgram::compile_all_functions(move(parsed)).serialize_for_bytecode_cache(program_type, source_hash.span());
}

Core::ImmutableBytes immutable_bytes_of(ByteBuffer const& bytes)
{
    return MUST(Core::ImmutableBytes::copy(bytes.bytes()));
}

}

TEST_CASE(bytecode_cache_round_trip)
{
    // The main thread's event loop, where the bytes of validated blobs are released, outlives the VM.
    Core::EventLoop event_loop;
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto source_code = source_code_from_utf8("cached.js"sv, "function add(a, b) { return a + b; } add(40, 2)"sv);
    auto source_hash = source_hash_filled_with(7);
    auto blob = serialize_fully_compiled(*source_code, ProgramType::Script, source_hash);
    EXPECT(!blob.is_empty());

    // A blob names the runtime that wrote it, which is the one the test runs on.
    constexpr size_t runtime_tag_offset = 12;
    EXPECT(blob[runtime_tag_offset] == 'C' || blob[runtime_tag_offset] == 'R');
    bool const runs_on_the_rust_runtime = blob[runtime_tag_offset] == 'R';

    // Only a program compiled with all its functions, for its own type and a hash of the right size, can be cached.
    // The C++ runtime also serializes a program compiled without all its functions, to a blob that never
    // materializes, and a program of the other type, to a blob that then runs as that type.
    auto fully_compiled = CompiledProgram::compile_all_functions(ParsedProgram::parse(utf16_code_units_of(*source_code), ProgramType::Script, 1));
    EXPECT(fully_compiled.serialize_for_bytecode_cache(ProgramType::Script, source_hash.span().trim(16)).is_empty());
    EXPECT(fully_compiled.serialize_for_bytecode_cache(ProgramType::Script, source_hash.span()) == blob);
    if (runs_on_the_rust_runtime) {
        auto lazy_source_code = source_code_from_utf8("lazy.js"sv, "function never_called() { return 1; } 0"sv);
        auto parsed = ParsedProgram::parse(utf16_code_units_of(*lazy_source_code), ProgramType::Script, 1);
        EXPECT(CompiledProgram::compile(move(parsed)).serialize_for_bytecode_cache(ProgramType::Script, source_hash.span()).is_empty());
        EXPECT(fully_compiled.serialize_for_bytecode_cache(ProgramType::Module, source_hash.span()).is_empty());
    }

    auto cache = DecodedBytecodeCache::create(immutable_bytes_of(blob), ProgramType::Script, source_hash.span());
    EXPECT(cache);
    auto host_defined = realm.create<HostDefinedCell>();
    auto script = MUST(Script::create_from_bytecode_cache(cache.release_nonnull(), source_code, realm, "https://example.com/cached.js"sv, host_defined.ptr()));
    EXPECT_EQ(script->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(top_level_source_code(*script).ptr(), source_code.ptr());
    collect_garbage(vm);
    EXPECT_EQ(MUST(vm.run(script)).as_i32(), 42);
    EXPECT_EQ(MUST(evaluate(vm, realm, "add(1, 2)"sv)).as_i32(), 3);
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "add.toString()"sv), "function add(a, b) { return a + b; }"sv);

    // A blob only decodes for the program type and the source hash it was written for, and only when it is intact.
    EXPECT(!DecodedBytecodeCache::create(immutable_bytes_of(blob), ProgramType::Module, source_hash.span()));
    EXPECT(!DecodedBytecodeCache::create(immutable_bytes_of(blob), ProgramType::Script, source_hash_filled_with(8).span()));
    EXPECT(!DecodedBytecodeCache::create(immutable_bytes_of(MUST(blob.slice(0, blob.size() / 2))), ProgramType::Script, source_hash.span()));

    // The blobs of the two runtimes differ only in the tag that names their runtime, and each runtime rejects the
    // other's.
    auto blob_of_the_other_runtime = blob;
    blob_of_the_other_runtime[runtime_tag_offset] = runs_on_the_rust_runtime ? 'C' : 'R';
    EXPECT(!DecodedBytecodeCache::create(immutable_bytes_of(blob_of_the_other_runtime), ProgramType::Script, source_hash.span()));

    // A blob that does not match the source code it is given fails to become a script.
    auto other_source_code = source_code_from_utf8("cached.js"sv, "1"sv);
    auto mismatched = Script::create_from_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(blob), ProgramType::Script, source_hash.span()).release_nonnull(), other_source_code, realm, "cached.js"sv);
    EXPECT(mismatched.is_error());
    EXPECT_EQ(mismatched.error().size(), 1u);

    // Decoding and validating against the length of the source works on another thread, and the blob's bytes are
    // released on the main thread's event loop.
    RefPtr<DecodedBytecodeCache> validated;
    RefPtr<DecodedBytecodeCache> too_short;
    auto source_length = source_code->length_in_code_units();
    auto decoder = Threading::Thread::construct("DecodeBytecodeCache"sv, [&] -> intptr_t {
        validated = decode_and_validate_bytecode_cache(immutable_bytes_of(blob), ProgramType::Script, source_hash.span(), source_length, event_loop);
        too_short = decode_and_validate_bytecode_cache(immutable_bytes_of(blob), ProgramType::Script, source_hash.span(), 5, event_loop);
        return 0;
    });
    decoder->start();
    (void)decoder->join();
    EXPECT(validated);
    EXPECT(!too_short);
    auto validated_script = MUST(Script::create_from_bytecode_cache(validated.release_nonnull(), source_code, realm, "validated.js"sv));
    EXPECT_EQ(MUST(vm.run(validated_script)).as_i32(), 42);
    event_loop.pump(Core::EventLoop::WaitMode::PollForEvents);
}

TEST_CASE(bytecode_cache_generated_for_running_records)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto source_hash = source_hash_filled_with(3);

    // A host compiles a script for running, and a copy of its parse with all functions for the cache, which it then
    // installs in the running script.
    auto source_code = source_code_from_utf8("generated.js"sv, "function square(x) { return x * x; } var squares = [square(2)]; squares[0]"sv);
    auto parsed = ParsedProgram::parse(utf16_code_units_of(*source_code), ProgramType::Script, 1);
    auto copy_for_the_cache = parsed.clone();
    auto script = MUST(create_script(CompiledProgram::compile(move(parsed)), source_code, realm, "generated.js"sv, nullptr));
    EXPECT_EQ(MUST(vm.run(script)).as_i32(), 4);
    script->begin_bytecode_cache_generation();
    auto blob = CompiledProgram::compile_all_functions(move(copy_for_the_cache)).serialize_for_bytecode_cache(ProgramType::Script, source_hash.span());
    EXPECT(!blob.is_empty());
    auto cache = DecodedBytecodeCache::create(Core::ImmutableBytes::adopt(move(blob)), ProgramType::Script, source_hash.span());
    EXPECT(cache);
    script->install_generated_bytecode_cache(cache.release_nonnull(), source_code);
    EXPECT_EQ(MUST(evaluate(vm, realm, "square(5) + squares[0]"sv)).as_i32(), 29);

    // A script that already runs from a blob takes no other.
    auto other_blob = serialize_fully_compiled(*source_code, ProgramType::Script, source_hash);
    EXPECT(!script->try_install_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(other_blob), ProgramType::Script, source_hash.span()).release_nonnull(), source_code));

    // A host that gives up on generating the cache leaves the script as it was, ready to take a matching cache later.
    auto abandoned = MUST(Script::parse(source_code->code().utf16_view(), realm, "abandoned.js"sv));
    MUST(vm.run(abandoned));
    abandoned->begin_bytecode_cache_generation();
    abandoned->finish_bytecode_cache_generation_without_install();
    auto abandoned_source_code = top_level_source_code(*abandoned).release_nonnull();
    EXPECT(abandoned->try_install_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(other_blob), ProgramType::Script, source_hash.span()).release_nonnull(), abandoned_source_code));
    EXPECT_EQ(MUST(evaluate(vm, realm, "square(6)"sv)).as_i32(), 36);

    // Modules go the same way.
    auto module_source_code = source_code_from_utf8("generated.mjs"sv, "export function cube(x) { return x * x * x; } globalThis.cube = cube;"sv);
    auto module_blob = serialize_fully_compiled(*module_source_code, ProgramType::Module, source_hash);
    EXPECT(!module_blob.is_empty());
    EXPECT(!DecodedBytecodeCache::create(immutable_bytes_of(module_blob), ProgramType::Script, source_hash.span()));
    auto module_from_cache = MUST(SourceTextModule::parse_from_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(module_blob), ProgramType::Module, source_hash.span()).release_nonnull(), module_source_code, realm, "generated.mjs"sv));
    module_from_cache->load_requested_modules(nullptr);
    MUST(module_from_cache->link(vm));
    MUST(module_from_cache->evaluate(vm));
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(MUST(evaluate(vm, realm, "cube(3)"sv)).as_i32(), 27);

    auto parsed_module = ParsedProgram::parse(utf16_code_units_of(*module_source_code), ProgramType::Module, 1);
    auto copy_of_parsed_module = parsed_module.clone();
    auto running_module = MUST(create_module(CompiledProgram::compile(move(parsed_module)), module_source_code, realm, "running.mjs"sv, nullptr));
    running_module->begin_bytecode_cache_generation();
    auto generated_module_blob = CompiledProgram::compile_all_functions(move(copy_of_parsed_module)).serialize_for_bytecode_cache(ProgramType::Module, source_hash.span());
    running_module->install_generated_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(generated_module_blob), ProgramType::Module, source_hash.span()).release_nonnull(), module_source_code);
    running_module->load_requested_modules(nullptr);
    MUST(running_module->link(vm));
    MUST(running_module->evaluate(vm));
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(MUST(evaluate(vm, realm, "cube(4)"sv)).as_i32(), 64);

    auto abandoned_module = MUST(SourceTextModule::parse(module_source_code->code().utf16_view(), realm, "abandoned.mjs"sv));
    abandoned_module->begin_bytecode_cache_generation();
    abandoned_module->finish_bytecode_cache_generation_without_install();
    EXPECT(abandoned_module->try_install_bytecode_cache(DecodedBytecodeCache::create(immutable_bytes_of(module_blob), ProgramType::Module, source_hash.span()).release_nonnull(), top_level_source_code(*abandoned_module).release_nonnull()));
}

namespace {

size_t s_host_module_executions = 0;

void append_ascii_to_sink(JSStringSink const& sink, StringView ascii)
{
    Vector<u16> code_units;
    for (auto character : ascii)
        code_units.append(static_cast<u16>(character));
    sink.append(sink.context, code_units.data(), code_units.size());
}

constexpr JSHostModuleHooks host_module_hooks {
    .get_exported_names = [](JSModule*, JSStringSink* names) { append_ascii_to_sink(*names, "answer"sv); },
    .resolve_export = [](JSModule* module, u16 const* export_name, size_t export_name_length, JSResolvedBinding* out) {
        if (Utf16View { reinterpret_cast<char16_t const*>(export_name), export_name_length } != u"answer"sv)
            return;
        out->type = JS_RESOLVED_BINDING_BINDING_NAME;
        out->module = module;
        out->binding_name.append(out->binding_name.context, export_name, export_name_length); },
    .initialize_environment = [](JSModule*) { return JSCompletion { 0, JS_COMPLETION_NORMAL }; },
    .execute_module = [](JSModule* module, JSPromiseCapability* capability) {
        VERIFY(!capability);
        ++s_host_module_executions;
        if (auto* data = host_data_if<HostModuleData>(*reinterpret_cast<Module*>(module)))
            ++data->execution_count;
        return JSCompletion { 0, JS_COMPLETION_NORMAL }; },
};

constexpr char host_module_class_name[] = "TestHostModule";
constexpr char derived_host_module_class_name[] = "DerivedTestHostModule";

constexpr JSHostClass host_module_class {
    .abi_version = JS_HOST_ABI_VERSION,
    .kind = JS_HOST_CLASS_MODULE,
    .reserved = 0,
    .flags = 0,
    .name = host_module_class_name,
    .name_length = sizeof(host_module_class_name) - 1,
    .parent = nullptr,
    .hooks = &host_module_hooks,
    .user_data = nullptr,
};

constexpr JSHostClass derived_host_module_class {
    .abi_version = JS_HOST_ABI_VERSION,
    .kind = JS_HOST_CLASS_MODULE,
    .reserved = 0,
    .flags = 0,
    .name = derived_host_module_class_name,
    .name_length = sizeof(derived_host_module_class_name) - 1,
    .parent = &host_module_class,
    .hooks = &host_module_hooks,
    .user_data = nullptr,
};

// A host that loads modules from sources it holds, as HostLoadImportedModule lets it, and finishes each load before
// it returns.
class TestModuleLoader {
public:
    TestModuleLoader(VM& vm, Realm& realm)
        : m_vm(vm)
        , m_realm(realm)
    {
        vm.host_get_supported_import_attributes = [] { return Vector<Utf16String> { "type"_utf16 }; };
        vm.host_load_imported_module = [this](ImportedModuleReferrer referrer, ModuleRequest const& module_request, GC::Ptr<GC::Cell> load_state, ImportedModulePayload payload) {
            load(referrer, module_request, load_state, payload);
        };
    }

    void add_source(StringView specifier, StringView source)
    {
        m_sources.set(MUST(String::from_utf8(specifier)), MUST(String::from_utf8(source)));
    }

    GC::Ptr<Module> loaded_module(StringView specifier) const
    {
        auto module = m_loaded_modules.get(MUST(String::from_utf8(specifier)));
        if (!module.has_value())
            return {};
        return module->ptr();
    }

    GC::Ptr<GC::Cell> last_load_state;
    GC::Ptr<GC::Cell> host_module_data;
    Vector<ModuleRequest> requests;

private:
    void load(ImportedModuleReferrer referrer, ModuleRequest const& module_request, GC::Ptr<GC::Cell> load_state, ImportedModulePayload payload)
    {
        requests.append(module_request);
        last_load_state = load_state;
        auto specifier = module_request.module_specifier.view().to_utf8_but_should_be_ported_to_utf16();

        // Like HTML's module map, the host loads each module once.
        if (auto loaded = m_loaded_modules.get(specifier); loaded.has_value()) {
            finish_loading_imported_module(referrer, module_request, payload, GC::Ref<Module> { **loaded });
            return;
        }

        auto module = load_module(specifier, attribute_value(module_request, "type"sv));
        if (!module.is_error())
            m_loaded_modules.set(specifier, GC::make_root(module.value()));
        finish_loading_imported_module(referrer, module_request, payload, module);
    }

    static Optional<String> attribute_value(ModuleRequest const& module_request, StringView key)
    {
        for (auto const& attribute : module_request.attributes) {
            if (attribute.key == key)
                return attribute.value.to_utf8_but_should_be_ported_to_utf16();
        }
        return {};
    }

    ThrowCompletionOr<GC::Ref<Module>> load_module(String const& specifier, Optional<String> const& type)
    {
        if (specifier == "./host.mjs"sv || specifier == "./derived-host.mjs"sv) {
            auto& host_class = specifier == "./host.mjs"sv ? host_module_class : derived_host_module_class;
            Vector<ModuleRequest> requested_modules { ModuleRequest { "./dependency.mjs"_utf16_fly_string } };
            host_module_data = m_vm.heap().allocate<HostModuleData>();
            return GC::Ref<Module> { HostModule::create(m_realm, host_class, specifier.bytes_as_string_view(), move(requested_modules), nullptr, host_module_data) };
        }
        if (specifier.starts_with_bytes("synthetic:"sv))
            return GC::Ref<Module> { SyntheticModule::create_default_export_synthetic_module(m_realm, PrimitiveString::create(m_vm, Utf16String::from_utf8(specifier)), specifier.to_byte_string()) };

        auto source = m_sources.get(specifier);
        if (!source.has_value())
            return m_vm.throw_completion<TypeError>(Utf16String::formatted("Cannot find {}", specifier));
        auto source_text = Utf16String::from_utf8(*source);

        if (type.has_value() && *type == "json"sv)
            return GC::Ref<Module> { TRY(parse_json_module(m_realm, source_text.utf16_view(), specifier.to_byte_string())) };
        if (type.has_value() && *type == "text"sv)
            return GC::Ref<Module> { create_text_module(m_realm, source_text.utf16_view(), specifier.to_byte_string()) };

        auto module = SourceTextModule::parse(source_text.utf16_view(), m_realm, specifier.bytes_as_string_view());
        if (module.is_error())
            return m_vm.throw_completion<SyntaxError>(module.error().first().to_utf16_string());
        return GC::Ref<Module> { module.release_value() };
    }

    VM& m_vm;
    Realm& m_realm;
    HashMap<String, String> m_sources;
    HashMap<String, GC::Root<Module>> m_loaded_modules;
};

}

TEST_CASE(modules_loaded_through_the_host)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    TestModuleLoader loader { vm, realm };
    s_host_module_executions = 0;

    loader.add_source("./dependency.mjs"sv, "export const value = 30; export function plus(x) { return value + x; }"sv);
    loader.add_source("./data.json"sv, "{ \"answer\": 10 }"sv);
    loader.add_source("./greeting.txt"sv, "hello"sv);
    auto main_source_text = "import { value, plus } from './dependency.mjs';\n"
                            "import data from './data.json' with { type: 'json' };\n"
                            "import greeting from './greeting.txt' with { type: 'text' };\n"
                            "import sheet from 'synthetic:sheet';\n"
                            "import './host.mjs';\n"
                            "export const total = plus(value) - 30 + data.answer + greeting.length + sheet.length - 3 - 15;\n"
                            "export { value as reexported };\n"
                            "globalThis.mainRan = true;"_utf16;
    auto host_defined = realm.create<HostDefinedCell>();
    auto main = MUST(SourceTextModule::parse(main_source_text.utf16_view(), realm, "./main.mjs"sv, {}, host_defined.ptr()));
    EXPECT_EQ(main->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(&main->realm(), &realm);
    Module& main_module = *main;
    EXPECT(is<SourceTextModule>(main_module));
    EXPECT(is<CyclicModule>(main_module));
    EXPECT(!is<SyntheticModule>(main_module));
    EXPECT(!is<HostModule>(main_module));
    EXPECT(!host_class_of(*main));
    EXPECT(!main->environment());

    auto const& requested_modules = main->requested_modules();
    EXPECT_EQ(requested_modules.size(), 5u);
    EXPECT_EQ(&main->requested_modules(), &requested_modules);
    EXPECT(requested_modules[0].module_specifier.view() == "./dependency.mjs"sv);
    EXPECT(requested_modules[0].attributes.is_empty());
    EXPECT(requested_modules[1].module_specifier.view() == "./data.json"sv);
    EXPECT_EQ(requested_modules[1].attributes.size(), 1u);
    EXPECT_EQ(requested_modules[1].attributes[0].key, "type"sv);
    EXPECT_EQ(requested_modules[1].attributes[0].value, "json"sv);
    EXPECT(requested_modules[4].module_specifier.view() == "./host.mjs"sv);

    // LoadRequestedModules hands the host the state it passes, and the host finishes each load.
    auto load_state = realm.create<HostDefinedCell>();
    main->load_requested_modules(load_state.ptr());
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(loader.last_load_state.ptr(), load_state.ptr());
    EXPECT_EQ(loader.requests.size(), 6u);
    EXPECT(loader.requests[1] == requested_modules[1]);
    EXPECT(!(loader.requests[0] == requested_modules[1]));

    MUST(main->link(vm));
    EXPECT(main->environment());
    MUST(main->evaluate(vm));
    vm_with_realm.run_queued_promise_jobs();
    EXPECT(MUST(evaluate(vm, realm, "globalThis.mainRan"sv)).as_bool());

    auto main_namespace = main->get_module_namespace(vm);
    EXPECT_EQ(property_of(vm, main_namespace, "total"sv).as_i32(), 42);
    EXPECT_EQ(property_of(vm, main_namespace, "reexported"sv).as_i32(), 30);
    auto exported_names = main_module.get_exported_names(vm);
    EXPECT_EQ(exported_names.size(), 2u);
    EXPECT(exported_names.contains_slow("total"_utf16_fly_string));
    EXPECT(exported_names.contains_slow("reexported"_utf16_fly_string));

    // Resolving an export follows indirect exports to the module that has the binding.
    auto dependency = loader.loaded_module("./dependency.mjs"sv);
    EXPECT(dependency);
    auto reexported = main->resolve_export(vm, "reexported"_utf16_fly_string);
    EXPECT(reexported.is_valid());
    EXPECT_EQ(reexported.type, ResolvedBinding::BindingName);
    EXPECT_EQ(reexported.module.ptr(), dependency.ptr());
    EXPECT_EQ(reexported.export_name, "value"sv);
    auto missing = main->resolve_export(vm, "missing"_utf16_fly_string);
    EXPECT(!missing.is_valid());
    EXPECT_EQ(missing.type, ResolvedBinding::Null);
    EXPECT(!missing.module);

    // Synthetic modules.
    auto data = loader.loaded_module("./data.json"sv);
    EXPECT(is<SyntheticModule>(*data));
    EXPECT(!is<CyclicModule>(*data));
    EXPECT_EQ(property_of(vm, property_of(vm, data->get_module_namespace(vm), "default"sv), "answer"sv).as_i32(), 10);
    EXPECT_EQ(utf8_of(vm, property_of(vm, loader.loaded_module("./greeting.txt"sv)->get_module_namespace(vm), "default"sv)), "hello"sv);
    EXPECT_EQ(utf8_of(vm, property_of(vm, loader.loaded_module("synthetic:sheet"sv)->get_module_namespace(vm), "default"sv)), "synthetic:sheet"sv);
    auto json_error = parse_json_module(realm, u"{ \"unterminated\": "sv, "broken.json"sv);
    EXPECT(json_error.is_error());
    EXPECT_EQ(utf8_of(vm, property_of(vm, json_error.error_value(), "name"sv)), "SyntaxError"sv);

    // The host module took part in loading, linking and evaluating the graph.
    auto host = loader.loaded_module("./host.mjs"sv);
    EXPECT(host);
    EXPECT(is<HostModule>(*host));
    EXPECT(is<CyclicModule>(*host));
    auto& host_module = static_cast<HostModule&>(*host);
    EXPECT_EQ(s_host_module_executions, 1u);
    EXPECT_EQ(host_class_of(*host), &host_module_class);
    EXPECT_EQ(&host_module.host_class(), &host_module_class);
    EXPECT_EQ(host_module.class_name(), "TestHostModule"sv);
    EXPECT(is_host_instance_of(*host, host_module_class));
    EXPECT(!is_host_instance_of(*host, derived_host_module_class));
    EXPECT(!is_host_instance_of(*dependency, host_module_class));
    auto* host_data = host_data_if<HostModuleData>(*host);
    EXPECT_EQ(host_data, loader.host_module_data.ptr());
    EXPECT_EQ(host_data->execution_count, 1u);
    EXPECT(!host_data_if<OtherHostModuleData>(*host));
    EXPECT(!host_data_if<HostModuleData>(*dependency));
    EXPECT_EQ(host_module.requested_modules().size(), 1u);
    EXPECT_EQ(host_module.get_imported_module(ModuleRequest { "./dependency.mjs"_utf16_fly_string }).ptr(), dependency.ptr());
    auto answer = host->resolve_export(vm, "answer"_utf16_fly_string);
    EXPECT_EQ(answer.type, ResolvedBinding::BindingName);
    EXPECT_EQ(answer.module.ptr(), host.ptr());
    EXPECT_EQ(answer.export_name, "answer"sv);
    EXPECT(!host->resolve_export(vm, "question"_utf16_fly_string).is_valid());
    EXPECT(host->get_exported_names(vm) == Vector<Utf16FlyString> { "answer"_utf16_fly_string });

    auto other_data = vm.heap().allocate<OtherHostModuleData>();
    host_module.set_host_data(other_data);
    EXPECT_EQ(host_module.host_data().ptr(), other_data.ptr());
    EXPECT(!host_data_if<HostModuleData>(*host));
    EXPECT_EQ(host_data_if<OtherHostModuleData>(*host), other_data.ptr());

    // A host module of a derived class is an instance of its parent class too.
    auto derived_importer_text = "import './derived-host.mjs';"_utf16;
    auto derived_importer = MUST(SourceTextModule::parse(derived_importer_text.utf16_view(), realm, "./derived-importer.mjs"sv));
    derived_importer->load_requested_modules(nullptr);
    MUST(derived_importer->link(vm));
    MUST(derived_importer->evaluate(vm));
    vm_with_realm.run_queued_promise_jobs();
    auto derived_host = loader.loaded_module("./derived-host.mjs"sv);
    EXPECT(is_host_instance_of(*derived_host, derived_host_module_class));
    EXPECT(is_host_instance_of(*derived_host, host_module_class));
    EXPECT_EQ(static_cast<HostModule&>(*derived_host).class_name(), "DerivedTestHostModule"sv);
    EXPECT_EQ(s_host_module_executions, 2u);

    collect_garbage(vm);
    EXPECT_EQ(main->requested_modules().size(), 5u);
    EXPECT_EQ(MUST(evaluate(vm, realm, "1"sv)).as_i32(), 1);
}

TEST_CASE(top_level_await)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    TestModuleLoader loader { vm, realm };

    loader.add_source("./slow.mjs"sv, "globalThis.order = ['slow before']; await null; globalThis.order.push('slow after'); export const ready = true;"sv);
    auto source_text = "import { ready } from './slow.mjs'; globalThis.order.push('main ' + ready);"_utf16;
    auto main = MUST(SourceTextModule::parse(source_text.utf16_view(), realm, "./tla-main.mjs"sv));
    EXPECT(top_level_source_code(*main));
    main->load_requested_modules(nullptr);
    MUST(main->link(vm));
    MUST(main->evaluate(vm));
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "globalThis.order.join()"sv), "slow before"sv);
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "globalThis.order.join()"sv), "slow before,slow after,main true"sv);

    // The body of a module with top-level await compiles as an async function.
    auto slow = loader.loaded_module("./slow.mjs"sv);
    EXPECT(!top_level_source_code(static_cast<SourceTextModule&>(*slow)));

    // Evaluating the module again hands out the same settled evaluation.
    MUST(main->evaluate(vm));
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "globalThis.order.join()"sv), "slow before,slow after,main true"sv);
}

TEST_CASE(dynamic_import)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    TestModuleLoader loader { vm, realm };

    loader.add_source("./found.mjs"sv, "export default 'found';"sv);
    MUST(evaluate(vm, realm,
        "import('./found.mjs').then(namespace => { globalThis.found = namespace.default; });\n"
        "import('./missing.mjs').then(() => { globalThis.missing = 'loaded'; }, error => { globalThis.missing = error.constructor.name + ': ' + error.message; });\n"
        "import('./broken.json', { with: { type: 'json' } }).catch(error => { globalThis.broken = error.constructor.name; });"sv));
    loader.add_source("./broken.json"sv, "{"sv);
    vm_with_realm.run_queued_promise_jobs();
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "globalThis.found"sv), "found"sv);
    EXPECT_EQ(evaluate_to_utf8(vm, realm, "globalThis.missing"sv), "TypeError: Cannot find ./missing.mjs"sv);
    EXPECT_EQ(loader.requests.size(), 3u);
    EXPECT(!loader.last_load_state);
}

TEST_CASE(link_errors)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    TestModuleLoader loader { vm, realm };

    loader.add_source("./exports-nothing.mjs"sv, "export {};"sv);
    auto source_text = "import { absent } from './exports-nothing.mjs';"_utf16;
    auto main = MUST(SourceTextModule::parse(source_text.utf16_view(), realm, "./link-error.mjs"sv));
    main->load_requested_modules(nullptr);
    auto linked = main->link(vm);
    EXPECT(linked.is_error());
    EXPECT_EQ(utf8_of(vm, property_of(vm, linked.error_value(), "name"sv)), "SyntaxError"sv);

    // Module syntax errors carry their line, counted from the offset, which is 0 by default, where lines start at 1.
    auto broken_text = "export const a = 1;\nexport const = 2;"_utf16;
    auto broken = SourceTextModule::parse(broken_text.utf16_view(), realm, "./broken.mjs"sv, {}, nullptr, 3);
    EXPECT(broken.is_error());
    EXPECT_EQ(broken.error().first().position->line, 4u);
    EXPECT_EQ(SourceTextModule::parse(broken_text.utf16_view(), realm, "./broken.mjs"sv).release_error().first().position->line, 2u);
}

TEST_CASE(dynamic_functions)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    // Hosts close event handlers over the global environment, which is the lexical environment of a running script.
    GC::Ptr<Environment> global_environment;
    vm.host_ensure_can_compile_strings = [&](Realm&, ReadonlySpan<Utf16String>, Utf16View, Utf16View, CompilationType, ReadonlySpan<Value>, Value) -> ThrowCompletionOr<void> {
        global_environment = vm.lexical_environment();
        return {};
    };
    MUST(evaluate(vm, realm, "eval('0')"sv));
    EXPECT(global_environment);

    auto body = u"return event * 2;"sv;
    auto source_text = "function onclick(event) {\nreturn event * 2;\n}"_utf16;
    auto compiled = MUST(CompiledDynamicFunction::compile(vm, source_text.utf16_view(), u"event"sv, body, FunctionKind::Normal));
    collect_garbage(vm);
    auto function = compiled.instantiate(realm, *global_environment, nullptr, ScriptOrModule {});
    EXPECT(Value(function).is_function());
    EXPECT_EQ(property_of(vm, function, "length"sv).as_i32(), 1);
    EXPECT_EQ(utf8_of(vm, property_of(vm, function, "name"sv)), "onclick"sv);
    auto callback = JobCallback::create(vm, *function, nullptr);
    AK::Array<Value, 1> arguments { Value(21) };
    EXPECT_EQ(MUST(vm.host_call_job_callback(*callback, js_undefined(), arguments.span())).as_i32(), 42);

    // Each instantiation is a new function.
    auto another_function = compiled.instantiate(realm, *global_environment, nullptr, ScriptOrModule {});
    EXPECT_NE(another_function.ptr(), function.ptr());

    auto broken_source_text = "function onclick(event) {\nreturn event *;\n}"_utf16;
    auto broken = CompiledDynamicFunction::compile(vm, broken_source_text.utf16_view(), u"event"sv, u"return event *;"sv, FunctionKind::Normal);
    EXPECT(broken.is_error());
    EXPECT(!broken.error().is_empty());
}
