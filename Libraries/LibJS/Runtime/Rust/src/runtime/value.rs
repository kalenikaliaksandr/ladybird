/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::ptr::NonNull;

use crate::build_configuration::HEAP_REGION_OFFSET_MASK;
use crate::gc::capi::js_heap_region_base;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::primitive_string::PrimitiveString;
use crate::layout::value::Value;
use libjs_abi::value as nan_box;

impl Value {
    pub const fn from_bool(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }

    pub const fn from_i32(value: i32) -> Self {
        Self(nan_box::SHIFTED_INT32_TAG | value as u32 as u64)
    }

    /// Like the C++ Value(double): integral doubles that fit in an i32, other than negative zero, are stored as
    /// Int32, and every NaN becomes the canonical NaN.
    pub fn from_f64(value: f64) -> Self {
        let is_negative_zero = value.to_bits() == nan_box::NEGATIVE_ZERO_BITS;
        if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) && value.trunc() == value && !is_negative_zero {
            return Self::from_i32(value as i32);
        }
        if value.is_nan() {
            return Self(nan_box::CANON_NAN_BITS);
        }
        Self(value.to_bits())
    }

    pub const fn tag(self) -> u64 {
        self.0 >> nan_box::TAG_SHIFT
    }

    pub const fn is_empty(self) -> bool {
        self.0 == nan_box::EMPTY_VALUE
    }

    pub const fn is_undefined(self) -> bool {
        self.0 == nan_box::UNDEFINED_VALUE
    }

    pub const fn is_null(self) -> bool {
        self.0 == nan_box::NULL_VALUE
    }

    pub const fn is_nullish(self) -> bool {
        (self.tag() & nan_box::IS_NULLISH_EXTRACT_PATTERN) == nan_box::IS_NULLISH_PATTERN
    }

    pub const fn is_boolean(self) -> bool {
        self.tag() == nan_box::BOOLEAN_TAG
    }

    pub const fn is_int32(self) -> bool {
        self.tag() == nan_box::INT32_TAG
    }

    pub const fn is_double(self) -> bool {
        (self.0 & nan_box::CANON_NAN_BITS) != nan_box::CANON_NAN_BITS || self.0 == nan_box::CANON_NAN_BITS
    }

    pub const fn is_number(self) -> bool {
        self.is_double() || self.is_int32()
    }

    pub const fn is_cell(self) -> bool {
        (self.tag() & nan_box::IS_CELL_PATTERN) == nan_box::IS_CELL_PATTERN
    }

    pub const fn is_object(self) -> bool {
        self.tag() == nan_box::OBJECT_TAG
    }

    pub const fn is_string(self) -> bool {
        self.tag() == nan_box::STRING_TAG
    }

    pub const fn is_symbol(self) -> bool {
        self.tag() == nan_box::SYMBOL_TAG
    }

    pub const fn is_bigint(self) -> bool {
        self.tag() == nan_box::BIGINT_TAG
    }

    pub const fn is_accessor(self) -> bool {
        self.tag() == nan_box::ACCESSOR_TAG
    }

    pub fn as_bool(self) -> bool {
        assert!(self.is_boolean());
        self.0 & 1 != 0
    }

    pub fn as_i32(self) -> i32 {
        debug_assert!(self.is_int32());
        self.0 as u32 as i32
    }

    /// The numeric value of a number.
    pub fn as_f64(self) -> f64 {
        debug_assert!(self.is_number());
        if self.is_int32() {
            return f64::from(self.as_i32());
        }
        f64::from_bits(self.0)
    }

    fn with_cell_tag<T>(tag: u64, cell: Gc<T>) -> Self {
        let address = cell.as_ptr() as usize as u64;
        Self((tag << nan_box::TAG_SHIFT) | (address & HEAP_REGION_OFFSET_MASK))
    }

    /// # Safety
    ///
    /// The value must hold a cell of type T.
    unsafe fn cell<T>(self) -> Gc<T> {
        debug_assert!(self.is_cell());
        // SAFETY: Cell values are offsets into the heap region, whose base LibGC fixed before any cell existed.
        let base = unsafe { js_heap_region_base } as u64;
        let address = (base + (self.0 & HEAP_REGION_OFFSET_MASK)) as usize;
        // SAFETY: The caller guarantees the value holds a live cell of type T.
        unsafe { Gc::from_non_null(NonNull::new_unchecked(core::ptr::without_provenance_mut::<T>(address))) }
    }

    pub fn from_object<T>(object: Gc<T>) -> Self {
        Self::with_cell_tag(nan_box::OBJECT_TAG, object)
    }

    pub fn from_string(string: Gc<PrimitiveString>) -> Self {
        Self::with_cell_tag(nan_box::STRING_TAG, string)
    }

    pub fn as_object(self) -> Gc<Object> {
        assert!(self.is_object());
        // SAFETY: The tag says the value holds an object.
        unsafe { self.cell() }
    }

    pub fn as_string(self) -> Gc<PrimitiveString> {
        assert!(self.is_string());
        // SAFETY: The tag says the value holds a string.
        unsafe { self.cell() }
    }
}

impl core::fmt::Debug for Value {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "Value(0x{:016x})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_the_cpp_encoding() {
        assert!(Value::from_f64(2.0).is_int32());
        assert_eq!(Value::from_f64(2.0).as_i32(), 2);
        assert!(Value::from_f64(-0.0).is_double());
        assert_eq!(Value::from_f64(f64::NAN).0, nan_box::CANON_NAN_BITS);
        assert!(Value::from_f64(0.5).is_double());
        assert!(Value::from_f64(f64::from(i32::MAX) + 1.0).is_double());
        assert!(Value::UNDEFINED.is_nullish() && Value::NULL.is_nullish() && !Value::FALSE.is_nullish());
        assert!(!Value::EMPTY.is_number() && Value::from_i32(-1).is_number());
    }
}
