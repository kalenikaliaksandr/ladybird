/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/Value.cpp the runtime implements so far.

use core::fmt::{self, Write};
use core::ptr::NonNull;

use crate::build_configuration::HEAP_REGION_OFFSET_MASK;
use crate::gc::capi::js_heap_region_base;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::primitive_string::PrimitiveString;
use crate::layout::value::Value;
use crate::runtime::accessor::Accessor;
use crate::runtime::big_int::BigInt;
use crate::runtime::symbol::Symbol;
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

    pub fn is_integral_number(self) -> bool {
        if self.is_int32() {
            return true;
        }
        self.is_finite_number() && self.as_f64().trunc() == self.as_f64()
    }

    pub fn is_finite_number(self) -> bool {
        if !self.is_number() {
            return false;
        }
        if self.is_int32() {
            return true;
        }
        self.as_f64().is_finite()
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

    pub fn from_symbol(symbol: Gc<Symbol>) -> Self {
        Self::with_cell_tag(nan_box::SYMBOL_TAG, symbol)
    }

    pub fn from_bigint(bigint: Gc<BigInt>) -> Self {
        Self::with_cell_tag(nan_box::BIGINT_TAG, bigint)
    }

    pub fn from_accessor(accessor: Gc<Accessor>) -> Self {
        Self::with_cell_tag(nan_box::ACCESSOR_TAG, accessor)
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

    pub fn as_symbol(self) -> Gc<Symbol> {
        assert!(self.is_symbol());
        // SAFETY: The tag says the value holds a symbol.
        unsafe { self.cell() }
    }

    pub fn as_bigint(self) -> Gc<BigInt> {
        assert!(self.is_bigint());
        // SAFETY: The tag says the value holds a BigInt.
        unsafe { self.cell() }
    }

    pub fn as_accessor(self) -> Gc<Accessor> {
        assert!(self.is_accessor());
        // SAFETY: The tag says the value holds an accessor.
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

    #[test]
    fn integral_numbers_are_finite_and_whole() {
        assert!(Value::from_i32(-7).is_integral_number());
        assert!(Value::from_f64(-0.0).is_integral_number());
        assert!(Value::from_f64(1e300).is_integral_number());
        assert!(!Value::from_f64(0.5).is_integral_number());
        assert!(!Value::from_f64(f64::INFINITY).is_integral_number());
        assert!(!Value::from_f64(f64::NAN).is_integral_number());
        assert!(!Value::UNDEFINED.is_integral_number() && !Value::UNDEFINED.is_finite_number());
    }
}

/// The text and UTF-16 builders Number::toString writes its ASCII output into.
pub trait NumberStringBuilder {
    fn append_ascii(&mut self, text: &[u8]);
    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize);
}

impl NumberStringBuilder for String {
    fn append_ascii(&mut self, text: &[u8]) {
        self.extend(text.iter().copied().map(char::from));
    }

    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize) {
        self.extend(core::iter::repeat_n(char::from(code_unit), count));
    }
}

impl NumberStringBuilder for Vec<u16> {
    fn append_ascii(&mut self, text: &[u8]) {
        self.extend(text.iter().copied().map(u16::from));
    }

    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize) {
        self.extend(core::iter::repeat_n(u16::from(code_unit), count));
    }
}

pub fn number_to_string(value: f64) -> String {
    let mut builder = String::new();
    append_number_to_string(&mut builder, value);
    builder
}

pub fn number_to_utf16_string(value: f64) -> Vec<u16> {
    let mut builder = Vec::new();
    append_number_to_string(&mut builder, value);
    builder
}

