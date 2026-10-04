/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Array.h>
#include <AK/Atomic.h>
#include <AK/BitCast.h>
#include <AK/ByteBuffer.h>
#include <AK/MemMem.h>
#include <AK/OwnPtr.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <thread>

#if !defined(AK_OS_WINDOWS)
#    include <fcntl.h>
#    include <sys/mman.h>
#    include <unistd.h>
#endif

#include "EmbeddingTest.h"

namespace {

// The hash of the source that keys a blob, as LibWeb computes it from the encoded source text.
constexpr Array<u8, JS_BYTECODE_CACHE_SOURCE_HASH_SIZE> source_hash = [] {
    Array<u8, JS_BYTECODE_CACHE_SOURCE_HASH_SIZE> hash {};
    for (size_t i = 0; i < hash.size(); ++i)
        hash[i] = static_cast<u8>(i * 7 + 1);
    return hash;
}();

JSSourceCode const* create_source_code(StringView filename, StringView code)
{
    return js_source_code_create(Utf16String::from_utf8(filename).into_raw(), Utf16String::from_utf8(code).into_raw());
}

Utf16String to_string(JSVM* vm, JSValue value)
{
    JSOwnedUtf16String string {};
    VERIFY(js_value_to_utf16_string(vm, value, &string).variant == JS_COMPLETION_NORMAL);
    return Utf16String::adopt_raw(string);
}

Utf16String run_script(JSVM* vm, JSScript* script)
{
    auto completion = js_script_run(vm, script, nullptr);
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return to_string(vm, completion.payload);
}

Utf16String evaluate(EmbeddedVM& embedded_vm, StringView source)
{
    auto completion = embedded_vm.evaluate(source);
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return to_string(embedded_vm.vm(), completion.payload);
}

// The bytes of a blob as an embedder holds them, like a Core::ImmutableBytes, which count their releases on whatever
// thread releases them.
struct BlobOwner {
    ByteBuffer bytes;
    Atomic<size_t>* releases { nullptr };
};

JSBytecodeCacheBlobOwner owner_of(BlobOwner* owner)
{
    return { owner, [](void* owner) {
                auto* blob_owner = static_cast<BlobOwner*>(owner);
                if (blob_owner->releases)
                    ++*blob_owner->releases;
                delete blob_owner;
            } };
}

JSDecodedBytecodeCache const* decode(ReadonlyBytes blob, JSProgramType program_type, Atomic<size_t>* releases = nullptr)
{
    auto* owner = new BlobOwner { MUST(ByteBuffer::copy(blob)), releases };
    auto bytes = owner->bytes.bytes();
    return js_bytecode_cache_decode(bytes.data(), bytes.size(), program_type, source_hash.data(), source_hash.size(), owner_of(owner));
}

// Blobs often come straight from the functions that make them, which the overloads copy before the temporaries go.
JSDecodedBytecodeCache const* decode(ByteBuffer const& blob, JSProgramType program_type, Atomic<size_t>* releases = nullptr)
{
    return decode(blob.bytes(), program_type, releases);
}

JSDecodedBytecodeCache const* decode_and_validate(ReadonlyBytes blob, JSProgramType program_type, size_t source_length, Atomic<size_t>* releases = nullptr)
{
    auto* owner = new BlobOwner { MUST(ByteBuffer::copy(blob)), releases };
    auto bytes = owner->bytes.bytes();
    return js_bytecode_cache_decode_and_validate(bytes.data(), bytes.size(), program_type, source_hash.data(), source_hash.size(), source_length, owner_of(owner));
}

struct ProgramsForRunningAndForTheCache {
    JSCompiledProgram* to_run { nullptr };
    ByteBuffer blob;
};

// What LibWeb does on its thread pool for a script it fetches: parse it once, compile it to run, and compile a copy of
// the parse with all its functions for the blob it stores in the cache.
ProgramsForRunningAndForTheCache compile_on_another_thread(JSSourceCode const* source_code, JSProgramType program_type)
{
    ProgramsForRunningAndForTheCache programs;
    std::thread worker([&programs, code = Utf16String::adopt_raw(js_source_code_code(source_code)), program_type] {
        auto* parsed = js_compile_parse(abi_view_of(code.utf16_view()), program_type, 1);
        VERIFY(!js_compile_parsed_program_has_errors(parsed));
        auto* parsed_for_cache = js_compile_parsed_program_clone(parsed);
        programs.to_run = js_compile_parsed_program(parsed);
        auto* compiled_for_cache = js_compile_parsed_program_with_all_functions(parsed_for_cache);
        JSByteSink sink { &programs.blob, [](void* context, u8 const* bytes, size_t length) {
                             static_cast<ByteBuffer*>(context)->append(bytes, length);
                             return true;
                         } };
        VERIFY(js_bytecode_cache_serialize(compiled_for_cache, program_type, source_hash.data(), source_hash.size(), &sink));
        js_compile_compiled_program_destroy(compiled_for_cache);
    });
    worker.join();
    return programs;
}

ByteBuffer blob_of(StringView source, JSProgramType program_type = JS_PROGRAM_TYPE_SCRIPT)
{
    auto const* source_code = create_source_code("test.js"sv, source);
    auto programs = compile_on_another_thread(source_code, program_type);
    js_compile_compiled_program_destroy(programs.to_run);
    js_source_code_release(source_code);
    return move(programs.blob);
}

struct ParserErrors {
    Vector<Utf16String> messages;

