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
#include <LibJS/Runtime/Agent.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/DeclarativeEnvironment.h>
#include <LibJS/Runtime/Environment.h>
#include <LibJS/Runtime/ErrorTypes.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/FunctionEnvironment.h>
#include <LibJS/Runtime/FunctionObject.h>
#include <LibJS/Runtime/GlobalEnvironment.h>
#include <LibJS/Runtime/GlobalObject.h>
#include <LibJS/Runtime/JobCallback.h>
#include <LibJS/Runtime/ModuleEnvironment.h>
#include <LibJS/Runtime/ObjectEnvironment.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/Promise.h>
#include <LibJS/Runtime/PromiseJob.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Reference.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// Running code as LibJS's users see it: the host hooks around promise jobs, the execution context stack that an event
// loop sets aside, the frames and environments of running code, and the bindings that identifiers resolve to. The same
// expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

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

// The host's queue of microtasks, which runs the promise jobs that the VM hands to the host in the order it hands them.
class MicrotaskQueue {
public:
    void enqueue(VM& vm, PromiseJob job)
    {
        m_microtasks.append(GC::make_root(GC::create_function(vm.heap(), [job = move(job)] {
            MUST(job.run());
        })));
    }

    bool is_empty() const { return m_microtasks.is_empty(); }

    void run_one_microtask()
    {
        auto microtask = m_microtasks.take_first();
        microtask->function()();
    }

    void perform_a_microtask_checkpoint()
    {
        while (!is_empty())
            run_one_microtask();
    }

private:
    Vector<GC::Root<GC::Function<void()>>> m_microtasks;
};

// A host's agent, whose event loop runs one microtask after another until the goal is met or none is left.
class MicrotaskRunningAgent final : public Agent {
public:
    AK_ALLOC_WITH_KMALLOC;

    explicit MicrotaskRunningAgent(MicrotaskQueue& microtask_queue)
        : Agent(CanBlock::No)
        , m_microtask_queue(microtask_queue)
    {
    }

    virtual void spin_event_loop_until(GC::Root<GC::Function<bool()>> goal_condition) override
    {
        while (!goal_condition->function()() && !m_microtask_queue.is_empty())
            m_microtask_queue.run_one_microtask();
        goal_was_met_when_the_spin_ended = goal_condition->function()();
    }

    Optional<bool> goal_was_met_when_the_spin_ended;

private:
    MicrotaskQueue& m_microtask_queue;
};

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source, StringView filename = {})
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm, filename);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static Utf16String string_of(Value value)
{
    return value.as_string().utf16_string();
}

static void const* address_of(Object const& object)
{
    return &object;
}

