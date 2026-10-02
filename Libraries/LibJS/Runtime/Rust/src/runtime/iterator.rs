/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Iterator records, the Iterator object and the iterator abstract operations of Libraries/LibJS/Runtime/Iterator.cpp.

use core::cell::Cell;
use core::ops::Deref;

use libjs_runtime_macros::Trace;

use crate::bytecode::executable::StaticPropertyLookupCacheSite;
use crate::gc::class::{GcCell, define_cell};
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::{call, call_function_object};
use crate::runtime::async_from_sync_iterator_prototype::create_async_from_sync_iterator;
use crate::runtime::completion::{Completion, ThrowCompletionOr};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::object::{MayInterfereWithIndexedPropertyAccess, ShouldThrowExceptions, allocate_object};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;

pub use libjs_abi::IteratorHint;

/// The hint of a GetIterator instruction, as the bytecode encodes it.
pub fn iterator_hint_from_bytecode(encoded: u32) -> IteratorHint {
    match encoded {
        0 => IteratorHint::Sync,
        1 => IteratorHint::Async,
        _ => unreachable!("the bytecode encodes an iterator hint"),
    }
}

/// The fields of an Iterator Record. They are cells, so that the records slow paths keep in locals and the ones
/// IteratorRecord cells hold go through the same operations, as the C++ IteratorRecord derives from IteratorRecordImpl.
#[repr(C)]
#[derive(Trace)]
pub struct IteratorRecordImpl {
    done: Cell<bool>,                   // [[Done]]
    iterator: Cell<Option<Gc<Object>>>, // [[Iterator]]
    next_method: Cell<Value>,           // [[NextMethod]]
}

impl IteratorRecordImpl {
    pub fn new(iterator: Option<Gc<Object>>, next_method: Value, done: bool) -> Self {
        Self {
            done: Cell::new(done),
            iterator: Cell::new(iterator),
            next_method: Cell::new(next_method),
        }
    }

    pub fn done(&self) -> bool {
        self.done.get()
    }

    pub fn set_done(&self, done: bool) {
        self.done.set(done);
    }

    pub fn iterator(&self) -> Gc<Object> {
        self.iterator.get().expect("the iterator record has an iterator")
    }

    pub fn next_method(&self) -> Value {
        self.next_method.get()
    }
}

// 7.4.1 Iterator Records, https://tc39.es/ecma262/#sec-iterator-records
#[repr(C)]
#[derive(Trace)]
pub struct IteratorRecord {
    header: CellHeader,
    record: IteratorRecordImpl,
}

define_cell!(IteratorRecord, Other);

impl Deref for IteratorRecord {
    type Target = IteratorRecordImpl;

    fn deref(&self) -> &IteratorRecordImpl {
        &self.record
    }
}

impl IteratorRecord {
    pub fn create(vm: &Vm, iterator: Option<Gc<Object>>, next_method: Value, done: bool) -> Gc<IteratorRecord> {
        vm.heap().allocate(IteratorRecord {
            header: CellHeader::for_class(Self::CLASS),
            record: IteratorRecordImpl::new(iterator, next_method, done),
        })
    }

    fn create_from_impl(vm: &Vm, iterator_record: &IteratorRecordImpl) -> Gc<IteratorRecord> {
        Self::create(
            vm,
            iterator_record.iterator.get(),
            iterator_record.next_method(),
            iterator_record.done(),
        )
    }
}

#[repr(C)]
#[derive(Trace)]
pub struct Iterator {
    base: Object,
    iterated: Cell<Gc<IteratorRecord>>, // [[Iterated]]
}

define_cell!(Iterator, Object, extends: [Object]);

impl Deref for Iterator {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl Iterator {
    pub fn create(vm: &Vm, prototype: Gc<Object>, iterated: Gc<IteratorRecord>) -> Gc<Iterator> {
        allocate_object(
            vm,
            Iterator {
                base: Object::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No),
                iterated: Cell::new(iterated),
            },
        )
    }

    /// Iterator(Object& prototype), whose [[Iterated]] is a record without an iterator.
    pub fn create_without_iterated(vm: &Vm, prototype: Gc<Object>) -> Gc<Iterator> {
        allocate_object(vm, Self::new_without_iterated(vm, prototype))
    }

