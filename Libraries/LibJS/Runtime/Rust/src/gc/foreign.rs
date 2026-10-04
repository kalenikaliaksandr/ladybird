/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! References from the runtime's cells to the cells of an embedder's C++ types, which LibGC allocates in the same heap.

use core::cell::Cell;
use core::ffi::c_void;
use core::ptr::NonNull;

use super::capi::gc_visitor_visit_cell;
use super::visitor::{Trace, Visitor};

/// A reference to a C++ GC cell, such as the implementation object a host object wraps, which keeps that cell alive
/// for as long as the cell or root holding the slot is.
#[derive(Default)]
#[repr(transparent)]
pub struct ForeignCellSlot(Cell<Option<NonNull<c_void>>>);

impl ForeignCellSlot {
    pub const fn empty() -> Self {
        Self(Cell::new(None))
    }

    pub fn get(&self) -> Option<NonNull<c_void>> {
        self.0.get()
    }

    /// The cell, or null, as the C ABI passes it.
    pub fn as_ptr(&self) -> *mut c_void {
        self.get().map_or(core::ptr::null_mut(), NonNull::as_ptr)
    }

    /// # Safety
    ///
    /// `cell` must be a live cell of the heap the runtime allocates from, C++ or Rust, which the slot then keeps alive.
    pub unsafe fn set(&self, cell: Option<NonNull<c_void>>) {
        self.0.set(cell);
    }
}

// SAFETY: Visits the one cell the slot holds.
unsafe impl Trace for ForeignCellSlot {
    fn trace(&self, visitor: &mut Visitor) {
        if let Some(cell) = self.get() {
            // SAFETY: The visitor is live, and set() only stores cells of its heap, which the slot has kept alive.
            unsafe { gc_visitor_visit_cell(visitor.as_raw(), cell.as_ptr()) };
        }
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::ForeignCellSlot;
    use crate::gc::root::Root;
    use crate::gc::weak::GcWeak;
    use crate::interpreter::vm::Vm;
    use crate::runtime::object::Object;
    use crate::runtime::realm::test_realm::TestRealm;

    const OBJECT_COUNT: usize = 64;

    struct Objects<'vm> {
        held_by_slots: Root<'vm, Vec<ForeignCellSlot>>,
        weakly_held_through_slots: Vec<GcWeak<Object>>,
        held_by_nothing: Vec<GcWeak<Object>>,
    }

    #[inline(never)]
    fn create_objects<'vm>(vm: &'vm Vm, test_realm: &TestRealm) -> Objects<'vm> {
        let held_by_slots = Root::new(
            vm,
            (0..OBJECT_COUNT).map(|_| ForeignCellSlot::empty()).collect::<Vec<_>>(),
        );
        let mut weakly_held_through_slots = Vec::new();
        let mut held_by_nothing = Vec::new();
        for slot in held_by_slots.get() {
            let object = test_realm.object();
            // SAFETY: The object is a live cell of the VM's heap.
            unsafe { slot.set(Some(object.as_non_null().cast())) };
            weakly_held_through_slots.push(GcWeak::new(vm.heap(), object));
            held_by_nothing.push(GcWeak::new(vm.heap(), test_realm.object()));
        }
        Objects {
            held_by_slots,
            weakly_held_through_slots,
            held_by_nothing,
        }
    }

    #[test]
    fn a_rooted_slot_keeps_its_cell_alive() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let objects = create_objects(&vm, &test_realm);
        vm.heap().collect_garbage();

        let dead_objects_held_by_nothing = objects
            .held_by_nothing
            .iter()
            .filter(|object| object.get().is_none())
            .count();
        assert!(
            dead_objects_held_by_nothing >= OBJECT_COUNT / 2,
            "only {dead_objects_held_by_nothing} of the objects nothing holds were collected"
        );
        assert!(
            objects
                .weakly_held_through_slots
                .iter()
                .all(|object| object.get().is_some())
        );
        for (slot, object) in objects
            .held_by_slots
            .get()
            .iter()
            .zip(&objects.weakly_held_through_slots)
        {
            assert_eq!(
                slot.as_ptr(),
                object.get().expect("the object is alive").as_ptr().cast()
            );
        }
    }
}
