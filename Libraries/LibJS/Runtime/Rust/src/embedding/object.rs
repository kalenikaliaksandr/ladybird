/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Objects, their properties and their internal methods.
//!
//! The exported functions of this file, and of the files that share its conversions, run on the thread that owns the
//! VM, and trust their arguments: `vm` is the embedder's VM, whose storage starts with the runtime's; objects, realms
//! and environments are live cells of its heap; a JSPropertyKey pointer points to a key that outlives the call; and
//! out parameters point to writable storage. Cells these functions return are not rooted: like every engine value,
//! they stay alive while the stack or a root reaches them.
//!
//! Completions follow the conventions of abi_types.rs, which are those of LibJS/HostObjectABI.h.

#![allow(
    clippy::missing_safety_doc,
    reason = "the module documentation states the contract every exported function shares"
)]

use core::ffi::c_void;

use crate::embedding::abi_types::{
    JSRealm, JSSymbol, JSUtf16View, append_to_value_sink, cell_from_abi, cell_into_abi, completion_into_abi,
    object_into_abi, optional_cell_from_abi, optional_object_into_abi, property_key_from_abi, vm_from_abi,
};
use crate::embedding::collections::{JSPropertyKind, property_kind_from_abi};
use crate::embedding::function::{JSNativeFunction, raw_native_function_from_abi};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{
    JS_PD_CONFIGURABLE, JS_PD_ENUMERABLE, JS_PD_HAS_CONFIGURABLE, JS_PD_HAS_ENUMERABLE, JS_PD_HAS_GET,
    JS_PD_HAS_PROPERTY_OFFSET, JS_PD_HAS_SET, JS_PD_HAS_VALUE, JS_PD_HAS_WRITABLE, JS_PD_PRESENT, JS_PD_WRITABLE,
    JS_PROPERTY_LOOKUP_PHASE_OWN_PROPERTY, JS_PROPERTY_LOOKUP_PHASE_PROTOTYPE_CHAIN, JSCompletion, JSGetCacheMetadata,
    JSObject, JSPropertyDescriptor, JSPropertyKey, JSSetCacheMetadata, JSVM, JSValue, JSValueSink,
};
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::function_object::FunctionObject;
use crate::runtime::object::{
    CacheableGetPropertyMetadata, CacheableSetPropertyMetadata, HostIntrinsicAccessor, IntegrityLevel, Object,
    PropertyLookupPhase, ShouldThrowExceptions,
};
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::property_descriptor::PropertyDescriptor;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;

// Conversions between the C types and the runtime's own, which the other files of the embedding module share.

const _: () = assert!(size_of::<JSValue>() == size_of::<Value>());

/// # Safety
///
/// `function` must be a live function object.
pub(crate) unsafe fn function_from_abi(function: *mut JSObject) -> Gc<FunctionObject> {
    // SAFETY: The caller passes a live object.
    unsafe { cell_from_abi::<JSObject>(function) }
        .downcast::<FunctionObject>()
        .expect("the object is a function")
}

/// # Safety
///
/// `function` must be null or a live function object.
pub(crate) unsafe fn optional_function_from_abi(function: *mut JSObject) -> Option<Gc<FunctionObject>> {
    // SAFETY: The caller passes null or a live object.
    unsafe { optional_cell_from_abi::<JSObject>(function) }
        .map(|function| function.downcast::<FunctionObject>().expect("the object is a function"))
}

/// # Safety
///
/// `values` must point to `count` values, or `count` must be 0.
pub(crate) unsafe fn values_from_abi<'values>(values: *const JSValue, count: usize) -> &'values [Value] {
    if count == 0 {
        return &[];
    }
    // SAFETY: Value is a transparent JSValue, and the caller passes `count` of them.
    unsafe { core::slice::from_raw_parts(values.cast::<Value>(), count) }
}

const ABSENT_PROPERTY_DESCRIPTOR: JSPropertyDescriptor = JSPropertyDescriptor {
    value: 0,
    get: core::ptr::null_mut(),
    set: core::ptr::null_mut(),
    property_offset: 0,
    flags: 0,
};

pub(crate) fn property_descriptor_to_abi(descriptor: Option<&PropertyDescriptor>) -> JSPropertyDescriptor {
    let mut abi_descriptor = ABSENT_PROPERTY_DESCRIPTOR;
    let Some(descriptor) = descriptor else {
        return abi_descriptor;
    };
    let mut flags = JS_PD_PRESENT;
    if let Some(value) = descriptor.value {
        flags |= JS_PD_HAS_VALUE;
        abi_descriptor.value = value.0;
    }
    if let Some(getter) = descriptor.get {
        flags |= JS_PD_HAS_GET;
        abi_descriptor.get = optional_object_into_abi(getter);
    }
    if let Some(setter) = descriptor.set {
        flags |= JS_PD_HAS_SET;
        abi_descriptor.set = optional_object_into_abi(setter);
    }
    let boolean_field_flags = |field: Option<bool>, has_field_flag: u16, field_is_true_flag: u16| match field {
        Some(true) => has_field_flag | field_is_true_flag,
        Some(false) => has_field_flag,
        None => 0,
    };
    flags |= boolean_field_flags(descriptor.writable, JS_PD_HAS_WRITABLE, JS_PD_WRITABLE);
    flags |= boolean_field_flags(descriptor.enumerable, JS_PD_HAS_ENUMERABLE, JS_PD_ENUMERABLE);
    flags |= boolean_field_flags(descriptor.configurable, JS_PD_HAS_CONFIGURABLE, JS_PD_CONFIGURABLE);
    if let Some(property_offset) = descriptor.property_offset {
        flags |= JS_PD_HAS_PROPERTY_OFFSET;
        abi_descriptor.property_offset = property_offset;
    }
    abi_descriptor.flags = flags;
    abi_descriptor
}

/// # Safety
///
/// The getter and setter of the descriptor must be null or live function objects.
pub(crate) unsafe fn property_descriptor_from_abi(abi_descriptor: &JSPropertyDescriptor) -> Option<PropertyDescriptor> {
    let has = |flag: u16| abi_descriptor.flags & flag != 0;
    if !has(JS_PD_PRESENT) {
        return None;
    }
    // SAFETY: The caller passes null or live functions as the getter and setter.
    let accessor_function = |function: *mut JSObject| unsafe { optional_function_from_abi(function) };
    Some(PropertyDescriptor {
        value: has(JS_PD_HAS_VALUE).then_some(Value(abi_descriptor.value)),
        get: has(JS_PD_HAS_GET).then(|| accessor_function(abi_descriptor.get)),
        set: has(JS_PD_HAS_SET).then(|| accessor_function(abi_descriptor.set)),
        writable: has(JS_PD_HAS_WRITABLE).then_some(has(JS_PD_WRITABLE)),
        enumerable: has(JS_PD_HAS_ENUMERABLE).then_some(has(JS_PD_ENUMERABLE)),
        configurable: has(JS_PD_HAS_CONFIGURABLE).then_some(has(JS_PD_CONFIGURABLE)),
        property_offset: has(JS_PD_HAS_PROPERTY_OFFSET).then_some(abi_descriptor.property_offset),
    })
}

