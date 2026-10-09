/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host objects: objects whose class an embedder defines with a JSHostClass, as LibJS/HostObjectABI.h describes.

use core::ffi::c_void;

use crate::abi::{JSHostClass, js_host_object_create};
use crate::object::Object;
use crate::realm::Realm;
use crate::vm::Vm;

/// A new host object of the class, with its wrappable and host data slots holding the cells, or null.
///
/// # Safety
///
/// The wrappable and the host data must each be null or a live cell that the class's hooks expect there.
pub unsafe fn create(
    vm: Vm,
    realm: Realm,
    host_class: &'static JSHostClass,
    prototype: Option<Object>,
    wrappable: *mut c_void,
    host_data: *mut c_void,
) -> Object {
    let prototype = prototype.map_or(core::ptr::null_mut(), Object::as_raw);
    // SAFETY: The VM, the realm and the prototype are live, and the caller guarantees the slots.
    let object =
        unsafe { js_host_object_create(vm.as_raw(), realm.as_raw(), host_class, prototype, wrappable, host_data) };
    // SAFETY: The runtime returns the new host object.
    unsafe { Object::from_raw(object) }.expect("the runtime creates the host object")
}
