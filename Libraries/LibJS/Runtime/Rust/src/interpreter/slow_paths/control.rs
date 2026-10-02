/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The slow paths for literals, iterators, generators and control flow, and their helpers.

use ak::{Utf16FlyString, Utf16String};

use crate::bytecode::executable::StaticPropertyLookupCacheSite;
use crate::bytecode::op;
use crate::bytecode::property_access::get_own_property_without_side_effects;
use crate::interpreter::runtime_functions::{SlowPathControl, asm_try, handle_asm_exception};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::execution_context::ExecutionContext;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::array::Array;
use crate::runtime::async_from_sync_iterator_prototype::create_async_from_sync_iterator;
use crate::runtime::async_generator::AsyncGenerator;
use crate::runtime::completion::{Completion, Must, completion_type_from_bytecode};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::generator_object::GeneratorObject;
use crate::runtime::iterator::{
    IteratorRecord, IteratorRecordImpl, get_iterator_from_method_impl, get_iterator_impl, get_iterator_values,
    iterator_close, iterator_hint_from_bytecode, iterator_next, iterator_step_value,
};
use crate::runtime::map::Map;
use crate::runtime::map_iterator::map_iteration_is_unobservable;
use crate::runtime::object::{IndexedStorageKind, IntegrityLevel};
use crate::runtime::property_attributes::{Attribute, DEFAULT_ATTRIBUTES, PropertyAttributes};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;
use crate::runtime::regexp_object::RegExpObject;
use crate::runtime::set::Set;
use crate::runtime::set_iterator::set_iteration_is_unobservable;
use crate::utf16::Utf16View;

/// Throws a new error and hands it to the interpreter.
fn throw_error(
    vm: &Vm,
    pc: u32,
    kind: ErrorKind,
    error_type: ErrorType,
    arguments: &[&dyn core::fmt::Display],
) -> SlowPathControl {
    match vm.throw_completion::<()>(kind, error_type, arguments) {
        Err(throw) => handle_asm_exception(vm, pc, throw.value()),
        Ok(()) => unreachable!("throw_completion always throws"),
    }
}

fn current_realm(vm: &Vm) -> Gc<Realm> {
    vm.current_realm().expect("slow paths run with a current realm")
}

fn create_type_error(vm: &Vm, realm: Gc<Realm>, message: &Utf16FlyString) -> Gc<Object> {
    ErrorKind::TypeError
        .create(vm, realm, Utf16View::of_fly_string(message).to_utf16_string())
        .upcast()
}

fn create_reference_error(vm: &Vm, realm: Gc<Realm>, message: &Utf16FlyString) -> Gc<Object> {
    ErrorKind::ReferenceError
        .create(vm, realm, Utf16View::of_fly_string(message).to_utf16_string())
        .upcast()
}

/// The iterator record a slow path receives as the three registers the bytecode keeps it in.
fn iterator_record_from_registers(
    iterator_object: Value,
    iterator_next: Value,
    iterator_done: Value,
) -> IteratorRecordImpl {
    IteratorRecordImpl::new(
        Some(iterator_object.as_object()),
        iterator_next,
        iterator_done.as_bool(),
    )
}

/// The primitive values a NewPrimitiveArray instruction carries after its fixed fields.
fn primitive_array_elements(instruction: &op::NewPrimitiveArray) -> &[Value] {
    // SAFETY: The bytecode holds element_count values right after the fixed fields of the instruction, within the
    // length it records, and it does not change while the executable that holds it is alive.
    unsafe { core::slice::from_raw_parts(instruction.elements.as_ptr(), instruction.element_count as usize) }
}

pub fn debugger_check_breakpoint(_vm: &Vm, _pc: u32) {
    // NB: The Rust runtime has no debugger, and C++ returns right away without one.
}

pub fn fallback_handler(_pc: u32) -> SlowPathControl {
    // NB: Every bytecode opcode has a DSL handler, so this should never run.
    unreachable!("every bytecode opcode has a handler")
}

pub fn new_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewArray,
    values: &mut op::NewArrayValues,
    elements: &[Value],
) -> SlowPathControl {
    let array = Array::create(vm, current_realm(vm), u64::from(instruction.element_count), None).must();
    for (index, element) in elements.iter().enumerate() {
        array.indexed_put(index as u32, *element, DEFAULT_ATTRIBUTES);
    }
    values.dst = Value::from_object(array);
    SlowPathControl::continue_at(pc + instruction.length())
}

pub fn new_primitive_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewPrimitiveArray,
    values: &mut op::NewPrimitiveArrayValues,
) -> SlowPathControl {
    let array = Array::create(vm, current_realm(vm), u64::from(instruction.element_count), None).must();
    for (index, element) in primitive_array_elements(instruction).iter().enumerate() {
        array.indexed_put(index as u32, *element, DEFAULT_ATTRIBUTES);
    }
    values.dst = Value::from_object(array);
    SlowPathControl::continue_at(pc + instruction.length())
}

pub fn new_array_with_length(vm: &Vm, pc: u32, values: &mut op::NewArrayWithLengthValues) -> SlowPathControl {
    let length = values.array_length.as_f64() as u64;
    let array = asm_try!(vm, pc, Array::create(vm, current_realm(vm), length, None));
    values.dst = Value::from_object(array);
    SlowPathControl::continue_at(pc + op::NewArrayWithLength::LENGTH)
}