    JSParserErrorSink sink()
    {
        return { this, [](void* context, JSOwnedUtf16String message, u32, u32) {
                    static_cast<ParserErrors*>(context)->messages.append(Utf16String::adopt_raw(message));
                } };
    }
};

JSScript* create_script_from_cache(EmbeddedVM& embedded_vm, JSDecodedBytecodeCache const* cache, JSSourceCode const* source_code, ParserErrors* errors = nullptr)
{
    auto sink = errors ? errors->sink() : JSParserErrorSink { nullptr, nullptr };
    return js_bytecode_cache_create_script(embedded_vm.vm(), cache, source_code, embedded_vm.realm(), ascii_view("test.js"sv), nullptr, errors ? &sink : nullptr);
}

// Reads the layout of a blob, as test-bytecode-cache.cpp does for the C++ runtime's blobs, to corrupt parts of it.
class BlobReader {
public:
    explicit BlobReader(ReadonlyBytes bytes)
        : m_bytes(bytes)
    {
    }

    void skip(size_t length)
    {
        VERIFY(m_offset + length <= m_bytes.size());
        m_offset += length;
    }

    void align_to(size_t alignment) { skip((alignment - (m_offset % alignment)) % alignment); }

    void align_bytes_payload_to(size_t alignment)
    {
        auto payload_offset = m_offset + sizeof(u32);
        skip((alignment - (payload_offset % alignment)) % alignment);
    }

    u8 read_u8()
    {
        VERIFY(m_offset < m_bytes.size());
        return m_bytes[m_offset++];
    }

    bool read_bool() { return read_u8() != 0; }

    u32 read_u32()
    {
        VERIFY(m_offset + sizeof(u32) <= m_bytes.size());
        u32 value = 0;
        for (size_t i = 0; i < sizeof(u32); ++i)
            value |= static_cast<u32>(m_bytes[m_offset + i]) << (8 * i);
        m_offset += sizeof(u32);
        return value;
    }

    void skip_utf16()
    {
        align_to(alignof(u16));
        skip(read_u32() * sizeof(u16));
    }

    void skip_utf16_vector()
    {
        auto length = read_u32();
        for (u32 i = 0; i < length; ++i)
            skip_utf16();
    }

    size_t offset() const { return m_offset; }

    // Magic, format version, runtime, program type, source hash, source length, top-level await and strictness.
    void skip_header() { skip(8 + sizeof(u32) + 1 + 1 + JS_BYTECODE_CACHE_SOURCE_HASH_SIZE + sizeof(u32) + 1 + 1); }

    void skip_script_declaration_metadata()
    {
        VERIFY(read_u8() == 0);
        for (size_t i = 0; i < 5; ++i)
            skip_utf16_vector();
        auto lexical_binding_count = read_u32();
        for (u32 i = 0; i < lexical_binding_count; ++i) {
            skip_utf16();
            skip(1);
        }
    }

    // Returns the number of declared functions.
    u32 enter_declaration_function_table()
    {
        auto count = read_u32();
        align_bytes_payload_to(bytecode_alignment);
        m_declaration_function_table_length = read_u32();
        return count;
    }

    void skip_declaration_function_table()
    {
        enter_declaration_function_table();
        skip(m_declaration_function_table_length);
    }

    // Leaves the reader at the start of the executable's bytecode.
    void enter_executable_bytecode()
    {
        skip(1);               // Strict mode.
        skip(sizeof(u32));     // Number of registers.
        skip(sizeof(u32));     // Number of arguments.
        skip(7 * sizeof(u32)); // Cache counters.
        skip(1);               // This value needs environment resolution.
        if (read_bool())       // Length identifier.
            skip(sizeof(u32));
        align_bytes_payload_to(bytecode_alignment);
        VERIFY(read_u32() > 0);
    }

    // Leaves the reader where the first declared function's source text starts, after its name.
    void enter_first_declaration_function()
    {
        if (read_bool())
            skip_utf16();
    }