// 6.1.6.1.20 Number::toString ( x ), https://tc39.es/ecma262/#sec-numeric-types-number-tostring
// Implementation for radix = 10
pub fn append_number_to_string(builder: &mut impl NumberStringBuilder, value: f64) {
    // 1. If x is NaN, return "NaN".
    if value.is_nan() {
        builder.append_ascii(b"NaN");
        return;
    }

    // 2. If x is +0𝔽 or -0𝔽, return "0".
    if value == 0.0 {
        builder.append_ascii(b"0");
        return;
    }

    // 4. If x is +∞𝔽, return "Infinity".
    if value.is_infinite() {
        builder.append_ascii(if value > 0.0 { b"Infinity" } else { b"-Infinity" });
        return;
    }

    // 5. Let n, k, and s be integers such that k ≥ 1, radix ^ (k - 1) ≤ s < radix ^ k, 𝔽(s × radix ^ (n - k)) is x,
    //    and k is as small as possible.
    let DecimalExponentialForm {
        is_negative,
        significand,
        exponent,
    } = convert_to_decimal_exponential_form(value);
    let significand_digits = DecimalDigits::new(significand);
    let digits = significand_digits.as_bytes();
    let k = digits.len() as i32;
    let n = exponent + k;

    // 3. If x < -0𝔽, return the string-concatenation of "-" and Number::toString(-x, radix).
    if is_negative {
        builder.append_ascii(b"-");
    }

    // 6. If radix ≠ 10 or n is in the inclusive interval from -5 to 21, then
    if (-5..=21).contains(&n) {
        if n >= k {
            // a. If n ≥ k, return the k digits of s followed by n - k zeros.
            builder.append_ascii(digits);
            builder.append_repeated_ascii(b'0', (n - k) as usize);
        } else if n > 0 {
            // b. Else if n > 0, return the most significant n digits of s, ".", and the remaining k - n digits.
            builder.append_ascii(&digits[..n as usize]);
            builder.append_ascii(b".");
            builder.append_ascii(&digits[n as usize..]);
        } else {
            // c. Else, return "0.", -n zeros, and the k digits of s.
            builder.append_ascii(b"0.");
            builder.append_repeated_ascii(b'0', n.unsigned_abs() as usize);
            builder.append_ascii(digits);
        }
        return;
    }

    // 7. NOTE: In this case, the input will be represented using scientific E notation, such as 1.2e+3.
    // 9. If n < 0, let exponentSign be "-". 10. Else, let exponentSign be "+".
    let exponent_sign: &[u8] = if n < 0 { b"-" } else { b"+" };
    let exponent_digits = DecimalDigits::new(u64::from((n - 1).unsigned_abs()));

    // 11. If k is 1, return the single digit of s, "e", exponentSign, and the decimal representation of abs(n - 1).
    // 12. Return the most significant digit of s, ".", the remaining k - 1 digits, "e", exponentSign, and abs(n - 1).
    builder.append_ascii(&digits[..1]);
    if k > 1 {
        builder.append_ascii(b".");
        builder.append_ascii(&digits[1..]);
    }
    builder.append_ascii(b"e");
    builder.append_ascii(exponent_sign);
    builder.append_ascii(exponent_digits.as_bytes());
}

/// A finite, non-zero double as (-1)^is_negative × significand × 10^exponent, with the contract of
/// AK::convert_to_decimal_exponential_form (Dragonbox): the significand has as few digits as possible, and of the
/// significands of that length that round-trip, it is the one closest to the double, the even one on a tie.
struct DecimalExponentialForm {
    is_negative: bool,
    significand: u64,
    exponent: i32,
}

fn convert_to_decimal_exponential_form(value: f64) -> DecimalExponentialForm {
    let magnitude = value.abs();
    let mut shortest = AsciiBuffer::new();
    write!(shortest, "{magnitude:e}").expect("the shortest exponential form of a double fits the buffer");
    let (mantissa_text, exponent_text) = shortest
        .as_str()
        .split_once('e')
        .expect("exponential formatting writes an exponent");

    let mut significand = 0u64;
    let mut digit_count = 0i32;
    for digit in mantissa_text.bytes().filter(u8::is_ascii_digit) {
        significand = significand * 10 + u64::from(digit - b'0');
        digit_count += 1;
    }
    let exponent_of_first_digit: i32 = exponent_text
        .parse()
        .expect("exponential formatting writes a decimal exponent");
    let exponent = exponent_of_first_digit - (digit_count - 1);

    DecimalExponentialForm {
        is_negative: value.is_sign_negative(),
        significand: prefer_even_significand_on_exact_tie(magnitude, significand, exponent),
        exponent,
    }
}

/// Rust's shortest formatting finds the same significand as Dragonbox, except when the double lies exactly halfway
/// between two shortest candidates: Rust then rounds up, where Dragonbox picks the even candidate.
fn prefer_even_significand_on_exact_tie(magnitude: f64, significand: u64, exponent: i32) -> u64 {
    if significand.is_multiple_of(2) {
        return significand;
    }
    [significand - 1, significand + 1]
        .into_iter()
        .find(|&neighbour| {
            neighbour != 0
                && is_exactly_halfway_between(magnitude, significand + neighbour, exponent)
                && decimal_rounds_to(neighbour, exponent, magnitude)
        })
        .unwrap_or(significand)
}

