/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::{Cell, UnsafeCell};

use ak::{Utf16FlyString, Utf16String, Utf16StringUnits};
use libjs_runtime_macros::Trace;

use crate::gc::class::{Class, GcCell, define_cell};
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::{StringToAtomCacheEntry, Vm};
use crate::layout::cell::{CellHeader, Gc};
pub use crate::layout::primitive_string::{DeferredKind, PrimitiveString};
use crate::layout::value::Value;
use crate::layout_forward::Utf16StringSlot;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::value::DecimalDigits;
use crate::utf16::{
    MAX_SHORT_STRING_BYTE_COUNT, Utf16Display, Utf16StringBuilder, Utf16View, concatenate, has_fly_string_storage,
    has_short_ascii_storage, to_utf16_fly_string,
};

define_cell!(PrimitiveString, PrimitiveString);
define_cell!(RopeString, PrimitiveString, extends: [PrimitiveString]);
define_cell!(Substring, PrimitiveString, extends: [PrimitiveString]);

// SAFETY: A string's own data holds no cells. The deferred kinds trace the strings they are made of.
unsafe impl Trace for PrimitiveString {
    fn trace(&self, _: &mut Visitor) {}
}

#[repr(C)]
#[derive(Trace)]
pub struct RopeString {
    base: PrimitiveString,
    lhs: Cell<Option<Gc<PrimitiveString>>>,
    rhs: Cell<Option<Gc<PrimitiveString>>>,
}

#[repr(C)]
#[derive(Trace)]
pub struct Substring {
    base: PrimitiveString,
    source_string: Cell<Option<Gc<PrimitiveString>>>,
    code_unit_offset: usize,
}

/// Mirrors AK::u64_hash, the MurmurHash3 64-bit finalizer.
pub(crate) fn u64_hash(mut key: u64) -> u32 {
    key ^= key >> 33;
    key = key.wrapping_mul(0xff51_afd7_ed55_8ccd);
    key ^= key >> 33;
    key = key.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    key ^= key >> 33;
    key as u32
}

fn is_ascii(code_unit: u16) -> bool {
    code_unit < 0x80
}

impl PrimitiveString {
    fn fly_string_cache_hash(string: &Utf16FlyString) -> usize {
        u64_hash(string.raw_identity() as u64) as usize
    }

    fn short_flat_string_storage_view(&self) -> Option<&[u8]> {
        if self.deferred_kind.get() != DeferredKind::None {
            return None;
        }

        let string = self.resolved_utf16_string()?;
        if !has_short_ascii_storage(string) {
            return None;
        }
        match string.as_units() {
            Utf16StringUnits::Ascii(bytes) => Some(bytes),
            Utf16StringUnits::Utf16(_) => None,
        }
    }

    fn try_create_short_flat_concatenated_string(
        vm: &Vm,
        lhs: &PrimitiveString,
        rhs: &PrimitiveString,
    ) -> Option<Gc<PrimitiveString>> {
        let lhs_view = lhs.short_flat_string_storage_view()?;
        let rhs_view = rhs.short_flat_string_storage_view()?;

        let byte_count = lhs_view.len() + rhs_view.len();
        if byte_count > MAX_SHORT_STRING_BYTE_COUNT {
            return None;
        }

        let string = concatenate(&[Utf16View::Ascii(lhs_view), Utf16View::Ascii(rhs_view)]);
        Some(Self::create(vm, string))
    }

    pub fn create(vm: &Vm, string: Utf16String) -> Gc<PrimitiveString> {
        let view = Utf16View::of_string(&string);
        if view.is_empty() {
            return vm.empty_string();
        }

        if view.length_in_code_units() == 1 {
            let code_unit = view.code_unit_at(0);
            if is_ascii(code_unit) {
                return vm.single_ascii_character_string(code_unit as u8);
            }
        }

        if has_short_ascii_storage(&string) {
            return Self::create_from_fly_string(vm, &to_utf16_fly_string(&string));
        }

        vm.heap().allocate(Self::new(string))
    }