    void skip_to_first_declaration_function_bytecode()
    {
        enter_first_declaration_function();
        skip(4 * sizeof(u32)); // Source text start and end, function length, formal parameter count.
        skip(3);               // Kind, strict mode, arrow function.
        if (read_bool())       // Simple parameter list.
            skip_utf16_vector();
        skip(2); // Uses this, uses this from environment.
        if (read_bool()) {
            skip_utf16(); // Class field initializer name.
            skip(1);
        }
        skip(3 + 2 * sizeof(u64) + 2); // Function metadata.
        align_bytes_payload_to(bytecode_alignment);
        VERIFY(read_u32() > 0);
        enter_executable_bytecode();
    }

private:
    // The alignment of bytecode within the blob.
    static constexpr size_t bytecode_alignment = 8;

    ReadonlyBytes m_bytes;
    size_t m_offset { 0 };
    size_t m_declaration_function_table_length { 0 };
};

size_t top_level_bytecode_offset(ReadonlyBytes blob)
{
    BlobReader reader { blob };
    reader.skip_header();
    reader.skip_script_declaration_metadata();
    reader.skip_declaration_function_table();
    reader.skip(1); // Program kind.
    reader.enter_executable_bytecode();
    return reader.offset();
}

size_t first_declaration_function_bytecode_offset(ReadonlyBytes blob)
{
    BlobReader reader { blob };
    reader.skip_header();
    reader.skip_script_declaration_metadata();
    VERIFY(reader.enter_declaration_function_table() > 0);
    reader.skip_to_first_declaration_function_bytecode();
    return reader.offset();
}

size_t first_declaration_function_source_text_start_offset(ReadonlyBytes blob)
{
    BlobReader reader { blob };
    reader.skip_header();
    reader.skip_script_declaration_metadata();
    VERIFY(reader.enter_declaration_function_table() > 0);
    reader.enter_first_declaration_function();
    return reader.offset();
}

ByteBuffer with_byte_flipped(ReadonlyBytes blob, size_t offset)
{
    auto corrupted = MUST(ByteBuffer::copy(blob));
    corrupted[offset] ^= 0xff;
    return corrupted;
}

// The embedder's side of compiling lazy functions off thread, which keeps the tasks for the test to run.
struct DeferredOffThreadCompilation {
    Vector<JSOffThreadTask> submitted_work;
    Vector<JSOffThreadTask> tasks_for_the_vms_thread;

    JSOffThreadCompilationCallbacks callbacks()
    {
        return {
            .context = this,
            .submit_work = [](void* context, JSOffThreadTask task) { static_cast<DeferredOffThreadCompilation*>(context)->submitted_work.append(task); },
            .post_to_main_thread = [](void* context, JSOffThreadTask task) { static_cast<DeferredOffThreadCompilation*>(context)->tasks_for_the_vms_thread.append(task); },
            .release = nullptr,
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

    void run_tasks_for_the_vms_thread()
    {
        for (auto task : exchange(tasks_for_the_vms_thread, {}))
            task.run(task.data);
    }
};

}

TEST_CASE(a_blob_made_and_validated_on_other_threads_runs_as_a_script_on_the_vms_thread)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto source = "var make_doubler = function (value) { return () => value * 2; };\nmake_doubler(21)() === 42;"sv;
    auto const* source_code = create_source_code("api.js"sv, source);
    auto source_length = js_source_code_length_in_code_units(source_code);
    auto programs = compile_on_another_thread(source_code, JS_PROGRAM_TYPE_SCRIPT);

    auto* script = js_compile_create_script_from_compiled_program(vm, programs.to_run, source_code, embedded_vm->realm(), ascii_view("api.js"sv), nullptr);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_HEAP_BYTECODE);
    EXPECT_EQ(run_script(vm, script), u"true"sv);

    // LibWeb decodes and validates a blob it loads on its thread pool, and creates the script on the VM's thread.
    Atomic<size_t> releases { 0 };
    JSDecodedBytecodeCache const* cache = nullptr;
    std::thread worker([&] {
        VERIFY(!decode_and_validate(programs.blob, JS_PROGRAM_TYPE_SCRIPT, source_length + 1, &releases));
        VERIFY(!decode_and_validate(programs.blob, JS_PROGRAM_TYPE_MODULE, source_length, &releases));
        cache = decode_and_validate(programs.blob, JS_PROGRAM_TYPE_SCRIPT, source_length, &releases);
    });
    worker.join();
    EXPECT_EQ(releases.load(), 2u);
    VERIFY(cache);

    auto* cached_script = create_script_from_cache(*embedded_vm, cache, source_code);
    VERIFY(cached_script);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(cached_script), JS_EXECUTABLE_BACKING_MAPPED_BYTECODE_CACHE);
    EXPECT_EQ(run_script(vm, cached_script), u"true"sv);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);
}

