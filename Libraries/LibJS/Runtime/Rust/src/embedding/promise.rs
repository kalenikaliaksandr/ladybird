/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Promises, promise capabilities and promise jobs.
//!
//! The functions here follow the contract object.rs states for the embedding module. A promise crosses as the JSObject
//! of the Promise, and the functions that take one abort for any other object, as the C++ as<JS::Promise>() does.
//! Settling a promise or adding a reaction to a settled one calls the embedder's enqueue_promise_job and
//! promise_rejection_tracker hooks, which may run JavaScript.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    JSRealm, cell_from_abi, cell_into_abi, completion_into_abi, object_into_abi, optional_cell_from_abi, vm_from_abi,
};
use crate::embedding::hooks::{JSPromiseJob, promise_job_from_abi};
use crate::embedding::object::function_from_abi;
use crate::gc::heap_function::HeapFunction;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSObject, JSPromiseCapability, JSVM, JSValue};
use crate::layout::value::Value;
use crate::runtime::promise::{Promise, PromiseState, promise_resolve};
use crate::runtime::promise_capability::{PromiseCapability, new_promise_capability};

/// [[PromiseState]], in the order of the C++ JS::Promise::State.
pub type JSPromiseState = u8;

pub const JS_PROMISE_STATE_PENDING: JSPromiseState = 0;
pub const JS_PROMISE_STATE_FULFILLED: JSPromiseState = 1;
pub const JS_PROMISE_STATE_REJECTED: JSPromiseState = 2;

/// The functions CreateResolvingFunctions ( promise ) returns, which share one [[AlreadyResolved]] record.
#[repr(C)]
pub struct JSPromiseResolvingFunctions {
    pub resolve: *mut JSObject,
    pub reject: *mut JSObject,
}

/// # Safety
///
/// `promise` must be a live Promise.
unsafe fn promise_from_abi(promise: *mut JSObject) -> Gc<Promise> {
    // SAFETY: The caller passes a live object.
    unsafe { cell_from_abi::<JSObject>(promise) }
        .downcast::<Promise>()
        .expect("the object is a Promise")
}

/// Runs a promise job that the embedder's enqueue_promise_job hook received, and returns its completion. Like the jobs
/// of the VM's own queue, it runs on top of the running execution context, so the embedder prepares that first, as
/// HTML prepares to run script with the job's realm. Each job runs once. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm` must be a live VM and `job` a promise job of it that the embedder kept alive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_job_run(vm: *mut JSVM, job: *mut JSPromiseJob) -> JSCompletion {
    // SAFETY: The caller passes a live VM and promise job.
    let (vm, job) = unsafe { (vm_from_abi(vm), promise_job_from_abi(job)) };
    completion_into_abi(HeapFunction::call(job, vm))
}

/// Promise::create(realm): a pending Promise whose prototype is the realm's %Promise.prototype%. Returns an unrooted
/// promise. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_create(vm: *mut JSVM, realm: *mut JSRealm) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSRealm>(realm)) };
    object_into_abi(Promise::create(vm, realm))
}

/// [[PromiseState]], one of JS_PROMISE_STATE_*. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_state(promise: *mut JSObject) -> JSPromiseState {
    // SAFETY: See the module documentation.
    match unsafe { promise_from_abi(promise) }.state() {
        PromiseState::Pending => JS_PROMISE_STATE_PENDING,
        PromiseState::Fulfilled => JS_PROMISE_STATE_FULFILLED,
        PromiseState::Rejected => JS_PROMISE_STATE_REJECTED,
    }
}

/// [[PromiseResult]]: the value or reason of a settled promise, and undefined while it is pending. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_result(promise: *mut JSObject) -> JSValue {
    // SAFETY: See the module documentation.
    unsafe { promise_from_abi(promise) }.result().0
}

/// [[PromiseIsHandled]]. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_is_handled(promise: *mut JSObject) -> bool {
    // SAFETY: See the module documentation.
    unsafe { promise_from_abi(promise) }.is_handled()
}