    /// Iterator(Object& prototype), not yet allocated, for the OrdinaryCreateFromConstructor of the Iterator constructor.
    pub fn new_without_iterated(vm: &Vm, prototype: Gc<Object>) -> Iterator {
        let iterated = IteratorRecord::create(vm, None, Value::UNDEFINED, false);
        Iterator {
            base: Object::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No),
            iterated: Cell::new(iterated),
        }
    }

    pub fn iterated(&self) -> Gc<IteratorRecord> {
        self.iterated.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveHandling {
    IterateStringPrimitives,
    RejectPrimitives,
}

/// The C++ BuiltinIterator interface: the step of an iterator the runtime takes without calling its next method,
/// given the iterator. It sets `done`, or the value the step produces.
pub type BuiltinIteratorNext = fn(&Object, &Vm, &mut bool, &mut Value) -> ThrowCompletionOr<()>;

/// What IteratorStep produces: DONE, or a value.
#[derive(Clone, Copy, Debug)]
pub enum IterationResult {
    Done,
    Value(Value),
}

// 7.4.13 IfAbruptCloseIterator ( value, iteratorRecord ), https://tc39.es/ecma262/#sec-ifabruptcloseiterator
macro_rules! try_or_close_iterator {
    ($vm:expr, $iterator_record:expr, $expression:expr) => {
        // 1. Assert: value is a Completion Record.
        match $expression {
            // 2. If value is an abrupt completion, return ? IteratorClose(iteratorRecord, value).
            Err(throw) => {
                return Err(
                    $crate::runtime::iterator::iterator_close($vm, $iterator_record, throw.into()).release_error(),
                );
            }
            // 3. Else, set value to ! value.
            Ok(value) => value,
        }
    };
}
pub(crate) use try_or_close_iterator;

// 4 IfAbruptCloseIterators ( value, iteratorRecords ), https://tc39.es/proposal-joint-iteration/#sec-ifabruptcloseiterators
macro_rules! try_or_close_iterators {
    ($vm:expr, $iterator_records:expr, $expression:expr) => {
        // 1. Assert: value is a Completion Record.
        match $expression {
            // 2. If value is an abrupt completion, return ? IteratorCloseAll(iteratorRecords, value).
            Err(throw) => {
                return Err(
                    $crate::runtime::iterator::iterator_close_all($vm, $iterator_records, throw.into()).release_error(),
                );
            }
            // 3. Else, set value to value.[[Value]].
            Ok(value) => value,
        }
    };
}
pub(crate) use try_or_close_iterators;

// 7.4.2 GetIteratorDirect ( obj ), https://tc39.es/ecma262/#sec-getiteratordirect
pub fn get_iterator_direct(vm: &Vm, object: Gc<Object>) -> ThrowCompletionOr<Gc<IteratorRecord>> {
    // 1. Let nextMethod be ? Get(obj, "next").
    let next_method = object.get_with_cache(
        vm,
        &vm.names.next,
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::GetIteratorDirectNext),
    )?;

    // 2. Let iteratorRecord be Record { [[Iterator]]: obj, [[NextMethod]]: nextMethod, [[Done]]: false }.
    // 3. Return iteratorRecord.
    Ok(IteratorRecord::create(vm, Some(object), next_method, false))
}

// 7.4.3 GetIteratorFromMethod ( obj, method ), https://tc39.es/ecma262/#sec-getiteratorfrommethod
pub fn get_iterator_from_method_impl(
    vm: &Vm,
    object: Value,
    method: Gc<FunctionObject>,
) -> ThrowCompletionOr<IteratorRecordImpl> {
    // 1. Let iterator be ? Call(method, obj).
    let iterator = call_function_object(vm, method, object, &[])?;

    // 2. If iterator is not an Object, throw a TypeError exception.
    if !iterator.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotIterable, &[&object]);
    }

    // 3. Let nextMethod be ? Get(iterator, "next").
    let next_method = iterator.get_with_cache(
        vm,
        &vm.names.next,
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::GetIteratorFromMethodNext),
    )?;

    // 4. Let iteratorRecord be the Iterator Record { [[Iterator]]: iterator, [[NextMethod]]: nextMethod, [[Done]]: false }.
    let iterator_record = IteratorRecordImpl::new(Some(iterator.as_object()), next_method, false);

    // 5. Return iteratorRecord.
    Ok(iterator_record)
}

pub fn get_iterator_from_method(
    vm: &Vm,
    object: Value,
    method: Gc<FunctionObject>,
) -> ThrowCompletionOr<Gc<IteratorRecord>> {
    let iterator_record_impl = get_iterator_from_method_impl(vm, object, method)?;
    Ok(IteratorRecord::create_from_impl(vm, &iterator_record_impl))
}

