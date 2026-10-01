/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::fmt;
use core::hash::{Hash, Hasher};
use core::num::NonZeroUsize;
use core::ptr::NonNull;

use ak::{Utf16FlyString, Utf16String};

use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::symbol::Symbol;
use crate::utf16::{Utf16View, to_utf16_fly_string};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringMayBeNumber {
    Yes,
    No,
}

/// The key of a property: one tagged word with the same bits as JS::PropertyKey. The low two bits say what it holds:
/// a fly string's raw word, which is either the address of its data or a short string, a symbol's address, or an
/// array index shifted left by two. Array-index strings are always stored as numbers, and fly strings are interned,
/// so two keys are equal exactly when their words are.
#[repr(transparent)]
pub struct PropertyKey(NonZeroUsize);

const _: () = assert!(size_of::<PropertyKey>() == size_of::<usize>());
const _: () = assert!(size_of::<Option<PropertyKey>>() == size_of::<usize>());
const _: () = assert!(size_of::<Utf16FlyString>() == size_of::<usize>());

impl PropertyKey {
    pub const NORMAL_STRING_FLAG: usize = 0;
    pub const SHORT_STRING_FLAG: usize = 1;
    pub const SYMBOL_FLAG: usize = 2;
    pub const NUMBER_FLAG: usize = 3;
    const FLAG_MASK: usize = 3;

    pub fn from_value(vm: &Vm, value: Value) -> ThrowCompletionOr<PropertyKey> {
        assert!(!value.is_empty());
        if value.is_symbol() {
            return Ok(PropertyKey::from_symbol(value.as_symbol()));
        }
        if value.is_integral_number() && value.as_f64() >= 0.0 && value.as_f64() < f64::from(u32::MAX) {
            return Ok(PropertyKey::from_number(value.as_f64() as u64));
        }
        Ok(PropertyKey::from_utf16_string(&value.to_utf16_string(vm)?))
    }

    pub fn is_string(&self) -> bool {
        let flag = self.flag();
        flag == Self::NORMAL_STRING_FLAG || flag == Self::SHORT_STRING_FLAG
    }

    pub fn is_number(&self) -> bool {
        self.flag() == Self::NUMBER_FLAG
    }

    pub fn is_symbol(&self) -> bool {
        self.flag() == Self::SYMBOL_FLAG
    }

    pub fn is_private(&self) -> bool {
        self.is_symbol() && self.as_symbol().is_private()
    }

    /// Mirrors the PropertyKey constructor from an integer: indices below u32::MAX are numbers, and larger ones are
    /// strings, since they cannot be array indices.
    pub fn from_number(index: u64) -> Self {
        if index >= u64::from(u32::MAX) {
            return Self::from_fly_string_without_number_check(Utf16FlyString::from_utf8(&index.to_string()));
        }
        Self::from_array_index(index as u32)
    }

    pub fn from_fly_string(string: Utf16FlyString, string_may_be_number: StringMayBeNumber) -> Self {
        if string_may_be_number == StringMayBeNumber::Yes
            && let Some(property_index) = array_index_of_canonical_string(Utf16View::of_fly_string(&string))
        {
            return Self::from_array_index(property_index);
        }

        Self::from_fly_string_without_number_check(string)
    }

    pub fn from_utf16_string(string: &Utf16String) -> Self {
        Self::from(to_utf16_fly_string(string))
    }

    pub fn from_utf8(string: &str) -> Self {
        Self::from(Utf16FlyString::from_utf8(string))
    }

    pub fn from_symbol(symbol: Gc<Symbol>) -> Self {
        let address = symbol.as_ptr().expose_provenance();
        debug_assert!(address & Self::FLAG_MASK == 0);
        Self(NonZeroUsize::new(address | Self::SYMBOL_FLAG).expect("a tagged symbol address is not zero"))
    }

    fn from_array_index(index: u32) -> Self {
        Self(NonZeroUsize::new((index as usize) << 2 | Self::NUMBER_FLAG).expect("a tagged number is not zero"))
    }