    pub fn create_from_utf16_view(vm: &Vm, string: Utf16View<'_>) -> Gc<PrimitiveString> {
        Self::create(vm, string.to_utf16_string())
    }

    pub fn create_from_utf8(vm: &Vm, string: &str) -> Gc<PrimitiveString> {
        Self::create(vm, Utf16String::from_utf8(string))
    }

    pub fn create_from_fly_string(vm: &Vm, string: &Utf16FlyString) -> Gc<PrimitiveString> {
        let view = Utf16View::of_fly_string(string);
        if view.is_empty() {
            return vm.empty_string();
        }

        if view.length_in_code_units() == 1 {
            let code_unit = view.code_unit_at(0);
            if is_ascii(code_unit) {
                return vm.single_ascii_character_string(code_unit as u8);
            }
        }

        let string_cache = vm.fly_string_cache();
        let cache_slot = &string_cache[Self::fly_string_cache_hash(string) & (string_cache.len() - 1)];
        if let Some(cached_string) = cache_slot.get()
            && cached_string
                .resolved_utf16_string()
                .is_some_and(|cached| cached.raw_identity() == string.raw_identity())
        {
            return cached_string;
        }

        let new_string = vm.heap().allocate(Self::new(Utf16String::from(string)));
        cache_slot.set(Some(new_string));
        new_string
    }

    pub fn create_from_unsigned_integer(vm: &Vm, number: u64) -> Gc<PrimitiveString> {
        let numeric_string_cache = vm.numeric_string_cache();
        if number < numeric_string_cache.len() as u64 {
            let cache_slot = &numeric_string_cache[number as usize];
            if cache_slot.get().is_none() {
                let string = Utf16FlyString::from_utf8(DecimalDigits::new(number).as_str());
                cache_slot.set(Some(Self::create_from_fly_string(vm, &string)));
            }
            return cache_slot.get().expect("the numeric string was just cached");
        }

        let large_cache = vm.large_numeric_string_cache();
        let cache_entry = &large_cache[(number & (large_cache.len() as u64 - 1)) as usize];
        if cache_entry.string.get().is_none() || cache_entry.number.get() != number {
            cache_entry.number.set(number);
            let string = Utf16FlyString::from_utf8(DecimalDigits::new(number).as_str());
            cache_entry.string.set(Some(Self::create_from_fly_string(vm, &string)));
        }
        cache_entry.string.get().expect("the numeric string was just cached")
    }

    pub fn create_from_concatenation(
        vm: &Vm,
        lhs: Gc<PrimitiveString>,
        rhs: Gc<PrimitiveString>,
    ) -> ThrowCompletionOr<Gc<PrimitiveString>> {
        if rhs.length_in_utf16_code_units() >= u32::MAX as usize - lhs.length_in_utf16_code_units() {
            return vm.throw_completion(ErrorKind::RangeError, ErrorType::InvalidLength, &[&"string"]);
        }

        // We're here to concatenate two strings into a new rope string. However, if any of them are empty, no rope is required.
        let lhs_empty = lhs.is_empty();
        let rhs_empty = rhs.is_empty();

        if lhs_empty && rhs_empty {
            return Ok(vm.empty_string());
        }

        if lhs_empty {
            return Ok(rhs);
        }

        if rhs_empty {
            return Ok(lhs);
        }

        if let Some(short_flat_string) = Self::try_create_short_flat_concatenated_string(vm, &lhs, &rhs) {
            return Ok(short_flat_string);
        }

        Ok(vm.heap().allocate(RopeString::new(lhs, rhs)).upcast())
    }

