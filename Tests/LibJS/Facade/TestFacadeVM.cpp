/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Function.h>
#include <LibGC/Root.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>
#include <LibJS/Heap/Cell.h>
#include <LibJS/Runtime/Agent.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/ErrorTypes.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/GlobalObject.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/JobCallback.h>
#include <LibJS/Runtime/ModuleRequest.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/Promise.h>
#include <LibJS/Runtime/PromiseJob.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Symbol.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// The VM, its realms and execution contexts, and the hooks through which the host takes part, as LibJS's users use
// them. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

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

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

// One of the embedder's cells, which a realm initializes when it creates it.
class RealmInitializedCell final : public Cell {
    GC_CELL(RealmInitializedCell, Cell);
    GC_DECLARE_ALLOCATOR(RealmInitializedCell);

public:
    GC::Ptr<Realm> initialized_in;

    virtual void initialize(Realm& realm) override
    {
        Base::initialize(realm);
        initialized_in = &realm;
    }

private:
    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(initialized_in);
    }
};

GC_DEFINE_ALLOCATOR(RealmInitializedCell);

// One of the embedder's cells that owns an execution context, which keeps what the context holds alive by visiting it.
class ExecutionContextOwner final : public GC::Cell {
    GC_CELL(ExecutionContextOwner, GC::Cell);
    GC_DECLARE_ALLOCATOR(ExecutionContextOwner);

public:
    explicit ExecutionContextOwner(NonnullOwnPtr<ExecutionContext> execution_context)
        : m_execution_context(move(execution_context))
    {
    }

private:
    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        m_execution_context->visit_edges(visitor);
    }

    NonnullOwnPtr<ExecutionContext> m_execution_context;
};

GC_DEFINE_ALLOCATOR(ExecutionContextOwner);

// A host's agent, whose event loop runs the promise jobs that the VM hands to the host.
class TestAgent final : public Agent {
public:
    AK_ALLOC_WITH_KMALLOC;

    TestAgent(CanBlock can_block, Vector<GC::Root<GC::Function<void()>>>& queued_promise_jobs)
        : Agent(can_block)
        , m_queued_promise_jobs(queued_promise_jobs)
    {
    }

    virtual void spin_event_loop_until(GC::Root<GC::Function<bool()>> goal_condition) override
    {
        ++spin_count;
        // The C++ runtime's goal condition holds a copy of the outcome that it takes before the promise settles, so
        // the jobs run until none are left rather than until the goal is met.
        while (!goal_condition->function()() && !m_queued_promise_jobs.is_empty()) {
            auto job = m_queued_promise_jobs.take_first();
            job->function()();
        }
    }

    size_t spin_count { 0 };

private:
    Vector<GC::Root<GC::Function<void()>>>& m_queued_promise_jobs;
};

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static void const* address_of(Object const& object)
{
    return &object;
}

static void const* address_of(Value value)
{
    return &value.as_object();
}

// Has the promise jobs that the VM hands to the host queued in `queued_promise_jobs`, which keeps them alive.
static void queue_promise_jobs_in(VM& vm, Vector<GC::Root<GC::Function<void()>>>& queued_promise_jobs)
{
    vm.host_enqueue_promise_job = [&vm, &queued_promise_jobs](PromiseJob job, GC::Ptr<Realm>) {
        queued_promise_jobs.append(GC::make_root(GC::create_function(vm.heap(), [job = move(job)] {
            MUST(job.run());
        })));
    };
    vm.host_promise_job_queue_is_empty = [&queued_promise_jobs] { return queued_promise_jobs.is_empty(); };
}

