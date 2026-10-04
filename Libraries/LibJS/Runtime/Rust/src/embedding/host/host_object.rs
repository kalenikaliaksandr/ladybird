/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host objects of kind JS_HOST_CLASS_OBJECT.
//!
//! The exported functions of this file run on the thread that owns the VM, and trust their arguments, as those of
//! embedding/object.rs do: `vm` is the embedder's VM, objects and realms are live cells of its heap, host classes are
//! tables that live as long as the process, and the embedder's cells are null or live GC cells of the same heap.
#![allow(
    clippy::missing_safety_doc,
    reason = "the module documentation states the contract every exported function shares"
)]

use core::ffi::c_void;
use core::ops::Deref;
use core::ptr::NonNull;

use crate::embedding::abi_types::{
    JSRealm, cell_from_abi, completion_from_abi, object_into_abi, optional_cell_from_abi, optional_object_into_abi,
    vm_from_abi,
};
use crate::embedding::error::error_data_from_host_hook;
use crate::embedding::hooks::lend_property_key_to_abi;
use crate::embedding::host::class_table::{
    bool_completion_from_hook, completion_without_result_from_hook, copy_host_class_flags_into_object,
    get_cache_metadata_into_abi, host_class_from_abi, host_class_into_abi, lend_object_to_hook, lookup_phase_into_abi,
    optional_object_completion_from_hook, set_cache_metadata_into_abi,
};
use crate::embedding::host::host_array::as_host_array;
use crate::embedding::host::host_function::as_host_function;
use crate::embedding::host::registry::runtime_class_and_allocator_of_host_class;
use crate::embedding::object::{property_descriptor_from_abi, property_descriptor_to_abi};
use crate::gc::class::{Class, Finalize, define_cell};
use crate::gc::class_id::ClassId;
use crate::gc::foreign::ForeignCellSlot;
use crate::gc::root::MarkedVec;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{
    JS_HOST_CLASS_IMMUTABLE_PROTOTYPE, JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE,
    JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH, JS_HOST_CLASS_OBJECT, JS_PD_CONFIGURABLE,
    JS_PD_ENUMERABLE, JS_PD_HAS_CONFIGURABLE, JS_PD_HAS_ENUMERABLE, JS_PD_HAS_VALUE, JS_PD_HAS_WRITABLE, JS_PD_PRESENT,
    JS_PD_WRITABLE, JSHostClass, JSHostObjectHooks, JSObject, JSPropertyDescriptor, JSVM, JSValue, JSValueSink,
};
pub use crate::layout::host_object::HostObject;
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::error_data::ErrorData;
use crate::runtime::object::{
    CacheableGetPropertyMetadata, CacheableSetPropertyMetadata, MayInterfereWithIndexedPropertyAccess,
    ORDINARY_OBJECT_METHODS, Object, ObjectMethods, PropertyLookupPhase, allocate_object_in,
};
use crate::runtime::property_descriptor::PropertyDescriptor;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;

// The class that the class of every host class table of this kind extends. Its own internal methods are ordinary,
// and no object has it as its class.
define_cell!(HostObject, Object, extends: [Object], methods: ORDINARY_OBJECT_METHODS, finalize: finalize);

// SAFETY: Visits the object and the embedder's two cells, which are all the cells a host object reaches.
unsafe impl Trace for HostObject {
    fn trace(&self, visitor: &mut Visitor) {
        self.base.trace(visitor);
        self.wrappable.trace(visitor);
        self.host_data.trace(visitor);
    }
}

impl Finalize for HostObject {
    fn finalize(&self) {
        if let Some(finalize) = self.host_class.host_object_hooks().finalize {
            // SAFETY: The hook takes a dying object of its class, which stays intact while it runs.
            unsafe { finalize(lend_object_to_hook(&self.base)) };
        }
    }
}

impl Deref for HostObject {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

/// The internal methods of the objects of a host class table: those of an ordinary object, with each method that the
/// table has a hook for calling the hook, as modified by the table's flags.
fn host_object_methods(table: &'static JSHostClass) -> ObjectMethods {
    let hooks = table.host_object_hooks();
    let mut methods = ObjectMethods {
        error_data: no_error_data,
        ..ORDINARY_OBJECT_METHODS
    };
    if hooks.get_prototype_of.is_some() {
        methods.internal_get_prototype_of = get_prototype_of_through_hook;
    }
    if hooks.set_prototype_of.is_some() {
        methods.internal_set_prototype_of = set_prototype_of_through_hook;
    } else if table.has_flag(JS_HOST_CLASS_IMMUTABLE_PROTOTYPE) {
        methods.internal_set_prototype_of = Object::set_immutable_prototype;
    }
    if hooks.is_extensible.is_some() {
        methods.internal_is_extensible = is_extensible_through_hook;
    }
    if hooks.prevent_extensions.is_some() {
        methods.internal_prevent_extensions = prevent_extensions_through_hook;
    }
    if hooks.get_own_property.is_some() {
        methods.internal_get_own_property = get_own_property_through_hook;
    }
    if hooks.define_own_property.is_some() {
        methods.internal_define_own_property = define_own_property_through_hook;
    }
    if hooks.has_property.is_some() {
        methods.internal_has_property = has_property_through_hook;
    }
    if hooks.get.is_some() {
        methods.internal_get = get_through_hook;
    }
    if hooks.set.is_some() {
        methods.internal_set = set_through_hook;
    }
    if hooks.delete_property.is_some() {
        methods.internal_delete = delete_through_hook;
    }
    if hooks.own_property_keys.is_some() {
        methods.internal_own_property_keys = own_property_keys_through_hook;
    }
    if table.has_flag(JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE) {
        methods.is_cacheable_for_property_absence = |_| false;
    }
    if hooks.is_cacheable_for_inherited_property.is_some() {
        methods.is_cacheable_for_inherited_property = is_cacheable_for_inherited_property_through_hook;
    }
    // The fast paths read keys, attributes and values straight from the shape and storage and follow the shape's
    // prototype, so they would bypass these hooks.
    let hooks_answer_instead_of_the_shape = hooks.get_prototype_of.is_some()
        || hooks.get_own_property.is_some()
        || hooks.has_property.is_some()
        || hooks.get.is_some()
        || hooks.own_property_keys.is_some();
    if hooks_answer_instead_of_the_shape
        || table.has_flag(JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH)
    {
        methods.eligible_for_own_property_enumeration_fast_path = |_| false;
    }
    if hooks.error_data.is_some() {
        methods.error_data = error_data_through_hook;
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

fn hooks_of(object: &Object) -> &'static JSHostObjectHooks {
    as_host_object(object).host_class.host_object_hooks()
}

fn no_error_data(_: &Object) -> Option<&ErrorData> {
    None
}

// The internal methods of a class whose table has a hook for them. The runtime only installs each for a table with
// that hook, holds no borrow of its state across the call, and lends the hook the object and key for its duration.

fn get_prototype_of_through_hook(object: &Object, _vm: &Vm) -> ThrowCompletionOr<Option<Gc<Object>>> {
    let hook = hooks_of(object)
        .get_prototype_of
        .expect("the class has a [[GetPrototypeOf]] hook");
    // SAFETY: The hook takes an object of its class, and completes with null or a live object.
    unsafe { optional_object_completion_from_hook(hook(lend_object_to_hook(object))) }
}

fn set_prototype_of_through_hook(object: &Object, _vm: &Vm, prototype: Option<Gc<Object>>) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .set_prototype_of
        .expect("the class has a [[SetPrototypeOf]] hook");
    // SAFETY: The hook takes an object of its class and null or a live object.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object), optional_object_into_abi(prototype)) })
}

fn is_extensible_through_hook(object: &Object, _vm: &Vm) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .is_extensible
        .expect("the class has an [[IsExtensible]] hook");
    // SAFETY: The hook takes an object of its class.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object)) })
}

fn prevent_extensions_through_hook(object: &Object, _vm: &Vm) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .prevent_extensions
        .expect("the class has a [[PreventExtensions]] hook");
    // SAFETY: The hook takes an object of its class.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object)) })
}

fn get_own_property_through_hook(
    object: &Object,
    _vm: &Vm,
    key: &PropertyKey,
) -> ThrowCompletionOr<Option<PropertyDescriptor>> {
    let hook = hooks_of(object)
        .get_own_property
        .expect("the class has a [[GetOwnProperty]] hook");
    let mut descriptor = property_descriptor_to_abi(None);
    // SAFETY: The hook takes an object of its class, a key, and a zeroed descriptor to fill in.
    completion_without_result_from_hook(unsafe {
        hook(
            lend_object_to_hook(object),
            lend_property_key_to_abi(key),
            &raw mut descriptor,
        )
    })?;
    // SAFETY: The getter and setter of a descriptor that a hook fills in are null or live functions.
    Ok(unsafe { property_descriptor_from_hook(&descriptor) })
}