// 7.4.4 GetIterator ( obj, kind ), https://tc39.es/ecma262/#sec-getiterator
pub fn get_iterator_impl(vm: &Vm, object: Value, kind: IteratorHint) -> ThrowCompletionOr<IteratorRecordImpl> {
    let method;
    let iterator_symbol = PropertyKey::from(vm.well_known_symbols().iterator);

    // 1. If kind is async, then
    if kind == IteratorHint::Async {
        // a. Let method be ? GetMethod(obj, @@asyncIterator).
        method = object.get_method(vm, &PropertyKey::from(vm.well_known_symbols().async_iterator))?;

        // b. If method is undefined, then
        if method.is_none() {
            // i. Let syncMethod be ? GetMethod(obj, @@iterator).
            let sync_method = object.get_method_with_cache(
                vm,
                &iterator_symbol,
                vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::GetIteratorSyncMethod),
            )?;

            // ii. If syncMethod is undefined, throw a TypeError exception.
            let Some(sync_method) = sync_method else {
                return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotIterable, &[&object]);
            };

            // iii. Let syncIteratorRecord be ? GetIteratorFromMethod(obj, syncMethod).
            let sync_iterator_record = get_iterator_from_method(vm, object, sync_method)?;

            // iv. Return CreateAsyncFromSyncIterator(syncIteratorRecord).
            return Ok(create_async_from_sync_iterator(vm, sync_iterator_record));
        }
    }
    // 2. Else,
    else {
        // a. Let method be ? GetMethod(obj, @@iterator).
        method = object.get_method_with_cache(
            vm,
            &iterator_symbol,
            vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::GetIteratorMethod),
        )?;
    }

    // 3. If method is undefined, throw a TypeError exception.
    let Some(method) = method else {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotIterable, &[&object]);
    };

    // 4. Return ? GetIteratorFromMethod(obj, method).
    get_iterator_from_method_impl(vm, object, method)
}

pub fn get_iterator(vm: &Vm, object: Value, kind: IteratorHint) -> ThrowCompletionOr<Gc<IteratorRecord>> {
    let iterator_record_impl = get_iterator_impl(vm, object, kind)?;
    Ok(IteratorRecord::create_from_impl(vm, &iterator_record_impl))
}

// 7.4.5 GetIteratorFlattenable ( obj, primitiveHandling ), https://tc39.es/ecma262/#sec-getiteratorflattenable
pub fn get_iterator_flattenable(
    vm: &Vm,
    object: Value,
    primitive_handling: PrimitiveHandling,
) -> ThrowCompletionOr<Gc<IteratorRecord>> {
    // 1. If obj is not an Object, then
    if !object.is_object() {
        // a. If primitiveHandling is reject-primitives, throw a TypeError exception.
        if primitive_handling == PrimitiveHandling::RejectPrimitives {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&object]);
        }

        // b. Assert: primitiveHandling is iterate-string-primitives.
        debug_assert!(primitive_handling == PrimitiveHandling::IterateStringPrimitives);

        // c. If obj is not a String, throw a TypeError exception.
        if !object.is_string() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAString, &[&object]);
        }
    }

    // 2. Let method be ? GetMethod(obj, %Symbol.iterator%).
    let method = object.get_method_with_cache(
        vm,
        &PropertyKey::from(vm.well_known_symbols().iterator),
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::GetIteratorFlattenableMethod),
    )?;

    // 3. If method is undefined, then
    let iterator = match method {
        // a. Let iterator be obj.
        None => object,
        // 4. Else,
        // a. Let iterator be ? Call(method, obj).
        Some(method) => call_function_object(vm, method, object, &[])?,
    };

    // 5. If iterator is not an Object, throw a TypeError exception.
    if !iterator.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&iterator]);
    }

    // 6. Return ? GetIteratorDirect(iterator).
    get_iterator_direct(vm, iterator.as_object())
}

// 7.4.6 IteratorNext ( iteratorRecord [ , value ] ), https://tc39.es/ecma262/#sec-iteratornext
pub fn iterator_next(
    vm: &Vm,
    iterator_record: &IteratorRecordImpl,
    value: Option<Value>,
) -> ThrowCompletionOr<Gc<Object>> {
    let iterator = Value::from_object(iterator_record.iterator());
    let result = match value {
        // 1. If value is not present, then
        // a. Let result be Completion(Call(iteratorRecord.[[NextMethod]], iteratorRecord.[[Iterator]])).
        None => call(vm, iterator_record.next_method(), iterator, &[]),
        // 2. Else,
        // a. Let result be Completion(Call(iteratorRecord.[[NextMethod]], iteratorRecord.[[Iterator]], « value »)).
        Some(value) => call(vm, iterator_record.next_method(), iterator, &[value]),
    };

    // 3. If result is a throw completion, then
    let result_value = match result {
        Err(throw) => {
            // a. Set iteratorRecord.[[Done]] to true.
            iterator_record.set_done(true);

            // b. Return ? result.
            return Err(throw);
        }
        // 4. Set result to ! result.
        Ok(result_value) => result_value,
    };

    // 5. If result is not an Object, then
    if !result_value.is_object() {
        // a. Set iteratorRecord.[[Done]] to true.
        iterator_record.set_done(true);

        // b. Throw a TypeError exception.
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::IterableNextBadReturn, &[]);
    }

    // 6. Return result.
    Ok(result_value.as_object())
}

// 7.4.7 IteratorComplete ( iteratorResult ), https://tc39.es/ecma262/#sec-iteratorcomplete
pub fn iterator_complete(vm: &Vm, iterator_result: Gc<Object>) -> ThrowCompletionOr<bool> {
    // 1. Return ToBoolean(? Get(iterResult, "done")).
    Ok(iterator_result
        .get_with_cache(
            vm,
            &vm.names.done,
            vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::IteratorCompleteDone),
        )?
        .to_boolean())
}