TEST_CASE(scripts_materialized_from_one_cache_run_independently)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto source = "function answer() { return 42; } globalThis.runs = (globalThis.runs ?? 0) + 1; answer() + runs"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto* cache = decode(blob_of(source), JS_PROGRAM_TYPE_SCRIPT);
    VERIFY(cache);
    auto* first = create_script_from_cache(*embedded_vm, cache, source_code);
    auto* second = create_script_from_cache(*embedded_vm, cache, source_code);
    VERIFY(first && second && first != second);
    js_bytecode_cache_release(cache);
    EXPECT_EQ(run_script(embedded_vm->vm(), first), u"43"sv);
    EXPECT_EQ(run_script(embedded_vm->vm(), second), u"44"sv);
    js_source_code_release(source_code);
}

TEST_CASE(blobs_that_do_not_match_their_source_or_are_corrupt_fail_to_materialize)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto source = "function f() { return 1; } f();"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto blob = blob_of(source);

    auto materialization_errors = [&](ByteBuffer const& bytes, JSSourceCode const* source_code_to_match) {
        // The corruptions keep the layout of the blob intact, so that only validating it against the source finds them.
        auto* cache = decode(bytes, JS_PROGRAM_TYPE_SCRIPT);
        VERIFY(cache);
        ParserErrors errors;
        EXPECT(!create_script_from_cache(*embedded_vm, cache, source_code_to_match, &errors));
        js_bytecode_cache_release(cache);
        return errors.messages;
    };
    Vector<Utf16String> const failure { "Failed to materialize bytecode cache"_utf16 };

    EXPECT_EQ(materialization_errors(with_byte_flipped(blob, top_level_bytecode_offset(blob)), source_code), failure);
    EXPECT_EQ(materialization_errors(with_byte_flipped(blob, first_declaration_function_bytecode_offset(blob)), source_code), failure);

    auto out_of_range_source_text = MUST(ByteBuffer::copy(blob));
    auto source_text_start = first_declaration_function_source_text_start_offset(blob);
    auto past_the_source = static_cast<u32>(source.length() + 1);
    for (size_t i = 0; i < sizeof(u32); ++i)
        out_of_range_source_text[source_text_start + i] = static_cast<u8>(past_the_source >> (8 * i));
    EXPECT_EQ(materialization_errors(out_of_range_source_text, source_code), failure);

    auto const* longer_source_code = create_source_code("test.js"sv, "function f() { return 1; } f(); "sv);
    EXPECT_EQ(materialization_errors(blob, longer_source_code), failure);
    js_source_code_release(longer_source_code);

    auto* cache = decode(blob, JS_PROGRAM_TYPE_SCRIPT);
    auto* script = create_script_from_cache(*embedded_vm, cache, source_code);
    VERIFY(script);
    EXPECT_EQ(run_script(embedded_vm->vm(), script), u"1"sv);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);
}

TEST_CASE(blobs_with_malformed_constants_fail_validation)
{
    // Only the first call of lazy() decodes its constants, and only creating a BigInt parses its digits.
    auto lazy_source = "var lazy = function lazy() { return 1234.5678; }; 0"sv;
    auto lazy_blob = blob_of(lazy_source);
    auto number_bits = bit_cast<u64>(1234.5678);
    Array<u8, sizeof(u64)> number_bytes;
    for (size_t i = 0; i < sizeof(u64); ++i)
        number_bytes[i] = static_cast<u8>(number_bits >> (8 * i));
    auto number_offset = AK::memmem_optional(lazy_blob.data(), lazy_blob.size(), number_bytes.data(), number_bytes.size());
    VERIFY(number_offset.has_value());
    auto with_malformed_lazy_constant = MUST(ByteBuffer::copy(lazy_blob));
    with_malformed_lazy_constant[*number_offset - 1] = 0xee; // The tag of the constant.

    auto big_int_source = "var big = 123456789n; big"sv;
    auto big_int_blob = blob_of(big_int_source);
    auto digits = "123456789"sv;
    auto digits_offset = AK::memmem_optional(big_int_blob.data(), big_int_blob.size(), digits.characters_without_null_termination(), digits.length());
    VERIFY(digits_offset.has_value());
    auto with_big_int_that_is_not_a_number = MUST(ByteBuffer::copy(big_int_blob));
    with_big_int_that_is_not_a_number[*digits_offset + 4] = 'z';

    Atomic<size_t> releases { 0 };
    EXPECT(!decode_and_validate(with_malformed_lazy_constant.bytes(), JS_PROGRAM_TYPE_SCRIPT, lazy_source.length(), &releases));
    EXPECT(!decode_and_validate(with_big_int_that_is_not_a_number.bytes(), JS_PROGRAM_TYPE_SCRIPT, big_int_source.length(), &releases));
    EXPECT_EQ(releases.load(), 2u);

    auto validates = [](ByteBuffer const& blob, StringView source) {
        auto* cache = decode_and_validate(blob.bytes(), JS_PROGRAM_TYPE_SCRIPT, source.length());
        if (cache)
            js_bytecode_cache_release(cache);
        return cache != nullptr;
    };
    EXPECT(validates(lazy_blob, lazy_source));
    EXPECT(validates(big_int_blob, big_int_source));
}

