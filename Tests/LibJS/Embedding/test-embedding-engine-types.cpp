/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Vector.h>
#include <LibGC/Function.h>
#include <LibGC/Heap.h>

#include "EmbeddingTest.h"

namespace {

// What a facade's VM::argument() reads: the arguments are the last slots of the running execution context, which
// follow its fixed fields.
JSValue argument(JSVM* vm, size_t index)
{
    u8 const* context = nullptr;
    __builtin_memcpy(&context, reinterpret_cast<u8 const*>(vm) + JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET, sizeof(context));
    auto slot_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET);
    auto argument_count = *reinterpret_cast<u32 const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
    if (index >= argument_count)
        return js_undefined;
    auto const* slots = reinterpret_cast<JSValue const*>(context + JS_LAYOUT_EXECUTION_CONTEXT_SIZE);
    return slots[slot_count - argument_count + index];
}

i32 int32_of(JSValue value)
{
    return static_cast<i32>(static_cast<u32>(value));
}

JSCompletion normal_completion(JSValue value = 0)
{
    return { value, JS_COMPLETION_NORMAL };
}

using ClosureFunction = GC::Function<JSCompletion(JSVM*)>;

// The facade's thunk for every closure: the context is the GC::Function that wraps the AK::Function.
JSCompletion call_closure_function(void* context, JSVM* vm)
{
    return static_cast<ClosureFunction*>(context)->function()(vm);
}

template<typename Callable>
JSObject* create_closure(EmbeddedVM& embedded_vm, Callable callable)
{
    auto function = GC::create_function(GC::Heap::the(), move(callable));
    return js_function_create_closure_with_name(embedded_vm.vm(), embedded_vm.realm(), ascii_view(""sv), call_closure_function, function.ptr());
}

// An embedder that queues promise jobs the way HTML queues microtasks, and records what its promise rejection tracker
// sees.
struct EngineTypesEmbedder {
    JSVM* vm { nullptr };
    Vector<GCRoot*> promise_jobs;
    struct TrackedRejection {
        u8 operation;
        bool promise_was_handled;
    };
    Vector<TrackedRejection> tracked_rejections;
};

EngineTypesEmbedder& embedder_of(void* data)
{
    return *static_cast<EngineTypesEmbedder*>(data);
}

void promise_rejection_tracker(void* data, JSVM*, JSObject* promise, u8 operation)
{
    embedder_of(data).tracked_rejections.append({ operation, js_promise_is_handled(promise) });
}

void enqueue_promise_job(void* data, JSVM*, JSPromiseJob* job, JSRealm*)
{
    embedder_of(data).promise_jobs.append(gc_root_create(reinterpret_cast<GCCell*>(job)));
}

bool promise_job_queue_is_empty(void* data, JSVM*)
{
    return embedder_of(data).promise_jobs.is_empty();
}

JSVmHostHooks const engine_types_hooks {
    .ensure_can_add_private_element = nullptr,
    .ensure_can_compile_strings = nullptr,
    .get_code_for_eval = nullptr,
    .promise_rejection_tracker = promise_rejection_tracker,
    .call_job_callback = nullptr,
    .enqueue_finalization_registry_cleanup_job = nullptr,
    .enqueue_promise_job = enqueue_promise_job,
    .promise_job_queue_is_empty = promise_job_queue_is_empty,
    .make_job_callback = nullptr,
    .get_import_meta_properties = nullptr,
    .finalize_import_meta = nullptr,
    .get_supported_import_attributes = nullptr,
    .load_imported_module = nullptr,
    .unrecognized_date_string = nullptr,
    .resize_array_buffer = nullptr,
    .grow_shared_array_buffer = nullptr,
    .system_utc_epoch_nanoseconds = nullptr,
    .on_unimplemented_property_access = nullptr,
};

struct EmbeddedVMWithQueues {
    NonnullOwnPtr<EmbeddedVM> embedded_vm { EmbeddedVM::create(EmbeddedVM::process_default_heap_options) };
    EngineTypesEmbedder embedder;

    EmbeddedVMWithQueues()
    {
        embedder.vm = embedded_vm->vm();
        js_vm_set_embedder(embedder.vm, &engine_types_hooks, &embedder);
        embedded_vm->initialize_realm();
    }

    ~EmbeddedVMWithQueues()
    {
        for (auto* root : embedder.promise_jobs)
            gc_root_destroy(root);
    }

    // Runs the queued promise jobs, and the ones they queue, on top of the realm's execution context.
    void drain_promise_jobs()
    {
        while (!embedder.promise_jobs.is_empty()) {
            auto* root = embedder.promise_jobs.take_first();
            auto completion = js_promise_job_run(embedder.vm, reinterpret_cast<JSPromiseJob*>(gc_root_cell(root)));
            gc_root_destroy(root);
            VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        }
    }
};

