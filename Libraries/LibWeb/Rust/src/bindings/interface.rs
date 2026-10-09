/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! What the generated bindings of an interface do with its wrappers, its interface object and its interface prototype
//! object, https://webidl.spec.whatwg.org/#es-interfaces
//!
//! A wrapper of an interface implemented in Rust is a platform object of the interface's wrapper class, which LibWeb's
//! C++ bindings define. Its host data holds the cell of its implementation, and its wrappable slot stays empty, so the
//! C++ bindings treat it like a platform object without a wrappable.

use core::ptr::NonNull;

use ak::Utf16FlyString;
use libjs_embedding::abi::{
    JS_ATTRIBUTE_CONFIGURABLE, JS_ATTRIBUTE_ENUMERABLE, JS_ATTRIBUTE_WRITABLE, JS_INTRINSIC_OBJECT_PROTOTYPE,
    JS_WELL_KNOWN_SYMBOL_TO_STRING_TAG, JSHostClass, JSNativeFunction,
};
use libjs_embedding::completion::ThrowCompletionOr;
use libjs_embedding::error::throw_type_error;
use libjs_embedding::gc::{CellType, Gc, Trace};
use libjs_embedding::host_object;
use libjs_embedding::object::Object;
use libjs_embedding::property_key::PropertyKey;
use libjs_embedding::realm::Realm;
use libjs_embedding::string;
use libjs_embedding::value::{Value, check_cell_encoding_against_libgc};
use libjs_embedding::vm::Vm;

use crate::bindings::CallContext;
use crate::bindings::messages::not_an_object_of_type;
use crate::bindings::services::web_bindings_ensure_web_prototype;

/// The object an attribute or an operation of an interface runs on, if it is a wrapper of the interface, with the
/// object's realm. A nullish this value stands for the global object of the current realm.
pub(crate) fn this_object(
    vm: Vm,
    wrapper_class: &'static JSHostClass,
    interface_name: &str,
) -> ThrowCompletionOr<(Object, Realm)> {
    let mut this_value = vm.running_frame().this_value();
    if this_value.is_nullish() {
        this_value = vm.current_realm().global_object().value();
    }
    match this_value.as_object() {
        Some(object) if object.is_host_object_of_class(wrapper_class) => Ok((object, object.shape_realm())),
        _ => Err(throw_type_error(vm, &not_an_object_of_type(interface_name))),
    }
}

/// The implementation of a wrapper of the interface.
pub(crate) fn implementation_of<T: Trace>(wrapper: Object, cell_type: &'static CellType<T>) -> Gc<T> {
    // SAFETY: The wrapper is a host object of the interface's wrapper class, whose host data the bindings set to a
    //         cell of the interface's implementation.
    unsafe { cell_type.downcast(wrapper.host_data()) }.expect("a wrapper holds the cell of its implementation")
}

/// The interface prototype object of the interface in the realm.
pub(crate) fn interface_prototype_object(realm: Realm, interface_name: u16) -> Object {
    // SAFETY: The realm is live, and the interface is implemented in Rust.
    let prototype = unsafe { web_bindings_ensure_web_prototype(realm.as_raw(), interface_name) };
    // SAFETY: LibWeb returns the live prototype.
    unsafe { Object::from_raw(prototype) }.expect("the realm has the interface prototype object")
}

/// The prototype of an object that newTarget constructs, steps 3.2 and 3.3 of internally creating a new object
/// implementing the interface, https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
pub(crate) fn prototype_from_new_target(vm: Vm, new_target: Object, interface_name: u16) -> ThrowCompletionOr<Object> {
    // 3.2. Let prototype be ? Get(newTarget, "prototype").
    let key_name = Utf16FlyString::from_utf8("prototype");
    let prototype = new_target.get(vm, &PropertyKey::from_fly_string(&key_name))?;

    // 3.3. If Type(prototype) is not Object, then:
    if let Some(prototype) = prototype.as_object() {
        return Ok(prototype);
    }
    // 1. Let targetRealm be ? GetFunctionRealm(newTarget).
    let target_realm = Realm::of_function(vm, new_target)?;
    // 2. Set prototype to the interface prototype object for interface in targetRealm.
    Ok(interface_prototype_object(target_realm, interface_name))
}

/// A new wrapper of the interface in the realm, whose host data holds the implementation.
pub(crate) fn create_wrapper<T>(
    vm: Vm,
    realm: Realm,
    wrapper_class: &'static JSHostClass,
    prototype: Object,
    implementation: Gc<T>,
) -> Object {
    // SAFETY: The wrapper class's hooks expect no wrappable, and the host data is a live cell.
    unsafe {
        host_object::create(
            vm,
            realm,
            wrapper_class,
            Some(prototype),
            core::ptr::null_mut(),
            implementation.as_cell(),
        )
    }
}

