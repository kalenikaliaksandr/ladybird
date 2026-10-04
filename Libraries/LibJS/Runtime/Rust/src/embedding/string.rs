/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! String values: creating them from the embedder's strings, and reading their code units. Every function runs on the
//! thread that owns the VM.

use core::mem::ManuallyDrop;

use ak::Utf16FlyString;

use crate::embedding::abi_types::{
    JSOwnedUtf16String, JSPrimitiveString, JSUtf16View, cell_from_abi, cell_into_abi, completion_into_abi,
    owned_utf16_string_from_abi, owned_utf16_string_into_abi, property_key_into_abi, vm_from_abi,
};
use crate::layout::host_class::{JSCompletion, JSPropertyKey, JSVM};
use crate::runtime::primitive_string::PrimitiveString;

/// A string of the code units `string` views, which are copied. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `string` a view of code units that stay unchanged during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_from_utf16_view(
    vm: *mut JSVM,
    string: JSUtf16View,
) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a view of code units that stay unchanged during the call.
    cell_into_abi(PrimitiveString::create_from_utf16_view(vm, unsafe { string.as_view() }))
}

/// A string of the `length` bytes of UTF-8 at `data`, converted to UTF-16, with U+FFFD in place of each sequence that
/// is not UTF-8. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `data` must point to `length` bytes unless `length` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_from_utf8(
    vm: *mut JSVM,
    data: *const u8,
    length: usize,
) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    if length == 0 {
        return cell_into_abi(vm.empty_string());
    }
    // SAFETY: The caller passes `length` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(data, length) };
    cell_into_abi(PrimitiveString::create_from_utf8(vm, &String::from_utf8_lossy(bytes)))
}

/// A string that adopts the storage of `string` without copying it, unless the string is short enough for the VM to
/// share a cached one. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `string` an AK::Utf16String whose reference the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_from_owned_utf16_string(
    vm: *mut JSVM,
    string: JSOwnedUtf16String,
) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller gives up its reference to the string.
    cell_into_abi(PrimitiveString::create(vm, unsafe {
        owned_utf16_string_from_abi(string)
    }))
}

/// The string of an AK::Utf16FlyString, which the VM caches by the fly string's identity. The caller keeps its
/// reference to the fly string. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `fly_string` the raw word of an AK::Utf16FlyString that stays alive during the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_from_utf16_fly_string(
    vm: *mut JSVM,
    fly_string: usize,
) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller keeps the fly string alive and its reference, which this borrows without releasing.
    let fly_string = ManuallyDrop::new(unsafe { Utf16FlyString::from_raw_owned(fly_string) });
    cell_into_abi(PrimitiveString::create_from_fly_string(vm, &fly_string))
}

/// The decimal digits of `number`, as a string the VM may share. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_from_unsigned_integer(vm: *mut JSVM, number: u64) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    cell_into_abi(PrimitiveString::create_from_unsigned_integer(vm, number))
}

/// The `code_unit_length` code units of `string` from `code_unit_offset` on, which must lie within it. The result
/// refers to `string` until it is first read. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `string` a string of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_substring(
    vm: *mut JSVM,
    string: *mut JSPrimitiveString,
    code_unit_offset: usize,
    code_unit_length: usize,
) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a string of the VM.
    let string = unsafe { cell_from_abi(string) };
    cell_into_abi(PrimitiveString::create_from_substring(
        vm,
        string,
        code_unit_offset,
        code_unit_length,
    ))
}

/// The concatenation of `lhs` and `rhs`, a JSPrimitiveString that is the payload of the normal completion, which throws
/// a RangeError if it would be too long. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `lhs` and `rhs` strings of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_create_concatenation(
    vm: *mut JSVM,
    lhs: *mut JSPrimitiveString,
    rhs: *mut JSPrimitiveString,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes strings of the VM.
    let (lhs, rhs) = unsafe { (cell_from_abi(lhs), cell_from_abi(rhs)) };
    completion_into_abi(
        PrimitiveString::create_from_concatenation(vm, lhs, rhs).map(cell_into_abi::<JSPrimitiveString>),
    )
}

/// The code units of `string` as an AK::Utf16String, which the caller owns. It shares the string's storage, after
/// resolving a string the VM built lazily. Call on the VM's thread.
///
/// # Safety
///
/// `string` must be a string of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_utf16_string(string: *mut JSPrimitiveString) -> JSOwnedUtf16String {
    // SAFETY: The caller passes a string of the VM.
    owned_utf16_string_into_abi(unsafe { cell_from_abi(string) }.utf16_string())
}