static void run_queued_promise_jobs(Vector<GC::Root<GC::Function<void()>>>& queued_promise_jobs)
{
    while (!queued_promise_jobs.is_empty()) {
        auto job = queued_promise_jobs.take_first();
        job->function()();
    }
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

TEST_CASE(the_vm_and_what_it_holds)
{
    auto vm = VM::create();
    EXPECT_EQ(&VM::the(), vm.ptr());
    EXPECT_EQ(&vm->heap(), &GC::Heap::the());

    {
        NonnullRefPtr<VM> another_reference = vm;
        EXPECT_EQ(another_reference.ptr(), vm.ptr());
    }
    EXPECT_EQ(&VM::the(), vm.ptr());

    EXPECT(!vm->has_running_execution_context());
    EXPECT(vm->execution_context_stack().is_empty());
    EXPECT_EQ(vm->execution_context_stack().size(), 0u);
    EXPECT(!vm->did_reach_stack_space_limit());

    EXPECT_EQ(vm->error_message(VM::ErrorMessage::OutOfMemory), "Out of memory"sv);

    EXPECT_EQ(vm->well_known_symbol_iterator()->descriptive_string(), "Symbol(Symbol.iterator)"sv);
    EXPECT_EQ(vm->well_known_symbol_to_string_tag()->descriptive_string(), "Symbol(Symbol.toStringTag)"sv);
    EXPECT_EQ(vm->well_known_symbol_async_dispose()->descriptive_string(), "Symbol(Symbol.asyncDispose)"sv);
    EXPECT_EQ(vm->well_known_symbol_unscopables()->descriptive_string(), "Symbol(Symbol.unscopables)"sv);
    EXPECT_EQ(vm->well_known_symbol_iterator().ptr(), vm->well_known_symbol_iterator().ptr());
    EXPECT_NE(vm->well_known_symbol_iterator().ptr(), vm->well_known_symbol_async_iterator().ptr());

    EXPECT_EQ(vm->empty_string().length_in_utf16_code_units(), 0u);
    EXPECT_EQ(&vm->empty_string(), &vm->empty_string());

    EXPECT(vm->names.length.is_string());
    EXPECT_EQ(vm->names.length, PropertyKey { "length"_utf16_fly_string });

    EXPECT(!vm->debugging_enabled());
    vm->enable_debugging();
    EXPECT(vm->debugging_enabled());
    vm->disable_debugging();
    EXPECT(!vm->debugging_enabled());

    EXPECT(!vm->agent());
}

TEST_CASE(realm_with_an_ordinary_global_object)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& execution_context = *vm_with_realm.realm_execution_context;
    auto& realm = vm_with_realm.realm();

    // InitializeHostDefinedRealm leaves the realm's execution context running.
    EXPECT_EQ(&vm.running_execution_context(), &execution_context);
    EXPECT_EQ(vm.execution_context_stack().size(), 1u);
    EXPECT_EQ(vm.current_realm(), &realm);
    EXPECT_EQ(&vm.realm(), &realm);
    EXPECT(!execution_context.function);
    EXPECT(!vm.active_function_object());
    EXPECT(execution_context.script_or_module.has<Empty>());

    Value global_object = &realm.global_object();
    EXPECT(is<GlobalObject>(global_object.as_object()));
    EXPECT(same_value(global_object, MUST(evaluate(vm, realm, "globalThis"sv))));
    EXPECT(same_value(global_object, MUST(evaluate(vm, realm, "this"sv))));
    EXPECT(&realm.global_environment());

    auto& intrinsics = realm.intrinsics();
    EXPECT(same_value(intrinsics.object_prototype(), MUST(evaluate(vm, realm, "Object.prototype"sv))));
    EXPECT(same_value(intrinsics.function_prototype(), MUST(evaluate(vm, realm, "Function.prototype"sv))));
    EXPECT(same_value(intrinsics.array_prototype(), MUST(evaluate(vm, realm, "Array.prototype"sv))));
    EXPECT(same_value(intrinsics.string_prototype(), MUST(evaluate(vm, realm, "String.prototype"sv))));
    EXPECT(same_value(intrinsics.error_prototype(), MUST(evaluate(vm, realm, "Error.prototype"sv))));
    EXPECT(same_value(intrinsics.iterator_prototype(), MUST(evaluate(vm, realm, "Iterator.prototype"sv))));
    EXPECT(same_value(intrinsics.async_iterator_prototype(), MUST(evaluate(vm, realm, "Object.getPrototypeOf(Object.getPrototypeOf(Object.getPrototypeOf((async function* () {})())))"sv))));
    EXPECT(same_value(intrinsics.json_stringify_function(), MUST(evaluate(vm, realm, "JSON.stringify"sv))));

    // The constructors are of types that LibJS's users name without needing to know them.
    EXPECT_EQ(static_cast<void const*>(intrinsics.error_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Error"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.promise_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Promise"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.shared_array_buffer_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "SharedArrayBuffer"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.data_view_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "DataView"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.uint8_array_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Uint8Array"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.uint8_clamped_array_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Uint8ClampedArray"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.big_int64_array_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "BigInt64Array"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.float16_array_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Float16Array"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.float64_array_constructor().ptr()), address_of(MUST(evaluate(vm, realm, "Float64Array"sv))));
    EXPECT_EQ(static_cast<void const*>(intrinsics.console_object().ptr()), address_of(MUST(evaluate(vm, realm, "console"sv))));

    // The realm holds one cell of the embedder, and creates the embedder's cells initialized in itself.
    EXPECT(!realm.host_defined());
    auto host_defined = realm.create<RealmInitializedCell>();
    EXPECT_EQ(host_defined->initialized_in.ptr(), &realm);
    realm.set_host_defined(host_defined.ptr());
    EXPECT_EQ(realm.host_defined().ptr(), host_defined.ptr());

    GC::Weak<RealmInitializedCell> weak_host_defined { host_defined };
    collect_garbage(vm);
    EXPECT(weak_host_defined);
    EXPECT_EQ(realm.host_defined().ptr(), weak_host_defined.ptr().ptr());

    realm.set_host_defined(nullptr);
    EXPECT(!realm.host_defined());
}

TEST_CASE(realm_with_a_global_this_value_of_the_host)
{
    auto vm = VM::create();

    size_t creation_count = 0;
    GC::Ptr<Realm> realm_being_created;
    auto execution_context = MUST(Realm::initialize_host_defined_realm(*vm, nullptr, [&](Realm& realm) -> GC::Ref<Object> {
        ++creation_count;
        realm_being_created = &realm;
        EXPECT_EQ(vm->current_realm(), &realm);
        return realm.intrinsics().array_prototype();
    }));
    auto& realm = *execution_context->realm;

    EXPECT_EQ(creation_count, 1u);
    EXPECT_EQ(realm_being_created.ptr(), &realm);
    EXPECT(same_value(MUST(evaluate(*vm, realm, "this"sv)), realm.intrinsics().array_prototype()));
    EXPECT(MUST(evaluate(*vm, realm, "this === Array.prototype"sv)).as_bool());

    EXPECT_EQ(vm->pop_execution_context(), execution_context.ptr());
}

TEST_CASE(scripts)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto source_text = "6 * 7"_utf16;
    auto host_defined = realm.create<RealmInitializedCell>();
    auto script = Script::parse(source_text.utf16_view(), realm, "answer.js"sv, {}, host_defined.ptr());
    EXPECT(!script.is_error());
    EXPECT_EQ(&script.value()->realm(), &realm);
    EXPECT_EQ(script.value()->host_defined().ptr(), host_defined.ptr());
    EXPECT_EQ(MUST(vm.run(script.value())).as_i32(), 42);

    auto thrown = evaluate(vm, realm, "throw 5"sv);
    EXPECT(thrown.is_error());
    EXPECT_EQ(thrown.error_value().as_i32(), 5);

    auto broken_source_text = "let x = ;"_utf16;
    auto broken_script = Script::parse(broken_source_text.utf16_view(), realm);
    EXPECT(broken_script.is_error());
    EXPECT_EQ(broken_script.error().size(), 1u);
    auto const& parser_error = broken_script.error().first();
    EXPECT(!parser_error.message.is_empty());
    EXPECT(parser_error.position.has_value());
    EXPECT_EQ(parser_error.position->line, 1u);
    EXPECT_EQ(parser_error.position->column, 9u);

    // A script names itself as the active script while it is the ScriptOrModule of a context on the stack.
    EXPECT(vm.get_active_script_or_module().has<Empty>());
    vm_with_realm.realm_execution_context->script_or_module = script.value();
    auto active = vm.get_active_script_or_module();
    EXPECT(active.has<GC::Ref<Script>>());
    EXPECT_EQ(active.get<GC::Ref<Script>>().ptr(), script.value().ptr());
    auto visited = active.visit(
        [](GC::Ref<Script>& active_script) { return active_script->host_defined(); },
        [](GC::Ref<Module>&) -> GC::Ptr<GC::Cell> { return nullptr; },
        [](Empty) -> GC::Ptr<GC::Cell> { return nullptr; });
    EXPECT_EQ(visited.ptr(), host_defined.ptr());
    vm_with_realm.realm_execution_context->script_or_module = Empty {};
    EXPECT(vm.get_active_script_or_module().has<Empty>());
}

TEST_CASE(throw_completion_formats_the_message_of_the_error)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto expect_thrown = [&](Completion completion, StringView constructor_name, Utf16View message) {
        EXPECT(completion.is_error());
        auto error = completion.value();
        EXPECT(error.is_object());
        EXPECT(same_value(property_of(vm, error, "constructor"sv), MUST(evaluate(vm, realm, constructor_name))));
        EXPECT_EQ(property_of(vm, error, "message"sv).as_string().utf16_string(), message);
    };

    expect_thrown(vm.throw_completion<TypeError>(ErrorType::NotAnObject, "Banana"sv), "TypeError"sv, "Banana is not an object"sv);
    expect_thrown(vm.throw_completion<RangeError>(ErrorType::InvalidLength, "array"sv), "RangeError"sv, "Invalid array length"sv);
    expect_thrown(vm.throw_completion<TypeError>(ErrorType::DetachedArrayBuffer), "TypeError"sv, "ArrayBuffer is detached"sv);
    expect_thrown(vm.throw_completion<SyntaxError>("A plain message"sv), "SyntaxError"sv, "A plain message"sv);
    expect_thrown(vm.throw_completion<InternalError>(vm.error_message(VM::ErrorMessage::OutOfMemory)), "InternalError"sv, "Out of memory"sv);
    expect_thrown(vm.throw_completion<EvalError>("eval"sv), "EvalError"sv, "eval"sv);
    expect_thrown(vm.throw_completion<ReferenceError>("reference"sv), "ReferenceError"sv, "reference"sv);
    expect_thrown(vm.throw_completion<URIError>("uri"sv), "URIError"sv, "uri"sv);
    expect_thrown(vm.throw_completion<JS::Error>("error"sv), "Error"sv, "error"sv);

    auto non_ascii_message = Utf16String::from_utf8("Ünïcödé \U0001F600"sv);
    expect_thrown(vm.throw_completion<TypeError>(non_ascii_message), "TypeError"sv, non_ascii_message.utf16_view());
    auto formatted_non_ascii_message = Utf16String::formatted("{} is not a function", non_ascii_message);
    expect_thrown(vm.throw_completion<TypeError>(ErrorType::NotAFunction, non_ascii_message), "TypeError"sv, formatted_non_ascii_message.utf16_view());

    ThrowCompletionOr<Value> as_throw_completion_or_value = vm.throw_completion<TypeError>("returned"sv);
    EXPECT(as_throw_completion_or_value.is_throw_completion());
    ThrowCompletionOr<void> as_throw_completion_or_void = vm.throw_completion<TypeError>("returned"sv);
    EXPECT(as_throw_completion_or_void.is_throw_completion());
}