/// Whether magnitude is exactly (sum_of_significands / 2) × 10^exponent, for an odd sum_of_significands.
fn is_exactly_halfway_between(magnitude: f64, sum_of_significands: u64, exponent: i32) -> bool {
    // 2 × magnitude is odd_binary_significand × 2^(1 + trailing_zeros + binary_exponent), and the other side is
    // sum_of_significands × 5^exponent × 2^exponent. Once a negative exponent's 5^-exponent is moved across, both are
    // an odd number times a power of two, so they are equal exactly when the powers of two and the odd numbers are.
    let bits = magnitude.to_bits();
    let biased_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (binary_significand, binary_exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased_exponent - 1075)
    };

    let trailing_zeros = binary_significand.trailing_zeros();
    let odd_binary_significand = u128::from(binary_significand >> trailing_zeros);
    if 1 + trailing_zeros as i32 + binary_exponent != exponent {
        return false;
    }

    let power_of_five = 5u128.checked_pow(exponent.unsigned_abs());
    let sum_of_significands = u128::from(sum_of_significands);
    if exponent >= 0 {
        power_of_five.and_then(|power| power.checked_mul(sum_of_significands)) == Some(odd_binary_significand)
    } else {
        power_of_five.and_then(|power| power.checked_mul(odd_binary_significand)) == Some(sum_of_significands)
    }
}

fn decimal_rounds_to(significand: u64, exponent: i32, magnitude: f64) -> bool {
    let mut decimal = AsciiBuffer::new();
    write!(decimal, "{significand}e{exponent}").expect("a decimal significand and exponent fit the buffer");
    decimal.as_str().parse::<f64>() == Ok(magnitude)
}

struct AsciiBuffer {
    bytes: [u8; 32],
    length: usize,
}

impl AsciiBuffer {
    fn new() -> Self {
        Self {
            bytes: [0; 32],
            length: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.length]).expect("only formatted ASCII is written")
    }
}

impl Write for AsciiBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length + text.len();
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

struct DecimalDigits {
    digits: [u8; 20],
    start: usize,
}

impl DecimalDigits {
    fn new(mut value: u64) -> Self {
        let mut digits = [0; 20];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self { digits, start }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.digits[self.start..]
    }
}

#[cfg(test)]
mod number_to_string_tests {
    use super::*;

    #[test]
    fn number_to_string_matches_the_cpp_runtime() {
        for &(bits, expected) in NUMBER_TO_STRING_CASES {
            let value = f64::from_bits(bits);
            assert_eq!(
                number_to_string(value),
                expected,
                "Number::toString({value:e}), bits {bits:016x}"
            );
            assert_eq!(
                number_to_utf16_string(value),
                expected.encode_utf16().collect::<Vec<_>>(),
                "Number::toString({value:e}) as UTF-16, bits {bits:016x}"
            );
        }
    }