static void const* address_of(Value value)
{
    return &value.as_object();
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

// Evaluates a script that calls `Function('')` once, and runs `steps` from the host hook that the call reaches, while
// the frames of the script are on the execution context stack.
template<typename Steps>
static ThrowCompletionOr<Value> evaluate_calling_the_host_from_running_code(VM& vm, Realm& realm, StringView filename, StringView source, Steps steps)
{
    size_t host_calls = 0;
    vm.host_ensure_can_compile_strings = [&](Realm&, ReadonlySpan<Utf16String>, Utf16View, Utf16View, CompilationType, ReadonlySpan<Value>, Value) -> ThrowCompletionOr<void> {
        ++host_calls;
        steps();
        return {};
    };
    auto result = evaluate(vm, realm, source, filename);
    vm.host_ensure_can_compile_strings = [](Realm&, ReadonlySpan<Utf16String>, Utf16View, Utf16View, CompilationType, ReadonlySpan<Value>, Value) -> ThrowCompletionOr<void> {
        return {};
    };
    EXPECT_EQ(host_calls, 1u);
    return result;
}

static Vector<ExecutionContext*> execution_contexts_from_the_top(VM& vm)
{
    Vector<ExecutionContext*> execution_contexts;
    vm.for_each_execution_context_top_to_bottom([&](ExecutionContext& execution_context) {
        execution_contexts.append(&execution_context);
        return true;
    });
    return execution_contexts;
}

// The frame that Internals.markAsGarbage() resolves identifiers from.
static Environment& topmost_lexical_environment(VM& vm)
{
    auto execution_context = vm.last_execution_context_matching([](ExecutionContext* execution_context) {
        return execution_context->lexical_environment != nullptr;
    });
    VERIFY(execution_context.has_value());
    return *execution_context.value()->lexical_environment;
}

static StringView kind_of(Environment& environment)
{
    if (is<GlobalEnvironment>(environment))
        return "global"sv;
    if (is<ObjectEnvironment>(environment))
        return "object"sv;
    if (is<FunctionEnvironment>(environment))
        return "function"sv;
    if (is<ModuleEnvironment>(environment))
        return "module"sv;
    if (is<DeclarativeEnvironment>(environment))
        return "declarative"sv;
    VERIFY_NOT_REACHED();
}

static Vector<Utf16FlyString> fly_strings(std::initializer_list<StringView> strings)
{
    Vector<Utf16FlyString> fly_strings;
    for (auto string : strings)
        fly_strings.append(Utf16FlyString::from_utf8(string));
    return fly_strings;
}

TEST_CASE(host_hooks_around_promise_jobs)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    MicrotaskQueue microtask_queue;
    Vector<StringView> hook_calls;
    Vector<Realm*> realms_of_promise_jobs;
    bool report_an_empty_promise_job_queue = true;

    vm.host_enqueue_promise_job = [&](PromiseJob job, GC::Ptr<Realm> job_realm) {
        hook_calls.append("enqueue promise job"sv);
        realms_of_promise_jobs.append(job_realm.ptr());
        microtask_queue.enqueue(vm, move(job));
    };
    vm.host_promise_job_queue_is_empty = [&] {
        hook_calls.append("is the promise job queue empty"sv);
        return report_an_empty_promise_job_queue && microtask_queue.is_empty();
    };
    vm.host_make_job_callback = [&, make_job_callback = move(vm.host_make_job_callback)](FunctionObject& callable) {
        hook_calls.append("make job callback"sv);
        return make_job_callback(callable);
    };
    vm.host_call_job_callback = [&, call_job_callback = move(vm.host_call_job_callback)](JobCallback& job_callback, Value this_value, ReadonlySpan<Value> arguments) {
        hook_calls.append("call job callback"sv);
        return call_job_callback(job_callback, this_value, arguments);
    };
    vm.host_promise_rejection_tracker = [&](Promise&, Promise::RejectionOperation operation) {
        hook_calls.append(operation == Promise::RejectionOperation::Reject ? "track a rejection"sv : "track a handled rejection"sv);
    };

    auto take_hook_calls = [&] {
        auto taken_hook_calls = move(hook_calls);
        hook_calls.clear();
        return taken_hook_calls;
    };

    MUST(evaluate(vm, realm,
        "globalThis.order = [];"
        "Promise.resolve(1).then(value => order.push(`fulfilled with ${value}`));"
        "Promise.reject(2).catch(reason => order.push(`rejected with ${reason}`));"
        "order.push('end of script');"sv));
    // Both runtimes look at the state of a settled promise before they make the job callbacks of a reaction (C++ bug
    // #180), so a handled rejection is tracked first.
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "make job callback"sv, "enqueue promise job"sv, "track a rejection"sv, "track a handled rejection"sv, "make job callback"sv, "enqueue promise job"sv }));
    EXPECT(string_of(MUST(evaluate(vm, realm, "order.join()"sv))) == "end of script"sv);

    // The jobs stay alive while the host holds them.
    collect_garbage(vm);
    microtask_queue.perform_a_microtask_checkpoint();
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "call job callback"sv, "call job callback"sv }));
    EXPECT(string_of(MUST(evaluate(vm, realm, "order.join()"sv))) == "end of script,fulfilled with 1,rejected with 2"sv);

    // An await that resumes from a promise job, while no other job is queued, continues without queuing one.
    auto async_function = "order = [];"
                          "(async () => { order.push('started'); await 1; order.push('resumed once'); await 2; order.push('resumed twice'); })();"
                          "order.push('end of script');"sv;
    MUST(evaluate(vm, realm, async_function));
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "enqueue promise job"sv }));
    microtask_queue.perform_a_microtask_checkpoint();
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "is the promise job queue empty"sv }));
    EXPECT(string_of(MUST(evaluate(vm, realm, "order.join()"sv))) == "started,end of script,resumed once,resumed twice"sv);

    report_an_empty_promise_job_queue = false;
    MUST(evaluate(vm, realm, async_function));
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "enqueue promise job"sv }));
    microtask_queue.perform_a_microtask_checkpoint();
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "is the promise job queue empty"sv, "enqueue promise job"sv }));
    EXPECT(string_of(MUST(evaluate(vm, realm, "order.join()"sv))) == "started,end of script,resumed once,resumed twice"sv);

    // An await of a pending promise reacts to it as then() does.
    MUST(evaluate(vm, realm,
        "order = [];"
        "let resolvePending;"
        "const pending = new Promise(resolve => { resolvePending = resolve; });"
        "(async () => { order.push('started'); order.push(`resumed with ${await pending}`); })();"
        "order.push('awaiting');"
        "resolvePending(3);"sv));
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "make job callback"sv, "make job callback"sv, "enqueue promise job"sv }));
    microtask_queue.perform_a_microtask_checkpoint();
    EXPECT_EQ(take_hook_calls(), (Vector<StringView> { "call job callback"sv }));
    EXPECT(string_of(MUST(evaluate(vm, realm, "order.join()"sv))) == "started,awaiting,resumed with 3"sv);

    EXPECT_EQ(realms_of_promise_jobs.size(), 6u);
    for (auto* realm_of_promise_job : realms_of_promise_jobs)
        EXPECT_EQ(realm_of_promise_job, &realm);
}

