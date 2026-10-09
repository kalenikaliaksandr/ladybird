/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The result of an operation that can throw, and its JSCompletion form in the ABI.

use crate::abi::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW, JSCompletion};
use crate::value::Value;

/// A thrown value, which lives on the stack like any value.
#[derive(Clone, Copy)]
pub struct Thrown(pub Value);

pub type ThrowCompletionOr<T> = Result<T, Thrown>;

/// The JSCompletion that a native function returns for its result.
pub fn into_abi(result: ThrowCompletionOr<Value>) -> JSCompletion {
    match result {
        Ok(value) => JSCompletion {
            payload: value.raw(),
            variant: JS_COMPLETION_NORMAL,
        },
        Err(Thrown(value)) => JSCompletion {
            payload: value.raw(),
            variant: JS_COMPLETION_THROW,
        },
    }
}

/// The payload of a completion, or the value it throws.
pub(crate) fn payload_from_abi(completion: JSCompletion) -> ThrowCompletionOr<u64> {
    if completion.variant == JS_COMPLETION_THROW {
        return Err(Thrown(Value::from_raw(completion.payload)));
    }
    debug_assert!(completion.variant == JS_COMPLETION_NORMAL);
    Ok(completion.payload)
}

pub(crate) fn value_from_abi(completion: JSCompletion) -> ThrowCompletionOr<Value> {
    payload_from_abi(completion).map(Value::from_raw)
}

pub(crate) fn bool_from_abi(completion: JSCompletion) -> ThrowCompletionOr<bool> {
    payload_from_abi(completion).map(|payload| payload != 0)
}

pub(crate) fn pointer_from_abi<T>(completion: JSCompletion) -> ThrowCompletionOr<*mut T> {
    payload_from_abi(completion).map(|payload| payload as usize as *mut T)
}
