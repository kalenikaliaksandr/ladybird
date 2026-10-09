/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Throwing errors of the kinds the runtime creates.

use ak::Utf16String;

use crate::abi::{JS_ERROR_KIND_RANGE_ERROR, JS_ERROR_KIND_TYPE_ERROR, JSErrorKind, js_error_throw_with_owned_message};
use crate::completion::{Thrown, payload_from_abi};
use crate::vm::Vm;

/// Creates an error of the kind in the current realm, or a TypeError in the realm of a TypeErrorRealmScope, and
/// returns the throw of it.
pub fn throw(vm: Vm, kind: JSErrorKind, message: &str) -> Thrown {
    let message = Utf16String::from_utf8(message);
    // SAFETY: The VM runs code, and the message gives up its reference to the error.
    let completion = unsafe { js_error_throw_with_owned_message(vm.as_raw(), kind, message.into_raw()) };
    match payload_from_abi(completion) {
        Err(thrown) => thrown,
        Ok(_) => unreachable!("throwing an error completes abruptly"),
    }
}

pub fn throw_type_error(vm: Vm, message: &str) -> Thrown {
    throw(vm, JS_ERROR_KIND_TYPE_ERROR, message)
}

pub fn throw_range_error(vm: Vm, message: &str) -> Thrown {
    throw(vm, JS_ERROR_KIND_RANGE_ERROR, message)
}