TEST_CASE(frames_of_running_code)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto& realm_execution_context = *vm_with_realm.realm_execution_context;
    MUST(evaluate(vm, realm, "globalThis.receiver = { name: 'receiver', method(argument) { return callee(`${this.name} argument`); } };"sv, "receiver.js"sv));

    auto result = evaluate_calling_the_host_from_running_code(vm, realm, "frames.js"sv,
        "function callee(first, second) {"
        "    Function('');"
        "}"
        "receiver.method('method argument', 'extra argument');"sv,
        [&] {
            auto frames = execution_contexts_from_the_top(vm);
            EXPECT_EQ(frames.size(), 5u);
            EXPECT_EQ(frames.last(), &realm_execution_context);

            Vector<Utf16FlyString> function_names;
            for (auto* frame : frames)
                function_names.append(frame->function_name());
            EXPECT_EQ(function_names, fly_strings({ ""sv, "callee"sv, "method"sv, ""sv, ""sv }));

            // Only frames that run bytecode have source code, which tells the scripts that the code comes from apart.
            auto& native_function_frame = *frames[0];
            auto& callee_frame = *frames[1];
            auto& method_frame = *frames[2];
            auto& script_frame = *frames[3];
            EXPECT(!native_function_frame.source_code());
            EXPECT(callee_frame.source_code());
            EXPECT_NE(callee_frame.source_code(), method_frame.source_code());
            EXPECT_EQ(callee_frame.source_code(), script_frame.source_code());
            EXPECT(!realm_execution_context.source_code());

            EXPECT_EQ(address_of(*vm.active_function_object()), address_of(property_of(vm, &realm.global_object(), "Function"sv)));
            EXPECT_EQ(address_of(*callee_frame.function), address_of(property_of(vm, &realm.global_object(), "callee"sv)));
            EXPECT(!script_frame.function);

            // DevTools shows the arguments a frame was passed, and its this value.
            EXPECT_EQ(callee_frame.passed_argument_count, 1u);
            EXPECT_EQ(callee_frame.arguments_span().size(), 2u);
            EXPECT(string_of(callee_frame.arguments_span()[0]) == "receiver argument"sv);
            EXPECT(callee_frame.arguments_span()[1].is_undefined());
            EXPECT_EQ(method_frame.passed_argument_count, 2u);
            EXPECT(string_of(method_frame.arguments_span().slice(0, method_frame.passed_argument_count)[1]) == "extra argument"sv);
            // The frame of a function whose code never uses `this` has no this value.
            EXPECT(native_function_frame.this_value.value().is_undefined());
            EXPECT(!callee_frame.this_value.has_value());
            EXPECT_EQ(address_of(method_frame.this_value.value()), address_of(property_of(vm, &realm.global_object(), "receiver"sv)));
        });
    EXPECT(!result.is_error());
}

