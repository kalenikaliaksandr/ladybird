/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::mem::ManuallyDrop;
use std::ptr::NonNull;

/// A growable array with a C layout.
///
/// This crate is compiled into more than one library, and a path that one copy makes can be
/// changed or freed by another. All copies allocate through the same allocator, but only a C
/// layout is sure to be the same in each copy, so this keeps the parts of a `Vec` in fields of
/// its own.
#[repr(C)]
pub struct RawBuffer<T: Copy> {
    pointer: NonNull<T>,
    length: usize,
    capacity: usize,
}

impl<T: Copy> RawBuffer<T> {
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    pub fn from_vec(vec: Vec<T>) -> Self {
        let mut vec = ManuallyDrop::new(vec);
        Self {
            // SAFETY: A Vec's pointer is never null, also when it has no allocation.
            pointer: unsafe { NonNull::new_unchecked(vec.as_mut_ptr()) },
            length: vec.len(),
            capacity: vec.capacity(),
        }
    }

    fn with_vec<R>(&mut self, operation: impl FnOnce(&mut Vec<T>) -> R) -> R {
        // SAFETY: The parts came from a Vec of the same element type, made with the allocator
        // that every copy of this crate uses.
        let mut vec =
            ManuallyDrop::new(unsafe { Vec::from_raw_parts(self.pointer.as_ptr(), self.length, self.capacity) });
        let result = operation(&mut vec);
        // SAFETY: As in from_vec().
        self.pointer = unsafe { NonNull::new_unchecked(vec.as_mut_ptr()) };
        self.length = vec.len();
        self.capacity = vec.capacity();
        result
    }

    pub fn as_slice(&self) -> &[T] {
        // SAFETY: The first `length` elements are initialized, and the pointer is valid for them.
        unsafe { ak::slice_from_raw_parts(self.pointer.as_ptr(), self.length) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: As above, and `&mut self` makes the access unique.
        unsafe { ak::slice_from_raw_parts_mut(self.pointer.as_ptr(), self.length) }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn last(&self) -> Option<&T> {
        self.as_slice().last()
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.as_mut_slice().last_mut()
    }

    pub fn push(&mut self, value: T) {
        self.with_vec(|vec| vec.push(value));
    }

    pub fn extend_from_slice(&mut self, values: &[T]) {
        if !values.is_empty() {
            self.with_vec(|vec| vec.extend_from_slice(values));
        }
    }

    /// Appends `count` copies of `value` and returns them, so that a caller can write them.
    pub fn push_n(&mut self, count: usize, value: T) -> &mut [T] {
        let start = self.length;
        self.with_vec(|vec| vec.resize(start + count, value));
        &mut self.as_mut_slice()[start..]
    }

    pub fn clear(&mut self) {
        self.length = 0;
    }

    /// Keeps the first `length` elements.
    pub fn truncate(&mut self, length: usize) {
        self.length = self.length.min(length);
    }
}

impl<T: Copy> Default for RawBuffer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> Clone for RawBuffer<T> {
    fn clone(&self) -> Self {
        Self::from_vec(self.as_slice().to_vec())
    }
}

impl<T: Copy> Drop for RawBuffer<T> {
    fn drop(&mut self) {
        // SAFETY: See with_vec(). Nothing uses the parts after this.
        drop(unsafe { Vec::from_raw_parts(self.pointer.as_ptr(), self.length, self.capacity) });
    }
}

// SAFETY: A RawBuffer owns its elements like a Vec does.
unsafe impl<T: Copy + Send> Send for RawBuffer<T> {}
// SAFETY: As above.
unsafe impl<T: Copy + Sync> Sync for RawBuffer<T> {}
