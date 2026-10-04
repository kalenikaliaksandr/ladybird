/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! RegExp objects, their source and flags.
//!
//! The functions here follow the contract object.rs states for the embedding module. A RegExp crosses as its
//! JSObject, and the functions that take one abort for any other object, as the C++ as<JS::RegExpObject>() does.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    JSOwnedUtf16String, cell_from_abi, completion_into_abi, owned_utf16_string_into_abi, vm_from_abi,
};
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSObject, JSVM, JSValue};
use crate::layout::value::Value;
use crate::runtime::regexp_object::{RegExpObject, regexp_create};

/// # Safety
///
/// `regexp` must be a live RegExp object.
unsafe fn regexp_from_abi(regexp: *mut JSObject) -> Gc<RegExpObject> {
    // SAFETY: The caller passes a live object.
    unsafe { cell_from_abi::<JSObject>(regexp) }
        .downcast::<RegExpObject>()
        .expect("the object is a RegExp")
}

/// RegExpCreate ( P, F ), whose payload is a RegExp of the current realm's %RegExp%. P and F are converted to strings,
/// undefined to the empty one, and a pattern or flags that do not parse throw a SyntaxError. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_regexp_create(vm: *mut JSVM, pattern: JSValue, flags: JSValue) -> JSCompletion {
    // SAFETY: See the module documentation.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(regexp_create(vm, Value(pattern), Value(flags)))
}

/// [[OriginalSource]], the pattern the RegExp was created with, as an owned string. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_regexp_pattern(regexp: *mut JSObject) -> JSOwnedUtf16String {
    // SAFETY: See the module documentation.
    owned_utf16_string_into_abi(unsafe { regexp_from_abi(regexp) }.pattern())
}

/// [[OriginalFlags]], the flags the RegExp was created with, as an owned string. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_regexp_flags(regexp: *mut JSObject) -> JSOwnedUtf16String {
    // SAFETY: See the module documentation.
    owned_utf16_string_into_abi(unsafe { regexp_from_abi(regexp) }.flags())
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{cell_of_payload, owned_utf16_string_from_abi, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::JS_COMPLETION_THROW;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    fn string(vm: &Vm, text: &str) -> JSValue {
        Value::from_string(PrimitiveString::create_from_utf8(vm, text)).0
    }

    #[test]
    fn regexps_round_trip_their_source_and_flags() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        // SAFETY: The VM is live, and the payload of a normal completion is the RegExp.
        let regexp = unsafe {
            cell_of_payload::<JSObject>(js_regexp_create(abi_vm, string(&vm, "a+(b)\u{3b1}"), string(&vm, "gu")))
        };
        realm.global_object().define_direct_property(
            &vm,
            &key("regexp"),
            Value::from_object(regexp),
            DEFAULT_ATTRIBUTES,
        );
        assert_eq!(
            utf8(
                run_script(
                    &vm,
                    realm,
                    "regexp.exec('xaab\u{3b1}')[1] + regexp.lastIndex + regexp.flags"
                )
                .must()
            ),
            "b5gu"
        );

        let abi_regexp = crate::embedding::abi_types::object_into_abi(regexp);
        // SAFETY: The RegExp is live, and the owned strings are adopted once.
        let (pattern, flags) = unsafe {
            (
                owned_utf16_string_from_abi(js_regexp_pattern(abi_regexp)),
                owned_utf16_string_from_abi(js_regexp_flags(abi_regexp)),
            )
        };
        assert_eq!(Utf16View::of_string(&pattern).to_utf8(), "a+(b)\u{3b1}");
        assert_eq!(Utf16View::of_string(&flags).to_utf8(), "gu");

        // SAFETY: The VM is live.
        let completion = unsafe { js_regexp_create(abi_vm, string(&vm, "("), Value::UNDEFINED.0) };
        assert!(completion.variant == JS_COMPLETION_THROW);
        assert_eq!(utf8(Value(completion.payload)), "[object SyntaxError]");
        // SAFETY: As above.
        let completion = unsafe { js_regexp_create(abi_vm, string(&vm, "a"), string(&vm, "gg")) };
        assert!(completion.variant == JS_COMPLETION_THROW);
    }
}