/// # Safety
///
/// `descriptor` must be null or point to a descriptor whose getter and setter are null or live function objects.
unsafe fn optional_property_descriptor_from_abi(descriptor: *const JSPropertyDescriptor) -> Option<PropertyDescriptor> {
    // SAFETY: The caller passes null or a valid descriptor.
    unsafe { descriptor.as_ref() }.and_then(|descriptor| {
        // SAFETY: As above.
        unsafe { property_descriptor_from_abi(descriptor) }
    })
}

pub(crate) fn lookup_phase_from_abi(phase: u8) -> PropertyLookupPhase {
    match phase {
        JS_PROPERTY_LOOKUP_PHASE_OWN_PROPERTY => PropertyLookupPhase::OwnProperty,
        JS_PROPERTY_LOOKUP_PHASE_PROTOTYPE_CHAIN => PropertyLookupPhase::PrototypeChain,
        phase => panic!("{phase} is not a property lookup phase"),
    }
}

/// # Safety
///
/// `metadata` must be null or point to inline cache metadata the runtime handed out for a [[Get]] still running.
unsafe fn get_cache_metadata_from_abi<'metadata>(
    metadata: *mut JSGetCacheMetadata,
) -> Option<&'metadata mut CacheableGetPropertyMetadata> {
    // SAFETY: The runtime passes its own metadata through the embedder unchanged, as the caller guarantees.
    unsafe { metadata.cast::<CacheableGetPropertyMetadata>().as_mut() }
}

/// # Safety
///
/// `metadata` must be null or point to inline cache metadata the runtime handed out for a [[Set]] still running.
pub(crate) unsafe fn set_cache_metadata_from_abi<'metadata>(
    metadata: *mut JSSetCacheMetadata,
) -> Option<&'metadata mut CacheableSetPropertyMetadata> {
    // SAFETY: The runtime passes its own metadata through the embedder unchanged, as the caller guarantees.
    unsafe { metadata.cast::<CacheableSetPropertyMetadata>().as_mut() }
}

/// Appends every key of a list of property keys to the embedder's sink. The list stays rooted while the sink runs,
/// and no borrow of the runtime's state is held across a call into the sink, which may call back into the VM.
///
/// # Safety
///
/// `sink` must point to a sink with an append function.
unsafe fn append_keys_to_sink(
    keys: ThrowCompletionOr<crate::gc::root::MarkedVec<'_, Value>>,
    sink: *mut JSValueSink,
) -> JSCompletion {
    let keys = match keys {
        Ok(keys) => keys,
        Err(throw) => return completion_into_abi::<()>(Err(throw)),
    };
    // SAFETY: The caller passes a valid sink.
    let sink = unsafe { &*sink };
    for index in 0..keys.len() {
        append_to_value_sink(sink, keys.get(index).expect("the index is in bounds"));
    }
    completion_into_abi(Ok(()))
}

/// The bits of the attributes byte of a property, JS::Attribute.
pub const JS_ATTRIBUTE_WRITABLE: u8 = 1 << 0;
pub const JS_ATTRIBUTE_ENUMERABLE: u8 = 1 << 1;
pub const JS_ATTRIBUTE_CONFIGURABLE: u8 = 1 << 2;

const _: () = assert!(JS_ATTRIBUTE_WRITABLE == Attribute::WRITABLE);
const _: () = assert!(JS_ATTRIBUTE_ENUMERABLE == Attribute::ENUMERABLE);
const _: () = assert!(JS_ATTRIBUTE_CONFIGURABLE == Attribute::CONFIGURABLE);

pub const JS_INTEGRITY_LEVEL_SEALED: u8 = 0;
pub const JS_INTEGRITY_LEVEL_FROZEN: u8 = 1;

fn integrity_level_from_abi(level: u8) -> IntegrityLevel {
    match level {
        JS_INTEGRITY_LEVEL_SEALED => IntegrityLevel::Sealed,
        JS_INTEGRITY_LEVEL_FROZEN => IntegrityLevel::Frozen,
        level => panic!("{level} is not an integrity level"),
    }
}

/// Computes the value of a property that an intrinsic accessor defines, when it is first read: given the realm of the
/// object, returns the value, which the runtime then stores in the property.
pub type JSIntrinsicAccessor = Option<unsafe extern "C" fn(realm: *mut JSRealm) -> JSValue>;

pub fn call_host_intrinsic_accessor(accessor: HostIntrinsicAccessor, realm: Gc<Realm>) -> Value {
    // SAFETY: The embedder defined the accessor for a property of an object of this runtime, and computes its value in
    //         the realm of that object.
    Value(unsafe { accessor(cell_into_abi::<JSRealm>(realm)) })
}

// Creating objects

/// OrdinaryObjectCreate: a new ordinary object of the realm whose [[Prototype]] is `prototype`, which may be null.
/// Returns an unrooted object. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_create(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    prototype: *mut JSObject,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    object_into_abi(Object::create(vm, realm, prototype))
}

// Operations on objects

/// Get(O, P). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_get(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.get(vm, key))
}

/// Set(O, P, V, Throw), with an unused payload. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_set(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
    throw_exceptions: bool,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    let throw_exceptions = if throw_exceptions {
        ShouldThrowExceptions::Yes
    } else {
        ShouldThrowExceptions::No
    };
    completion_into_abi(object.set(vm, key, Value(value), throw_exceptions))
}

/// HasProperty(O, P). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_has_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.has_property(vm, key))
}

/// HasOwnProperty(O, P). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_has_own_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.has_own_property(vm, key))
}

/// DeletePropertyOrThrow(O, P), with an unused payload. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_delete_property_or_throw(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.delete_property_or_throw(vm, key))
}

/// DefinePropertyOrThrow(O, P, desc), with an unused payload. The descriptor must be present, and the runtime writes
/// it back, which reports the offset of a new property. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_property_or_throw(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    descriptor: *mut JSPropertyDescriptor,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    // SAFETY: As above, the descriptor is valid.
    let mut property_descriptor =
        unsafe { property_descriptor_from_abi(&*descriptor) }.expect("the descriptor is present");
    let result = object.define_property_or_throw(vm, key, &mut property_descriptor);
    // SAFETY: As above.
    unsafe { descriptor.write(property_descriptor_to_abi(Some(&property_descriptor))) };
    completion_into_abi(result)
}

/// CreateDataProperty(O, P, V). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_create_data_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.create_data_property(vm, key, Value(value), None, None))
}

/// CreateDataPropertyOrThrow(O, P, V). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_create_data_property_or_throw(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.create_data_property_or_throw(vm, key, Value(value)))
}