/// A view of the code units of `string`, after resolving a string the VM built lazily. The view stays valid for as long
/// as the string lives. Call on the VM's thread.
///
/// # Safety
///
/// `string` must be a string of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_utf16_view(string: *mut JSPrimitiveString) -> JSUtf16View {
    // SAFETY: The caller passes a string of the VM.
    JSUtf16View::of(unsafe { cell_from_abi(string) }.resolved_utf16_string_view())
}

/// The length of `string` in UTF-16 code units. Call on the VM's thread.
///
/// # Safety
///
/// `string` must be a string of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_length_in_code_units(string: *mut JSPrimitiveString) -> usize {
    // SAFETY: The caller passes a string of the VM.
    unsafe { cell_from_abi(string) }.length_in_utf16_code_units()
}

/// Whether `lhs` and `rhs` have the same code units. Call on the VM's thread.
///
/// # Safety
///
/// `lhs` and `rhs` must be strings of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_equals(lhs: *mut JSPrimitiveString, rhs: *mut JSPrimitiveString) -> bool {
    // SAFETY: The caller passes strings of the VM.
    let (lhs, rhs) = unsafe { (cell_from_abi(lhs), cell_from_abi(rhs)) };
    *lhs == *rhs
}

/// The property key of `string`, which the caller owns along with its reference to the key's string. Call on the
/// VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `string` a string of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_string_to_property_key(vm: *mut JSVM, string: *mut JSPrimitiveString) -> JSPropertyKey {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a string of the VM.
    property_key_into_abi(unsafe { cell_from_abi(string) }.property_key(vm))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use ak::{Utf16FlyString, Utf16String, Utf16StringUnits};

    use super::*;
    use crate::embedding::abi_types::{cell_of_payload, completion_from_abi, property_key_from_abi, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::JS_COMPLETION_THROW;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::utf8;
    use crate::runtime::property_key::PropertyKey;
    use crate::runtime::realm::test_realm::TestRealm;
    use crate::utf16::Utf16View;

    fn utf16_storage_address(string: &Utf16String) -> *const u16 {
        match string.as_units() {
            Utf16StringUnits::Utf16(code_units) => code_units.as_ptr(),
            Utf16StringUnits::Ascii(_) => panic!("the string has UTF-16 storage"),
        }
    }

    fn view_to_utf8(view: JSUtf16View) -> String {
        // SAFETY: The tests view strings that outlive the view.
        unsafe { view.as_view() }.to_utf8()
    }

    #[test]
    fn owned_strings_cross_both_ways_without_copies() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);
        let string = Utf16String::from_utf8("αβγδεζηθικλμ, a string with UTF-16 storage");
        let identity = string.raw_identity();
        let storage = utf16_storage_address(&string);

        // SAFETY: The VM is live, and the test gives up its reference to the string.
        unsafe {
            let primitive_string =
                js_string_create_from_owned_utf16_string(abi_vm, owned_utf16_string_into_abi(string));
            let view = js_string_utf16_view(primitive_string);
            assert!(!view.has_ascii_storage && view.data == storage.cast());
            assert_eq!(js_string_length_in_code_units(primitive_string), 42);
            let read_back = owned_utf16_string_from_abi(js_string_utf16_string(primitive_string));
            assert_eq!(read_back.raw_identity(), identity);
        }
    }

    #[test]
    fn strings_from_views_and_utf8_are_copies() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);
        let code_units: Vec<u16> = "a borrowed UTF-16 view, ü".encode_utf16().collect();

        // SAFETY: The VM is live, and the views and bytes outlive the calls.
        unsafe {
            let from_view = js_string_create_from_utf16_view(abi_vm, JSUtf16View::of(Utf16View::Utf16(&code_units)));
            assert!(js_string_utf16_view(from_view).data != code_units.as_ptr().cast());
            assert_eq!(
                view_to_utf8(js_string_utf16_view(from_view)),
                "a borrowed UTF-16 view, ü"
            );

            let from_ascii =
                js_string_create_from_utf16_view(abi_vm, JSUtf16View::of(Utf16View::Ascii(b"ascii storage")));
            assert_eq!(view_to_utf8(js_string_utf16_view(from_ascii)), "ascii storage");
            let empty_view = JSUtf16View {
                data: core::ptr::null(),
                length_in_code_units: 0,
                has_ascii_storage: true,
            };
            assert_eq!(
                js_string_length_in_code_units(js_string_create_from_utf16_view(abi_vm, empty_view)),
                0
            );

            let bytes = b"caf\xc3\xa9 \xff!";
            let from_utf8 = js_string_create_from_utf8(abi_vm, bytes.as_ptr(), bytes.len());
            assert_eq!(view_to_utf8(js_string_utf16_view(from_utf8)), "café \u{fffd}!");
            let empty_utf8 = js_string_create_from_utf8(abi_vm, core::ptr::null(), 0);
            assert_eq!(js_string_length_in_code_units(empty_utf8), 0);

            let same_text = js_string_create_from_utf8(abi_vm, b"ascii storage".as_ptr(), 13);
            assert!(js_string_equals(same_text, from_ascii) && !js_string_equals(same_text, from_utf8));
            let number = js_string_create_from_unsigned_integer(abi_vm, 1_234_567);
            assert_eq!(view_to_utf8(js_string_utf16_view(number)), "1234567");
        }
    }

    #[test]
    fn fly_strings_share_the_cached_string() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);
        let fly_string = Utf16FlyString::from_utf8("an interned string");

        // SAFETY: The VM is live, and the test keeps the fly string alive.
        unsafe {
            let first = js_string_create_from_utf16_fly_string(abi_vm, fly_string.raw_identity());
            let second = js_string_create_from_utf16_fly_string(abi_vm, fly_string.raw_identity());
            assert!(first == second);
            assert_eq!(view_to_utf8(js_string_utf16_view(first)), "an interned string");
        }
        assert_eq!(Utf16View::of_fly_string(&fly_string).to_utf8(), "an interned string");
    }

    #[test]
    fn ropes_and_substrings_resolve_before_they_are_read() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);

        // SAFETY: The VM is live, and the strings are the cells the payloads point to.
        unsafe {
            let lhs = js_string_create_from_utf8(abi_vm, b"the left half, ".as_ptr(), 15);
            let rhs = js_string_create_from_utf8(abi_vm, b"and the right half".as_ptr(), 18);
            let rope = cell_into_abi(cell_of_payload::<JSPrimitiveString>(js_string_create_concatenation(
                abi_vm, lhs, rhs,
            )));
            assert_eq!(js_string_length_in_code_units(rope), 33);

            // A substring of a substring is taken from the rope, and reading it resolves both.
            let substring = js_string_create_substring(abi_vm, rope, 4, 14);
            let inner_substring = js_string_create_substring(abi_vm, substring, 0, 10);
            let view = js_string_utf16_view(inner_substring);
            assert_eq!(view_to_utf8(view), "left half,");
            // The view is of the substring's own storage, which outlives the strings it was taken from.
            vm.heap().collect_garbage();
            assert_eq!(view_to_utf8(view), "left half,");
            assert_eq!(
                view_to_utf8(js_string_utf16_view(rope)),
                "the left half, and the right half"
            );

            let index = js_string_create_from_utf8(abi_vm, b"12".as_ptr(), 2);
            let key = js_string_to_property_key(abi_vm, index);
            assert_eq!(property_key_from_abi(&raw const key).as_number(), 12);
            let named = js_string_to_property_key(abi_vm, rope);
            assert!(
                *property_key_from_abi(&raw const named) == PropertyKey::from_utf8("the left half, and the right half")
            );
            // The key's word owns a reference to its string, which this gives back.
            drop(core::ptr::from_ref(&named).cast::<PropertyKey>().read());
        }
    }

    #[test]
    fn concatenations_that_are_too_long_throw() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);

        // SAFETY: The VM is live, and the strings are the cells the payloads point to.
        unsafe {
            // Ropes double the length without building the string.
            let mut string = js_string_create_from_utf8(abi_vm, b"ab".as_ptr(), 2);
            for _ in 0..30 {
                let doubled = js_string_create_concatenation(abi_vm, string, string);
                string = cell_into_abi(cell_of_payload::<JSPrimitiveString>(doubled));
            }
            assert_eq!(js_string_length_in_code_units(string), 1 << 31);
            let completion = js_string_create_concatenation(abi_vm, string, string);
            assert!(completion.variant == JS_COMPLETION_THROW);
            let thrown = completion_from_abi(completion)
                .expect_err("the concatenation throws")
                .value();
            assert_eq!(
                utf8(Value::from_string(thrown.to_primitive_string(&vm).must())),
                "RangeError: Invalid string length"
            );
        }
    }
}
