/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Values and their conversions. Every function runs on the thread that owns the VM, and the conversions that can
//! throw follow the completion conventions of abi_types.rs.

use crate::embedding::abi_types::{
    JSBigInt, JSOwnedUtf16String, JSPrimitiveString, cell_into_abi, completion_into_abi, completion_writing_result_to,
    owned_utf16_string_into_abi, property_key_into_abi, value_from_abi, vm_from_abi,
};
use crate::layout::host_class::{JSCompletion, JSPropertyKey, JSVM, JSValue};
use crate::runtime::abstract_operations::can_be_held_weakly;
use crate::runtime::value::{PreferredType, is_loosely_equal, is_strictly_equal, same_value, same_value_zero};

/// The type hint of ToPrimitive, as C++ Value::PreferredType.
pub type JSPreferredType = u8;

pub const JS_PREFERRED_TYPE_DEFAULT: JSPreferredType = 0;
pub const JS_PREFERRED_TYPE_STRING: JSPreferredType = 1;
pub const JS_PREFERRED_TYPE_NUMBER: JSPreferredType = 2;

fn preferred_type_from_abi(preferred_type: JSPreferredType) -> PreferredType {
    match preferred_type {
        JS_PREFERRED_TYPE_DEFAULT => PreferredType::Default,
        JS_PREFERRED_TYPE_STRING => PreferredType::String,
        JS_PREFERRED_TYPE_NUMBER => PreferredType::Number,
        preferred_type => panic!("the embedder passed an unknown preferred type {preferred_type}"),
    }
}

/// ToBoolean(value). Call on the VM's thread.
///
/// # Safety
///
/// `value` must be a value of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_boolean(value: JSValue) -> bool {
    value_from_abi(value).to_boolean()
}

/// IsCallable(value). Call on the VM's thread.
///
/// # Safety
///
/// `value` must be a value of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_function(value: JSValue) -> bool {
    value_from_abi(value).is_function()
}

/// IsConstructor(value). Call on the VM's thread.
///
/// # Safety
///
/// `value` must be a value of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_constructor(value: JSValue) -> bool {
    value_from_abi(value).is_constructor()
}

/// IsStrictlyEqual(lhs, rhs). Call on the VM's thread.
///
/// # Safety
///
/// `lhs` and `rhs` must be values of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_strictly_equal(lhs: JSValue, rhs: JSValue) -> bool {
    is_strictly_equal(value_from_abi(lhs), value_from_abi(rhs))
}

/// SameValue(lhs, rhs). Call on the VM's thread.
///
/// # Safety
///
/// `lhs` and `rhs` must be values of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_same_value(lhs: JSValue, rhs: JSValue) -> bool {
    same_value(value_from_abi(lhs), value_from_abi(rhs))
}

/// SameValueZero(lhs, rhs). Call on the VM's thread.
///
/// # Safety
///
/// `lhs` and `rhs` must be values of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_same_value_zero(lhs: JSValue, rhs: JSValue) -> bool {
    same_value_zero(value_from_abi(lhs), value_from_abi(rhs))
}

/// CanBeHeldWeakly(v): whether `value` is an object or a symbol that is not in the global symbol registry, which
/// WeakRefs, WeakMaps, WeakSets and FinalizationRegistries accept. Call on the VM's thread.
///
/// # Safety
///
/// `value` must be a value of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_can_be_held_weakly(value: JSValue) -> bool {
    can_be_held_weakly(value_from_abi(value))
}

/// The result of the typeof operator on `value`, one of the VM's cached strings. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_typeof(vm: *mut JSVM, value: JSValue) -> *mut JSPrimitiveString {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    cell_into_abi(value_from_abi(value).typeof_(vm))
}

/// A description of `value` for diagnostics, which never runs JavaScript: strings as they are, numbers, symbols and
/// BigInts as ToString would show them, and objects as "[object ClassName]". The caller owns the returned string. Call
/// on the VM's thread.
///
/// # Safety
///
/// `value` must be a value of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_utf16_string_without_side_effects(value: JSValue) -> JSOwnedUtf16String {
    owned_utf16_string_into_abi(value_from_abi(value).to_utf16_string_without_side_effects())
}

