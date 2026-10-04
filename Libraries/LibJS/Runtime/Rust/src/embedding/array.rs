/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Creating arrays and reading and writing their indexed storage.
//!
//! The functions here follow the contract object.rs states for the embedding module.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    JSRealm, cell_from_abi, completion_into_abi, object_into_abi, optional_cell_from_abi, vm_from_abi,
};
use crate::embedding::object::values_from_abi;
use crate::gc::root::MarkedVec;
use crate::layout::host_class::{JSCompletion, JSObject, JSVM, JSValue};
use crate::layout::value::Value;
use crate::runtime::array::Array;
use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;

/// ArrayCreate(length, proto), whose payload is the array. A null prototype stands for the realm's
/// %Array.prototype%, and a length above 2^32 - 1 throws a RangeError. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_create(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    length: u64,
    prototype: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, realm, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    completion_into_abi(Array::create(vm, realm, length, prototype))
}

/// CreateArrayFromList(elements), for `count` values at `elements`, which the runtime roots while it allocates the
/// array. Returns an unrooted array. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_create_from(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    elements: *const JSValue,
    count: usize,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm, elements) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            values_from_abi(elements, count),
        )
    };
    let rooted_elements = MarkedVec::with_capacity(vm, elements.len());
    for element in elements {
        rooted_elements.push(*element);
    }
    object_into_abi(Array::create_from_list(vm, realm, &rooted_elements))
}

/// The size of the object's indexed storage: the length of an array, or one past its highest index. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_indexed_array_like_size(object: *mut JSObject) -> u32 {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.indexed_array_like_size()
}

/// Reads the element of the index from the object's indexed storage, without running any internal method. Returns
/// whether there is one, and writes its value and its JS_ATTRIBUTE_* bits when there is; `attributes` may be null.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_indexed_get(
    object: *mut JSObject,
    index: u32,
    value: *mut JSValue,
    attributes: *mut u8,
) -> bool {
    // SAFETY: See the module documentation.
    let Some(element) = unsafe { cell_from_abi::<JSObject>(object) }.indexed_get(index) else {
        return false;
    };
    // SAFETY: As above, `value` is writable, and `attributes` is null or writable.
    unsafe {
        value.write(element.value.0);
        if let Some(attributes) = attributes.as_mut() {
            *attributes = element.attributes.bits();
        }
    }
    true
}

/// Appends a writable, enumerable and configurable element to the object's indexed storage, without running any
/// internal method. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_indexed_append(object: *mut JSObject, value: JSValue) {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.indexed_append(Value(value), DEFAULT_ATTRIBUTES);
}

/// Removes the first element of the object's indexed storage, which must not be empty, shifting the others down, and
/// returns its value, which is empty for a hole. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_array_indexed_take_first(object: *mut JSObject) -> JSValue {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }
        .indexed_take_first()
        .value
        .0
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{cell_into_abi, cell_of_payload, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::JS_COMPLETION_THROW;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::Attribute;
    use crate::runtime::realm::test_realm::key;
    use crate::utilities::initialize_realm;

    #[test]
    fn arrays_grow_and_shrink_through_their_indexed_storage() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let elements = [Value::from_i32(1).0, Value::from_i32(2).0];
        // SAFETY: The arguments are live.
        let array =
            unsafe { js_array_create_from(vm_into_abi(&vm), cell_into_abi::<JSRealm>(realm), elements.as_ptr(), 2) };
        // SAFETY: As above.
        unsafe {
            js_array_indexed_append(array, Value::from_i32(3).0);
            assert_eq!(js_array_indexed_array_like_size(array), 3);
            let (mut value, mut attributes) = (0, 0);
            assert!(js_array_indexed_get(array, 2, &raw mut value, &raw mut attributes));
            assert_eq!(value, Value::from_i32(3).0);
            assert_eq!(
                attributes,
                Attribute::WRITABLE | Attribute::ENUMERABLE | Attribute::CONFIGURABLE
            );
            assert!(!js_array_indexed_get(array, 3, &raw mut value, core::ptr::null_mut()));
            assert_eq!(js_array_indexed_take_first(array), Value::from_i32(1).0);
            assert_eq!(js_array_indexed_array_like_size(array), 2);
        }
        let array = Value::from_object(
            // SAFETY: As above.
            unsafe { cell_from_abi::<JSObject>(array) },
        );
        realm
            .global_object()
            .define_direct_property(&vm, &key("array"), array, DEFAULT_ATTRIBUTES);
        assert_eq!(
            utf8(run_script(&vm, realm, "array.join() + ' ' + array.length").expect("runs")),
            "2,3 2"
        );

        // SAFETY: As above.
        unsafe {
            let empty = js_array_create(
                vm_into_abi(&vm),
                cell_into_abi::<JSRealm>(realm),
                4,
                core::ptr::null_mut(),
            );
            let empty = cell_into_abi::<JSObject>(cell_of_payload::<JSObject>(empty));
            assert_eq!(js_array_indexed_array_like_size(empty), 4);
            let mut hole = 0;
            assert!(!js_array_indexed_get(empty, 0, &raw mut hole, core::ptr::null_mut()));
            let too_long = js_array_create(
                vm_into_abi(&vm),
                cell_into_abi::<JSRealm>(realm),
                1 << 32,
                core::ptr::null_mut(),
            );
            assert_eq!(too_long.variant, JS_COMPLETION_THROW);
        }
    }
}