TEST_CASE(environments_of_running_code)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto result = evaluate_calling_the_host_from_running_code(vm, realm, "environments.js"sv,
        "var globalVar = 'global var';"
        "let globalLet = 'global let';"
        "const globalConst = 'global const';"
        "function outer(outerParameter) {"
        "    let outerLet = 'outer let';"
        "    function inner(innerParameter) {"
        "        with ({ withProperty: 'with property' }) {"
        "            Function('');"
        "        }"
        "        return [outerParameter, outerLet, innerParameter].join();"
        "    }"
        "    return inner('inner argument');"
        "}"
        "outer('outer argument');"sv,
        [&] {
            // DevTools walks the environments of a paused frame from its lexical environment outwards.
            Vector<Environment*> environments;
            Vector<StringView> kinds;
            for (auto* environment = &topmost_lexical_environment(vm); environment; environment = environment->outer_environment()) {
                environments.append(environment);
                kinds.append(kind_of(*environment));
            }
            EXPECT_EQ(kinds, (Vector<StringView> { "object"sv, "declarative"sv, "function"sv, "global"sv }));
            if (kinds.size() != 4)
                return;

            auto& with_environment = as<ObjectEnvironment>(*environments[0]);
            EXPECT(!environments[0]->is_declarative_environment());
            EXPECT(!environments[0]->is_function_environment());
            EXPECT(string_of(property_of(vm, &with_environment.binding_object(), "withProperty"sv)) == "with property"sv);
            EXPECT(MUST(environments[0]->has_binding("withProperty"_utf16_fly_string)));
            EXPECT(!MUST(environments[0]->has_binding("outerLet"_utf16_fly_string)));

            auto& block_environment = as<DeclarativeEnvironment>(*environments[1]);
            EXPECT(environments[1]->is_declarative_environment());
            EXPECT(!environments[1]->is_function_environment());
            EXPECT_EQ(block_environment.bindings(), fly_strings({ "outerLet"sv }));
            EXPECT(block_environment.binding_is_mutable_by_name("outerLet"_utf16_fly_string));
            EXPECT(string_of(MUST(block_environment.get_binding_value(vm, "outerLet"_utf16_fly_string, false))) == "outer let"sv);

            auto& function_environment = as<FunctionEnvironment>(*environments[2]);
            EXPECT(environments[2]->is_declarative_environment());
            EXPECT(environments[2]->is_function_environment());
            EXPECT(is<DeclarativeEnvironment>(function_environment));
            EXPECT_EQ(function_environment.bindings(), fly_strings({ "outerParameter"sv }));
            EXPECT_EQ(address_of(function_environment.function_object()), address_of(property_of(vm, &realm.global_object(), "outer"sv)));

            auto& global_environment = as<GlobalEnvironment>(*environments[3]);
            EXPECT_EQ(&global_environment, &realm.global_environment());
            EXPECT(!environments[3]->is_declarative_environment());
            EXPECT(!environments[3]->is_function_environment());
            EXPECT_EQ(address_of(global_environment.global_this_value()), address_of(realm.global_object()));
            auto& declarative_record = global_environment.declarative_record();
            EXPECT_EQ(declarative_record.bindings(), fly_strings({ "globalLet"sv, "globalConst"sv }));
            EXPECT(declarative_record.binding_is_mutable_by_name("globalLet"_utf16_fly_string));
            EXPECT(!declarative_record.binding_is_mutable_by_name("globalConst"_utf16_fly_string));
            EXPECT(string_of(MUST(declarative_record.get_binding_value(vm, "globalConst"_utf16_fly_string, false))) == "global const"sv);
            EXPECT(MUST(global_environment.has_binding("globalVar"_utf16_fly_string)));
            EXPECT(MUST(global_environment.has_binding("globalLet"_utf16_fly_string)));
            EXPECT(!MUST(global_environment.has_binding("outerLet"_utf16_fly_string)));

            // A binding the host changes is what the code sees when it continues.
            MUST(block_environment.set_mutable_binding(vm, "outerLet"_utf16_fly_string, PrimitiveString::create(vm, "changed by the host"_utf16), true));
            auto assignment_to_a_constant = declarative_record.set_mutable_binding(vm, "globalConst"_utf16_fly_string, js_undefined(), true);
            EXPECT(assignment_to_a_constant.is_error());
        });
    EXPECT(string_of(MUST(result)) == "outer argument,changed by the host,inner argument"sv);
}