/// ToPrimitive(value, preferred_type), whose result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_primitive(
    vm: *mut JSVM,
    value: JSValue,
    preferred_type: JSPreferredType,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).to_primitive(vm, preferred_type_from_abi(preferred_type)))
}

/// ToNumber(value), whose Number result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_number(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).to_number(vm))
}

/// ToNumeric(value), whose Number or BigInt result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_numeric(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).to_numeric(vm))
}

/// ToObject(value), whose JSObject result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_object(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).to_object(vm))
}

/// ToNumber(value) as a double, written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_double(vm: *mut JSVM, value: JSValue, out: *mut f64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_double(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToIntegerOrInfinity(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_integer_or_infinity(vm: *mut JSVM, value: JSValue, out: *mut f64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_integer_or_infinity(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToInt32(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_i32(vm: *mut JSVM, value: JSValue, out: *mut i32) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_i32(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToUint32(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_u32(vm: *mut JSVM, value: JSValue, out: *mut u32) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_u32(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToInt16(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_i16(vm: *mut JSVM, value: JSValue, out: *mut i16) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_i16(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToUint16(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_u16(vm: *mut JSVM, value: JSValue, out: *mut u16) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_u16(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToInt8(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_i8(vm: *mut JSVM, value: JSValue, out: *mut i8) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_i8(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToUint8(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_u8(vm: *mut JSVM, value: JSValue, out: *mut u8) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_u8(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToUint8Clamp(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_u8_clamp(vm: *mut JSVM, value: JSValue, out: *mut u8) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_u8_clamp(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToBigInt(value), whose JSBigInt result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_bigint(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).to_bigint(vm).map(cell_into_abi::<JSBigInt>))
}

/// ToBigInt64(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_bigint_int64(vm: *mut JSVM, value: JSValue, out: *mut i64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_bigint_int64(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToBigUint64(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_bigint_uint64(vm: *mut JSVM, value: JSValue, out: *mut u64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_bigint_uint64(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToLength(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_length(vm: *mut JSVM, value: JSValue, out: *mut u64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_length(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToIndex(value), written to `out`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_index(vm: *mut JSVM, value: JSValue, out: *mut u64) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_index(vm);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToPropertyKey(value), written to `out`. The caller owns the key, and with it the reference to its string that a
/// string key holds. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_property_key(
    vm: *mut JSVM,
    value: JSValue,
    out: *mut JSPropertyKey,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value).to_property_key(vm).map(property_key_into_abi);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// ToString(value) as a string cell, a JSPrimitiveString that is the payload of the normal completion. Call on the
/// VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_primitive_string(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(
        value_from_abi(value)
            .to_primitive_string(vm)
            .map(cell_into_abi::<JSPrimitiveString>),
    )
}

/// ToString(value), written to `out`. The caller owns the string. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `value` a value of it, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_to_utf16_string(
    vm: *mut JSVM,
    value: JSValue,
    out: *mut JSOwnedUtf16String,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let result = value_from_abi(value)
        .to_utf16_string(vm)
        .map(owned_utf16_string_into_abi);
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe { completion_writing_result_to(result, out) }
}

/// IsArray(value), whose bool result is the payload of the normal completion, and which throws for a revoked proxy.
/// Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_array(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).is_array(vm))
}

/// IsRegExp(value), whose bool result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_regexp(vm: *mut JSVM, value: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(value_from_abi(value).is_regexp(vm))
}

/// IsLooselyEqual(lhs, rhs), whose bool result is the payload of the normal completion. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `lhs` and `rhs` values of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_value_is_loosely_equal(vm: *mut JSVM, lhs: JSValue, rhs: JSValue) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(is_loosely_equal(vm, value_from_abi(lhs), value_from_abi(rhs)))
}