JSPromiseCapability* new_promise_capability(EmbeddedVM& embedded_vm)
{
    auto* promise_constructor = embedded_vm.intrinsic(JS_INTRINSIC_PROMISE_CONSTRUCTOR);
    return pointer_of_payload<JSPromiseCapability>(js_promise_capability_new(embedded_vm.vm(), value_of_object(promise_constructor)));
}

}

TEST_CASE(promises_settle_through_the_jobs_the_embedder_drains)
{
    EmbeddedVMWithQueues setup;
    auto& embedded_vm = *setup.embedded_vm;
    auto* vm = embedded_vm.vm();

    auto* capability = new_promise_capability(embedded_vm);
    auto* promise = js_promise_capability_promise(capability);
    EXPECT_EQ(js_promise_state(promise), JS_PROMISE_STATE_PENDING);

    // The reaction runs a script while the job runs it.
    Vector<JSValue> fulfilled_values;
    auto* on_fulfilled = create_closure(embedded_vm, [&](JSVM* vm) -> JSCompletion {
        auto value = argument(vm, 0);
        fulfilled_values.append(value);
        auto script = embedded_vm.evaluate("globalThis.reacted = (globalThis.reacted ?? 0) + 1;"sv);
        if (script.variant != JS_COMPLETION_NORMAL)
            return script;
        return normal_completion(int32_value(int32_of(value) + 1));
    });
    auto* chained_capability = new_promise_capability(embedded_vm);
    auto* chained = js_promise_capability_promise(chained_capability);
    EXPECT_EQ(js_promise_perform_then(vm, promise, value_of_object(on_fulfilled), js_undefined, chained_capability), value_of_object(chained));

    JSValue resolution[] = { int32_value(41) };
    EXPECT_EQ(js_function_call(vm, value_of_object(js_promise_capability_resolve(capability)), js_undefined, resolution, 1).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_promise_state(promise), JS_PROMISE_STATE_FULFILLED);
    EXPECT_EQ(js_promise_result(promise), int32_value(41));
    EXPECT(fulfilled_values.is_empty());
    EXPECT_EQ(setup.embedder.promise_jobs.size(), 1u);

    embedded_vm.collect_garbage();
    setup.drain_promise_jobs();
    EXPECT_EQ(fulfilled_values, Vector<JSValue> { int32_value(41) });
    EXPECT_EQ(js_promise_state(chained), JS_PROMISE_STATE_FULFILLED);
    EXPECT_EQ(js_promise_result(chained), int32_value(42));
    EXPECT(embedded_vm.run("if (reacted !== 1) throw new Error();"sv));
}

TEST_CASE(rejections_reach_the_tracker_until_a_reaction_handles_them)
{
    EmbeddedVMWithQueues setup;
    auto& embedded_vm = *setup.embedded_vm;
    auto* vm = embedded_vm.vm();
    auto& tracked_rejections = setup.embedder.tracked_rejections;

    auto* promise = js_promise_create(vm, embedded_vm.realm());
    js_promise_reject(vm, promise, int32_value(7));
    EXPECT_EQ(js_promise_state(promise), JS_PROMISE_STATE_REJECTED);
    EXPECT_EQ(tracked_rejections.size(), 1u);
    EXPECT_EQ(tracked_rejections[0].operation, JS_PROMISE_REJECTION_OPERATION_REJECT);

    // A reaction added to a rejected promise tells the tracker before it marks the promise handled.
    JSValue rejection_reason = js_undefined;
    auto* on_rejected = create_closure(embedded_vm, [&](JSVM* vm) -> JSCompletion {
        rejection_reason = argument(vm, 0);
        return normal_completion(js_undefined);
    });
    EXPECT_EQ(js_promise_perform_then(vm, promise, js_undefined, value_of_object(on_rejected), nullptr), js_undefined);
    EXPECT_EQ(tracked_rejections.size(), 2u);
    EXPECT_EQ(tracked_rejections[1].operation, JS_PROMISE_REJECTION_OPERATION_HANDLE);
    EXPECT(!tracked_rejections[1].promise_was_handled);
    EXPECT(js_promise_is_handled(promise));
    setup.drain_promise_jobs();
    EXPECT_EQ(rejection_reason, int32_value(7));

    // A promise the embedder marks handled itself, as WebIDL does, is not tracked when it is rejected.
    auto* marked = js_promise_create(vm, embedded_vm.realm());
    js_promise_set_is_handled(marked);
    auto resolving_functions = js_promise_create_resolving_functions(vm, marked);
    JSValue reason[] = { int32_value(8) };
    EXPECT_EQ(js_function_call(vm, value_of_object(resolving_functions.reject), js_undefined, reason, 1).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_function_call(vm, value_of_object(resolving_functions.resolve), js_undefined, reason, 1).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_promise_state(marked), JS_PROMISE_STATE_REJECTED);
    EXPECT_EQ(js_promise_result(marked), int32_value(8));
    EXPECT_EQ(tracked_rejections.size(), 2u);
}