TEST_CASE(resolving_bindings_as_internals_does)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto reference_error = MUST(evaluate(vm, realm, "ReferenceError"sv));
    auto type_error = MUST(evaluate(vm, realm, "TypeError"sv));
    auto expect_thrown = [&](auto const& completion, Value constructor, Optional<StringView> message = {}) {
        EXPECT(completion.is_error());
        if (!completion.is_error())
            return;
        auto error = completion.error_value();
        EXPECT(same_value(property_of(vm, error, "constructor"sv), constructor));
        if (message.has_value())
            EXPECT(string_of(property_of(vm, error, "message"sv)) == *message);
    };

    auto result = evaluate_calling_the_host_from_running_code(vm, realm, "bindings.js"sv,
        "var globalVar = 'global var';"
        "const globalConst = 'global const';"
        "function f(parameter) {"
        "    let local = { name: 'local' };"
        "    const readLocal = () => local;"
        "    let withObject = { withProperty: 'with property' };"
        "    with (withObject) {"
        "        Function('');"
        "    }"
        "    return [readLocal(), 'withProperty' in withObject, globalThis.createdByTheHost].join();"
        "}"
        "f('argument');"sv,
        [&] {
            auto& lexical_environment = topmost_lexical_environment(vm);

            // A binding in an environment, which Internals.markAsGarbage() reads, overwrites and tries to delete.
            auto local = MUST(vm.resolve_binding("local"_utf16_fly_string, Strict::No, &lexical_environment));
            EXPECT(!local.is_unresolvable());
            EXPECT(local.is_environment_reference());
            EXPECT_EQ(kind_of(local.base_environment()), "declarative"sv);
            EXPECT(string_of(property_of(vm, MUST(local.get_value(vm)), "name"sv)) == "local"sv);
            MUST(local.put_value(vm, PrimitiveString::create(vm, "overwritten"_utf16)));
            EXPECT(!MUST(local.delete_(vm)));
            EXPECT(string_of(MUST(local.get_value(vm))) == "overwritten"sv);

            // Without an environment, the binding is resolved from the running execution context's.
            auto local_from_the_running_context = MUST(vm.resolve_binding("local"_utf16_fly_string, Strict::No));
            EXPECT_EQ(&local_from_the_running_context.base_environment(), &local.base_environment());

            // A parameter that no closure captures lives in a register, which no environment has.
            EXPECT(MUST(vm.resolve_binding("parameter"_utf16_fly_string, Strict::No, &lexical_environment)).is_unresolvable());

            // The property of a with statement's object.
            auto with_property = MUST(vm.resolve_binding("withProperty"_utf16_fly_string, Strict::No, &lexical_environment));
            EXPECT_EQ(kind_of(with_property.base_environment()), "object"sv);
            EXPECT(string_of(MUST(with_property.get_value(vm))) == "with property"sv);
            EXPECT(MUST(with_property.delete_(vm)));

            // A var of the global object, which cannot be deleted.
            auto global_var = MUST(vm.resolve_binding("globalVar"_utf16_fly_string, Strict::Yes, &lexical_environment));
            EXPECT_EQ(kind_of(global_var.base_environment()), "global"sv);
            EXPECT(string_of(MUST(global_var.get_value(vm))) == "global var"sv);
            EXPECT(!MUST(global_var.delete_(vm)));

            // An unresolvable reference, which only sloppy code may assign to.
            auto unresolvable = MUST(vm.resolve_binding("createdByTheHost"_utf16_fly_string, Strict::No, &lexical_environment));
            EXPECT(unresolvable.is_unresolvable());
            expect_thrown(unresolvable.get_value(vm), reference_error, "'createdByTheHost' is not defined"sv);
            auto strict_unresolvable = MUST(vm.resolve_binding("createdByTheHost"_utf16_fly_string, Strict::Yes, &lexical_environment));
            expect_thrown(strict_unresolvable.put_value(vm, Value(1)), reference_error, "'createdByTheHost' is not defined"sv);
            EXPECT(MUST(unresolvable.delete_(vm)));
            MUST(unresolvable.put_value(vm, PrimitiveString::create(vm, "created by the host"_utf16)));
            auto created = MUST(vm.resolve_binding("createdByTheHost"_utf16_fly_string, Strict::No, &lexical_environment));
            EXPECT(!created.is_unresolvable());
            EXPECT_EQ(kind_of(created.base_environment()), "global"sv);
            EXPECT(string_of(MUST(created.get_value(vm))) == "created by the host"sv);

            // An assignment to a constant throws a TypeError.
            auto constant = MUST(vm.resolve_binding("globalConst"_utf16_fly_string, Strict::No, &lexical_environment));
            expect_thrown(constant.put_value(vm, js_undefined()), type_error);
        });
    EXPECT(string_of(MUST(result)) == "overwritten,false,created by the host"sv);

    auto created = MUST(vm.resolve_binding("createdByTheHost"_utf16_fly_string, Strict::No, &realm.global_environment()));
    EXPECT(MUST(created.delete_(vm)));
    EXPECT(MUST(vm.resolve_binding("createdByTheHost"_utf16_fly_string, Strict::No, &realm.global_environment())).is_unresolvable());
}