TEST_CASE(type_error_realm_scope)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto other_realm_execution_context = MUST(Realm::initialize_host_defined_realm(vm, nullptr, nullptr));
    auto& other_realm = *other_realm_execution_context->realm;
    EXPECT_EQ(vm.pop_execution_context(), other_realm_execution_context.ptr());
    // Nothing on the stack holds the other realm now, so the test does.
    auto other_realm_root = GC::make_root(other_realm);

    vm.push_execution_context(*other_realm_execution_context);
    auto other_type_error = MUST(evaluate(vm, other_realm, "TypeError"sv));
    auto other_range_error = MUST(evaluate(vm, other_realm, "RangeError"sv));
    vm.pop_execution_context();
    auto type_error = MUST(evaluate(vm, realm, "TypeError"sv));
    auto range_error = MUST(evaluate(vm, realm, "RangeError"sv));
    EXPECT(!same_value(type_error, other_type_error));

    auto constructor_of_thrown = [&](Completion completion) {
        EXPECT(completion.is_error());
        return property_of(vm, completion.value(), "constructor"sv);
    };

    {
        VM::TypeErrorRealmScope scope { vm, other_realm };
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("in scope"sv)), other_type_error));
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>(ErrorType::NotAnObject, "x"sv)), other_type_error));
        // Only TypeErrors are redirected.
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<RangeError>("in scope"sv)), range_error));

        // A callee's execution context is unaffected.
        auto callee_execution_context = ExecutionContext::create(0, ReadonlySpan<Value> {}, 0);
        callee_execution_context->realm = &realm;
        vm.push_execution_context(*callee_execution_context);
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("in callee"sv)), type_error));
        vm.pop_execution_context();
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("back in scope"sv)), other_type_error));

        {
            VM::TypeErrorRealmScope nested_scope { vm, realm };
            EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("nested"sv)), type_error));
        }
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("after nested"sv)), other_type_error));

        scope.restore();
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("restored"sv)), type_error));
        scope.restore();
    }

    {
        VM::TypeErrorRealmScope scope { vm, other_realm };
    }
    EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("after scope"sv)), type_error));
    EXPECT(!same_value(other_range_error, range_error));
}