// 7.4.8 IteratorValue ( iteratorResult ), https://tc39.es/ecma262/#sec-iteratorvalue
pub fn iterator_value(vm: &Vm, iterator_result: Gc<Object>) -> ThrowCompletionOr<Value> {
    // 1. Return ? Get(iterResult, "value").
    iterator_result.get_with_cache(
        vm,
        &vm.names.value,
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::IteratorValueValue),
    )
}

// NB: We must not retrieve the iterated value in an observable manner when IteratorStep is invoked. In the slow path,
//     we have to access the value via get(vm.names.value), so we must take care to avoid this.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WithValue {
    No,
    Yes,
}

fn iterator_step_impl(
    vm: &Vm,
    iterator_record: &IteratorRecordImpl,
    with_value: WithValue,
) -> ThrowCompletionOr<IterationResult> {
    // OPTIMIZATION: Calling next can be skipped only if it would enter the current realm.
    let next_method = iterator_record.next_method();
    if next_method.is_function() && next_method.as_function().realm() == vm.current_realm() {
        let iterator = iterator_record.iterator();
        if let Some(builtin_iterator_next) = iterator.as_builtin_iterator_if_next_is_not_redefined(next_method) {
            let mut value = Value::UNDEFINED;
            let mut done = false;
            builtin_iterator_next(&iterator, vm, &mut done, &mut value)?;

            if done {
                iterator_record.set_done(true);
                return Ok(IterationResult::Done);
            }

            return Ok(IterationResult::Value(value));
        }
    }

    // 7.4.9 IteratorStep ( iteratorRecord ), https://tc39.es/ecma262/#sec-iteratorstep
    // 1. Let result be ? IteratorNext(iteratorRecord).
    let result = iterator_next(vm, iterator_record, None)?;

    // 2. Let done be Completion(IteratorComplete(result)).
    let done = iterator_complete(vm, result);

    // 3. If done is a throw completion, then
    let done = match done {
        Err(throw) => {
            // a. Set iteratorRecord.[[Done]] to true.
            iterator_record.set_done(true);

            // b. Return ? done.
            return Err(throw);
        }
        // 4. Set done to ! done.
        Ok(done) => done,
    };

    // 5. If done is true, then
    if done {
        // a. Set iteratorRecord.[[Done]] to true.
        iterator_record.set_done(true);

        // b. Return DONE.
        return Ok(IterationResult::Done);
    }

    if with_value == WithValue::No {
        // 6. Return result.
        // NB: We use undefined as a meaningless sentinel value when the iterated value is not required.
        return Ok(IterationResult::Value(Value::UNDEFINED));
    }

    // 7.4.10 IteratorStepValue ( iteratorRecord ), https://tc39.es/ecma262/#sec-iteratorstepvalue
    // 3. Let value be Completion(IteratorValue(result)).
    let value = iterator_value(vm, result);

    // 4. If value is a throw completion, then
    if value.is_err() {
        // a. Set iteratorRecord.[[Done]] to true.
        iterator_record.set_done(true);
    }

    // 5. Return ? value.
    Ok(IterationResult::Value(value?))
}

// 7.4.9 IteratorStep ( iteratorRecord ), https://tc39.es/ecma262/#sec-iteratorstep
pub fn iterator_step(vm: &Vm, iterator_record: &IteratorRecordImpl) -> ThrowCompletionOr<IterationResult> {
    iterator_step_impl(vm, iterator_record, WithValue::No)
}

// 7.4.10 IteratorStepValue ( iteratorRecord ), https://tc39.es/ecma262/#sec-iteratorstepvalue
pub fn iterator_step_value(vm: &Vm, iterator_record: &IteratorRecordImpl) -> ThrowCompletionOr<Option<Value>> {
    let result = iterator_step_impl(vm, iterator_record, WithValue::Yes)?;

    Ok(match result {
        IterationResult::Done => None,
        IterationResult::Value(value) => Some(value),
    })
}

