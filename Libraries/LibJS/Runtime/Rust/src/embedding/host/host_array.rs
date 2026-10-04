/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host arrays, of kind JS_HOST_CLASS_ARRAY.

use core::ffi::c_void;
use core::ops::Deref;
use core::ptr::NonNull;

use libjs_runtime_macros::Trace;

use crate::embedding::hooks::lend_property_key_to_abi;
use crate::embedding::host::class_table::{
    bool_completion_from_hook, copy_host_class_flags_into_object, lend_object_to_hook, lookup_phase_into_abi,
    set_cache_metadata_into_abi,
};
use crate::embedding::host::registry::runtime_class_and_allocator_of_host_class;
use crate::gc::class::{Class, define_cell};
use crate::gc::foreign::ForeignCellSlot;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JS_HOST_CLASS_ARRAY, JSHostArrayHooks, JSHostClass};
use crate::layout::value::Value;
use crate::runtime::array::{ARRAY_OBJECT_METHODS, Array};
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::object::{
    CacheableSetPropertyMetadata, Object, ObjectMethods, PropertyLookupPhase, allocate_object_in,
};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;

/// An Array exotic object whose [[Set]] and [[Delete]] may come from its host class, with a C++ GC cell for any state
/// of its own.
#[repr(C)]
#[derive(Trace)]
pub struct HostArray {
    pub base: Array,
    #[gc(untraced)]
    pub host_class: &'static JSHostClass,
    pub host_data: ForeignCellSlot,
}

// The class that the class of every host class table of this kind extends. No array has it as its class.
define_cell!(HostArray, Object, extends: [Array, Object], methods: ARRAY_OBJECT_METHODS);

impl Deref for HostArray {
    type Target = Array;

    fn deref(&self) -> &Array {
        &self.base
    }
}

/// The internal methods of the arrays of a host class table: those of an Array exotic object, with [[Set]] and
/// [[Delete]] calling the table's hooks for them.
fn host_array_methods(table: &'static JSHostClass) -> &'static ObjectMethods {
    let hooks = table.host_array_hooks();
    if hooks.set.is_none() && hooks.delete_property.is_none() {
        return &ARRAY_OBJECT_METHODS;
    }
    let mut methods = ObjectMethods { ..ARRAY_OBJECT_METHODS };
    if hooks.set.is_some() {
        methods.internal_set = set_through_hook;
    }
    if hooks.delete_property.is_some() {
        methods.internal_delete = delete_through_hook;
    }
    Box::leak(Box::new(methods))
}

/// The class of the arrays of `table`, which extends `parent`.
pub fn derive_host_array_class(table: &'static JSHostClass, parent: &'static Class) -> &'static Class {
    Class::derive_runtime(parent, table.class_name(), host_array_methods(table))
}

/// The host array an internal method of a host array class was called on.
pub(crate) fn as_host_array(object: &Object) -> &HostArray {
    debug_assert!(object.is::<HostArray>());
    // SAFETY: The object is a HostArray, which starts with its Object.
    unsafe { &*core::ptr::from_ref(object).cast::<HostArray>() }
}

fn hooks_of(object: &Object) -> &'static JSHostArrayHooks {
    as_host_array(object).host_class.host_array_hooks()
}

// The internal methods of a class whose table has a hook for them. The runtime holds no borrow of its state across
// the call, and lends the hook the array and key for its duration.

fn set_through_hook(
    object: &Object,
    _vm: &Vm,
    key: &PropertyKey,
    value: Value,
    receiver: Value,
    cacheable_metadata: Option<&mut CacheableSetPropertyMetadata>,
    phase: PropertyLookupPhase,
) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object).set.expect("the class has a [[Set]] hook");
    // SAFETY: The hook takes an array of its class, a key, and the caller's cache metadata and phase, which it passes
    //         on untouched, if at all, to the engine operation it delegates to.
    bool_completion_from_hook(unsafe {
        hook(
            lend_object_to_hook(object),
            lend_property_key_to_abi(key),
            value.0,
            receiver.0,
            set_cache_metadata_into_abi(cacheable_metadata),
            lookup_phase_into_abi(phase),
        )
    })
}

fn delete_through_hook(object: &Object, _vm: &Vm, key: &PropertyKey) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .delete_property
        .expect("the class has a [[Delete]] hook");
    // SAFETY: The hook takes an array of its class and a key.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object), lend_property_key_to_abi(key)) })
}

impl HostArray {
    /// HostArray::create(): an empty array of the class `table` describes. The prototype defaults to
    /// %Array.prototype%.
    ///
    /// # Safety
    ///
    /// `table` must be of kind JS_HOST_CLASS_ARRAY, and `host_data` absent or a live cell of the VM's heap.
    pub unsafe fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        table: &'static JSHostClass,
        prototype: Option<Gc<Object>>,
        host_data: Option<NonNull<c_void>>,
    ) -> Gc<HostArray> {
        let (class, allocator) = runtime_class_and_allocator_of_host_class(vm, table, JS_HOST_CLASS_ARRAY);
        let prototype = prototype.unwrap_or_else(|| realm.intrinsics().array_prototype(vm));
        let base = Array::new_with_class(vm, class, realm, prototype);
        copy_host_class_flags_into_object(table, &base);
        let array = HostArray {
            base,
            host_class: table,
            host_data: ForeignCellSlot::empty(),
        };
        // SAFETY: The caller passes an absent or live cell, which the slot then keeps alive.
        unsafe { array.host_data.set(host_data) };
        let array = allocate_object_in(vm, allocator, array);
        array.initialize(vm, realm);
        array
    }

    /// The [[Set]] of an Array exotic object, for hooks that add to it rather than replace it.
    pub fn array_set(
        &self,
        vm: &Vm,
        key: &PropertyKey,
        value: Value,
        receiver: Value,
        cacheable_metadata: Option<&mut CacheableSetPropertyMetadata>,
        phase: PropertyLookupPhase,
    ) -> ThrowCompletionOr<bool> {
        (ARRAY_OBJECT_METHODS.internal_set)(self, vm, key, value, receiver, cacheable_metadata, phase)
    }

    /// The [[Delete]] of an Array exotic object, for hooks that add to it rather than replace it.
    pub fn array_delete(&self, vm: &Vm, key: &PropertyKey) -> ThrowCompletionOr<bool> {
        (ARRAY_OBJECT_METHODS.internal_delete)(self, vm, key)
    }
}
