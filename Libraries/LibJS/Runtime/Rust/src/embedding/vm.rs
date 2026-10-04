/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Constructing the VM in storage the embedder provides, and the VM's state.

use crate::interpreter::vm::{Vm, VmOptions};
use crate::layout::host_class::JSVM;
use crate::layout::vm::{VM_ALIGN, VM_SIZE};

// LibJS/Embedding/Layout.h tells an embedder to reserve this much storage for the VM.
const _: () = assert!(size_of::<Vm>() <= VM_SIZE, "the Vm outgrew VM_SIZE in src/layout/vm.rs");
const _: () = assert!(
    align_of::<Vm>() <= VM_ALIGN,
    "the Vm outgrew VM_ALIGN in src/layout/vm.rs"
);

/// Creates a VM in an allocation of its own, for an embedder that does not provide storage for it, such as the
/// embedding tests. With `become_process_default_heap`, its heap is the one GC::Heap::the() returns, so that the
/// embedder's C++ cells live in it too. The caller owns the VM and destroys it with js_vm_destroy(). Main thread only.
#[unsafe(no_mangle)]
pub extern "C" fn js_vm_create(become_process_default_heap: bool) -> *mut JSVM {
    let vm = Vm::create_with(VmOptions {
        become_process_default_heap,
        ..VmOptions::default()
    });
    Box::into_raw(vm).cast()
}

/// Destroys a VM that js_vm_create() created, with its heap and every cell in it. Main thread only.
///
/// # Safety
///
/// `vm` must come from js_vm_create() and not have been destroyed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_destroy(vm: *mut JSVM) {
    assert!(!vm.is_null(), "the embedder passes the VM it created");
    // SAFETY: The caller gives back the allocation js_vm_create() made.
    drop(unsafe { Box::from_raw(vm.cast::<Vm>()) });
}