TEST_CASE(execution_context_stack)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();
    auto& realm_execution_context = *vm_with_realm.realm_execution_context;

    auto caller_context = ExecutionContext::create(0, ReadonlySpan<Value> {}, 2);
    caller_context->realm = &realm;
    caller_context->arguments_span()[0] = Value(1);
    caller_context->arguments_span()[1] = Value(2);
    caller_context->this_value = Value(42);
    caller_context->skip_when_determining_incumbent_counter = 3;
    EXPECT_EQ(caller_context->argument_count, 2u);
    EXPECT_EQ(caller_context->arguments_span().size(), 2u);
    EXPECT_EQ(caller_context->argument(1).as_i32(), 2);
    EXPECT(caller_context->argument(2).is_undefined());

    vm.push_execution_context(*caller_context);
    EXPECT_EQ(&vm.running_execution_context(), caller_context.ptr());
    EXPECT_EQ(vm.execution_context_stack().size(), 2u);
    EXPECT_EQ(vm.argument_count(), 2u);
    EXPECT_EQ(vm.argument(0).as_i32(), 1);
    EXPECT_EQ(vm.argument(1).as_i32(), 2);
    EXPECT(vm.argument(2).is_undefined());
    EXPECT_EQ(vm.this_value().as_i32(), 42);
    EXPECT_EQ(vm.current_realm(), &realm);
    EXPECT(!vm.active_function_object());
    EXPECT_EQ(vm.running_execution_context().skip_when_determining_incumbent_counter, 3u);

    auto copy = caller_context->copy();
    EXPECT_EQ(copy->realm.ptr(), &realm);
    EXPECT_EQ(copy->argument_count, 2u);
    EXPECT_EQ(copy->argument(1).as_i32(), 2);
    EXPECT_EQ(copy->this_value.value().as_i32(), 42);

    auto source_text = "1"_utf16;
    auto script = MUST(Script::parse(source_text.utf16_view(), realm));

    auto& interpreter_stack = vm.interpreter_stack();
    auto* interpreter_stack_mark = interpreter_stack.top();
    auto* callee_context = interpreter_stack.allocate(0, ReadonlySpan<Value> {}, 1);
    VERIFY(callee_context);
    EXPECT_NE(interpreter_stack.top(), interpreter_stack_mark);
    callee_context->realm = &realm;
    callee_context->arguments_span()[0] = Value(7);
    callee_context->script_or_module = script;
    MUST(vm.push_execution_context(*callee_context, VM::CheckStackSpaceLimitTag {}));
    EXPECT_EQ(vm.argument_count(), 1u);
    EXPECT_EQ(vm.argument(0).as_i32(), 7);
    EXPECT_EQ(vm.execution_context_stack().size(), 3u);
    EXPECT_EQ(vm.get_active_script_or_module().get<GC::Ref<Script>>().ptr(), script.ptr());

    Vector<ExecutionContext*> walked;
    vm.for_each_execution_context_top_to_bottom([&](ExecutionContext& execution_context) {
        walked.append(&execution_context);
        return true;
    });
    EXPECT_EQ(walked, (Vector<ExecutionContext*> { callee_context, caller_context.ptr(), &realm_execution_context }));

    walked.clear();
    vm.for_each_execution_context_top_to_bottom([&](ExecutionContext& execution_context) {
        walked.append(&execution_context);
        return execution_context.script_or_module.has<Empty>();
    });
    EXPECT_EQ(walked, (Vector<ExecutionContext*> { callee_context }));

    auto counted = vm.last_execution_context_matching([](ExecutionContext* execution_context) {
        return execution_context->skip_when_determining_incumbent_counter == 3;
    });
    EXPECT(counted.has_value());
    EXPECT_EQ(counted.value(), caller_context.ptr());
    auto with_script = vm.last_execution_context_matching([](ExecutionContext* execution_context) {
        return !execution_context->script_or_module.has<Empty>();
    });
    EXPECT_EQ(with_script.value(), callee_context);
    EXPECT(!vm.last_execution_context_matching([](ExecutionContext*) { return false; }).has_value());

    EXPECT_EQ(vm.pop_execution_context(), callee_context);
    interpreter_stack.deallocate(interpreter_stack_mark);
    EXPECT_EQ(interpreter_stack.top(), interpreter_stack_mark);
    EXPECT_EQ(vm.pop_execution_context(), caller_context.ptr());
    EXPECT_EQ(&vm.running_execution_context(), &realm_execution_context);

    // An event loop sets the stack aside while it runs a task, and brings it back afterwards.
    vm.save_execution_context_stack();
    EXPECT(!vm.has_running_execution_context());
    EXPECT(vm.execution_context_stack().is_empty());
    vm.push_execution_context(*caller_context);
    EXPECT_EQ(vm.execution_context_stack().size(), 1u);
    vm.clear_execution_context_stack();
    EXPECT(!vm.has_running_execution_context());
    vm.restore_execution_context_stack();
    EXPECT_EQ(&vm.running_execution_context(), &realm_execution_context);
    EXPECT_EQ(vm.execution_context_stack().size(), 1u);
}