pub fn array_append(
    vm: &Vm,
    pc: u32,
    instruction: &op::ArrayAppend,
    values: &mut op::ArrayAppendValues,
) -> SlowPathControl {
    let rhs = values.src;
    let lhs_array = values.dst.as_object();
    debug_assert!(lhs_array.is_array_exotic_object());
    let lhs_size = lhs_array.indexed_array_like_size();

    if instruction.is_spread {
        let rhs_array = if rhs.is_object() {
            rhs.as_object().downcast::<Array>()
        } else {
            None
        };
        let rhs_set = if rhs.is_object() {
            rhs.as_object().downcast::<Set>()
        } else {
            None
        };
        let rhs_map = if rhs.is_object() {
            rhs.as_object().downcast::<Map>()
        } else {
            None
        };
        let mut iterator_record = None;

        if (rhs_array.is_some() || rhs_set.is_some() || rhs_map.is_some())
            && matches!(
                lhs_array.indexed_storage_kind(),
                IndexedStorageKind::None | IndexedStorageKind::Packed
            )
        {
            let iterator_method = asm_try!(
                vm,
                pc,
                rhs.get_method_with_cache(
                    vm,
                    &PropertyKey::from(vm.well_known_symbols().iterator),
                    vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::ArrayAppendIteratorMethod),
                )
            );
            let Some(iterator_method) = iterator_method else {
                return throw_error(vm, pc, ErrorKind::TypeError, ErrorType::NotIterable, &[&rhs]);
            };

            // OPTIMIZATION: The original array iterator has no observable side effects, so a packed
            //               array can be appended in bulk if its next method is also unchanged.
            let original_iterator_method = current_realm(vm).array_prototype_values_function();
            if let Some(rhs_array) = rhs_array
                && iterator_method == original_iterator_method
                && rhs_array.is_simple_packed_array()
            {
                let iterator_prototype = current_realm(vm).array_iterator_prototype();

                // NB: Inspect the intrinsic prototype's own property without invoking it. Using get()
                //     here would call an accessor with the prototype as its receiver, whereas the
                //     iterator protocol calls it with the newly created iterator as its receiver.
                //     Accessors and replacement methods therefore take the generic path below.
                let next_method = get_own_property_without_side_effects(
                    &iterator_prototype,
                    &vm.names.next,
                    vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::ArrayAppendNextMethod),
                );
                if next_method.is_function()
                    && next_method.as_function().as_native_function().is_some()
                    && next_method.as_function().is_array_prototype_next_builtin()
                    && rhs_array.indexed_packed_element_count() <= u32::MAX - lhs_size
                {
                    lhs_array.indexed_append_packed_elements_of(&rhs_array);
                    return SlowPathControl::continue_at(pc + op::ArrayAppend::LENGTH);
                }
            }

            // OPTIMIZATION: Iterating a Set or Map with its original iteration functions cannot be observed, and no
            //               user code runs while we append, so the collection storage can be read directly.
            if let Some(rhs_set) = rhs_set
                && set_iteration_is_unobservable(vm, current_realm(vm), iterator_method)
                && rhs_set.set_size() <= (u32::MAX - lhs_size) as usize
            {
                let mut index = lhs_size;
                rhs_set.for_each_value(|value| {
                    lhs_array.indexed_put(index, value, DEFAULT_ATTRIBUTES);
                    index += 1;
                });
                return SlowPathControl::continue_at(pc + op::ArrayAppend::LENGTH);
            }
            if let Some(rhs_map) = rhs_map
                && map_iteration_is_unobservable(vm, current_realm(vm), iterator_method)
                && rhs_map.map_size() <= (u32::MAX - lhs_size) as usize
            {
                // NB: Creating the entry arrays can trigger garbage collection, which does not modify maps.
                let realm = current_realm(vm);
                let mut index = lhs_size;
                rhs_map.for_each_entry(|key, value| {
                    let entry = Array::create_from(vm, realm, &[key, value]);
                    lhs_array.indexed_put(index, Value::from_object(entry), DEFAULT_ATTRIBUTES);
                    index += 1;
                });
                return SlowPathControl::continue_at(pc + op::ArrayAppend::LENGTH);
            }

            iterator_record = Some(asm_try!(
                vm,
                pc,
                get_iterator_from_method_impl(vm, rhs, iterator_method)
            ));
        }

        let mut index = u64::from(lhs_size);
        if let Some(iterator_record) = iterator_record {
            loop {
                let iterator_value = asm_try!(vm, pc, iterator_step_value(vm, &iterator_record));
                let Some(iterator_value) = iterator_value else {
                    break;
                };
                // NB: The C++ runtime truncates the size_t index to the u32 indexed_put() takes.
                lhs_array.indexed_put(index as u32, iterator_value, DEFAULT_ATTRIBUTES);
                index += 1;
            }
            return SlowPathControl::continue_at(pc + op::ArrayAppend::LENGTH);
        }

        let result = get_iterator_values(vm, rhs, |iterator_value| {
            lhs_array.indexed_put(index as u32, iterator_value, DEFAULT_ATTRIBUTES);
            index += 1;
            None
        });
        if result.is_error() {
            return handle_asm_exception(vm, pc, result.value());
        }
    } else {
        lhs_array.indexed_put(lhs_size, rhs, DEFAULT_ATTRIBUTES);
    }

    SlowPathControl::continue_at(pc + op::ArrayAppend::LENGTH)
}

pub fn get_template_object(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetTemplateObject,
    values: &mut op::GetTemplateObjectValues,
    strings: &[Value],
) -> SlowPathControl {
    let cache = vm.current_executable().template_object_cache(instruction.cache);

    if let Some(cached_template_object) = cache.cached_template_object() {
        values.dst = Value::from_object(cached_template_object);
        return SlowPathControl::continue_at(pc + instruction.length());
    }

    let realm = current_realm(vm);
    let count = instruction.strings_count / 2;
    let template_object = Array::create(vm, realm, u64::from(count), None).must();
    let raw_object = Array::create(vm, realm, u64::from(count), None).must();

    let enumerable = PropertyAttributes::new(Attribute::ENUMERABLE);
    for index in 0..count {
        template_object.indexed_put(index, strings[index as usize], enumerable);
        raw_object.indexed_put(index, strings[(count + index) as usize], enumerable);
    }

    raw_object.set_integrity_level(vm, IntegrityLevel::Frozen).must();
    template_object.define_direct_property(
        vm,
        &vm.names.raw,
        Value::from_object(raw_object),
        PropertyAttributes::default(),
    );
    template_object.set_integrity_level(vm, IntegrityLevel::Frozen).must();

    cache.set_cached_template_object(template_object);
    values.dst = Value::from_object(template_object);
    SlowPathControl::continue_at(pc + instruction.length())
}