TEST_CASE(module_environments)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto reference_error = MUST(evaluate(vm, realm, "ReferenceError"sv));
    auto type_error = MUST(evaluate(vm, realm, "TypeError"sv));
    auto expect_thrown = [&](auto const& completion, Value constructor) {
        EXPECT(completion.is_error());
        if (completion.is_error())
            EXPECT(same_value(property_of(vm, completion.error_value(), "constructor"sv), constructor));
    };

    // As a WebAssembly module record sets up the environment of its exports.
    auto environment = new_module_environment(nullptr);
    EXPECT(!environment->outer_environment());
    EXPECT(is<ModuleEnvironment>(static_cast<Environment&>(*environment)));
    EXPECT(is<DeclarativeEnvironment>(static_cast<Environment&>(*environment)));
    EXPECT(!is<FunctionEnvironment>(static_cast<Environment&>(*environment)));
    EXPECT(environment->is_declarative_environment());

    auto answer = "answer"_utf16_fly_string;
    MUST(environment->create_immutable_binding(vm, answer, true));
    EXPECT(MUST(environment->has_binding(answer)));
    expect_thrown(environment->get_binding_value(vm, answer, true), reference_error);

    DeclarativeEnvironment& declarative_environment = *environment;
    MUST(declarative_environment.initialize_binding(vm, answer, Value(42), Environment::InitializeBindingHint::Normal));
    EXPECT_EQ(MUST(environment->get_binding_value(vm, answer, true)).as_i32(), 42);
    EXPECT(!environment->binding_is_mutable_by_name(answer));
    expect_thrown(environment->set_mutable_binding(vm, answer, Value(1), true), type_error);

    auto counter = "counter"_utf16_fly_string;
    MUST(environment->create_mutable_binding(vm, counter, false));
    MUST(environment->initialize_binding(vm, counter, Value(1), Environment::InitializeBindingHint::Normal));
    MUST(environment->set_mutable_binding(vm, counter, Value(2), true));
    EXPECT_EQ(MUST(environment->get_binding_value(vm, counter, true)).as_i32(), 2);
    EXPECT(environment->binding_is_mutable_by_name(counter));
    EXPECT_EQ(environment->bindings(), fly_strings({ "answer"sv, "counter"sv }));
    EXPECT(!MUST(environment->has_binding("missing"_utf16_fly_string)));

    collect_garbage(vm);
    EXPECT_EQ(MUST(environment->get_binding_value(vm, counter, true)).as_i32(), 2);

    auto inner_environment = new_module_environment(&realm.global_environment());
    EXPECT_EQ(inner_environment->outer_environment(), &realm.global_environment());
}