    fn from_fly_string_without_number_check(string: Utf16FlyString) -> Self {
        let raw = string.into_raw();
        debug_assert!(
            raw & Self::FLAG_MASK == Self::NORMAL_STRING_FLAG || raw & Self::FLAG_MASK == Self::SHORT_STRING_FLAG
        );
        Self(NonZeroUsize::new(raw).expect("a raw AK string is never zero"))
    }

    fn flag(&self) -> usize {
        self.0.get() & Self::FLAG_MASK
    }

    pub fn as_number(&self) -> u32 {
        assert!(self.is_number());
        (self.0.get() >> 2) as u32
    }

    pub fn as_string(&self) -> &Utf16FlyString {
        assert!(self.is_string());
        // SAFETY: A string key's word is the raw word of the fly string it owns, and Utf16FlyString is a transparent
        // wrapper around that word.
        unsafe { &*core::ptr::from_ref(&self.0).cast::<Utf16FlyString>() }
    }

    pub fn as_symbol(&self) -> Gc<Symbol> {
        assert!(self.is_symbol());
        let address = self.0.get() & !Self::FLAG_MASK;
        // SAFETY: A symbol key holds the address of its symbol, which it keeps alive.
        unsafe {
            Gc::from_non_null(NonNull::new_unchecked(
                core::ptr::with_exposed_provenance_mut::<Symbol>(address),
            ))
        }
    }

    pub fn to_value(&self, vm: &Vm) -> Value {
        assert!(!self.is_private());
        if self.is_string() {
            return Value::from_string(PrimitiveString::create_from_fly_string(vm, self.as_string()));
        }
        if self.is_symbol() {
            return Value::from_symbol(self.as_symbol());
        }
        Value::from_string(PrimitiveString::create_from_unsigned_integer(
            vm,
            u64::from(self.as_number()),
        ))
    }

    pub fn to_utf16_string(&self) -> Utf16String {
        if self.is_string() {
            return Utf16String::from(self.as_string());
        }
        if self.is_symbol() {
            return self.as_symbol().descriptive_string();
        }
        Utf16String::from_utf8(&self.as_number().to_string())
    }
}

/// The index an array-index string stands for, if it is one: the canonical decimal form of an integer below
/// u32::MAX, the same strings that the C++ PropertyKey stores as numbers.
fn array_index_of_canonical_string(string: Utf16View<'_>) -> Option<u32> {
    if string.is_empty() {
        return None;
    }
    let first_code_unit = string.code_unit_at(0);
    let is_ascii_digit = |code_unit: u16| (u16::from(b'0')..=u16::from(b'9')).contains(&code_unit);
    if !is_ascii_digit(first_code_unit) || (first_code_unit == u16::from(b'0') && string.length_in_code_units() > 1) {
        return None;
    }
    let mut property_index: u32 = 0;
    for code_unit in string.code_units() {
        if !is_ascii_digit(code_unit) {
            return None;
        }
        property_index = property_index
            .checked_mul(10)?
            .checked_add(u32::from(code_unit - u16::from(b'0')))?;
    }
    (property_index < u32::MAX).then_some(property_index)
}

impl From<u32> for PropertyKey {
    fn from(index: u32) -> Self {
        Self::from_number(u64::from(index))
    }
}

impl From<Utf16FlyString> for PropertyKey {
    fn from(string: Utf16FlyString) -> Self {
        Self::from_fly_string(string, StringMayBeNumber::Yes)
    }
}

impl From<&Utf16String> for PropertyKey {
    fn from(string: &Utf16String) -> Self {
        Self::from_utf16_string(string)
    }
}

impl From<Gc<Symbol>> for PropertyKey {
    fn from(symbol: Gc<Symbol>) -> Self {
        Self::from_symbol(symbol)
    }
}

impl Clone for PropertyKey {
    fn clone(&self) -> Self {
        if self.is_string() {
            // SAFETY: The key owns a reference to its fly string, so the string is live; the clone owns the new one.
            unsafe { ak::reference_utf16_string(self.0.get()) };
        }
        Self(self.0)
    }
}

impl Drop for PropertyKey {
    fn drop(&mut self) {
        if self.is_string() {
            // SAFETY: The key owns one reference to its fly string, which this releases.
            drop(unsafe { Utf16FlyString::from_raw_owned(self.0.get()) });
        }
    }
}