/// IsExtensible(O). Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_is_extensible(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.is_extensible(vm))
}

/// SetIntegrityLevel(O, level), where the level is JS_INTEGRITY_LEVEL_SEALED or JS_INTEGRITY_LEVEL_FROZEN. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_set_integrity_level(
    vm: *mut JSVM,
    object: *mut JSObject,
    level: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.set_integrity_level(vm, integrity_level_from_abi(level)))
}

/// TestIntegrityLevel(O, level), where the level is JS_INTEGRITY_LEVEL_SEALED or JS_INTEGRITY_LEVEL_FROZEN. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_test_integrity_level(
    vm: *mut JSVM,
    object: *mut JSObject,
    level: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.test_integrity_level(vm, integrity_level_from_abi(level)))
}

/// SetImmutablePrototype(O, V), whose payload is whether the prototype is now `prototype`, which may be null. An
/// exotic object with an immutable prototype, such as Location, calls it from its set_prototype_of hook. Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_set_immutable_prototype(
    vm: *mut JSVM,
    object: *mut JSObject,
    prototype: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    completion_into_abi(object.set_immutable_prototype(vm, prototype))
}

/// EnumerableOwnProperties(O, kind), with kind a JS_PROPERTY_KIND_* value: appends to the sink, in order, the key, the
/// value or a [key, value] array of each enumerable own string-keyed property, with an unused payload. The getters of
/// the properties run, and the sink may call back into the VM. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_enumerable_own_property_names(
    vm: *mut JSVM,
    object: *mut JSObject,
    kind: JSPropertyKind,
    sink: *mut JSValueSink,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    // SAFETY: As above, the sink is valid.
    unsafe {
        append_keys_to_sink(
            object.enumerable_own_property_names(vm, property_kind_from_abi(kind)),
            sink,
        )
    }
}

/// Called with the context it came with and each key EnumerateObjectProperties produces, a String value. It may run
/// JavaScript, and returns true to stop the enumeration.
pub type JSPropertyEnumerationCallback = Option<unsafe extern "C" fn(context: *mut c_void, key: JSValue) -> bool>;

/// EnumerateObjectProperties(O), the keys a for-in loop visits: calls `callback` with each string key of an enumerable
/// property of the object and of its prototype chain, each at most once, until the callback returns true. The payload
/// is whether the callback stopped the enumeration. A throw comes from an internal method of an object the
/// enumeration visits. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_enumerate_object_properties(
    vm: *mut JSVM,
    object: *mut JSObject,
    callback: JSPropertyEnumerationCallback,
    context: *mut c_void,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    let callback = callback.expect("the embedder passes a callback");
    let stopped = object.enumerate_object_properties(vm, |key| {
        // SAFETY: The embedder's callback takes keys with the context it came with.
        unsafe { callback(context, key.0) }.then_some(())
    });
    completion_into_abi(stopped.map(|stopped| stopped.is_some()))
}

/// The value of the property of the key, looked up in the storage of the object and of its prototype chain without
/// running any internal method or getter, or undefined if there is none. For an accessor property, it is the
/// accessor itself, a cell of kind JS_LAYOUT_CELL_KIND_ACCESSOR. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_get_without_side_effects(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSValue {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    object.get_without_side_effects(vm, key).0
}

// Defining properties directly in the object's storage, as built-in objects do

/// Stores a data property in the object's own storage, replacing any property of the key, without running any
/// internal method. The attributes are JS_ATTRIBUTE_* bits. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_direct_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
    attributes: u8,
) {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    object.define_direct_property(vm, key, Value(value), PropertyAttributes::new(attributes));
}

/// Stores an accessor property in the object's own storage. Either function may be null; when the key already holds an
/// accessor, a non-null function replaces its getter or setter and the attributes are kept. Borrows the key. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_direct_accessor(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    getter: *mut JSObject,
    setter: *mut JSObject,
    attributes: u8,
) {
    // SAFETY: See the module documentation.
    let (vm, object, key, getter, setter) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            optional_function_from_abi(getter),
            optional_function_from_abi(setter),
        )
    };
    object.define_direct_accessor(vm, key, getter, setter, PropertyAttributes::new(attributes));
}

/// js_object_define_direct_accessor() for an accessor whose getter result the runtime caches in the object until
/// js_object_clear_cached_accessor_value(). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_direct_cached_accessor(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    getter: *mut JSObject,
    setter: *mut JSObject,
    attributes: u8,
) {
    // SAFETY: See the module documentation.
    let (vm, object, key, getter, setter) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            optional_function_from_abi(getter),
            optional_function_from_abi(setter),
        )
    };
    object.define_direct_cached_accessor(vm, key, getter, setter, PropertyAttributes::new(attributes));
}

/// Forgets the getter result that a cached accessor of the key keeps, if any. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_clear_cached_accessor_value(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    object.clear_cached_accessor_value(vm, key);
}

/// Stores `value` in the object under a private symbol from js_symbol_create_private(), as an engine-private property,
/// which no internal method, and so no script, ever sees. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_set_engine_private_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    private_symbol: *mut JSSymbol,
    value: JSValue,
) {
    // SAFETY: See the module documentation; the symbol is a live symbol.
    let (vm, object, private_symbol) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            cell_from_abi::<JSSymbol>(private_symbol),
        )
    };
    object.set_engine_private_property(vm, private_symbol, Value(value));
}

/// Defines a method whose behaviour is a raw native function, as a direct property of the key with the attributes.
/// The function's [[Realm]] is the realm, and its "name" property is the key. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_native_function(
    vm: *mut JSVM,
    object: *mut JSObject,
    realm: *mut JSRealm,
    key: *const JSPropertyKey,
    behaviour: JSNativeFunction,
    length: i32,
    attributes: u8,
) {
    // SAFETY: See the module documentation.
    let (vm, object, realm, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            cell_from_abi::<JSRealm>(realm),
            property_key_from_abi(key),
        )
    };
    object.define_native_function(
        vm,
        realm,
        key,
        raw_native_function_from_abi(behaviour),
        length,
        PropertyAttributes::new(attributes),
        None,
    );
}

/// Defines an accessor property whose getter ("get <key>", length 0) and setter ("set <key>", length 1) are raw native
/// functions. Either may be null for a missing function. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_native_accessor(
    vm: *mut JSVM,
    object: *mut JSObject,
    realm: *mut JSRealm,
    key: *const JSPropertyKey,
    getter: JSNativeFunction,
    setter: JSNativeFunction,
    attributes: u8,
) {
    // SAFETY: See the module documentation.
    let (vm, object, realm, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            cell_from_abi::<JSRealm>(realm),
            property_key_from_abi(key),
        )
    };
    object.define_native_accessor(
        vm,
        realm,
        key,
        raw_native_function_from_abi(getter),
        raw_native_function_from_abi(setter),
        PropertyAttributes::new(attributes),
    );
}