/// Sets [[PromiseIsHandled]] to true, without telling the promise_rejection_tracker hook, which the embedder does
/// itself where it needs to. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_set_is_handled(promise: *mut JSObject) {
    // SAFETY: See the module documentation.
    unsafe { promise_from_abi(promise) }.set_is_handled();
}

/// FulfillPromise ( promise, value ), which aborts unless the promise is pending, and queues its fulfill reactions.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_fulfill(vm: *mut JSVM, promise: *mut JSObject, value: JSValue) {
    // SAFETY: See the module documentation.
    let (vm, promise) = unsafe { (vm_from_abi(vm), promise_from_abi(promise)) };
    promise.fulfill(vm, Value(value));
}

/// RejectPromise ( promise, reason ), which aborts unless the promise is pending, tells the promise_rejection_tracker
/// hook unless the promise is handled, and queues its reject reactions. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_reject(vm: *mut JSVM, promise: *mut JSObject, reason: JSValue) {
    // SAFETY: See the module documentation.
    let (vm, promise) = unsafe { (vm_from_abi(vm), promise_from_abi(promise)) };
    promise.reject(vm, Value(reason));
}

/// PerformPromiseThen ( promise, onFulfilled, onRejected [ , resultCapability ] ). A reaction that is not a function
/// passes the value or reason on, and a null capability stands for none. Returns the capability's promise, or
/// undefined without one. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_perform_then(
    vm: *mut JSVM,
    promise: *mut JSObject,
    on_fulfilled: JSValue,
    on_rejected: JSValue,
    result_capability: *mut JSPromiseCapability,
) -> JSValue {
    // SAFETY: See the module documentation.
    let (vm, promise, result_capability) = unsafe {
        (
            vm_from_abi(vm),
            promise_from_abi(promise),
            optional_cell_from_abi::<JSPromiseCapability>(result_capability),
        )
    };
    promise
        .perform_then(vm, Value(on_fulfilled), Value(on_rejected), result_capability)
        .0
}

/// CreateResolvingFunctions ( promise ), whose functions belong to the current realm. Returns unrooted functions. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_create_resolving_functions(
    vm: *mut JSVM,
    promise: *mut JSObject,
) -> JSPromiseResolvingFunctions {
    // SAFETY: See the module documentation.
    let (vm, promise) = unsafe { (vm_from_abi(vm), promise_from_abi(promise)) };
    let resolving_functions = promise.create_resolving_functions(vm);
    JSPromiseResolvingFunctions {
        resolve: object_into_abi(resolving_functions.resolve),
        reject: object_into_abi(resolving_functions.reject),
    }
}

/// PromiseResolve ( C, x ), whose payload is the promise: x itself if it is a promise whose "constructor" is C, and
/// otherwise a new promise of C resolved with x. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_resolve(vm: *mut JSVM, constructor: *mut JSObject, value: JSValue) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, constructor) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(constructor)) };
    completion_into_abi(promise_resolve(vm, constructor, Value(value)))
}

/// NewPromiseCapability ( C ), whose payload is the JSPromiseCapability. It throws a TypeError if C is not a
/// constructor, and whatever constructing C throws; the realm's %Promise% does neither. Returns an unrooted capability.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_capability_new(vm: *mut JSVM, constructor: JSValue) -> JSCompletion {
    // SAFETY: See the module documentation.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(new_promise_capability(vm, Value(constructor)).map(cell_into_abi::<JSPromiseCapability>))
}

/// PromiseCapability::create(): the PromiseCapability Record { [[Promise]], [[Resolve]], [[Reject]] } of an object and
/// two functions. Returns an unrooted capability. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_capability_create(
    vm: *mut JSVM,
    promise: *mut JSObject,
    resolve: *mut JSObject,
    reject: *mut JSObject,
) -> *mut JSPromiseCapability {
    // SAFETY: See the module documentation.
    let (vm, promise, resolve, reject) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(promise),
            function_from_abi(resolve),
            function_from_abi(reject),
        )
    };
    cell_into_abi(PromiseCapability::create(vm, promise, resolve, reject))
}

