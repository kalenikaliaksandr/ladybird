/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Function.h>
#include <LibGC/Heap.h>

#include "EmbeddingTest.h"

namespace {

constexpr u8 all_attributes = JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE;

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

void define_global(EmbeddedVM& embedded_vm, StringView name, JSValue value)
{
    auto fly_name = Utf16FlyString::from_utf8(name);
    JSPropertyKey key { fly_name.raw_identity() };
    js_object_define_direct_property(embedded_vm.vm(), embedded_vm.global_object(), &key, value, all_attributes);
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

// An embedder that queues promise jobs the way HTML queues microtasks, and finalization registry cleanups the way it
// queues global tasks, and records what its promise rejection tracker sees.
struct EngineTypesEmbedder {
    JSVM* vm { nullptr };
    Vector<GCRoot*> promise_jobs;
    Vector<GCRoot*> finalization_registries_to_clean_up;
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

void enqueue_finalization_registry_cleanup_job(void* data, JSVM*, JSObject* finalization_registry)
{
    embedder_of(data).finalization_registries_to_clean_up.append(gc_root_create(reinterpret_cast<GCCell*>(finalization_registry)));
}

JSVmHostHooks const engine_types_hooks {
    .ensure_can_add_private_element = nullptr,
    .ensure_can_compile_strings = nullptr,
    .get_code_for_eval = nullptr,
    .promise_rejection_tracker = promise_rejection_tracker,
    .call_job_callback = nullptr,
    .enqueue_finalization_registry_cleanup_job = enqueue_finalization_registry_cleanup_job,
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
        for (auto* root : embedder.finalization_registries_to_clean_up)
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

namespace {

// Records the entries it visits, and changes the collection, runs a script and collects garbage on the way.
struct CollectionVisit {
    EmbeddedVM& embedded_vm;
    JSObject* collection;
    Vector<JSValue> keys;
    bool throw_at_added_key { true };
};

JSCompletion visit_map_entry(void* context, JSValue key, JSValue value)
{
    auto& visit = *static_cast<CollectionVisit*>(context);
    visit.keys.append(key);
    if (key == int32_value(0)) {
        js_collections_map_remove(visit.collection, int32_value(1));
        js_collections_map_set(visit.collection, int32_value(9), int32_value(90));
    } else if (key == int32_value(2)) {
        VERIFY(value == int32_value(20));
        auto completion = visit.embedded_vm.evaluate("map.delete(3); map.set('added by script', 1);"sv);
        if (completion.variant != JS_COMPLETION_NORMAL)
            return completion;
        visit.embedded_vm.collect_garbage();
    } else if (key == int32_value(9) && visit.throw_at_added_key) {
        return { int32_value(13), JS_COMPLETION_THROW };
    }
    return normal_completion();
}

JSCompletion visit_set_value(void* context, JSValue value)
{
    auto& visit = *static_cast<CollectionVisit*>(context);
    visit.keys.append(value);
    if (value == int32_value(0)) {
        js_collections_set_remove(visit.collection, int32_value(1));
        js_collections_set_add(visit.collection, int32_value(5));
        auto completion = visit.embedded_vm.evaluate("set.add(6); set.delete(2);"sv);
        if (completion.variant != JS_COMPLETION_NORMAL)
            return completion;
        visit.embedded_vm.collect_garbage();
    }
    return normal_completion();
}

}

TEST_CASE(maps_and_sets_are_iterated_live_while_the_callback_changes_them)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();

    auto* map = js_collections_map_create(vm, embedded_vm->realm());
    define_global(*embedded_vm, "map"sv, value_of_object(map));
    for (i32 key = 0; key < 4; ++key)
        js_collections_map_set(map, int32_value(key), int32_value(key * 10));
    EXPECT_EQ(js_collections_map_size(map), 4u);
    JSValue value = js_undefined;
    EXPECT(js_collections_map_get(map, int32_value(3), &value));
    EXPECT_EQ(value, int32_value(30));
    EXPECT(!js_collections_map_has(map, int32_value(4)));

    // Removed entries are skipped and added ones are visited, and the throw stops the iteration.
    CollectionVisit visit { *embedded_vm, map, {} };
    auto completion = js_collections_map_for_each_entry(map, visit_map_entry, &visit);
    EXPECT_EQ(completion.variant, JS_COMPLETION_THROW);
    EXPECT_EQ(completion.payload, int32_value(13));
    EXPECT_EQ(visit.keys, (Vector<JSValue> { int32_value(0), int32_value(2), int32_value(9) }));

    visit.keys.clear();
    visit.throw_at_added_key = false;
    completion = js_collections_map_for_each_entry(map, visit_map_entry, &visit);
    EXPECT_EQ(completion.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(visit.keys.size(), 4u);
    EXPECT(embedded_vm->run("if ([...map.keys()].join() !== '0,2,9,added by script') throw new Error();"sv));

    auto* entries = js_collections_map_iterator_create(vm, embedded_vm->realm(), map, JS_PROPERTY_KIND_KEY_AND_VALUE);
    define_global(*embedded_vm, "entries"sv, value_of_object(entries));
    js_collections_map_clear(map);
    js_collections_map_set(map, int32_value(1), int32_value(2));
    EXPECT(embedded_vm->run("if (JSON.stringify([...entries]) !== '[[1,2]]') throw new Error();"sv));

    auto* set = js_collections_set_create(vm, embedded_vm->realm());
    define_global(*embedded_vm, "set"sv, value_of_object(set));
    for (i32 element = 0; element < 4; ++element)
        js_collections_set_add(set, int32_value(element));
    CollectionVisit set_visit { *embedded_vm, set, {} };
    completion = js_collections_set_for_each_value(set, visit_set_value, &set_visit);
    EXPECT_EQ(completion.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(set_visit.keys, (Vector<JSValue> { int32_value(0), int32_value(3), int32_value(5), int32_value(6) }));
    EXPECT_EQ(js_collections_set_size(set), 4u);
    EXPECT(js_collections_set_has(set, int32_value(6)));

    auto* values = js_collections_set_iterator_create(vm, embedded_vm->realm(), set, JS_PROPERTY_KIND_VALUE);
    define_global(*embedded_vm, "values"sv, value_of_object(values));
    EXPECT(embedded_vm->run("if ([...values].join() !== '0,3,5,6' || !(set instanceof Set)) throw new Error();"sv));
    js_collections_set_clear(set);
    EXPECT_EQ(js_collections_set_size(set), 0u);
}

TEST_CASE(dates_regexps_and_json_round_trip)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();

    // 2026-10-04T12:34:56.789Z
    auto time_value = js_date_make_date(js_date_make_day(2026, 9, 4), js_date_make_time(12, 34, 56, 789));
    auto* date = js_date_create(vm, embedded_vm->realm(), time_value);
    define_global(*embedded_vm, "date"sv, value_of_object(date));
    EXPECT(embedded_vm->run("if (date.toISOString() !== '2026-10-04T12:34:56.789Z') throw new Error();"sv));
    auto* parsed_date = embedded_vm->object_of("new Date(Date.UTC(1999, 11, 31, 23, 59, 58, 7))"sv);
    auto parsed_time = js_date_date_value(parsed_date);
    EXPECT_EQ(js_date_year_from_time(parsed_time), 1999);
    EXPECT_EQ(js_date_month_from_time(parsed_time), 11);
    EXPECT_EQ(js_date_date_from_time(parsed_time), 31);
    EXPECT_EQ(js_date_hour_from_time(parsed_time), 23);
    EXPECT_EQ(js_date_min_from_time(parsed_time), 59);
    EXPECT_EQ(js_date_sec_from_time(parsed_time), 58);
    EXPECT_EQ(js_date_ms_from_time(parsed_time), 7);

    auto regexp_completion = js_regexp_create(vm, embedded_vm->value_of("'a+(b)'"sv), embedded_vm->value_of("'gi'"sv));
    auto* regexp = pointer_of_payload<JSObject>(regexp_completion);
    EXPECT_EQ(Utf16String::adopt_raw(js_regexp_pattern(regexp)), "a+(b)"sv);
    EXPECT_EQ(Utf16String::adopt_raw(js_regexp_flags(regexp)), "gi"sv);
    define_global(*embedded_vm, "regexp"sv, value_of_object(regexp));
    EXPECT(embedded_vm->run("if (regexp.exec('xAAB')[1] !== 'B' || !(regexp instanceof RegExp)) throw new Error();"sv));
    EXPECT_EQ(js_regexp_create(vm, embedded_vm->value_of("'('"sv), js_undefined).variant, JS_COMPLETION_THROW);

    auto json = R"({"list":[1,"two",{"three":null}],"nested":{"flag":true}})"sv;
    auto parsed = js_json_parse(vm, ascii_view(json));
    EXPECT_EQ(parsed.variant, JS_COMPLETION_NORMAL);
    define_global(*embedded_vm, "parsed"sv, parsed.payload);
    EXPECT(embedded_vm->run("if (parsed.list[2].three !== null || parsed.nested.flag !== true) throw new Error();"sv));

    JSOwnedUtf16String serialized = 0;
    auto stringified = js_json_stringify(vm, parsed.payload, js_undefined, js_undefined, &serialized);
    EXPECT_EQ(stringified.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(stringified.payload, 1u);
    EXPECT_EQ(Utf16String::adopt_raw(serialized), json);

    // The replacer runs C++ that reads the holder while the VM serializes.
    size_t replacer_calls = 0;
    auto* replacer = create_closure(*embedded_vm, [&](JSVM* vm) -> JSCompletion {
        ++replacer_calls;
        auto value = argument(vm, 1);
        embedded_vm->collect_garbage();
        return normal_completion(value == js_true ? int32_value(1) : value);
    });
    stringified = js_json_stringify(vm, embedded_vm->value_of("({ a: true, b: [true] })"sv), value_of_object(replacer), int32_value(1), &serialized);
    EXPECT_EQ(stringified.payload, 1u);
    EXPECT_EQ(Utf16String::adopt_raw(serialized), "{\n \"a\": 1,\n \"b\": [\n  1\n ]\n}"sv);
    EXPECT_EQ(replacer_calls, 4u);

    serialized = 0;
    stringified = js_json_stringify(vm, js_undefined, js_undefined, js_undefined, &serialized);
    EXPECT_EQ(stringified.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(stringified.payload, 0u);
    EXPECT_EQ(serialized, 0u);
    EXPECT_EQ(js_json_parse(vm, ascii_view("{,}"sv)).variant, JS_COMPLETION_THROW);
}

namespace {

struct ValueListSink {
    EmbeddedVM& embedded_vm;
    Vector<JSValue> values;
};

// Takes each value, then runs a script and collects garbage while the VM waits for it.
void append_and_reenter(void* context, JSValue value)
{
    auto& sink = *static_cast<ValueListSink*>(context);
    sink.values.append(value);
    VERIFY(sink.embedded_vm.run("log.push('appended');"sv));
    sink.embedded_vm.collect_garbage();
}

}

TEST_CASE(iterators_close_when_the_embedder_stops_on_a_throw)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    EXPECT(embedded_vm->run(R"~~~(
        var log = [];
        function* generator() {
            try {
                yield 1;
                yield 2;
                yield 3;
            } finally {
                log.push("closed");
            }
        }
    )~~~"sv));

    auto* record = pointer_of_payload<JSIteratorRecord>(js_iterator_get(vm, embedded_vm->value_of("generator()"sv), JS_ITERATOR_HINT_SYNC));
    EXPECT(!js_iterator_record_done(record));
    JSValue value = js_undefined;
    auto step = js_iterator_step_value(vm, record, &value);
    EXPECT_EQ(step.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(step.payload, 1u);
    EXPECT_EQ(value, int32_value(1));

    // Converting the value failed, so the embedder closes the iterator with the throw, as TRY_OR_CLOSE_ITERATOR does.
    auto conversion_error = embedded_vm->evaluate("throw new TypeError('not convertible');"sv);
    EXPECT_EQ(conversion_error.variant, JS_COMPLETION_THROW);
    auto closed = js_iterator_close(vm, record, conversion_error);
    EXPECT_EQ(closed.variant, JS_COMPLETION_THROW);
    EXPECT_EQ(closed.payload, conversion_error.payload);
    EXPECT(embedded_vm->run("if (log.join() !== 'closed') throw new Error(log.join());"sv));
    step = js_iterator_step_value(vm, record, &value);
    EXPECT_EQ(step.payload, 0u);
    EXPECT(js_iterator_record_done(record));

    // IteratorToList steps to the end before the sink sees any value.
    auto* method = embedded_vm->object_of("generator"sv);
    record = pointer_of_payload<JSIteratorRecord>(js_iterator_get_from_method(vm, js_undefined, method));
    ValueListSink sink { *embedded_vm, {} };
    JSValueSink value_sink { &sink, append_and_reenter };
    EXPECT_EQ(js_iterator_to_list(vm, record, &value_sink).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(sink.values, (Vector<JSValue> { int32_value(1), int32_value(2), int32_value(3) }));
    EXPECT(embedded_vm->run("if (log.join() !== 'closed,closed,appended,appended,appended') throw new Error(log.join());"sv));

    // IteratorNext, IteratorComplete and IteratorValue on a result object the embedder created.
    record = pointer_of_payload<JSIteratorRecord>(js_iterator_get(vm, embedded_vm->value_of("['only']"sv), JS_ITERATOR_HINT_SYNC));
    auto* result = pointer_of_payload<JSObject>(js_iterator_next(vm, record, nullptr));
    EXPECT_EQ(js_iterator_complete(vm, result).payload, 0u);
    auto only = js_iterator_value(vm, result);
    define_global(*embedded_vm, "only"sv, only.payload);
    EXPECT(embedded_vm->run("if (only !== 'only') throw new Error();"sv));
    auto* done = js_iterator_create_result_object(vm, embedded_vm->realm(), js_undefined, true);
    EXPECT_EQ(js_iterator_complete(vm, done).payload, 1u);
    EXPECT_EQ(js_iterator_value(vm, done).payload, js_undefined);
}

TEST_CASE(array_likes_report_the_length_their_getter_returns)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* vm = embedded_vm->vm();
    u64 length = 0;
    EXPECT_EQ(js_array_length_of_array_like(vm, embedded_vm->object_of("({ get length() { return '2.5'; } })"sv), &length).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(length, 2u);
    auto thrown = js_array_length_of_array_like(vm, embedded_vm->object_of("({ get length() { throw 'no length'; } })"sv), &length);
    EXPECT_EQ(thrown.variant, JS_COMPLETION_THROW);
    EXPECT_EQ(length, 2u);
}

TEST_CASE(the_embedder_runs_the_cleanup_of_a_finalization_registry)
{
    EmbeddedVMWithQueues setup;
    auto& embedded_vm = *setup.embedded_vm;
    auto* vm = embedded_vm.vm();
    EXPECT(embedded_vm.run(R"~~~(
        globalThis.held = [];
        globalThis.registry = new FinalizationRegistry(value => held.push(value));
        (() => {
            for (let i = 0; i < 100; ++i)
                registry.register({}, i);
        })();
    )~~~"sv));
    embedded_vm.collect_garbage();
    EXPECT(!setup.embedder.finalization_registries_to_clean_up.is_empty());
    if (setup.embedder.finalization_registries_to_clean_up.is_empty())
        return;

    // The hook only queued the cleanup, which runs when the embedder gets to it.
    EXPECT(embedded_vm.run("if (held.length !== 0) throw new Error();"sv));
    auto* registry_root = setup.embedder.finalization_registries_to_clean_up.take_first();
    auto* registry = reinterpret_cast<JSObject*>(gc_root_cell(registry_root));
    EXPECT_EQ(registry, embedded_vm.object_of("registry"sv));
    EXPECT_EQ(js_weak_finalization_registry_realm(registry), embedded_vm.realm());
    auto* cleanup_callback = js_realm_job_callback_callback(js_weak_finalization_registry_cleanup_callback(registry));
    EXPECT(js_value_is_function(value_of_object(cleanup_callback)));

    EXPECT_EQ(js_weak_finalization_registry_cleanup(vm, registry, nullptr).variant, JS_COMPLETION_NORMAL);
    gc_root_destroy(registry_root);
    EXPECT(embedded_vm.run("if (held.length === 0 || new Set(held).size !== held.length) throw new Error();"sv));
}