TEST_CASE(execution_contexts_that_the_embedder_visits)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;

    auto other_realm_execution_context = MUST(Realm::initialize_host_defined_realm(vm, nullptr, nullptr));
    vm.pop_execution_context();
    GC::Weak<Realm> other_realm { other_realm_execution_context->realm };

    auto owner = GC::make_root(vm.heap().allocate<ExecutionContextOwner>(move(other_realm_execution_context)));
    collect_garbage(vm);
    EXPECT(other_realm);
    EXPECT(&other_realm->global_object());
}

TEST_CASE(host_hooks_for_promises)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
    queue_promise_jobs_in(vm, queued_promise_jobs);

    Vector<void const*> realms_of_queued_jobs;
    auto queue_job = move(vm.host_enqueue_promise_job);
    vm.host_enqueue_promise_job = [&](PromiseJob job, GC::Ptr<Realm> job_realm) {
        realms_of_queued_jobs.append(job_realm.ptr());
        queue_job(move(job), job_realm);
    };

    struct TrackedRejection {
        void const* promise;
        Promise::RejectionOperation operation;
    };
    Vector<TrackedRejection> tracked_rejections;
    vm.host_promise_rejection_tracker = [&](Promise& promise, Promise::RejectionOperation operation) {
        tracked_rejections.append({ address_of(promise), operation });
    };

    auto rejected = MUST(evaluate(vm, realm, "globalThis.rejected = Promise.reject(1)"sv));
    EXPECT(is<Promise>(rejected.as_object()));
    EXPECT_EQ(tracked_rejections.size(), 1u);
    EXPECT_EQ(tracked_rejections[0].promise, address_of(rejected));
    EXPECT_EQ(tracked_rejections[0].operation, Promise::RejectionOperation::Reject);
    EXPECT(queued_promise_jobs.is_empty());

    MUST(evaluate(vm, realm, "rejected.catch(() => {}); undefined"sv));
    EXPECT_EQ(tracked_rejections.size(), 2u);
    EXPECT_EQ(tracked_rejections[1].promise, address_of(rejected));
    EXPECT_EQ(tracked_rejections[1].operation, Promise::RejectionOperation::Handle);
    EXPECT_EQ(queued_promise_jobs.size(), 1u);
    run_queued_promise_jobs(queued_promise_jobs);

    auto custom_data = realm.create<RealmInitializedCell>();
    size_t made_job_callbacks = 0;
    vm.host_make_job_callback = [&](FunctionObject& callable) {
        ++made_job_callbacks;
        return JobCallback::create(vm, callable, custom_data.ptr());
    };
    size_t called_job_callbacks = 0;
    vm.host_call_job_callback = [&, call = move(vm.host_call_job_callback)](JobCallback& job_callback, Value this_value, ReadonlySpan<Value> arguments) {
        ++called_job_callbacks;
        EXPECT_EQ(job_callback.custom_data().ptr(), custom_data.ptr());
        EXPECT_EQ(arguments.size(), 1u);
        return call(job_callback, this_value, arguments);
    };

    realms_of_queued_jobs.clear();
    MUST(evaluate(vm, realm, "globalThis.result = 0; Promise.resolve(5).then(value => { result = value * 2; }); undefined"sv));
    EXPECT_EQ(made_job_callbacks, 1u);
    EXPECT_EQ(called_job_callbacks, 0u);
    EXPECT_EQ(queued_promise_jobs.size(), 1u);
    EXPECT_EQ(realms_of_queued_jobs, (Vector<void const*> { &realm }));
    collect_garbage(vm);
    run_queued_promise_jobs(queued_promise_jobs);
    EXPECT_EQ(called_job_callbacks, 1u);
    EXPECT_EQ(MUST(evaluate(vm, realm, "result"sv)).as_i32(), 10);

    auto function = MUST(evaluate(vm, realm, "(function () {})"sv));
    auto job_callback_without_custom_data = JobCallback::create(vm, function.as_function(), nullptr);
    EXPECT(!job_callback_without_custom_data->custom_data());
    EXPECT_EQ(address_of(job_callback_without_custom_data->callback()), address_of(function));
}

