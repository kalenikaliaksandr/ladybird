/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Constructing the VM in storage the embedder provides, and the VM's state.

use core::ffi::c_void;

use crate::embedding::abi_types::vm_from_abi;
use crate::gc::capi::GCHeap;
use crate::interpreter::vm::{Vm, VmOptions};
use crate::layout::host_class::JSVM;
use crate::layout::vm::{VM_ALIGN, VM_SIZE};

// LibJS/Embedding/Layout.h tells an embedder to reserve this much storage for the VM.
const _: () = assert!(size_of::<Vm>() <= VM_SIZE, "the Vm outgrew VM_SIZE in src/layout/vm.rs");
const _: () = assert!(
    align_of::<Vm>() <= VM_ALIGN,
    "the Vm outgrew VM_ALIGN in src/layout/vm.rs"
);

/// How a VM is set up, which is fixed once it is constructed. All false is the setup of a standalone runtime.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct JSVmOptions {
    /// Makes the VM's heap the one GC::Heap::the() returns, so that C++ cells of the embedder are allocated from it.
    pub become_process_default_heap: bool,
    /// Backs SharedArrayBuffers with shared memory, which other processes can map.
    pub shared_memory_shared_array_buffers: bool,
}

impl From<JSVmOptions> for VmOptions {
    fn from(options: JSVmOptions) -> Self {
        Self {
            become_process_default_heap: options.become_process_default_heap,
            shared_memory_shared_array_buffers: options.shared_memory_shared_array_buffers,
        }
    }
}

/// Constructs a VM in the embedder's `storage` of `size` bytes, aligned to `align`, where the VM then has to stay until
/// js_vm_destroy_at(). The embedder keeps owning the storage. JS_LAYOUT_VM_SIZE and JS_LAYOUT_VM_ALIGN are always
/// enough. Returns false, and constructs nothing, if the storage is smaller or less aligned than the VM needs.
/// `options` may be null for the setup of a standalone runtime. The VM's heap belongs to the calling thread, which is
/// the only one that may use the VM from then on.
///
/// # Safety
///
/// `storage` must be valid for writes of `size` bytes and aligned to `align`. `options` must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_construct_at(
    storage: *mut c_void,
    size: usize,
    align: usize,
    options: *const JSVmOptions,
) -> bool {
    if storage.is_null() || size < size_of::<Vm>() || align < align_of::<Vm>() || !storage.cast::<Vm>().is_aligned() {
        return false;
    }
    // SAFETY: The caller passes null or valid options.
    let options = unsafe { options.as_ref() }.copied().unwrap_or_default();
    // SAFETY: The storage is large and aligned enough for a Vm, and the embedder keeps the VM there until it destroys it.
    unsafe { Vm::create_at(storage.cast(), options.into()) };
    true
}

/// Destroys a VM that js_vm_construct_at() constructed, finalizing every cell of its heap, and leaves its storage to
/// the embedder. Only the thread that constructed the VM may call this.
///
/// # Safety
///
/// `vm` must be a live VM, which nothing uses afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_destroy_at(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM that it gives up.
    unsafe { core::ptr::drop_in_place(vm.cast::<Vm>()) };
}

/// The LibGC heap of the VM, a GC::Heap that the VM owns. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm` must be a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_heap(vm: *mut JSVM) -> *mut GCHeap {
    // SAFETY: The caller passes a live VM.
    unsafe { vm_from_abi(vm) }.heap().raw()
}

/// Collects the garbage of the VM's heap, as GC::Heap::collect_garbage() does. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm` must be a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_collect_garbage(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    unsafe { vm_from_abi(vm) }.heap().collect_garbage();
}

/// VM::finish_execution_generation(): ends the synchronous run of code during which WeakRef targets that were
/// dereferenced stay alive, as an embedder does at the end of each microtask checkpoint. Only the VM's thread may call
/// this.
///
/// # Safety
///
/// `vm` must be a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_vm_finish_execution_generation(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let generation = &unsafe { vm_from_abi(vm) }.head.execution_generation;
    generation.set(generation.get() + 1);
}

/// Forgets the system time zone that Date and Temporal cached, so that they pick up a change of the host's time zone.
/// It is the cache of the process rather than of a VM, and any thread may call this.
#[unsafe(no_mangle)]
pub extern "C" fn js_vm_clear_system_time_zone_cache() {
    crate::runtime::date::clear_system_time_zone_cache();
}