TEST_CASE(setting_the_execution_context_stack_aside)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto other_realm_execution_context = MUST(Realm::initialize_host_defined_realm(vm, nullptr, nullptr));
    auto& other_realm = *other_realm_execution_context->realm;
    vm.pop_execution_context();
    auto other_realm_root = GC::make_root(other_realm);

    auto type_error = MUST(evaluate(vm, realm, "TypeError"sv));
    vm.push_execution_context(*other_realm_execution_context);
    auto other_type_error = MUST(evaluate(vm, other_realm, "TypeError"sv));
    vm.pop_execution_context();
    auto constructor_of_thrown = [&](Completion completion) {
        EXPECT(completion.is_error());
        return property_of(vm, completion.value(), "constructor"sv);
    };

    // A TypeErrorRealmScope belongs to the stack that is set aside, along with its depth.
    {
        VM::TypeErrorRealmScope scope { vm, other_realm };
        vm.save_execution_context_stack();
        auto task_execution_context = ExecutionContext::create(0, ReadonlySpan<Value> {}, 0);
        task_execution_context->realm = &realm;
        vm.push_execution_context(*task_execution_context);
        EXPECT_EQ(vm.execution_context_stack().size(), 1u);
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("while set aside"sv)), type_error));
        vm.pop_execution_context();
        vm.restore_execution_context_stack();
        EXPECT(same_value(constructor_of_thrown(vm.throw_completion<TypeError>("after restoring"sv)), other_type_error));
    }

    // An event loop sets the stack of running code aside to run a task, as LibWeb's HTML parser and its event loop do.
    auto result = evaluate_calling_the_host_from_running_code(vm, realm, "event-loop-task.js"sv,
        "function f() { Function(''); return 'the code continued'; }"
        "f();"sv,
        [&] {
            auto* running_execution_context = &vm.running_execution_context();
            auto frames_before = execution_contexts_from_the_top(vm);
            auto stack_size_before = vm.execution_context_stack().size();
            EXPECT(stack_size_before >= 3u);

            vm.save_execution_context_stack();
            EXPECT(!vm.has_running_execution_context());
            EXPECT(vm.execution_context_stack().is_empty());
            EXPECT(execution_contexts_from_the_top(vm).is_empty());
            EXPECT(vm.get_active_script_or_module().has<Empty>());

            vm.push_execution_context(*other_realm_execution_context);
            EXPECT_EQ(vm.current_realm(), &other_realm);
            EXPECT_EQ(MUST(evaluate(vm, other_realm, "6 * 7"sv)).as_i32(), 42);

            // A task may set the stack aside once more, and clear what it pushed.
            vm.save_execution_context_stack();
            EXPECT(vm.execution_context_stack().is_empty());
            vm.push_execution_context(*other_realm_execution_context);
            vm.clear_execution_context_stack();
            EXPECT(!vm.has_running_execution_context());
            vm.restore_execution_context_stack();
            EXPECT_EQ(&vm.running_execution_context(), other_realm_execution_context.ptr());
            EXPECT_EQ(vm.execution_context_stack().size(), 1u);

            vm.clear_execution_context_stack();
            EXPECT(vm.execution_context_stack().is_empty());
            vm.restore_execution_context_stack();

            EXPECT_EQ(&vm.running_execution_context(), running_execution_context);
            EXPECT_EQ(vm.execution_context_stack().size(), stack_size_before);
            EXPECT_EQ(execution_contexts_from_the_top(vm), frames_before);
            EXPECT_EQ(vm.current_realm(), &realm);
        });
    EXPECT(string_of(MUST(result)) == "the code continued"sv);
}