// 7.4.11 IteratorClose ( iteratorRecord, completion , https://tc39.es/ecma262/#sec-iteratorclose
// 7.4.13 AsyncIteratorClose ( iteratorRecord, completion ), https://tc39.es/ecma262/#sec-asynciteratorclose
// NOTE: These only differ in that async awaits the inner value after the call.
// NB: AsyncIteratorClose is not implemented here. It is inlined as bytecode
//     by the codegen, using the Await bytecode op to yield naturally instead of
//     spinning the event loop synchronously.
fn iterator_close_impl(vm: &Vm, iterator_record: &IteratorRecordImpl, completion: Completion) -> Completion {
    // 1. Assert: Type(iteratorRecord.[[Iterator]]) is Object.

    // 2. Let iterator be iteratorRecord.[[Iterator]].
    let iterator = Value::from_object(iterator_record.iterator());

    // 3. Let innerResult be Completion(GetMethod(iterator, "return")).
    let mut inner_result: ThrowCompletionOr<Value> = Ok(Value::UNDEFINED);
    let get_method_result = iterator.get_method(vm, &vm.names.return_);
    if let Err(throw) = get_method_result {
        inner_result = Err(throw);
    }

    // 4. If innerResult.[[Type]] is normal, then
    if let Ok(return_method) = get_method_result {
        // a. Let return be innerResult.[[Value]].
        // b. If return is undefined, return ? completion.
        let Some(return_method) = return_method else {
            return completion;
        };

        // c. Set innerResult to Completion(Call(return, iterator)).
        inner_result = call_function_object(vm, return_method, iterator, &[]);
    }

    // 5. If completion.[[Type]] is throw, return ? completion.
    if completion.is_error() {
        return completion;
    }

    // 6. If innerResult.[[Type]] is throw, return ? innerResult.
    let inner_value = match inner_result {
        Err(throw) => return throw.into(),
        Ok(inner_value) => inner_value,
    };

    // 7. If Type(innerResult.[[Value]]) is not Object, throw a TypeError exception.
    if !inner_value.is_object() {
        return vm
            .throw_completion::<Value>(ErrorKind::TypeError, ErrorType::IterableReturnBadReturn, &[])
            .into();
    }

    // 8. Return ? completion.
    completion
}

// 7.4.11 IteratorClose ( iteratorRecord, completion , https://tc39.es/ecma262/#sec-iteratorclose
pub fn iterator_close(vm: &Vm, iterator_record: &IteratorRecordImpl, completion: Completion) -> Completion {
    iterator_close_impl(vm, iterator_record, completion)
}

// 7.4.12 IteratorCloseAll ( iters, completion ), https://tc39.es/ecma262/#sec-iteratorclose
pub fn iterator_close_all(
    vm: &Vm,
    iterator_records: &MarkedVec<'_, Gc<IteratorRecord>>,
    mut completion: Completion,
) -> Completion {
    // 1. For each element iter of iters, in reverse List order, do
    for index in (0..iterator_records.len()).rev() {
        let iterator_record = iterator_records.get(index).expect("the index is within the records");

        // a. Set completion to Completion(IteratorClose(iter, completion)).
        completion = iterator_close(vm, &iterator_record, completion);
    }

    // 2. Return ? completion.
    completion
}

// 7.4.15 CreateIteratorResultObject ( value, done ), https://tc39.es/ecma262/#sec-createiterresultobject
pub fn create_iterator_result_object(vm: &Vm, realm: Gc<Realm>, value: Value, done: bool) -> Gc<Object> {
    // 1. Let obj be OrdinaryObjectCreate(%Object.prototype%).
    let object = Object::create_with_premade_shape(vm, realm.iterator_result_object_shape());

    // 2. Perform ! CreateDataPropertyOrThrow(obj, "value", value).
    object.put_direct(realm.iterator_result_object_value_offset(), value);

    // 3. Perform ! CreateDataPropertyOrThrow(obj, "done", done).
    object.put_direct(realm.iterator_result_object_done_offset(), Value::from_bool(done));

    // 4. Return obj.
    object
}

// 7.4.17 IteratorToList ( iteratorRecord ), https://tc39.es/ecma262/#sec-iteratortolist
pub fn iterator_to_list<'vm>(
    vm: &'vm Vm,
    iterator_record: &IteratorRecordImpl,
) -> ThrowCompletionOr<MarkedVec<'vm, Value>> {
    // 1. Let values be a new empty List.
    let values = MarkedVec::new(vm);

    // 2. Repeat,
    loop {
        // a. Let next be ? IteratorStepValue(iteratorRecord).
        let next = iterator_step_value(vm, iterator_record)?;

        // b. If next is DONE, then
        let Some(next) = next else {
            // i. Return values.
            return Ok(values);
        };

        // c. Append next to values.
        values.push(next);
    }
}

// 7.3.36 SetterThatIgnoresPrototypeProperties ( thisValue, home, p, v ), https://tc39.es/ecma262/#sec-SetterThatIgnoresPrototypeProperties
pub fn setter_that_ignores_prototype_properties(
    vm: &Vm,
    this: Value,
    home: Gc<Object>,
    property: &PropertyKey,
    value: Value,
) -> ThrowCompletionOr<()> {
    // 1. If this is not an Object, then
    if !this.is_object() {
        // a. Throw a TypeError exception.
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&this]);
    }

    let this_object = this.as_object();

    // 2. If this is home, then
    if this_object == home {
        // a. NOTE: Throwing here emulates assignment to a non-writable data property on the home object in strict mode code.
        // b. Throw a TypeError exception.
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::DescWriteNonWritable, &[&this]);
    }

    // 3. Let desc be ? this.[[GetOwnProperty]](p).
    let descriptor = this_object.internal_get_own_property(vm, property)?;

    // 4. If desc is undefined, then
    if descriptor.is_none() {
        // a. Perform ? CreateDataPropertyOrThrow(this, p, v).
        this_object.create_data_property_or_throw(vm, property, value)?;
    }
    // 5. Else,
    else {
        // a. Perform ? Set(this, p, v, true).
        this_object.set(vm, property, value, ShouldThrowExceptions::Yes)?;
    }

    // 6. Return unused.
    Ok(())
}

