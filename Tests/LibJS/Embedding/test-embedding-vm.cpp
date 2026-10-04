/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/Vector.h>
#include <LibGC/Heap.h>

#include "EmbeddingTest.h"

namespace {

// An embedder that records what its hooks see and queues promise jobs the way an event loop queues microtasks.
struct TestEmbedder {
    JSVM* vm { nullptr };
    EmbeddedVM* embedded_vm { nullptr };

    // The promise jobs, which stay rooted until they run.
    Vector<GCRoot*> promise_jobs;

    size_t private_element_checks { 0 };
    Vector<u8> compilation_types;
    bool reentered_while_compiling { false };
    size_t code_for_eval_lookups { 0 };
    Vector<u8> promise_rejection_operations;
    size_t job_callbacks_made { 0 };
    size_t job_callbacks_called { 0 };
    size_t promise_job_queue_checks { 0 };
    size_t finalization_registry_cleanups { 0 };
    size_t supported_import_attribute_lookups { 0 };
    Vector<u8> imported_module_referrer_kinds;
    Vector<u8> imported_module_payload_kinds;
    Vector<ByteString> unrecognized_date_strings;
    size_t array_buffer_resizes { 0 };
    size_t shared_array_buffer_grows { 0 };
};

constexpr i64 test_epoch_nanoseconds = 1234567890123456789;

TestEmbedder& embedder_of(void* data)
{
    return *static_cast<TestEmbedder*>(data);
}

JSCompletion normal_completion()
{
    return { 0, JS_COMPLETION_NORMAL };
}

ByteString ascii_string(JSUtf16View view)
{
    VERIFY(view.has_ascii_storage);
    return ByteString(static_cast<char const*>(view.data), view.length_in_code_units);
}

void run_promise_jobs(TestEmbedder& embedder)
{
    while (!embedder.promise_jobs.is_empty()) {
        auto* root = embedder.promise_jobs.take_first();
        auto completion = js_promise_job_run(embedder.vm, reinterpret_cast<JSPromiseJob*>(gc_root_cell(root)));
        gc_root_destroy(root);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    }
}

JSCompletion ensure_can_add_private_element(void* data, JSVM*, JSObject*)
{
    ++embedder_of(data).private_element_checks;
    return normal_completion();
}

JSCompletion ensure_can_compile_strings(void* data, JSVM*, JSEnsureCanCompileStringsArguments const* arguments)
{
    auto& embedder = embedder_of(data);
    embedder.compilation_types.append(arguments->compilation_type);
    auto code = ascii_string(arguments->code_string);
    if (code == "forbidden"sv)
        return { arguments->body_arg, JS_COMPLETION_THROW };
    if (code == "'reenter'"sv) {
        // Runs a script, and with it this hook again, while the VM waits for the hook.
        embedder.reentered_while_compiling = embedder.embedded_vm->run("if (eval('6 * 7') !== 42) throw new Error();"sv);
    }
    return normal_completion();
}

bool get_code_for_eval(void* data, JSVM*, JSObject*, JSValue*)
{
    ++embedder_of(data).code_for_eval_lookups;
    return false;
}

void promise_rejection_tracker(void* data, JSVM*, JSObject*, u8 operation)
{
    embedder_of(data).promise_rejection_operations.append(operation);
}

JSCompletion call_job_callback(void* data, JSVM* vm, JSJobCallback* job_callback, JSValue this_value, JSValue const* arguments, size_t argument_count)
{
    ++embedder_of(data).job_callbacks_called;
    return js_vm_default_host_call_job_callback(vm, job_callback, this_value, arguments, argument_count);
}

void enqueue_finalization_registry_cleanup_job(void* data, JSVM*, JSObject*)
{
    ++embedder_of(data).finalization_registry_cleanups;
}

void enqueue_promise_job(void* data, JSVM*, JSPromiseJob* job, JSRealm*)
{
    embedder_of(data).promise_jobs.append(gc_root_create(reinterpret_cast<GCCell*>(job)));
}

bool promise_job_queue_is_empty(void* data, JSVM*)
{
    auto& embedder = embedder_of(data);
    ++embedder.promise_job_queue_checks;
    return embedder.promise_jobs.is_empty();
}

JSJobCallback* make_job_callback(void* data, JSVM* vm, JSObject* callable)
{
    ++embedder_of(data).job_callbacks_made;
    return js_realm_job_callback_create(vm, callable, nullptr);
}

void get_supported_import_attributes(void* data, JSVM*, JSStringSink* attribute_keys)
{
    ++embedder_of(data).supported_import_attribute_lookups;
    static constexpr u16 type[] = { 't', 'y', 'p', 'e' };
    attribute_keys->append(attribute_keys->context, type, array_size(type));
}

void load_imported_module(void* data, JSVM*, JSImportedModuleReferrer referrer, JSModuleRequest const*, void*, JSImportedModulePayload payload)
{
    // The module never loads, so the import() stays pending.
    auto& embedder = embedder_of(data);
    embedder.imported_module_referrer_kinds.append(referrer.kind);
    embedder.imported_module_payload_kinds.append(payload.kind);
}

void unrecognized_date_string(void* data, JSVM*, JSUtf16View date_string)
{
    embedder_of(data).unrecognized_date_strings.append(ascii_string(date_string));
}

JSCompletion resize_array_buffer(void* data, JSVM* vm, JSObject* buffer, size_t new_byte_length)
{
    ++embedder_of(data).array_buffer_resizes;
    return js_vm_default_host_resize_array_buffer(vm, buffer, new_byte_length);
}

JSCompletion grow_shared_array_buffer(void* data, JSVM* vm, JSObject* buffer, size_t new_byte_length)
{
    ++embedder_of(data).shared_array_buffer_grows;
    return js_vm_default_host_grow_shared_array_buffer(vm, buffer, new_byte_length);
}

i64 system_utc_epoch_nanoseconds(void*, JSVM*, JSObject*)
{
    return test_epoch_nanoseconds;
}

JSVmHostHooks const test_hooks {
    .ensure_can_add_private_element = ensure_can_add_private_element,
    .ensure_can_compile_strings = ensure_can_compile_strings,
    .get_code_for_eval = get_code_for_eval,
    .promise_rejection_tracker = promise_rejection_tracker,
    .call_job_callback = call_job_callback,
    .enqueue_finalization_registry_cleanup_job = enqueue_finalization_registry_cleanup_job,
    .enqueue_promise_job = enqueue_promise_job,
    .promise_job_queue_is_empty = promise_job_queue_is_empty,
    .make_job_callback = make_job_callback,
    .get_import_meta_properties = nullptr,
    .finalize_import_meta = nullptr,
    .get_supported_import_attributes = get_supported_import_attributes,
    .load_imported_module = load_imported_module,
    .unrecognized_date_string = unrecognized_date_string,
    .resize_array_buffer = resize_array_buffer,
    .grow_shared_array_buffer = grow_shared_array_buffer,
    .system_utc_epoch_nanoseconds = system_utc_epoch_nanoseconds,
    .on_unimplemented_property_access = nullptr,
};

struct EmbeddedVMWithHooks {
    NonnullOwnPtr<EmbeddedVM> embedded_vm { EmbeddedVM::create({ .become_process_default_heap = true, .shared_memory_shared_array_buffers = false }) };
    TestEmbedder embedder;

