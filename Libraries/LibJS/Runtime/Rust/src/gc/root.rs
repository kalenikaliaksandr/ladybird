/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Precise roots for values that live outside cells and outside the stack, where the collector would not find them.

use core::cell::RefCell;

use super::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;

/// The roots a VM keeps alive. Entries are boxed values registered by address, so slots never move.
#[derive(Default)]
pub struct RootSet {
    entries: RefCell<Vec<Option<*const dyn Trace>>>,
    free_slots: RefCell<Vec<usize>>,
}

impl RootSet {
    fn register(&self, value: *const dyn Trace) -> usize {
        let mut entries = self.entries.borrow_mut();
        if let Some(slot) = self.free_slots.borrow_mut().pop() {
            entries[slot] = Some(value);
            return slot;
        }
        entries.push(Some(value));
        entries.len() - 1
    }

    fn unregister(&self, slot: usize) {
        self.entries.borrow_mut()[slot] = None;
        self.free_slots.borrow_mut().push(slot);
    }

    pub fn trace(&self, visitor: &mut Visitor) {
        for value in self.entries.borrow().iter().flatten() {
            // SAFETY: A registered value stays alive until its root unregisters it.
            unsafe { &**value }.trace(visitor);
        }
    }
}

/// Keeps a value and every cell it reaches alive for as long as it exists.
pub struct Root<'vm, T: Trace + 'static> {
    vm: &'vm Vm,
    value: Box<T>,
    slot: usize,
}

impl<'vm, T: Trace + 'static> Root<'vm, T> {
    pub fn new(vm: &'vm Vm, value: T) -> Self {
        let value = Box::new(value);
        let slot = vm
            .roots()
            .register(core::ptr::from_ref::<T>(&value) as *const dyn Trace);
        Self { vm, value, slot }
    }

    pub fn get(&self) -> &T {
        &self.value
    }
}

impl<T: Trace + Copy + 'static> Root<'_, T> {
    pub fn value(&self) -> T {
        *self.value
    }
}

impl<T: Trace + 'static> Drop for Root<'_, T> {
    fn drop(&mut self) {
        self.vm.roots().unregister(self.slot);
    }
}

/// A growable list of values that are kept alive, for the temporaries a Vec would hide from the collector. Elements
/// are copied in and out, so no reference into the list outlives a call.
pub struct MarkedVec<'vm, T: Trace + Clone + 'static> {
    values: Root<'vm, RefCell<Vec<T>>>,
}

// SAFETY: Forwards to the values; the borrow is never held across anything that collects.
unsafe impl<T: Trace> Trace for RefCell<Vec<T>> {
    fn trace(&self, visitor: &mut Visitor) {
        self.borrow().trace(visitor);
    }
}

impl<'vm, T: Trace + Clone + 'static> MarkedVec<'vm, T> {
    pub fn new(vm: &'vm Vm) -> Self {
        Self {
            values: Root::new(vm, RefCell::new(Vec::new())),
        }
    }

    pub fn with_capacity(vm: &'vm Vm, capacity: usize) -> Self {
        Self {
            values: Root::new(vm, RefCell::new(Vec::with_capacity(capacity))),
        }
    }

    pub fn push(&self, value: T) {
        self.values.get().borrow_mut().push(value);
    }

    pub fn insert(&self, index: usize, value: T) {
        self.values.get().borrow_mut().insert(index, value);
    }

    pub fn pop(&self) -> Option<T> {
        self.values.get().borrow_mut().pop()
    }

    pub fn get(&self, index: usize) -> Option<T> {
        self.values.get().borrow().get(index).cloned()
    }

    pub fn set(&self, index: usize, value: T) {
        self.values.get().borrow_mut()[index] = value;
    }

    pub fn len(&self) -> usize {
        self.values.get().borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Lends the values to `callback`, which must not change the list.
    pub fn with_values<R>(&self, callback: impl FnOnce(&[T]) -> R) -> R {
        callback(&self.values.get().borrow())
    }

    /// Copies the values out, for handing them to code that does not collect.
    pub fn to_vec(&self) -> Vec<T> {
        self.values.get().borrow().clone()
    }
}