pub fn new_regexp(vm: &Vm, pc: u32, instruction: &op::NewRegExp, values: &mut op::NewRegExpValues) -> SlowPathControl {
    let realm = current_realm(vm);
    let executable = vm.current_executable();
    let regexp_object = RegExpObject::create_with_pattern_and_flags(
        vm,
        realm,
        Utf16String::from(executable.get_string(instruction.source_index)),
        Utf16String::from(executable.get_string(instruction.flags_index)),
    );
    regexp_object.set_realm(realm);
    regexp_object.set_legacy_features_enabled(true);
    values.dst = Value::from_object(regexp_object);
    SlowPathControl::continue_at(pc + op::NewRegExp::LENGTH)
}

pub fn new_reference_error(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewReferenceError,
    values: &mut op::NewReferenceErrorValues,
) -> SlowPathControl {
    let realm = current_realm(vm);
    let executable = vm.current_executable();
    values.dst = Value::from_object(create_reference_error(
        vm,
        realm,
        executable.get_string(instruction.error_string),
    ));
    SlowPathControl::continue_at(pc + op::NewReferenceError::LENGTH)
}

pub fn new_type_error(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewTypeError,
    values: &mut op::NewTypeErrorValues,
) -> SlowPathControl {
    let realm = current_realm(vm);
    let executable = vm.current_executable();
    values.dst = Value::from_object(create_type_error(
        vm,
        realm,
        executable.get_string(instruction.error_string),
    ));
    SlowPathControl::continue_at(pc + op::NewTypeError::LENGTH)
}

pub fn get_iterator(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetIterator,
    values: &mut op::GetIteratorValues,
) -> SlowPathControl {
    let iterator_record = asm_try!(
        vm,
        pc,
        get_iterator_impl(vm, values.iterable, iterator_hint_from_bytecode(instruction.hint))
    );
    values.dst_iterator_object = Value::from_object(iterator_record.iterator());
    values.dst_iterator_next = iterator_record.next_method();
    values.dst_iterator_done = Value::from_bool(iterator_record.done());
    SlowPathControl::continue_at(pc + op::GetIterator::LENGTH)
}

pub fn iterator_close_slow_path(
    vm: &Vm,
    pc: u32,
    instruction: &op::IteratorClose,
    values: &mut op::IteratorCloseValues,
) -> SlowPathControl {
    let iterator_record =
        iterator_record_from_registers(values.iterator_object, values.iterator_next, values.iterator_done);

    let completion = Completion::new(
        completion_type_from_bytecode(instruction.completion_type),
        values.completion_value,
    );
    asm_try!(
        vm,
        pc,
        iterator_close(vm, &iterator_record, completion).into_throw_completion_or()
    );
    SlowPathControl::continue_at(pc + op::IteratorClose::LENGTH)
}

pub fn iterator_next_slow_path(vm: &Vm, pc: u32, values: &mut op::IteratorNextValues) -> SlowPathControl {
    let iterator_record =
        iterator_record_from_registers(values.iterator_object, values.iterator_next, values.iterator_done);
    let result = iterator_next(vm, &iterator_record, None);
    if iterator_record.done() {
        values.iterator_done = Value::TRUE;
    }
    values.dst = Value::from_object(asm_try!(vm, pc, result));
    SlowPathControl::continue_at(pc + op::IteratorNext::LENGTH)
}

pub fn iterator_next_unpack(vm: &Vm, pc: u32, values: &mut op::IteratorNextUnpackValues) -> SlowPathControl {
    let iterator_record =
        iterator_record_from_registers(values.iterator_object, values.iterator_next, values.iterator_done);
    let iteration_result_or_error = iterator_step_value(vm, &iterator_record);
    if iterator_record.done() {
        values.iterator_done = Value::TRUE;
    }

    if let Some(iteration_result) = asm_try!(vm, pc, iteration_result_or_error) {
        values.dst_value = iteration_result;
        values.dst_done = Value::FALSE;
    } else {
        values.dst_value = Value::UNDEFINED;
        values.dst_done = Value::TRUE;
    }

    SlowPathControl::continue_at(pc + op::IteratorNextUnpack::LENGTH)
}

pub fn iterator_to_array(vm: &Vm, pc: u32, values: &mut op::IteratorToArrayValues) -> SlowPathControl {
    let iterator_record = iterator_record_from_registers(
        values.iterator_object,
        values.iterator_next_method,
        values.iterator_done_property,
    );

    let array = Array::create(vm, current_realm(vm), 0, None).must();
    let mut index: u64 = 0;
    loop {
        let value_or_error = iterator_step_value(vm, &iterator_record);
        if iterator_record.done() {
            values.iterator_done_property = Value::TRUE;
        }
        let value = asm_try!(vm, pc, value_or_error);
        let Some(value) = value else {
            values.dst = Value::from_object(array);
            return SlowPathControl::continue_at(pc + op::IteratorToArray::LENGTH);
        };

        array
            .create_data_property_or_throw(vm, &PropertyKey::from_number(index), value)
            .must();
        index += 1;
    }
}

pub fn create_async_from_sync_iterator_slow_path(
    vm: &Vm,
    pc: u32,
    values: &mut op::CreateAsyncFromSyncIteratorValues,
) -> SlowPathControl {
    let iterator = values.iterator.as_object();
    let next_method = values.next_method;
    let done = values.done.as_bool();

    let iterator_record = IteratorRecord::create(vm, Some(iterator), next_method, done);
    let async_from_sync_iterator = create_async_from_sync_iterator(vm, iterator_record);

    let realm = current_realm(vm);
    let iterator_object = Object::create(vm, realm, None);
    iterator_object.define_direct_property(
        vm,
        &vm.names.iterator,
        Value::from_object(async_from_sync_iterator.iterator()),
        DEFAULT_ATTRIBUTES,
    );
    iterator_object.define_direct_property(
        vm,
        &vm.names.nextMethod,
        async_from_sync_iterator.next_method(),
        DEFAULT_ATTRIBUTES,
    );
    iterator_object.define_direct_property(
        vm,
        &vm.names.done,
        Value::from_bool(async_from_sync_iterator.done()),
        DEFAULT_ATTRIBUTES,
    );

    values.dst = Value::from_object(iterator_object);
    SlowPathControl::continue_at(pc + op::CreateAsyncFromSyncIterator::LENGTH)
}

