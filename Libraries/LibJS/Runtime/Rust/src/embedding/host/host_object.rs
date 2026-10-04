/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host objects of kind JS_HOST_CLASS_OBJECT.

use core::ffi::c_void;
use core::ops::Deref;
use core::ptr::NonNull;

use crate::embedding::host::class_table::copy_host_class_flags_into_object;
use crate::embedding::host::registry::runtime_class_and_allocator_of_host_class;
use crate::gc::class::{Class, define_cell};
use crate::gc::class_id::ClassId;
use crate::gc::foreign::ForeignCellSlot;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{
    JS_HOST_CLASS_IMMUTABLE_PROTOTYPE, JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE,
    JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH, JS_HOST_CLASS_OBJECT, JSHostClass,
};
pub use crate::layout::host_object::HostObject;
use crate::runtime::error_data::ErrorData;
use crate::runtime::object::{
    MayInterfereWithIndexedPropertyAccess, ORDINARY_OBJECT_METHODS, Object, ObjectMethods, allocate_object_in,
};
use crate::runtime::realm::Realm;

// The class that the class of every host class table of this kind extends. Its own internal methods are ordinary,
// and no object has it as its class.
define_cell!(HostObject, Object, extends: [Object], methods: ORDINARY_OBJECT_METHODS);

// SAFETY: Visits the object and the embedder's two cells, which are all the cells a host object reaches.
unsafe impl Trace for HostObject {
    fn trace(&self, visitor: &mut Visitor) {
        self.base.trace(visitor);
        self.wrappable.trace(visitor);
        self.host_data.trace(visitor);
    }
}

impl Deref for HostObject {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

fn no_error_data(_: &Object) -> Option<&ErrorData> {
    None
}

/// The internal methods of the objects of a host class table: those of an ordinary object, as modified by the table's
/// flags.
fn host_object_methods(table: &'static JSHostClass) -> ObjectMethods {
    let mut methods = ObjectMethods {
        error_data: no_error_data,
        ..ORDINARY_OBJECT_METHODS
    };
    if table.has_flag(JS_HOST_CLASS_IMMUTABLE_PROTOTYPE) {
        methods.internal_set_prototype_of = Object::set_immutable_prototype;
    }
    if table.has_flag(JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE) {
        methods.is_cacheable_for_property_absence = |_| false;
    }
    if table.has_flag(JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH) {
        methods.eligible_for_own_property_enumeration_fast_path = |_| false;
    }
    methods
}

/// The class of the objects of `table`, which extends `parent`.
pub fn derive_host_object_class(table: &'static JSHostClass, parent: &'static Class) -> &'static Class {
    Class::derive_runtime(
        parent,
        table.class_name(),
        Box::leak(Box::new(host_object_methods(table))),
    )
}

impl HostObject {
    /// HostObject::create(): a host object of the class `table` describes, with the realm's empty object shape
    /// transitioned to `prototype`, holding the embedder's `wrappable` and `host_data` cells.
    ///
    /// # Safety
    ///
    /// `table` must be of kind JS_HOST_CLASS_OBJECT, and `wrappable` and `host_data` must be absent or live cells of
    /// the VM's heap.
    pub unsafe fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        table: &'static JSHostClass,
        prototype: Option<Gc<Object>>,
        wrappable: Option<NonNull<c_void>>,
        host_data: Option<NonNull<c_void>>,
    ) -> Gc<HostObject> {
        let (class, allocator) = runtime_class_and_allocator_of_host_class(vm, table, JS_HOST_CLASS_OBJECT);
        let base = Object::new_with_realm_and_prototype(
            vm,
            class,
            realm,
            prototype,
            MayInterfereWithIndexedPropertyAccess::No,
        );
        copy_host_class_flags_into_object(table, &base);
        let host_object = HostObject {
            base,
            host_class: table,
            wrappable: ForeignCellSlot::empty(),
            host_data: ForeignCellSlot::empty(),
        };
        // SAFETY: The caller passes absent or live cells, which the slots then keep alive.
        unsafe {
            host_object.wrappable.set(wrappable);
            host_object.host_data.set(host_data);
        }
        let host_object = allocate_object_in(vm, allocator, host_object);
        host_object.initialize(vm, realm);
        host_object
    }
}

/// The host object an internal method of a host object class was called on.
pub(crate) fn as_host_object(object: &Object) -> &HostObject {
    debug_assert!(object.is::<HostObject>());
    // SAFETY: The object is a HostObject, which starts with its Object.
    unsafe { &*core::ptr::from_ref(object).cast::<HostObject>() }
}

/// host_class_of(): the host class of an object of any host kind, or none for any other object.
pub fn host_class_of(object: &Object) -> Option<&'static JSHostClass> {
    match object.class().id {
        ClassId::HostObject => Some(as_host_object(object).host_class),
        _ => None,
    }
}

/// is_host_instance_of(): whether the object's host class is `table` or derives from it through JSHostClass::parent.
pub fn is_host_instance_of(object: &Object, table: &'static JSHostClass) -> bool {
    host_class_of(object).is_some_and(|host_class| host_class.is_or_derives_from(table))
}

/// The slot of an object of any host kind that holds the embedder's companion cell.
fn host_data_slot_of(object: &Object) -> Option<&ForeignCellSlot> {
    match object.class().id {
        ClassId::HostObject => Some(&as_host_object(object).host_data),
        _ => None,
    }
}

/// host_data_of(): the companion cell of a host object of any kind, or none.
pub fn host_data_of(object: &Object) -> Option<NonNull<c_void>> {
    host_data_slot_of(object).and_then(ForeignCellSlot::get)
}

/// Replaces the companion cell of a host object of any kind.
///
/// # Safety
///
/// `host_data` must be absent or a live cell of the VM's heap.
pub unsafe fn set_host_data(object: &Object, host_data: Option<NonNull<c_void>>) {
    let slot = host_data_slot_of(object).expect("only host objects have host data");
    // SAFETY: The caller passes an absent or live cell, which the slot then keeps alive.
    unsafe { slot.set(host_data) };
}