/// [[Promise]] of the capability, which is an object but not necessarily a Promise. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_capability_promise(capability: *mut JSPromiseCapability) -> *mut JSObject {
    // SAFETY: See the module documentation.
    object_into_abi(unsafe { cell_from_abi::<JSPromiseCapability>(capability) }.promise())
}

/// [[Resolve]] of the capability, a function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_capability_resolve(capability: *mut JSPromiseCapability) -> *mut JSObject {
    // SAFETY: See the module documentation.
    object_into_abi(unsafe { cell_from_abi::<JSPromiseCapability>(capability) }.resolve())
}

/// [[Reject]] of the capability, a function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_promise_capability_reject(capability: *mut JSPromiseCapability) -> *mut JSObject {
    // SAFETY: See the module documentation.
    object_into_abi(unsafe { cell_from_abi::<JSPromiseCapability>(capability) }.reject())
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{cell_of_payload, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::layout::realm::Realm;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::utilities::initialize_realm;

    fn define_global(vm: &Vm, realm: Gc<Realm>, name: &str, value: Value) {
        realm
            .global_object()
            .define_direct_property(vm, &key(name), value, DEFAULT_ATTRIBUTES);
    }

    fn object_value(object: *mut JSObject) -> Value {
        // SAFETY: The tests pass live objects.
        Value::from_object(unsafe { cell_from_abi::<JSObject>(object) })
    }

    fn new_capability(vm: &Vm, constructor: Value) -> *mut JSPromiseCapability {
        // SAFETY: The VM is live.
        let completion = unsafe { js_promise_capability_new(vm_into_abi(vm), constructor.0) };
        // SAFETY: The payload of a normal completion is the capability.
        cell_into_abi(unsafe { cell_of_payload::<JSPromiseCapability>(completion) })
    }

    #[test]
    fn capabilities_settle_their_promises_through_reactions() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let promise_constructor = Value::from_object(realm.intrinsics().promise_constructor(&vm));
        run_script(&vm, realm, "var log = [];").must();
        let on_fulfilled = run_script(
            &vm,
            realm,
            "value => { log.push('fulfilled ' + value); return value + 1; }",
        )
        .must();
        let on_rejected = run_script(&vm, realm, "reason => { log.push('rejected ' + reason); }").must();

        let capability = new_capability(&vm, promise_constructor);
        let chained = new_capability(&vm, promise_constructor);
        // SAFETY: The capabilities and their cells are live.
        unsafe {
            let promise = js_promise_capability_promise(capability);
            assert_eq!(js_promise_state(promise), JS_PROMISE_STATE_PENDING);
            assert!(js_promise_result(promise) == Value::UNDEFINED.0);
            let chained_promise = js_promise_capability_promise(chained);
            let returned = js_promise_perform_then(vm_into_abi(&vm), promise, on_fulfilled.0, on_rejected.0, chained);
            assert!(returned == object_value(chained_promise).0);
            define_global(&vm, realm, "chained", object_value(chained_promise));
            define_global(
                &vm,
                realm,
                "resolve",
                object_value(js_promise_capability_resolve(capability)),
            );
            run_script(
                &vm,
                realm,
                "resolve(41); chained.then(value => log.push('chained ' + value));",
            )
            .must();
            assert_eq!(js_promise_state(promise), JS_PROMISE_STATE_FULFILLED);
            assert!(js_promise_result(promise) == Value::from_i32(41).0);
        }
        vm.run_queued_promise_jobs();
        assert_eq!(
            utf8(run_script(&vm, realm, "log.join()").must()),
            "fulfilled 41,chained 42"
        );

        // Without a capability, PerformPromiseThen returns undefined.
        let capability = new_capability(&vm, promise_constructor);
        // SAFETY: As above.
        unsafe {
            let promise = js_promise_capability_promise(capability);
            assert!(!js_promise_is_handled(promise));
            let returned = js_promise_perform_then(
                vm_into_abi(&vm),
                promise,
                Value::UNDEFINED.0,
                on_rejected.0,
                core::ptr::null_mut(),
            );
            assert!(returned == Value::UNDEFINED.0);
            assert!(js_promise_is_handled(promise));
            js_promise_reject(vm_into_abi(&vm), promise, Value::from_i32(7).0);
            assert_eq!(js_promise_state(promise), JS_PROMISE_STATE_REJECTED);
            assert!(js_promise_result(promise) == Value::from_i32(7).0);

            let unhandled = js_promise_create(vm_into_abi(&vm), cell_into_abi::<JSRealm>(realm));
            js_promise_set_is_handled(unhandled);
            assert!(js_promise_is_handled(unhandled));
        }
        vm.run_queued_promise_jobs();
        assert_eq!(
            utf8(run_script(&vm, realm, "log.join()").must()),
            "fulfilled 41,chained 42,rejected 7"
        );
    }

    #[test]
    fn promises_are_created_resolved_and_given_resolving_functions() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let abi_realm = cell_into_abi::<JSRealm>(realm);
        let promise_constructor = object_into_abi(realm.intrinsics().promise_constructor(&vm));

        // SAFETY: The VM, realm and cells are live.
        unsafe {
            let promise = js_promise_create(abi_vm, abi_realm);
            define_global(&vm, realm, "promise", object_value(promise));
            assert_eq!(
                utf8(run_script(&vm, realm, "Object.getPrototypeOf(promise) === Promise.prototype").must()),
                "true"
            );

            // PromiseResolve returns a promise of the constructor itself, and wraps anything else.
            let same = js_promise_resolve(abi_vm, promise_constructor, object_value(promise).0);
            assert!(same.variant == JS_COMPLETION_NORMAL);
            assert!(cell_of_payload::<JSObject>(same) == cell_from_abi::<JSObject>(promise));
            let wrapped = js_promise_resolve(abi_vm, promise_constructor, Value::from_i32(3).0);
            let wrapped = cell_into_abi::<JSObject>(cell_of_payload::<JSObject>(wrapped));
            assert_eq!(js_promise_state(wrapped), JS_PROMISE_STATE_FULFILLED);
            assert!(js_promise_result(wrapped) == Value::from_i32(3).0);

            // The two resolving functions share one [[AlreadyResolved]]: the first call settles the promise.
            let functions = js_promise_create_resolving_functions(abi_vm, promise);
            define_global(&vm, realm, "resolve", object_value(functions.resolve));
            define_global(&vm, realm, "reject", object_value(functions.reject));
            run_script(&vm, realm, "reject('first'); resolve('second');").must();
            assert_eq!(js_promise_state(promise), JS_PROMISE_STATE_REJECTED);
            assert_eq!(utf8(Value(js_promise_result(promise))), "first");

            let fulfilled = js_promise_create(abi_vm, abi_realm);
            js_promise_fulfill(abi_vm, fulfilled, Value::TRUE.0);
            assert_eq!(js_promise_state(fulfilled), JS_PROMISE_STATE_FULFILLED);

            // A capability of any object and two functions, and NewPromiseCapability of something that is not a
            // constructor.
            let capability =
                js_promise_capability_create(abi_vm, promise_constructor, functions.resolve, functions.reject);
            assert!(js_promise_capability_promise(capability) == promise_constructor);
            assert!(js_promise_capability_reject(capability) == functions.reject);
            assert_eq!(
                js_promise_capability_new(abi_vm, Value::from_i32(1).0).variant,
                JS_COMPLETION_THROW
            );
        }
    }

    #[test]
    fn a_capability_of_a_subclass_runs_the_subclass_constructor() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let constructor = run_script(
            &vm,
            realm,
            "var constructed = 0; class Sub extends Promise { constructor(executor) { super(executor); ++constructed; } }; Sub",
        )
        .must();
        let capability = new_capability(&vm, constructor);
        // SAFETY: The capability is live.
        let promise = unsafe { js_promise_capability_promise(capability) };
        define_global(&vm, realm, "sub", object_value(promise));
        assert_eq!(
            utf8(run_script(&vm, realm, "constructed === 1 && sub instanceof Sub").must()),
            "true"
        );
        // SAFETY: The promise is live, and an instance of a subclass of Promise is a Promise.
        assert_eq!(unsafe { js_promise_state(promise) }, JS_PROMISE_STATE_PENDING);
    }
}