pub fn get_completion_fields(_vm: &Vm, pc: u32, values: &mut op::GetCompletionFieldsValues) -> SlowPathControl {
    let completion_source = values.completion.as_object();
    if let Some(generator) = completion_source.downcast::<GeneratorObject>() {
        values.value_dst = generator.pending_completion_value();
        values.type_dst = Value::from_i32(generator.pending_completion_type() as i32);
        return SlowPathControl::continue_at(pc + op::GetCompletionFields::LENGTH);
    }

    let async_generator = completion_source
        .downcast::<AsyncGenerator>()
        .expect("a completion source is a generator or an async generator");
    values.value_dst = async_generator.pending_completion_value();
    values.type_dst = Value::from_i32(async_generator.pending_completion_type() as i32);
    SlowPathControl::continue_at(pc + op::GetCompletionFields::LENGTH)
}

pub fn set_completion_type(
    _vm: &Vm,
    pc: u32,
    instruction: &op::SetCompletionType,
    values: &mut op::SetCompletionTypeValues,
) -> SlowPathControl {
    let completion_source = values.completion.as_object();
    if let Some(generator) = completion_source.downcast::<GeneratorObject>() {
        generator.set_pending_completion_type(completion_type_from_bytecode(instruction.completion_type));
        return SlowPathControl::continue_at(pc + op::SetCompletionType::LENGTH);
    }

    completion_source
        .downcast::<AsyncGenerator>()
        .expect("a completion source is a generator or an async generator")
        .set_pending_completion_type(completion_type_from_bytecode(instruction.completion_type));
    SlowPathControl::continue_at(pc + op::SetCompletionType::LENGTH)
}

pub fn debugger(pc: u32) -> SlowPathControl {
    // NB: The Rust runtime has no debugger to pause in, and C++ continues past the statement without one.
    SlowPathControl::continue_at(pc + op::Debugger::LENGTH)
}

pub fn throw_if_tdz(vm: &Vm, pc: u32, values: &op::ThrowIfTDZValues) -> SlowPathControl {
    let value = values.src;
    if value.is_empty() {
        return throw_error(
            vm,
            pc,
            ErrorKind::ReferenceError,
            ErrorType::BindingNotInitialized,
            &[&value],
        );
    }
    SlowPathControl::continue_at(pc + op::ThrowIfTDZ::LENGTH)
}

pub fn throw_if_not_object(vm: &Vm, pc: u32, values: &op::ThrowIfNotObjectValues) -> SlowPathControl {
    let src = values.src;
    if !src.is_object() {
        return throw_error(vm, pc, ErrorKind::TypeError, ErrorType::NotAnObject, &[&src]);
    }
    SlowPathControl::continue_at(pc + op::ThrowIfNotObject::LENGTH)
}

pub fn throw_if_nullish(vm: &Vm, pc: u32, values: &op::ThrowIfNullishValues) -> SlowPathControl {
    let value = values.src;
    if value.is_nullish() {
        return throw_error(vm, pc, ErrorKind::TypeError, ErrorType::NotObjectCoercible, &[&value]);
    }
    SlowPathControl::continue_at(pc + op::ThrowIfNullish::LENGTH)
}

pub fn throw_const_assignment(vm: &Vm, pc: u32) -> SlowPathControl {
    throw_error(vm, pc, ErrorKind::TypeError, ErrorType::InvalidAssignToConst, &[])
}

pub fn r#await(vm: &Vm, instruction: &op::Await, values: &op::AwaitValues) -> SlowPathControl {
    let yielded_value = if values.argument.is_empty() {
        Value::UNDEFINED
    } else {
        values.argument
    };
    let context = running_execution_context(vm);
    context.yield_continuation.set(instruction.continuation_label.0);
    context.yield_is_await.set(true);
    context.yield_value_is_iterator_result.set(false);
    vm.do_return(yielded_value);
    SlowPathControl::EXIT
}

pub fn r#yield(vm: &Vm, instruction: &op::Yield, values: &op::YieldValues) -> SlowPathControl {
    let yielded_value = if values.value.is_empty() {
        Value::UNDEFINED
    } else {
        values.value
    };
    let context = running_execution_context(vm);
    match instruction.continuation_label.get() {
        Some(continuation_label) => context.yield_continuation.set(continuation_label.0),
        None => context.yield_continuation.set(ExecutionContext::NO_YIELD_CONTINUATION),
    }
    context.yield_is_await.set(false);
    context.yield_value_is_iterator_result.set(false);
    vm.do_return(yielded_value);
    SlowPathControl::EXIT
}

pub fn yield_iterator_result(
    vm: &Vm,
    instruction: &op::YieldIteratorResult,
    values: &op::YieldIteratorResultValues,
) -> SlowPathControl {
    let yielded_value = if values.value.is_empty() {
        Value::UNDEFINED
    } else {
        values.value
    };
    let context = running_execution_context(vm);
    context.yield_continuation.set(instruction.continuation_label.0);
    context.yield_is_await.set(false);
    context.yield_value_is_iterator_result.set(true);
    vm.do_return(yielded_value);
    SlowPathControl::EXIT
}

fn running_execution_context(vm: &Vm) -> &ExecutionContext {
    let context = vm.running_execution_context().expect("a generator's frame is running");
    // SAFETY: The running context is live while the slow path runs in it.
    unsafe { context.as_ref() }
}