    EmbeddedVMWithHooks()
    {
        embedder.vm = embedded_vm->vm();
        embedder.embedded_vm = embedded_vm.ptr();
        js_vm_set_embedder(embedder.vm, &test_hooks, &embedder);
        embedded_vm->initialize_realm();
    }

    ~EmbeddedVMWithHooks()
    {
        for (auto* root : embedder.promise_jobs)
            gc_root_destroy(root);
    }
};

}

TEST_CASE(a_vm_in_embedder_storage_can_make_its_heap_the_process_default)
{
    auto embedded_vm = EmbeddedVM::create({ .become_process_default_heap = true, .shared_memory_shared_array_buffers = false });
    auto* heap = reinterpret_cast<GC::Heap*>(js_vm_heap(embedded_vm->vm()));
    EXPECT_EQ(heap, &GC::Heap::the());

    // The heap gathers its roots from the VM at the address it was constructed at.
    js_vm_collect_garbage(embedded_vm->vm());
    heap->collect_garbage();
}

TEST_CASE(a_vm_needs_storage_of_its_size_and_alignment)
{
    alignas(JS_LAYOUT_VM_ALIGN) static u8 storage[JS_LAYOUT_VM_SIZE + JS_LAYOUT_VM_ALIGN];
    EXPECT(!js_vm_construct_at(nullptr, sizeof(storage), JS_LAYOUT_VM_ALIGN, nullptr));
    EXPECT(!js_vm_construct_at(storage, 64, JS_LAYOUT_VM_ALIGN, nullptr));
    EXPECT(!js_vm_construct_at(storage, sizeof(storage), 1, nullptr));
    EXPECT(!js_vm_construct_at(storage + 1, JS_LAYOUT_VM_SIZE, JS_LAYOUT_VM_ALIGN, nullptr));

    EXPECT(js_vm_construct_at(storage, JS_LAYOUT_VM_SIZE, JS_LAYOUT_VM_ALIGN, nullptr));
    auto* vm = reinterpret_cast<JSVM*>(storage);
    js_vm_finish_execution_generation(vm);
    js_vm_collect_garbage(vm);
    js_vm_destroy_at(vm);
}