/// The descriptor that a [[GetOwnProperty]] hook filled in. A data property with all of its attributes, such as an
/// indexed or named property of a collection, skips the general conversion, as the C++ HostObject does, since reads
/// of such properties spend a measurable share of their time in it.
///
/// # Safety
///
/// The getter and setter of the descriptor must be null or live functions.
unsafe fn property_descriptor_from_hook(descriptor: &JSPropertyDescriptor) -> Option<PropertyDescriptor> {
    const COMPLETE_DATA_DESCRIPTOR_FLAGS: u16 =
        JS_PD_PRESENT | JS_PD_HAS_VALUE | JS_PD_HAS_WRITABLE | JS_PD_HAS_ENUMERABLE | JS_PD_HAS_CONFIGURABLE;
    const ATTRIBUTE_FLAGS: u16 = JS_PD_WRITABLE | JS_PD_ENUMERABLE | JS_PD_CONFIGURABLE;
    if descriptor.flags & !ATTRIBUTE_FLAGS == COMPLETE_DATA_DESCRIPTOR_FLAGS {
        return Some(PropertyDescriptor {
            value: Some(Value(descriptor.value)),
            writable: Some(descriptor.flags & JS_PD_WRITABLE != 0),
            enumerable: Some(descriptor.flags & JS_PD_ENUMERABLE != 0),
            configurable: Some(descriptor.flags & JS_PD_CONFIGURABLE != 0),
            ..Default::default()
        });
    }
    // SAFETY: The caller guarantees that the getter and setter are null or live functions.
    unsafe { property_descriptor_from_abi(descriptor) }
}

fn define_own_property_through_hook(
    object: &Object,
    _vm: &Vm,
    key: &PropertyKey,
    descriptor: &mut PropertyDescriptor,
    precomputed_get_own_property: Option<&Option<PropertyDescriptor>>,
) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .define_own_property
        .expect("the class has a [[DefineOwnProperty]] hook");
    let mut abi_descriptor = property_descriptor_to_abi(Some(descriptor));
    let abi_precomputed_get_own_property =
        precomputed_get_own_property.map(|precomputed| property_descriptor_to_abi(precomputed.as_ref()));
    // SAFETY: The hook takes an object of its class, a key, a present descriptor it may update, and null or the
    //         result of a [[GetOwnProperty]] the caller already ran.
    let completion = unsafe {
        hook(
            lend_object_to_hook(object),
            lend_property_key_to_abi(key),
            &raw mut abi_descriptor,
            abi_precomputed_get_own_property
                .as_ref()
                .map_or(core::ptr::null(), core::ptr::from_ref),
        )
    };
    // SAFETY: The getter and setter of a descriptor that a hook writes back are null or live functions.
    if let Some(descriptor_written_back_by_hook) = unsafe { property_descriptor_from_abi(&abi_descriptor) } {
        *descriptor = descriptor_written_back_by_hook;
    }
    bool_completion_from_hook(completion)
}

fn has_property_through_hook(object: &Object, _vm: &Vm, key: &PropertyKey) -> ThrowCompletionOr<bool> {
    let hook = hooks_of(object)
        .has_property
        .expect("the class has a [[HasProperty]] hook");
    // SAFETY: The hook takes an object of its class and a key.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object), lend_property_key_to_abi(key)) })
}

fn get_through_hook(
    object: &Object,
    _vm: &Vm,
    key: &PropertyKey,
    receiver: Value,
    cacheable_metadata: Option<&mut CacheableGetPropertyMetadata>,
    phase: PropertyLookupPhase,
) -> ThrowCompletionOr<Value> {
    let hook = hooks_of(object).get.expect("the class has a [[Get]] hook");
    // SAFETY: The hook takes an object of its class, a key, and the caller's cache metadata and phase, which it
    //         passes on untouched, if at all, to the engine operation it delegates to.
    completion_from_abi(unsafe {
        hook(
            lend_object_to_hook(object),
            lend_property_key_to_abi(key),
            receiver.0,
            get_cache_metadata_into_abi(cacheable_metadata),
            lookup_phase_into_abi(phase),
        )
    })
}

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
    // SAFETY: As for [[Get]].
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
    // SAFETY: The hook takes an object of its class and a key.
    bool_completion_from_hook(unsafe { hook(lend_object_to_hook(object), lend_property_key_to_abi(key)) })
}

/// Appends a key that an [[OwnPropertyKeys]] hook hands its sink to the list that is the sink's context.
unsafe extern "C" fn append_key_to_list(keys: *mut c_void, key: JSValue) {
    // SAFETY: The sink's context is the rooted list of keys of the [[OwnPropertyKeys]] call that made the sink.
    let keys = unsafe { &*keys.cast::<MarkedVec<'_, Value>>() };
    keys.push(Value(key));
}

fn own_property_keys_through_hook<'vm>(object: &Object, vm: &'vm Vm) -> ThrowCompletionOr<MarkedVec<'vm, Value>> {
    let hook = hooks_of(object)
        .own_property_keys
        .expect("the class has an [[OwnPropertyKeys]] hook");
    let keys = MarkedVec::new(vm);
    let mut sink = JSValueSink {
        context: core::ptr::from_ref(&keys).cast_mut().cast(),
        append: Some(append_key_to_list),
    };
    // SAFETY: The hook takes an object of its class and a sink, whose list roots the keys appended to it.
    completion_without_result_from_hook(unsafe { hook(lend_object_to_hook(object), &raw mut sink) })?;
    Ok(keys)
}

fn is_cacheable_for_inherited_property_through_hook(object: &Object) -> bool {
    let hook = hooks_of(object)
        .is_cacheable_for_inherited_property
        .expect("the class has an is_cacheable_for_inherited_property hook");
    // SAFETY: The hook takes an object of its class.
    unsafe { hook(lend_object_to_hook(object)) }
}

fn error_data_through_hook(object: &Object) -> Option<&ErrorData> {
    let hook = hooks_of(object).error_data.expect("the class has an error_data hook");
    // SAFETY: The hook takes an object of its class, and returns null or error data that lives as long as the object,
    //         in a cell the object keeps alive.
    unsafe { error_data_from_host_hook(object, hook(lend_object_to_hook(object))) }
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
        ClassId::HostFunction => Some(as_host_function(object).host_class),
        ClassId::HostArray => Some(as_host_array(object).host_class),
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
        ClassId::HostFunction => Some(&as_host_function(object).host_data),
        ClassId::HostArray => Some(&as_host_array(object).host_data),
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

// The embedding ABI of host objects, and of what every kind of host object shares

/// HostObject::create(): a host object of `host_class`, a table of kind JS_HOST_CLASS_OBJECT, made from the realm's
/// empty object shape with `prototype_or_null` as its [[Prototype]]. `wrappable_or_null` is the embedder's
/// implementation object, which direct getter functions read at JS_HOST_OBJECT_WRAPPABLE_OFFSET, and
/// `host_data_or_null` a cell with the rest of its per-object state; the object keeps both alive. Returns an unrooted
/// object. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_create(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    host_class: *const JSHostClass,
    prototype_or_null: *mut JSObject,
    wrappable_or_null: *mut c_void,
    host_data_or_null: *mut c_void,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    unsafe {
        object_into_abi(HostObject::create(
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            host_class_from_abi(host_class),
            optional_cell_from_abi::<JSObject>(prototype_or_null),
            NonNull::new(wrappable_or_null),
            NonNull::new(host_data_or_null),
        ))
    }
}

/// HostObject::wrappable(): the implementation object of a host object of kind JS_HOST_CLASS_OBJECT, or null. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_wrappable(host_object: *mut JSObject) -> *mut c_void {
    // SAFETY: See the module documentation.
    let host_object = unsafe { cell_from_abi::<JSObject>(host_object) };
    assert!(host_object.is::<HostObject>(), "only host objects have a wrappable");
    as_host_object(&host_object).wrappable.as_ptr()
}

/// host_class_of(): the host class of an object of any host kind, or null for any other object. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_host_class_of(object: *mut JSObject) -> *const JSHostClass {
    // SAFETY: See the module documentation.
    let object = unsafe { cell_from_abi::<JSObject>(object) };
    host_class_of(&object).map_or(core::ptr::null(), host_class_into_abi)
}

/// is_host_instance_of(): whether the host class of the object is `host_class` or derives from it through
/// JSHostClass::parent, false for an object of no host kind. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_is_host_instance_of(
    object: *mut JSObject,
    host_class: *const JSHostClass,
) -> bool {
    // SAFETY: See the module documentation.
    let (object, host_class) = unsafe { (cell_from_abi::<JSObject>(object), host_class_from_abi(host_class)) };
    is_host_instance_of(&object, host_class)
}

/// host_data_of(): the companion cell of a host object of any kind, or null, also for an object of no host kind.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_host_data_of(object: *mut JSObject) -> *mut c_void {
    // SAFETY: See the module documentation.
    let object = unsafe { cell_from_abi::<JSObject>(object) };
    host_data_of(&object).map_or(core::ptr::null_mut(), NonNull::as_ptr)
}

