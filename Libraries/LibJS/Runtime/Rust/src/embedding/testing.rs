/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! A VM with one realm and a way to evaluate scripts in it, for the C++ embedding tests in Tests/LibJS/Embedding,
//! which can only reach the runtime through this ABI. It stands in for the VM, realm and script parts of the ABI in
//! the tests until those exist, and is not meant for embedders.

use crate::embedding::abi_types::{JSRealm, cell_into_abi, completion_into_abi, vm_into_abi};
use crate::interpreter::execution_context::OwnedExecutionContext;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSVM};
use crate::layout::value::Value;
use crate::runtime::completion::Must;
use crate::runtime::error::ErrorKind;
use crate::runtime::realm::Realm;
use crate::script::Script;

/// A VM whose realm's execution context is on its execution context stack, like the realm a tool runs scripts in.
pub struct JSTestingRealm {
    // Fields drop in order, so the context is gone before the VM that held it on its execution context stack.
    root_execution_context: OwnedExecutionContext,
    vm: Box<Vm>,
}

impl Drop for JSTestingRealm {
    fn drop(&mut self) {
        while self.vm.pop_execution_context() != self.root_execution_context.as_non_null() {}
    }
}

impl JSTestingRealm {
    fn realm(&self) -> Gc<Realm> {
        self.root_execution_context
            .realm
            .get()
            .expect("the realm's execution context has its realm")
    }
}

/// Creates a VM and a realm whose execution context it pushes. The caller owns the result and destroys it with
/// js_testing_realm_destroy. Call on the thread that runs the test.
#[unsafe(no_mangle)]
pub extern "C" fn js_testing_realm_create() -> *mut JSTestingRealm {
    let vm = Vm::create();
    let root_execution_context = Realm::initialize_host_defined_realm(&vm, None, None).must();
    Box::into_raw(Box::new(JSTestingRealm {
        root_execution_context,
        vm,
    }))
}

/// Destroys a testing realm and its VM. Call on the thread that created it.
///
/// # Safety
///
/// `testing_realm` must come from js_testing_realm_create and not have been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_testing_realm_destroy(testing_realm: *mut JSTestingRealm) {
    assert!(!testing_realm.is_null(), "the test passes its testing realm");
    // SAFETY: The caller gives back the box js_testing_realm_create made.
    drop(unsafe { Box::from_raw(testing_realm) });
}

/// The VM of a testing realm. Call on the thread that created it.
///
/// # Safety
///
/// `testing_realm` must be a live testing realm.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_testing_realm_vm(testing_realm: *mut JSTestingRealm) -> *mut JSVM {
    // SAFETY: The caller passes a live testing realm.
    vm_into_abi(&unsafe { &*testing_realm }.vm)
}

/// The realm of a testing realm, which is the VM's current realm. Call on the thread that created it.
///
/// # Safety
///
/// `testing_realm` must be a live testing realm.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_testing_realm_realm(testing_realm: *mut JSTestingRealm) -> *mut JSRealm {
    // SAFETY: The caller passes a live testing realm.
    cell_into_abi(unsafe { &*testing_realm }.realm())
}

/// Runs a script of `length` bytes of UTF-8 in the realm, with a completion of its result. A script that does not
/// parse throws a SyntaxError. Call on the thread that created the testing realm.
///
/// # Safety
///
/// `testing_realm` must be a live testing realm, and `source` must point to `length` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_testing_realm_evaluate(
    testing_realm: *mut JSTestingRealm,
    source: *const u8,
    length: usize,
) -> JSCompletion {
    // SAFETY: The caller passes a live testing realm.
    let testing_realm = unsafe { &*testing_realm };
    let vm = &testing_realm.vm;
    // SAFETY: The caller passes `length` bytes of UTF-8.
    let source = core::str::from_utf8(unsafe { core::slice::from_raw_parts(source, length) })
        .expect("the test passes a script in UTF-8");
    let source: Vec<u16> = source.encode_utf16().collect();
    let script = match Script::parse(vm, &source, testing_realm.realm()) {
        Ok(script) => script,
        Err(errors) => {
            let message = errors.first().map(|error| error.message.clone()).unwrap_or_default();
            return completion_into_abi::<Value>(vm.throw_completion_with_message(ErrorKind::SyntaxError, message));
        }
    };
    completion_into_abi(vm.run_script(script, None))
}