TEST_CASE(a_realm_of_the_embedder_evaluates_scripts)
{
    auto embedded_vm = EmbeddedVM::create();
    embedded_vm->initialize_realm();
    EXPECT(embedded_vm->realm() != nullptr);
    u8 const* running_execution_context = nullptr;
    __builtin_memcpy(&running_execution_context, reinterpret_cast<u8 const*>(embedded_vm->vm()) + JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET, sizeof(running_execution_context));
    EXPECT_EQ(running_execution_context, embedded_vm->realm_execution_context());

    EXPECT(embedded_vm->run("if (1 + 1 !== 2) throw new Error();"sv));
    EXPECT(embedded_vm->run("var counter = 41;"sv));
    js_vm_collect_garbage(embedded_vm->vm());
    EXPECT(embedded_vm->run("if (++counter !== 42 || typeof globalThis.Array !== 'function') throw new Error();"sv));

    EXPECT_EQ(embedded_vm->evaluate("throw new TypeError();"sv).variant, JS_COMPLETION_THROW);
    EXPECT_EQ(embedded_vm->evaluate("let = = ;"sv).variant, JS_COMPLETION_THROW);
    // A global lexical declaration of an earlier script conflicts with one of a later script before the later runs.
    EXPECT(embedded_vm->run("let declared_by_two_scripts;"sv));
    EXPECT_EQ(embedded_vm->evaluate("let declared_by_two_scripts;"sv).variant, JS_COMPLETION_THROW);
}