/// A realm to run scripts in for the unit tests of the interpreter, with the little of the global object they need.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_script_realm {
    use super::*;
    use crate::runtime::completion::ThrowCompletionOr;
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::object::StackFrameInfo;
    use crate::runtime::print::{PrintContext, print};
    use crate::runtime::realm::test_realm::{TestRealm, key};
    use crate::script::Script;

    pub struct ScriptRealm<'vm> {
        vm: &'vm Vm,
        pub test_realm: TestRealm<'vm>,
        pub global: Gc<Object>,
    }

    impl<'vm> ScriptRealm<'vm> {
        /// A realm whose global object has `Symbol.iterator` and `ArrayPrototype`, %Array.prototype%, once `prelude`
        /// has run in it. The functions the prelude declares are compiled, so that the interpreter calls them inline:
        /// the slow path of calls is not in the runtime yet.
        pub fn new(vm: &'vm Vm, prelude: &str) -> Self {
            let test_realm = TestRealm::with_function_intrinsics(vm);
            let realm = test_realm.realm;
            let global = set_up_global_object(vm, realm);
            let symbol = test_realm.object();
            symbol.define_direct_property(
                vm,
                &key("iterator"),
                Value::from_symbol(vm.well_known_symbols().iterator),
                DEFAULT_ATTRIBUTES,
            );
            global.define_direct_property(vm, &key("Symbol"), Value::from_object(symbol), DEFAULT_ATTRIBUTES);
            global.define_direct_property(
                vm,
                &key("ArrayPrototype"),
                Value::from_object(realm.array_prototype()),
                DEFAULT_ATTRIBUTES,
            );
            let script_realm = Self { vm, test_realm, global };
            script_realm.run(prelude).must();

            let keys = global.internal_own_property_keys(vm).must();
            for index in 0..keys.len() {
                let property_key = PropertyKey::from_value(vm, keys.get(index).expect("the index is in bounds")).must();
                let value = global.get(vm, &property_key).must();
                if value.is_function() {
                    value
                        .as_object()
                        .get_stack_frame_info(vm, &mut StackFrameInfo::default());
                }
            }
            script_realm
        }

        pub fn run(&self, source: &str) -> ThrowCompletionOr<Value> {
            let source: Vec<u16> = source.encode_utf16().collect();
            let script = Script::parse(self.vm, &source, self.test_realm.realm).expect("the script parses");
            self.vm.run_script(script, None)
        }

        /// Runs `source` and describes its completion the way the C++ js REPL prints it.
        pub fn evaluate(&self, source: &str) -> String {
            let completion = self.run(source);
            let mut text = Vec::new();
            let mut context = PrintContext {
                vm: self.vm,
                stream: &mut text,
                strip_ansi: true,
                raw_strings: false,
            };
            let printed = match completion {
                Ok(value) => print(value, &mut context),
                Err(throw) => {
                    context
                        .stream
                        .write_all(b"Uncaught ")
                        .expect("writing into a buffer succeeds");
                    print(throw.value(), &mut context)
                }
            };
            printed.expect("printing into a buffer succeeds");
            String::from_utf8(text).expect("the printed value is UTF-8")
        }

        pub fn global_value(&self, name: &str) -> Value {
            self.global.get(self.vm, &key(name)).must()
        }
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::test_script_realm::ScriptRealm;
    use super::*;
    use crate::bytecode::operand::{InstructionHeader, Operand};
    use crate::runtime::async_from_sync_iterator::AsyncFromSyncIterator;
    use crate::runtime::completion::CompletionType;
    use crate::runtime::iterator::IteratorHint;
    use crate::runtime::realm::test_realm::{key, thrown_message};

    /// The functions the scripts below share.
    const PRELUDE: &str = r#"
var log = "";
function show(array) { let s = "["; for (let i = 0; i < array.length; i++) { if (i > 0) s += ","; s += i in array ? "" + array[i] : "_"; } return s + "]"; }
function makeIterable(limit, returnMethod) { let iterable = { limit: limit, returnMethod: returnMethod }; iterable[Symbol.iterator] = iterableIterator; return iterable; }
function iterableIterator() { log += "I"; return { n: 0, limit: this.limit, next: counterNext, return: this.returnMethod }; }
function counterNext() { this.n++; log += "n" + this.n; return { value: this.n, done: this.n > this.limit }; }
function closeNormally() { log += "R"; return {}; }
function closeWithPrimitive() { log += "P"; return 1; }
function closeByThrowing() { log += "T"; throw "from return"; }
function withIterator(iteratorMethod) { let iterable = {}; iterable[Symbol.iterator] = iteratorMethod; return iterable; }
function throwingIterator() { log += "I"; return { next: throwingNext, return: closeNormally }; }
function throwingNext() { log += "n"; throw "next"; }
function primitiveResultIterator() { log += "I"; return { next: primitiveNext, return: closeNormally }; }
function primitiveNext() { log += "n"; return 1; }
function undefinedValuesIterator() { log += "I"; return { next: undefinedNext, return: closeNormally }; }
function undefinedNext() { log += "u"; return { value: undefined, done: false }; }
function returnsPrimitive() { return 1; }
function boom() { throw "boom"; }
function firstOf(iterable) { for (let x of iterable) return x; }
function tag(strings) { return strings; }
function templateOf() { return tag`a${1}b\n${2}`; }
function invalidEscapesOf() { return tag`\unicode and \u{110000}${0}\xg`; }
function twoSites() { return tag`x` === tag`x`; }
function finallyLoop() { let s = ""; for (let i = 0; i < 4; i++) { try { if (i == 1) continue; if (i == 3) break; s += "t" + i; } finally { s += "f" + i; } } return s; }
function finallyAfterReturn() { try { return "try"; } finally { log += "fin"; } }
function finallyOverridesReturn() { try { return 1; } finally { return 2; } }
function finallyOverridesThrow() { try { throw 1; } finally { return "f"; } }
function finallyRethrows() { try { try { throw "inner"; } finally { log += "f1"; } } catch (e) { log += "c:" + e; } finally { log += "f2"; } return log; }
function breakInFinally() { for (;;) { try { throw "x"; } finally { break; } } return "after"; }
function throwInFinally() { try { return 1; } finally { throw "f"; } }
function nestedFinally() { let s = ""; for (let i = 0; i < 2; i++) { try { try { continue; } finally { s += "a" + i; } } finally { s += "b" + i; } } return s; }
function returnThroughFinallyInForOf(iterable) { for (let x of iterable) { try { return x; } finally { log += "F"; } } }
"#;

    /// What Build/release/bin/js -i -l printed for each script, run after the prelude with
    /// `var ArrayPrototype = Array.prototype;` in front of it. The scripts that end in a TypeError the runtime throws
    /// are tested through the operations that throw it, since throwing stops the process inside the interpreter until
    /// realms have error constructors.
    const SCRIPT_OUTCOMES: &[(&str, &str)] = &[
        ("show([1, , 3, , ]) + [1, , 3, , ].length", "\"[1,_,3,_]4\""),
        (
            "let x = \"x\"; show([x, , x + \"y\", undefined, null, true]) + [x, , ].length",
            "\"[x,_,xy,undefined,null,true]2\"",
        ),
        (
            "[].length + \"|\" + [,].length + \"|\" + show([,,]) + \"|\" + show([null, false, -0, 1.5])",
            "\"0|1|[_,_]|[null,false,0,1.5]\"",
        ),
        (
            "let src = [1, 2, 3]; show([0, ...src, 4, , ...src, ...[]])",
            "\"[0,1,2,3,4,_,1,2,3]\"",
        ),
        ("log = \"\"; show([...makeIterable(3)]) + log", "\"[1,2,3]In1n2n3n4\""),
        (
            "let holes = [1, , 3]; show([...holes]) + show([...[1], , 2])",
            "\"[1,undefined,3][1,_,2]\"",
        ),
        (
            "log = \"\"; show([0, ...makeIterable(2), ...makeIterable(1)]) + log",
            "\"[0,1,2,1]In1n2n3In1n2\"",
        ),
        (
            "let t1 = templateOf(), t2 = templateOf(); \"\" + (t1 === t2) + (t1 === tag`a${1}b\\n${2}`) + t1.length + t1.raw.length + show(t1) + show(t1.raw)",
            "\"truefalse33[a,b\\n,][a,b\\\\n,]\"",
        ),
        (
            "let t = templateOf(); t[0] = \"changed\"; t.raw[0] = \"changed\"; t.extra = 1; delete t[1]; t.raw.length = 0; t.raw = 1; t[0] + t.raw[0] + t.extra + t.length + t.raw.length",
            "\"aaundefined33\"",
        ),
        (
            "let t = invalidEscapesOf(); show(t) + \"|\" + show(t.raw)",
            "\"[undefined,undefined]|[\\\\unicode and \\\\u{110000},\\\\xg]\"",
        ),
        ("twoSites()", "false"),
        (
            "let first = templateOf(); first === templateOf() && first.raw === templateOf().raw",
            "true",
        ),
        ("`a${1 + 1}b${\"c\"}`", "\"a2bc\""),
        ("let s = \"\"; for (let x of [1, 2, 3]) s += x; s", "\"123\""),
        (
            "log = \"\"; for (let x of makeIterable(5, closeNormally)) { log += \"b\" + x; if (x == 2) break; } log",
            "\"In1b1n2b2R\"",
        ),
        (
            "log = \"\"; for (let x of makeIterable(3, closeNormally)) { if (x == 2) continue; log += \"b\" + x; } log",
            "\"In1b1n2n3b3n4\"",
        ),
        ("log = \"\"; firstOf(makeIterable(5, closeNormally)) + log", "\"1In1R\""),
        (
            "log = \"\"; try { for (let x of makeIterable(5, closeByThrowing)) throw \"body\"; } catch (e) { log += \"|\" + e; } log",
            "\"In1T|body\"",
        ),
        (
            "log = \"\"; try { for (let x of makeIterable(5, closeByThrowing)) break; } catch (e) { log += \"|\" + e; } log",
            "\"In1T|from return\"",
        ),
        (
            "log = \"\"; try { for (let x of makeIterable(5, closeWithPrimitive)) throw \"body\"; } catch (e) { log += \"|\" + e; } log",
            "\"In1P|body\"",
        ),
        (
            "log = \"\"; for (let x of makeIterable(2, closeNormally)) {} log",
            "\"In1n2n3\"",
        ),
        (
            "log = \"\"; outer: for (let i = 0; i < 2; i++) { for (let x of makeIterable(5, closeNormally)) { log += i; continue outer; } } log",
            "\"In10RIn11R\"",
        ),
        (
            "log = \"\"; outer: for (let x of makeIterable(5, closeNormally)) { for (let y of makeIterable(5, closeNormally)) { log += \"|\"; break outer; } } log",
            "\"In1In1|RR\"",
        ),
        (
            "log = \"\"; for (let x of makeIterable(3)) { if (x == 2) break; } log",
            "\"In1n2\"",
        ),
        (
            "log = \"\"; try { for (let x of withIterator(throwingIterator)) {} } catch (e) { log += \"|\" + e; } log",
            "\"In|next\"",
        ),
        (
            "log = \"\"; let [a, , b] = makeIterable(5, closeNormally); a + \",\" + b + log",
            "\"1,3In1n2n3R\"",
        ),
        (
            "log = \"\"; let [a, ...rest] = makeIterable(3, closeNormally); a + show(rest) + log",
            "\"1[2,3]In1n2n3n4\"",
        ),
        (
            "log = \"\"; let [a, b, c] = makeIterable(1, closeNormally); a + \",\" + b + \",\" + c + log",
            "\"1,undefined,undefinedIn1n2\"",
        ),
        ("let [x, [y, z]] = [1, [2, 3]]; x + y + z", "6"),
        (
            "log = \"\"; try { let [a = boom()] = withIterator(undefinedValuesIterator); } catch (e) { log += \"|\" + e; } log",
            "\"IuR|boom\"",
        ),
        ("log = \"\"; let [] = makeIterable(5, closeNormally); log", "\"IR\""),
        ("log = \"\"; let [, ] = makeIterable(5, closeNormally); log", "\"In1R\""),
        (
            "log = \"\"; let [...all] = makeIterable(2, closeNormally); show(all) + log",
            "\"[1,2]In1n2n3\"",
        ),
        (
            "log = \"\"; let p, q; [p, q] = makeIterable(5, closeNormally); p + q + log",
            "\"3In1n2R\"",
        ),
        (
            "log = \"\"; try { let [a] = withIterator(throwingIterator); } catch (e) { log += \"|\" + e; } log",
            "\"In|next\"",
        ),
        ("finallyLoop()", "\"t0f0f1t2f2f3\""),
        ("log = \"\"; finallyAfterReturn() + log", "\"tryfin\""),
        ("finallyOverridesReturn()", "2"),
        ("finallyOverridesThrow()", "\"f\""),
        ("log = \"\"; finallyRethrows()", "\"f1c:innerf2\""),
        ("breakInFinally()", "\"after\""),
        (
            "let caught; try { throwInFinally(); } catch (e) { caught = e; } caught",
            "\"f\"",
        ),
        ("nestedFinally()", "\"a0b0a1b1\""),
        (
            "log = \"\"; for (let x of makeIterable(5, closeNormally)) { try { break; } finally { log += \"F\"; } } log",
            "\"In1FR\"",
        ),
        (
            "log = \"\"; returnThroughFinallyInForOf(makeIterable(5, closeNormally)) + log",
            "\"1In1FR\"",
        ),
        (
            "log = \"\"; let r = \"\"; for (let x of makeIterable(3, closeNormally)) { try { if (x == 1) continue; r += x; } finally { r += \"f\"; } } r + log",
            "\"f2f3fIn1n2n3n4\"",
        ),
        ("debugger; 1", "1"),
    ];

    fn check_script_outcomes(collect_on_every_allocation: bool) {
        let vm = Vm::create();
        vm.heap()
            .set_should_collect_on_every_allocation(collect_on_every_allocation);
        for (source, expected) in SCRIPT_OUTCOMES {
            let script_realm = ScriptRealm::new(&vm, PRELUDE);
            assert_eq!(script_realm.evaluate(source), *expected, "evaluating {source:?}");
        }
    }

    #[test]
    fn literals_iteration_and_finally_blocks_behave_like_the_cpp_runtime() {
        check_script_outcomes(false);
    }

    #[test]
    fn literals_iteration_and_finally_blocks_survive_collecting_on_every_allocation() {
        check_script_outcomes(true);
    }

    #[test]
    fn template_objects_are_frozen_and_cached_by_their_call_site() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        script_realm
            .run("var first = templateOf(); var second = templateOf();")
            .must();
        let first = script_realm.global_value("first").as_object();
        assert!(first == script_realm.global_value("second").as_object());
        assert!(first.is_array_exotic_object());
        assert!(first.test_integrity_level(&vm, IntegrityLevel::Frozen).must());

        let raw_descriptor = first
            .internal_get_own_property(&vm, &vm.names.raw)
            .must()
            .expect("the template object has a raw property");
        assert_eq!(
            (
                raw_descriptor.writable,
                raw_descriptor.enumerable,
                raw_descriptor.configurable
            ),
            (Some(false), Some(false), Some(false))
        );
        let raw = raw_descriptor.value.expect("raw is a data property").as_object();
        assert!(raw.test_integrity_level(&vm, IntegrityLevel::Frozen).must());
        let element = first
            .internal_get_own_property(&vm, &PropertyKey::from(0))
            .must()
            .expect("the template object has its strings");
        assert_eq!(
            (element.writable, element.enumerable, element.configurable),
            (Some(false), Some(true), Some(false))
        );
    }

    fn array_append_instruction(is_spread: bool) -> op::ArrayAppend {
        op::ArrayAppend {
            header: InstructionHeader {
                opcode: 0,
                strict: false,
            },
            dst: Operand(0),
            src: Operand(0),
            is_spread,
        }
    }

    #[test]
    fn spreading_a_packed_array_with_its_original_iterator_appends_it_in_bulk() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let realm = script_realm.test_realm.realm;
        let names = &vm.names;

        // With the original %Array.prototype.values% and %ArrayIteratorPrototype%.next, a packed array is appended in
        // bulk, even to an array without indexed storage.
        let source = script_realm.test_realm.array(&[Value::from_i32(1), Value::from_i32(2)]);
        let destination = script_realm.test_realm.array(&[]);
        assert_eq!(destination.indexed_storage_kind(), IndexedStorageKind::None);
        let mut values = op::ArrayAppendValues {
            dst: Value::from_object(destination),
            src: Value::from_object(source),
        };
        let control = array_append(&vm, 0, &array_append_instruction(true), &mut values);
        assert_eq!(control, SlowPathControl::continue_at(op::ArrayAppend::LENGTH));
        assert_eq!(destination.indexed_array_like_size(), 2);
        assert_eq!(destination.indexed_storage_kind(), IndexedStorageKind::Packed);
        assert!(destination.get(&vm, &PropertyKey::from(1)).must() == Value::from_i32(2));

        // A source with holes takes the iterator protocol, which reads the holes as undefined.
        source.indexed_put(3, Value::from_i32(3), DEFAULT_ATTRIBUTES);
        let mut values = op::ArrayAppendValues {
            dst: Value::from_object(destination),
            src: Value::from_object(source),
        };
        array_append(&vm, 0, &array_append_instruction(true), &mut values);
        assert_eq!(destination.indexed_array_like_size(), 6);
        assert!(destination.get(&vm, &PropertyKey::from(4)).must().is_undefined());
        assert!(destination.get(&vm, &PropertyKey::from(5)).must() == Value::from_i32(3));

        // So does a source whose iterator prototype's next method is not the original one.
        let array_iterator_prototype = realm.array_iterator_prototype();
        array_iterator_prototype.define_direct_property(&vm, &names.next, Value::UNDEFINED, DEFAULT_ATTRIBUTES);
        let packed_source = script_realm.test_realm.array(&[Value::from_i32(4)]);
        let mut values = op::ArrayAppendValues {
            dst: Value::from_object(destination),
            src: Value::from_object(packed_source),
        };
        let message = thrown_message(|| array_append(&vm, 0, &array_append_instruction(true), &mut values));
        assert!(message.contains("is not a function"), "{message}");

        let mut values = op::ArrayAppendValues {
            dst: Value::from_object(destination),
            src: Value::EMPTY,
        };
        array_append(&vm, 0, &array_append_instruction(false), &mut values);
        assert_eq!(destination.indexed_array_like_size(), 7);
        assert_eq!(destination.indexed_storage_kind(), IndexedStorageKind::Holey);
    }

    fn get_iterator_instruction(hint: IteratorHint) -> op::GetIterator {
        op::GetIterator {
            header: InstructionHeader {
                opcode: 0,
                strict: false,
            },
            dst_iterator_object: Operand(0),
            dst_iterator_next: Operand(0),
            dst_iterator_done: Operand(0),
            iterable: Operand(0),
            hint: hint as u32,
        }
    }

    #[test]
    fn iterator_slow_paths_write_the_iterator_record_back() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        script_realm
            .run(
                "var iterable = makeIterable(1, closeNormally); var primitive = withIterator(primitiveResultIterator);",
            )
            .must();

        let mut get_iterator_values = op::GetIteratorValues {
            dst_iterator_object: Value::EMPTY,
            dst_iterator_next: Value::EMPTY,
            dst_iterator_done: Value::EMPTY,
            iterable: script_realm.global_value("iterable"),
        };
        let instruction = get_iterator_instruction(IteratorHint::Sync);
        let control = get_iterator(&vm, 0, &instruction, &mut get_iterator_values);
        assert_eq!(control, SlowPathControl::continue_at(op::GetIterator::LENGTH));
        assert!(get_iterator_values.dst_iterator_object.is_object());
        assert!(get_iterator_values.dst_iterator_next == script_realm.global_value("counterNext"));
        assert!(get_iterator_values.dst_iterator_done == Value::FALSE);

        let mut next_values = op::IteratorNextValues {
            dst: Value::EMPTY,
            iterator_object: get_iterator_values.dst_iterator_object,
            iterator_next: get_iterator_values.dst_iterator_next,
            iterator_done: get_iterator_values.dst_iterator_done,
        };
        iterator_next_slow_path(&vm, 0, &mut next_values);
        let result = next_values.dst.as_object();
        assert!(result.get(&vm, &key("value")).must() == Value::from_i32(1));
        assert!(next_values.iterator_done == Value::FALSE);

        let mut unpack_values = op::IteratorNextUnpackValues {
            dst_value: Value::EMPTY,
            dst_done: Value::EMPTY,
            iterator_object: get_iterator_values.dst_iterator_object,
            iterator_next: get_iterator_values.dst_iterator_next,
            iterator_done: Value::FALSE,
        };
        iterator_next_unpack(&vm, 0, &mut unpack_values);
        assert!(unpack_values.dst_value == Value::UNDEFINED);
        assert!(unpack_values.dst_done == Value::TRUE);
        assert!(unpack_values.iterator_done == Value::TRUE);

        // The async hint gets the sync iterator and wraps it in an async-from-sync iterator.
        script_realm.run("log = \"\";").must();
        let async_instruction = get_iterator_instruction(IteratorHint::Async);
        let control = get_iterator(&vm, 0, &async_instruction, &mut get_iterator_values);
        assert_eq!(control, SlowPathControl::continue_at(op::GetIterator::LENGTH));
        assert!(
            get_iterator_values
                .dst_iterator_object
                .as_object()
                .is::<AsyncFromSyncIterator>()
        );
        assert!(get_iterator_values.dst_iterator_done == Value::FALSE);
        assert_eq!(script_realm.global_value("log").as_string().to_utf8(), "I");

        let mut get_primitive_iterator_values = op::GetIteratorValues {
            iterable: script_realm.global_value("primitive"),
            ..get_iterator_values
        };
        get_iterator(&vm, 0, &instruction, &mut get_primitive_iterator_values);
        let mut primitive_next_values = op::IteratorNextValues {
            dst: Value::EMPTY,
            iterator_object: get_primitive_iterator_values.dst_iterator_object,
            iterator_next: get_primitive_iterator_values.dst_iterator_next,
            iterator_done: Value::FALSE,
        };
        let message = thrown_message(|| iterator_next_slow_path(&vm, 0, &mut primitive_next_values));
        assert!(
            message.contains("iterator.next() returned a non-object value"),
            "{message}"
        );
    }

    fn set_completion_type_instruction(completion_type: CompletionType) -> op::SetCompletionType {
        op::SetCompletionType {
            header: InstructionHeader {
                opcode: 0,
                strict: false,
            },
            completion: Operand(0),
            completion_type: completion_type as u32,
        }
    }

    #[test]
    fn completion_fields_are_the_pending_completion_of_a_generator() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let generator = script_realm.run("(function* () {})()").must();
        let generator_object = generator
            .as_object()
            .downcast::<GeneratorObject>()
            .expect("calling a generator function creates a generator");
        generator_object.set_pending_completion(Completion::new(CompletionType::Throw, Value::from_i32(5)));

        let mut get_values = op::GetCompletionFieldsValues {
            type_dst: Value::EMPTY,
            value_dst: Value::EMPTY,
            completion: generator,
        };
        assert!(
            get_completion_fields(&vm, 0, &mut get_values)
                == SlowPathControl::continue_at(op::GetCompletionFields::LENGTH)
        );
        assert!(get_values.value_dst == Value::from_i32(5));
        assert!(get_values.type_dst == Value::from_i32(CompletionType::Throw as i32));

        let mut set_values = op::SetCompletionTypeValues { completion: generator };
        let instruction = set_completion_type_instruction(CompletionType::Return);
        assert!(
            set_completion_type(&vm, 0, &instruction, &mut set_values)
                == SlowPathControl::continue_at(op::SetCompletionType::LENGTH)
        );
        assert!(generator_object.pending_completion_type() == CompletionType::Return);
        assert!(generator_object.pending_completion_value() == Value::from_i32(5));
    }

    #[test]
    fn completion_fields_are_the_pending_completion_of_an_async_generator() {
        let vm = Vm::create();
        let script_realm = ScriptRealm::new(&vm, PRELUDE);
        let generator = script_realm.run("(async function* () {})()").must();
        let async_generator = generator
            .as_object()
            .downcast::<AsyncGenerator>()
            .expect("calling an async generator function creates an async generator");
        async_generator.set_pending_completion(Completion::new(CompletionType::Throw, Value::from_i32(6)));

        let mut get_values = op::GetCompletionFieldsValues {
            type_dst: Value::EMPTY,
            value_dst: Value::EMPTY,
            completion: generator,
        };
        assert!(
            get_completion_fields(&vm, 0, &mut get_values)
                == SlowPathControl::continue_at(op::GetCompletionFields::LENGTH)
        );
        assert!(get_values.value_dst == Value::from_i32(6));
        assert!(get_values.type_dst == Value::from_i32(CompletionType::Throw as i32));

        let mut set_values = op::SetCompletionTypeValues { completion: generator };
        let instruction = set_completion_type_instruction(CompletionType::Return);
        assert!(
            set_completion_type(&vm, 0, &instruction, &mut set_values)
                == SlowPathControl::continue_at(op::SetCompletionType::LENGTH)
        );
        assert!(async_generator.pending_completion_type() == CompletionType::Return);
        assert!(async_generator.pending_completion_value() == Value::from_i32(6));
    }
}