    pub fn create_from_substring(
        vm: &Vm,
        string: Gc<PrimitiveString>,
        code_unit_offset: usize,
        code_unit_length: usize,
    ) -> Gc<PrimitiveString> {
        let string_length = string.length_in_utf16_code_units();
        assert!(code_unit_offset <= string_length);
        assert!(code_unit_length <= string_length - code_unit_offset);

        if code_unit_length == 0 {
            return vm.empty_string();
        }

        if code_unit_offset == 0 && code_unit_length == string_length {
            return string;
        }

        if code_unit_length == 1 {
            let code_unit = string.utf16_string_view().code_unit_at(code_unit_offset);
            if is_ascii(code_unit) {
                return vm.single_ascii_character_string(code_unit as u8);
            }
        }

        if string.deferred_kind.get() == DeferredKind::Substring {
            let substring = string.as_substring();
            return Self::create_from_substring(
                vm,
                substring.source_string(),
                substring.code_unit_offset + code_unit_offset,
                code_unit_length,
            );
        }

        vm.heap()
            .allocate(Substring::new(string, code_unit_offset, code_unit_length))
            .upcast()
    }

    fn new_deferred(class: &'static Class, deferred_kind: DeferredKind, length_in_utf16_code_units: usize) -> Self {
        assert!(length_in_utf16_code_units < u32::MAX as usize);
        Self {
            header: CellHeader::for_class(class),
            deferred_kind: Cell::new(deferred_kind),
            length_in_utf16_code_units: Cell::new(length_in_utf16_code_units as u32),
            utf16_string: Utf16StringSlot(UnsafeCell::new(None)),
        }
    }

    pub(crate) fn new(string: Utf16String) -> Self {
        let length_in_utf16_code_units = Utf16View::of_string(&string).length_in_code_units() as u32;
        Self {
            header: CellHeader::for_class(Self::CLASS),
            deferred_kind: Cell::new(DeferredKind::None),
            length_in_utf16_code_units: Cell::new(length_in_utf16_code_units),
            utf16_string: Utf16StringSlot(UnsafeCell::new(Some(string))),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.length_in_utf16_code_units.get() == 0
    }

    pub fn utf16_string(&self) -> Utf16String {
        self.resolve_if_needed();

        self.resolved_utf16_string()
            .expect("a resolved string has its UTF-16 string")
            .clone()
    }

    pub fn property_key(&self, vm: &Vm) -> PropertyKey {
        self.resolve_if_needed();

        let string = self
            .resolved_utf16_string()
            .expect("a resolved string has its UTF-16 string");
        if has_fly_string_storage(string) {
            return PropertyKey::from(to_utf16_fly_string(string));
        }

        let this = core::ptr::from_ref(self);
        let mut string_to_atom_cache = vm.string_to_atom_cache().borrow_mut();
        for i in 0..string_to_atom_cache.len() {
            if !string_to_atom_cache[i]
                .string
                .is_some_and(|cached| core::ptr::eq(cached.as_ptr(), this))
            {
                continue;
            }
            if i != 0 {
                string_to_atom_cache.swap(0, i);
            }
            return PropertyKey::from(
                string_to_atom_cache[0]
                    .atom
                    .clone()
                    .expect("a cached string has its atom"),
            );
        }

        let fly_string = to_utf16_fly_string(string);
        if !has_fly_string_storage(string) {
            string_to_atom_cache[1] = core::mem::take(&mut string_to_atom_cache[0]);
            string_to_atom_cache[0] = StringToAtomCacheEntry {
                // SAFETY: Strings only exist as cells, since every way to create one allocates it.
                string: Some(unsafe { Gc::from_ref(self) }),
                atom: Some(fly_string.clone()),
            };
        }
        PropertyKey::from(fly_string)
    }

    pub fn has_utf16_string(&self) -> bool {
        self.resolved_utf16_string().is_some()
    }

    pub fn length_in_utf16_code_units(&self) -> usize {
        self.length_in_utf16_code_units.get() as usize
    }

    pub fn code_unit_at(&self, index: usize) -> u16 {
        self.utf16_string_view().code_unit_at(index)
    }

    /// Converts to UTF-8, replacing unpaired surrogates with U+FFFD.
    pub fn to_utf8(&self) -> String {
        self.utf16_string_view().to_utf8()
    }

    pub fn get(&self, vm: &Vm, property_key: &PropertyKey) -> ThrowCompletionOr<Option<Value>> {
        if property_key.is_symbol() {
            return Ok(None);
        }

        if property_key.is_string() && property_key.as_string() == vm.names.length.as_string() {
            return Ok(Some(Value::from_f64(self.length_in_utf16_code_units() as f64)));
        }

        // CanonicalNumericIndexString in CanonicalIndexMode::IgnoreNumericRoundtrip only treats number keys as indices.
        if !property_key.is_number() {
            return Ok(None);
        }
        let index = property_key.as_number() as usize;

        let string = self.utf16_string_view();
        if string.length_in_code_units() <= index {
            return Ok(None);
        }

        // SAFETY: Strings only exist as cells, since every way to create one allocates it.
        let this = unsafe { Gc::from_ref(self) };
        Ok(Some(Value::from_string(Self::create_from_substring(
            vm, this, index, 1,
        ))))
    }

    /// A view of the string's code units. A substring that is not resolved yet is viewed in the string it was taken
    /// from, so the view must not be held across anything that can collect garbage.
    pub fn utf16_string_view(&self) -> Utf16View<'_> {
        if !self.has_utf16_string() {
            if self.deferred_kind.get() == DeferredKind::Substring {
                let substring = self.as_substring();
                let source_string = substring.source_string();
                // SAFETY: This substring keeps its source alive until it resolves, and resolving cannot collect
                // garbage, so the source outlives every view a caller may hold without collecting.
                let source_string = unsafe { source_string.as_non_null().as_ref() };
                return source_string
                    .utf16_string_view()
                    .substring_view(substring.code_unit_offset, self.length_in_utf16_code_units());
            }
            self.resolve_if_needed();
        }
        Utf16View::of_string(
            self.resolved_utf16_string()
                .expect("a resolved string has its UTF-16 string"),
        )
    }

