/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Strings. They cross the ABI as AK::Utf16String, which the runtime's strings share storage with.

use core::ptr::NonNull;

use ak::{Utf16FlyString, Utf16String};

use crate::abi::{
    JSOwnedUtf16String, JSPrimitiveString, JSUtf16View, js_string_create_from_owned_utf16_string,
    js_string_create_from_utf16_fly_string, js_string_create_from_utf16_view, js_value_to_utf16_string,
};
use crate::completion::{ThrowCompletionOr, payload_from_abi};
use crate::value::Value;
use crate::vm::Vm;

/// ToString(value), https://tc39.es/ecma262/#sec-tostring
pub fn to_utf16_string(vm: Vm, value: Value) -> ThrowCompletionOr<Utf16String> {
    let mut string: JSOwnedUtf16String = 0;
    // SAFETY: The value is live, and the out parameter is valid for writes.
    payload_from_abi(unsafe { js_value_to_utf16_string(vm.as_raw(), value.raw(), &raw mut string) })?;
    // SAFETY: A normal completion gives the caller one reference to the string.
    Ok(unsafe { Utf16String::from_raw_owned(string) })
}

fn string_value(string: *mut JSPrimitiveString) -> Value {
    // SAFETY: The runtime returns a live string.
    unsafe { Value::from_primitive_string(NonNull::new(string).expect("the runtime creates the string")) }
}

/// A string of ASCII bytes, which are copied.
pub fn from_ascii(vm: Vm, string: &[u8]) -> Value {
    debug_assert!(string.is_ascii());
    let view = JSUtf16View {
        data: string.as_ptr().cast(),
        length_in_code_units: string.len(),
        has_ascii_storage: true,
    };
    // SAFETY: The view's bytes stay unchanged during the call.
    string_value(unsafe { js_string_create_from_utf16_view(vm.as_raw(), view) })
}

/// A string that adopts the storage of `string`.
pub fn from_utf16_string(vm: Vm, string: Utf16String) -> Value {
    // SAFETY: The string gives up its reference to the runtime.
    string_value(unsafe { js_string_create_from_owned_utf16_string(vm.as_raw(), string.into_raw()) })
}

/// The string of a fly string, which the VM caches by the fly string's identity.
pub fn from_fly_string(vm: Vm, string: &Utf16FlyString) -> Value {
    // SAFETY: The fly string stays alive during the call.
    string_value(unsafe { js_string_create_from_utf16_fly_string(vm.as_raw(), string.raw_identity()) })
}