TEST_CASE(host_hooks_for_dynamic_code)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    struct StringCompilation {
        Realm* callee_realm;
        Vector<Utf16String> parameter_strings;
        Utf16String body_string;
        Utf16String code_string;
        CompilationType compilation_type;
        size_t parameter_arg_count;
    };
    Vector<StringCompilation> compilations;
    vm.host_ensure_can_compile_strings = [&](Realm& callee_realm, ReadonlySpan<Utf16String> parameter_strings, Utf16View body_string, Utf16View code_string, CompilationType compilation_type, ReadonlySpan<Value> parameter_args, Value) -> ThrowCompletionOr<void> {
        compilations.append({ &callee_realm, Vector<Utf16String> { parameter_strings }, Utf16String::from_utf16(body_string), Utf16String::from_utf16(code_string), compilation_type, parameter_args.size() });
        return {};
    };

    EXPECT_EQ(MUST(evaluate(vm, realm, "eval('1 + 1')"sv)).as_i32(), 2);
    EXPECT_EQ(MUST(evaluate(vm, realm, "(0, eval)('2 + 2')"sv)).as_i32(), 4);
    EXPECT_EQ(MUST(evaluate(vm, realm, "new Function('a', 'return a')(7)"sv)).as_i32(), 7);
    EXPECT_EQ(compilations.size(), 3u);

    EXPECT_EQ(compilations[0].callee_realm, &realm);
    EXPECT_EQ(compilations[0].compilation_type, CompilationType::DirectEval);
    EXPECT(compilations[0].parameter_strings.is_empty());
    EXPECT_EQ(compilations[0].body_string, "1 + 1"sv);
    EXPECT_EQ(compilations[0].code_string, "1 + 1"sv);

    EXPECT_EQ(compilations[1].compilation_type, CompilationType::IndirectEval);
    EXPECT_EQ(compilations[1].code_string, "2 + 2"sv);

    EXPECT_EQ(compilations[2].compilation_type, CompilationType::Function);
    EXPECT_EQ(compilations[2].parameter_strings, (Vector<Utf16String> { "a"_utf16 }));
    EXPECT_EQ(compilations[2].body_string, "return a"sv);
    EXPECT_EQ(compilations[2].parameter_arg_count, 1u);

    vm.host_ensure_can_compile_strings = [&](Realm&, ReadonlySpan<Utf16String>, Utf16View, Utf16View, CompilationType, ReadonlySpan<Value>, Value) -> ThrowCompletionOr<void> {
        return vm.throw_completion<EvalError>("blocked"sv);
    };
    EXPECT(MUST(evaluate(vm, realm, "try { eval('1'); false } catch (e) { e instanceof EvalError && e.message === 'blocked' }"sv)).as_bool());

    // An object that the host has code for evaluates to that code.
    EXPECT(MUST(evaluate(vm, realm, "typeof eval({})"sv)).is_string());
    vm.host_ensure_can_compile_strings = [](Realm&, ReadonlySpan<Utf16String>, Utf16View, Utf16View, CompilationType, ReadonlySpan<Value>, Value) -> ThrowCompletionOr<void> {
        return {};
    };
    Vector<void const*> objects_asked_for_code;
    vm.host_get_code_for_eval = [&](Object const& argument) -> GC::Ptr<PrimitiveString> {
        objects_asked_for_code.append(address_of(argument));
        return PrimitiveString::create(vm, "40 + 2"_utf16);
    };
    EXPECT_EQ(MUST(evaluate(vm, realm, "globalThis.code = {}; eval(code)"sv)).as_i32(), 42);
    EXPECT_EQ(objects_asked_for_code, (Vector<void const*> { address_of(MUST(evaluate(vm, realm, "code"sv))) }));
}

