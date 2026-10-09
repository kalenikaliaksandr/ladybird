/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Objects, and the fields of objects that an embedder reads directly.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::abi::{
    JS_HOST_OBJECT_HOST_CLASS_OFFSET, JS_HOST_OBJECT_HOST_DATA_OFFSET, JSHostClass, JSNativeFunction, JSObject,
    js_object_create, js_object_create_data_property, js_object_define_direct_property,
    js_object_define_native_accessor, js_object_define_native_function, js_object_get, js_object_set_prototype,
};
use crate::completion::{ThrowCompletionOr, bool_from_abi, value_from_abi};
use crate::layout::{
    JS_LAYOUT_CELL_CLASS_OFFSET, JS_LAYOUT_CLASS_ID_HOST_OBJECT, JS_LAYOUT_CLASS_ID_OFFSET,
    JS_LAYOUT_OBJECT_FLAGS_OFFSET, JS_LAYOUT_OBJECT_SHAPE_OFFSET, JS_LAYOUT_SHAPE_REALM_OFFSET,
};
use crate::property_key::PropertyKey;
use crate::realm::Realm;
use crate::value::Value;
use crate::vm::Vm;

/// An object. Like a value, it keeps the object alive only while it is on the stack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Object {
    pointer: NonNull<JSObject>,
    _not_send: PhantomData<*const ()>,
}

/// Reads a field of a cell at an offset that LibJS/Embedding/Layout.h gives.
///
/// # Safety
///
/// `cell` must point to a live cell that has a field of type T at `offset`.
pub(crate) unsafe fn read_field<T: Copy>(cell: *const u8, offset: usize) -> T {
    // SAFETY: The caller guarantees that the field exists. Fields of packed runtime structs can be unaligned.
    unsafe { cell.add(offset).cast::<T>().read_unaligned() }
}

impl Object {
    /// # Safety
    ///
    /// `pointer` must be the address of a live object.
    pub const unsafe fn from_non_null(pointer: NonNull<JSObject>) -> Self {
        Self {
            pointer,
            _not_send: PhantomData,
        }
    }

    /// # Safety
    ///
    /// `pointer` must be null or the address of a live object.
    pub unsafe fn from_raw(pointer: *mut JSObject) -> Option<Self> {
        // SAFETY: The caller guarantees that a pointer that is not null is the address of a live object.
        NonNull::new(pointer).map(|pointer| unsafe { Self::from_non_null(pointer) })
    }

    pub const fn as_raw(self) -> *mut JSObject {
        self.pointer.as_ptr()
    }

    pub fn value(self) -> Value {
        Value::from_object(self)
    }

    fn field<T: Copy>(self, offset: usize) -> T {
        // SAFETY: Every object has the fields of the Object section of Layout.h.
        unsafe { read_field(self.as_raw().cast::<u8>(), offset) }
    }

    pub fn flags(self) -> u16 {
        self.field(JS_LAYOUT_OBJECT_FLAGS_OFFSET)
    }

    /// The ClassId of the object's class, one of the JS_LAYOUT_CLASS_ID_ constants.
    pub fn class_id(self) -> usize {
        let class: *const u8 = self.field(JS_LAYOUT_CELL_CLASS_OFFSET);
        // SAFETY: The first word of every cell points to its class, which has an id.
        usize::from(unsafe { read_field::<u16>(class, JS_LAYOUT_CLASS_ID_OFFSET) })
    }

    /// The realm of the object's shape, which is the realm the object was created in.
    pub fn shape_realm(self) -> Realm {
        let shape: *const u8 = self.field(JS_LAYOUT_OBJECT_SHAPE_OFFSET);
        // SAFETY: Every object has a shape, which has a realm.
        unsafe { Realm::from_non_null(NonNull::new_unchecked(read_field(shape, JS_LAYOUT_SHAPE_REALM_OFFSET))) }
    }

    /// Whether the object is a host object of exactly this class.
    pub fn is_host_object_of_class(self, host_class: &'static JSHostClass) -> bool {
        self.class_id() == JS_LAYOUT_CLASS_ID_HOST_OBJECT
            && core::ptr::eq(
                self.field::<*const JSHostClass>(JS_HOST_OBJECT_HOST_CLASS_OFFSET),
                host_class,
            )
    }

    /// The host data of a host object.
    ///
    /// # Safety
    ///
    /// The object must be a host object.
    pub unsafe fn host_data(self) -> *mut c_void {
        self.field(JS_HOST_OBJECT_HOST_DATA_OFFSET)
    }

    pub fn create(vm: Vm, realm: Realm, prototype: Option<Object>) -> Object {
        let prototype = prototype.map_or(core::ptr::null_mut(), Object::as_raw);
        // SAFETY: The realm and the prototype are live, and creating an object cannot fail.
        unsafe { Object::from_raw(js_object_create(vm.as_raw(), realm.as_raw(), prototype)) }
            .expect("the runtime creates the object")
    }

    pub fn get(self, vm: Vm, key: &PropertyKey<'_>) -> ThrowCompletionOr<Value> {
        // SAFETY: The object is live, and the key outlives the call.
        value_from_abi(unsafe { js_object_get(vm.as_raw(), self.as_raw(), key.as_abi()) })
    }

    pub fn create_data_property(self, vm: Vm, key: &PropertyKey<'_>, value: Value) -> ThrowCompletionOr<bool> {
        // SAFETY: The object is live, and the key outlives the call.
        bool_from_abi(unsafe { js_object_create_data_property(vm.as_raw(), self.as_raw(), key.as_abi(), value.raw()) })
    }

    /// Stores a data property in the object's own storage without running any internal method. The attributes are
    /// JS_ATTRIBUTE_* bits.
    pub fn define_direct_property(self, vm: Vm, key: &PropertyKey<'_>, value: Value, attributes: u8) {
        // SAFETY: The object is live, and the key outlives the call.
        unsafe { js_object_define_direct_property(vm.as_raw(), self.as_raw(), key.as_abi(), value.raw(), attributes) };
    }

    pub fn define_native_function(
        self,
        vm: Vm,
        realm: Realm,
        key: &PropertyKey<'_>,
        behaviour: JSNativeFunction,
        length: i32,
        attributes: u8,
    ) {
        // SAFETY: The object and the realm are live, and the key outlives the call.
        unsafe {
            js_object_define_native_function(
                vm.as_raw(),
                self.as_raw(),
                realm.as_raw(),
                key.as_abi(),
                behaviour,
                length,
                attributes,
            );
        }
    }

    pub fn define_native_accessor(
        self,
        vm: Vm,
        realm: Realm,
        key: &PropertyKey<'_>,
        getter: JSNativeFunction,
        setter: JSNativeFunction,
        attributes: u8,
    ) {
        // SAFETY: The object and the realm are live, and the key outlives the call.
        unsafe {
            js_object_define_native_accessor(
                vm.as_raw(),
                self.as_raw(),
                realm.as_raw(),
                key.as_abi(),
                getter,
                setter,
                attributes,
            );
        }
    }

    pub fn set_prototype(self, vm: Vm, prototype: Option<Object>) {
        let prototype = prototype.map_or(core::ptr::null_mut(), Object::as_raw);
        // SAFETY: The object and the prototype are live.
        unsafe { js_object_set_prototype(vm.as_raw(), self.as_raw(), prototype) };
    }
}
