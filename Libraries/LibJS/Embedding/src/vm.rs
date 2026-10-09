/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The VM, and the parts of its head and of the running execution context that an embedder reads and writes
//! directly, as LibJS/Runtime/VM.h does.

use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::abi::{GCHeap, JSRealm, JSVM, js_vm_heap};
use crate::layout::{
    JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET,
    JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET,
    JS_LAYOUT_EXECUTION_CONTEXT_SIZE, JS_LAYOUT_EXECUTION_CONTEXT_THIS_VALUE_OFFSET,
    JS_LAYOUT_VM_EXECUTION_CONTEXT_STACK_LENGTH_OFFSET, JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET,
    JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_DEPTH_OFFSET, JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_OFFSET,
};
use crate::object::read_field;
use crate::realm::Realm;
use crate::value::Value;

const _: () = assert!(crate::layout::JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_SIZE == size_of::<usize>());
const _: () = assert!(crate::layout::JS_LAYOUT_VM_EXECUTION_CONTEXT_STACK_LENGTH_SIZE == size_of::<usize>());
const _: () = assert!(crate::layout::JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_SIZE == size_of::<usize>());
const _: () = assert!(crate::layout::JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_DEPTH_SIZE == size_of::<usize>());
const _: () = assert!(crate::layout::JS_LAYOUT_EXECUTION_CONTEXT_THIS_VALUE_SIZE == size_of::<u64>());
const _: () = assert!(crate::layout::JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_SIZE == size_of::<u32>());

/// The VM, which a process has one of and which only the thread that owns it uses.
#[derive(Clone, Copy)]
pub struct Vm {
    pointer: NonNull<JSVM>,
    _not_send: PhantomData<*const ()>,
}

impl Vm {
    /// # Safety
    ///
    /// `pointer` must be the address of the live VM, and the caller must be on the thread that owns it.
    pub unsafe fn from_raw(pointer: *mut JSVM) -> Self {
        Self {
            pointer: NonNull::new(pointer).expect("the runtime passes its VM"),
            _not_send: PhantomData,
        }
    }

    pub const fn as_raw(self) -> *mut JSVM {
        self.pointer.as_ptr()
    }

    fn head_field<T: Copy>(self, offset: usize) -> T {
        // SAFETY: The VM's head has the fields that Layout.h gives offsets for.
        unsafe { read_field(self.as_raw().cast::<u8>(), offset) }
    }

    fn set_head_field<T: Copy>(self, offset: usize, value: T) {
        let field = self.as_raw().cast::<u8>().wrapping_add(offset).cast::<T>();
        // SAFETY: The VM's head has the fields that Layout.h gives offsets for.
        unsafe { field.write_unaligned(value) };
    }

    /// The running execution context, which every native function runs in.
    pub fn running_frame(self) -> CallFrame {
        let context: *mut u8 = self.head_field(JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET);
        CallFrame {
            context: NonNull::new(context).expect("a native function runs in an execution context"),
            _not_send: PhantomData,
        }
    }

    pub fn current_realm(self) -> Realm {
        self.running_frame().realm()
    }

    pub fn heap(self) -> NonNull<GCHeap> {
        // SAFETY: The VM is live.
        NonNull::new(unsafe { js_vm_heap(self.as_raw()) }).expect("the VM has a heap")
    }
}

/// The running execution context of a native function, whose arguments follow it directly.
#[derive(Clone, Copy)]
pub struct CallFrame {
    context: NonNull<u8>,
    _not_send: PhantomData<*const ()>,
}

impl CallFrame {
    fn field<T: Copy>(self, offset: usize) -> T {
        // SAFETY: The execution context has the fields that Layout.h gives offsets for.
        unsafe { read_field(self.context.as_ptr(), offset) }
    }

    pub fn this_value(self) -> Value {
        let value = Value::from_raw(self.field(JS_LAYOUT_EXECUTION_CONTEXT_THIS_VALUE_OFFSET));
        debug_assert!(!value.is_empty(), "a native function has a this value");
        value
    }

    pub fn argument_count(self) -> usize {
        self.field::<u32>(JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET) as usize
    }

    /// The argument at the index, or undefined past the last one.
    pub fn argument(self, index: usize) -> Value {
        let argument_count = self.argument_count();
        if index >= argument_count {
            return Value::UNDEFINED;
        }
        let slot_count = self
            .field::<u32>(JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET)
            as usize;
        let slots = self
            .context
            .as_ptr()
            .wrapping_add(JS_LAYOUT_EXECUTION_CONTEXT_SIZE)
            .cast::<u64>();
        // SAFETY: The arguments are the last `argument_count` value slots that follow the context.
        Value::from_raw(unsafe { slots.add(slot_count - argument_count + index).read() })
    }

    pub fn realm(self) -> Realm {
        let realm: *mut JSRealm = self.field(JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET);
        // SAFETY: A running execution context has a realm.
        unsafe { Realm::from_non_null(NonNull::new(realm).expect("the execution context has a realm")) }
    }
}

/// Has the TypeErrors that are thrown at the execution context stack depth the scope was created at created in a
/// realm, until the scope is restored or dropped. Callees that push execution contexts are unaffected. This is
/// JS::VM::TypeErrorRealmScope.
pub struct TypeErrorRealmScope {
    vm: Vm,
    previous_realm: *mut JSRealm,
    previous_depth: usize,
    active: bool,
}

impl TypeErrorRealmScope {
    pub fn new(vm: Vm, realm: Realm) -> Self {
        let scope = Self {
            vm,
            previous_realm: vm.head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_OFFSET),
            previous_depth: vm.head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_DEPTH_OFFSET),
            active: true,
        };
        vm.set_head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_OFFSET, realm.as_raw());
        let depth: usize = vm.head_field(JS_LAYOUT_VM_EXECUTION_CONTEXT_STACK_LENGTH_OFFSET);
        vm.set_head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_DEPTH_OFFSET, depth);
        scope
    }

    pub fn restore(&mut self) {
        if !self.active {
            return;
        }
        self.vm
            .set_head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_OFFSET, self.previous_realm);
        self.vm
            .set_head_field(JS_LAYOUT_VM_TYPE_ERROR_REALM_OVERRIDE_DEPTH_OFFSET, self.previous_depth);
        self.active = false;
    }
}

impl Drop for TypeErrorRealmScope {
    fn drop(&mut self) {
        self.restore();
    }
}
