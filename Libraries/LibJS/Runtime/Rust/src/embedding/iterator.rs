/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Iterators and iterator result objects.
//!
//! The functions here follow the contract object.rs states for the embedding module. The operations on an Iterator
//! Record call the iterator's methods, which may run any JavaScript.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use crate::embedding::abi_types::{
    CellAbi, JSRealm, append_to_value_sink, cell_from_abi, cell_into_abi, completion_from_abi, completion_into_abi,
    object_into_abi, vm_from_abi,
};
use crate::embedding::object::function_from_abi;
use crate::layout::host_class::{JSCompletion, JSObject, JSVM, JSValue, JSValueSink};
use crate::layout::value::Value;
use crate::runtime::completion::{Completion, ThrowCompletionOr};
use crate::runtime::iterator::{
    IterationResult, IteratorHint, IteratorRecord, create_iterator_result_object, get_iterator,
    get_iterator_from_method, iterator_close, iterator_complete, iterator_next, iterator_step, iterator_step_value,
    iterator_to_list, iterator_value,
};

/// An Iterator Record { [[Iterator]], [[NextMethod]], [[Done]] }, a cell that the operations on it update in place.
pub struct JSIteratorRecord {
    _opaque: [u8; 0],
}

impl CellAbi for JSIteratorRecord {
    type Cell = IteratorRecord;
}

/// The kind of iterator GetIterator gets, in the order of the C++ JS::IteratorHint.
pub type JSIteratorHint = u8;

pub const JS_ITERATOR_HINT_SYNC: JSIteratorHint = 0;
pub const JS_ITERATOR_HINT_ASYNC: JSIteratorHint = 1;

fn iterator_hint_from_abi(hint: JSIteratorHint) -> IteratorHint {
    match hint {
        JS_ITERATOR_HINT_SYNC => IteratorHint::Sync,
        JS_ITERATOR_HINT_ASYNC => IteratorHint::Async,
        hint => panic!("the embedder passed an unknown iterator hint {hint}"),
    }
}

/// A normal completion whose payload is whether the step produced something, which goes to `value`, or a throw
/// completion.
///
/// # Safety
///
/// `value` must be writable.
unsafe fn step_completion_into_abi(step: ThrowCompletionOr<Option<Value>>, value: *mut JSValue) -> JSCompletion {
    completion_into_abi(step.map(|step| {
        let Some(stepped_value) = step else {
            return false;
        };
        assert!(!value.is_null(), "the embedder passes an out parameter");
        // SAFETY: The caller guarantees that `value` is writable.
        unsafe { value.write(stepped_value.0) };
        true
    }))
}

/// GetIterator ( obj, kind ) with kind one of JS_ITERATOR_HINT_*, whose payload is the JSIteratorRecord. An async
/// iterator of an object without @@asyncIterator wraps its sync iterator, as CreateAsyncFromSyncIterator does. Returns
/// an unrooted record. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_get(vm: *mut JSVM, value: JSValue, hint: JSIteratorHint) -> JSCompletion {
    // SAFETY: See the module documentation.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(
        get_iterator(vm, Value(value), iterator_hint_from_abi(hint)).map(cell_into_abi::<JSIteratorRecord>),
    )
}

/// GetIteratorFromMethod ( obj, method ), whose payload is the JSIteratorRecord. Returns an unrooted record. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_get_from_method(
    vm: *mut JSVM,
    value: JSValue,
    method: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, method) = unsafe { (vm_from_abi(vm), function_from_abi(method)) };
    completion_into_abi(get_iterator_from_method(vm, Value(value), method).map(cell_into_abi::<JSIteratorRecord>))
}

/// [[Iterator]] of the record. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_record_iterator(record: *mut JSIteratorRecord) -> *mut JSObject {
    // SAFETY: See the module documentation.
    object_into_abi(unsafe { cell_from_abi::<JSIteratorRecord>(record) }.iterator())
}