TEST_CASE(host_hooks_for_objects_and_buffers)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    // A block, so that the classes can be declared again.
    auto add_private_element = "{"
                               "class Base { constructor(o) { return o; } }"
                               "class Stamper extends Base { #stamp = 1; }"
                               "try { new Stamper(globalThis.target = {}); 'added' } catch (e) { e.message }"
                               "}"sv;
    EXPECT_EQ(MUST(evaluate(vm, realm, add_private_element)).as_string().utf16_string(), "added"sv);

    Vector<void const*> objects_refused;
    vm.host_ensure_can_add_private_element = [&](Object& object) -> ThrowCompletionOr<void> {
        objects_refused.append(address_of(object));
        return vm.throw_completion<TypeError>("refused"sv);
    };
    EXPECT_EQ(MUST(evaluate(vm, realm, add_private_element)).as_string().utf16_string(), "refused"sv);
    EXPECT_EQ(objects_refused, (Vector<void const*> { address_of(MUST(evaluate(vm, realm, "target"sv))) }));

    Vector<Utf16String> unrecognized_date_strings;
    vm.host_unrecognized_date_string = [&](Utf16View date_string) {
        unrecognized_date_strings.append(Utf16String::from_utf16(date_string));
    };
    EXPECT(MUST(evaluate(vm, realm, "new Date('definitely not a date').getTime()"sv)).is_nan());
#ifdef LIBJS_TESTS_RUN_ON_THE_RUST_RUNTIME
    EXPECT_EQ(unrecognized_date_strings, (Vector<Utf16String> { "definitely not a date"_utf16 }));
#else
    // The C++ runtime compares the time it parsed with NAN, which no double equals, so it never tells the host.
    EXPECT(unrecognized_date_strings.is_empty());