TEST_CASE(execution_contexts_on_the_interpreter_stack)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto source_text = "globalThis.ran = 'the script ran'"_utf16;
    auto script = MUST(Script::parse(source_text.utf16_view(), realm));

    // As LibWeb pushes a context to make a module or script the active one while it finishes loading or evaluates it.
    auto& stack = vm.interpreter_stack();
    auto* stack_mark = stack.top();
    auto* script_execution_context = stack.allocate(0, ReadonlySpan<Value> {}, 0);
    VERIFY(script_execution_context);
    EXPECT_NE(stack.top(), stack_mark);
    script_execution_context->realm = &realm;
    script_execution_context->script_or_module = script;
    vm.push_execution_context(*script_execution_context);
    EXPECT_EQ(vm.get_active_script_or_module().get<GC::Ref<Script>>().ptr(), script.ptr());
    EXPECT_EQ(vm.current_realm(), &realm);
    EXPECT_EQ(vm.argument_count(), 0u);

    // Code that runs meanwhile allocates its frames above the context, and frees them again.
    auto* top_before_running_code = stack.top();
    EXPECT(string_of(MUST(vm.run(script))) == "the script ran"sv);
    EXPECT_EQ(stack.top(), top_before_running_code);
    EXPECT(string_of(MUST(evaluate(vm, realm, "ran"sv))) == "the script ran"sv);

    auto* arguments_mark = stack.top();
    auto* callee_execution_context = stack.allocate(1, ReadonlySpan<Value> {}, 2);
    VERIFY(callee_execution_context);
    callee_execution_context->realm = &realm;
    callee_execution_context->arguments_span()[0] = Value(1);
    callee_execution_context->arguments_span()[1] = Value(2);
    vm.push_execution_context(*callee_execution_context);
    EXPECT_EQ(vm.argument_count(), 2u);
    EXPECT_EQ(vm.argument(1).as_i32(), 2);
    EXPECT(vm.get_active_script_or_module().has<GC::Ref<Script>>());
    EXPECT_EQ(vm.pop_execution_context(), callee_execution_context);
    stack.deallocate(arguments_mark);
    EXPECT_EQ(stack.top(), arguments_mark);

    EXPECT_EQ(vm.pop_execution_context(), script_execution_context);
    stack.deallocate(stack_mark);
    EXPECT_EQ(stack.top(), stack_mark);

    // A context that does not fit on the stack is not allocated.
    EXPECT(!stack.allocate(16 * MiB, ReadonlySpan<Value> {}, 0));
    EXPECT_EQ(stack.top(), stack_mark);
}

TEST_CASE(debugger_is_attached_while_debugging_is_enabled)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    VM const& const_vm = vm;

    EXPECT(!vm.debugging_enabled());
    EXPECT(!vm.debugger());
    vm.enable_debugging();
    EXPECT(vm.debugging_enabled());
    EXPECT(vm.debugger());
    EXPECT_EQ(const_vm.debugger(), vm.debugger());
    vm.enable_debugging();
    EXPECT(vm.debugger());
    vm.disable_debugging();
    EXPECT(!vm.debugging_enabled());
    EXPECT(!vm.debugger());
    EXPECT(!const_vm.debugger());
}

TEST_CASE(agent_spins_its_event_loop_until_an_await_in_native_code_settles)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    MicrotaskQueue microtask_queue;
    vm.host_enqueue_promise_job = [&](PromiseJob job, GC::Ptr<Realm>) {
        microtask_queue.enqueue(vm, move(job));
    };
    vm.host_promise_job_queue_is_empty = [&] { return microtask_queue.is_empty(); };

    vm.set_agent(adopt_own(*new MicrotaskRunningAgent(microtask_queue)));
    auto& agent = static_cast<MicrotaskRunningAgent&>(*vm.agent());

    // AsyncDisposableStack.prototype.disposeAsync() awaits each disposal in native code.
    auto disposed = MUST(evaluate(vm, realm,
        "globalThis.disposed = [];"
        "const stack = new AsyncDisposableStack();"
        "stack.use({ async [Symbol.asyncDispose]() { await null; disposed.push('first'); } });"
        "stack.use({ async [Symbol.asyncDispose]() { disposed.push('second'); } });"
        "stack.disposeAsync();"
        "disposed.join()"sv));
    EXPECT(string_of(disposed) == "second,first"sv);
    EXPECT(agent.goal_was_met_when_the_spin_ended.has_value());
#ifdef LIBJS_TESTS_RUN_ON_THE_RUST_RUNTIME
    EXPECT(agent.goal_was_met_when_the_spin_ended.value());
#else
    // C++ bug #163: the goal condition captures a copy of the outcome from before the promise settles, so the goal is
    // never met, and an agent that spins until it is would spin forever.
    EXPECT(!agent.goal_was_met_when_the_spin_ended.value());
#endif

    microtask_queue.perform_a_microtask_checkpoint();
    vm.set_agent(nullptr);
}