/// Defines a data property of a string key whose value the accessor computes the first time anything reads it.
/// Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_intrinsic_accessor(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    attributes: u8,
    accessor: JSIntrinsicAccessor,
) {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    object.define_host_intrinsic_accessor(
        vm,
        key,
        PropertyAttributes::new(attributes),
        accessor.expect("the intrinsic accessor is not null"),
    );
}

/// Records a property of a web platform interface that the embedder does not implement, so that reading it can be
/// reported without making it observable to JavaScript. Borrows the name. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_define_unimplemented_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    name: JSUtf16View,
) {
    // SAFETY: See the module documentation.
    let (vm, object, name) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object), name.as_view()) };
    object.define_unimplemented_property(vm, name.to_utf16_fly_string());
}

// The object's shape and storage

/// The object's [[Prototype]] as its shape records it, without running [[GetPrototypeOf]], or null. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_prototype(object: *mut JSObject) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let object = unsafe { cell_from_abi::<JSObject>(object) };
    optional_object_into_abi(object.prototype())
}

/// Sets the [[Prototype]] the object's shape records, without running [[SetPrototypeOf]]. The prototype may be null.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_set_prototype(vm: *mut JSVM, object: *mut JSObject, prototype: *mut JSObject) {
    // SAFETY: See the module documentation.
    let (vm, object, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    object.set_prototype(vm, prototype);
}

/// Whether the object's own storage has a property of the key, without running any internal method. Borrows the key.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_storage_has(object: *mut JSObject, key: *const JSPropertyKey) -> bool {
    // SAFETY: See the module documentation.
    let (object, key) = unsafe { (cell_from_abi::<JSObject>(object), property_key_from_abi(key)) };
    object.storage_has(key)
}

/// Removes the property of the key from the object's own storage, which must have it, without running any internal
/// method. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_storage_delete(vm: *mut JSVM, object: *mut JSObject, key: *const JSPropertyKey) {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    object.storage_delete(vm, key);
}

/// Gives the object a shape of its own suited to an object that serves as a prototype, unless it has one. Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_convert_to_prototype_if_needed(vm: *mut JSVM, object: *mut JSObject) {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    object.convert_to_prototype_if_needed(vm);
}

/// Gives the object a dictionary shape of its own, which no inline cache has seen, so that every cached lookup of its
/// properties misses and looks again. An embedder calls it when a property that its hooks report appears without the
/// object's shape changing, as a document's named properties do. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_invalidate_property_lookup_caches(vm: *mut JSVM, object: *mut JSObject) {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    object.invalidate_property_lookup_caches(vm);
}

/// Lets new own properties of the object be added through the inline caches again, which its host class's
/// JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY flag kept from it. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_clear_requires_slow_add_own_property(object: *mut JSObject) {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.clear_requires_slow_add_own_property();
}

/// Whether the object is an arguments object with a parameter map, as a mapped arguments object is. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_has_parameter_map(object: *mut JSObject) -> bool {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.has_parameter_map()
}

/// The id of the object's class, one of Layout.h's JS_LAYOUT_CLASS_ID_* values. A class derived at run time, as each
/// host class is, has the id of the class it extends. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_class_id(object: *mut JSObject) -> u16 {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.class().id as u16
}

/// The name of the object's class, which for an object of a host class is the name in its JSHostClass: static UTF-8,
/// not null-terminated, whose length goes to `out_length`. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_class_name(object: *mut JSObject, out_length: *mut usize) -> *const u8 {
    // SAFETY: See the module documentation.
    let class_name = unsafe { cell_from_abi::<JSObject>(object) }.class().class_name();
    // SAFETY: As above, the out parameter is writable.
    unsafe { out_length.write(class_name.len()) };
    class_name.as_ptr()
}

/// Whether the object's class is the class of the id (a JS_LAYOUT_CLASS_ID_* value) or extends it. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_is_subclass_of(object: *mut JSObject, class_id: u16) -> bool {
    // SAFETY: See the module documentation.
    let mut class = Some(unsafe { cell_from_abi::<JSObject>(object) }.class());
    while let Some(current) = class {
        if current.id as u16 == class_id {
            return true;
        }
        class = current.parent;
    }
    false
}

// The engine queries that inline caches and enumeration ask of an object's class

/// Whether inline caches may remember that a key is missing from the object. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_is_cacheable_for_property_absence(object: *mut JSObject) -> bool {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.is_cacheable_for_property_absence()
}

/// Whether inline caches may remember a property found in the object's prototype chain, which a host class may answer
/// through a hook that runs JavaScript. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_is_cacheable_for_inherited_property(object: *mut JSObject) -> bool {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.is_cacheable_for_inherited_property()
}

/// Whether enumeration may read the object's own keys, attributes and values straight from its shape and storage.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_eligible_for_own_property_enumeration_fast_path(object: *mut JSObject) -> bool {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSObject>(object) }.eligible_for_own_property_enumeration_fast_path()
}

// The internal methods, which dispatch through the object's class like a call through a C++ Object pointer

/// [[GetPrototypeOf]] ( ), whose payload is the prototype or null. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_get_prototype_of(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.internal_get_prototype_of(vm))
}

/// [[SetPrototypeOf]] ( V ), where V may be null. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_set_prototype_of(
    vm: *mut JSVM,
    object: *mut JSObject,
    prototype: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    completion_into_abi(object.internal_set_prototype_of(vm, prototype))
}

/// [[IsExtensible]] ( ). Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_is_extensible(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.internal_is_extensible(vm))
}

/// [[PreventExtensions]] ( ). Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_prevent_extensions(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.internal_prevent_extensions(vm))
}

/// [[GetOwnProperty]] ( P ), with an unused payload. The runtime writes the property's descriptor, with its storage
/// offset when it has one, or a zeroed descriptor for an absent property to `out`. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_get_own_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    out: *mut JSPropertyDescriptor,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    let descriptor = object.internal_get_own_property(vm, key);
    // SAFETY: As above, `out` is writable.
    unsafe { write_own_property_descriptor(descriptor, out) }
}

/// [[DefineOwnProperty]] ( P, Desc ). The descriptor must be present, and the runtime writes it back, which reports
/// the storage offset of a new property. A non-null `precomputed_get_own_property` is the result of a
/// [[GetOwnProperty]] the caller already ran, absent when JS_PD_PRESENT is clear. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_define_own_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    descriptor: *mut JSPropertyDescriptor,
    precomputed_get_own_property: *const JSPropertyDescriptor,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    unsafe {
        define_own_property_from_abi(
            vm,
            object,
            key,
            descriptor,
            precomputed_get_own_property,
            Object::internal_define_own_property,
        )
    }
}

/// [[HasProperty]] ( P ). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_has_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.internal_has_property(vm, key))
}