/// [[NextMethod]] of the record. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_record_next_method(record: *mut JSIteratorRecord) -> JSValue {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSIteratorRecord>(record) }.next_method().0
}

/// [[Done]] of the record. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_record_done(record: *mut JSIteratorRecord) -> bool {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSIteratorRecord>(record) }.done()
}

/// IteratorNext ( iteratorRecord [ , value ] ), with `value` null for none, whose payload is the iterator result
/// object. A throw, or a result that is not an object, marks the record done. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_next(
    vm: *mut JSVM,
    record: *mut JSIteratorRecord,
    value: *const JSValue,
) -> JSCompletion {
    // SAFETY: See the module documentation; `value` is null or readable.
    let (vm, record, value) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSIteratorRecord>(record),
            value.as_ref().map(|value| Value(*value)),
        )
    };
    completion_into_abi(iterator_next(vm, &record, value))
}

/// IteratorComplete ( iteratorResult ), whose payload is the bool of its "done". Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_complete(vm: *mut JSVM, iterator_result: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, iterator_result) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(iterator_result)) };
    completion_into_abi(iterator_complete(vm, iterator_result))
}

/// IteratorValue ( iteratorResult ), whose payload is its "value". Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_value(vm: *mut JSVM, iterator_result: *mut JSObject) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, iterator_result) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(iterator_result)) };
    completion_into_abi(iterator_value(vm, iterator_result))
}

/// IteratorStep ( iteratorRecord ), whose payload is false once the iterator is done. Otherwise it is true, and
/// `value` is the value of the step of a builtin iterator, which steps without a result object, and undefined for any
/// other, as in the C++ IterationResult. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_step(
    vm: *mut JSVM,
    record: *mut JSIteratorRecord,
    value: *mut JSValue,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, record) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSIteratorRecord>(record)) };
    let step = iterator_step(vm, &record).map(|result| match result {
        IterationResult::Done => None,
        IterationResult::Value(value) => Some(value),
    });
    // SAFETY: As above, `value` is writable.
    unsafe { step_completion_into_abi(step, value) }
}

/// IteratorStepValue ( iteratorRecord ), whose payload is false once the iterator is done, and true when it produced
/// a value, which goes to `value`. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_step_value(
    vm: *mut JSVM,
    record: *mut JSIteratorRecord,
    value: *mut JSValue,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, record) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSIteratorRecord>(record)) };
    // SAFETY: As above, `value` is writable.
    unsafe { step_completion_into_abi(iterator_step_value(vm, &record), value) }
}

/// IteratorClose ( iteratorRecord, completion ): calls the iterator's "return" method, if it has one, and returns the
/// completion, unless it is normal and "return" throws or returns something other than an object. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_close(
    vm: *mut JSVM,
    record: *mut JSIteratorRecord,
    completion: JSCompletion,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, record) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSIteratorRecord>(record)) };
    let completion = Completion::from(completion_from_abi(completion));
    completion_into_abi(iterator_close(vm, &record, completion).into_throw_completion_or())
}

/// IteratorToList ( iteratorRecord ): steps the iterator to its end and then appends every value it produced to the
/// sink, in order, or appends nothing if a step throws. The values stay alive while the sink takes them. Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_to_list(
    vm: *mut JSVM,
    record: *mut JSIteratorRecord,
    values: *const JSValueSink,
) -> JSCompletion {
    // SAFETY: See the module documentation; the sink is readable.
    let (vm, record, sink) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSIteratorRecord>(record),
            values.as_ref().expect("the embedder passes a sink"),
        )
    };
    completion_into_abi(iterator_to_list(vm, &record).map(|list| {
        for index in 0..list.len() {
            let value = list.get(index).expect("the index is within the list");
            append_to_value_sink(sink, value);
        }
    }))
}

/// CreateIteratorResultObject ( value, done ), an object of the realm's %Object.prototype%. Returns an unrooted object.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_iterator_create_result_object(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    value: JSValue,
    done: bool,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSRealm>(realm)) };
    object_into_abi(create_iterator_result_object(vm, realm, Value(value), done))
}