    /// A view of the string's own code units, resolving a rope or a substring first. Unlike utf16_string_view(), the
    /// view stays valid for as long as the string lives, since a resolved string never changes.
    pub fn resolved_utf16_string_view(&self) -> Utf16View<'_> {
        self.resolve_if_needed();
        Utf16View::of_string(
            self.resolved_utf16_string()
                .expect("a resolved string has its UTF-16 string"),
        )
    }

    fn resolved_utf16_string(&self) -> Option<&Utf16String> {
        // SAFETY: The slot is only written while it is empty, so a string read out of it is never replaced.
        unsafe { (*self.utf16_string.0.get()).as_ref() }
    }

    fn set_resolved_utf16_string(&self, string: Utf16String) {
        assert!(!self.has_utf16_string());
        // SAFETY: The slot is empty, so nothing borrows the string it holds.
        unsafe { *self.utf16_string.0.get() = Some(string) };
    }

    fn resolve_if_needed(&self) {
        match self.deferred_kind.get() {
            DeferredKind::None => {}
            DeferredKind::Rope => self.as_rope_string().resolve(),
            DeferredKind::Substring => self.as_substring().resolve(),
        }
    }

    fn as_rope_string(&self) -> &RopeString {
        debug_assert!(self.header_class().is_subclass_of(RopeString::CLASS));
        // SAFETY: Only RopeString creates strings that are deferred as ropes, and it starts with its PrimitiveString.
        unsafe { &*core::ptr::from_ref(self).cast::<RopeString>() }
    }

    fn as_substring(&self) -> &Substring {
        debug_assert!(self.header_class().is_subclass_of(Substring::CLASS));
        // SAFETY: Only Substring creates strings that are deferred as substrings, and it starts with its
        // PrimitiveString.
        unsafe { &*core::ptr::from_ref(self).cast::<Substring>() }
    }

    fn header_class(&self) -> &'static Class {
        self.header.class
    }
}

impl PartialEq for PrimitiveString {
    fn eq(&self, other: &Self) -> bool {
        if core::ptr::eq(self, other) {
            return true;
        }
        if self.length_in_utf16_code_units() != other.length_in_utf16_code_units() {
            return false;
        }
        if let (Some(string), Some(other_string)) = (self.resolved_utf16_string(), other.resolved_utf16_string()) {
            return string == other_string;
        }
        self.utf16_string_view() == other.utf16_string_view()
    }
}

impl Eq for PrimitiveString {}