TEST_CASE(blobs_of_another_runtime_version_program_or_source_are_rejected_and_released)
{
    auto blob = blob_of("var x = 1;"sv);
    Atomic<size_t> releases { 0 };

    // A blob written for the C++ runtime is the same but for the tag that names its runtime, after the magic and the
    // format version.
    constexpr size_t runtime_offset = 8 + sizeof(u32);
    EXPECT_EQ(blob[runtime_offset], static_cast<u8>('R'));
    auto blob_of_the_cpp_runtime = MUST(ByteBuffer::copy(blob));
    blob_of_the_cpp_runtime[runtime_offset] = 'C';
    EXPECT(!decode(blob_of_the_cpp_runtime, JS_PROGRAM_TYPE_SCRIPT, &releases));

    auto blob_of_an_older_format = MUST(ByteBuffer::copy(blob));
    blob_of_an_older_format[8] -= 1;
    EXPECT(!decode(blob_of_an_older_format, JS_PROGRAM_TYPE_SCRIPT, &releases));

    EXPECT(!decode(blob, JS_PROGRAM_TYPE_MODULE, &releases));
    EXPECT(!decode(blob.bytes().trim(blob.size() - 1), JS_PROGRAM_TYPE_SCRIPT, &releases));

    auto* owner = new BlobOwner { MUST(ByteBuffer::copy(blob)), &releases };
    auto bytes = owner->bytes.bytes();
    auto other_source_hash = source_hash;
    other_source_hash[0] ^= 1;
    EXPECT(!js_bytecode_cache_decode(bytes.data(), bytes.size(), JS_PROGRAM_TYPE_SCRIPT, other_source_hash.data(), other_source_hash.size(), owner_of(owner)));
    EXPECT_EQ(releases.load(), 5u);

    auto* cache = decode(blob, JS_PROGRAM_TYPE_SCRIPT, &releases);
    VERIFY(cache);
    EXPECT_EQ(releases.load(), 5u);
    js_bytecode_cache_retain(cache);
    js_bytecode_cache_release(cache);
    EXPECT_EQ(releases.load(), 5u);
    js_bytecode_cache_release(cache);
    EXPECT_EQ(releases.load(), 6u);
}

TEST_CASE(the_blob_stays_alive_while_a_record_made_from_it_does)
{
    Atomic<size_t> releases { 0 };
    {
        auto embedded_vm = EmbeddedVM::create_with_realm();
        auto source = "var lazy = function () { return 'lazy'; }; lazy"sv;
        auto const* source_code = create_source_code("test.js"sv, source);
        auto* cache = decode(blob_of(source), JS_PROGRAM_TYPE_SCRIPT, &releases);
        auto* script = create_script_from_cache(*embedded_vm, cache, source_code);
        VERIFY(script);
        js_bytecode_cache_release(cache);
        js_source_code_release(source_code);
        embedded_vm->collect_garbage();
        EXPECT_EQ(releases.load(), 0u);
        // The function compiles from the blob only now.
        EXPECT_EQ(run_script(embedded_vm->vm(), script), u"function () { return 'lazy'; }"sv);
        EXPECT_EQ(evaluate(*embedded_vm, "lazy()"sv), u"lazy"sv);
        EXPECT_EQ(releases.load(), 0u);
    }
    EXPECT_EQ(releases.load(), 1u);
}

