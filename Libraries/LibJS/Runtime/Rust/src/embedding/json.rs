/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! JSON parsing and serialization.
//!
//! The functions here follow the contract object.rs states for the embedding module, and run in the current realm,
//! whose intrinsics the values they create use.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    JSOwnedUtf16String, JSUtf16View, completion_into_abi, owned_utf16_string_into_abi, vm_from_abi,
};
use crate::layout::host_class::{JSCompletion, JSVM, JSValue};
use crate::layout::value::Value;
use crate::runtime::json_object::JSONObject;

/// ParseJSON ( text ) without a reviver, whose payload is the value the JSON text describes. Text that is not JSON
/// throws a SyntaxError. The text is borrowed for the call. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_json_parse(vm: *mut JSVM, text: JSUtf16View) -> JSCompletion {
    // SAFETY: See the module documentation; the view is of code units that outlive the call.
    let (vm, text) = unsafe { (vm_from_abi(vm), text.as_view()) };
    completion_into_abi(JSONObject::parse_json(vm, text, None))
}

/// JSON.stringify ( value, replacer, space ), with undefined for an argument not given. The payload is whether there
/// is a string, which is then written to `string` as an owned string: values such as undefined and functions have
/// none. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_json_stringify(
    vm: *mut JSVM,
    value: JSValue,
    replacer: JSValue,
    space: JSValue,
    string: *mut JSOwnedUtf16String,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let vm = unsafe { vm_from_abi(vm) };
    let serialized = JSONObject::stringify_impl(vm, Value(value), Value(replacer), Value(space));
    completion_into_abi(serialized.map(|serialized| {
        let Some(serialized) = serialized else {
            return false;
        };
        assert!(!string.is_null(), "the embedder passes an out parameter");
        // SAFETY: As above, `string` is writable.
        unsafe { string.write(owned_utf16_string_into_abi(serialized)) };
        true
    }))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{bool_of_payload, owned_utf16_string_from_abi, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    fn view_of(code_units: &[u16]) -> JSUtf16View {
        JSUtf16View {
            data: code_units.as_ptr().cast(),
            length_in_code_units: code_units.len(),
            has_ascii_storage: false,
        }
    }

    #[test]
    fn json_round_trips_through_the_embedder() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let text: Vec<u16> = r#"{"a":[1,2,{"b":null}],"c":"α"}"#.encode_utf16().collect();
        // SAFETY: The VM is live and the text outlives the call.
        let parsed = unsafe { js_json_parse(abi_vm, view_of(&text)) };
        assert!(parsed.variant == JS_COMPLETION_NORMAL);
        realm
            .global_object()
            .define_direct_property(&vm, &key("parsed"), Value(parsed.payload), DEFAULT_ATTRIBUTES);
        assert_eq!(
            utf8(run_script(&vm, realm, "parsed.a[2].b === null && parsed.c === '\u{3b1}'").must()),
            "true"
        );

        let space = Value::from_i32(1);
        let mut string = 0;
        // SAFETY: The VM is live and the out parameter is writable.
        let completion =
            unsafe { js_json_stringify(abi_vm, parsed.payload, Value::UNDEFINED.0, space.0, &raw mut string) };
        assert!(bool_of_payload(completion));
        // SAFETY: The completion wrote an owned string, which this adopts once.
        let serialized = unsafe { owned_utf16_string_from_abi(string) };
        assert_eq!(
            Utf16View::of_string(&serialized).to_utf8(),
            "{\n \"a\": [\n  1,\n  2,\n  {\n   \"b\": null\n  }\n ],\n \"c\": \"\u{3b1}\"\n}"
        );

        // Values without JSON have no string, and the replacer and toJSON run as for JSON.stringify().
        let mut untouched = 7;
        // SAFETY: As above.
        let completion = unsafe {
            js_json_stringify(
                abi_vm,
                Value::UNDEFINED.0,
                Value::UNDEFINED.0,
                Value::UNDEFINED.0,
                &raw mut untouched,
            )
        };
        assert!(!bool_of_payload(completion));
        assert_eq!(untouched, 7);
        let replacer = run_script(
            &vm,
            realm,
            "(key, value) => typeof value === 'number' ? value * 2 : value",
        )
        .must();
        let throwing = run_script(&vm, realm, "({ toJSON() { throw 'from toJSON'; } })").must();
        // SAFETY: As above.
        unsafe {
            let completion = js_json_stringify(abi_vm, parsed.payload, replacer.0, Value::UNDEFINED.0, &raw mut string);
            assert!(bool_of_payload(completion));
            assert_eq!(
                Utf16View::of_string(&owned_utf16_string_from_abi(string)).to_utf8(),
                "{\"a\":[2,4,{\"b\":null}],\"c\":\"\u{3b1}\"}"
            );
            let completion = js_json_stringify(
                abi_vm,
                throwing.0,
                Value::UNDEFINED.0,
                Value::UNDEFINED.0,
                &raw mut untouched,
            );
            assert!(completion.variant == JS_COMPLETION_THROW);
            assert_eq!(utf8(Value(completion.payload)), "from toJSON");
            assert_eq!(untouched, 7);
        }

        let malformed: Vec<u16> = "{\"a\":}".encode_utf16().collect();
        // SAFETY: As above.
        let completion = unsafe { js_json_parse(abi_vm, view_of(&malformed)) };
        assert!(completion.variant == JS_COMPLETION_THROW);
    }
}