TEST_CASE(scripts_reach_the_host_hooks_of_the_embedder)
{
    EmbeddedVMWithHooks setup;
    auto& embedder = setup.embedder;
    auto& embedded_vm = *setup.embedded_vm;

    EXPECT(embedded_vm.run(R"~~~(
        class WithPrivateField { #field = 1; }
        new WithPrivateField();

        if (eval("1 + 1") !== 2 || (0, eval)("2 + 2") !== 4 || new Function("a", "return a * 2")(21) !== 42)
            throw new Error("compiling strings");
        try {
            eval("forbidden");
            throw new Error("compiled forbidden code");
        } catch (error) {
            if (error !== "forbidden")
                throw error;
        }

        const not_code = {};
        if (eval(not_code) !== not_code)
            throw new Error("an object without code");

        Promise.reject(1).catch(() => {});
        globalThis.log = [];
        Promise.resolve(1).then(value => log.push(value));
        (async () => { await 2; await 3; log.push("async"); })();

        import("./missing.mjs", { with: { type: "json" } });

        if (!isNaN(new Date("not a date")))
            throw new Error("a date that is not one");

        const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
        buffer.resize(12);
        const shared_buffer = new SharedArrayBuffer(8, { maxByteLength: 16 });
        shared_buffer.grow(12);
        if (buffer.byteLength !== 12 || shared_buffer.byteLength !== 12)
            throw new Error("resizing buffers");

        if (Temporal.Now.instant().epochNanoseconds !== 1234567890123456789n)
            throw new Error("the time of the embedder");
    )~~~"sv));

    EXPECT(!embedder.promise_jobs.is_empty());
    run_promise_jobs(embedder);
    EXPECT(embedded_vm.run("if (log.join() !== '1,async') throw new Error(log.join());"sv));

    EXPECT(embedder.private_element_checks >= 1);
    EXPECT_EQ(embedder.compilation_types, (Vector<u8> { JS_COMPILATION_TYPE_DIRECT_EVAL, JS_COMPILATION_TYPE_INDIRECT_EVAL, JS_COMPILATION_TYPE_FUNCTION, JS_COMPILATION_TYPE_DIRECT_EVAL }));
    EXPECT_EQ(embedder.code_for_eval_lookups, 1u);
    EXPECT_EQ(embedder.promise_rejection_operations, (Vector<u8> { JS_PROMISE_REJECTION_OPERATION_REJECT, JS_PROMISE_REJECTION_OPERATION_HANDLE }));
    EXPECT(embedder.job_callbacks_made >= 2);
    EXPECT(embedder.job_callbacks_called >= 1);
    EXPECT(embedder.promise_job_queue_checks >= 1);
    EXPECT(embedder.supported_import_attribute_lookups >= 1);
    EXPECT_EQ(embedder.imported_module_referrer_kinds, Vector<u8> { JS_IMPORTED_MODULE_REFERRER_SCRIPT });
    EXPECT_EQ(embedder.imported_module_payload_kinds, Vector<u8> { JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY });
    EXPECT_EQ(embedder.unrecognized_date_strings, Vector<ByteString> { "not a date" });
    EXPECT_EQ(embedder.array_buffer_resizes, 1u);
    EXPECT_EQ(embedder.shared_array_buffer_grows, 1u);
}

TEST_CASE(a_finalization_registry_leaves_its_cleanup_to_the_embedder)
{
    EmbeddedVMWithHooks setup;
    EXPECT(setup.embedded_vm->run(R"~~~(
        globalThis.registry = new FinalizationRegistry(() => {});
        (() => {
            for (let i = 0; i < 100; ++i)
                registry.register({}, i);
        })();
    )~~~"sv));
    js_vm_collect_garbage(setup.embedder.vm);
    EXPECT(setup.embedder.finalization_registry_cleanups >= 1);
}

TEST_CASE(a_hook_can_run_scripts_while_the_vm_waits_for_it)
{
    EmbeddedVMWithHooks setup;
    EXPECT(setup.embedded_vm->run("if (eval(\"'reenter'\") !== 'reenter') throw new Error();"sv));
    EXPECT(setup.embedder.reentered_while_compiling);
    EXPECT_EQ(setup.embedder.compilation_types, (Vector<u8> { JS_COMPILATION_TYPE_DIRECT_EVAL, JS_COMPILATION_TYPE_DIRECT_EVAL }));
}

TEST_CASE(an_embedder_can_give_its_hooks_back_to_the_runtime)
{
    EmbeddedVMWithHooks setup;
    auto& embedder = setup.embedder;
    auto& embedded_vm = *setup.embedded_vm;
    EXPECT(embedded_vm.run("globalThis.log = []; Promise.resolve().then(() => log.push('queued by the embedder'));"sv));
    EXPECT_EQ(embedder.promise_jobs.size(), 1u);

    // Without hooks, the VM queues promise jobs itself again, and runs them after each script.
    js_vm_set_embedder(embedder.vm, nullptr, nullptr);
    EXPECT(embedded_vm.run("Promise.resolve().then(() => log.push('queued by the VM'));"sv));
    EXPECT_EQ(embedder.promise_jobs.size(), 1u);
    run_promise_jobs(embedder);
    EXPECT(embedded_vm.run("if (log.join() !== 'queued by the VM,queued by the embedder') throw new Error(log.join());"sv));
}