impl PartialEq for PropertyKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for PropertyKey {}

impl Hash for PropertyKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

// SAFETY: A key reaches a cell only when it holds a symbol, which this visits.
unsafe impl Trace for PropertyKey {
    fn trace(&self, visitor: &mut Visitor) {
        if self.is_symbol() {
            visitor.visit(self.as_symbol());
        }
    }
}

impl fmt::Display for PropertyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_number() {
            return write!(formatter, "{}", self.as_number());
        }
        let string = self.to_utf16_string();
        write!(formatter, "{}", Utf16View::of_string(&string).to_utf8())
    }
}

impl fmt::Debug for PropertyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_string() {
            return write!(
                formatter,
                "PropertyKey({:?})",
                Utf16View::of_fly_string(self.as_string()).to_utf8()
            );
        }
        write!(formatter, "PropertyKey({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_index_strings_are_canonical_decimal_integers_below_u32_max() {
        let index_of = |string: &str| {
            let code_units: Vec<u16> = string.encode_utf16().collect();
            array_index_of_canonical_string(Utf16View::Utf16(&code_units))
        };
        assert_eq!(index_of("0"), Some(0));
        assert_eq!(index_of("7"), Some(7));
        assert_eq!(index_of("4294967294"), Some(4_294_967_294));
        assert_eq!(index_of("4294967295"), None);
        assert_eq!(index_of("4294967296"), None);
        assert_eq!(index_of("99999999999"), None);
        assert_eq!(index_of("01"), None);
        assert_eq!(index_of("00"), None);
        assert_eq!(index_of(""), None);
        assert_eq!(index_of("-1"), None);
        assert_eq!(index_of("+1"), None);
        assert_eq!(index_of("1 "), None);
        assert_eq!(index_of(" 1"), None);
        assert_eq!(index_of("1.0"), None);
        assert_eq!(index_of("1e3"), None);
        assert_eq!(index_of("\u{0661}"), None);
    }

    // The strings the C++ js binary orders first in Object.keys, as array indices, and some it does not.
    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn keys_from_strings_canonicalize_array_indices_like_cpp() {
        let index_strings = ["0", "7", "4294967294"];
        let other_strings = [
            "b",
            "4294967295",
            "01",
            "-1",
            "00",
            "+1",
            "1 ",
            "99999999999",
            "1.0",
            "1e3",
            "\u{0661}",
            "4294967296",
        ];
        for string in index_strings {
            let key = PropertyKey::from_utf8(string);
            assert!(key.is_number(), "{string}");
            assert_eq!(key.to_string(), string);
        }
        for string in other_strings {
            let key = PropertyKey::from_utf8(string);
            assert!(key.is_string(), "{string}");
            assert_eq!(key.to_string(), string);
        }
        assert!(PropertyKey::from_fly_string(Utf16FlyString::from_utf8("17"), StringMayBeNumber::No).is_string());
        assert_ne!(
            PropertyKey::from_fly_string(Utf16FlyString::from_utf8("17"), StringMayBeNumber::No),
            PropertyKey::from(17u32)
        );
        assert!(PropertyKey::from_number(u64::from(u32::MAX)).is_string());
        assert_eq!(
            PropertyKey::from_number(u64::from(u32::MAX)),
            PropertyKey::from_utf8("4294967295")
        );
    }

    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn keys_use_the_cpp_tagged_word() {
        assert_eq!(PropertyKey::from(0u32).0.get(), PropertyKey::NUMBER_FLAG);
        assert_eq!(PropertyKey::from(5u32).0.get(), 5 << 2 | PropertyKey::NUMBER_FLAG);
        assert_eq!(PropertyKey::from(u32::MAX - 1).as_number(), u32::MAX - 1);
        let short = PropertyKey::from_utf8("abc");
        assert_eq!(short.flag(), PropertyKey::SHORT_STRING_FLAG);
        assert_eq!(
            short.0.get(),
            ak::utf16_short_string_raw("abc").expect("a short string")
        );
        assert_eq!(PropertyKey::from_utf8("").0.get(), ak::SHORT_STRING_FLAG);
        let long = PropertyKey::from_utf8("a longer property name");
        assert_eq!(long.flag(), PropertyKey::NORMAL_STRING_FLAG);
        assert_eq!(long.0.get(), long.as_string().raw_identity());
    }

    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn equal_strings_make_equal_keys_with_equal_hashes() {
        use std::collections::hash_map::DefaultHasher;
        let hash_of = |key: &PropertyKey| {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            hasher.finish()
        };
        let name = "a property name that is interned";
        let from_utf8 = PropertyKey::from_utf8(name);
        let code_units: Vec<u16> = name.encode_utf16().collect();
        let from_utf16 = PropertyKey::from_utf16_string(&Utf16String::from_utf16(&code_units));
        assert_eq!(from_utf8, from_utf16);
        assert_eq!(hash_of(&from_utf8), hash_of(&from_utf16));
        assert_ne!(from_utf8, PropertyKey::from_utf8("another property name entirely"));
        let unpaired_surrogate = PropertyKey::from_utf16_string(&Utf16String::from_utf16(&[0x61, 0xd800]));
        assert!(unpaired_surrogate.is_string());
        assert_eq!(
            Utf16View::of_fly_string(unpaired_surrogate.as_string()),
            Utf16View::Utf16(&[0x61, 0xd800])
        );
    }

    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn clones_share_the_fly_string_and_release_it_when_dropped() {
        let reference_count = |key: &PropertyKey| {
            // SAFETY: A long string key's word is the address of its live data.
            let header = unsafe { &*core::ptr::with_exposed_provenance::<ak::Utf16StringDataHeader>(key.0.get()) };
            header.reference_count.load(core::sync::atomic::Ordering::Relaxed)
        };
        let key = PropertyKey::from_utf8("a property name for counting references");
        let count = reference_count(&key);
        let clone = key.clone();
        assert_eq!(clone, key);
        assert_eq!(reference_count(&key), count + 1);
        drop(clone);
        assert_eq!(reference_count(&key), count);
    }

    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn symbol_keys_are_tagged_addresses_and_convert_to_values() {
        let vm = Vm::create();
        let symbol = Symbol::create(
            &vm,
            Some(Utf16String::from_utf8("description")),
            crate::runtime::symbol::Kind::Unique,
        );
        let key = PropertyKey::from(symbol);
        assert!(key.is_symbol() && !key.is_private());
        assert_eq!(key.0.get(), symbol.as_ptr().addr() | PropertyKey::SYMBOL_FLAG);
        assert!(key.as_symbol() == symbol);
        assert_eq!(key.to_value(&vm), Value::from_symbol(symbol));
        assert_eq!(key.to_string(), "Symbol(description)");
        assert!(PropertyKey::from(Symbol::create_private(&vm)).is_private());
        assert_eq!(PropertyKey::from_value(&vm, Value::from_symbol(symbol)).ok(), Some(key));
    }

    #[cfg(libjs_runtime_tests_with_libgc)]
    #[test]
    fn keys_from_values_use_numbers_for_integral_indices() {
        let vm = Vm::create();
        let key_of = |value: Value| PropertyKey::from_value(&vm, value).expect("the conversion succeeds");
        assert_eq!(key_of(Value::from_i32(3)), PropertyKey::from(3u32));
        assert_eq!(key_of(Value::from_f64(-0.0)), PropertyKey::from(0u32));
        assert_eq!(key_of(Value::from_f64(4294967294.0)), PropertyKey::from(u32::MAX - 1));
        let string = PrimitiveString::create_from_utf8(&vm, "42");
        assert_eq!(key_of(Value::from_string(string)), PropertyKey::from(42u32));
        let string = PrimitiveString::create_from_utf8(&vm, "length");
        assert_eq!(key_of(Value::from_string(string)), vm.names.length);

        let number_key = PropertyKey::from(1234u32).to_value(&vm);
        assert_eq!(number_key.as_string().to_utf8(), "1234");
        let string_key = PropertyKey::from_utf8("a long enough string key").to_value(&vm);
        assert_eq!(string_key.as_string().to_utf8(), "a long enough string key");
    }
}