#if !defined(AK_OS_WINDOWS)
TEST_CASE(a_cached_script_runs_from_a_read_only_mapped_file)
{
    auto source = "let f = function mapped() { return 'hello'; }; f();"sv;
    auto blob = blob_of(source);
    char path[] = "/tmp/bytecode-cache-test-XXXXXX";
    auto fd = mkstemp(path);
    VERIFY(fd >= 0);
    VERIFY(write(fd, blob.data(), blob.size()) == static_cast<ssize_t>(blob.size()));
    auto* mapping = mmap(nullptr, blob.size(), PROT_READ, MAP_PRIVATE, fd, 0);
    VERIFY(mapping != MAP_FAILED);
    close(fd);
    unlink(path);

    struct Mapping {
        void* address { nullptr };
        size_t size { 0 };
        bool* unmapped { nullptr };
    };
    bool unmapped = false;
    JSBytecodeCacheBlobOwner owner { new Mapping { mapping, blob.size(), &unmapped }, [](void* owner) {
                                        auto* mapping = static_cast<Mapping*>(owner);
                                        VERIFY(munmap(mapping->address, mapping->size) == 0);
                                        *mapping->unmapped = true;
                                        delete mapping;
                                    } };
    {
        auto embedded_vm = EmbeddedVM::create_with_realm();
        auto const* source_code = create_source_code("test.js"sv, source);
        auto* cache = js_bytecode_cache_decode_and_validate(static_cast<u8 const*>(mapping), blob.size(), JS_PROGRAM_TYPE_SCRIPT, source_hash.data(), source_hash.size(), js_source_code_length_in_code_units(source_code), owner);
        VERIFY(cache);
        auto* script = create_script_from_cache(*embedded_vm, cache, source_code);
        VERIFY(script);
        js_bytecode_cache_release(cache);
        js_source_code_release(source_code);
        EXPECT_EQ(run_script(embedded_vm->vm(), script), u"hello"sv);
        EXPECT(!unmapped);
    }
    EXPECT(unmapped);
}
#endif

TEST_CASE(cached_functions_report_their_argument_names_to_the_debugger)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto source = "function inspect(argument) { debugger; } inspect(42);"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto* cache = decode(blob_of(source), JS_PROGRAM_TYPE_SCRIPT);
    auto* script = create_script_from_cache(*embedded_vm, cache, source_code);
    VERIFY(script);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);

    static Vector<ByteString> s_bindings;
    s_bindings.clear();
    js_debugger_enable(vm);
    js_debugger_set_pause_callback(
        vm, [](void*, JSVM* vm, JSDebuggerPauseInfo const* pause_info) {
            VERIFY(pause_info->stack_frame_count > 0);
            JSDebuggerFrameBindingSink sink {
                .context = vm,
                .append = [](void* context, JSDebuggerFrameBinding const* binding) {
                    s_bindings.append(ByteString::formatted("{}={}", byte_string_of(binding->name), to_string(static_cast<JSVM*>(context), binding->value)));
                },
            };
            js_debugger_bindings_for_frame(vm, pause_info->stack_frames[0].execution_context, &sink);
            js_debugger_continue_execution(vm, JS_RESUME_MODE_CONTINUE);
        },
        nullptr);
    EXPECT_EQ(js_script_run(vm, script, nullptr).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(s_bindings, Vector<ByteString> { "argument=42"sv });
    js_debugger_disable(vm);
}

TEST_CASE(function_source_text_comes_from_the_source_code_of_a_cached_script)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto source = "// Known Trick\xe2\x84\xa2\nlet f = function mapped() { return 'Known Trick\xe2\x84\xa2'; };\n"
                  "class Point { constructor() { this.x = 1; } }\n"
                  "[f.toString(), f.toString(), Point.toString(), (x => x).toString()].join('|')"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto* cache = decode(blob_of(source), JS_PROGRAM_TYPE_SCRIPT);
    auto* script = create_script_from_cache(*embedded_vm, cache, source_code);
    VERIFY(script);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);
    EXPECT_EQ(run_script(embedded_vm->vm(), script), Utf16String::from_utf8("function mapped() { return 'Known Trick\xe2\x84\xa2'; }|function mapped() { return 'Known Trick\xe2\x84\xa2'; }|class Point { constructor() { this.x = 1; } }|x => x"sv));
}

TEST_CASE(installing_a_cache_keeps_the_template_objects_and_state_of_a_running_script)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto source = "var tag = function (strings) { return strings; };\n"
                  "var f = function (use_template) { if (use_template) return tag`hello`; return null; };\n"
                  "var C = class { constructor() { this.value = 1; } method() { return this.value; } };\n"
                  "var before = f(true); new C().method();"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto* script = js_script_parse(vm, embedded_vm->realm(), ascii_view(source), ascii_view("test.js"sv), ascii_view(""sv), nullptr, 1, nullptr);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_SOURCE);
    EXPECT_EQ(run_script(vm, script), u"1"sv);

    auto* cache = decode(blob_of(source), JS_PROGRAM_TYPE_SCRIPT);
    // The class constructor ran, and its source text grew to that of the class, but the blob still finds it.
    js_bytecode_cache_script_begin_generation(vm, script);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_GENERATING_FRESH_CACHE_FROM_SOURCE);
    js_bytecode_cache_script_finish_generation_without_install(vm, script);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_SOURCE);
    js_bytecode_cache_script_begin_generation(vm, script);
    js_bytecode_cache_script_install_generated(vm, script, cache, source_code);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_MAPPED_BYTECODE_CACHE);
    EXPECT(!js_bytecode_cache_script_try_install(vm, script, cache, source_code));
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);

    // f() runs an executable from the blob now, which shares the template object cache of the one it replaced.
    EXPECT_EQ(evaluate(*embedded_vm, "[f(true) === before, new C().method(), f.toString().length > 0].join()"sv), u"true,1,true"sv);
}

