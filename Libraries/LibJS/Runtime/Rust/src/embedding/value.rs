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

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use ak::Utf16String;

    use super::*;
    use crate::embedding::abi_types::{
        bool_of_payload, cell_from_abi, cell_of_payload, completion_from_abi, owned_utf16_string_from_abi,
        property_key_from_abi, value_into_abi, vm_into_abi,
    };
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW, JSObject};
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_key::PropertyKey;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    fn owned_string_to_utf8(string: JSOwnedUtf16String) -> String {
        // SAFETY: The runtime gave the string's reference to the test.
        Utf16View::of_string(&unsafe { owned_utf16_string_from_abi(string) }).to_utf8()
    }

    fn thrown_error_text(vm: &Vm, completion: JSCompletion) -> String {
        assert!(completion.variant == JS_COMPLETION_THROW, "the conversion throws");
        let thrown = completion_from_abi(completion)
            .expect_err("the completion throws")
            .value();
        utf8(Value::from_string(thrown.to_primitive_string(vm).must()))
    }

    #[test]
    fn conversions_call_value_of_and_report_what_it_throws() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let seven = value_into_abi(run_script(&vm, realm, "({ valueOf() { return 7.5 } })").must());
        let throwing =
            value_into_abi(run_script(&vm, realm, "({ valueOf() { throw new RangeError('from valueOf') } })").must());

        let mut double = 0.0;
        // SAFETY: The VM and values are live, and the out parameters are locals.
        unsafe {
            assert!(js_value_to_double(abi_vm, seven, &raw mut double).variant == JS_COMPLETION_NORMAL);
            assert!(double == 7.5);
            let completion = js_value_to_double(abi_vm, throwing, &raw mut double);
            assert_eq!(thrown_error_text(&vm, completion), "RangeError: from valueOf");
            assert!(double == 7.5, "a throw leaves the out parameter untouched");

            let mut int32 = 0;
            assert!(js_value_to_i32(abi_vm, seven, &raw mut int32).variant == JS_COMPLETION_NORMAL && int32 == 7);
            let mut clamped = 0;
            js_value_to_u8_clamp(abi_vm, value_into_abi(Value::from_f64(300.0)), &raw mut clamped);
            assert_eq!(clamped, 255);
            let mut wrapped = 0;
            js_value_to_u8(abi_vm, value_into_abi(Value::from_f64(300.0)), &raw mut wrapped);
            assert_eq!(wrapped, 44);
            let mut index = 0;
            let completion = js_value_to_index(abi_vm, value_into_abi(Value::from_f64(-1.0)), &raw mut index);
            assert_eq!(
                thrown_error_text(&vm, completion),
                "RangeError: Index must be a positive integer no greater than 2^53-1"
            );

            let number = js_value_to_number(abi_vm, seven);
            assert!(number.variant == JS_COMPLETION_NORMAL && Value(number.payload) == Value::from_f64(7.5));
            let primitive = js_value_to_primitive(abi_vm, seven, JS_PREFERRED_TYPE_NUMBER);
            assert!(Value(primitive.payload) == Value::from_f64(7.5));
            assert!(js_value_to_numeric(abi_vm, throwing).variant == JS_COMPLETION_THROW);
        }
    }

    #[test]
    fn strings_keys_and_objects_come_out_owned_or_as_cells() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let object = value_into_abi(run_script(&vm, realm, "({ toString() { return 'a long string key' } })").must());
        let symbol = value_into_abi(run_script(&vm, realm, "Symbol('s')").must());

        // SAFETY: The VM and values are live, and the out parameters are locals.
        unsafe {
            let mut string: JSOwnedUtf16String = 0;
            assert!(js_value_to_utf16_string(abi_vm, object, &raw mut string).variant == JS_COMPLETION_NORMAL);
            assert_eq!(owned_string_to_utf8(string), "a long string key");

            let mut key = JSPropertyKey { bits: 0 };
            js_value_to_property_key(abi_vm, object, &raw mut key);
            assert!(*property_key_from_abi(&raw const key) == PropertyKey::from_utf8("a long string key"));
            // The key's word owns a reference to its string, which this gives back.
            drop(core::ptr::from_ref(&key).cast::<PropertyKey>().read());
            js_value_to_property_key(abi_vm, value_into_abi(Value::from_i32(5)), &raw mut key);
            assert_eq!(property_key_from_abi(&raw const key).as_number(), 5);

            let primitive_string = js_value_to_primitive_string(abi_vm, value_into_abi(Value::from_i32(42)));
            assert_eq!(cell_of_payload::<JSPrimitiveString>(primitive_string).to_utf8(), "42");

            let wrapper = js_value_to_object(abi_vm, value_into_abi(Value::TRUE));
            assert_eq!(
                cell_of_payload::<JSObject>(wrapper).class().class_name(),
                "BooleanObject"
            );
            let completion = js_value_to_object(abi_vm, value_into_abi(Value::NULL));
            assert_eq!(
                thrown_error_text(&vm, completion),
                "TypeError: ToObject on null or undefined"
            );

            let completion = js_value_to_utf16_string(abi_vm, symbol, &raw mut string);
            assert_eq!(
                thrown_error_text(&vm, completion),
                "TypeError: Cannot convert symbol to string"
            );
            assert_eq!(
                owned_string_to_utf8(js_value_to_utf16_string_without_side_effects(symbol)),
                "Symbol(s)"
            );
            assert_eq!(
                owned_string_to_utf8(js_value_to_utf16_string_without_side_effects(object)),
                "[object Object]"
            );
            assert_eq!(cell_from_abi(js_value_typeof(abi_vm, symbol)).to_utf8(), "symbol");
            assert_eq!(cell_from_abi(js_value_typeof(abi_vm, object)).to_utf8(), "object");
        }
    }

    #[test]
    fn bigint_conversions_wrap_to_64_bits() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let big = value_into_abi(run_script(&vm, realm, "2n ** 64n + 5n").must());
        let negative = value_into_abi(run_script(&vm, realm, "-1n").must());

        // SAFETY: The VM and values are live, and the out parameters are locals.
        unsafe {
            let mut unsigned = 0;
            js_value_to_bigint_uint64(abi_vm, big, &raw mut unsigned);
            assert_eq!(unsigned, 5);
            let mut signed = 0;
            js_value_to_bigint_int64(abi_vm, negative, &raw mut signed);
            assert_eq!(signed, -1);
            js_value_to_bigint_uint64(abi_vm, negative, &raw mut unsigned);
            assert_eq!(unsigned, u64::MAX);
            let bigint = js_value_to_bigint(abi_vm, value_into_abi(Value::TRUE));
            assert!(cell_of_payload::<JSBigInt>(bigint).to_utf16_string() == Utf16String::from_utf8("1n"));
            let completion = js_value_to_bigint(abi_vm, value_into_abi(Value::from_i32(1)));
            assert_eq!(
                thrown_error_text(&vm, completion),
                "TypeError: Cannot convert number to BigInt"
            );
        }
    }

    #[test]
    fn predicates_and_equality_follow_the_spec() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let value = |source: &str| value_into_abi(run_script(&vm, realm, source).must());
        let array = value("[]");
        let revoked_proxy = value("var p = Proxy.revocable([], {}); p.revoke(); p.proxy");
        let arrow = value("() => 1");
        let class = value("(class {})");
        let regexp = value("/a/");
        let one_as_string = value("'1'");
        let nan = value_into_abi(Value::from_f64(f64::NAN));
        let positive_zero = value_into_abi(Value::from_f64(0.0));
        let negative_zero = value_into_abi(Value::from_f64(-0.0));

        // SAFETY: The VM and values are live, and the out parameters are locals.
        unsafe {
            assert!(bool_of_payload(js_value_is_array(abi_vm, array)));
            assert!(!bool_of_payload(js_value_is_array(abi_vm, arrow)));
            assert!(js_value_is_array(abi_vm, revoked_proxy).variant == JS_COMPLETION_THROW);
            assert!(bool_of_payload(js_value_is_regexp(abi_vm, regexp)));

            assert!(js_value_is_function(arrow) && !js_value_is_constructor(arrow));
            assert!(js_value_is_function(class) && js_value_is_constructor(class));
            assert!(!js_value_is_function(array));
            assert!(js_value_to_boolean(array) && !js_value_to_boolean(negative_zero));

            assert!(!js_value_is_strictly_equal(nan, nan) && js_value_same_value(nan, nan));
            assert!(js_value_is_strictly_equal(positive_zero, negative_zero));
            assert!(!js_value_same_value(positive_zero, negative_zero));
            assert!(js_value_same_value_zero(positive_zero, negative_zero));
            assert!(bool_of_payload(js_value_is_loosely_equal(
                abi_vm,
                one_as_string,
                value_into_abi(Value::from_i32(1))
            )));
            assert!(!bool_of_payload(js_value_is_loosely_equal(
                abi_vm,
                value_into_abi(Value::NULL),
                positive_zero
            )));
        }
    }
}