#endif

    struct BufferResize {
        void const* buffer;
        size_t new_byte_length;
    };
    Vector<BufferResize> resizes;
    vm.host_resize_array_buffer = [&, resize = move(vm.host_resize_array_buffer)](ArrayBuffer& buffer, size_t new_byte_length) -> ThrowCompletionOr<HandledByHost> {
        resizes.append({ &buffer, new_byte_length });
        if (new_byte_length == 4)
            return vm.throw_completion<RangeError>("too small"sv);
        return resize(buffer, new_byte_length);
    };
    EXPECT_EQ(MUST(evaluate(vm, realm, "globalThis.buffer = new ArrayBuffer(8, { maxByteLength: 16 }); buffer.resize(12); buffer.byteLength"sv)).as_i32(), 12);
    EXPECT(MUST(evaluate(vm, realm, "try { buffer.resize(4); false } catch (e) { e instanceof RangeError && e.message === 'too small' && buffer.byteLength === 12 }"sv)).as_bool());
    auto buffer = MUST(evaluate(vm, realm, "buffer"sv));
    EXPECT_EQ(resizes.size(), 2u);
    EXPECT_EQ(resizes[0].buffer, address_of(buffer));
    EXPECT_EQ(resizes[0].new_byte_length, 12u);
    EXPECT_EQ(resizes[1].new_byte_length, 4u);

    Vector<BufferResize> grows;
    vm.host_grow_shared_array_buffer = [&, grow = move(vm.host_grow_shared_array_buffer)](ArrayBuffer& buffer, size_t new_byte_length) -> ThrowCompletionOr<HandledByHost> {
        grows.append({ &buffer, new_byte_length });
        return grow(buffer, new_byte_length);
    };
    EXPECT_EQ(MUST(evaluate(vm, realm, "globalThis.shared = new SharedArrayBuffer(8, { maxByteLength: 16 }); shared.grow(12); shared.byteLength"sv)).as_i32(), 12);
    EXPECT_EQ(grows.size(), 1u);
    EXPECT_EQ(grows[0].buffer, address_of(MUST(evaluate(vm, realm, "shared"sv))));
    EXPECT_EQ(grows[0].new_byte_length, 12u);
}

TEST_CASE(host_hooks_for_module_loading)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    size_t supported_attribute_requests = 0;
    vm.host_get_supported_import_attributes = [&] {
        ++supported_attribute_requests;
        return Vector<Utf16String> { "type"_utf16 };
    };

    struct ModuleLoad {
        void const* referring_script;
        ModuleRequest module_request;
        bool has_load_state;
        bool has_promise_capability;
    };
    Vector<ModuleLoad> loads;
    vm.host_load_imported_module = [&](ImportedModuleReferrer referrer, ModuleRequest const& module_request, GC::Ptr<GC::Cell> load_state, ImportedModulePayload payload) {
        void const* referring_script = nullptr;
        if (referrer.has<GC::Ref<Script>>())
            referring_script = referrer.get<GC::Ref<Script>>().ptr();
        loads.append({ referring_script, module_request, load_state != nullptr, payload.has<GC::Ref<PromiseCapability>>() });
    };

    auto source_text = "import('./nothing.mjs', { with: { type: 'json' } }); undefined"_utf16;
    auto script = MUST(Script::parse(source_text.utf16_view(), realm, "importer.js"sv));
    MUST(vm.run(script));

    EXPECT(supported_attribute_requests >= 1u);
    EXPECT_EQ(loads.size(), 1u);
    EXPECT_EQ(loads[0].referring_script, static_cast<void const*>(script.ptr()));
    EXPECT(loads[0].module_request.module_specifier.view() == "./nothing.mjs"sv);
    EXPECT_EQ(loads[0].module_request.attributes.size(), 1u);
    EXPECT_EQ(loads[0].module_request.attributes[0].key, "type"sv);
    EXPECT_EQ(loads[0].module_request.attributes[0].value, "json"sv);
    EXPECT(!loads[0].has_load_state);
    EXPECT(loads[0].has_promise_capability);
}

TEST_CASE(agent)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto wait = "try { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 0) } catch (e) { e.constructor.name }"sv;
    EXPECT_EQ(MUST(evaluate(vm, realm, wait)).as_string().utf16_string(), "timed-out"sv);

    Vector<GC::Root<GC::Function<void()>>> queued_promise_jobs;
    queue_promise_jobs_in(vm, queued_promise_jobs);

    vm.set_agent(adopt_own(*new TestAgent(Agent::CanBlock::No, queued_promise_jobs)));
    auto& agent = static_cast<TestAgent&>(*vm.agent());
    EXPECT_EQ(agent.can_block(), Agent::CanBlock::No);
    EXPECT_EQ(MUST(evaluate(vm, realm, wait)).as_string().utf16_string(), "TypeError"sv);

    // Awaiting in native code spins the agent's event loop until the promise settles.
    auto dispose = "globalThis.disposed = false;"
                   "const stack = new AsyncDisposableStack();"
                   "stack.use({ [Symbol.asyncDispose]() { disposed = true; return Promise.resolve(); } });"
                   "stack.disposeAsync();"
                   "disposed"sv;
    EXPECT(MUST(evaluate(vm, realm, dispose)).as_bool());
    EXPECT(agent.spin_count >= 1u);
    run_queued_promise_jobs(queued_promise_jobs);

    vm.set_agent(adopt_own(*new TestAgent(Agent::CanBlock::Yes, queued_promise_jobs)));
    EXPECT_EQ(MUST(evaluate(vm, realm, wait)).as_string().utf16_string(), "timed-out"sv);

    vm.set_agent(nullptr);
    EXPECT(!vm.agent());
    EXPECT_EQ(MUST(evaluate(vm, realm, wait)).as_string().utf16_string(), "timed-out"sv);

    vm.finish_execution_generation();
}
