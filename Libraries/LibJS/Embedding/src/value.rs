/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! JS values, NaN-boxed as LibGC/NanBoxedValue.h and LibJS/Runtime/Value.h encode them.

use core::marker::PhantomData;
use core::ptr::NonNull;

use libjs_abi::value as nan_box;

use crate::abi::{JSPrimitiveString, JSValue};
use crate::gc::capi::{GCLayout, gc_get_layout, heap_region_base};
use crate::layout::HEAP_REGION_OFFSET_MASK;
use crate::object::Object;

/// A JS value. A value that holds a cell keeps the cell alive only while it is on the stack, where the conservative
/// stack scan finds it, so a value must not be kept anywhere else.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Value {
    bits: JSValue,
    _not_send: PhantomData<*const ()>,
}

const _: () = assert!(size_of::<Value>() == size_of::<JSValue>());

impl Value {
    pub const UNDEFINED: Value = Value::from_raw(nan_box::UNDEFINED_VALUE);
    pub const NULL: Value = Value::from_raw(nan_box::NULL_VALUE);

    pub const fn from_raw(bits: JSValue) -> Self {
        Self {
            bits,
            _not_send: PhantomData,
        }
    }

    pub const fn raw(self) -> JSValue {
        self.bits
    }

    pub const fn from_bool(value: bool) -> Self {
        Self::from_raw(if value {
            nan_box::TRUE_VALUE
        } else {
            nan_box::FALSE_VALUE
        })
    }

    /// Integral doubles that fit in an i32, other than negative zero, are stored as Int32, and every NaN becomes the
    /// canonical NaN, as the runtime stores them.
    pub fn from_f64(value: f64) -> Self {
        let is_negative_zero = value.to_bits() == nan_box::NEGATIVE_ZERO_BITS;
        if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) && value.trunc() == value && !is_negative_zero {
            return Self::from_raw(nan_box::SHIFTED_INT32_TAG | u64::from(value as i32 as u32));
        }
        if value.is_nan() {
            return Self::from_raw(nan_box::CANON_NAN_BITS);
        }
        Self::from_raw(value.to_bits())
    }

    pub fn from_object(object: Object) -> Self {
        Self::with_cell_tag(nan_box::OBJECT_TAG, object.as_raw().cast())
    }

    /// # Safety
    ///
    /// `string` must be the address of a live string cell.
    pub unsafe fn from_primitive_string(string: NonNull<JSPrimitiveString>) -> Self {
        Self::with_cell_tag(nan_box::STRING_TAG, string.as_ptr().cast())
    }

    fn with_cell_tag(tag: u64, cell: *mut u8) -> Self {
        Self::from_raw((tag << nan_box::TAG_SHIFT) | (cell as usize as u64 & HEAP_REGION_OFFSET_MASK))
    }

    const fn tag(self) -> u64 {
        self.bits >> nan_box::TAG_SHIFT
    }

    pub const fn is_empty(self) -> bool {
        self.bits == nan_box::EMPTY_VALUE
    }

    pub const fn is_undefined(self) -> bool {
        self.bits == nan_box::UNDEFINED_VALUE
    }

    pub const fn is_nullish(self) -> bool {
        (self.tag() & nan_box::IS_NULLISH_EXTRACT_PATTERN) == nan_box::IS_NULLISH_PATTERN
    }

    pub const fn is_object(self) -> bool {
        self.tag() == nan_box::OBJECT_TAG
    }

    pub fn as_object(self) -> Option<Object> {
        if !self.is_object() {
            return None;
        }
        // SAFETY: An object value holds the offset of a live object in the heap region.
        Some(unsafe { Object::from_non_null(NonNull::new_unchecked(decode_cell_address(self.bits) as *mut _)) })
    }
}

/// Checks that the mask that encodes cells in values, which the crate computes when it is built, is the one of the LibGC
/// that the process uses. A build of the crate with other features than LibGC's would encode wrong addresses.
pub fn check_cell_encoding_against_libgc() {
    let mut layout = GCLayout::default();
    // SAFETY: The layout is valid for writes.
    unsafe { gc_get_layout(&raw mut layout) };
    assert_eq!(
        layout.heap_region_offset_mask, HEAP_REGION_OFFSET_MASK,
        "libjs_embedding encodes cells in values with the heap region mask of another LibGC"
    );
}

/// Makes the whole encoded value live up to this point, and the decoded address from this point on. The conservative
/// stack scan recognizes a cell by its tagged value or by its address, but not by its bare heap offset, which the
/// compiler could otherwise keep instead of the value or the address while either is held across an allocation.
#[inline(always)]
fn decode_cell_address(encoded: u64) -> usize {
    let base = heap_region_base() as u64;
    let mut address = base + (encoded & HEAP_REGION_OFFSET_MASK);
    // SAFETY: The assembly is empty: it only has the compiler put `encoded` and `address` in registers here.
    unsafe {
        core::arch::asm!(
            "/* {encoded} {address} */",
            encoded = in(reg) encoded,
            address = inout(reg) address,
            options(nomem, nostack, preserves_flags),
        );
    }
    address as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_stored_as_the_runtime_stores_them() {
        assert_eq!(Value::from_f64(42.0).raw(), nan_box::SHIFTED_INT32_TAG | 42);
        assert_eq!(
            Value::from_f64(-1.0).raw(),
            nan_box::SHIFTED_INT32_TAG | u64::from(u32::MAX)
        );
        assert_eq!(Value::from_f64(-0.0).raw(), nan_box::NEGATIVE_ZERO_BITS);
        assert_eq!(Value::from_f64(0.5).raw(), 0.5f64.to_bits());
        assert_eq!(Value::from_f64(f64::NAN).raw(), nan_box::CANON_NAN_BITS);
        assert_eq!(Value::from_f64(2_147_483_648.0).raw(), 2_147_483_648.0f64.to_bits());
    }

    #[test]
    fn nullish_values() {
        assert!(Value::UNDEFINED.is_nullish());
        assert!(Value::NULL.is_nullish());
        assert!(!Value::from_bool(false).is_nullish());
        assert!(!Value::UNDEFINED.is_object());
    }
}
