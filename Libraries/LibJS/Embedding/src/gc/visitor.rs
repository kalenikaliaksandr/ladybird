/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Tracing the cells and values that a Rust cell holds.

use core::ffi::c_void;
use core::ptr::NonNull;

use ak::{Utf16FlyString, Utf16String};

use crate::abi::GCVisitor;
use crate::gc::capi::{gc_visitor_visit_cell, gc_visitor_visit_values};
use crate::object::Object;
use crate::value::Value;

/// The visitor that LibGC passes to the visit_edges function of a cell type.
pub struct Visitor {
    raw: NonNull<GCVisitor>,
}

impl Visitor {
    /// # Safety
    ///
    /// `raw` must be the visitor LibGC passed, for the duration of the visit.
    pub(crate) unsafe fn from_raw(raw: *mut GCVisitor) -> Self {
        Self {
            raw: NonNull::new(raw).expect("LibGC passes a visitor"),
        }
    }

    /// # Safety
    ///
    /// `cell` must be null or the address of a live cell.
    pub unsafe fn visit_cell(&mut self, cell: *mut c_void) {
        if cell.is_null() {
            return;
        }
        // SAFETY: The visitor is live during the visit, and the caller guarantees that the cell is live.
        unsafe { gc_visitor_visit_cell(self.raw.as_ptr(), cell) };
    }

    pub fn visit_value(&mut self, value: Value) {
        let bits = value.raw();
        // SAFETY: The visitor is live during the visit, and the value is one the visited cell holds.
        unsafe { gc_visitor_visit_values(self.raw.as_ptr(), &raw const bits, 1) };
    }
}

/// A type whose values a Rust cell can hold, which visits every cell and value it holds.
///
/// # Safety
///
/// trace() must visit every cell and every value that holds a cell, or LibGC frees them while they are in use.
pub unsafe trait Trace {
    fn trace(&self, visitor: &mut Visitor);
}

macro_rules! trace_nothing {
    ($($type:ty),* $(,)?) => {
        $(
            // SAFETY: The type holds no cells.
            unsafe impl Trace for $type {
                fn trace(&self, _: &mut Visitor) {}
            }
        )*
    };
}

trace_nothing!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize,
    f32,
    f64,
    char,
    String,
    Utf16String,
    Utf16FlyString,
);

// SAFETY: Visits the value.
unsafe impl Trace for Value {
    fn trace(&self, visitor: &mut Visitor) {
        visitor.visit_value(*self);
    }
}

// SAFETY: Visits the object.
unsafe impl Trace for Object {
    fn trace(&self, visitor: &mut Visitor) {
        // SAFETY: An object is a live cell.
        unsafe { visitor.visit_cell(self.as_raw().cast()) };
    }
}

// SAFETY: Traces what the option holds.
unsafe impl<T: Trace> Trace for Option<T> {
    fn trace(&self, visitor: &mut Visitor) {
        if let Some(value) = self {
            value.trace(visitor);
        }
    }
}

// SAFETY: Traces every element.
unsafe impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, visitor: &mut Visitor) {
        for value in self {
            value.trace(visitor);
        }
    }
}

// SAFETY: Traces what the box holds.
unsafe impl<T: Trace + ?Sized> Trace for Box<T> {
    fn trace(&self, visitor: &mut Visitor) {
        (**self).trace(visitor);
    }
}

// SAFETY: Traces what the cell holds.
unsafe impl<T: Trace + Copy> Trace for core::cell::Cell<T> {
    fn trace(&self, visitor: &mut Visitor) {
        self.get().trace(visitor);
    }
}