impl Utf16Display for Gc<PrimitiveString> {
    fn fmt_utf16(&self, builder: &mut Utf16StringBuilder) {
        builder.append(self.utf16_string_view());
    }
}

impl RopeString {
    fn new(lhs: Gc<PrimitiveString>, rhs: Gc<PrimitiveString>) -> Self {
        Self {
            base: PrimitiveString::new_deferred(
                Self::CLASS,
                DeferredKind::Rope,
                lhs.length_in_utf16_code_units() + rhs.length_in_utf16_code_units(),
            ),
            lhs: Cell::new(Some(lhs)),
            rhs: Cell::new(Some(rhs)),
        }
    }

    fn lhs(&self) -> Gc<PrimitiveString> {
        self.lhs.get().expect("an unresolved rope has both sides")
    }

    fn rhs(&self) -> Gc<PrimitiveString> {
        self.rhs.get().expect("an unresolved rope has both sides")
    }

    fn resolve(&self) {
        // This vector will hold all the pieces of the rope that need to be assembled
        // into the resolved string.
        // NB: Resolving takes no VM, so it cannot allocate cells or collect garbage while these vectors hold strings
        //     that only the rope keeps alive.
        let mut pieces: Vec<Gc<PrimitiveString>> = Vec::with_capacity(2);

        // NOTE: We traverse the rope tree without using recursion, since we'd run out of
        //       stack space quickly when handling a long sequence of unresolved concatenations.
        let mut stack: Vec<Gc<PrimitiveString>> = Vec::with_capacity(2);
        stack.push(self.rhs());
        stack.push(self.lhs());
        while let Some(current) = stack.pop() {
            if current.deferred_kind.get() == DeferredKind::Rope {
                let current_rope_string = current.as_rope_string();
                stack.push(current_rope_string.rhs());
                stack.push(current_rope_string.lhs());
                continue;
            }

            pieces.push(current);
        }

        let views: Vec<Utf16View<'_>> = pieces.iter().map(|piece| piece.utf16_string_view()).collect();
        let string = concatenate(&views);
        debug_assert_eq!(
            Utf16View::of_string(&string).length_in_code_units(),
            self.base.length_in_utf16_code_units()
        );

        self.base.set_resolved_utf16_string(string);
        self.base.deferred_kind.set(DeferredKind::None);
        self.lhs.set(None);
        self.rhs.set(None);
    }
}

impl Substring {
    fn new(source_string: Gc<PrimitiveString>, code_unit_offset: usize, code_unit_length: usize) -> Self {
        Self {
            base: PrimitiveString::new_deferred(Self::CLASS, DeferredKind::Substring, code_unit_length),
            source_string: Cell::new(Some(source_string)),
            code_unit_offset,
        }
    }

    fn source_string(&self) -> Gc<PrimitiveString> {
        self.source_string
            .get()
            .expect("an unresolved substring has its source")
    }