/// Unwraps the raw arguments that LibWeb's C++ bindings pass the entry points that initialize the interface object and
/// the interface prototype object, which run before any binding of the interface.
///
/// # Safety
///
/// The pointers must be the live VM, a live realm and a live object.
pub(crate) unsafe fn entry_point_arguments(
    vm: *mut libjs_embedding::abi::JSVM,
    realm: *mut libjs_embedding::abi::JSRealm,
    object: *mut libjs_embedding::abi::JSObject,
) -> (Vm, Realm, Object) {
    check_cell_encoding_against_libgc();
    // SAFETY: The caller guarantees that the pointers are live.
    unsafe {
        (
            Vm::from_raw(vm),
            Realm::from_non_null(NonNull::new(realm).expect("C++ passes a realm")),
            Object::from_raw(object).expect("C++ passes an object"),
        )
    }
}

fn with_property_key<R>(name: &str, f: impl FnOnce(&PropertyKey<'_>) -> R) -> R {
    let name = Utf16FlyString::from_utf8(name);
    f(&PropertyKey::from_fly_string(&name))
}

/// Gives the interface prototype object its [[Prototype]], %Object.prototype%. The Rust bindings do not handle
/// interfaces that inherit from another interface yet.
pub(crate) fn set_interface_prototype_object_prototype(vm: Vm, realm: Realm, prototype: Object) {
    prototype.set_prototype(vm, Some(realm.intrinsic(vm, JS_INTRINSIC_OBJECT_PROTOTYPE)));
}

/// Defines a regular attribute on the interface prototype object, https://webidl.spec.whatwg.org/#es-attributes
pub(crate) fn define_attribute(
    vm: Vm,
    realm: Realm,
    prototype: Object,
    name: &str,
    getter: JSNativeFunction,
    setter: JSNativeFunction,
) {
    with_property_key(name, |key| {
        prototype.define_native_accessor(
            vm,
            realm,
            key,
            getter,
            setter,
            JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE,
        );
    });
}

/// Defines a regular operation on the interface prototype object, https://webidl.spec.whatwg.org/#es-operations
pub(crate) fn define_operation(
    vm: Vm,
    realm: Realm,
    prototype: Object,
    name: &str,
    behaviour: JSNativeFunction,
    length: i32,
) {
    with_property_key(name, |key| {
        prototype.define_native_function(
            vm,
            realm,
            key,
            behaviour,
            length,
            JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE,
        );
    });
}

/// Defines @@toStringTag on the interface prototype object, https://webidl.spec.whatwg.org/#es-interface-prototype-object
pub(crate) fn define_to_string_tag(vm: Vm, prototype: Object, name: &str) {
    let key = PropertyKey::well_known_symbol(vm, JS_WELL_KNOWN_SYMBOL_TO_STRING_TAG);
    prototype.define_direct_property(
        vm,
        &key,
        string::from_ascii(vm, name.as_bytes()),
        JS_ATTRIBUTE_CONFIGURABLE,
    );
}

/// Defines the "length", "name" and "prototype" properties of the interface object,
/// https://webidl.spec.whatwg.org/#es-interface-call
pub(crate) fn define_interface_object_properties(
    vm: Vm,
    realm: Realm,
    constructor: Object,
    name: &str,
    length: i32,
    interface_name: u16,
) {
    with_property_key("length", |key| {
        constructor.define_direct_property(vm, key, Value::from_f64(f64::from(length)), JS_ATTRIBUTE_CONFIGURABLE);
    });
    with_property_key("name", |key| {
        constructor.define_direct_property(
            vm,
            key,
            string::from_ascii(vm, name.as_bytes()),
            JS_ATTRIBUTE_CONFIGURABLE,
        );
    });
    let prototype = interface_prototype_object(realm, interface_name);
    with_property_key("prototype", |key| {
        constructor.define_direct_property(vm, key, prototype.value(), 0);
    });
}

/// A new object for a dictionary value, https://webidl.spec.whatwg.org/#js-dictionary
pub(crate) fn create_dictionary_object(cx: &CallContext) -> Object {
    // 1. Let O be OrdinaryObjectCreate(%Object.prototype%).
    let object_prototype = cx.realm().intrinsic(cx.vm(), JS_INTRINSIC_OBJECT_PROTOTYPE);
    Object::create(cx.vm(), cx.realm(), Some(object_prototype))
}

/// Stores a member of a dictionary value in its object, step 3.2.3 of https://webidl.spec.whatwg.org/#js-dictionary
pub(crate) fn set_dictionary_member(
    cx: &CallContext,
    object: Object,
    name: &str,
    value: Value,
) -> ThrowCompletionOr<()> {
    // 3. Perform ! CreateDataPropertyOrThrow(O, key, value).
    with_property_key(name, |key| object.create_data_property(cx.vm(), key, value))?;
    Ok(())
}