TEST_CASE(a_failed_install_leaves_the_script_as_it_was)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto source = "function declared() { return 1; } var f = function () { return declared(); }; f();"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto* script = js_script_parse(vm, embedded_vm->realm(), ascii_view(source), ascii_view("test.js"sv), ascii_view(""sv), nullptr, 1, nullptr);
    EXPECT_EQ(run_script(vm, script), u"1"sv);

    auto blob = blob_of(source);
    for (auto offset : { top_level_bytecode_offset(blob), first_declaration_function_bytecode_offset(blob) }) {
        auto* corrupt = decode(with_byte_flipped(blob, offset), JS_PROGRAM_TYPE_SCRIPT);
        VERIFY(corrupt);
        EXPECT(!js_bytecode_cache_script_try_install(vm, script, corrupt, source_code));
        js_bytecode_cache_release(corrupt);
    }
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_SOURCE);
    EXPECT_EQ(run_script(vm, script), u"1"sv);

    auto* cache = decode(blob, JS_PROGRAM_TYPE_SCRIPT);
    EXPECT(js_bytecode_cache_script_try_install(vm, script, cache, source_code));
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);
    EXPECT_EQ(run_script(vm, script), u"1"sv);
}

TEST_CASE(lazy_functions_compiled_off_thread_give_way_to_an_installed_cache)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    auto source = "var lazy = function () { function nested() { return 'nested'; } return nested(); }; 0"sv;
    auto const* source_code = create_source_code("test.js"sv, source);
    auto programs = compile_on_another_thread(source_code, JS_PROGRAM_TYPE_SCRIPT);
    auto* script = js_compile_create_script_from_compiled_program(vm, programs.to_run, source_code, embedded_vm->realm(), ascii_view("test.js"sv), nullptr);
    EXPECT_EQ(run_script(vm, script), u"0"sv);

    // A worker compiles lazy() while the cache that LibWeb generated in the meantime gets installed.
    DeferredOffThreadCompilation compilation;
    auto callbacks = compilation.callbacks();
    js_compile_remaining_functions_of_script_off_thread(vm, script, &callbacks);
    EXPECT_EQ(compilation.submitted_work.size(), 1u);
    compilation.run_submitted_work_on_a_worker();

    js_bytecode_cache_script_begin_generation(vm, script);
    EXPECT_EQ(js_bytecode_cache_script_executable_backing(script), JS_EXECUTABLE_BACKING_GENERATING_FRESH_CACHE_FROM_HEAP_BYTECODE);
    auto* cache = decode(programs.blob, JS_PROGRAM_TYPE_SCRIPT);
    js_bytecode_cache_script_install_generated(vm, script, cache, source_code);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);

    compilation.run_tasks_for_the_vms_thread();
    EXPECT_EQ(evaluate(*embedded_vm, "lazy()"sv), u"nested"sv);
}

TEST_CASE(installing_a_cache_into_modules_replaces_their_bodies)
{
    auto embedded_vm = EmbeddedVM::create_with_realm();
    auto* vm = embedded_vm->vm();
    for (auto source : { "export const value = await Promise.resolve(1);"sv, "export function plain() { return 'plain'; }"sv }) {
        auto const* source_code = create_source_code("test.mjs"sv, source);
        auto* module = js_module_parse_source_text_module(vm, embedded_vm->realm(), ascii_view(source), ascii_view("test.mjs"sv), ascii_view(""sv), nullptr, 0, nullptr);
        VERIFY(module);
        EXPECT_EQ(js_bytecode_cache_module_executable_backing(module), JS_EXECUTABLE_BACKING_SOURCE);
        auto* cache = decode(blob_of(source, JS_PROGRAM_TYPE_MODULE), JS_PROGRAM_TYPE_MODULE);
        VERIFY(cache);
        js_bytecode_cache_module_begin_generation(vm, module);
        EXPECT_EQ(js_bytecode_cache_module_executable_backing(module), JS_EXECUTABLE_BACKING_GENERATING_FRESH_CACHE_FROM_SOURCE);
        js_bytecode_cache_module_install_generated(vm, module, cache, source_code);
        EXPECT_EQ(js_bytecode_cache_module_executable_backing(module), JS_EXECUTABLE_BACKING_MAPPED_BYTECODE_CACHE);
        EXPECT(!js_bytecode_cache_module_try_install(vm, module, cache, source_code));
        js_bytecode_cache_release(cache);
        js_source_code_release(source_code);
    }
}