// Non-standard
/// Iterates `iterable`, handing each value to `callback`, and closes the iterator with the completion the callback
/// returns to stop early.
pub fn get_iterator_values(
    vm: &Vm,
    iterable: Value,
    mut callback: impl FnMut(Value) -> Option<Completion>,
) -> Completion {
    let iterator_record = match get_iterator_impl(vm, iterable, IteratorHint::Sync) {
        Ok(iterator_record) => iterator_record,
        Err(throw) => return throw.into(),
    };

    loop {
        let next = match iterator_step_value(vm, &iterator_record) {
            Ok(next) => next,
            Err(throw) => return throw.into(),
        };
        let Some(next) = next else {
            return Completion::normal(Value::UNDEFINED);
        };

        if let Some(completion) = callback(next) {
            return iterator_close(vm, &iterator_record, completion);
        }
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::interpreter::slow_paths::control::test_script_realm::ScriptRealm;
    use crate::runtime::completion::{CompletionType, Must, Throw};

    use crate::runtime::realm::test_realm::{enumerable_keys, key, thrown_message};

    const PRELUDE: &str = r#"
var log = "";
function makeIterable(limit, returnMethod) { let iterable = { limit: limit, returnMethod: returnMethod }; iterable[Symbol.iterator] = iterableIterator; return iterable; }
function iterableIterator() { log += "I"; return { n: 0, limit: this.limit, next: counterNext, return: this.returnMethod }; }
function counterNext() { this.n++; log += "n" + this.n; return { value: this.n, done: this.n > this.limit }; }
function closeNormally() { log += "R"; return {}; }
function closeNamed() { log += this.name; return {}; }
function closeWithPrimitive() { log += "P"; return 1; }
function closeByThrowing() { log += "T"; throw "from return"; }
function returnsPrimitive() { return 1; }
function primitiveNext() { return 1; }
function throwingNext() { throw "next"; }
function withIterator(iteratorMethod) { let iterable = {}; iterable[Symbol.iterator] = iteratorMethod; return iterable; }
"#;

    /// "Kind: message" of the error a trapped throw would have created, as the C++ js prints an uncaught one.
    fn thrown_error<T>(operation: impl FnOnce() -> T) -> String {
        let message = thrown_message(operation);
        let error = message
            .split("creating a ")
            .nth(1)
            .unwrap_or_else(|| panic!("the operation stopped without throwing: {message}"));
        let (kind, message) = error
            .split_once(" with the message \"")
            .expect("the error has a message");
        format!("{kind}: {}", message.rsplit_once('"').expect("the message is quoted").0)
    }

    fn log(script_realm: &ScriptRealm) -> String {
        script_realm.global_value("log").as_string().to_utf8()
    }

    fn string(value: Value) -> String {
        value.as_string().to_utf8()
    }

    fn record_for(vm: &Vm, script_realm: &ScriptRealm, source: &str) -> IteratorRecordImpl {
        let iterable = script_realm.run(source).must();
        get_iterator_impl(vm, iterable, IteratorHint::Sync).must()
    }

    #[test]
    fn iterator_records_keep_their_iterator_and_next_method_alive() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let iterator = script_realm.test_realm.object();
        let next_method = script_realm.global_value("counterNext");
        let record = IteratorRecord::create(&vm, Some(iterator), next_method, false);
        let iterator_object = Iterator::create(&vm, script_realm.test_realm.object(), record);
        vm.heap().collect_garbage();
        assert!(record.iterator() == iterator && record.next_method() == next_method && !record.done());
        assert!(iterator_object.iterated() == record);
        let without_iterated = Iterator::create_without_iterated(&vm, script_realm.test_realm.object());
        assert!(without_iterated.iterated().next_method() == Value::UNDEFINED);
    }

    #[test]
    fn getting_an_iterator_reports_what_is_not_iterable_like_the_cpp_runtime() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        script_realm
            .run("var notCallable = {}; notCallable[Symbol.iterator] = 1; var primitiveIterator = withIterator(returnsPrimitive);")
            .must();
        let get = |value: Value| get_iterator_impl(&vm, value, IteratorHint::Sync).map(|_| ());
        let plain = Value::from_object(script_realm.test_realm.object());
        assert_eq!(
            thrown_error(|| get(plain)),
            "TypeError: [object Object] is not iterable"
        );
        assert_eq!(
            thrown_error(|| get(Value::NULL)),
            "TypeError: ToObject on null or undefined"
        );
        assert_eq!(
            thrown_error(|| get(script_realm.global_value("notCallable"))),
            "TypeError: 1 is not a function"
        );
        assert_eq!(
            thrown_error(|| get(script_realm.global_value("primitiveIterator"))),
            "TypeError: [object Object] is not iterable"
        );

        let iterable = script_realm.run("makeIterable(2, closeNormally)").must();
        let record = get_iterator(&vm, iterable, IteratorHint::Sync).must();
        assert!(record.next_method() == script_realm.global_value("counterNext") && !record.done());
    }

    #[test]
    fn stepping_reads_values_until_done_and_marks_the_record() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let record = record_for(&vm, &script_realm, "makeIterable(3, closeNormally)");
        let values = iterator_to_list(&vm, &record).must();
        assert!(values.to_vec() == [1, 2, 3].map(Value::from_i32));
        assert!(record.done());
        assert_eq!(log(&script_realm), "In1n2n3n4");

        let record = record_for(&vm, &script_realm, "makeIterable(1, closeNormally)");
        assert!(
            matches!(iterator_step(&vm, &record).must(), IterationResult::Value(value) if value == Value::UNDEFINED)
        );
        let result = iterator_next(&vm, &record, Some(Value::from_i32(5))).must();
        assert!(iterator_complete(&vm, result).must());
        assert!(iterator_value(&vm, result).must() == Value::from_i32(2));
        assert!(!record.done());
        assert!(matches!(iterator_step(&vm, &record).must(), IterationResult::Done));
        assert!(record.done());
    }

    #[test]
    fn iterator_next_marks_the_record_done_when_next_throws_or_returns_a_primitive() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let iterator = script_realm.test_realm.object();

        let throwing = IteratorRecordImpl::new(Some(iterator), script_realm.global_value("throwingNext"), false);
        let thrown = iterator_next(&vm, &throwing, None).expect_err("next throws");
        assert_eq!(string(thrown.value()), "next");
        assert!(throwing.done());

        let primitive = IteratorRecordImpl::new(Some(iterator), script_realm.global_value("primitiveNext"), false);
        assert_eq!(
            thrown_error(|| iterator_next(&vm, &primitive, None)),
            "TypeError: iterator.next() returned a non-object value"
        );
        assert!(primitive.done());
    }

    #[test]
    fn closing_an_iterator_calls_return_and_keeps_the_completion_like_the_cpp_runtime() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let normal = Completion::normal(Value::from_i32(7));
        let throw = Completion::new(CompletionType::Throw, Value::from_i32(8));

        let record = record_for(&vm, &script_realm, "log = \"\"; makeIterable(5, closeNormally)");
        let completion = iterator_close(&vm, &record, normal);
        assert!(completion.completion_type() == CompletionType::Normal && completion.value() == Value::from_i32(7));
        assert_eq!(log(&script_realm), "IR");

        let record = record_for(&vm, &script_realm, "log = \"\"; makeIterable(5)");
        let completion = iterator_close(&vm, &record, Completion::new(CompletionType::Break, Value::UNDEFINED));
        assert!(completion.completion_type() == CompletionType::Break);
        assert_eq!(log(&script_realm), "I");

        let record = record_for(&vm, &script_realm, "makeIterable(5, closeByThrowing)");
        let completion = iterator_close(&vm, &record, normal);
        assert!(completion.is_error() && string(completion.value()) == "from return");
        let completion = iterator_close(&vm, &record, throw);
        assert!(completion.is_error() && completion.value() == Value::from_i32(8));

        let record = record_for(&vm, &script_realm, "makeIterable(5, closeWithPrimitive)");
        assert!(iterator_close(&vm, &record, throw).value() == Value::from_i32(8));
        assert_eq!(
            thrown_error(|| iterator_close(&vm, &record, normal)),
            "TypeError: iterator.return() returned a non-object value"
        );
    }

    fn value_or_close(
        vm: &Vm,
        iterator_record: &IteratorRecordImpl,
        value: ThrowCompletionOr<Value>,
    ) -> ThrowCompletionOr<Value> {
        Ok(try_or_close_iterator!(vm, iterator_record, value))
    }

    #[test]
    fn an_abrupt_completion_closes_the_iterator_and_is_returned() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let record = record_for(&vm, &script_realm, "log = \"\"; makeIterable(5, closeByThrowing)");
        assert!(value_or_close(&vm, &record, Ok(Value::from_i32(1))).must() == Value::from_i32(1));
        assert_eq!(log(&script_realm), "I");
        let thrown = value_or_close(&vm, &record, Err(Throw::new(Value::from_i32(2)))).expect_err("the value throws");
        assert!(thrown.value() == Value::from_i32(2));
        assert_eq!(log(&script_realm), "IT");
    }

    #[test]
    fn closing_all_iterators_goes_in_reverse_order() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        script_realm
            .run(
                "log = \"\"; var a = { name: \"a\", return: closeNamed }; var b = { name: \"b\", return: closeNamed };",
            )
            .must();
        let records = MarkedVec::new(&vm);
        for name in ["a", "b"] {
            let iterator = script_realm.global_value(name).as_object();
            records.push(IteratorRecord::create(&vm, Some(iterator), Value::UNDEFINED, false));
        }
        let completion = iterator_close_all(&vm, &records, Completion::normal(Value::UNDEFINED));
        assert!(!completion.is_abrupt());
        assert_eq!(log(&script_realm), "ba");
    }

    #[test]
    fn flattenable_iterators_reject_primitives_like_the_cpp_runtime() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let one = Value::from_i32(1);
        assert_eq!(
            thrown_error(|| get_iterator_flattenable(&vm, one, PrimitiveHandling::RejectPrimitives)),
            "TypeError: 1 is not an object"
        );
        assert_eq!(
            thrown_error(|| get_iterator_flattenable(&vm, one, PrimitiveHandling::IterateStringPrimitives)),
            "TypeError: 1 is not a string"
        );
        assert_eq!(
            thrown_error(|| get_iterator_flattenable(&vm, Value::NULL, PrimitiveHandling::IterateStringPrimitives)),
            "TypeError: null is not a string"
        );
        let primitive_iterator = script_realm.run("withIterator(returnsPrimitive)").must();
        assert_eq!(
            thrown_error(|| get_iterator_flattenable(&vm, primitive_iterator, PrimitiveHandling::RejectPrimitives)),
            "TypeError: 1 is not an object"
        );

        let direct = script_realm.run("({ next: counterNext })").must();
        let record = get_iterator_flattenable(&vm, direct, PrimitiveHandling::RejectPrimitives).must();
        assert!(record.iterator() == direct.as_object());
        assert!(record.next_method() == script_realm.global_value("counterNext"));
    }

    #[test]
    fn iterator_result_objects_have_their_premade_shape() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let realm = script_realm.test_realm.realm;
        let names = &vm.names;
        let result = create_iterator_result_object(&vm, realm, Value::from_i32(3), true);
        assert_eq!(enumerable_keys(&vm, &result), "value,done");
        assert!(result.get(&vm, &names.value).must() == Value::from_i32(3));
        assert!(result.get(&vm, &names.done).must() == Value::TRUE);
        assert!(result.prototype() == Some(realm.object_prototype()));
    }

    #[test]
    fn the_setter_that_ignores_prototype_properties_writes_only_to_its_receiver() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let home = script_realm.test_realm.object();
        let property = key("tag");
        let set =
            |this: Value, value: Value| setter_that_ignores_prototype_properties(&vm, this, home, &property, value);
        assert_eq!(
            thrown_error(|| set(Value::from_i32(1), Value::NULL)),
            "TypeError: 1 is not an object"
        );
        assert_eq!(
            thrown_error(|| set(Value::from_object(home), Value::NULL)),
            "TypeError: Cannot write to non-writable property '[object Object]'"
        );

        let receiver = Object::create(&vm, script_realm.test_realm.realm, Some(home));
        set(Value::from_object(receiver), Value::from_i32(5)).must();
        let descriptor = receiver
            .internal_get_own_property(&vm, &property)
            .must()
            .expect("the receiver has the property now");
        assert!(descriptor.value == Some(Value::from_i32(5)));
        assert_eq!(
            (descriptor.writable, descriptor.enumerable, descriptor.configurable),
            (Some(true), Some(true), Some(true))
        );
        set(Value::from_object(receiver), Value::from_i32(6)).must();
        assert!(receiver.get(&vm, &property).must() == Value::from_i32(6));
        assert!(home.get(&vm, &property).must() == Value::UNDEFINED);
    }

    #[test]
    fn iterating_values_closes_the_iterator_when_the_callback_stops() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let iterable = script_realm.run("log = \"\"; makeIterable(5, closeNormally)").must();
        let mut seen = 0;
        let completion = get_iterator_values(&vm, iterable, |value| {
            seen += 1;
            (value == Value::from_i32(2)).then(|| Completion::new(CompletionType::Throw, Value::from_i32(9)))
        });
        assert!(completion.is_error() && completion.value() == Value::from_i32(9));
        assert_eq!(seen, 2);
        assert_eq!(log(&script_realm), "In1n2R");

        let iterable = script_realm.run("log = \"\"; makeIterable(2, closeNormally)").must();
        let completion = get_iterator_values(&vm, iterable, |_| None);
        assert!(!completion.is_abrupt() && completion.value() == Value::UNDEFINED);
        assert_eq!(log(&script_realm), "In1n2n3");
    }
}