/// set_host_data(): replaces the companion cell of a host object of any kind with `host_data_or_null`, which the object
/// then keeps alive. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_object_set_host_data(host_object: *mut JSObject, host_data_or_null: *mut c_void) {
    // SAFETY: See the module documentation.
    unsafe { set_host_data(&cell_from_abi::<JSObject>(host_object), NonNull::new(host_data_or_null)) };
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod tests {
    use core::ffi::c_char;

    use super::*;
    use crate::gc::capi::gc_cell_type_info;
    use crate::gc::class::GcCell;
    use crate::layout::host_class::{
        JS_HOST_ABI_VERSION, JS_HOST_CLASS_IS_GLOBAL_OBJECT, JS_HOST_CLASS_IS_HTMLDDA,
        JS_HOST_CLASS_IS_PLATFORM_OBJECT, JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS,
        JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT,
    };
    use crate::layout::host_object::{
        JS_HOST_OBJECT_HOST_CLASS_OFFSET, JS_HOST_OBJECT_HOST_DATA_OFFSET, JS_HOST_OBJECT_WRAPPABLE_OFFSET,
    };
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::{TestRealm, check_that_host_defined_slots_keep_their_cells_alive, key};
    use crate::utilities::initialize_realm;

    /// A table that lives as long as the process, as the ABI requires: tables are static constant data in C.
    pub(crate) fn leak_host_class(
        kind: u8,
        name: &'static str,
        parent: Option<&'static JSHostClass>,
        hooks: *const c_void,
        flags: u32,
    ) -> &'static JSHostClass {
        Box::leak(Box::new(JSHostClass {
            abi_version: JS_HOST_ABI_VERSION,
            kind,
            reserved: 0,
            flags,
            name: name.as_ptr().cast::<c_char>(),
            name_length: name.len(),
            parent: parent.map_or(core::ptr::null(), core::ptr::from_ref),
            hooks,
            user_data: core::ptr::null(),
        }))
    }

    pub(crate) fn object_class(
        name: &'static str,
        parent: Option<&'static JSHostClass>,
        flags: u32,
    ) -> &'static JSHostClass {
        leak_host_class(JS_HOST_CLASS_OBJECT, name, parent, core::ptr::null(), flags)
    }

    fn create(vm: &Vm, realm: Gc<Realm>, table: &'static JSHostClass, prototype: Option<Gc<Object>>) -> Gc<HostObject> {
        // SAFETY: The test's tables are of kind JS_HOST_CLASS_OBJECT, and the objects hold no cells.
        unsafe { HostObject::create(vm, realm, table, prototype, None, None) }
    }

    fn type_info_address(object: Gc<HostObject>) -> usize {
        // SAFETY: The object is a live cell.
        unsafe { gc_cell_type_info(object.as_ptr().cast()) }.addr()
    }

    #[test]
    fn classes_follow_the_chain_of_tables() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let base_class = object_class("Base", None, 0);
        let derived_class = object_class("Derived", Some(base_class), 0);
        let base = create(&vm, test_realm.realm, base_class, None);
        let derived = create(&vm, test_realm.realm, derived_class, None);
        let ordinary = test_realm.object();

        assert_eq!(base.class().class_name(), "Base");
        assert_eq!(derived.class().class_name(), "Derived");
        assert!(derived.class().is_subclass_of(base.class()));
        assert!(base.class().is_subclass_of(HostObject::CLASS) && !base.class().is_subclass_of(derived.class()));
        assert!(base.is::<HostObject>() && !ordinary.is::<HostObject>());
        assert_eq!(base.class().id, ClassId::HostObject);

        assert!(host_class_of(&derived).is_some_and(|table| core::ptr::eq(table, derived_class)));
        assert!(host_class_of(&ordinary).is_none());
        assert!(is_host_instance_of(&derived, derived_class) && is_host_instance_of(&derived, base_class));
        assert!(!is_host_instance_of(&base, derived_class) && !is_host_instance_of(&ordinary, base_class));

        let second_derived = create(&vm, test_realm.realm, derived_class, None);
        assert!(core::ptr::eq(second_derived.class(), derived.class()));
    }

    #[test]
    fn table_flags_become_object_flags() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let all_flags = object_class(
            "AllFlags",
            None,
            JS_HOST_CLASS_IS_PLATFORM_OBJECT
                | JS_HOST_CLASS_REQUIRES_SLOW_ADD_OWN_PROPERTY
                | JS_HOST_CLASS_MAY_INTERFERE_WITH_INDEXED_PROPERTY_ACCESS
                | JS_HOST_CLASS_IS_HTMLDDA
                | JS_HOST_CLASS_IS_GLOBAL_OBJECT
                | JS_HOST_CLASS_NOT_CACHEABLE_FOR_PROPERTY_ABSENCE
                | JS_HOST_CLASS_NOT_ELIGIBLE_FOR_OWN_PROPERTY_ENUMERATION_FAST_PATH,
        );
        let flagged = create(&vm, realm, all_flags, Some(realm.object_prototype()));
        assert!(flagged.is_platform_object() && flagged.requires_slow_add_own_property());
        assert!(flagged.may_interfere_with_indexed_property_access() && flagged.is_htmldda());
        assert!(flagged.has_global_object_flag());
        assert!(!flagged.is_cacheable_for_property_absence());
        assert!(!flagged.eligible_for_own_property_enumeration_fast_path());

        let plain = create(
            &vm,
            realm,
            object_class("NoFlags", None, 0),
            Some(realm.object_prototype()),
        );
        assert!(!plain.is_platform_object() && !plain.requires_slow_add_own_property());
        assert!(!plain.may_interfere_with_indexed_property_access() && !plain.is_htmldda());
        assert!(!plain.has_global_object_flag());
        assert!(plain.is_cacheable_for_property_absence());
        assert!(plain.eligible_for_own_property_enumeration_fast_path());
        assert!(plain.extensible() && plain.error_data().is_none());

        realm.global_object().define_direct_property(
            &vm,
            &key("flagged"),
            Value::from_object(flagged.upcast::<Object>()),
            DEFAULT_ATTRIBUTES,
        );
        assert_eq!(utf8(run_script(&vm, realm, "typeof flagged").must()), "undefined");
        assert_eq!(utf8(run_script(&vm, realm, "flagged == null").must()), "true");
    }

    #[test]
    fn immutable_prototype_flag() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object_prototype = test_realm.realm.object_prototype();
        let immutable = create(
            &vm,
            test_realm.realm,
            object_class("ImmutablePrototype", None, JS_HOST_CLASS_IMMUTABLE_PROTOTYPE),
            Some(object_prototype),
        );
        assert!(
            !immutable
                .internal_set_prototype_of(&vm, Some(test_realm.object()))
                .must()
        );
        assert!(immutable.internal_set_prototype_of(&vm, Some(object_prototype)).must());
        assert!(!immutable.internal_set_prototype_of(&vm, None).must());
        assert_eq!(immutable.prototype(), Some(object_prototype));
    }

    #[test]
    fn host_objects_keep_their_cells_at_the_fixed_offsets() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let table = object_class("NoFlags", None, 0);
        let wrappable = test_realm.object();
        let host_data = test_realm.object();
        // SAFETY: The table is of the right kind, and the cells are live objects.
        let object = unsafe {
            HostObject::create(
                &vm,
                test_realm.realm,
                table,
                None,
                Some(wrappable.as_non_null().cast()),
                Some(host_data.as_non_null().cast()),
            )
        };
        let word_at = |offset: usize| {
            // SAFETY: The offsets are those of the words of a host object.
            unsafe { object.as_ptr().cast::<u8>().add(offset).cast::<usize>().read() }
        };
        assert_eq!(
            word_at(JS_HOST_OBJECT_HOST_CLASS_OFFSET),
            core::ptr::from_ref(table).addr()
        );
        assert_eq!(word_at(JS_HOST_OBJECT_WRAPPABLE_OFFSET), wrappable.as_ptr().addr());
        assert_eq!(word_at(JS_HOST_OBJECT_HOST_DATA_OFFSET), host_data.as_ptr().addr());
        assert_eq!(host_data_of(&object), Some(host_data.as_non_null().cast()));

        // SAFETY: The new companion is a live object.
        unsafe { set_host_data(&object, Some(wrappable.as_non_null().cast())) };
        assert_eq!(host_data_of(&object), Some(wrappable.as_non_null().cast()));
        assert_eq!(host_data_of(&test_realm.object()), None);
    }

    #[test]
    fn host_objects_keep_their_cells_alive() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let table = object_class("CellHolder", None, 0);
        let create_holding = |wrappable: Option<NonNull<c_void>>, host_data: Option<NonNull<c_void>>| {
            // SAFETY: The table is of the right kind, and the slots the check passes hold live objects.
            unsafe { HostObject::create(&vm, test_realm.realm, table, None, wrappable, host_data) }
        };
        check_that_host_defined_slots_keep_their_cells_alive(
            &vm,
            &test_realm,
            |slot| create_holding(slot.get(), None),
            |holder| holder.wrappable.get(),
        );
        check_that_host_defined_slots_keep_their_cells_alive(
            &vm,
            &test_realm,
            |slot| create_holding(None, slot.get()),
            |holder| host_data_of(&holder),
        );
    }

    #[test]
    fn each_host_class_has_its_own_allocator_unless_it_shares_its_parents() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let a_class = object_class("AllocatorA", None, 0);
        let b_class = object_class("AllocatorB", None, 0);
        let sharing_child_class = object_class(
            "AllocatorASharingChild",
            Some(a_class),
            JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT,
        );
        let sharing_grandchild_class = object_class(
            "AllocatorASharingGrandchild",
            Some(sharing_child_class),
            JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT,
        );
        let isolated_child_class = object_class("AllocatorAIsolatedChild", Some(a_class), 0);

        let first_a = create(&vm, test_realm.realm, a_class, None);
        let second_a = create(&vm, test_realm.realm, a_class, None);
        let b = create(&vm, test_realm.realm, b_class, None);
        let sharing_grandchild = create(&vm, test_realm.realm, sharing_grandchild_class, None);
        let sharing_child = create(&vm, test_realm.realm, sharing_child_class, None);
        let isolated_child = create(&vm, test_realm.realm, isolated_child_class, None);

        assert_eq!(type_info_address(first_a), type_info_address(second_a));
        assert_ne!(type_info_address(first_a), type_info_address(b));
        assert_eq!(type_info_address(sharing_child), type_info_address(first_a));
        assert_eq!(type_info_address(sharing_grandchild), type_info_address(first_a));
        assert_ne!(type_info_address(isolated_child), type_info_address(first_a));
        assert_eq!(
            type_info_address(first_a),
            core::ptr::from_ref(&first_a.class().type_info).addr()
        );

        assert!(core::ptr::eq(
            host_class_of(&sharing_grandchild).expect("a host object"),
            sharing_grandchild_class
        ));
        assert_eq!(sharing_grandchild.class().class_name(), "AllocatorASharingGrandchild");
        assert!(sharing_grandchild.class().is_subclass_of(first_a.class()));
    }
}