    fn resolve(&self) {
        let source_string = self.source_string();
        let source_view = source_string
            .utf16_string_view()
            .substring_view(self.code_unit_offset, self.base.length_in_utf16_code_units());

        let string = source_view.to_utf16_string();
        self.base.set_resolved_utf16_string(string);
        self.base.deferred_kind.set(DeferredKind::None);
        self.source_string.set(None);
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::interpreter::vm::FLY_STRING_CACHE_SIZE;
    use crate::runtime::symbol::Symbol;

    fn is_same(lhs: Gc<PrimitiveString>, rhs: Gc<PrimitiveString>) -> bool {
        lhs == rhs
    }

    fn concatenate_strings(vm: &Vm, lhs: Gc<PrimitiveString>, rhs: Gc<PrimitiveString>) -> Gc<PrimitiveString> {
        PrimitiveString::create_from_concatenation(vm, lhs, rhs).expect("the strings are short enough")
    }

    #[test]
    fn creation_reuses_the_strings_the_vm_caches() {
        let vm = Vm::create();
        assert!(is_same(
            PrimitiveString::create(&vm, Utf16String::default()),
            vm.empty_string()
        ));
        assert!(is_same(PrimitiveString::create_from_utf8(&vm, ""), vm.empty_string()));
        assert!(is_same(
            PrimitiveString::create_from_utf8(&vm, "a"),
            vm.single_ascii_character_string(b'a')
        ));
        assert!(is_same(
            PrimitiveString::create_from_utf8(&vm, "abc"),
            PrimitiveString::create_from_utf8(&vm, "abc")
        ));
        let long = "a string too long to be stored inline";
        assert!(!is_same(
            PrimitiveString::create_from_utf8(&vm, long),
            PrimitiveString::create_from_utf8(&vm, long)
        ));
        let fly_string = Utf16FlyString::from_utf8(long);
        let from_fly_string = PrimitiveString::create_from_fly_string(&vm, &fly_string);
        assert!(is_same(
            from_fly_string,
            PrimitiveString::create_from_fly_string(&vm, &fly_string)
        ));
        assert_eq!(from_fly_string.to_utf8(), long);
        let non_ascii = PrimitiveString::create_from_utf8(&vm, "\u{e9}");
        assert_eq!(non_ascii.length_in_utf16_code_units(), 1);
        assert_eq!(non_ascii.code_unit_at(0), 0xe9);
        assert_eq!(vm.cached_strings().object_Object.to_utf8(), "[object Object]");
        assert_eq!(vm.cached_strings().bigint.to_utf8(), "bigint");
    }

    #[test]
    fn numeric_strings_are_cached() {
        let vm = Vm::create();
        for number in [0, 5, 10, 999, 1000, 4_294_967_295, 123_456_789_012_345] {
            let string = PrimitiveString::create_from_unsigned_integer(&vm, number);
            assert_eq!(string.to_utf8(), number.to_string());
            assert!(is_same(
                string,
                PrimitiveString::create_from_unsigned_integer(&vm, number)
            ));
        }
        assert!(is_same(
            PrimitiveString::create_from_unsigned_integer(&vm, 7),
            vm.single_ascii_character_string(b'7')
        ));
    }

    #[test]
    fn concatenation_avoids_ropes_where_cpp_does() {
        let vm = Vm::create();
        let empty = vm.empty_string();
        let abcd = PrimitiveString::create_from_utf8(&vm, "abcd");
        assert!(is_same(concatenate_strings(&vm, empty, empty), empty));
        assert!(is_same(concatenate_strings(&vm, empty, abcd), abcd));
        assert!(is_same(concatenate_strings(&vm, abcd, empty), abcd));

        let short_flat = concatenate_strings(&vm, abcd, PrimitiveString::create_from_utf8(&vm, "efg"));
        assert_eq!(short_flat.deferred_kind.get(), DeferredKind::None);
        assert!(is_same(short_flat, PrimitiveString::create_from_utf8(&vm, "abcdefg")));

        let rope = concatenate_strings(&vm, abcd, PrimitiveString::create_from_utf8(&vm, "efgh"));
        assert_eq!(rope.deferred_kind.get(), DeferredKind::Rope);
        assert!(!rope.has_utf16_string());
        assert_eq!(rope.length_in_utf16_code_units(), 8);
        assert_eq!(rope.to_utf8(), "abcdefgh");
        assert_eq!(rope.deferred_kind.get(), DeferredKind::None);

        let non_ascii = concatenate_strings(
            &vm,
            PrimitiveString::create_from_utf8(&vm, "\u{e9}"),
            PrimitiveString::create_from_utf8(&vm, "x"),
        );
        assert_eq!(non_ascii.deferred_kind.get(), DeferredKind::Rope);
        assert_eq!(non_ascii.to_utf8(), "\u{e9}x");
    }

    #[test]
    fn long_ropes_resolve_without_recursion() {
        let vm = Vm::create();
        let piece = PrimitiveString::create_from_utf8(&vm, "ab");
        let mut left_deep = vm.empty_string();
        let mut right_deep = vm.empty_string();
        for _ in 0..50_000 {
            left_deep = concatenate_strings(&vm, left_deep, piece);
            right_deep = concatenate_strings(&vm, piece, right_deep);
        }
        let mixed = concatenate_strings(&vm, left_deep, PrimitiveString::create_from_utf8(&vm, "\u{1f600}"));
        vm.heap().collect_garbage();
        assert_eq!(left_deep.length_in_utf16_code_units(), 100_000);
        assert!(*left_deep == *right_deep);
        assert_eq!(mixed.length_in_utf16_code_units(), 100_002);
        assert_eq!(mixed.code_unit_at(99_999), u16::from(b'b'));
        assert_eq!(mixed.code_unit_at(100_000), 0xd83d);
        assert_eq!(right_deep.utf16_string_view().code_unit_at(0), u16::from(b'a'));
    }

    #[test]
    fn substrings_view_their_source_until_they_resolve() {
        let vm = Vm::create();
        let source = PrimitiveString::create_from_utf8(&vm, "hello, wonderful world");
        let length = source.length_in_utf16_code_units();
        assert!(is_same(
            PrimitiveString::create_from_substring(&vm, source, 0, length),
            source
        ));
        assert!(is_same(
            PrimitiveString::create_from_substring(&vm, source, 3, 0),
            vm.empty_string()
        ));
        assert!(is_same(
            PrimitiveString::create_from_substring(&vm, source, 0, 1),
            vm.single_ascii_character_string(b'h')
        ));

        let wonderful = PrimitiveString::create_from_substring(&vm, source, 7, 9);
        assert_eq!(wonderful.deferred_kind.get(), DeferredKind::Substring);
        assert_eq!(wonderful.utf16_string_view(), "wonderful");
        assert_eq!(wonderful.code_unit_at(8), u16::from(b'l'));
        assert_eq!(wonderful.deferred_kind.get(), DeferredKind::Substring);

        let der = PrimitiveString::create_from_substring(&vm, wonderful, 3, 3);
        assert!(is_same(der.as_substring().source_string(), source));
        assert_eq!(der.to_utf8(), "der");

        assert_eq!(Utf16View::of_string(&wonderful.utf16_string()), "wonderful");
        assert_eq!(wonderful.deferred_kind.get(), DeferredKind::None);
        assert!(wonderful.as_substring().source_string.get().is_none());

        let rope = concatenate_strings(&vm, source, PrimitiveString::create_from_utf8(&vm, ", and more"));
        let spanning = PrimitiveString::create_from_substring(&vm, rope, 17, 10);
        assert_eq!(spanning.to_utf8(), "world, and");
        assert_eq!(rope.deferred_kind.get(), DeferredKind::None);
    }

    #[test]
    fn equality_compares_code_units_whatever_the_representation() {
        let vm = Vm::create();
        let flat = PrimitiveString::create_from_utf8(&vm, "abcdefgh");
        let rope = concatenate_strings(
            &vm,
            PrimitiveString::create_from_utf8(&vm, "abcd"),
            PrimitiveString::create_from_utf8(&vm, "efgh"),
        );
        let source = PrimitiveString::create_from_utf8(&vm, "xxabcdefghxx");
        let substring = PrimitiveString::create_from_substring(&vm, source, 2, 8);
        let utf16 = PrimitiveString::create_from_utf16_view(
            &vm,
            Utf16View::Utf16(&[0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68]),
        );
        assert!(*flat == *substring && *substring == *rope && *rope == *utf16);
        assert_eq!(substring.deferred_kind.get(), DeferredKind::Substring);
        assert!(*flat != *PrimitiveString::create_from_utf8(&vm, "abcdefgX"));
        assert!(*flat != *PrimitiveString::create_from_utf8(&vm, "abcdefg"));
    }

    #[test]
    fn property_keys_of_strings_are_canonical_and_interned() {
        let vm = Vm::create();
        assert_eq!(
            PrimitiveString::create_from_utf8(&vm, "123").property_key(&vm),
            PropertyKey::from(123u32)
        );
        let leading_zero = PrimitiveString::create_from_utf8(&vm, "0123").property_key(&vm);
        assert!(leading_zero.is_string());

        let text = "a string that is not stored as a fly string";
        let string = PrimitiveString::create_from_utf8(&vm, text);
        let key = string.property_key(&vm);
        assert_eq!(key, PropertyKey::from_utf8(text));
        assert!(
            vm.string_to_atom_cache().borrow()[0]
                .string
                .is_some_and(|cached| is_same(cached, string))
        );
        assert_eq!(string.property_key(&vm), key);

        let fly_string = Utf16FlyString::from_utf8("a string that is stored as a fly string");
        let from_fly_string = PrimitiveString::create_from_fly_string(&vm, &fly_string);
        assert_eq!(
            from_fly_string.property_key(&vm).as_string().raw_identity(),
            fly_string.raw_identity()
        );
    }

    #[test]
    fn get_reads_the_length_and_the_code_units() {
        let vm = Vm::create();
        let string = PrimitiveString::create_from_utf8(&vm, "h\u{e9}llo");
        let get = |key: &PropertyKey| string.get(&vm, key).expect("getting from a string cannot throw");
        assert_eq!(get(&vm.names.length), Some(Value::from_i32(5)));
        let character = get(&PropertyKey::from(1u32)).expect("index 1 exists");
        assert_eq!(character.as_string().to_utf8(), "\u{e9}");
        assert!(is_same(
            get(&PropertyKey::from_utf8("4")).expect("index 4 exists").as_string(),
            vm.single_ascii_character_string(b'o')
        ));
        assert_eq!(get(&PropertyKey::from(5u32)), None);
        assert_eq!(get(&PropertyKey::from_utf8("foo")), None);
        assert_eq!(get(&PropertyKey::from(Symbol::create_private(&vm))), None);
    }

    #[inline(never)]
    fn create_rope_of_unreachable_parts(vm: &Vm) -> Gc<PrimitiveString> {
        let lhs = PrimitiveString::create_from_utf8(vm, "the left part of the rope, ");
        let rhs = PrimitiveString::create_from_substring(
            vm,
            PrimitiveString::create_from_utf8(vm, "xx the right part of the rope xx"),
            3,
            26,
        );
        concatenate_strings(vm, lhs, rhs)
    }

    #[test]
    fn deferred_strings_keep_their_parts_alive() {
        let vm = Vm::create();
        let rope = create_rope_of_unreachable_parts(&vm);
        for _ in 0..3 {
            vm.heap().collect_garbage();
        }
        assert_eq!(rope.to_utf8(), "the left part of the rope, the right part of the rope");
    }

    #[test]
    fn strings_survive_collecting_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let mut string = PrimitiveString::create_from_utf8(&vm, "start");
        for index in 0..64u64 {
            let number = PrimitiveString::create_from_unsigned_integer(&vm, index * 1_000_003);
            string = concatenate_strings(&vm, string, number);
            let length = string.length_in_utf16_code_units();
            string = PrimitiveString::create_from_substring(&vm, string, 1, length - 1);
            assert!(string.property_key(&vm).is_string());
        }
        vm.heap().set_should_collect_on_every_allocation(false);
        let mut expected = String::from("start");
        for index in 0..64u64 {
            expected.push_str(&(index * 1_000_003).to_string());
            expected.remove(0);
        }
        assert_eq!(string.to_utf8(), expected);
    }

    #[inline(never)]
    fn fill_the_fly_string_cache(vm: &Vm) {
        for index in 0..4096 {
            let fly_string = Utf16FlyString::from_utf8(&format!("a fly string that is cached weakly {index}"));
            PrimitiveString::create_from_fly_string(vm, &fly_string);
        }
    }

    #[test]
    fn the_fly_string_cache_does_not_keep_strings_alive() {
        let vm = Vm::create();
        fill_the_fly_string_cache(&vm);
        let filled_slots = || vm.fly_string_cache().iter().filter(|slot| slot.get().is_some()).count();
        assert!(filled_slots() > FLY_STRING_CACHE_SIZE / 2);
        vm.heap().collect_garbage();
        assert!(filled_slots() < 16, "{} cache slots survived", filled_slots());
    }
}