/// [[Get]] ( P, Receiver ). `cache_metadata` is null or the metadata a [[Get]] hook received, and the phase a
/// JS_PROPERTY_LOOKUP_PHASE_* value. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_get(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    receiver: JSValue,
    cache_metadata: *mut JSGetCacheMetadata,
    phase: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, cache_metadata) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            get_cache_metadata_from_abi(cache_metadata),
        )
    };
    completion_into_abi(object.internal_get(vm, key, Value(receiver), cache_metadata, lookup_phase_from_abi(phase)))
}

/// [[Get]] ( P, Receiver ) of the object as one found in the prototype chain of the lookup that
/// `metadata_for_caller` (null or the metadata a [[Get]] hook received) belongs to. The runtime fills that metadata
/// only for a hit an inline cache can keep, so an object that forwards its lookups to another one can let them be
/// cached. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_get_as_prototype_of(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    receiver: JSValue,
    metadata_for_caller: *mut JSGetCacheMetadata,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, metadata_for_caller) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            get_cache_metadata_from_abi(metadata_for_caller),
        )
    };
    completion_into_abi(object.internal_get_as_prototype_of(vm, key, Value(receiver), metadata_for_caller))
}

/// [[Set]] ( P, V, Receiver ). `cache_metadata` is null or the metadata a [[Set]] hook received, and the phase a
/// JS_PROPERTY_LOOKUP_PHASE_* value. Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_set(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
    receiver: JSValue,
    cache_metadata: *mut JSSetCacheMetadata,
    phase: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, cache_metadata) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            set_cache_metadata_from_abi(cache_metadata),
        )
    };
    completion_into_abi(object.internal_set(
        vm,
        key,
        Value(value),
        Value(receiver),
        cache_metadata,
        lookup_phase_from_abi(phase),
    ))
}

/// [[Delete]] ( P ). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_delete(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.internal_delete(vm, key))
}

/// [[OwnPropertyKeys]] ( ), with an unused payload. The runtime appends each key, as a string or symbol value, to the
/// sink, which may call back into the VM. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_internal_own_property_keys(
    vm: *mut JSVM,
    object: *mut JSObject,
    keys: *mut JSValueSink,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    // SAFETY: As above, the sink is valid.
    unsafe { append_keys_to_sink(object.internal_own_property_keys(vm), keys) }
}

// The ordinary internal methods, which never dispatch to the object's class, for host classes whose hooks defer to them

/// OrdinaryGetPrototypeOf ( O ), whose payload is the prototype or null. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_get_prototype_of(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.ordinary_get_prototype_of(vm))
}

/// OrdinarySetPrototypeOf ( O, V ), where V may be null. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_set_prototype_of(
    vm: *mut JSVM,
    object: *mut JSObject,
    prototype: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, prototype) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            optional_cell_from_abi::<JSObject>(prototype),
        )
    };
    completion_into_abi(object.ordinary_set_prototype_of(vm, prototype))
}

/// OrdinaryIsExtensible ( O ). Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_is_extensible(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.ordinary_is_extensible(vm))
}

/// OrdinaryPreventExtensions ( O ). Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_prevent_extensions(vm: *mut JSVM, object: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    completion_into_abi(object.ordinary_prevent_extensions(vm))
}

/// OrdinaryGetOwnProperty ( O, P ), which writes to `out` like js_object_internal_get_own_property(). Borrows the key.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_get_own_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    out: *mut JSPropertyDescriptor,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    let descriptor = object.ordinary_get_own_property(vm, key);
    // SAFETY: As above, `out` is writable.
    unsafe { write_own_property_descriptor(descriptor, out) }
}

/// OrdinaryDefineOwnProperty ( O, P, Desc ), which takes its descriptors like
/// js_object_internal_define_own_property(). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_define_own_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    descriptor: *mut JSPropertyDescriptor,
    precomputed_get_own_property: *const JSPropertyDescriptor,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    unsafe {
        define_own_property_from_abi(
            vm,
            object,
            key,
            descriptor,
            precomputed_get_own_property,
            Object::ordinary_define_own_property,
        )
    }
}

/// OrdinaryHasProperty ( O, P ). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_has_property(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.ordinary_has_property(vm, key))
}

/// OrdinaryGet ( O, P, Receiver ), which takes its cache metadata and phase like js_object_internal_get(). Borrows
/// the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_get(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    receiver: JSValue,
    cache_metadata: *mut JSGetCacheMetadata,
    phase: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, cache_metadata) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            get_cache_metadata_from_abi(cache_metadata),
        )
    };
    completion_into_abi(object.ordinary_get(vm, key, Value(receiver), cache_metadata, lookup_phase_from_abi(phase)))
}

/// OrdinarySet ( O, P, V, Receiver ), which takes its cache metadata and phase like js_object_internal_set(). Borrows
/// the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_set(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
    receiver: JSValue,
    cache_metadata: *mut JSSetCacheMetadata,
    phase: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, cache_metadata) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            set_cache_metadata_from_abi(cache_metadata),
        )
    };
    completion_into_abi(object.ordinary_set(
        vm,
        key,
        Value(value),
        Value(receiver),
        cache_metadata,
        lookup_phase_from_abi(phase),
    ))
}

/// OrdinarySetWithOwnDescriptor ( O, P, V, Receiver, ownDesc ), where ownDesc is absent when the pointer is null or
/// JS_PD_PRESENT is clear. Takes its cache metadata and phase like js_object_internal_set(). Borrows the key. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_set_with_own_descriptor(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    value: JSValue,
    receiver: JSValue,
    own_descriptor: *const JSPropertyDescriptor,
    cache_metadata: *mut JSSetCacheMetadata,
    phase: u8,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key, own_descriptor, cache_metadata) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
            optional_property_descriptor_from_abi(own_descriptor),
            set_cache_metadata_from_abi(cache_metadata),
        )
    };
    completion_into_abi(object.ordinary_set_with_own_descriptor(
        vm,
        key,
        Value(value),
        Value(receiver),
        own_descriptor,
        cache_metadata,
        lookup_phase_from_abi(phase),
    ))
}

/// OrdinaryDelete ( O, P ). Borrows the key. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_delete(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    completion_into_abi(object.ordinary_delete(vm, key))
}

/// OrdinaryOwnPropertyKeys ( O ), which appends to the sink like js_object_internal_own_property_keys(). Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_object_ordinary_own_property_keys(
    vm: *mut JSVM,
    object: *mut JSObject,
    keys: *mut JSValueSink,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, object) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(object)) };
    // SAFETY: As above, the sink is valid.
    unsafe { append_keys_to_sink(object.ordinary_own_property_keys(vm), keys) }
}

