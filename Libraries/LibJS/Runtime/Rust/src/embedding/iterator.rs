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

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::ffi::c_void;

    use super::*;
    use crate::embedding::abi_types::{bool_of_payload, cell_of_payload, throw_completion_into_abi, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::layout::cell::Gc;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::layout::realm::Realm;
    use crate::runtime::completion::{Must, Throw};
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::utilities::initialize_realm;

    fn record_of(completion: JSCompletion) -> *mut JSIteratorRecord {
        // SAFETY: The tests only pass completions whose payload is a record.
        cell_into_abi(unsafe { cell_of_payload::<JSIteratorRecord>(completion) })
    }

    fn define_global(vm: &Vm, realm: Gc<Realm>, name: &str, value: Value) {
        realm
            .global_object()
            .define_direct_property(vm, &key(name), value, DEFAULT_ATTRIBUTES);
    }

    const GENERATOR: &str = r#"
        var log = [];
        function* generator() {
            try {
                const sent = yield 1;
                log.push("sent " + sent);
                yield 2;
                yield 3;
            } finally {
                log.push("closed");
            }
        }
    "#;

    #[test]
    fn records_step_next_and_close_iterators() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        run_script(&vm, realm, GENERATOR).must();
        let generator = run_script(&vm, realm, "generator()").must();

        // SAFETY: The VM and the cells are live, and out parameters are locals.
        unsafe {
            let record = record_of(js_iterator_get(abi_vm, generator.0, JS_ITERATOR_HINT_SYNC));
            assert!(object_into_abi(generator.as_object()) == js_iterator_record_iterator(record));
            assert!(Value(js_iterator_record_next_method(record)).is_function());
            assert!(!js_iterator_record_done(record));

            let mut value = 0;
            assert!(bool_of_payload(js_iterator_step_value(abi_vm, record, &raw mut value)));
            assert!(value == Value::from_i32(1).0);

            let sent = Value::from_i32(42).0;
            let result = js_iterator_next(abi_vm, record, &raw const sent);
            let result = object_into_abi(cell_of_payload::<JSObject>(result));
            assert!(!bool_of_payload(js_iterator_complete(abi_vm, result)));
            let next_value = js_iterator_value(abi_vm, result);
            assert!(next_value.variant == JS_COMPLETION_NORMAL && next_value.payload == Value::from_i32(2).0);

            // Closing with a throw completion runs the generator's finally block and passes the throw on.
            let thrown = throw_completion_into_abi(Throw::new(Value::from_i32(7)));
            let closed = js_iterator_close(abi_vm, record, thrown);
            assert!(closed.variant == JS_COMPLETION_THROW && closed.payload == Value::from_i32(7).0);
            assert_eq!(utf8(run_script(&vm, realm, "log.join()").must()), "sent 42,closed");

            // Stepping a closed generator finds it done, which marks the record done.
            assert!(!bool_of_payload(js_iterator_step(abi_vm, record, &raw mut value)));
            assert!(js_iterator_record_done(record));
        }
    }

    #[test]
    fn closing_with_a_normal_completion_reports_a_bad_return() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let iterable = run_script(
            &vm,
            realm,
            "({ [Symbol.iterator]() { return { next() { return { done: false, value: 1 }; }, return() { return 1; } }; } })",
        )
        .must();
        // SAFETY: The VM and cells are live.
        unsafe {
            let record = record_of(js_iterator_get(abi_vm, iterable.0, JS_ITERATOR_HINT_SYNC));
            let normal = completion_into_abi(Ok(Value::from_i32(5)));
            let closed = js_iterator_close(abi_vm, record, normal);
            assert!(closed.variant == JS_COMPLETION_THROW);
            assert_eq!(utf8(Value(closed.payload)), "[object TypeError]");

            // With a throw completion, the result of "return" does not matter.
            let thrown = throw_completion_into_abi(Throw::new(Value::NULL));
            let closed = js_iterator_close(abi_vm, record, thrown);
            assert!(closed.variant == JS_COMPLETION_THROW && closed.payload == Value::NULL.0);

            // The step of a builtin iterator produces the value without a result object, even for IteratorStep.
            let array = run_script(&vm, realm, "[8, 9]").must();
            let record = record_of(js_iterator_get(abi_vm, array.0, JS_ITERATOR_HINT_SYNC));
            let mut value = 0;
            assert!(bool_of_payload(js_iterator_step(abi_vm, record, &raw mut value)));
            assert!(value == Value::from_i32(8).0);
        }
    }

    struct ReentrantSink<'a> {
        vm: &'a Vm,
        realm: Gc<Realm>,
        values: Vec<i32>,
    }

    /// Takes each value, then runs a script and collects garbage while the VM waits for it.
    unsafe extern "C" fn append_and_reenter(context: *mut c_void, value: JSValue) {
        // SAFETY: The tests pass a ReentrantSink as the context.
        let sink = unsafe { &mut *context.cast::<ReentrantSink<'_>>() };
        sink.values.push(Value(value).as_i32());
        run_script(sink.vm, sink.realm, "log.push('sink'); [1, 2, 3].map(x => ({ x }));").must();
        sink.vm.heap().collect_garbage();
    }

    #[test]
    fn lists_of_iterated_values_reach_a_reentrant_sink() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        run_script(&vm, realm, GENERATOR).must();
        let mut sink = ReentrantSink {
            vm: &vm,
            realm,
            values: Vec::new(),
        };
        let value_sink = JSValueSink {
            context: (&raw mut sink).cast(),
            append: Some(append_and_reenter),
        };
        // SAFETY: The VM and cells are live, and the sink outlives the calls.
        unsafe {
            let method = run_script(&vm, realm, "generator").must();
            let record = record_of(js_iterator_get_from_method(
                abi_vm,
                Value::UNDEFINED.0,
                object_into_abi(method.as_object()),
            ));
            let completion = js_iterator_to_list(abi_vm, record, &raw const value_sink);
            assert!(completion.variant == JS_COMPLETION_NORMAL && completion.payload == 0);
            assert_eq!(sink.values, [1, 2, 3]);
            assert_eq!(
                utf8(run_script(&vm, realm, "log.join()").must()),
                "sent undefined,closed,sink,sink,sink"
            );

            // A step that throws appends nothing.
            sink.values.clear();
            let throwing = run_script(&vm, realm, "(function* () { yield 1; throw 'midway'; })()").must();
            let record = record_of(js_iterator_get(abi_vm, throwing.0, JS_ITERATOR_HINT_SYNC));
            let completion = js_iterator_to_list(abi_vm, record, &raw const value_sink);
            assert!(completion.variant == JS_COMPLETION_THROW);
            assert_eq!(utf8(Value(completion.payload)), "midway");
            assert!(sink.values.is_empty());
        }
    }

    #[test]
    fn async_records_wrap_sync_iterators_and_result_objects_are_plain() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let array = run_script(&vm, realm, "var log = []; [5]").must();
        // SAFETY: The VM and cells are live.
        unsafe {
            let record = record_of(js_iterator_get(abi_vm, array.0, JS_ITERATOR_HINT_ASYNC));
            let result = js_iterator_next(abi_vm, record, core::ptr::null());
            define_global(
                &vm,
                realm,
                "promise",
                Value::from_object(cell_of_payload::<JSObject>(result)),
            );
            run_script(
                &vm,
                realm,
                "promise.then(result => log.push(result.value, result.done));",
            )
            .must();
            vm.run_queued_promise_jobs();
            assert_eq!(utf8(run_script(&vm, realm, "log.join()").must()), "5,false");

            let iterator_result =
                js_iterator_create_result_object(abi_vm, cell_into_abi::<JSRealm>(realm), Value::TRUE.0, true);
            define_global(
                &vm,
                realm,
                "result",
                Value::from_object(cell_from_abi::<JSObject>(iterator_result)),
            );
            assert_eq!(
                utf8(
                    run_script(
                        &vm,
                        realm,
                        "Object.getPrototypeOf(result) === Object.prototype && JSON.stringify(result)"
                    )
                    .must()
                ),
                "{\"value\":true,\"done\":true}"
            );
            assert!(bool_of_payload(js_iterator_complete(abi_vm, iterator_result)));
        }
        let not_iterable = Value::from_i32(1);
        // SAFETY: The VM is live.
        assert_eq!(
            unsafe { js_iterator_get(abi_vm, not_iterable.0, JS_ITERATOR_HINT_SYNC) }.variant,
            JS_COMPLETION_THROW
        );
    }
}
