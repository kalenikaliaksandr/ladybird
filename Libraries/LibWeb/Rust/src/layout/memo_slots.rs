/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::cell::RefCell;

/// The cells of a memo that the runs of a layout pass share, one per slot of the arena. The layout stage provides a cell
/// for every slot before each pass, so a pass writes cells but never adds any, and runs that write different slots share
/// no state.
#[derive(Clone)]
pub(crate) struct MemoSlots<T>(Vec<RefCell<T>>);

impl<T> Default for MemoSlots<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: Default> MemoSlots<T> {
    /// Provides a cell for every slot below `capacity`, and keeps what the cells hold.
    pub(crate) fn provide_up_to(&mut self, capacity: usize) {
        if self.0.len() < capacity {
            self.0.resize_with(capacity, RefCell::default);
        }
    }

    #[cfg(test)]
    pub(crate) fn with_capacity_for_test(capacity: usize) -> Self {
        let mut slots = Self::default();
        slots.provide_up_to(capacity);
        slots
    }
}

impl<T> MemoSlots<T> {
    /// The cell of `slot` to read, where one was provided.
    pub(crate) fn get(&self, slot: u32) -> Option<&RefCell<T>> {
        self.0.get(slot as usize)
    }

    /// The cell of `slot` to write, which the layout stage provided before the pass.
    #[track_caller]
    pub(crate) fn cell_to_write(&self, slot: u32) -> &RefCell<T> {
        self.0.get(slot as usize).unwrap_or_else(|| {
            panic!(
                "memo slot {slot} lies beyond the {} slots provided before the pass",
                self.0.len()
            )
        })
    }

    /// What `slot` holds, to change between passes.
    pub(crate) fn get_mut(&mut self, slot: u32) -> Option<&mut T> {
        self.0.get_mut(slot as usize).map(RefCell::get_mut)
    }
}