/// # Safety
///
/// `out` must be writable.
unsafe fn write_own_property_descriptor(
    descriptor: ThrowCompletionOr<Option<PropertyDescriptor>>,
    out: *mut JSPropertyDescriptor,
) -> JSCompletion {
    match descriptor {
        Ok(descriptor) => {
            // SAFETY: The caller passes writable storage.
            unsafe { out.write(property_descriptor_to_abi(descriptor.as_ref())) };
            completion_into_abi(Ok(()))
        }
        Err(throw) => completion_into_abi::<()>(Err(throw)),
    }
}

type DefineOwnProperty = fn(
    &Object,
    &Vm,
    &PropertyKey,
    &mut PropertyDescriptor,
    Option<&Option<PropertyDescriptor>>,
) -> ThrowCompletionOr<bool>;

/// # Safety
///
/// See the module documentation. `descriptor` must point to a present descriptor.
unsafe fn define_own_property_from_abi(
    vm: *mut JSVM,
    object: *mut JSObject,
    key: *const JSPropertyKey,
    descriptor: *mut JSPropertyDescriptor,
    precomputed_get_own_property: *const JSPropertyDescriptor,
    define_own_property: DefineOwnProperty,
) -> JSCompletion {
    // SAFETY: The caller passes valid arguments.
    let (vm, object, key) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSObject>(object),
            property_key_from_abi(key),
        )
    };
    // SAFETY: As above, the descriptor is valid and present.
    let mut property_descriptor =
        unsafe { property_descriptor_from_abi(&*descriptor) }.expect("the descriptor is present");
    // SAFETY: As above, the precomputed descriptor is null or valid.
    let precomputed_get_own_property = unsafe { precomputed_get_own_property.as_ref() }.map(|precomputed| {
        // SAFETY: As above.
        unsafe { property_descriptor_from_abi(precomputed) }
    });
    let result = define_own_property(
        &object,
        vm,
        key,
        &mut property_descriptor,
        precomputed_get_own_property.as_ref(),
    );
    // SAFETY: As above, the descriptor is writable.
    unsafe { descriptor.write(property_descriptor_to_abi(Some(&property_descriptor))) };
    completion_into_abi(result)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;
    use core::ffi::c_void;

    use super::*;
    use crate::embedding::abi_types::vm_into_abi;
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JSValueSink};
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::{TestRealm, key, own_keys};
    use crate::utilities::initialize_realm;

    std::thread_local! {
        static REALMS_SEEN_BY_THE_ACCESSOR: Cell<Vec<*mut JSRealm>> = const { Cell::new(Vec::new()) };
    }

    unsafe extern "C" fn host_intrinsic_accessor(realm: *mut JSRealm) -> JSValue {
        REALMS_SEEN_BY_THE_ACCESSOR.with(|realms| {
            let mut seen = realms.take();
            seen.push(realm);
            realms.set(seen);
        });
        Value::from_i32(42).0
    }

    #[test]
    fn a_host_intrinsic_accessor_computes_its_property_when_first_read() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = test_realm.object();
        object.define_host_intrinsic_accessor(&vm, &key("answer"), DEFAULT_ATTRIBUTES, host_intrinsic_accessor);
        assert_eq!(own_keys(&vm, &object), "answer");
        assert!(REALMS_SEEN_BY_THE_ACCESSOR.take().is_empty());

        assert_eq!(object.get(&vm, &key("answer")).must(), Value::from_i32(42));
        assert_eq!(object.get(&vm, &key("answer")).must(), Value::from_i32(42));
        assert_eq!(
            REALMS_SEEN_BY_THE_ACCESSOR.take(),
            [cell_into_abi::<JSRealm>(test_realm.realm)]
        );
    }

    fn abi_key(key: &PropertyKey) -> *const JSPropertyKey {
        core::ptr::from_ref(key).cast()
    }

    #[test]
    fn descriptors_round_trip_with_their_property_offsets() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = test_realm.object();
        let vm_pointer = vm_into_abi(&vm);
        let object_pointer = object_into_abi(object);
        let name = key("name");

        let mut descriptor = property_descriptor_to_abi(Some(&PropertyDescriptor {
            value: Some(Value::from_i32(7)),
            writable: Some(false),
            enumerable: Some(true),
            configurable: Some(false),
            ..Default::default()
        }));
        // SAFETY: The arguments are live.
        let defined = unsafe {
            js_object_internal_define_own_property(
                vm_pointer,
                object_pointer,
                abi_key(&name),
                &raw mut descriptor,
                core::ptr::null(),
            )
        };
        assert_eq!((defined.variant, defined.payload), (JS_COMPLETION_NORMAL, 1));
        assert!(descriptor.flags & JS_PD_HAS_PROPERTY_OFFSET != 0);
        let offset_of_new_property = descriptor.property_offset;

        let mut own_property = ABSENT_PROPERTY_DESCRIPTOR;
        // SAFETY: As above.
        let found = unsafe {
            js_object_ordinary_get_own_property(vm_pointer, object_pointer, abi_key(&name), &raw mut own_property)
        };
        assert_eq!(found.variant, JS_COMPLETION_NORMAL);
        // SAFETY: The descriptor came from the runtime.
        let own_property = unsafe { property_descriptor_from_abi(&own_property) }.expect("the property exists");
        assert_eq!(own_property.value, Some(Value::from_i32(7)));
        assert_eq!(
            (
                own_property.writable,
                own_property.enumerable,
                own_property.configurable
            ),
            (Some(false), Some(true), Some(false))
        );
        assert_eq!(own_property.property_offset, Some(offset_of_new_property));

        let mut missing = property_descriptor_to_abi(Some(&PropertyDescriptor::default()));
        // SAFETY: As above.
        unsafe {
            js_object_internal_get_own_property(vm_pointer, object_pointer, abi_key(&key("missing")), &raw mut missing)
        };
        assert_eq!(missing.flags, 0);

        // Redefining a non-configurable property fails without throwing.
        let mut redefinition = property_descriptor_to_abi(Some(&PropertyDescriptor {
            value: Some(Value::from_i32(8)),
            ..Default::default()
        }));
        // SAFETY: As above.
        let redefined = unsafe {
            js_object_internal_define_own_property(
                vm_pointer,
                object_pointer,
                abi_key(&name),
                &raw mut redefinition,
                core::ptr::null(),
            )
        };
        assert_eq!((redefined.variant, redefined.payload), (JS_COMPLETION_NORMAL, 0));
    }

    #[test]
    fn accessor_descriptors_cross_with_their_functions() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let object = test_realm.object();
        object.define_native_accessor(
            &vm,
            test_realm.realm,
            &key("accessor"),
            crate::runtime::native_function::raw_native!(|_| Ok(Value::from_i32(1))),
            None,
            PropertyAttributes::new(Attribute::CONFIGURABLE),
        );
        let descriptor = object
            .internal_get_own_property(&vm, &key("accessor"))
            .must()
            .expect("the accessor exists");
        let abi_descriptor = property_descriptor_to_abi(Some(&descriptor));
        assert!(abi_descriptor.flags & JS_PD_HAS_GET != 0 && !abi_descriptor.get.is_null());
        assert!(abi_descriptor.flags & JS_PD_HAS_SET != 0 && abi_descriptor.set.is_null());
        assert!(abi_descriptor.flags & JS_PD_HAS_VALUE == 0);
        // SAFETY: The descriptor came from the runtime.
        let round_tripped =
            unsafe { property_descriptor_from_abi(&abi_descriptor) }.expect("the descriptor is present");
        assert_eq!(round_tripped.get, descriptor.get);
        assert_eq!(round_tripped.set, Some(None));
        assert_eq!(round_tripped.configurable, Some(true));
        assert_eq!(round_tripped.enumerable, Some(false));
    }

    struct ReenteringKeySink<'vm> {
        vm: &'vm Vm,
        object: Gc<Object>,
        keys_and_values: Vec<(Value, Value)>,
    }

    unsafe extern "C" fn reentering_append(context: *mut c_void, key: JSValue) {
        // SAFETY: The test passes its sink as the context.
        let sink = unsafe { &mut *context.cast::<ReenteringKeySink<'_>>() };
        let property_key = PropertyKey::from_value(sink.vm, Value(key)).must();
        // Reading the property and collecting garbage both run in the middle of the enumeration.
        sink.vm.heap().collect_garbage();
        // SAFETY: The sink's VM and object are live.
        let value = unsafe {
            js_object_get(
                vm_into_abi(sink.vm),
                object_into_abi(sink.object),
                abi_key(&property_key),
            )
        };
        assert_eq!(value.variant, JS_COMPLETION_NORMAL);
        sink.keys_and_values.push((Value(key), Value(value.payload)));
    }

    #[test]
    fn own_property_keys_reach_a_sink_that_calls_back_into_the_vm() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = test_realm.object();
        for (index, name) in ["a", "b", "c"].into_iter().enumerate() {
            object.define_direct_property(&vm, &key(name), Value::from_i32(index as i32), DEFAULT_ATTRIBUTES);
        }
        object.define_direct_property(&vm, &PropertyKey::from(3u32), Value::NULL, DEFAULT_ATTRIBUTES);
        let mut sink_state = ReenteringKeySink {
            vm: &vm,
            object,
            keys_and_values: Vec::new(),
        };
        let mut sink = JSValueSink {
            context: core::ptr::from_mut(&mut sink_state).cast(),
            append: Some(reentering_append),
        };
        // SAFETY: The arguments are live.
        let completion =
            unsafe { js_object_internal_own_property_keys(vm_into_abi(&vm), object_into_abi(object), &raw mut sink) };
        assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
        let keys_and_values = sink_state
            .keys_and_values
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    crate::utf16::Utf16View::of_string(&key.to_utf16_string_without_side_effects()).to_utf8(),
                    crate::utf16::Utf16View::of_string(&value.to_utf16_string_without_side_effects()).to_utf8()
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(keys_and_values, ["3=null", "a=0", "b=1", "c=2"]);
    }

    #[test]
    fn integrity_levels_follow_the_abi_constants() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = test_realm.object();
        object.define_direct_property(&vm, &key("x"), Value::from_i32(1), DEFAULT_ATTRIBUTES);
        let (vm_pointer, object_pointer) = (vm_into_abi(&vm), object_into_abi(object));
        // SAFETY: The arguments are live.
        unsafe {
            assert_eq!(
                js_object_test_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_SEALED).payload,
                0
            );
            assert_eq!(
                js_object_set_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_SEALED).payload,
                1
            );
            assert_eq!(
                js_object_test_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_SEALED).payload,
                1
            );
            assert_eq!(
                js_object_test_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_FROZEN).payload,
                0
            );
            assert_eq!(
                js_object_set_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_FROZEN).payload,
                1
            );
            assert_eq!(
                js_object_test_integrity_level(vm_pointer, object_pointer, JS_INTEGRITY_LEVEL_FROZEN).payload,
                1
            );
            assert_eq!(js_object_is_extensible(vm_pointer, object_pointer).payload, 0);
        }
    }

    #[test]
    fn class_ids_follow_the_parent_chain() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let array = object_into_abi(test_realm.array(&[]));
        let object = object_into_abi(test_realm.object());
        use crate::gc::class_id::ClassId;
        // SAFETY: The objects are live.
        unsafe {
            assert_eq!(js_object_class_id(array), ClassId::Array as u16);
            assert!(js_object_is_subclass_of(array, ClassId::Array as u16));
            assert!(js_object_is_subclass_of(array, ClassId::Object as u16));
            assert!(!js_object_is_subclass_of(object, ClassId::Array as u16));
            assert!(!js_object_is_subclass_of(array, ClassId::FunctionObject as u16));
        }
    }

    fn class_name_of(object: *mut JSObject) -> &'static str {
        let mut length = 0;
        // SAFETY: The object is live, and class names are static.
        unsafe {
            let name = js_object_class_name(object, &raw mut length);
            core::str::from_utf8(core::slice::from_raw_parts(name, length)).expect("class names are UTF-8")
        }
    }

    #[test]
    fn class_names_are_those_of_the_runtimes_classes() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object_of = |source: &str| object_into_abi(run_script(&vm, realm, source).must().as_object());
        assert_eq!(class_name_of(object_of("({})")), "Object");
        assert_eq!(class_name_of(object_of("[]")), "Array");
        assert_eq!(class_name_of(object_of("new Map")), "Map");
        assert_eq!(class_name_of(object_of("(function () {})")), "ECMAScriptFunctionObject");
    }

    #[test]
    fn reading_without_side_effects_follows_the_prototype_chain_and_runs_no_getter() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object = run_script(
            &vm,
            realm,
            "globalThis.getterRuns = 0;
             Object.create({ inherited: 1, get accessor() { getterRuns++; return 2; } }, { own: { value: 3 } })",
        )
        .must()
        .as_object();
        let (vm_pointer, object_pointer) = (vm_into_abi(&vm), object_into_abi(object));
        let read = |name: &str| {
            let name = key(name);
            // SAFETY: The arguments are live.
            Value(unsafe { js_object_get_without_side_effects(vm_pointer, object_pointer, abi_key(&name)) })
        };
        assert_eq!(read("own"), Value::from_i32(3));
        assert_eq!(read("inherited"), Value::from_i32(1));
        assert!(read("accessor").is_accessor());
        assert!(read("missing").is_undefined());
        assert_eq!(run_script(&vm, realm, "getterRuns").must(), Value::from_i32(0));
    }

    struct ReenteringValueSink<'vm> {
        vm: &'vm Vm,
        values: Vec<String>,
    }

    unsafe extern "C" fn collect_value_after_running_a_script(context: *mut c_void, value: JSValue) {
        // SAFETY: The test passes its sink as the context.
        let sink = unsafe { &mut *context.cast::<ReenteringValueSink<'_>>() };
        let realm = sink.vm.current_realm().expect("there is a realm");
        run_script(sink.vm, realm, "globalThis.sinkRuns = (globalThis.sinkRuns ?? 0) + 1").must();
        sink.vm.heap().collect_garbage();
        let string = Value(value).to_primitive_string(sink.vm).must();
        sink.values.push(utf8(Value::from_string(string)));
    }

    #[test]
    fn enumerable_own_properties_reach_the_sink_in_order_with_their_getters_run() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object = run_script(
            &vm,
            realm,
            "const object = { a: 1, get b() { return 'from getter'; }, [Symbol('s')]: 3 };
             Object.defineProperty(object, 'hidden', { value: 4, enumerable: false });
             object",
        )
        .must()
        .as_object();
        let (vm_pointer, object_pointer) = (vm_into_abi(&vm), object_into_abi(object));
        let collect = |kind: JSPropertyKind| {
            let mut sink_state = ReenteringValueSink {
                vm: &vm,
                values: Vec::new(),
            };
            let mut sink = JSValueSink {
                context: core::ptr::from_mut(&mut sink_state).cast(),
                append: Some(collect_value_after_running_a_script),
            };
            // SAFETY: The arguments are live, and the sink collects into a local.
            let completion =
                unsafe { js_object_enumerable_own_property_names(vm_pointer, object_pointer, kind, &raw mut sink) };
            assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
            sink_state.values
        };
        use crate::embedding::collections::{
            JS_PROPERTY_KIND_KEY, JS_PROPERTY_KIND_KEY_AND_VALUE, JS_PROPERTY_KIND_VALUE,
        };
        assert_eq!(collect(JS_PROPERTY_KIND_KEY), ["a", "b"]);
        assert_eq!(collect(JS_PROPERTY_KIND_VALUE), ["1", "from getter"]);
        assert_eq!(collect(JS_PROPERTY_KIND_KEY_AND_VALUE), ["a,1", "b,from getter"]);
        assert_eq!(run_script(&vm, realm, "sinkRuns").must(), Value::from_i32(6));
    }

    struct EnumerationState<'vm> {
        vm: &'vm Vm,
        keys: Vec<String>,
        stop_at: &'static str,
    }

    unsafe extern "C" fn record_key_and_stop_at_the_wanted_one(context: *mut c_void, key: JSValue) -> bool {
        // SAFETY: The test passes its state as the context.
        let state = unsafe { &mut *context.cast::<EnumerationState<'_>>() };
        let realm = state.vm.current_realm().expect("there is a realm");
        run_script(state.vm, realm, "delete object.deletedOnTheWay").must();
        state.vm.heap().collect_garbage();
        let key = utf8(Value(key));
        let stop = key == state.stop_at;
        state.keys.push(key);
        stop
    }

    #[test]
    fn enumerating_object_properties_visits_the_keys_of_a_for_in_loop_until_told_to_stop() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object_of = || {
            run_script(
                &vm,
                realm,
                "globalThis.object = Object.create({ inherited: 1, own: 'shadowed' });
                 object.own = 2; object.deletedOnTheWay = 3; object[Symbol('s')] = 4; object.last = 5;
                 object",
            )
            .must()
            .as_object()
        };
        let vm_pointer = vm_into_abi(&vm);
        let enumerate = |object: Gc<Object>, stop_at: &'static str| {
            let mut state = EnumerationState {
                vm: &vm,
                keys: Vec::new(),
                stop_at,
            };
            // SAFETY: The arguments are live, and the callback records into a local.
            let completion = unsafe {
                js_object_enumerate_object_properties(
                    vm_pointer,
                    object_into_abi(object),
                    Some(record_key_and_stop_at_the_wanted_one),
                    core::ptr::from_mut(&mut state).cast(),
                )
            };
            assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
            (state.keys, completion.payload == 1)
        };
        assert_eq!(
            enumerate(object_of(), ""),
            (
                vec!["own".to_string(), "last".to_string(), "inherited".to_string()],
                false
            )
        );
        assert_eq!(enumerate(object_of(), "own"), (vec!["own".to_string()], true));
    }

    #[test]
    fn set_immutable_prototype_only_accepts_the_current_prototype() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let vm_pointer = vm_into_abi(&vm);
        let object = object_into_abi(realm.object_prototype());
        let set = |prototype: *mut JSObject| {
            // SAFETY: The arguments are live.
            let completion = unsafe { js_object_set_immutable_prototype(vm_pointer, object, prototype) };
            assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
            completion.payload == 1
        };
        assert!(set(core::ptr::null_mut()));
        assert!(!set(object_into_abi(realm.array_prototype())));
        // SAFETY: The object is live.
        assert!(unsafe { js_object_prototype(object) }.is_null());
    }

    #[test]
    fn engine_private_properties_stay_hidden_from_scripts() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let vm_pointer = vm_into_abi(&vm);
        let object = run_script(&vm, realm, "globalThis.target = { visible: 1 }")
            .must()
            .as_object();
        // SAFETY: The arguments are live.
        let private_symbol = unsafe { crate::embedding::symbol::js_symbol_create_private(vm_pointer) };
        // SAFETY: As above.
        unsafe {
            js_object_set_engine_private_property(
                vm_pointer,
                object_into_abi(object),
                private_symbol,
                Value::from_i32(9).0,
            );
        }
        vm.heap().collect_garbage();
        // SAFETY: The symbol is live.
        let stored = object.get_engine_private_property(unsafe { cell_from_abi(private_symbol) });
        assert_eq!(stored.map(|stored| stored.value), Some(Value::from_i32(9)));
        assert_eq!(
            utf8(
                run_script(
                    &vm,
                    realm,
                    "`${Reflect.ownKeys(target).length} ${Object.getOwnPropertySymbols(target).length}`"
                )
                .must()
            ),
            "1 0"
        );
    }

    #[test]
    fn invalidating_lookup_caches_gives_the_object_a_shape_no_cache_has_seen() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object = run_script(
            &vm,
            realm,
            "globalThis.cached = { x: 1 }; globalThis.readX = o => o.x;
             for (let i = 0; i < 10; ++i) readX(cached); cached",
        )
        .must()
        .as_object();
        let shape_before = object.shape();
        // SAFETY: The arguments are live.
        unsafe { js_object_invalidate_property_lookup_caches(vm_into_abi(&vm), object_into_abi(object)) };
        assert!(object.shape() != shape_before && object.shape().is_dictionary());
        assert_eq!(run_script(&vm, realm, "readX(cached)").must(), Value::from_i32(1));

        object.set_requires_slow_add_own_property();
        // SAFETY: As above.
        unsafe { js_object_clear_requires_slow_add_own_property(object_into_abi(object)) };
        assert!(!object.requires_slow_add_own_property());
    }
}