    /// Doubles with their String() in the C++ runtime, as bit patterns so that each case is exactly one double. They
    /// include doubles exactly halfway between two shortest candidates, such as 1125899906842624.25.
    const NUMBER_TO_STRING_CASES: &[(u64, &str)] = &[
        (0x0000000000000000, "0"),
        (0x8000000000000000, "0"),
        (0x7ff8000000000000, "NaN"),
        (0x7ff0000000000000, "Infinity"),
        (0xfff0000000000000, "-Infinity"),
        (0x3ff0000000000000, "1"),
        (0xbff0000000000000, "-1"),
        (0x4000000000000000, "2"),
        (0x4024000000000000, "10"),
        (0x4059000000000000, "100"),
        (0x405ec00000000000, "123"),
        (0xc05ec00000000000, "-123"),
        (0x3fb999999999999a, "0.1"),
        (0x3fc999999999999a, "0.2"),
        (0x3fd3333333333333, "0.3"),
        (0x3fd3333333333334, "0.30000000000000004"),
        (0x3fd5555555555555, "0.3333333333333333"),
        (0x3fe5555555555555, "0.6666666666666666"),
        (0xbfd5555555555555, "-0.3333333333333333"),
        (0x3fe0000000000000, "0.5"),
        (0x3fd0000000000000, "0.25"),
        (0x3fc0000000000000, "0.125"),
        (0x3ff8000000000000, "1.5"),
        (0x444b1ae4d6e2ef50, "1e+21"),
        (0x4415af1d78b58c40, "100000000000000000000"),
        (0x3eb0c6f7a0b5ed8d, "0.000001"),
        (0x3e7ad7f29abcaf48, "1e-7"),
        (0x3e8421f5f40d8376, "1.5e-7"),
        (0x3c36b082c2148b8e, "1.23e-18"),
        (0x4340000000000000, "9007199254740992"),
        (0x4340000000000001, "9007199254740994"),
        (0x433fffffffffffff, "9007199254740991"),
        (0xc340000000000000, "-9007199254740992"),
        (0x43f0000000000000, "18446744073709552000"),
        (0x43e0000000000000, "9223372036854776000"),
        (0x41e0000000000000, "2147483648"),
        (0x41f0000000000000, "4294967296"),
        (0x41dfffffffc00000, "2147483647"),
        (0x41efffffffe00000, "4294967295"),
        (0x7fefffffffffffff, "1.7976931348623157e+308"),
        (0xffefffffffffffff, "-1.7976931348623157e+308"),
        (0x0000000000000001, "5e-324"),
        (0x8000000000000001, "-5e-324"),
        (0x0000000000000002, "1e-323"),
        (0x0000000000000003, "1.5e-323"),
        (0x0010000000000000, "2.2250738585072014e-308"),
        (0x000fffffffffffff, "2.225073858507201e-308"),
        (0x3cb0000000000000, "2.220446049250313e-16"),
        (0xc33fffffffffffff, "-9007199254740991"),
        (0x400921fb54442d18, "3.141592653589793"),
        (0x4005bf0a8b145769, "2.718281828459045"),
        (0x3ff6a09e667f3bcd, "1.4142135623730951"),
        (0x3fe62e42fefa39ef, "0.6931471805599453"),
        (0x441ac53a7e04bcda, "123456789012345680000"),
        (0x4484ea15b273b38a, "1.2345678901234568e+22"),
        (0x3ee9e0fcaf9380fc, "0.00001234"),
        (0x3eb4b3fd5942cd96, "0.000001234"),
        (0x3e808ffde1023e12, "1.234e-7"),
        (0x4310000000000001, "1125899906842624.2"),
        (0x4310000000000003, "1125899906842624.8"),
        (0x44b52d02c7e14af6, "1e+23"),
        (0x4480f0cf064dd592, "1e+22"),
        (0x3ea0c6f7a0b5ed8d, "5e-7"),
        (0x4011666666666666, "4.35"),
        (0x3fb1eb851eb851ec, "0.07"),
        (0x444b1ae4d6e2ef4f, "999999999999999900000"),
        (0x7e37e43c8800759c, "1e+300"),
        (0x01a56e1fc2f8f359, "1e-300"),
        (0x3e8091f1667f0595, "1.2345678901234566e-7"),
        (0x4450bb448ec2f608, "1.2345678901234568e+21"),
        (0x441ac53a7e04bcd9, "123456789012345670000"),
        (0x3eb4b66dc01ec6fb, "0.0000012345678901234567"),
        (0xbe7ad7f29abcaf48, "-1e-7"),
        (0xc44b1ae4d6e2ef50, "-1e+21"),
        (0x41f0000000080000, "4294967296.5"),
        (0x405edd2f1a9fbe77, "123.456"),
        (0xbeb92a737110e454, "-0.0000015"),
        (0x3eff75104d551d69, "0.00003"),
        (0x41b1de784a000000, "299792458"),
        (0x44dfe185ca57c517, "6.02214076e+23"),
        (0x390b860bde023111, "6.62607015e-34"),
        (0x0090000000000000, "5.696189077778436e-306"),
        (0x0450000000000000, "6.567258882077402e-288"),
        (0x0810000000000000, "7.571533991467358e-270"),
        (0x0bd0000000000000, "8.729384361624432e-252"),
        (0x0f90000000000000, "1.0064294952495521e-233"),
        (0x1350000000000000, "1.1603342079438231e-215"),
        (0x1710000000000000, "1.3377742608693866e-197"),
        (0x1ad0000000000000, "1.542348713665846e-179"),
        (0x1e90000000000000, "1.778206999588062e-161"),
        (0x2250000000000000, "2.0501330894674953e-143"),
        (0x2610000000000000, "2.3636425261531484e-125"),
        (0x29d0000000000000, "2.7250942976052165e-107"),
        (0x2d90000000000000, "3.141819817790545e-89"),
        (0x3150000000000000, "3.622271631530685e-71"),
        (0x3510000000000000, "4.176194859519056e-53"),
        (0x38d0000000000000, "4.81482486096809e-35"),
        (0x3c90000000000000, "5.551115123125783e-17"),
        (0x4050000000000000, "64"),
        (0x4410000000000000, "73786976294838210000"),
        (0x47d0000000000000, "8.507059173023462e+37"),
        (0x4b90000000000000, "9.807971461541689e+55"),
        (0x4f50000000000000, "1.130782121458166e+74"),
        (0x5310000000000000, "1.3037030248540711e+92"),
        (0x56d0000000000000, "1.5030672529752533e+110"),
        (0x5a90000000000000, "1.7329185588255093e+128"),
        (0x5e50000000000000, "1.997919072202235e+146"),
        (0x6210000000000000, "2.3034438628061165e+164"),
        (0x65d0000000000000, "2.6556899640838355e+182"),
        (0x6990000000000000, "3.061802069160839e+200"),
        (0x6d50000000000000, "3.530017448385272e+218"),
        (0x7110000000000000, "4.0698330278807704e+236"),
        (0x74d0000000000000, "4.692198018002938e+254"),
        (0x7890000000000000, "5.409735998829212e+272"),
        (0x7c50000000000000, "6.237000967296e+290"),
        (0x7fc0000000000000, "2.247116418577895e+307"),
        (0x7fd0000000000000, "4.49423283715579e+307"),
        (0x7fe0000000000000, "8.98846567431158e+307"),
        (0x0000000000000004, "2e-323"),
        (0x0636b0a8e891fffd, "9.999999999999996e-279"),
        (0x0636b0a8e891fffe, "9.999999999999998e-279"),
        (0x0636b0a8e891ffff, "1e-278"),
        (0x0636b0a8e8920000, "1.0000000000000001e-278"),
        (0x0636b0a8e8920001, "1.0000000000000003e-278"),
        (0x0f8fcbaa82a1611f, "9.999999999999997e-234"),
        (0x0f8fcbaa82a16120, "9.999999999999998e-234"),
        (0x0f8fcbaa82a16121, "1e-233"),
        (0x0f8fcbaa82a16122, "1.0000000000000001e-233"),
        (0x0f8fcbaa82a16123, "1.0000000000000002e-233"),
        (0x18e6470cff6546b4, "9.999999999999996e-189"),
        (0x18e6470cff6546b5, "9.999999999999998e-189"),
        (0x18e6470cff6546b6, "1e-188"),
        (0x18e6470cff6546b7, "1.0000000000000001e-188"),
        (0x18e6470cff6546b8, "1.0000000000000003e-188"),
        (0x223f37ad21436d0a, "9.999999999999997e-144"),
        (0x223f37ad21436d0b, "9.999999999999998e-144"),
        (0x223f37ad21436d0c, "1e-143"),
        (0x223f37ad21436d0d, "1.0000000000000001e-143"),
        (0x223f37ad21436d0e, "1.0000000000000002e-143"),
        (0x2b95df5ca28ef40b, "9.999999999999996e-99"),
        (0x2b95df5ca28ef40c, "9.999999999999998e-99"),
        (0x2b95df5ca28ef40d, "1e-98"),
        (0x2b95df5ca28ef40e, "1.0000000000000001e-98"),
        (0x2b95df5ca28ef40f, "1.0000000000000003e-98"),
        (0x34eea6608e29b24b, "9.999999999999998e-54"),
        (0x34eea6608e29b24c, "9.999999999999999e-54"),
        (0x34eea6608e29b24d, "1e-53"),
        (0x34eea6608e29b24e, "1.0000000000000001e-53"),
        (0x34eea6608e29b24f, "1.0000000000000003e-53"),
        (0x3e45798ee2308c38, "9.999999999999997e-9"),
        (0x3e45798ee2308c39, "9.999999999999999e-9"),
        (0x3e45798ee2308c3a, "1e-8"),
        (0x3e45798ee2308c3b, "1.0000000000000002e-8"),
        (0x3e45798ee2308c3c, "1.0000000000000004e-8"),
        (0x479e17b843576919, "9.999999999999997e+36"),
        (0x479e17b84357691a, "9.999999999999998e+36"),
        (0x479e17b84357691b, "1e+37"),
        (0x479e17b84357691c, "1.0000000000000001e+37"),
        (0x479e17b84357691d, "1.0000000000000002e+37"),
        (0x50f5159af8044460, "9.999999999999996e+81"),
        (0x50f5159af8044461, "9.999999999999998e+81"),
        (0x50f5159af8044462, "1e+82"),
        (0x50f5159af8044463, "1.0000000000000001e+82"),
        (0x50f5159af8044464, "1.0000000000000003e+82"),
        (0x5a4d8ba7f519c84d, "9.999999999999997e+126"),
        (0x5a4d8ba7f519c84e, "9.999999999999998e+126"),
        (0x5a4d8ba7f519c84f, "1e+127"),
        (0x5a4d8ba7f519c850, "1.0000000000000001e+127"),
        (0x5a4d8ba7f519c851, "1.0000000000000002e+127"),
        (0x63a4b378469b6730, "9.999999999999997e+171"),
        (0x63a4b378469b6731, "9.999999999999999e+171"),
        (0x63a4b378469b6732, "1e+172"),
        (0x63a4b378469b6733, "1.0000000000000003e+172"),
        (0x63a4b378469b6734, "1.0000000000000004e+172"),
        (0x6cfd022390f8b835, "9.999999999999997e+216"),
        (0x6cfd022390f8b836, "9.999999999999998e+216"),
        (0x6cfd022390f8b837, "1e+217"),
        (0x6cfd022390f8b838, "1.0000000000000001e+217"),
        (0x6cfd022390f8b839, "1.0000000000000002e+217"),
        (0x7654531e58a03e8a, "9.999999999999997e+261"),
        (0x7654531e58a03e8b, "9.999999999999998e+261"),
        (0x7654531e58a03e8c, "1e+262"),
        (0x7654531e58a03e8d, "1.0000000000000002e+262"),
        (0x7654531e58a03e8e, "1.0000000000000004e+262"),
        (0x7fac7b1f3cac7431, "9.999999999999997e+306"),
        (0x7fac7b1f3cac7432, "9.999999999999999e+306"),
        (0x7fac7b1f3cac7433, "1e+307"),
        (0x7fac7b1f3cac7434, "1.0000000000000001e+307"),
        (0x7fac7b1f3cac7435, "1.0000000000000002e+307"),
        (0x7fe1ccf385ebc89e, "9.999999999999996e+307"),
        (0x7fe1ccf385ebc89f, "9.999999999999998e+307"),
        (0x7fe1ccf385ebc8a0, "1e+308"),
        (0x7fe1ccf385ebc8a1, "1.0000000000000002e+308"),
        (0x7fe1ccf385ebc8a2, "1.0000000000000004e+308"),
        (0x3eb0c6f7a0b5ed8b, "9.999999999999995e-7"),
        (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
        (0x3eb0c6f7a0b5ed8e, "0.0000010000000000000002"),
        (0x3eb0c6f7a0b5ed8f, "0.0000010000000000000004"),
        (0x3e7ad7f29abcaf46, "9.999999999999997e-8"),
        (0x3e7ad7f29abcaf47, "9.999999999999998e-8"),
        (0x3e7ad7f29abcaf49, "1.0000000000000001e-7"),
        (0x3e7ad7f29abcaf4a, "1.0000000000000002e-7"),
        (0x4415af1d78b58c3e, "99999999999999970000"),
        (0x4415af1d78b58c3f, "99999999999999980000"),
        (0x4415af1d78b58c41, "100000000000000020000"),
        (0x4415af1d78b58c42, "100000000000000030000"),
        (0x444b1ae4d6e2ef4e, "999999999999999700000"),
        (0x444b1ae4d6e2ef51, "1.0000000000000001e+21"),
        (0x444b1ae4d6e2ef52, "1.0000000000000003e+21"),
        (0x433ffffffffffffe, "9007199254740990"),
        (0x4340000000000002, "9007199254740996"),
        (0x432ffffffffffffe, "4503599627370495"),
        (0x432fffffffffffff, "4503599627370495.5"),
        (0x4330000000000000, "4503599627370496"),
        (0x4330000000000001, "4503599627370497"),
        (0x4330000000000002, "4503599627370498"),
        (0x434ffffffffffffe, "18014398509481980"),
        (0x434fffffffffffff, "18014398509481982"),
        (0x4350000000000000, "18014398509481984"),
        (0x4350000000000001, "18014398509481988"),
        (0x4350000000000002, "18014398509481990"),
        (0xc054e00000000000, "-83.5"),
        (0x3fc53f7ced916873, "0.166"),
        (0x406f400000000000, "250"),
        (0xc074d80000000000, "-333.5"),
        (0x3fda9fbe76c8b439, "0.416"),
        (0x407f400000000000, "500"),
        (0xc0823c0000000000, "-583.5"),
        (0x3fe54fdf3b645a1d, "0.666"),
        (0x4087700000000000, "750"),
        (0xc08a0c0000000000, "-833.5"),
        (0x3fed4fdf3b645a1d, "0.916"),
        (0x408f400000000000, "1000"),
        (0xdc1b77ae0bf34dad, "-4.9911105725155504e+135"),
        (0x2eaa5d555d5d6c79, "6.785660674745272e-84"),
        (0x733c75ed334879c4, "1.24371618519318e+247"),
        (0xe4b9c30f2a68f125, "-1.6311586497843164e+177"),
        (0xb7ac6ca72a6a68b9, "-1.6314820258654558e-40"),
        (0xbfcece69f5e25e3f, "-0.2406742525679295"),
        (0x796d177505842666, "8.057755371405623e+276"),
        (0xeffd8abb474418be, "-2.8665228437957636e+231"),
        (0x5d7089a5f7743cc2, "1.2604127373919135e+142"),
        (0x5b91ef14aeb5d478, "1.2729550572097727e+133"),
        (0xa88f69d5836da456, "-2.551213381306969e-113"),
        (0x947c80c87620c643, "-5.4186924695997535e-210"),
        (0xa190320af3143e02, "-5.066358731024768e-147"),
        (0x9848b143dc6b8901, "-1.0824224925384549e-191"),
        (0x40811f8f77ac5ceb, "547.9450524774414"),
        (0x76af82579d6c3e63, "4.9609277569007504e+263"),
        (0xf7f6ff6897c504e4, "-7.593471064170383e+269"),
        (0xbe960fc3d9827afc, "-3.287431922324738e-7"),
        (0x0ca04237a2cc8afe, "7.26671897031033e-248"),
        (0x3f85702e9c7a9965, "0.010467876577717925"),
        (0xe7be910aad5b800a, "-5.4475842471175054e+191"),
        (0x4d26ab4617261b9d, "4.6627507177823825e+63"),
        (0xd02e1a059a83e642, "-1.742766355921457e+78"),
        (0xef7e2f42a7ae6eaf, "-1.1440955667566245e+229"),
        (0x3a5956b7e9b7eb3b, "1.279279639208626e-27"),
        (0xac80803d6769e5bd, "-2.472065483180713e-94"),
        (0x362d5bfab7f4182d, "1.0044222210166761e-47"),
        (0x39374765d78e5160, "4.483352622176303e-33"),
        (0x2745a557aeae63c0, "1.6765123592528922e-119"),
        (0x5863203573878ca0, "6.028764038861866e+117"),
        (0xd66c85180967af3a, "-2.0931310924971078e+108"),
        (0x75a6eb0478c3e040, "5.5058415467340815e+258"),
        (0x16e0c14e406343aa, "1.7511360846860286e-198"),
        (0xbd2290600317b3e7, "-3.2976226244021695e-14"),
        (0xabdfee1b7fd2d1f8, "-2.335725428778002e-97"),
        (0x18f6fb43869286de, "2.0631988061609328e-188"),
        (0x79e349ad56a97777, "1.3676303143924114e+279"),
        (0xbfe2c6e0a44aa206, "-0.5867770394152678"),
        (0x591c33321e216b64, "1.8204850979289672e+121"),
        (0x18d5035c9499813a, "4.716214055847976e-189"),
        (0x0002d2485b84b1dd, "3.923662683323697e-309"),
        (0x000f74c75b693341, "2.149444564230886e-308"),
        (0x001b14aeb951a326, "3.7660475180651344e-308"),
        (0x001f9fb92684912d, "4.397847206030633e-308"),
        (0x00001420046fdd48, "1.09325590933037e-310"),
        (0x0008dc4a49c2a4e4, "1.2322053712920567e-308"),
        (0x000a2dd43ba2ceff, "1.415566910511916e-308"),
        (0x000c8aef2aa17d39, "1.7442787715464685e-308"),
        (0x00073734bf1d863a, "1.0034594417785896e-308"),
        (0x00134334065b51db, "2.6787820752561945e-308"),
        (0x0000bed5e97da30c, "1.03667795791766e-309"),
        (0x000a7646934339fd, "1.454922170792351e-308"),
        (0x0010e5e258d38d44, "2.3499540470914154e-308"),
        (0x0014dd9a0d24fa75, "2.9017232533103855e-308"),
        (0x4327d342fca93e58, "3353104562298668"),
        (0x431af02e641d397a, "1895607858318942.5"),
        (0x42ff0b1630c9a395, "546119638358585.3"),
        (0x42e3f4d5eed4e7e2, "175538257176383.06"),
        (0x42cdcce8d634ef81, "65531833772511.01"),
        (0x42b9cbb234635bf5, "28362658833243.957"),
        (0x429eef44b0202547, "8503249602569.319"),
        (0x4285775bb0f4a24a, "2950297951892.286"),
        (0x4264a4644b1cef5e, "709259057383.4802"),
        (0x4257936fec2a0b76, "405031334056.1791"),
        (0x42392d9fab5c5ee9, "108139621212.37074"),
        (0x4313c4fc43ab4fa5, "1391153075901417.2"),
        (0x431ca34d57b8b41d, "2015212981857543.2"),
        (0x4312cc54196fbf95, "1322802789216229.2"),
        (0x4314995b0a070d71, "1449528955880284.2"),
        (0x4312b38cff1f0361, "1315991934451928.2"),
        (0x43126d6132315629, "1296703450535306.2"),
        (0x431feb76e1270281, "2246155023532192.2"),
        (0x4313eac06af21b21, "1401534176593608.2"),
        (0x4312a54b4cf5f341, "1312073103277264.2"),
        (0x431b89045d872655, "1937619053300117.2"),
        (0x431a08d6d9208935, "1832017063322189.2"),
        (0x4319ae29d100d021, "1807092260287496.2"),
        (0x4311d20bdf6fddc1, "1254005759801200.2"),
        (0x43144a4f0c0c21c5, "1427800724801649.2"),
        (0x43186f3bcdb1a6c1, "1719425521445296.2"),
        (0x43129f1dabae2aad, "1310374840994475.2"),
        (0x43177916278dc319, "1651765131047110.2"),
        (0x431374b8786870e1, "1369090050104376.2"),
        (0x431defaea4dfe925, "2106576923523657.2"),
        (0x4310a589f4e802f1, "1171402891329724.2"),
        (0x4314b199f4af1bf9, "1456193581860606.2"),
        (0x43117b77eadc24cd, "1230207393925427.2"),
        (0x43152f0ee17d2ab5, "1490678867511981.2"),
        (0x4315281c08250e01, "1488768842941312.2"),
        (0x43041614c6d9cdaa, "706722253191605.2"),
        (0x4308e1b33f2db342, "875444927051368.2"),
        (0x4309f69bf5b9b1b2, "913503015089718.2"),
        (0x430b8a58c8fd5762, "968992288123628.2"),
        (0x4309fd3ef84a2c12, "914415164147074.2"),
        (0x430266669d5d7f22, "647392561704932.2"),
        (0x43043903364e4ad2, "711523186624858.2"),
        (0x4304db01fcda7faa, "733787639730165.2"),
        (0x430d0c14b3a22dda, "1022007172154811.2"),
        (0x43076c1db8b4827a, "824099921629263.2"),
        (0x42e01f9a994254e4, "141823390126759.12"),
        (0x42e48b6d4d7c2ee4, "180712534434167.12"),
        (0x42e569377d404e24, "188333173637745.12"),
        (0x42e6b2158c91b474, "199632972189091.62"),
        (0x42e7d7d14bdaee34, "209725574535025.62"),
        (0x42e661dec7bf9584, "196876842171564.12"),
        (0x42d4e4f5933f6508, "91894420667796.12"),
        (0x42d41587a7108d28, "88330810966580.62"),
        (0x42be570d183e9010, "33359230680720.062"),
        (0x42b6830db9d99ad0, "24752126810522.812"),
        (0x428989506e795240, "3509693828906.2812"),
        (0x3e85b2c8a8a06bce, "1.6166548660951684e-7"),
        (0x4414c7d8fc54cf0f, "95833854680972380000"),
        (0x3e8cbdd5ffc77376, "2.1414120955536544e-7"),
        (0x441b501f6ffe61ed, "125958886593956360000"),
        (0x3e8bf84f3701100b, "2.083924317571433e-7"),
        (0x441a4c59d336bbc0, "121279251642072430000"),
        (0x3e85d4ebf2dda025, "1.6265902885314037e-7"),
        (0x441bd76fd1af5cdd, "128396486745458690000"),
        (0x3e8eea0988a79aa4, "2.3032879810300927e-7"),
        (0x4410df7bb56f709a, "77812892390545330000"),
        (0x3e854b2f3c840fa8, "1.5865034997547052e-7"),
        (0x44194868ee2b0497, "116596570969893880000"),
        (0x3e8a7d28249d5ac9, "1.9735763806079843e-7"),
        (0x4416e94bc6f4fcfd, "105659779602804850000"),
        (0x3e89b1eae27791b5, "1.914425962055453e-7"),
        (0x4412acc54faf846b, "86122709421741750000"),
        (0x3e8f4d7c6e08cc6f, "2.3322313948276081e-7"),
        (0x441fea45cfcc6cf4, "147182548385043120000"),
        (0x3e8414c1cf59fefc, "1.4961572218987618e-7"),
        (0x441f34d0b1dfce5a, "143913700886230500000"),
    ];
}
