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