namespace {

// Loads every module a test imports from source text it registered under the module's specifier.
struct SourceTextModuleLoader {
    EmbeddedVM* embedded_vm { nullptr };
    Vector<std::pair<StringView, StringView>> sources;
};

SourceTextModuleLoader s_module_loader;

void load_imported_module(void*, JSVM* vm, JSImportedModuleReferrer referrer, JSModuleRequest const* module_request, void*, JSImportedModulePayload payload)
{
    auto specifier = byte_string_of(js_module_request_specifier(module_request));
    auto source = s_module_loader.sources.first_matching([&](auto const& entry) { return entry.first == specifier.view(); });
    VERIFY(source.has_value());
    auto* module = js_module_parse_source_text_module(vm, s_module_loader.embedded_vm->realm(), ascii_view(source->second), ascii_view(specifier), ascii_view(""sv), nullptr, 0, nullptr);
    VERIFY(module);
    js_module_finish_loading_imported_module(vm, referrer, module_request, payload, { reinterpret_cast<uintptr_t>(module), JS_COMPLETION_NORMAL });
}

constexpr JSVmHostHooks module_loading_hooks {
    .ensure_can_add_private_element = nullptr,
    .ensure_can_compile_strings = nullptr,
    .get_code_for_eval = nullptr,
    .promise_rejection_tracker = nullptr,
    .call_job_callback = nullptr,
    .enqueue_finalization_registry_cleanup_job = nullptr,
    .enqueue_promise_job = nullptr,
    .promise_job_queue_is_empty = nullptr,
    .make_job_callback = nullptr,
    .get_import_meta_properties = nullptr,
    .finalize_import_meta = nullptr,
    .get_supported_import_attributes = nullptr,
    .load_imported_module = load_imported_module,
    .unrecognized_date_string = nullptr,
    .resize_array_buffer = nullptr,
    .grow_shared_array_buffer = nullptr,
    .system_utc_epoch_nanoseconds = nullptr,
    .on_unimplemented_property_access = nullptr,
};

void collect_binding_name(void* context, u16 const* code_units, size_t length)
{
    static_cast<Vector<ByteString>*>(context)->append(byte_string_of({ code_units, length, false }));
}

}

TEST_CASE(a_cached_module_resolves_a_reexported_import_to_the_module_that_declares_it)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    s_module_loader = { embedded_vm.ptr(), { { "./source.mjs"sv, "export function pass() { return 'pass'; }"sv } } };
    js_vm_set_embedder(vm, &module_loading_hooks, nullptr);

    auto source = "import { pass as renamed } from './source.mjs'; export { renamed as default };\n"
                  "export const awaited = await Promise.resolve(renamed());"sv;
    auto const* source_code = create_source_code("test.mjs"sv, source);
    auto* cache = decode(blob_of(source, JS_PROGRAM_TYPE_MODULE), JS_PROGRAM_TYPE_MODULE);
    VERIFY(cache);
    auto* module = js_bytecode_cache_create_module(vm, cache, source_code, embedded_vm->realm(), ascii_view("test.mjs"sv), nullptr, nullptr);
    VERIFY(module);
    js_bytecode_cache_release(cache);
    js_source_code_release(source_code);
    EXPECT_EQ(js_bytecode_cache_module_executable_backing(module), JS_EXECUTABLE_BACKING_MAPPED_BYTECODE_CACHE);

    js_module_load_requested_modules(vm, module, nullptr);
    EXPECT_EQ(js_module_link(vm, module).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_module_evaluate(vm, module).variant, JS_COMPLETION_NORMAL);
    js_vm_run_queued_promise_jobs(vm);

    Vector<ByteString> binding_names;
    JSResolvedBinding resolution { .module = nullptr, .binding_name = { .context = &binding_names, .append = collect_binding_name }, .type = JS_RESOLVED_BINDING_NULL };
    js_module_resolve_export(vm, module, ascii_view("default"sv), &resolution);
    EXPECT_EQ(resolution.type, JS_RESOLVED_BINDING_BINDING_NAME);
    EXPECT(resolution.module && resolution.module != module);
    EXPECT_EQ(binding_names, Vector<ByteString> { "pass"sv });

    auto* namespace_object = js_module_get_module_namespace(vm, module);
    auto awaited_name = Utf16FlyString::from_utf8("awaited"sv);
    JSPropertyKey awaited_key { awaited_name.raw_identity() };
    auto awaited = js_object_get(vm, namespace_object, &awaited_key);
    EXPECT_EQ(awaited.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(to_string(vm, awaited.payload), u"pass"sv);

    js_vm_set_embedder(vm, nullptr, nullptr);
    s_module_loader = {};
}
