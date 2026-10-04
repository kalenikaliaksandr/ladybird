/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! FinalizationRegistries, whose cleanup the embedder runs as the jobs its enqueue_finalization_registry_cleanup_job
//! hook queues.
//!
//! WeakRefs need nothing of the embedder but the end of each synchronous run of code, ClearKeptObjects(), which
//! js_vm_finish_execution_generation() performs.
//!
//! The functions here follow the contract object.rs states for the embedding module. A FinalizationRegistry crosses as
//! its JSObject, and the functions that take one abort for any other object, as the C++ as<JS::FinalizationRegistry>()
//! does.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    JSRealm, cell_from_abi, cell_into_abi, completion_into_abi, optional_cell_from_abi, vm_from_abi,
};
use crate::embedding::realm::JSJobCallback;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSObject, JSVM};
use crate::runtime::finalization_registry::FinalizationRegistry;

/// # Safety
///
/// `finalization_registry` must be a live FinalizationRegistry.
unsafe fn finalization_registry_from_abi(finalization_registry: *mut JSObject) -> Gc<FinalizationRegistry> {
    // SAFETY: The caller passes a live object.
    unsafe { cell_from_abi::<JSObject>(finalization_registry) }
        .downcast::<FinalizationRegistry>()
        .expect("the object is a FinalizationRegistry")
}

/// [[Realm]] of the FinalizationRegistry, the realm its constructor belonged to. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_weak_finalization_registry_realm(finalization_registry: *mut JSObject) -> *mut JSRealm {
    // SAFETY: See the module documentation.
    cell_into_abi(unsafe { finalization_registry_from_abi(finalization_registry) }.realm())
}

/// [[CleanupCallback]] of the FinalizationRegistry, the JobCallback Record that HostMakeJobCallback made of the
/// function its constructor received. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_weak_finalization_registry_cleanup_callback(
    finalization_registry: *mut JSObject,
) -> *mut JSJobCallback {
    // SAFETY: See the module documentation.
    cell_into_abi(unsafe { finalization_registry_from_abi(finalization_registry) }.cleanup_callback())
}

/// CleanupFinalizationRegistry ( finalizationRegistry ): calls the callback, or [[CleanupCallback]] for null, through
/// the call_job_callback hook with the held value of each registered target that has been collected, and forgets
/// those registrations. A throw stops it and leaves the remaining ones for the next cleanup. It runs JavaScript, so
/// the embedder prepares to run script first, as HTML does. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_weak_finalization_registry_cleanup(
    vm: *mut JSVM,
    finalization_registry: *mut JSObject,
    callback: *mut JSJobCallback,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, finalization_registry, callback) = unsafe {
        (
            vm_from_abi(vm),
            finalization_registry_from_abi(finalization_registry),
            optional_cell_from_abi::<JSJobCallback>(callback),
        )
    };
    completion_into_abi(finalization_registry.cleanup(vm, callback))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{object_into_abi, vm_into_abi};
    use crate::embedding::realm::{js_realm_job_callback_callback, js_realm_job_callback_create};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::utilities::initialize_realm;

    const REGISTER_UNREACHABLE_TARGETS: &str = r#"
        var held = [];
        var registry = new FinalizationRegistry(value => held.push(value));
        (() => {
            for (let i = 0; i < 100; ++i)
                registry.register({}, i);
        })();
        registry
    "#;

    #[test]
    fn the_embedder_cleans_up_a_finalization_registry() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let registry = object_into_abi(run_script(&vm, realm, REGISTER_UNREACHABLE_TARGETS).must().as_object());
        vm.heap().collect_garbage();

        // SAFETY: The VM and the registry are live.
        unsafe {
            assert!(js_weak_finalization_registry_realm(registry) == cell_into_abi::<JSRealm>(realm));
            let cleanup_callback = js_weak_finalization_registry_cleanup_callback(registry);
            let callback = js_realm_job_callback_callback(cleanup_callback);
            assert!(Value::from_object(cell_from_abi::<JSObject>(callback)).is_function());

            // Another callback that throws stops at the first held value, and leaves the rest to the next cleanup.
            let thrower = run_script(&vm, realm, "value => { throw value; }").must();
            let throwing_callback =
                js_realm_job_callback_create(abi_vm, object_into_abi(thrower.as_object()), core::ptr::null_mut());
            let completion = js_weak_finalization_registry_cleanup(abi_vm, registry, throwing_callback);
            assert!(completion.variant == JS_COMPLETION_THROW);

            let completion = js_weak_finalization_registry_cleanup(abi_vm, registry, core::ptr::null_mut());
            assert!(completion.variant == JS_COMPLETION_NORMAL);
        }
        // The throwing callback took one held value, and the registry's own callback the others.
        let collected = run_script(&vm, realm, "held.length").must().as_i32();
        assert!(collected > 0, "no target was collected");
        assert_eq!(
            utf8(run_script(&vm, realm, "new Set(held).size === held.length").must()),
            "true"
        );
        // SAFETY: As above.
        let completion = unsafe { js_weak_finalization_registry_cleanup(abi_vm, registry, core::ptr::null_mut()) };
        assert!(completion.variant == JS_COMPLETION_NORMAL);
        assert_eq!(run_script(&vm, realm, "held.length").must().as_i32(), collected);
    }
}