/// Host objects whose hooks are written against LibJS/HostObjectABI.h and the embedding ABI alone, the way an embedder
/// writes them.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod hook_tests {
    use core::cell::Cell;

    use super::tests::leak_host_class;
    use super::*;
    use crate::embedding::abi_types::{JS_ERROR_KIND_TYPE_ERROR, JSUtf16View, append_to_value_sink, vm_into_abi};
    use crate::embedding::error::js_error_throw;
    use crate::embedding::hooks::clone_lent_property_key_from_abi;
    use crate::embedding::object::{
        js_object_internal_get_as_prototype_of, js_object_ordinary_define_own_property, js_object_ordinary_delete,
        js_object_ordinary_get, js_object_ordinary_get_own_property, js_object_ordinary_get_prototype_of,
        js_object_ordinary_has_property, js_object_ordinary_is_extensible, js_object_ordinary_own_property_keys,
        js_object_ordinary_set, js_object_ordinary_set_prototype_of,
    };
    use crate::gc::weak::GcWeak;
    use crate::layout::host_class::{
        JS_COMPLETION_NORMAL, JS_HOST_CLASS_OBJECT, JS_PD_HAS_GET, JS_PD_HAS_PROPERTY_OFFSET, JS_PD_HAS_SET,
        JSCompletion, JSGetCacheMetadata, JSObject, JSPropertyKey, JSSetCacheMetadata, JSVM,
    };
    use crate::runtime::completion::Must;
    use crate::runtime::error::Error;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::object::{CacheableGetPropertyMetadataType, CacheableSetPropertyMetadataType};
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::utf16::Utf16View;
    use crate::utilities::{RootExecutionContext, initialize_realm};

    std::thread_local! {
        static HOOK_VM: Cell<*const Vm> = const { Cell::new(core::ptr::null()) };
        static KEYLESS_HOOKS_THROW: Cell<bool> = const { Cell::new(false) };
        static LAST_DEFINED_DESCRIPTOR: Cell<Option<JSPropertyDescriptor>> = const { Cell::new(None) };
        /// Not given, given and absent, or given with a descriptor.
        static LAST_PRECOMPUTED_GET_OWN_PROPERTY: Cell<Option<Option<JSPropertyDescriptor>>> = const { Cell::new(None) };
        static LAST_INTERCEPTED_VALUE: Cell<JSValue> = const { Cell::new(0) };
        static INHERITED_PROPERTY_IS_CACHEABLE: Cell<bool> = const { Cell::new(true) };
        static FINALIZED_HOST_OBJECTS: Cell<usize> = const { Cell::new(0) };
        /// A script each hook runs, and a collection each one starts, before it does anything else, while
        /// REENTRANT_HOOK_DEPTH is 0, so that the script can reach the hooks again without running itself forever.
        static SCRIPT_FOR_HOOKS_TO_REENTER_WITH: Cell<Option<&'static str>> = const { Cell::new(None) };
        static REENTRANT_HOOK_DEPTH: Cell<u32> = const { Cell::new(0) };
        static HOOKS_THAT_REENTERED: core::cell::RefCell<Vec<&'static str>> = const { core::cell::RefCell::new(Vec::new()) };
    }

    /// A VM with a realm whose global object scripts can reach, which the hooks of this module run in.
    pub(crate) struct HookTestEnvironment<'vm> {
        vm: &'vm Vm,
        root_execution_context: RootExecutionContext<'vm>,
    }

    impl<'vm> HookTestEnvironment<'vm> {
        pub(crate) fn new(vm: &'vm Vm) -> Self {
            HOOK_VM.set(core::ptr::from_ref(vm));
            Self {
                vm,
                root_execution_context: initialize_realm(vm),
            }
        }

        pub(crate) fn realm(&self) -> Gc<Realm> {
            self.root_execution_context.realm()
        }

        pub(crate) fn define_global(&self, name: &str, object: Gc<Object>) {
            self.realm().global_object().define_direct_property(
                self.vm,
                &key(name),
                Value::from_object(object),
                DEFAULT_ATTRIBUTES,
            );
        }

        /// The completion value as a string, or "uncaught <error>" for an exception that escaped the script.
        pub(crate) fn evaluate(&self, source: &str) -> String {
            match run_script(self.vm, self.realm(), source) {
                Ok(value) => utf8(value),
                Err(throw) => format!("uncaught {}", utf8(throw.value())),
            }
        }

        /// "<name>: <message>" of what the statements throw, or "no exception".
        pub(crate) fn exception_from(&self, statements: &str) -> String {
            self.evaluate(&format!(
                "(() => {{ try {{ {statements}; }} catch (error) {{ return `${{error.name}}: ${{error.message}}`; }} return 'no exception'; }})()"
            ))
        }

        pub(crate) fn create(&self, table: &'static JSHostClass, prototype: Option<Gc<Object>>) -> Gc<HostObject> {
            // SAFETY: The tables of this module are of kind JS_HOST_CLASS_OBJECT.
            unsafe { HostObject::create(self.vm, self.realm(), table, prototype, None, None) }
        }
    }

    impl Drop for HookTestEnvironment<'_> {
        fn drop(&mut self) {
            HOOK_VM.set(core::ptr::null());
        }
    }

    pub(crate) fn hook_vm() -> &'static Vm {
        let vm = HOOK_VM.get();
        assert!(!vm.is_null(), "hooks run while a test environment exists");
        // SAFETY: The environment that set the VM outlives the hooks it runs.
        unsafe { &*vm }
    }

    fn abi_vm() -> *mut JSVM {
        vm_into_abi(hook_vm())
    }

    pub(crate) fn key_is(key: JSPropertyKey, name: &str) -> bool {
        // SAFETY: Hooks receive live keys.
        let key = unsafe { clone_lent_property_key_from_abi(key) };
        key == PropertyKey::from_utf8(name)
    }

    fn string_value(text: &str) -> JSValue {
        Value::from_string(PrimitiveString::create_from_utf8(hook_vm(), text)).0
    }

    pub(crate) fn hook_error(hook: &str) -> JSCompletion {
        let message = format!("{hook} threw");
        // SAFETY: The view is of the message, which outlives the call.
        unsafe {
            js_error_throw(
                abi_vm(),
                JS_ERROR_KIND_TYPE_ERROR,
                JSUtf16View::of(Utf16View::Ascii(message.as_bytes())),
            )
        }
    }

    pub(crate) fn normal(payload: u64) -> JSCompletion {
        JSCompletion {
            payload,
            variant: JS_COMPLETION_NORMAL,
        }
    }

    /// Runs the script the test gave the hooks, and collects garbage, unless a hook is already doing so.
    pub(crate) fn reenter(hook: &'static str) {
        let Some(script) = SCRIPT_FOR_HOOKS_TO_REENTER_WITH.get() else {
            return;
        };
        if REENTRANT_HOOK_DEPTH.get() > 0 {
            return;
        }
        REENTRANT_HOOK_DEPTH.set(1);
        let vm = hook_vm();
        let realm = vm.current_realm().expect("hooks run in a realm");
        let completion = run_script(vm, realm, script);
        vm.heap().collect_garbage();
        REENTRANT_HOOK_DEPTH.set(0);
        assert!(completion.is_ok(), "the script that {hook} runs completes");
        HOOKS_THAT_REENTERED.with_borrow_mut(|hooks| hooks.push(hook));
    }

    /// The hooks that re-entered the VM with `script` while `operation` ran, sorted and once each.
    pub(crate) fn hooks_that_reentered_while(script: &'static str, operation: impl FnOnce()) -> Vec<&'static str> {
        HOOKS_THAT_REENTERED.with_borrow_mut(Vec::clear);
        SCRIPT_FOR_HOOKS_TO_REENTER_WITH.set(Some(script));
        operation();
        SCRIPT_FOR_HOOKS_TO_REENTER_WITH.set(None);
        let mut hooks = HOOKS_THAT_REENTERED.with_borrow(Vec::clone);
        hooks.sort_unstable();
        hooks.dedup();
        hooks
    }

    /// A script for hooks to re-enter the VM with, which reaches the hooks of `host` again and makes garbage.
    pub(crate) const REENTRANT_SCRIPT: &str = "globalThis.reentered = (globalThis.reentered | 0) + 1; \
         (typeof host === 'object' ? host.answer + Object.keys(host).length : 0) + [1, 2, 3].map(String).join()";

    // Hooks that implement every internal method. Hooks taking a key answer some keys themselves, throw for
    // "throwing", and leave the rest to the ordinary internal method, the way bindings do.

    unsafe extern "C" fn intercepting_get_prototype_of(object: *mut JSObject) -> JSCompletion {
        reenter("get_prototype_of");
        if KEYLESS_HOOKS_THROW.get() {
            return hook_error("get_prototype_of");
        }
        // SAFETY: The object is the hook's own.
        unsafe { js_object_ordinary_get_prototype_of(abi_vm(), object) }
    }

    unsafe extern "C" fn intercepting_set_prototype_of(
        object: *mut JSObject,
        prototype: *mut JSObject,
    ) -> JSCompletion {
        reenter("set_prototype_of");
        if KEYLESS_HOOKS_THROW.get() {
            return hook_error("set_prototype_of");
        }
        // SAFETY: The object and prototype are the hook's own.
        unsafe { js_object_ordinary_set_prototype_of(abi_vm(), object, prototype) }
    }

    unsafe extern "C" fn intercepting_is_extensible(object: *mut JSObject) -> JSCompletion {
        reenter("is_extensible");
        if KEYLESS_HOOKS_THROW.get() {
            return hook_error("is_extensible");
        }
        // SAFETY: The object is the hook's own.
        unsafe { js_object_ordinary_is_extensible(abi_vm(), object) }
    }

    unsafe extern "C" fn intercepting_prevent_extensions(_object: *mut JSObject) -> JSCompletion {
        reenter("prevent_extensions");
        if KEYLESS_HOOKS_THROW.get() {
            return hook_error("prevent_extensions");
        }
        normal(0)
    }

    unsafe extern "C" fn intercepting_get_own_property(
        object: *mut JSObject,
        key: JSPropertyKey,
        out: *mut JSPropertyDescriptor,
    ) -> JSCompletion {
        reenter("get_own_property");
        if key_is(key, "throwing") {
            return hook_error("get_own_property");
        }
        if key_is(key, "virtual") {
            // SAFETY: The descriptor is the hook's to fill in.
            unsafe {
                out.write(JSPropertyDescriptor {
                    value: string_value("virtual value"),
                    get: core::ptr::null_mut(),
                    set: core::ptr::null_mut(),
                    property_offset: 0,
                    flags: JS_PD_PRESENT
                        | JS_PD_HAS_VALUE
                        | JS_PD_HAS_WRITABLE
                        | JS_PD_HAS_ENUMERABLE
                        | JS_PD_ENUMERABLE
                        | JS_PD_HAS_CONFIGURABLE
                        | JS_PD_CONFIGURABLE,
                });
            }
            return normal(0);
        }
        // SAFETY: The arguments are the hook's own.
        unsafe { js_object_ordinary_get_own_property(abi_vm(), object, &raw const key, out) }
    }

    unsafe extern "C" fn intercepting_define_own_property(
        object: *mut JSObject,
        key: JSPropertyKey,
        descriptor: *mut JSPropertyDescriptor,
        precomputed_get_own_property: *const JSPropertyDescriptor,
    ) -> JSCompletion {
        reenter("define_own_property");
        if key_is(key, "throwing") {
            return hook_error("define_own_property");
        }
        if key_is(key, "rejected") {
            return normal(0);
        }
        // SAFETY: The descriptors are the hook's own.
        unsafe {
            LAST_DEFINED_DESCRIPTOR.set(Some(*descriptor));
            LAST_PRECOMPUTED_GET_OWN_PROPERTY.set(Some(precomputed_get_own_property.as_ref().copied()));
            js_object_ordinary_define_own_property(
                abi_vm(),
                object,
                &raw const key,
                descriptor,
                precomputed_get_own_property,
            )
        }
    }

    unsafe extern "C" fn intercepting_has_property(object: *mut JSObject, key: JSPropertyKey) -> JSCompletion {
        reenter("has_property");
        if key_is(key, "throwing") {
            return hook_error("has_property");
        }
        if key_is(key, "magic") {
            return normal(1);
        }
        // SAFETY: The arguments are the hook's own.
        unsafe { js_object_ordinary_has_property(abi_vm(), object, &raw const key) }
    }

    unsafe extern "C" fn intercepting_get(
        object: *mut JSObject,
        key: JSPropertyKey,
        receiver: JSValue,
        metadata: *mut JSGetCacheMetadata,
        phase: u8,
    ) -> JSCompletion {
        reenter("get");
        if key_is(key, "throwing") {
            return hook_error("get");
        }
        if key_is(key, "answer") {
            return normal(Value::from_i32(42).0);
        }
        // SAFETY: The arguments are the hook's own.
        unsafe { js_object_ordinary_get(abi_vm(), object, &raw const key, receiver, metadata, phase) }
    }

    unsafe extern "C" fn intercepting_set(
        object: *mut JSObject,
        key: JSPropertyKey,
        value: JSValue,
        receiver: JSValue,
        metadata: *mut JSSetCacheMetadata,
        phase: u8,
    ) -> JSCompletion {
        reenter("set");
        if key_is(key, "throwing") {
            return hook_error("set");
        }
        if key_is(key, "intercepted") {
            LAST_INTERCEPTED_VALUE.set(value);
            return normal(1);
        }
        // SAFETY: The arguments are the hook's own.
        unsafe { js_object_ordinary_set(abi_vm(), object, &raw const key, value, receiver, metadata, phase) }
    }

    unsafe extern "C" fn intercepting_delete_property(object: *mut JSObject, key: JSPropertyKey) -> JSCompletion {
        reenter("delete_property");
        if key_is(key, "throwing") {
            return hook_error("delete_property");
        }
        if key_is(key, "undeletable") {
            return normal(0);
        }
        // SAFETY: The arguments are the hook's own.
        unsafe { js_object_ordinary_delete(abi_vm(), object, &raw const key) }
    }

    unsafe extern "C" fn intercepting_own_property_keys(object: *mut JSObject, keys: *mut JSValueSink) -> JSCompletion {
        reenter("own_property_keys");
        if KEYLESS_HOOKS_THROW.get() {
            return hook_error("own_property_keys");
        }
        // SAFETY: The object and sink are the hook's own.
        let completion = unsafe { js_object_ordinary_own_property_keys(abi_vm(), object, keys) };
        if completion.variant == JS_COMPLETION_NORMAL {
            // SAFETY: As above.
            append_to_value_sink(unsafe { &*keys }, Value(string_value("virtual")));
        }
        completion
    }

    unsafe extern "C" fn reentering_is_cacheable_for_inherited_property(_object: *mut JSObject) -> bool {
        reenter("is_cacheable_for_inherited_property");
        INHERITED_PROPERTY_IS_CACHEABLE.get()
    }

    unsafe extern "C" fn error_data_of_host_data(object: *mut JSObject) -> *mut c_void {
        reenter("error_data");
        // SAFETY: The object is a live host object.
        let object = unsafe { Gc::<Object>::from_non_null(NonNull::new(object).expect("an object").cast()) };
        let error = host_data_of(&object).expect("the object holds an error");
        // SAFETY: The test's host data is an Error.
        let error = unsafe { Gc::<Object>::from_non_null(error.cast()) };
        error.error_data().map_or(core::ptr::null_mut(), |error_data| {
            core::ptr::from_ref(error_data).cast_mut().cast()
        })
    }

    unsafe extern "C" fn count_finalized_host_object(_object: *mut JSObject) {
        FINALIZED_HOST_OBJECTS.set(FINALIZED_HOST_OBJECTS.get() + 1);
    }

    /// Only a [[Get]] hook, which doubles the numbers the ordinary [[Get]] finds. It passes no cache metadata on, so
    /// that inline caches never answer for it.
    unsafe extern "C" fn doubling_get(
        object: *mut JSObject,
        key: JSPropertyKey,
        receiver: JSValue,
        _metadata: *mut JSGetCacheMetadata,
        phase: u8,
    ) -> JSCompletion {
        // SAFETY: The arguments are the hook's own.
        let completion =
            unsafe { js_object_ordinary_get(abi_vm(), object, &raw const key, receiver, core::ptr::null_mut(), phase) };
        let value = Value(completion.payload);
        if completion.variant != JS_COMPLETION_NORMAL || !value.is_number() {
            return completion;
        }
        normal(Value::from_f64(value.as_f64() * 2.0).0)
    }

    /// Forwards every [[Get]] to the object in its host data as a lookup in that object's prototype chain, passing the
    /// caller's metadata on, as WindowProxy forwards to its Window.
    unsafe extern "C" fn forwarding_get(
        object: *mut JSObject,
        key: JSPropertyKey,
        receiver: JSValue,
        metadata: *mut JSGetCacheMetadata,
        _phase: u8,
    ) -> JSCompletion {
        reenter("forwarding get");
        // SAFETY: The object is a live host object.
        let object = unsafe { Gc::<Object>::from_non_null(NonNull::new(object).expect("an object").cast()) };
        let target = host_data_of(&object).expect("the object forwards to its host data");
        // SAFETY: The arguments are the hook's own, and the target a live object.
        unsafe {
            js_object_internal_get_as_prototype_of(abi_vm(), target.as_ptr().cast(), &raw const key, receiver, metadata)
        }
    }

    /// Hooks written against HostObjectABI.h alone. The first leaves the descriptor it is given zeroed, and the second
    /// accepts every definition but zeroes the descriptor it writes back.
    unsafe extern "C" fn report_every_property_absent(
        _object: *mut JSObject,
        _key: JSPropertyKey,
        _out: *mut JSPropertyDescriptor,
    ) -> JSCompletion {
        normal(0)
    }

    unsafe extern "C" fn accept_definition_and_zero_descriptor(
        _object: *mut JSObject,
        _key: JSPropertyKey,
        descriptor: *mut JSPropertyDescriptor,
        _precomputed_get_own_property: *const JSPropertyDescriptor,
    ) -> JSCompletion {
        // SAFETY: The descriptor is the hook's to update.
        unsafe { descriptor.write(property_descriptor_to_abi(None)) };
        normal(1)
    }

    const NO_HOOKS: JSHostObjectHooks = JSHostObjectHooks {
        get_prototype_of: None,
        set_prototype_of: None,
        is_extensible: None,
        prevent_extensions: None,
        get_own_property: None,
        define_own_property: None,
        has_property: None,
        get: None,
        set: None,
        delete_property: None,
        own_property_keys: None,
        is_cacheable_for_inherited_property: None,
        error_data: None,
        finalize: None,
    };

    const INTERCEPTING_HOOKS: JSHostObjectHooks = JSHostObjectHooks {
        get_prototype_of: Some(intercepting_get_prototype_of),
        set_prototype_of: Some(intercepting_set_prototype_of),
        is_extensible: Some(intercepting_is_extensible),
        prevent_extensions: Some(intercepting_prevent_extensions),
        get_own_property: Some(intercepting_get_own_property),
        define_own_property: Some(intercepting_define_own_property),
        has_property: Some(intercepting_has_property),
        get: Some(intercepting_get),
        set: Some(intercepting_set),
        delete_property: Some(intercepting_delete_property),
        own_property_keys: Some(intercepting_own_property_keys),
        ..NO_HOOKS
    };

    fn object_class_with_hooks(name: &'static str, hooks: JSHostObjectHooks, flags: u32) -> &'static JSHostClass {
        let hooks: &'static JSHostObjectHooks = Box::leak(Box::new(hooks));
        leak_host_class(
            JS_HOST_CLASS_OBJECT,
            name,
            None,
            core::ptr::from_ref(hooks).cast(),
            flags,
        )
    }

    fn create_intercepting_object(environment: &HookTestEnvironment<'_>) -> Gc<HostObject> {
        let table = object_class_with_hooks("InterceptingHostObject", INTERCEPTING_HOOKS, 0);
        let object = environment.create(table, Some(environment.realm().object_prototype()));
        environment.define_global("host", object.upcast());
        object
    }

    #[test]
    fn object_hooks_answer_scripts() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        create_intercepting_object(&environment);

        assert_eq!(environment.evaluate("host.answer"), "42");
        assert_eq!(environment.evaluate("'magic' in host"), "true");
        assert_eq!(
            environment.evaluate("JSON.stringify(Object.getOwnPropertyDescriptor(host, 'virtual'))"),
            r#"{"value":"virtual value","writable":false,"enumerable":true,"configurable":true}"#
        );
        assert_eq!(
            environment.evaluate("host.intercepted = 7; host.intercepted"),
            "undefined"
        );
        assert!(Value(LAST_INTERCEPTED_VALUE.get()) == Value::from_i32(7));
        assert_eq!(
            environment.evaluate("host.undeletable = 1; delete host.undeletable"),
            "false"
        );
        assert_eq!(
            environment.evaluate("Reflect.defineProperty(host, 'rejected', { value: 1 })"),
            "false"
        );
        assert_eq!(
            environment
                .evaluate("Object.defineProperty(host, 'defined', { value: 1, enumerable: true }); host.defined"),
            "1"
        );
        assert_eq!(environment.evaluate("host.expando = 2; delete host.expando"), "true");
        assert_eq!(
            environment.evaluate("Reflect.ownKeys(host).join()"),
            "undeletable,defined,virtual"
        );
        assert_eq!(environment.evaluate("Reflect.preventExtensions(host)"), "false");
        assert_eq!(environment.evaluate("Object.isExtensible(host)"), "true");
        assert_eq!(
            environment.evaluate("Object.getPrototypeOf(host) === Object.prototype"),
            "true"
        );
        assert_eq!(
            environment
                .evaluate("const proto = { inherited: 3 }; Reflect.setPrototypeOf(host, proto) && host.inherited"),
            "3"
        );
    }

    const OPERATIONS_REPORTING_WHAT_THEY_THROW: &str = "].map(operation => { try { operation(); return 'no exception'; } catch (error) { return error.message; } }).join()";

    #[test]
    fn object_hooks_throw_into_scripts() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        create_intercepting_object(&environment);

        assert_eq!(
            environment.evaluate(&format!(
                "[() => host.throwing, () => 'throwing' in host, () => Object.getOwnPropertyDescriptor(host, 'throwing'), () => {{ host.throwing = 1; }}, () => delete host.throwing, () => Object.defineProperty(host, 'throwing', {{ value: 1 }}){OPERATIONS_REPORTING_WHAT_THEY_THROW}"
            )),
            "get threw,has_property threw,get_own_property threw,set threw,delete_property threw,define_own_property threw"
        );

        KEYLESS_HOOKS_THROW.set(true);
        let result = environment.evaluate(&format!(
            "[() => Object.getPrototypeOf(host), () => Reflect.setPrototypeOf(host, null), () => Reflect.isExtensible(host), () => Reflect.preventExtensions(host), () => Reflect.ownKeys(host){OPERATIONS_REPORTING_WHAT_THEY_THROW}"
        ));
        KEYLESS_HOOKS_THROW.set(false);
        assert_eq!(
            result,
            "get_prototype_of threw,set_prototype_of threw,is_extensible threw,prevent_extensions threw,own_property_keys threw"
        );
    }

    #[test]
    fn enumeration_goes_through_the_hooks() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);

        let intercepting = create_intercepting_object(&environment);
        assert!(!intercepting.eligible_for_own_property_enumeration_fast_path());
        assert_eq!(environment.evaluate("host.plain = 1"), "1");
        assert_eq!(environment.evaluate("Object.keys(host).join()"), "plain,virtual");
        assert_eq!(
            environment
                .evaluate("(() => { const keys = []; for (const key in host) keys.push(key); return keys.join(); })()"),
            "plain,virtual"
        );
        assert_eq!(
            environment.evaluate("JSON.stringify(host)"),
            r#"{"plain":1,"virtual":"virtual value"}"#
        );
        assert_eq!(
            environment.evaluate("Object.keys(Object.assign({}, host)).join()"),
            "plain,virtual"
        );
        assert_eq!(environment.evaluate("Object.keys({ ...host }).join()"), "plain,virtual");

        let doubling_class = object_class_with_hooks(
            "NumberDoubling",
            JSHostObjectHooks {
                get: Some(doubling_get),
                ..NO_HOOKS
            },
            0,
        );
        let doubling = environment.create(doubling_class, Some(environment.realm().object_prototype()));
        assert!(!doubling.eligible_for_own_property_enumeration_fast_path());
        environment.define_global("doubling", doubling.upcast());
        assert_eq!(environment.evaluate("doubling.number = 2; doubling.number"), "4");
        assert_eq!(environment.evaluate("JSON.stringify(doubling)"), r#"{"number":4}"#);
        assert_eq!(environment.evaluate("Object.values(doubling).join()"), "4");
        assert_eq!(environment.evaluate("Object.assign({}, doubling).number"), "4");
        assert_eq!(environment.evaluate("({ ...doubling }).number"), "4");
    }

    #[test]
    fn hand_written_descriptor_hooks() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let table = object_class_with_hooks(
            "HandWritten",
            JSHostObjectHooks {
                get_own_property: Some(report_every_property_absent),
                define_own_property: Some(accept_definition_and_zero_descriptor),
                ..NO_HOOKS
            },
            0,
        );
        let object = environment.create(table, Some(environment.realm().object_prototype()));
        environment.define_global("handWritten", object.upcast());

        assert_eq!(
            environment.evaluate("Reflect.defineProperty(handWritten, 'defined', { value: 1 })"),
            "true"
        );
        assert_eq!(environment.evaluate("handWritten.assigned = 2"), "2");
        assert_eq!(
            environment.evaluate("Object.getOwnPropertyDescriptor(handWritten, 'defined')"),
            "undefined"
        );
        assert_eq!(environment.evaluate("'assigned' in handWritten"), "false");
        assert_eq!(
            environment.evaluate("handWritten.toString === Object.prototype.toString"),
            "true"
        );
    }

    fn same_descriptor(actual: &PropertyDescriptor, expected: &PropertyDescriptor) -> bool {
        actual.value.map(|value| value.0) == expected.value.map(|value| value.0)
            && actual.get == expected.get
            && actual.set == expected.set
            && actual.writable == expected.writable
            && actual.enumerable == expected.enumerable
            && actual.configurable == expected.configurable
            && actual.property_offset == expected.property_offset
    }

    #[test]
    fn property_descriptors_round_trip_through_the_abi() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let getter = run_script(&vm, environment.realm(), "(function getter() {})")
            .must()
            .as_function();
        // SAFETY: The descriptors hold no getters or setters but `getter`, which is live.
        let round_trip = |descriptor: &PropertyDescriptor| unsafe {
            property_descriptor_from_abi(&property_descriptor_to_abi(Some(descriptor))).expect("a present descriptor")
        };

        let data_descriptor = PropertyDescriptor {
            value: Some(Value::from_f64(1.5)),
            writable: Some(true),
            enumerable: Some(false),
            configurable: Some(true),
            property_offset: Some(5),
            ..Default::default()
        };
        assert!(same_descriptor(&round_trip(&data_descriptor), &data_descriptor));
        assert_eq!(
            property_descriptor_to_abi(Some(&data_descriptor)).flags,
            JS_PD_PRESENT
                | JS_PD_HAS_VALUE
                | JS_PD_HAS_WRITABLE
                | JS_PD_WRITABLE
                | JS_PD_HAS_ENUMERABLE
                | JS_PD_HAS_CONFIGURABLE
                | JS_PD_CONFIGURABLE
                | JS_PD_HAS_PROPERTY_OFFSET
        );

        let accessor_descriptor = PropertyDescriptor {
            get: Some(Some(getter)),
            set: Some(None),
            enumerable: Some(true),
            ..Default::default()
        };
        let accessor_round_trip = round_trip(&accessor_descriptor);
        assert!(same_descriptor(&accessor_round_trip, &accessor_descriptor));
        assert!(accessor_round_trip.set.is_some());
        assert_eq!(
            property_descriptor_to_abi(Some(&accessor_descriptor)).flags,
            JS_PD_PRESENT | JS_PD_HAS_GET | JS_PD_HAS_SET | JS_PD_HAS_ENUMERABLE | JS_PD_ENUMERABLE
        );

        let empty_descriptor = PropertyDescriptor::default();
        assert!(same_descriptor(&round_trip(&empty_descriptor), &empty_descriptor));
        assert_eq!(property_descriptor_to_abi(None).flags, 0);
        // SAFETY: An absent descriptor holds nothing.
        assert!(unsafe { property_descriptor_from_abi(&property_descriptor_to_abi(None)) }.is_none());

        // A complete data descriptor from a hook takes the direct conversion, which must agree with the general one.
        let complete_data_descriptor = PropertyDescriptor {
            property_offset: None,
            ..data_descriptor
        };
        let abi_descriptor = property_descriptor_to_abi(Some(&complete_data_descriptor));
        // SAFETY: The descriptor has no getter or setter.
        let (direct, general) = unsafe {
            (
                property_descriptor_from_hook(&abi_descriptor),
                property_descriptor_from_abi(&abi_descriptor),
            )
        };
        assert!(same_descriptor(&direct.expect("present"), &general.expect("present")));
        // SAFETY: As above.
        let with_offset = unsafe { property_descriptor_from_hook(&property_descriptor_to_abi(Some(&data_descriptor))) };
        assert!(same_descriptor(&with_offset.expect("present"), &data_descriptor));
    }

    #[test]
    fn property_offsets_and_cache_metadata_pass_through_hooks() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let object = create_intercepting_object(&environment);
        let fresh = key("fresh");

        LAST_DEFINED_DESCRIPTOR.set(None);
        let mut new_property_offset = None;
        assert!(
            object
                .create_data_property(&vm, &fresh, Value::from_i32(3), Some(&mut new_property_offset), None)
                .must()
        );
        let storage_entry = object.storage_get(&vm, &fresh).expect("the property is stored");
        assert!(new_property_offset.is_some());
        assert_eq!(new_property_offset, storage_entry.property_offset);
        let defined = LAST_DEFINED_DESCRIPTOR.get().expect("the hook saw the definition");
        assert!(defined.value == Value::from_i32(3).0 && defined.flags & JS_PD_WRITABLE != 0);
        assert!(matches!(LAST_PRECOMPUTED_GET_OWN_PROPERTY.get(), Some(None)));

        // An empty precomputed [[GetOwnProperty]] result must stay distinct from none at all.
        assert_eq!(environment.evaluate("host.assigned = 1"), "1");
        let precomputed = LAST_PRECOMPUTED_GET_OWN_PROPERTY
            .get()
            .expect("the hook saw the definition");
        assert!(precomputed.is_some_and(|precomputed| precomputed.flags & JS_PD_PRESENT == 0));

        let precomputed_get_own_property = object.internal_get_own_property(&vm, &fresh).must();
        let mut redefinition = PropertyDescriptor {
            value: Some(Value::from_i32(5)),
            ..Default::default()
        };
        assert!(
            object
                .internal_define_own_property(&vm, &fresh, &mut redefinition, Some(&precomputed_get_own_property))
                .must()
        );
        let precomputed_seen_by_hook = LAST_PRECOMPUTED_GET_OWN_PROPERTY
            .get()
            .flatten()
            .expect("the hook saw the precomputed descriptor");
        // SAFETY: The descriptor is a data descriptor.
        let precomputed_seen_by_hook =
            unsafe { property_descriptor_from_abi(&precomputed_seen_by_hook) }.expect("present");
        assert!(same_descriptor(
            &precomputed_seen_by_hook,
            precomputed_get_own_property.as_ref().expect("present")
        ));
        assert!(object.get(&vm, &fresh).must() == Value::from_i32(5));
        object
            .set(
                &vm,
                &fresh,
                Value::from_i32(3),
                crate::runtime::object::ShouldThrowExceptions::Yes,
            )
            .must();

        let own_descriptor = object.internal_get_own_property(&vm, &fresh).must().expect("present");
        assert_eq!(own_descriptor.property_offset, new_property_offset);

        let receiver = Value::from_object(object);
        let mut get_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            object
                .internal_get(
                    &vm,
                    &fresh,
                    receiver,
                    Some(&mut get_metadata),
                    PropertyLookupPhase::OwnProperty
                )
                .must()
                == Value::from_i32(3)
        );
        assert_eq!(get_metadata.r#type, CacheableGetPropertyMetadataType::GetOwnProperty);
        assert_eq!(get_metadata.property_offset, new_property_offset);

        let mut set_metadata = CacheableSetPropertyMetadata::default();
        assert!(
            object
                .internal_set(
                    &vm,
                    &fresh,
                    Value::from_i32(4),
                    receiver,
                    Some(&mut set_metadata),
                    PropertyLookupPhase::OwnProperty
                )
                .must()
        );
        assert_eq!(set_metadata.r#type, CacheableSetPropertyMetadataType::ChangeOwnProperty);
        assert_eq!(set_metadata.property_offset, new_property_offset);
        assert!(object.get(&vm, &fresh).must() == Value::from_i32(4));

        let mut answer_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            object
                .internal_get(
                    &vm,
                    &key("answer"),
                    receiver,
                    Some(&mut answer_metadata),
                    PropertyLookupPhase::OwnProperty
                )
                .must()
                == Value::from_i32(42)
        );
        assert_eq!(answer_metadata.r#type, CacheableGetPropertyMetadataType::NotCacheable);
    }

    #[test]
    fn inherited_property_cacheability_comes_from_the_hook() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let inherited = key("inherited");
        let prototype = Object::create(&vm, environment.realm(), Some(environment.realm().object_prototype()));
        prototype.define_direct_property(&vm, &inherited, Value::from_i32(1), DEFAULT_ATTRIBUTES);
        let table = object_class_with_hooks(
            "InheritedCacheability",
            JSHostObjectHooks {
                is_cacheable_for_inherited_property: Some(reentering_is_cacheable_for_inherited_property),
                ..NO_HOOKS
            },
            0,
        );
        let object = environment.create(table, Some(prototype));
        let receiver = Value::from_object(object);

        INHERITED_PROPERTY_IS_CACHEABLE.set(true);
        let mut cacheable_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            object
                .internal_get(
                    &vm,
                    &inherited,
                    receiver,
                    Some(&mut cacheable_metadata),
                    PropertyLookupPhase::OwnProperty
                )
                .must()
                == Value::from_i32(1)
        );
        assert_eq!(
            cacheable_metadata.r#type,
            CacheableGetPropertyMetadataType::GetPropertyInPrototypeChain
        );

        INHERITED_PROPERTY_IS_CACHEABLE.set(false);
        let mut uncacheable_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            object
                .internal_get(
                    &vm,
                    &inherited,
                    receiver,
                    Some(&mut uncacheable_metadata),
                    PropertyLookupPhase::OwnProperty
                )
                .must()
                == Value::from_i32(1)
        );
        assert_eq!(
            uncacheable_metadata.r#type,
            CacheableGetPropertyMetadataType::NotCacheable
        );
        INHERITED_PROPERTY_IS_CACHEABLE.set(true);
    }

    #[test]
    fn error_data_hook() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let error = Error::create(&vm, environment.realm());
        let table = object_class_with_hooks(
            "ErrorDataHostObject",
            JSHostObjectHooks {
                error_data: Some(error_data_of_host_data),
                ..NO_HOOKS
            },
            0,
        );
        // SAFETY: The table is of the right kind, and the error a live cell.
        let object = unsafe {
            HostObject::create(
                &vm,
                environment.realm(),
                table,
                Some(environment.realm().object_prototype()),
                None,
                Some(error.as_non_null().cast()),
            )
        };

        assert!(object.has_error_data());
        assert!(core::ptr::eq(
            object.error_data().expect("error data"),
            error.upcast::<Object>().error_data().expect("error data")
        ));
        assert!(
            !environment
                .create(super::tests::object_class("NoFlags", None, 0), None)
                .has_error_data()
        );

        environment.define_global("errorish", object.upcast());
        assert_eq!(
            environment.evaluate("Object.prototype.toString.call(errorish)"),
            "[object Error]"
        );
    }

    #[inline(never)]
    fn allocate_unreachable_host_objects(
        environment: &HookTestEnvironment<'_>,
        table: &'static JSHostClass,
    ) -> Vec<GcWeak<HostObject>> {
        (0..32)
            .map(|_| GcWeak::new(environment.vm.heap(), environment.create(table, None)))
            .collect()
    }

    #[test]
    fn finalize_hook_runs_for_collected_objects() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let table = object_class_with_hooks(
            "CountingFinalizer",
            JSHostObjectHooks {
                finalize: Some(count_finalized_host_object),
                ..NO_HOOKS
            },
            0,
        );
        FINALIZED_HOST_OBJECTS.set(0);
        let objects = allocate_unreachable_host_objects(&environment, table);
        vm.heap().collect_garbage();
        let collected_count = objects.iter().filter(|object| object.get().is_none()).count();
        assert!(collected_count > 0);
        assert_eq!(FINALIZED_HOST_OBJECTS.get(), collected_count);
    }

    #[test]
    fn get_as_prototype_of_passes_on_only_cacheable_prototype_chain_hits() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let realm = environment.realm();
        let (own, inherited, missing) = (key("own"), key("inherited"), key("missing"));

        let prototype = Object::create(&vm, realm, Some(realm.object_prototype()));
        prototype.define_direct_property(&vm, &inherited, Value::from_i32(1), DEFAULT_ATTRIBUTES);
        let holder = Object::create(&vm, realm, Some(prototype));
        holder.define_direct_property(&vm, &own, Value::from_i32(2), DEFAULT_ATTRIBUTES);
        holder.convert_to_prototype_if_needed(&vm);
        let receiver = Value::from_object(Object::create(&vm, realm, None));

        let mut own_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            holder
                .internal_get_as_prototype_of(&vm, &own, receiver, Some(&mut own_metadata))
                .must()
                == Value::from_i32(2)
        );
        assert_eq!(
            own_metadata.r#type,
            CacheableGetPropertyMetadataType::GetPropertyInPrototypeChain
        );
        assert_eq!(own_metadata.prototype, Some(holder));
        assert_eq!(
            own_metadata.property_offset,
            holder.storage_get(&vm, &own).expect("stored").property_offset
        );

        let mut inherited_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            holder
                .internal_get_as_prototype_of(&vm, &inherited, receiver, Some(&mut inherited_metadata))
                .must()
                == Value::from_i32(1)
        );
        assert_eq!(
            inherited_metadata.r#type,
            CacheableGetPropertyMetadataType::GetPropertyInPrototypeChain
        );
        assert_eq!(inherited_metadata.prototype, Some(prototype));

        // A miss is cacheable on its own terms, but not as a hit to pass on, so the caller's metadata stays as it was.
        let mut untouched_metadata = CacheableGetPropertyMetadata {
            r#type: CacheableGetPropertyMetadataType::GetOwnProperty,
            property_offset: Some(7),
            ..Default::default()
        };
        assert!(
            holder
                .internal_get_as_prototype_of(&vm, &missing, receiver, Some(&mut untouched_metadata))
                .must()
                == Value::UNDEFINED
        );
        assert_eq!(
            untouched_metadata.r#type,
            CacheableGetPropertyMetadataType::GetOwnProperty
        );
        assert_eq!(untouched_metadata.property_offset, Some(7));

        // So does a value that a host hook produced without filling the metadata.
        let host = create_intercepting_object(&environment);
        let mut host_metadata = CacheableGetPropertyMetadata::default();
        assert!(
            host.internal_get_as_prototype_of(&vm, &key("answer"), receiver, Some(&mut host_metadata))
                .must()
                == Value::from_i32(42)
        );
        assert_eq!(host_metadata.r#type, CacheableGetPropertyMetadataType::NotCacheable);

        assert!(holder.internal_get_as_prototype_of(&vm, &own, receiver, None).must() == Value::from_i32(2));

        // A hook that forwards to the holder that way gets its lookups cached, so that bytecode stops calling it.
        let forwarding_class = object_class_with_hooks(
            "Forwarding",
            JSHostObjectHooks {
                get: Some(forwarding_get),
                ..NO_HOOKS
            },
            0,
        );
        // SAFETY: The table is of the right kind, and the holder a live cell.
        let forwarding = unsafe {
            HostObject::create(
                &vm,
                realm,
                forwarding_class,
                Some(realm.object_prototype()),
                None,
                Some(holder.as_non_null().cast()),
            )
        };
        environment.define_global("forwarding", forwarding.upcast());
        HOOKS_THAT_REENTERED.with_borrow_mut(Vec::clear);
        SCRIPT_FOR_HOOKS_TO_REENTER_WITH.set(Some("0"));
        assert_eq!(
            environment
                .evaluate("(() => { let sum = 0; for (let i = 0; i < 10; ++i) sum += forwarding.own; return sum; })()"),
            "20"
        );
        SCRIPT_FOR_HOOKS_TO_REENTER_WITH.set(None);
        assert_eq!(HOOKS_THAT_REENTERED.with_borrow(Vec::len), 1);
    }

    #[test]
    fn every_hook_may_reenter_the_vm() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let host = create_intercepting_object(&environment);
        let cacheability_class = object_class_with_hooks(
            "ReenteringCacheability",
            JSHostObjectHooks {
                is_cacheable_for_inherited_property: Some(reentering_is_cacheable_for_inherited_property),
                ..NO_HOOKS
            },
            0,
        );
        let inheriting = environment.create(cacheability_class, Some(host.upcast()));
        environment.define_global("inheriting", inheriting.upcast());
        let error = Error::create(&vm, environment.realm());
        let error_data_class = object_class_with_hooks(
            "ReenteringErrorData",
            JSHostObjectHooks {
                error_data: Some(error_data_of_host_data),
                ..NO_HOOKS
            },
            0,
        );
        // SAFETY: The table is of the right kind, and the error a live cell.
        let errorish = unsafe {
            HostObject::create(
                &vm,
                environment.realm(),
                error_data_class,
                None,
                None,
                Some(error.as_non_null().cast()),
            )
        };
        environment.define_global("errorish", errorish.upcast());

        // The script reaches the hooks again from inside them, and makes garbage for the collection that follows.
        let hooks_that_reentered = hooks_that_reentered_while(REENTRANT_SCRIPT, || {
            assert_eq!(
                environment.evaluate(
                    "Object.getPrototypeOf(host); Reflect.setPrototypeOf(host, Object.prototype); Reflect.isExtensible(host); \
                     Reflect.preventExtensions(host); Object.getOwnPropertyDescriptor(host, 'x'); Object.defineProperty(host, 'x', { value: 1, configurable: true }); \
                     'x' in host; host.x; host.y = 2; delete host.y; Reflect.ownKeys(host); \
                     host.toString; inheriting.toString; Object.prototype.toString.call(errorish)"
                ),
                "[object Error]"
            );
        });
        assert_eq!(
            hooks_that_reentered,
            [
                "define_own_property",
                "delete_property",
                "error_data",
                "get",
                "get_own_property",
                "get_prototype_of",
                "has_property",
                "is_cacheable_for_inherited_property",
                "is_extensible",
                "own_property_keys",
                "prevent_extensions",
                "set",
                "set_prototype_of",
            ]
        );
        assert!(
            environment
                .evaluate("globalThis.reentered")
                .parse::<u32>()
                .expect("a count")
                >= 13
        );
        assert_eq!(environment.evaluate("host.x"), "1");
    }
}
