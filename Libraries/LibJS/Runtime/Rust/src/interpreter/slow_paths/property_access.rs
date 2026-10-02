/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The slow paths for property access and its inline caches, and their helpers.

use core::ops::ControlFlow;
use std::collections::HashSet;

use ak::Utf16FlyString;
use libjs_abi::PutKind;
use libjs_abi::value as nan_box;

use crate::bytecode::executable::{
    Executable, ObjectPropertyIteratorCache, ObjectPropertyIteratorCacheData, ObjectPropertyIteratorFastPath,
    PropertyLookupCache, PropertyLookupCacheEntryType,
};
use crate::bytecode::op;
use crate::bytecode::operand::{IdentifierTableIndex, OptionalIndex};
use crate::bytecode::property_access::{
    self, CachePropertyAbsence, GetByIdMode, Strict, base_object_for_get, get_by_value_with_keyed_cache,
    get_cached_property_value, object_can_cache_property_additions, property_addition_is_cacheable,
    put_by_property_key,
};
use crate::gc::class::GcCell;
use crate::gc::class::class_of;
use crate::gc::root::MarkedVec;
use crate::interpreter::runtime_functions::{SlowPathControl, asm_try, handle_asm_exception};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::shape::{PrototypeChainValidity, Shape};
use crate::layout::value::Value;
use crate::runtime::abstract_operations::function_object_as_object;
use crate::runtime::array::Array;
use crate::runtime::array_buffer::{ElementType, Order};
use crate::runtime::canonical_index::{CanonicalIndex, CanonicalIndexType};
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::ecmascript_function_object::as_ecmascript_function_object;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::object::{IndexedStorageKind, Object};
use crate::runtime::private_environment::{PrivateEnvironment, PrivateName};
use crate::runtime::property_attributes::{Attribute, DEFAULT_ATTRIBUTES, PropertyAttributes};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::reference::Reference;
use crate::runtime::typed_array::{Kind, is_valid_integer_index, typed_array_of_object};

fn strict_of(header_strict: bool) -> Strict {
    if header_strict { Strict::Yes } else { Strict::No }
}

fn put_kind_from_operand(kind: u32) -> PutKind {
    match kind {
        0 => PutKind::Normal,
        1 => PutKind::Getter,
        2 => PutKind::Setter,
        3 => PutKind::Prototype,
        4 => PutKind::Own,
        _ => unreachable!("the bytecode only holds valid put kinds"),
    }
}

fn is_non_negative_int32(value: Value) -> bool {
    value.is_int32() && value.as_i32() >= 0
}

fn optional_identifier(vm: &Vm, index: &OptionalIndex<IdentifierTableIndex>) -> Option<Utf16FlyString> {
    let index = index.get()?;
    Some(vm.current_executable().get_identifier(index).clone())
}

fn running_private_environment(vm: &Vm) -> Gc<PrivateEnvironment> {
    let context = vm
        .running_execution_context()
        .expect("private names are used in an execution context");
    // SAFETY: The running execution context is live.
    unsafe { context.as_ref() }
        .private_environment
        .get()
        .expect("private names are used where a private environment is active")
}

#[cold]
fn throw_type_error(vm: &Vm, pc: u32, error_type: ErrorType) -> SlowPathControl {
    match vm.throw_completion::<()>(ErrorKind::TypeError, error_type, &[]) {
        Err(throw) => handle_asm_exception(vm, pc, throw.value()),
        Ok(()) => unreachable!("throw_completion always throws"),
    }
}

// 6.2.4.9 MakePrivateReference ( baseValue, privateIdentifier ), https://tc39.es/ecma262/#sec-makeprivatereference
/// The [[ReferencedName]] of the private reference, whose [[Base]] is the base value.
fn make_private_reference(vm: &Vm, private_identifier: &Utf16FlyString) -> PrivateName {
    // 1. Let privEnv be the running execution context's PrivateEnvironment.
    // 2. Assert: privEnv is not null.
    let private_environment = running_private_environment(vm);

    // 3. Let privateName be ResolvePrivateIdentifier(privEnv, privateIdentifier).
    // 4. Return the Reference Record { [[Base]]: baseValue, [[ReferencedName]]: privateName, [[Strict]]: true, [[ThisValue]]: empty }.
    private_environment.resolve_private_identifier(private_identifier)
}

/// GetValue of a private reference, as Reference::get_value does it.
// 6.2.4.5 GetValue ( V ), https://tc39.es/ecma262/#sec-getvalue
fn get_private_reference_value(vm: &Vm, base_value: Value, private_name: &PrivateName) -> ThrowCompletionOr<Value> {
    // 4. If IsPropertyReference(V) is true, then
    // a. Let baseObj be ? ToObject(V.[[Base]]).
    let base_obj = base_value.to_object(vm)?;

    // b. If IsPrivateReference(V) is true, then
    // i. Return ? PrivateGet(baseObj, V.[[ReferencedName]]).
    base_obj.private_get(vm, private_name)
}

/// PutValue of a private reference, as Reference::put_value does it.
// 6.2.4.6 PutValue ( V, W ), https://tc39.es/ecma262/#sec-putvalue
fn put_private_reference_value(
    vm: &Vm,
    base_value: Value,
    private_name: &PrivateName,
    value: Value,
) -> ThrowCompletionOr<()> {
    // 5. If IsPropertyReference(V) is true, then
    // a. Let baseObj be ? ToObject(V.[[Base]]).
    let base_obj = base_value.to_object(vm)?;

    // b. If IsPrivateReference(V) is true, then
    // i. Return ? PrivateSet(baseObj, V.[[ReferencedName]], W).
    base_obj.private_set(vm, private_name, value)
}

pub fn get_by_id(vm: &Vm, pc: u32, instruction: &op::GetById, values: &mut op::GetByIdValues) -> SlowPathControl {
    let base_value = values.base;
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let property_key = executable.get_property_key(instruction.property);
    let value = asm_try!(
        vm,
        pc,
        property_access::get_by_id(
            vm,
            GetByIdMode::Normal,
            || optional_identifier(vm, &instruction.base_identifier),
            property_key,
            base_value,
            base_value,
            cache,
            CachePropertyAbsence::Yes,
        )
    );
    values.dst = value;
    SlowPathControl::continue_at(pc + op::GetById::LENGTH)
}

pub fn get_by_id_cached_accessor(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetById,
    values: &mut op::GetByIdValues,
) -> SlowPathControl {
    let object = values.base.as_object();
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let entry = cache
        .first_entry()
        .expect("the interpreter found the accessor through the cache");

    let holder = entry.prototype.unwrap_or(object);
    let value = holder.get_direct(entry.property_offset);
    assert!(value.is_accessor());
    let getter = value.as_accessor().getter();
    let result = asm_try!(vm, pc, get_cached_property_value(vm, value, Value::from_object(object)));
    if let Some(getter) = getter
        && function_object_as_object(getter).is_direct_getter_function()
        && let Some(completed_entry) = cache.first_entry_slot()
    {
        let completed = completed_entry.get();
        if completed.shape == Some(object.shape()) {
            let completed_holder = completed.prototype.unwrap_or(object);
            let completed_value = completed_holder.get_direct(completed.property_offset);
            if completed_value.is_accessor() && completed_value.as_accessor().getter() == Some(getter) {
                completed_entry.direct_getter_validated.set(true);
            }
        }
    }
    values.dst = result;
    SlowPathControl::continue_at(pc + op::GetById::LENGTH)
}

pub fn get_by_id_with_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetByIdWithThis,
    values: &mut op::GetByIdWithThisValues,
) -> SlowPathControl {
    let base_value = values.base;
    let this_value = values.this_value;
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let property_key = executable.get_property_key(instruction.property);
    let value = asm_try!(
        vm,
        pc,
        property_access::get_by_id(
            vm,
            GetByIdMode::Normal,
            || None,
            property_key,
            base_value,
            this_value,
            cache,
            CachePropertyAbsence::No,
        )
    );
    values.dst = value;
    SlowPathControl::continue_at(pc + op::GetByIdWithThis::LENGTH)
}

pub fn put_by_id(vm: &Vm, pc: u32, instruction: &op::PutById, values: &mut op::PutByIdValues) -> SlowPathControl {
    let value = values.src;
    let base = values.base;
    let executable = vm.current_executable();
    let property_key = executable.get_property_key(instruction.property);
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    asm_try!(
        vm,
        pc,
        put_by_property_key(
            vm,
            base,
            base,
            value,
            || optional_identifier(vm, &instruction.base_identifier),
            property_key,
            put_kind_from_operand(instruction.kind),
            strict_of(instruction.header.strict),
            Some(cache),
        )
    );
    SlowPathControl::continue_at(pc + op::PutById::LENGTH)
}

pub fn put_by_id_with_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::PutByIdWithThis,
    values: &mut op::PutByIdWithThisValues,
) -> SlowPathControl {
    let value = values.src;
    let base = values.base;
    let executable = vm.current_executable();
    let name = executable.get_property_key(instruction.property);
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    asm_try!(
        vm,
        pc,
        put_by_property_key(
            vm,
            base,
            values.this_value,
            value,
            || None,
            name,
            put_kind_from_operand(instruction.kind),
            strict_of(instruction.header.strict),
            Some(cache),
        )
    );
    SlowPathControl::continue_at(pc + op::PutByIdWithThis::LENGTH)
}

pub fn get_by_value(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetByValue,
    values: &mut op::GetByValueValues,
) -> SlowPathControl {
    let base_value = values.base;
    let property_key_value = values.property;
    let object = asm_try!(
        vm,
        pc,
        base_object_for_get(
            vm,
            base_value,
            || optional_identifier(vm, &instruction.base_identifier),
            &property_key_value,
        )
    );
    let property_key = asm_try!(vm, pc, property_key_value.to_property_key(vm));
    if base_value.is_string() {
        let string_value = asm_try!(vm, pc, base_value.as_string().get(vm, &property_key));
        if let Some(string_value) = string_value {
            values.dst = string_value;
            return SlowPathControl::continue_at(pc + op::GetByValue::LENGTH);
        }
    }
    values.dst = asm_try!(
        vm,
        pc,
        get_by_value_with_keyed_cache(vm, object, base_value, &property_key)
    );
    SlowPathControl::continue_at(pc + op::GetByValue::LENGTH)
}

pub fn get_by_value_with_this(vm: &Vm, pc: u32, values: &mut op::GetByValueWithThisValues) -> SlowPathControl {
    let property_key_value = values.property;
    let object = asm_try!(vm, pc, values.base.to_object(vm));
    let property_key = asm_try!(vm, pc, property_key_value.to_property_key(vm));
    let value = asm_try!(
        vm,
        pc,
        get_by_value_with_keyed_cache(vm, object, values.this_value, &property_key)
    );
    values.dst = value;
    SlowPathControl::continue_at(pc + op::GetByValueWithThis::LENGTH)
}

fn length_property_key(executable: &Executable) -> &PropertyKey {
    executable.get_property_key(
        executable
            .length_identifier
            .expect("an executable that reads a length has the length in its property key table"),
    )
}

pub fn get_length(vm: &Vm, pc: u32, instruction: &op::GetLength, values: &mut op::GetLengthValues) -> SlowPathControl {
    let base_value = values.base;
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let value = asm_try!(
        vm,
        pc,
        property_access::get_by_id(
            vm,
            GetByIdMode::Length,
            || optional_identifier(vm, &instruction.base_identifier),
            length_property_key(&executable),
            base_value,
            base_value,
            cache,
            CachePropertyAbsence::No,
        )
    );
    values.dst = value;
    SlowPathControl::continue_at(pc + op::GetLength::LENGTH)
}

pub fn get_length_with_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetLengthWithThis,
    values: &mut op::GetLengthWithThisValues,
) -> SlowPathControl {
    let base_value = values.base;
    let this_value = values.this_value;
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let value = asm_try!(
        vm,
        pc,
        property_access::get_by_id(
            vm,
            GetByIdMode::Length,
            || None,
            length_property_key(&executable),
            base_value,
            this_value,
            cache,
            CachePropertyAbsence::No,
        )
    );
    values.dst = value;
    SlowPathControl::continue_at(pc + op::GetLengthWithThis::LENGTH)
}

pub fn get_method(vm: &Vm, pc: u32, instruction: &op::GetMethod, values: &mut op::GetMethodValues) -> SlowPathControl {
    let executable = vm.current_executable();
    let property_key = executable.get_property_key(instruction.property);
    let method = asm_try!(vm, pc, values.object.get_method(vm, property_key));
    values.dst = method.map_or(Value::UNDEFINED, Value::from_object);
    SlowPathControl::continue_at(pc + op::GetMethod::LENGTH)
}

pub fn put_by_value(
    vm: &Vm,
    pc: u32,
    instruction: &op::PutByValue,
    values: &mut op::PutByValueValues,
) -> SlowPathControl {
    let value = values.src;
    let base = values.base;
    let property = values.property;
    let property_key = asm_try!(vm, pc, property.to_property_key(vm));
    asm_try!(
        vm,
        pc,
        put_by_property_key(
            vm,
            base,
            base,
            value,
            || optional_identifier(vm, &instruction.base_identifier),
            &property_key,
            put_kind_from_operand(instruction.kind),
            strict_of(instruction.header.strict),
            None,
        )
    );
    SlowPathControl::continue_at(pc + op::PutByValue::LENGTH)
}

pub fn put_by_value_with_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::PutByValueWithThis,
    values: &mut op::PutByValueWithThisValues,
) -> SlowPathControl {
    let value = values.src;
    let base = values.base;
    let this_value = values.this_value;
    let property_key = asm_try!(vm, pc, values.property.to_property_key(vm));
    asm_try!(
        vm,
        pc,
        put_by_property_key(
            vm,
            base,
            this_value,
            value,
            || None,
            &property_key,
            put_kind_from_operand(instruction.kind),
            strict_of(instruction.header.strict),
            None,
        )
    );
    SlowPathControl::continue_at(pc + op::PutByValueWithThis::LENGTH)
}

pub fn put_by_spread(vm: &Vm, pc: u32, values: &mut op::PutBySpreadValues) -> SlowPathControl {
    let value = values.src;
    let base = values.base;

    // a. Let baseObj be ? ToObject(V.[[Base]]).
    let object = asm_try!(vm, pc, base.to_object(vm));

    asm_try!(
        vm,
        pc,
        object.copy_data_properties(vm, value, &MarkedVec::new(vm), &MarkedVec::new(vm))
    );
    SlowPathControl::continue_at(pc + op::PutBySpread::LENGTH)
}

pub fn delete_by_id(
    vm: &Vm,
    pc: u32,
    instruction: &op::DeleteById,
    values: &mut op::DeleteByIdValues,
) -> SlowPathControl {
    let property_key = vm.current_executable().get_property_key(instruction.property).clone();
    let result = asm_try!(
        vm,
        pc,
        Reference::with_base_value(values.base, property_key, None, strict_of(instruction.header.strict)).delete_(vm)
    );
    values.dst = Value::from_bool(result);
    SlowPathControl::continue_at(pc + op::DeleteById::LENGTH)
}

pub fn delete_by_value(
    vm: &Vm,
    pc: u32,
    instruction: &op::DeleteByValue,
    values: &mut op::DeleteByValueValues,
) -> SlowPathControl {
    let property_key = asm_try!(vm, pc, values.property.to_property_key(vm));
    let result = asm_try!(
        vm,
        pc,
        Reference::with_base_value(values.base, property_key, None, strict_of(instruction.header.strict)).delete_(vm)
    );
    values.dst = Value::from_bool(result);
    SlowPathControl::continue_at(pc + op::DeleteByValue::LENGTH)
}

pub fn copy_object_excluding_properties(
    vm: &Vm,
    pc: u32,
    instruction: &op::CopyObjectExcludingProperties,
    values: &mut op::CopyObjectExcludingPropertiesValues,
    excluded_names: &[Value],
) -> SlowPathControl {
    let realm = vm.current_realm().expect("there is a current realm");
    let from_object = values.from_object;
    let to_object = Object::create(vm, realm, Some(realm.object_prototype()));

    let excluded_name_keys = MarkedVec::with_capacity(vm, excluded_names.len());
    for &excluded_name in excluded_names {
        excluded_name_keys.push(asm_try!(vm, pc, excluded_name.to_property_key(vm)));
    }

    asm_try!(
        vm,
        pc,
        to_object.copy_data_properties(vm, from_object, &excluded_name_keys, &MarkedVec::new(vm))
    );
    values.dst = Value::from_object(to_object);
    SlowPathControl::continue_at(pc + instruction.length())
}

pub fn create_data_property_or_throw(
    vm: &Vm,
    pc: u32,
    values: &mut op::CreateDataPropertyOrThrowValues,
) -> SlowPathControl {
    let object = values.object.as_object();
    let property = asm_try!(vm, pc, values.property.to_property_key(vm));
    let value = values.value;
    asm_try!(vm, pc, object.create_data_property_or_throw(vm, &property, value));
    SlowPathControl::continue_at(pc + op::CreateDataPropertyOrThrow::LENGTH)
}

pub fn new_object(vm: &Vm, pc: u32, instruction: &op::NewObject, values: &mut op::NewObjectValues) -> SlowPathControl {
    let realm = vm.current_realm().expect("there is a current realm");

    if instruction.cache != u32::MAX {
        let executable = vm.current_executable();
        let cache = executable.object_shape_cache(instruction.cache);
        if let Some(cached_shape) = cache.shape.get() {
            values.dst = Value::from_object(Object::create_with_premade_shape(vm, cached_shape));
            return SlowPathControl::continue_at(pc + op::NewObject::LENGTH);
        }
    }

    values.dst = Value::from_object(Object::create(vm, realm, Some(realm.object_prototype())));
    SlowPathControl::continue_at(pc + op::NewObject::LENGTH)
}

pub fn new_object_with_no_prototype(
    vm: &Vm,
    pc: u32,
    values: &mut op::NewObjectWithNoPrototypeValues,
) -> SlowPathControl {
    let realm = vm.current_realm().expect("there is a current realm");
    values.dst = Value::from_object(Object::create(vm, realm, None));
    SlowPathControl::continue_at(pc + op::NewObjectWithNoPrototype::LENGTH)
}

pub fn cache_object_shape(
    vm: &Vm,
    pc: u32,
    instruction: &op::CacheObjectShape,
    values: &mut op::CacheObjectShapeValues,
) -> SlowPathControl {
    let executable = vm.current_executable();
    let cache = executable.object_shape_cache(instruction.cache);
    if cache.shape.get().is_none() {
        let object = values.object.as_object();
        if !object.shape().is_dictionary() {
            cache.shape.set(Some(object.shape()));
        }
    }
    SlowPathControl::continue_at(pc + op::CacheObjectShape::LENGTH)
}

pub fn init_object_literal_property(
    vm: &Vm,
    pc: u32,
    instruction: &op::InitObjectLiteralProperty,
    values: &mut op::InitObjectLiteralPropertyValues,
) -> SlowPathControl {
    let object = values.object.as_object();
    let value = values.src;
    let executable = vm.current_executable();
    let cache = executable.object_shape_cache(instruction.shape_cache_index);
    let property_slot = instruction.property_slot as usize;

    let cached_shape = cache.shape.get();
    let cached_property_offset = cache.property_offsets.borrow().get(property_slot).copied();
    if let Some(cached_shape) = cached_shape
        && object.shape() == cached_shape
        && let Some(property_offset) = cached_property_offset
    {
        object.put_direct(property_offset, value);
        return SlowPathControl::continue_at(pc + op::InitObjectLiteralProperty::LENGTH);
    }

    let property_key = executable.get_property_key(instruction.property);
    object.define_direct_property(
        vm,
        property_key,
        value,
        PropertyAttributes::new(Attribute::ENUMERABLE | Attribute::WRITABLE | Attribute::CONFIGURABLE),
    );

    if !object.shape().is_dictionary()
        && let Some(metadata) = object.shape().lookup(property_key)
    {
        let mut property_offsets = cache.property_offsets.borrow_mut();
        if property_slot >= property_offsets.len() {
            property_offsets.resize(property_slot + 1, 0);
        }
        property_offsets[property_slot] = metadata.offset;
    }

    SlowPathControl::continue_at(pc + op::InitObjectLiteralProperty::LENGTH)
}

pub fn has_private_id(
    vm: &Vm,
    pc: u32,
    instruction: &op::HasPrivateId,
    values: &mut op::HasPrivateIdValues,
) -> SlowPathControl {
    let base = values.base;
    if !base.is_object() {
        return throw_type_error(vm, pc, ErrorType::InOperatorWithObject);
    }

    let private_environment = running_private_environment(vm);
    let private_name =
        private_environment.resolve_private_identifier(vm.current_executable().get_identifier(instruction.property));
    values.dst = Value::from_bool(base.as_object().private_element_find(&private_name).is_some());
    SlowPathControl::continue_at(pc + op::HasPrivateId::LENGTH)
}

pub fn add_private_name(vm: &Vm, pc: u32, instruction: &op::AddPrivateName) -> SlowPathControl {
    let name = vm.current_executable().get_identifier(instruction.name).clone();
    running_private_environment(vm).add_private_name(name);
    SlowPathControl::continue_at(pc + op::AddPrivateName::LENGTH)
}

// Direct handler for GetPrivateById: bypasses Reference indirection.
pub fn get_private_by_id(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetPrivateById,
    values: &mut op::GetPrivateByIdValues,
) -> SlowPathControl {
    let base_value = values.base;

    if !base_value.is_object() {
        asm_try!(vm, pc, base_value.to_object(vm));
        let private_name = make_private_reference(vm, vm.current_executable().get_identifier(instruction.property));
        let result = asm_try!(vm, pc, get_private_reference_value(vm, base_value, &private_name));
        values.dst = result;
        return SlowPathControl::continue_at(pc + op::GetPrivateById::LENGTH);
    }

    let private_environment = running_private_environment(vm);
    let private_name =
        private_environment.resolve_private_identifier(vm.current_executable().get_identifier(instruction.property));
    let result = asm_try!(vm, pc, base_value.as_object().private_get(vm, &private_name));
    values.dst = result;
    SlowPathControl::continue_at(pc + op::GetPrivateById::LENGTH)
}

// Direct handler for PutPrivateById: bypasses Reference indirection.
pub fn put_private_by_id(
    vm: &Vm,
    pc: u32,
    instruction: &op::PutPrivateById,
    values: &mut op::PutPrivateByIdValues,
) -> SlowPathControl {
    let base_value = values.base;
    let value = values.src;

    if !base_value.is_object() {
        let object = asm_try!(vm, pc, base_value.to_object(vm));
        let private_name = make_private_reference(vm, vm.current_executable().get_identifier(instruction.property));
        asm_try!(
            vm,
            pc,
            put_private_reference_value(vm, Value::from_object(object), &private_name, value)
        );
        return SlowPathControl::continue_at(pc + op::PutPrivateById::LENGTH);
    }

    let private_environment = running_private_environment(vm);
    let private_name =
        private_environment.resolve_private_identifier(vm.current_executable().get_identifier(instruction.property));
    asm_try!(vm, pc, base_value.as_object().private_set(vm, &private_name, value));
    SlowPathControl::continue_at(pc + op::PutPrivateById::LENGTH)
}

/// What a fast for-in snapshot is built from, as FastPropertyNameIteratorData in SlowPaths.cpp.
struct FastPropertyNameIteratorData<'vm> {
    properties: MarkedVec<'vm, PropertyKey>,
    fast_path: ObjectPropertyIteratorFastPath,
    indexed_property_count: u32,
    receiver_has_magical_length_property: bool,
    shape: Gc<Shape>,
    prototype_chain_validity: Option<Gc<PrototypeChainValidity>>,
}

fn shape_has_enumerable_string_property(shape: &Shape) -> bool {
    let mut has_enumerable_string_property = false;
    shape.for_each_property_in_insertion_order(|property_key, metadata| {
        if property_key.is_string() && metadata.attributes.is_enumerable() {
            has_enumerable_string_property = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    has_enumerable_string_property
}

fn property_name_iterator_fast_path_is_still_eligible(
    object: Gc<Object>,
    fast_path: ObjectPropertyIteratorFastPath,
    indexed_property_count: u32,
) -> bool {
    let mut object_to_check = Some(object);
    let mut is_receiver = true;

    while let Some(current) = object_to_check {
        if !current.eligible_for_own_property_enumeration_fast_path() {
            return false;
        }

        if is_receiver {
            if fast_path == ObjectPropertyIteratorFastPath::PackedIndexed {
                if current.indexed_storage_kind() != IndexedStorageKind::Packed {
                    return false;
                }
                if current.indexed_array_like_size() != indexed_property_count {
                    return false;
                }
            } else if current.indexed_array_like_size() != 0 {
                return false;
            }
        } else if current.indexed_array_like_size() != 0 {
            return false;
        }

        object_to_check = current.prototype();
        is_receiver = false;
    }

    true
}

fn object_property_iterator_cache_matches(object: Gc<Object>, cache: &ObjectPropertyIteratorCacheData) -> bool {
    // A cache entry represents the fully flattened key snapshot for one bytecode
    // site. Reusing it is only valid while the receiver still has the same local
    // state and the prototype chain validity token says nothing above it changed.
    if object.has_magical_length_property() != cache.receiver_has_magical_length_property() {
        return false;
    }

    let shape = object.shape();
    if Some(shape) != cache.shape() {
        return false;
    }

    if shape.is_dictionary() && shape.dictionary_generation() != cache.shape_dictionary_generation() {
        return false;
    }

    if cache
        .prototype_chain_validity()
        .is_some_and(|validity| !validity.is_valid())
    {
        return false;
    }

    property_name_iterator_fast_path_is_still_eligible(object, cache.fast_path(), cache.indexed_property_count())
}

/// The objects a walk up a prototype chain has visited, kept alive so that their addresses stay theirs.
struct SeenObjects<'vm> {
    objects: MarkedVec<'vm, Gc<Object>>,
    addresses: HashSet<usize>,
}

impl<'vm> SeenObjects<'vm> {
    fn new(vm: &'vm Vm) -> Self {
        Self {
            objects: MarkedVec::new(vm),
            addresses: HashSet::new(),
        }
    }

    fn contains(&self, object: Gc<Object>) -> bool {
        self.addresses.contains(&object.as_ptr().addr())
    }

    fn set(&mut self, object: Gc<Object>) {
        if self.addresses.insert(object.as_ptr().addr()) {
            self.objects.push(object);
        }
    }

    fn clear(&mut self) {
        self.addresses.clear();
        while self.objects.pop().is_some() {}
    }
}

/// The keys a for-in has already decided about while it walks up the prototype chain. The keys are never symbols,
/// so a plain set may hold them.
struct ShadowingState {
    seen_non_enumerable_properties: HashSet<PropertyKey>,
    seen_properties: Option<HashSet<PropertyKey>>,
}

impl ShadowingState {
    fn new() -> Self {
        Self {
            seen_non_enumerable_properties: HashSet::new(),
            seen_properties: None,
        }
    }

    fn ensure_seen_properties(&mut self, properties: &MarkedVec<'_, PropertyKey>) -> &mut HashSet<PropertyKey> {
        self.seen_properties.get_or_insert_with(|| {
            // Prototype shadowing ignores enumerability, so once we start looking
            // above the receiver we need an explicit visited set for names we have
            // already decided to expose from lower objects.
            let mut seen_properties = HashSet::with_capacity(properties.len());
            for index in 0..properties.len() {
                seen_properties.insert(properties.get(index).expect("the index is in bounds"));
            }
            seen_properties
        })
    }

    /// Decides about one key of an object in the chain, appending it to `properties` if the for-in visits it.
    fn visit(
        &mut self,
        properties: &MarkedVec<'_, PropertyKey>,
        property_key: &PropertyKey,
        enumerable: bool,
        in_prototype_chain: bool,
    ) {
        if !enumerable {
            self.seen_non_enumerable_properties.insert(property_key.clone());
        }
        if in_prototype_chain && enumerable {
            if self.seen_non_enumerable_properties.contains(property_key) {
                return;
            }
            if self.ensure_seen_properties(properties).contains(property_key) {
                return;
            }
        }
        if enumerable {
            properties.push(property_key.clone());
        }
        if let Some(seen_properties) = &mut self.seen_properties {
            seen_properties.insert(property_key.clone());
        }
    }
}

fn try_get_fast_property_name_iterator_data(
    vm: &Vm,
    object: Gc<Object>,
) -> ThrowCompletionOr<Option<FastPropertyNameIteratorData<'_>>> {
    let mut fast_path = ObjectPropertyIteratorFastPath::PlainNamed;
    let mut indexed_property_count = 0;
    let receiver_has_magical_length_property = object.has_magical_length_property();
    let shape = object.shape();

    let mut seen_objects = SeenObjects::new(vm);
    let mut estimated_properties_count = 0usize;
    let mut prototype_chain_has_enumerable_named_properties = false;
    let mut object_to_check = Some(object);
    while let Some(current) = object_to_check
        && !seen_objects.contains(current)
    {
        seen_objects.set(current);
        if !current.eligible_for_own_property_enumeration_fast_path() {
            return Ok(None);
        }
        if current == object {
            if current.indexed_array_like_size() != 0 {
                if current.indexed_storage_kind() != IndexedStorageKind::Packed {
                    return Ok(None);
                }
                fast_path = ObjectPropertyIteratorFastPath::PackedIndexed;
                indexed_property_count = current.indexed_array_like_size();
            } else {
                fast_path = ObjectPropertyIteratorFastPath::PlainNamed;
            }
        } else if current.indexed_array_like_size() != 0 {
            // The fast path only knows how to synthesize a packed indexed prefix
            // for the receiver itself. As soon as indexed properties appear in
            // the prototype chain, we fall back to the generic enumeration path.
            return Ok(None);
        } else if !prototype_chain_has_enumerable_named_properties {
            prototype_chain_has_enumerable_named_properties = shape_has_enumerable_string_property(&current.shape());
        }
        estimated_properties_count += current.shape().property_count() as usize;
        object_to_check = current.internal_get_prototype_of(vm)?;
    }
    seen_objects.clear();

    let mut prototype_chain_validity = None;
    if let Some(prototype) = object.shape().prototype() {
        prototype_chain_validity = prototype.shape().prototype_chain_validity();
        if prototype_chain_validity.is_none() {
            return Ok(None);
        }
    }

    let make_result = |properties| FastPropertyNameIteratorData {
        properties,
        fast_path,
        indexed_property_count,
        receiver_has_magical_length_property,
        shape,
        prototype_chain_validity,
    };

    if !prototype_chain_has_enumerable_named_properties {
        // Common case: only the receiver contributes enumerable string keys, so
        // we can copy them straight from the shape without any shadowing work.
        let properties = MarkedVec::with_capacity(vm, object.shape().property_count() as usize);
        object
            .shape()
            .for_each_property_in_insertion_order(|property_key, metadata| {
                if property_key.is_string() && metadata.attributes.is_enumerable() {
                    properties.push(property_key.clone());
                }
                ControlFlow::Continue(())
            });
        return Ok(Some(make_result(properties)));
    }

    let properties = MarkedVec::with_capacity(vm, estimated_properties_count);
    let mut shadowing = ShadowingState::new();

    let mut in_prototype_chain = false;
    let mut object_to_check = Some(object);
    while let Some(current) = object_to_check
        && !seen_objects.contains(current)
    {
        seen_objects.set(current);

        // Arrays keep a non-enumerable magical `length` property outside the shape
        // table, but it still shadows enumerable `length` properties higher up the
        // prototype chain during for-in.
        if current.has_magical_length_property() {
            shadowing.seen_non_enumerable_properties.insert(vm.names.length.clone());
        }

        current
            .shape()
            .for_each_property_in_insertion_order(|property_key, metadata| {
                if !property_key.is_string() {
                    return ControlFlow::Continue(());
                }
                shadowing.visit(
                    &properties,
                    property_key,
                    metadata.attributes.is_enumerable(),
                    in_prototype_chain,
                );
                ControlFlow::Continue(())
            });
        in_prototype_chain = true;
        object_to_check = current.internal_get_prototype_of(vm)?;
    }

    Ok(Some(make_result(properties)))
}

// 14.7.5.9 EnumerateObjectProperties ( O ), https://tc39.es/ecma262/#sec-enumerate-object-properties
fn get_object_property_iterator_impl(
    vm: &Vm,
    object: Gc<Object>,
    cache: Option<&ObjectPropertyIteratorCache>,
) -> ThrowCompletionOr<Gc<ObjectPropertyIteratorCacheData>> {
    // While the spec does provide an algorithm, it allows us to implement it ourselves so long as we meet the following invariants:
    //    1- Returned property keys do not include keys that are Symbols
    //    2- Properties of the target object may be deleted during enumeration. A property that is deleted before it is processed by the iterator's next method is ignored
    //    3- If new properties are added to the target object during enumeration, the newly added properties are not guaranteed to be processed in the active enumeration
    //    4- A property name will be returned by the iterator's next method at most once in any enumeration.
    //    5- Enumerating the properties of the target object includes enumerating properties of its prototype, and the prototype of the prototype, and so on, recursively;
    //       but a property of a prototype is not processed if it has the same name as a property that has already been processed by the iterator's next method.
    //    6- The values of [[Enumerable]] attributes are not considered when determining if a property of a prototype object has already been processed.
    //    7- The enumerable property names of prototype objects must be obtained by invoking EnumerateObjectProperties passing the prototype object as the argument.
    //    8- EnumerateObjectProperties must obtain the own property keys of the target object by calling its [[OwnPropertyKeys]] internal method.
    //    9- Property attributes of the target object must be obtained by calling its [[GetOwnProperty]] internal method

    // Invariant 3 effectively allows the implementation to ignore newly added keys, and we do so (similar to other implementations).
    // Note: While the spec doesn't explicitly require these to be ordered, it says that the values should be retrieved via OwnPropertyKeys,
    //       so we just keep the order consistent anyway.

    if let Some(cache) = cache
        && let Some(data) = cache.data.get()
    {
        // The flattened key snapshot for this site is still valid, so reuse it as-is. The per-loop
        // iteration state (the cursor) lives in a bytecode register, not in this cell.
        if object_property_iterator_cache_matches(object, &data) {
            return Ok(data);
        }
    }

    // Keep a snapshot on the shape so sites that alternate between shapes can reuse
    // previously collected keys instead of rebuilding the list each time.
    if let Some(shape_cache) = object.shape().property_iterator_cache()
        && object_property_iterator_cache_matches(object, &shape_cache)
    {
        if let Some(cache) = cache {
            cache.data.set(Some(shape_cache));
        }
        return Ok(shape_cache);
    }

    if let Some(fast_iterator_data) = try_get_fast_property_name_iterator_data(vm, object)? {
        let cache_data = ObjectPropertyIteratorCacheData::create_with_fast_path(
            vm,
            &fast_iterator_data.properties,
            fast_iterator_data.fast_path,
            fast_iterator_data.indexed_property_count,
            fast_iterator_data.receiver_has_magical_length_property,
            fast_iterator_data.shape,
            fast_iterator_data.prototype_chain_validity,
        );
        if let Some(cache) = cache {
            cache.data.set(Some(cache_data));
        }
        object.shape().set_property_iterator_cache(cache_data);
        return Ok(cache_data);
    }

    let mut estimated_properties_count = 0;
    let mut seen_objects = SeenObjects::new(vm);
    let mut object_to_check = Some(object);
    while let Some(current) = object_to_check
        && !seen_objects.contains(current)
    {
        seen_objects.set(current);
        estimated_properties_count += current.own_properties_count();
        object_to_check = current.internal_get_prototype_of(vm)?;
    }
    seen_objects.clear();

    let properties = MarkedVec::with_capacity(vm, estimated_properties_count);
    let mut shadowing = ShadowingState::new();

    // Collect all keys immediately (invariant no. 5)
    let mut in_prototype_chain = false;
    let mut object_to_check = Some(object);
    while let Some(current) = object_to_check
        && !seen_objects.contains(current)
    {
        seen_objects.set(current);
        current.for_each_own_property_with_enumerability(vm, |property_key, enumerable| {
            shadowing.visit(&properties, property_key, enumerable, in_prototype_chain);
            Ok(())
        })?;
        in_prototype_chain = true;
        object_to_check = current.internal_get_prototype_of(vm)?;
    }

    // A slow-path snapshot has no fast path to revalidate; enumeration filters deleted keys with
    // has_property() at each step. It is not cached on the site, because the key set depends on the
    // receiver rather than only its shape.
    Ok(ObjectPropertyIteratorCacheData::create(vm, &properties))
}

pub fn get_object_property_iterator(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetObjectPropertyIterator,
    values: &mut op::GetObjectPropertyIteratorValues,
) -> SlowPathControl {
    let executable = vm.current_executable();
    let cache = executable.object_property_iterator_cache(instruction.cache);
    // ToObject the enumeration source once here. The boxed receiver, not the raw source value, is
    // what ObjectPropertyIteratorNext revalidates against and calls has_property() on.
    let receiver = asm_try!(vm, pc, values.object.to_object(vm));
    let keys = asm_try!(vm, pc, get_object_property_iterator_impl(vm, receiver, Some(cache)));
    values.dst_keys = Value::with_cell_tag(nan_box::IS_CELL_BIT, keys);
    values.dst_receiver = Value::from_object(receiver);
    SlowPathControl::continue_at(pc + op::GetObjectPropertyIterator::LENGTH)
}

// Advance a for-in enumeration by one step. The receiver, its flattened key snapshot, and the cursor
// are all passed in explicitly; there is no iterator object. This is the slow companion to the flap
// fast path, reached once the snapshot's shape guards no longer hold (or never held, for a snapshot
// with no fast path), so it filters every remaining key with has_property() the way the spec's
// deleted-property invariant requires.
fn object_property_iterator_next_step(
    vm: &Vm,
    receiver: Gc<Object>,
    keys: Gc<ObjectPropertyIteratorCacheData>,
    cursor: &mut usize,
    done: &mut bool,
    value: &mut Value,
) -> ThrowCompletionOr<()> {
    let indexed_count = keys.indexed_property_count() as usize;
    let total = indexed_count
        .checked_add(keys.property_count())
        .expect("the key count fits in usize");

    while *cursor < total {
        let current = *cursor;
        *cursor += 1;
        let entry = if current < indexed_count {
            PropertyKey::from(current as u32)
        } else {
            keys.property(current - indexed_count)
        };

        // Invariant 2: a property deleted before the iterator reaches it is skipped.
        if !receiver.has_property(vm, &entry)? {
            continue;
        }

        *done = false;
        *value = entry.to_value(vm);
        return Ok(());
    }

    *done = true;
    Ok(())
}

/// The key snapshot GetObjectPropertyIterator stored in a register, as a value holding a cell.
fn object_property_iterator_cache_data_of(value: Value) -> Gc<ObjectPropertyIteratorCacheData> {
    assert!(value.tag() == nan_box::IS_CELL_BIT);
    // SAFETY: The tag says the value holds a cell, whose class is checked next.
    let cell = unsafe { value.cell::<ObjectPropertyIteratorCacheData>() };
    assert!(class_of(cell).is_subclass_of(ObjectPropertyIteratorCacheData::CLASS));
    cell
}

pub fn object_property_iterator_next(
    vm: &Vm,
    pc: u32,
    values: &mut op::ObjectPropertyIteratorNextValues,
) -> SlowPathControl {
    let receiver = values.receiver.as_object();
    let keys = object_property_iterator_cache_data_of(values.keys);
    // The cursor is a Number that only for-in codegen and this op ever write: an int32 while it fits,
    // and a double once it does not. The fast path only handles the int32 case, so a snapshot larger
    // than the int32 range finishes here. Value(double) narrows back to int32 whenever possible.
    assert!(values.cursor.is_integral_number());
    let cursor_number = values.cursor.as_f64();
    assert!(cursor_number >= 0.0 && cursor_number <= usize::MAX as f64);
    let mut cursor = cursor_number as usize;
    let mut value = Value::UNDEFINED;
    let mut done = false;
    asm_try!(
        vm,
        pc,
        object_property_iterator_next_step(vm, receiver, keys, &mut cursor, &mut done, &mut value)
    );
    values.dst_done = Value::from_bool(done);
    values.dst_value = value;
    values.cursor = Value::from_f64(cursor as f64);
    SlowPathControl::continue_at(pc + op::ObjectPropertyIteratorNext::LENGTH)
}

pub fn try_put_by_value_holey_array(values: &op::PutByValueValues) -> bool {
    let base = values.base;
    if !base.is_object() {
        return false;
    }

    let property = values.property;
    if !is_non_negative_int32(property) {
        return false;
    }

    let object = base.as_object();
    let Some(array) = object.downcast::<Array>() else {
        return false;
    };

    if array.is_proxy_target()
        || !array.default_prototype_chain_intact()
        || !array.extensible()
        || array.may_interfere_with_indexed_property_access()
        || array.indexed_storage_kind() != IndexedStorageKind::Holey
    {
        return false;
    }

    let index = property.as_i32() as u32;
    if index >= array.indexed_array_like_size() {
        return false;
    }

    array.indexed_put(index, values.src, DEFAULT_ATTRIBUTES);
    true
}

pub fn try_inline_get_by_id_accessor(vm: &Vm, pc: u32, instruction: &op::GetById, values: &op::GetByIdValues) -> bool {
    let object = values.base.as_object();
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);
    let entry = cache
        .first_entry()
        .expect("the interpreter found the accessor through the cache");

    let holder = entry.prototype.unwrap_or(object);
    let value = holder.get_direct(entry.property_offset);
    assert!(value.is_accessor());

    let Some(getter) = value.as_accessor().getter() else {
        return false;
    };
    let Some(getter_function) = as_ecmascript_function_object(getter) else {
        return false;
    };

    if !getter_function.can_inline_call() {
        return false;
    }

    vm.push_inline_frame(
        getter_function,
        getter_function.inline_call_executable(),
        &[],
        pc + instruction.length(),
        instruction.dst.0,
        Value::from_object(object),
        None,
        false,
    )
    .is_some()
}

// Fast cache-only PutById. Tries all cache entries for ChangeOwnProperty and
// AddOwnProperty. Returns whether the cache handled the put; if not, the caller
// uses the full slow path.
pub fn try_put_by_id_cache(vm: &Vm, instruction: &op::PutById, values: &op::PutByIdValues) -> bool {
    let base = values.base;
    if !base.is_object() {
        return false;
    }
    let object = base.as_object();
    let value = values.src;
    let executable = vm.current_executable();
    let cache = executable.property_lookup_cache(instruction.cache as usize);

    for entry in cache.entry_slots_for_shape(object.shape()) {
        match entry.entry_type.get() {
            PropertyLookupCacheEntryType::ChangeOwnProperty => {
                let Some(cached_shape) = entry.shape.get() else {
                    continue;
                };
                if cached_shape != object.shape() {
                    continue;
                }
                if cached_shape.is_dictionary()
                    && cached_shape.dictionary_generation() != entry.shape_dictionary_generation.get()
                {
                    continue;
                }
                let current = object.get_direct(entry.property_offset.get());
                if current.is_accessor() || !entry.writes_data_property.get() {
                    return false;
                }
                object.put_direct(entry.property_offset.get(), value);
                return true;
            }
            PropertyLookupCacheEntryType::AddOwnProperty => {
                if entry.from_shape.get() != Some(object.shape()) {
                    continue;
                }
                if !object_can_cache_property_additions(&object) {
                    continue;
                }
                if object.has_magical_length_property()
                    && !property_addition_is_cacheable(vm, &object, executable.get_property_key(instruction.property))
                {
                    continue;
                }
                let Some(cached_shape) = entry.shape.get() else {
                    continue;
                };
                if !object.extensible() {
                    continue;
                }
                if cached_shape.is_dictionary()
                    && object.shape().dictionary_generation() != entry.shape_dictionary_generation.get()
                {
                    continue;
                }
                if entry
                    .prototype_chain_validity
                    .get()
                    .is_some_and(|validity| !validity.is_valid())
                {
                    continue;
                }
                object.unsafe_set_shape(cached_shape);
                object.put_direct(entry.property_offset.get(), value);
                return true;
            }
            _ => continue,
        }
    }
    false
}

// Fast cache-only GetById. Tries all cache entries for own-property and prototype
// chain lookups. Returns the cached value on hit, or Empty on miss.
pub fn try_get_by_id_cache(base: Value, cache: &PropertyLookupCache) -> Value {
    if !base.is_object() {
        return Value::EMPTY;
    }
    let object = base.as_object();
    let shape = object.shape();

    for entry in cache.entry_slots_for_shape(shape) {
        let entry_type = entry.entry_type.get();
        if entry_type == PropertyLookupCacheEntryType::GetMissingProperty {
            if !object.is_cacheable_for_property_absence() {
                continue;
            }
            if Some(shape) != entry.shape.get() {
                continue;
            }
            if shape.is_dictionary() && shape.dictionary_generation() != entry.shape_dictionary_generation.get() {
                continue;
            }
            if shape.prototype().is_some()
                && !entry
                    .prototype_chain_validity
                    .get()
                    .is_some_and(|validity| validity.is_valid())
            {
                continue;
            }
            return Value::UNDEFINED;
        }

        if entry_type != PropertyLookupCacheEntryType::GetOwnProperty
            && entry_type != PropertyLookupCacheEntryType::GetPropertyInPrototypeChain
        {
            continue;
        }

        if let Some(cached_prototype) = entry.prototype.get() {
            if Some(shape) != entry.shape.get() {
                continue;
            }
            if shape.is_dictionary() && shape.dictionary_generation() != entry.shape_dictionary_generation.get() {
                continue;
            }
            if !entry
                .prototype_chain_validity
                .get()
                .is_some_and(|validity| validity.is_valid())
            {
                continue;
            }
            let value = cached_prototype.get_direct(entry.property_offset.get());
            if value.is_accessor() {
                return Value::EMPTY;
            }
            return value;
        } else if Some(shape) == entry.shape.get() {
            if shape.is_dictionary() && shape.dictionary_generation() != entry.shape_dictionary_generation.get() {
                continue;
            }
            let value = object.get_direct(entry.property_offset.get());
            if value.is_accessor() {
                return Value::EMPTY;
            }
            return value;
        }
    }
    Value::EMPTY
}

// Fast path for GetByValue on typed arrays.
// Returns whether it stored the result in dst; if not, the caller falls to the slow path.
pub fn try_get_by_value_typed_array(vm: &Vm, values: &mut op::GetByValueValues) -> bool {
    let base = values.base;
    if !base.is_object() {
        return false;
    }

    let property = values.property;
    if !is_non_negative_int32(property) {
        return false;
    }

    let object = base.as_object();
    if !object.is_typed_array() {
        return false;
    }

    let typed_array = typed_array_of_object(&object);
    let index = property.as_i32() as u32;

    // Fast path: fixed-length typed array with cached data pointer
    let array_length = typed_array.array_length();
    if array_length.is_auto() {
        return false;
    }

    let length = array_length.length();
    if index >= length {
        values.dst = Value::UNDEFINED;
        return true;
    }

    if !is_valid_integer_index(typed_array, CanonicalIndex::new(CanonicalIndexType::Index, index)) {
        values.dst = Value::UNDEFINED;
        return true;
    }

    let buffer = typed_array.viewed_array_buffer();
    let Some(byte_index) = (index as usize)
        .checked_mul(typed_array.element_size() as usize)
        .and_then(|byte_index| byte_index.checked_add(typed_array.byte_offset() as usize))
    else {
        return false;
    };

    let element_type = match typed_array.kind() {
        Kind::Uint8Array | Kind::Uint8ClampedArray => ElementType::Uint8,
        Kind::Int8Array => ElementType::Int8,
        Kind::Uint16Array => ElementType::Uint16,
        Kind::Int16Array => ElementType::Int16,
        Kind::Uint32Array => ElementType::Uint32,
        Kind::Int32Array => ElementType::Int32,
        Kind::Float32Array => ElementType::Float32,
        Kind::Float64Array => ElementType::Float64,
        _ => return false,
    };

    values.dst = buffer.get_value(vm, byte_index, element_type, true, Order::Unordered, true);
    true
}

// Fast path for PutByValue on typed arrays.
// Returns whether it stored the value; if not, the caller falls to the slow path.
pub fn try_put_by_value_typed_array(vm: &Vm, values: &op::PutByValueValues) -> bool {
    let base = values.base;
    if !base.is_object() {
        return false;
    }

    let property = values.property;
    if !is_non_negative_int32(property) {
        return false;
    }

    let object = base.as_object();
    if !object.is_typed_array() {
        return false;
    }

    let typed_array = typed_array_of_object(&object);
    let index = property.as_i32() as u32;

    let array_length = typed_array.array_length();
    if array_length.is_auto() {
        return false;
    }

    // NB: An out-of-bounds write is not simply a no-op: TypedArraySetElement still
    //     evaluates ToNumber(value) for its side effects before discarding the store.
    //     Fall back to the slow path so those side effects happen.
    if index >= array_length.length() {
        return false;
    }

    if !is_valid_integer_index(typed_array, CanonicalIndex::new(CanonicalIndexType::Index, index)) {
        return false;
    }

    let buffer = typed_array.viewed_array_buffer();
    let Some(byte_index) = (index as usize)
        .checked_mul(typed_array.element_size() as usize)
        .and_then(|byte_index| byte_index.checked_add(typed_array.byte_offset() as usize))
    else {
        return false;
    };
    let value = values.src;

    if value.is_int32() {
        let int_value = value.as_i32();
        let (element_type, value) = match typed_array.kind() {
            Kind::Uint8Array => (ElementType::Uint8, value),
            Kind::Uint8ClampedArray => (ElementType::Uint8Clamped, Value::from_i32(int_value.clamp(0, 255))),
            Kind::Int8Array => (ElementType::Int8, value),
            Kind::Uint16Array => (ElementType::Uint16, value),
            Kind::Int16Array => (ElementType::Int16, value),
            Kind::Uint32Array => (ElementType::Uint32, value),
            Kind::Int32Array => (ElementType::Int32, value),
            _ => return false,
        };
        buffer.set_value(vm, byte_index, element_type, value, true, Order::Unordered, true);
        return true;
    }

    if value.is_double() {
        let double_value = value.as_f64();
        let element_type = match typed_array.kind() {
            Kind::Float32Array => ElementType::Float32,
            Kind::Float64Array => ElementType::Float64,
            _ => return false,
        };
        buffer.set_value(
            vm,
            byte_index,
            element_type,
            Value::from_f64(double_value),
            true,
            Order::Unordered,
            true,
        );
        return true;
    }

    false
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::bytecode::executable::{
        Executable, ExecutableCacheCounts, KeyedPropertyLookupCache, PROPERTY_LOOKUP_CACHE_DATA_TAG_MASK,
    };
    use crate::bytecode::operand::{InstructionHeader, Operand, PropertyKeyTableIndex};
    use crate::runtime::abstract_operations::new_private_environment;
    use crate::runtime::completion::Must;
    use crate::runtime::object::{PrivateElement, PrivateElementKind, ShouldThrowExceptions};
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::runtime::property_descriptor::PropertyDescriptor;
    use crate::runtime::realm::test_realm::{TestRealm, enumerable_keys, key, thrown_message};
    use crate::runtime::symbol::{Kind, Symbol};

    use crate::utf16::Utf16View;

    fn int(value: i32) -> Value {
        Value::from_i32(value)
    }

    fn header(strict: bool) -> InstructionHeader {
        InstructionHeader { opcode: 0, strict }
    }

    fn optional_identifier_index(index: Option<u32>) -> OptionalIndex<IdentifierTableIndex> {
        // SAFETY: An OptionalIndex is a transparent u32, with all bits set for no index.
        unsafe { core::mem::transmute::<u32, OptionalIndex<IdentifierTableIndex>>(index.unwrap_or(u32::MAX)) }
    }

    fn no_identifier() -> OptionalIndex<IdentifierTableIndex> {
        optional_identifier_index(None)
    }

    fn string_of(value: Value) -> String {
        Utf16View::of_string(&value.to_utf16_string_without_side_effects()).to_utf8()
    }

    /// Runs `executable` as a script of the test realm whose `this` is `this_object`, which lets scripts keep their
    /// state in properties: bindings need the global environment, which the realm does not have yet.
    fn run_executable(vm: &Vm, test_realm: &TestRealm, this_object: Gc<Object>, executable: Gc<Executable>) -> Value {
        let stack = vm.interpreter_stack();
        let mark = stack.top.get();
        let constant_count = u32::try_from(executable.constants().len()).expect("the constant count fits in u32");
        let context = stack
            .allocate(executable.registers_and_locals_count(), constant_count, 0)
            .expect("the interpreter stack has room");
        // SAFETY: The context was just allocated.
        let context_ref = unsafe { context.as_ref() };
        context_ref.realm.set(Some(test_realm.realm));
        context_ref.this_value.set(Value::from_object(this_object));
        vm.push_execution_context(context);
        let result = vm.run_executable(context, executable, 0);
        vm.pop_execution_context();
        stack.deallocate(mark);
        result.unwrap_or_else(|exception| panic!("the script threw {}", string_of(exception)))
    }

    fn compile(vm: &Vm, source: &str) -> Gc<Executable> {
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let parsed = libjs_rust::compile::parse(&code_units, libjs_rust::ast::ProgramType::Script, 1);
        assert!(!parsed.has_errors(), "the script parses");
        let compiled = libjs_rust::compile::compile_script(parsed, code_units.len());
        assert!(
            compiled.declarations.var_names.is_empty() && compiled.declarations.lexical_names.is_empty(),
            "test scripts keep their state in this"
        );
        Executable::create(vm, compiled.executable)
    }

    fn run(vm: &Vm, test_realm: &TestRealm, this_object: Gc<Object>, source: &str) -> Value {
        let executable = compile(vm, source);
        run_executable(vm, test_realm, this_object, executable)
    }

    fn collected_values(vm: &Vm, this_object: Gc<Object>) -> String {
        let collected = this_object.get(vm, &key("r")).must().as_object();
        let count = this_object.get(vm, &key("n")).must().as_i32();
        (0..count)
            .map(|index| string_of(collected.get(vm, &PropertyKey::from(index as u32)).must()))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Runs a case the way the C++ oracle ran it: the case collects values in this.r[0..this.n], which come back
    /// joined with commas.
    fn run_case(vm: &Vm, test_realm: &TestRealm, case: &str) -> String {
        let this_object = test_realm.object();
        run(
            vm,
            test_realm,
            this_object,
            &format!("this.r = {{}}; this.n = 0; {case}"),
        );
        collected_values(vm, this_object)
    }

    /// Each case with what Build/release/bin/js prints for
    /// `this.r = {}; this.n = 0; <case>; Object.values(this.r).map(String).join(',')`.
    const ORACLE_CASES: &[(&str, &str)] = &[
        (
            "this.o = {b: 1, a: 2, c: 3}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "b,a,c",
        ),
        (
            "this.o = {b: 1}; this.o[2] = 2; this.o.a = 3; this.o[0] = 4; this.o[1] = 5; for (this.k in this.o) this.r[this.n++] = this.k;",
            "0,1,2,b,a",
        ),
        (
            "this.p = {x: 1, y: 2, z: 3}; this.o = {__proto__: this.p, y: 4, w: 5}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "y,w,x,z",
        ),
        (
            "this.g = {g1: 1, shared: 1}; this.p = {__proto__: this.g, p1: 1, shared: 2}; this.o = {__proto__: this.p, o1: 1}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "o1,p1,shared,g1",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; for (this.k in this.o) { delete this.o.b; this.r[this.n++] = this.k; }",
            "a,c",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; for (this.k in this.o) { this.o.d = 1; this.r[this.n++] = this.k; }",
            "a,b,c",
        ),
        (
            "this.o = {}; this.o[0] = 'a'; this.o[1] = 'b'; this.o.c = 'c'; for (this.k in this.o) this.r[this.n++] = this.k;",
            "0,1,c",
        ),
        (
            "this.o = {}; this.o[0] = 1; this.o[5] = 2; this.o.x = 3; for (this.k in this.o) this.r[this.n++] = this.k;",
            "0,5,x",
        ),
        (
            "this.p = {}; this.p[0] = 'x'; this.p.q = 1; this.o = {__proto__: this.p, a: 1}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "a,0,q",
        ),
        (
            "this.objs = {}; this.objs[0] = {a: 1}; this.objs[1] = {b: 1}; this.objs[2] = {a: 1}; this.objs[3] = {c: 1}; this.objs[3][0] = 1; for (this.i = 0; this.i < 4; this.i++) for (this.k in this.objs[this.i]) this.r[this.n++] = this.k;",
            "a,b,a,0,c",
        ),
        (
            "this.o = {a: 1, b: 2}; for (this.i = 0; this.i < 2; this.i++) { for (this.k in this.o) this.r[this.n++] = this.k; this.o.c = 3; }",
            "a,b,a,b,c",
        ),
        (
            "this.p = {x: 1}; this.o = {__proto__: this.p, a: 1}; for (this.i = 0; this.i < 2; this.i++) { for (this.k in this.o) this.r[this.n++] = this.k; this.p.y = 2; }",
            "a,x,a,x,y",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; delete this.o.a; for (this.k in this.o) this.r[this.n++] = this.k;",
            "b,c",
        ),
        (
            "this.o = {__proto__: null, a: 1}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "a",
        ),
        (
            "this.o = {\"01\": 1, \"1\": 2, \"4294967295\": 3, \"4294967294\": 4}; for (this.k in this.o) this.r[this.n++] = this.k;",
            "1,4294967294,01,4294967295",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; for (this.k in this.o) { delete this.o.c; this.o.c = 4; this.r[this.n++] = this.k; }",
            "a,b,c",
        ),
        (
            "this.p = {a: 1, b: 2}; this.o = {__proto__: this.p, c: 3}; for (this.k in this.o) { delete this.p.b; this.r[this.n++] = this.k; }",
            "c,a",
        ),
        (
            "this.o = {}; this.o[1.5] = 1; this.o[-1] = 2; this.o[true] = 3; this.o[null] = 4; this.o[undefined] = 5; this.o[4294967295] = 6; this.o[-0] = 7; this.o[1e21] = 8; for (this.k in this.o) this.r[this.n++] = this.k;",
            "0,1.5,-1,true,null,undefined,4294967295,1e+21",
        ),
        (
            "this.o = {a: 1, b: 2}; this.r[0] = this.o.a; this.r[1] = this.o[\"b\"]; this.r[2] = this.o.c; this.o.c = 3; this.r[3] = this.o.c; this.r[4] = delete this.o.a; this.r[5] = this.o.a; this.r[6] = delete this.o.zz; this.n = 7;",
            "1,2,undefined,3,true,undefined,true",
        ),
        (
            "this.objs = {}; this.objs[0] = {x: 1}; this.objs[1] = {a: 0, x: 2}; this.objs[2] = {b: 0, x: 3}; this.objs[3] = {c: 0, x: 4}; this.objs[4] = {d: 0, x: 5}; this.objs[5] = {x: 6}; this.objs[6] = {e: 0, x: 7}; for (this.i = 0; this.i < 7; this.i++) this.r[this.n++] = this.objs[this.i].x;",
            "1,2,3,4,5,6,7",
        ),
        (
            "this.objs = {}; this.objs[0] = {x: 1}; this.objs[1] = {a: 0, x: 2}; this.objs[2] = {b: 0, x: 3}; this.objs[3] = {c: 0, x: 4}; this.objs[4] = {d: 0, x: 5}; this.objs[5] = {x: 6}; this.objs[6] = {e: 0}; for (this.i = 0; this.i < 7; this.i++) { this.objs[this.i].x = this.i; this.objs[this.i].y = this.i; } for (this.i = 0; this.i < 7; this.i++) { this.r[this.n++] = this.objs[this.i].x; this.r[this.n++] = this.objs[this.i][\"y\"]; }",
            "0,0,1,1,2,2,3,3,4,4,5,5,6,6",
        ),
        (
            "this.p = {x: 1}; this.o = {__proto__: this.p}; for (this.i = 0; this.i < 3; this.i++) { this.r[this.n++] = this.o.x; this.p.x = this.n; }",
            "1,1,2",
        ),
        (
            "this.p = {}; this.o = {__proto__: this.p}; for (this.i = 0; this.i < 3; this.i++) { this.r[this.n++] = this.o.m; this.p.m = this.n; }",
            "undefined,1,2",
        ),
        (
            "this.p = {}; this.o = {__proto__: this.p}; for (this.i = 0; this.i < 3; this.i++) { this.r[this.n++] = this.o[\"m\"]; this.p.m = this.n; }",
            "undefined,1,2",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; this.s = {...this.o, d: 4}; for (this.k in this.s) this.r[this.n++] = this.k;",
            "a,b,c,d",
        ),
        (
            "this.o = {a: 1, b: 2, c: 3}; this.o[0] = 9; ({a: this.x, ...this.rest} = this.o); for (this.k in this.rest) this.r[this.n++] = this.k; this.r[this.n++] = this.x;",
            "0,b,c,1",
        ),
        (
            "for (this.i = 0; this.i < 4; this.i++) { this.o2 = {a: this.i, b: this.i}; this.r[this.n++] = this.o2.a; } this.o2.c = 5; for (this.k in this.o2) this.r[this.n++] = this.k;",
            "0,1,2,3,a,b,c",
        ),
        (
            "for (this.i = 0; this.i < 3; this.i++) { this.t = {x: this.i, y: 2}; this.r[this.n++] = this.t.x; this.r[this.n++] = this.t.y; }",
            "0,2,1,2,2,2",
        ),
        (
            "this.o = {}; for (this.i = 0; this.i < 3; this.i++) this.o[this.i] = this.i; this.o[10] = 10; for (this.k in this.o) this.r[this.n++] = this.k;",
            "0,1,2,10",
        ),
        (
            "this.o = {a: 1}; for (this.i = 0; this.i < 3; this.i++) { this.r[this.n++] = this.o.a; delete this.o.a; this.o.a = this.i; }",
            "1,0,1",
        ),
        (
            "this.o = {a: 1, b: 2}; this.s = {x: 0, ...this.o, ...null, ...undefined, b: 3}; for (this.k in this.s) { this.r[this.n++] = this.k; this.r[this.n++] = this.s[this.k]; }",
            "x,0,a,1,b,3",
        ),
        (
            "this.p = {inherited: 1}; this.o = {__proto__: this.p, own: 2}; this.s = {...this.o}; for (this.k in this.s) this.r[this.n++] = this.k;",
            "own",
        ),
        (
            "this.o = {a: 1, b: 2}; ({b: this.x, ...this.rest} = this.o); ({...this.all} = this.o); for (this.k in this.rest) this.r[this.n++] = this.k; for (this.k in this.all) this.r[this.n++] = this.k;",
            "a,a,b",
        ),
        (
            "\"use strict\"; this.o = {a: 1}; this.r[0] = delete this.o.a; this.r[1] = this.o.a; this.o.b = 2; this.r[2] = this.o[\"b\"]; this.n = 3;",
            "true,undefined,2",
        ),
        (
            "this.o = {}; for (this.i = 0; this.i < 70; this.i++) this.o[this.i] = this.i; for (this.i = 0; this.i < 70; this.i++) delete this.o[this.i]; this.o.x = 1; for (this.k in this.o) this.r[this.n++] = this.k;",
            "x",
        ),
    ];

    #[test]
    fn scripts_access_properties_like_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        for (case, expected) in ORACLE_CASES {
            assert_eq!(run_case(&vm, &test_realm, case), *expected, "{case}");
        }
    }

    #[test]
    fn scripts_access_properties_like_the_cpp_runtime_when_every_allocation_collects() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        vm.heap().set_should_collect_on_every_allocation(true);
        for (case, expected) in ORACLE_CASES {
            assert_eq!(run_case(&vm, &test_realm, case), *expected, "{case}");
        }
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn property_keys_follow_to_property_key() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let key_of = |value: Value| value.to_property_key(&vm).must();
        assert_eq!(key_of(int(7)), PropertyKey::from(7u32));
        assert_eq!(key_of(int(-7)), key("-7"));
        assert_eq!(key_of(Value::from_f64(7.0)), PropertyKey::from(7u32));
        assert_eq!(key_of(Value::from_f64(-0.0)), PropertyKey::from(0u32));
        assert_eq!(key_of(Value::from_f64(0.5)), key("0.5"));
        assert_eq!(key_of(Value::from_f64(f64::NAN)), key("NaN"));
        assert_eq!(key_of(Value::from_f64(4294967295.0)), key("4294967295"));
        assert!(key_of(Value::from_f64(4294967295.0)).is_string());
        assert_eq!(key_of(Value::from_bool(false)), key("false"));
        assert_eq!(key_of(Value::NULL), key("null"));
        assert_eq!(key_of(Value::UNDEFINED), key("undefined"));
        let string = PrimitiveString::create_from_utf8(&vm, "42");
        assert_eq!(key_of(Value::from_string(string)), PropertyKey::from(42u32));
        let symbol = Symbol::create(&vm, None, Kind::Unique);
        assert_eq!(key_of(Value::from_symbol(symbol)), PropertyKey::from(symbol));

        // An object without toString or valueOf has no primitive value.
        let object = Object::create(&vm, test_realm.realm, None);
        let message = thrown_message(|| Value::from_object(object).to_property_key(&vm));
        assert!(message.contains("Cannot convert object to string"), "{message}");
    }

    #[test]
    fn strings_and_their_prototype_answer_property_reads() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let string_prototype = test_realm.realm.string_prototype(&vm);
        string_prototype
            .set(&vm, &key("shared"), int(5), ShouldThrowExceptions::Yes)
            .must();
        let result = run_case(
            &vm,
            &test_realm,
            "this.s = 'abc'; this.r[0] = this.s[1]; this.r[1] = this.s[5]; this.r[2] = this.s.shared; this.r[3] = this.s['shared']; this.r[4] = this.s.length; this.r[5] = this.s.missing; this.n = 6;",
        );
        assert_eq!(result, "b,undefined,5,5,3,undefined");
    }

    #[test]
    fn dictionary_objects_and_arrays_take_the_slow_paths() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        vm.heap().set_should_collect_on_every_allocation(true);
        let this_object = test_realm.object();
        let dictionary = test_realm.object();
        dictionary
            .set(&vm, &key("a"), int(1), ShouldThrowExceptions::Yes)
            .must();
        dictionary
            .set(&vm, &key("b"), int(2), ShouldThrowExceptions::Yes)
            .must();
        dictionary.invalidate_property_lookup_caches(&vm);
        assert!(dictionary.shape().is_dictionary());
        this_object
            .set(
                &vm,
                &key("d"),
                Value::from_object(dictionary),
                ShouldThrowExceptions::Yes,
            )
            .must();
        let array = test_realm.array(&[int(10), int(20), int(30)]);
        this_object
            .set(&vm, &key("a"), Value::from_object(array), ShouldThrowExceptions::Yes)
            .must();

        run(
            &vm,
            &test_realm,
            this_object,
            "this.r = {}; this.n = 0; \
             for (this.i = 0; this.i < 3; this.i++) { this.r[this.n++] = this.d.a; this.d.a = this.i; delete this.d.b; this.d.b = this.n; } \
             for (this.k in this.d) this.r[this.n++] = this.k; \
             for (this.k in this.a) this.r[this.n++] = this.k; \
             this.r[this.n++] = this.a.length; this.a[5] = 50; this.r[this.n++] = this.a.length; \
             delete this.a[1]; this.a[1] = 21; this.r[this.n++] = this.a[1]; \
             for (this.k in this.a) this.r[this.n++] = this.k;",
        );
        assert_eq!(collected_values(&vm, this_object), "1,0,1,a,b,0,1,2,3,6,21,0,1,2,5");
        assert!(dictionary.shape().is_dictionary());
    }

    fn test_executable(
        vm: &Vm,
        identifiers: &[&str],
        property_keys: &[&str],
        property_lookup_caches: u32,
        object_shape_caches: u32,
    ) -> Gc<Executable> {
        let counts = ExecutableCacheCounts {
            property_lookup_caches,
            global_variable_caches: 0,
            environment_coordinate_caches: 0,
            environment_shape_caches: 0,
        };
        let mut executable = Executable::new(vec![0u8; 8].into_boxed_slice(), 5, 0, 0, Box::new([]), &counts, true);
        executable.identifier_table = identifiers.iter().map(|name| Utf16FlyString::from_utf8(name)).collect();
        executable.set_property_key_table(
            property_keys
                .iter()
                .map(|name| Utf16FlyString::from_utf8(name))
                .collect(),
        );
        executable.allocate_object_caches(object_shape_caches, 1);
        executable.length_identifier = property_keys
            .iter()
            .position(|name| *name == "length")
            .map(|index| PropertyKeyTableIndex(index as u32));
        Executable::create_from_parts(vm, executable)
    }

    /// Runs `operation` in a frame of `executable`, as a slow path runs in the frame of the instruction it executes.
    fn in_frame<R>(
        vm: &Vm,
        test_realm: &TestRealm,
        executable: Gc<Executable>,
        private_environment: Option<Gc<PrivateEnvironment>>,
        operation: impl FnOnce() -> R,
    ) -> R {
        let stack = vm.interpreter_stack();
        let mark = stack.top.get();
        let context = stack.allocate(0, 0, 0).expect("the interpreter stack has room");
        // SAFETY: The context was just allocated, and an Executable starts with its head.
        unsafe {
            let context_ref = context.as_ref();
            context_ref.realm.set(Some(test_realm.realm));
            context_ref
                .executable
                .set(Some(Gc::from_non_null(executable.as_non_null().cast())));
            context_ref.private_environment.set(private_environment);
        }
        vm.push_execution_context(context);
        let result = operation();
        vm.pop_execution_context();
        stack.deallocate(mark);
        result
    }

    #[test]
    fn private_names_are_added_found_read_and_written() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        vm.heap().set_should_collect_on_every_allocation(true);
        let executable = test_executable(&vm, &["#x", "#m"], &[], 0, 0);
        let private_environment = new_private_environment(&vm, None);
        let object = test_realm.object();
        in_frame(&vm, &test_realm, executable, Some(private_environment), || {
            for name in 0..2 {
                let add = op::AddPrivateName {
                    header: header(true),
                    name: IdentifierTableIndex(name),
                };
                assert_eq!(
                    add_private_name(&vm, 4, &add),
                    SlowPathControl::continue_at(4 + op::AddPrivateName::LENGTH)
                );
            }
            let x = private_environment.resolve_private_identifier(&Utf16FlyString::from_utf8("#x"));
            let m = private_environment.resolve_private_identifier(&Utf16FlyString::from_utf8("#m"));

            let has = op::HasPrivateId {
                header: header(true),
                dst: Operand(0),
                base: Operand(0),
                property: IdentifierTableIndex(0),
            };
            let mut has_values = op::HasPrivateIdValues {
                dst: Value::EMPTY,
                base: Value::from_object(object),
            };
            has_private_id(&vm, 0, &has, &mut has_values);
            assert_eq!(has_values.dst, Value::FALSE);
            object.private_field_add(&vm, &x, int(1)).must();
            has_private_id(&vm, 0, &has, &mut has_values);
            assert_eq!(has_values.dst, Value::TRUE);
            let message = thrown_message(|| {
                let mut values = op::HasPrivateIdValues {
                    dst: Value::EMPTY,
                    base: int(1),
                };
                has_private_id(&vm, 0, &has, &mut values)
            });
            assert!(message.contains("'in' operator must be used on an object"), "{message}");

            let put = op::PutPrivateById {
                header: header(true),
                base: Operand(0),
                property: IdentifierTableIndex(0),
                src: Operand(0),
            };
            let mut put_values = op::PutPrivateByIdValues {
                base: Value::from_object(object),
                src: int(2),
            };
            assert_eq!(
                put_private_by_id(&vm, 8, &put, &mut put_values),
                SlowPathControl::continue_at(8 + op::PutPrivateById::LENGTH)
            );
            let get = op::GetPrivateById {
                header: header(true),
                dst: Operand(0),
                base: Operand(0),
                property: IdentifierTableIndex(0),
            };
            let mut get_values = op::GetPrivateByIdValues {
                dst: Value::EMPTY,
                base: Value::from_object(object),
            };
            get_private_by_id(&vm, 0, &get, &mut get_values);
            assert_eq!(get_values.dst, int(2));

            // A method can be read but not written, and a name the object lacks can be neither.
            object
                .private_method_or_accessor_add(
                    &vm,
                    PrivateElement {
                        key: m,
                        kind: PrivateElementKind::Method,
                        value: int(3),
                    },
                )
                .must();
            let get_method = op::GetPrivateById {
                header: header(true),
                dst: Operand(0),
                base: Operand(0),
                property: IdentifierTableIndex(1),
            };
            get_private_by_id(&vm, 0, &get_method, &mut get_values);
            assert_eq!(get_values.dst, int(3));
            let put_method = op::PutPrivateById {
                header: header(true),
                base: Operand(0),
                property: IdentifierTableIndex(1),
                src: Operand(0),
            };
            let message = thrown_message(|| put_private_by_id(&vm, 0, &put_method, &mut put_values));
            assert!(message.contains("#m"), "{message}");
            let other = test_realm.object();
            let message = thrown_message(|| {
                let mut values = op::GetPrivateByIdValues {
                    dst: Value::EMPTY,
                    base: Value::from_object(other),
                };
                get_private_by_id(&vm, 0, &get, &mut values)
            });
            assert!(message.contains("#x"), "{message}");

            // A primitive base needs ToObject, which throws for undefined.
            let message = thrown_message(|| {
                let mut values = op::GetPrivateByIdValues {
                    dst: Value::EMPTY,
                    base: Value::UNDEFINED,
                };
                get_private_by_id(&vm, 0, &get, &mut values)
            });
            assert!(message.contains("ToObject on null or undefined"), "{message}");
        });
    }

    #[test]
    fn deletes_throw_only_in_strict_code() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let executable = test_executable(&vm, &[], &["x", "y"], 0, 0);
        let object = test_realm.object();
        let mut descriptor = PropertyDescriptor {
            value: Some(int(1)),
            configurable: Some(false),
            ..Default::default()
        };
        object.define_property_or_throw(&vm, &key("x"), &mut descriptor).must();
        object.set(&vm, &key("y"), int(2), ShouldThrowExceptions::Yes).must();
        in_frame(&vm, &test_realm, executable, None, || {
            let delete = |strict: bool, property: u32| {
                let instruction = op::DeleteById {
                    header: header(strict),
                    dst: Operand(0),
                    base: Operand(0),
                    property: PropertyKeyTableIndex(property),
                };
                let mut values = op::DeleteByIdValues {
                    dst: Value::EMPTY,
                    base: Value::from_object(object),
                };
                delete_by_id(&vm, 0, &instruction, &mut values);
                values.dst
            };
            assert_eq!(delete(false, 0), Value::FALSE);
            let message = thrown_message(|| delete(true, 0));
            assert!(
                message.contains("Cannot delete property 'x' of [object Object]"),
                "{message}"
            );
            assert_eq!(delete(true, 1), Value::TRUE);
            assert!(object.storage_get(&vm, &key("y")).is_none());

            let instruction = op::DeleteByValue {
                header: header(true),
                dst: Operand(0),
                base: Operand(0),
                property: Operand(0),
            };
            let mut values = op::DeleteByValueValues {
                dst: Value::EMPTY,
                base: Value::from_object(object),
                property: Value::from_f64(1.5),
            };
            delete_by_value(&vm, 0, &instruction, &mut values);
            assert_eq!(values.dst, Value::TRUE);
            let message = thrown_message(|| {
                let mut values = op::DeleteByValueValues {
                    dst: Value::EMPTY,
                    base: Value::NULL,
                    property: int(0),
                };
                delete_by_value(&vm, 0, &instruction, &mut values)
            });
            assert!(message.contains("ToObject on null or undefined"), "{message}");
        });
    }

    #[test]
    fn keyed_lookups_fill_the_vm_wide_cache() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let prototype = test_realm.object();
        let object = Object::create(&vm, test_realm.realm, Some(prototype));
        object.set(&vm, &key("own"), int(2), ShouldThrowExceptions::Yes).must();
        let receiver = Value::from_object(object);
        let cache = vm.keyed_property_lookup_cache();
        let index_for = |name: &str| KeyedPropertyLookupCache::entry_index_for(object.shape(), key(name).as_string());
        let entry_for = |name: &str| cache.entry(index_for(name));
        let get =
            |property_key: &PropertyKey| get_by_value_with_keyed_cache(&vm, object, receiver, property_key).must();

        // The cache keeps one entry per index, so the names must not share an entry for the object's shape.
        let name_with_free_entry = |base: &str, taken: &[usize]| {
            (0..)
                .map(|suffix| format!("{base}{suffix}"))
                .find(|name| !taken.contains(&index_for(name)))
                .expect("some name has a free entry")
        };
        let inherited_name = name_with_free_entry("inherited", &[index_for("own")]);
        let missing_name = name_with_free_entry("missing", &[index_for("own"), index_for(&inherited_name)]);
        let (inherited_name, missing_name) = (inherited_name.as_str(), missing_name.as_str());
        prototype
            .set(&vm, &key(inherited_name), int(1), ShouldThrowExceptions::Yes)
            .must();

        for name in ["own", inherited_name, missing_name] {
            assert!(entry_for(name).entry_type == PropertyLookupCacheEntryType::Empty);
        }
        assert_eq!(get(&key("own")), int(2));
        assert_eq!(get(&key(inherited_name)), int(1));
        assert_eq!(get(&key(missing_name)), Value::UNDEFINED);
        let own = entry_for("own");
        assert!(own.entry_type == PropertyLookupCacheEntryType::GetOwnProperty && own.shape == Some(object.shape()));
        let inherited = entry_for(inherited_name);
        assert!(inherited.entry_type == PropertyLookupCacheEntryType::GetPropertyInPrototypeChain);
        assert!(inherited.prototype == Some(prototype));
        let missing = entry_for(missing_name);
        assert!(missing.entry_type == PropertyLookupCacheEntryType::GetMissingProperty);
        assert!(
            missing
                .prototype_chain_validity
                .is_some_and(|validity| validity.is_valid())
        );

        // The cached entries answer again, and stop answering once the prototype chain changes.
        assert_eq!(get(&key(inherited_name)), int(1));
        prototype
            .set(&vm, &key(missing_name), int(3), ShouldThrowExceptions::Yes)
            .must();
        prototype
            .set(&vm, &key(inherited_name), int(4), ShouldThrowExceptions::Yes)
            .must();
        assert!(!missing.prototype_chain_validity.unwrap().is_valid());
        assert_eq!(get(&key(missing_name)), int(3));
        assert_eq!(get(&key(inherited_name)), int(4));
        assert!(entry_for(missing_name).entry_type == PropertyLookupCacheEntryType::GetPropertyInPrototypeChain);

        // Symbols and indices are looked up without the cache.
        let symbol = Symbol::create(&vm, None, Kind::Unique);
        object
            .set(&vm, &PropertyKey::from(symbol), int(5), ShouldThrowExceptions::Yes)
            .must();
        assert_eq!(get(&PropertyKey::from(symbol)), int(5));
        assert_eq!(get(&PropertyKey::from(0u32)), Value::UNDEFINED);

        // Arrays are not cacheable for absence.
        let array = test_realm.array(&[int(1)]);
        let array_receiver = Value::from_object(array);
        assert_eq!(
            get_by_value_with_keyed_cache(&vm, array.upcast(), array_receiver, &key("nothing")).must(),
            Value::UNDEFINED
        );
        let index = KeyedPropertyLookupCache::entry_index_for(array.shape(), key("nothing").as_string());
        assert!(cache.entry(index).entry_type == PropertyLookupCacheEntryType::Empty);
    }

    #[inline(never)]
    fn fill_keyed_cache_with_temporary_shapes(vm: &Vm, test_realm: &TestRealm) -> Vec<usize> {
        (0..64)
            .map(|index| {
                let object = test_realm.object();
                let name = key(&format!("temporary{index}"));
                object.set(vm, &name, int(index), ShouldThrowExceptions::Yes).must();
                assert_eq!(
                    get_by_value_with_keyed_cache(vm, object, Value::from_object(object), &name).must(),
                    int(index)
                );
                KeyedPropertyLookupCache::entry_index_for(object.shape(), name.as_string())
            })
            .collect()
    }

    #[inline(never)]
    fn fill_object_shape_caches_with_temporary_shapes(vm: &Vm, test_realm: &TestRealm, executable: &Executable) {
        for index in 0..64 {
            let object = test_realm.object();
            object
                .set(
                    vm,
                    &key(&format!("temporary{index}")),
                    int(1),
                    ShouldThrowExceptions::Yes,
                )
                .must();
            executable.object_shape_cache(index).shape.set(Some(object.shape()));
        }
    }

    #[test]
    fn caches_forget_the_shapes_that_die() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let indices = fill_keyed_cache_with_temporary_shapes(&vm, &test_realm);
        let executable = test_executable(&vm, &[], &[], 0, 64);
        fill_object_shape_caches_with_temporary_shapes(&vm, &test_realm, &executable);
        vm.heap().collect_garbage();
        let pruned = indices
            .iter()
            .filter(|index| {
                vm.keyed_property_lookup_cache().entry(**index).entry_type == PropertyLookupCacheEntryType::Empty
            })
            .count();
        assert!(
            pruned >= 32,
            "only {pruned} of the 64 dead shapes were pruned from the keyed cache"
        );
        let pruned = (0..64)
            .filter(|index| executable.object_shape_cache(*index).shape.get().is_none())
            .count();
        assert!(
            pruned >= 32,
            "only {pruned} of the 64 dead shapes were pruned from the object shape caches"
        );
    }

    #[test]
    fn data_properties_are_copied_through_every_path() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        vm.heap().set_should_collect_on_every_allocation(true);
        let no_keys = MarkedVec::new(&vm);
        let no_values = MarkedVec::new(&vm);

        // An empty ordinary target takes over the shape of a source with only default data properties.
        let source = test_realm.object();
        source.set(&vm, &key("a"), int(1), ShouldThrowExceptions::Yes).must();
        source.set(&vm, &key("b"), int(2), ShouldThrowExceptions::Yes).must();
        let target = test_realm.object();
        target
            .copy_data_properties(&vm, Value::from_object(source), &no_keys, &no_values)
            .must();
        assert!(target.shape() == source.shape());
        assert_eq!(target.get(&vm, &key("b")).must(), int(2));

        // Excluded keys and packed elements go through the shape walk.
        source
            .set(&vm, &PropertyKey::from(0u32), int(0), ShouldThrowExceptions::Yes)
            .must();
        source
            .set(&vm, &PropertyKey::from(1u32), int(10), ShouldThrowExceptions::Yes)
            .must();
        let excluded = MarkedVec::new(&vm);
        excluded.push(key("a"));
        excluded.push(PropertyKey::from(1u32));
        let target = test_realm.object();
        target
            .copy_data_properties(&vm, Value::from_object(source), &excluded, &no_values)
            .must();
        assert_eq!(enumerable_keys(&vm, &target), "0,b");

        // Non-enumerable properties and accessors take the generic path, which skips the former and reads the latter.
        let mut hidden = PropertyDescriptor {
            value: Some(int(3)),
            writable: Some(true),
            enumerable: Some(false),
            configurable: Some(true),
            ..Default::default()
        };
        source.define_property_or_throw(&vm, &key("hidden"), &mut hidden).must();
        let mut accessor = PropertyDescriptor {
            get: Some(None),
            set: Some(None),
            enumerable: Some(true),
            configurable: Some(true),
            ..Default::default()
        };
        source
            .define_property_or_throw(&vm, &key("accessor"), &mut accessor)
            .must();
        let symbol = Symbol::create(&vm, None, Kind::Unique);
        source
            .set(&vm, &PropertyKey::from(symbol), int(4), ShouldThrowExceptions::Yes)
            .must();
        let target = test_realm.object();
        let excluded_values = MarkedVec::new(&vm);
        excluded_values.push(int(2));
        target
            .copy_data_properties(&vm, Value::from_object(source), &no_keys, &excluded_values)
            .must();
        assert_eq!(enumerable_keys(&vm, &target), "0,1,a,accessor");
        assert_eq!(target.get(&vm, &key("accessor")).must(), Value::UNDEFINED);
        assert_eq!(target.get(&vm, &PropertyKey::from(symbol)).must(), int(4));

        // Nullish sources copy nothing.
        let target = test_realm.object();
        target
            .copy_data_properties(&vm, Value::NULL, &no_keys, &no_values)
            .must();
        assert_eq!(enumerable_keys(&vm, &target), "");
    }

    #[test]
    fn get_by_id_caches_answer_the_cache_only_paths() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let executable = test_executable(&vm, &[], &["x", "length", "y"], 3, 0);
        let prototype = test_realm.object();
        prototype.set(&vm, &key("y"), int(7), ShouldThrowExceptions::Yes).must();
        let object = Object::create(&vm, test_realm.realm, Some(prototype));
        object.set(&vm, &key("x"), int(1), ShouldThrowExceptions::Yes).must();
        let base = Value::from_object(object);
        let fill = |name: &str, cache: &PropertyLookupCache| {
            property_access::get_by_id(
                &vm,
                GetByIdMode::Normal,
                || None,
                &key(name),
                base,
                base,
                cache,
                CachePropertyAbsence::Yes,
            )
            .must()
        };

        let cache = executable.property_lookup_cache(0);
        assert_eq!(try_get_by_id_cache(base, cache), Value::EMPTY);
        fill("x", cache);
        assert_eq!(try_get_by_id_cache(base, cache), int(1));
        assert_eq!(try_get_by_id_cache(int(1), cache), Value::EMPTY);
        let other = test_realm.object();
        assert_eq!(try_get_by_id_cache(Value::from_object(other), cache), Value::EMPTY);

        let inherited_cache = executable.property_lookup_cache(1);
        fill("y", inherited_cache);
        assert_eq!(try_get_by_id_cache(base, inherited_cache), int(7));
        prototype.set(&vm, &key("z"), int(8), ShouldThrowExceptions::Yes).must();
        assert_eq!(try_get_by_id_cache(base, inherited_cache), Value::EMPTY);

        let missing_cache = executable.property_lookup_cache(2);
        fill("missing", missing_cache);
        assert_eq!(try_get_by_id_cache(base, missing_cache), Value::UNDEFINED);

        // An accessor found through the cache is the interpreter's to call.
        let accessor_object = test_realm.object();
        let mut accessor = PropertyDescriptor {
            get: Some(None),
            set: Some(None),
            configurable: Some(true),
            ..Default::default()
        };
        accessor_object
            .define_property_or_throw(&vm, &key("x"), &mut accessor)
            .must();
        cache.clear();
        cache.update(PropertyLookupCacheEntryType::GetOwnProperty, |entry| {
            entry.shape = Some(accessor_object.shape());
            entry.property_offset = 0;
        });
        let accessor_base = Value::from_object(accessor_object);
        assert_eq!(try_get_by_id_cache(accessor_base, cache), Value::EMPTY);
        in_frame(&vm, &test_realm, executable, None, || {
            let instruction = op::GetById {
                header: header(true),
                dst: Operand(0),
                base: Operand(0),
                property: PropertyKeyTableIndex(0),
                base_identifier: no_identifier(),
                cache: 0,
            };
            let mut values = op::GetByIdValues {
                dst: Value::EMPTY,
                base: accessor_base,
            };
            assert!(!try_inline_get_by_id_accessor(&vm, 0, &instruction, &values));
            assert_eq!(
                get_by_id_cached_accessor(&vm, 16, &instruction, &mut values),
                SlowPathControl::continue_at(16 + op::GetById::LENGTH)
            );
            assert_eq!(values.dst, Value::UNDEFINED);
        });
    }

    #[test]
    fn put_by_id_caches_replay_additions_and_changes() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let executable = test_executable(&vm, &[], &["x", "length"], 2, 0);
        let instruction = op::PutById {
            header: header(true),
            base: Operand(0),
            property: PropertyKeyTableIndex(0),
            src: Operand(0),
            kind: PutKind::Normal as u32,
            cache: 0,
            base_identifier: no_identifier(),
        };
        in_frame(&vm, &test_realm, executable, None, || {
            let put = |object: Gc<Object>, value: Value| {
                let mut values = op::PutByIdValues {
                    base: Value::from_object(object),
                    src: value,
                };
                put_by_id(&vm, 0, &instruction, &mut values)
            };
            let try_put = |object: Value, value: Value| {
                let values = op::PutByIdValues {
                    base: object,
                    src: value,
                };
                try_put_by_id_cache(&vm, &instruction, &values)
            };
            let first = test_realm.object();
            assert!(!try_put(Value::from_object(first), int(1)));
            put(first, int(1));
            assert_eq!(first.get(&vm, &key("x")).must(), int(1));

            // The cached addition applies to another object with the shape the first one had.
            let second = test_realm.object();
            assert!(try_put(Value::from_object(second), int(2)));
            assert!(second.shape() == first.shape());
            assert_eq!(second.get(&vm, &key("x")).must(), int(2));

            // A change of the property is cached once the slow path saw it.
            assert!(!try_put(Value::from_object(first), int(3)));
            put(first, int(3));
            assert!(try_put(Value::from_object(second), int(4)));
            assert_eq!(second.get(&vm, &key("x")).must(), int(4));
            assert_eq!(first.get(&vm, &key("x")).must(), int(3));

            // A cached addition does not apply to an object that is not extensible, nor to a primitive.
            let sealed = test_realm.object();
            sealed.internal_prevent_extensions(&vm).must();
            assert!(!try_put(Value::from_object(sealed), int(5)));
            assert!(!try_put(int(1), int(5)));
            let message = thrown_message(|| put(sealed, int(5)));
            assert!(
                message.contains("Cannot set property 'x' of [object Object]"),
                "{message}"
            );
        });
    }

    #[test]
    fn holey_array_stores_skip_the_slow_path_only_when_nothing_can_observe_them() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let array = test_realm.array(&[int(1), int(2), int(3)]);
        let store = |base: Value, index: i32, value: Value| {
            let values = op::PutByValueValues {
                base,
                property: int(index),
                src: value,
            };
            try_put_by_value_holey_array(&values)
        };
        let base = Value::from_object(array);
        assert!(!store(base, 1, int(20)), "the array is packed");
        array.internal_delete(&vm, &PropertyKey::from(1u32)).must();
        assert!(array.indexed_storage_kind() == IndexedStorageKind::Holey);
        assert!(store(base, 1, int(20)));
        assert_eq!(array.get(&vm, &PropertyKey::from(1u32)).must(), int(20));
        assert!(!store(base, 3, int(4)), "the index is past the end");
        assert!(!store(base, -1, int(4)));
        assert!(!store(Value::from_object(test_realm.object()), 0, int(4)));
        assert!(!store(int(1), 0, int(4)));

        array.set_prototype(&vm, Some(test_realm.object()));
        assert!(!store(base, 1, int(21)), "the prototype chain is not the default one");

        let values = op::PutByValueValues {
            base,
            property: int(0),
            src: int(0),
        };
        assert!(!try_put_by_value_typed_array(&vm, &values));
        let mut values = op::GetByValueValues {
            dst: Value::EMPTY,
            base,
            property: int(0),
        };
        assert!(!try_get_by_value_typed_array(&vm, &mut values));
    }

    #[test]
    fn object_literals_reuse_their_shape_and_offsets() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let this_object = test_realm.object();
        let executable = compile(
            &vm,
            "for (this.i = 0; this.i < 3; this.i++) { this[this.i] = {a: this.i, b: 2, c: 3}; }",
        );
        run_executable(&vm, &test_realm, this_object, executable);
        let objects: Vec<Gc<Object>> = (0..3u32)
            .map(|index| this_object.get(&vm, &PropertyKey::from(index)).must().as_object())
            .collect();
        assert!(objects[0].shape() == objects[1].shape() && objects[1].shape() == objects[2].shape());
        assert!(executable.object_shape_cache(0).shape.get() == Some(objects[0].shape()));
        assert_eq!(*executable.object_shape_cache(0).property_offsets.borrow(), [0, 1, 2]);
        for (index, object) in objects.iter().enumerate() {
            assert_eq!(object.get(&vm, &key("a")).must(), int(index as i32));
            assert_eq!(enumerable_keys(&vm, object), "a,b,c");
        }

        // A dictionary shape is not cached.
        let object = test_realm.object();
        object.invalidate_property_lookup_caches(&vm);
        let executable = test_executable(&vm, &[], &[], 0, 1);
        in_frame(&vm, &test_realm, executable, None, || {
            let instruction = op::CacheObjectShape {
                header: header(true),
                object: Operand(0),
                cache: 0,
            };
            let mut values = op::CacheObjectShapeValues {
                object: Value::from_object(object),
            };
            cache_object_shape(&vm, 0, &instruction, &mut values);
            assert!(executable.object_shape_cache(0).shape.get().is_none());

            let mut values = op::NewObjectWithNoPrototypeValues { dst: Value::EMPTY };
            new_object_with_no_prototype(&vm, 0, &mut values);
            assert!(values.dst.as_object().prototype().is_none());
        });
    }

    #[test]
    fn for_in_snapshots_are_cached_on_the_site_and_the_shape() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let executable = test_executable(&vm, &[], &[], 0, 0);
        let prototype = test_realm.object();
        prototype
            .set(&vm, &key("inherited"), int(1), ShouldThrowExceptions::Yes)
            .must();
        let object = Object::create(&vm, test_realm.realm, Some(prototype));
        object.set(&vm, &key("own"), int(2), ShouldThrowExceptions::Yes).must();
        in_frame(&vm, &test_realm, executable, None, || {
            let instruction = op::GetObjectPropertyIterator {
                header: header(true),
                dst_keys: Operand(0),
                dst_receiver: Operand(0),
                object: Operand(0),
                cache: 0,
            };
            let iterate = |object: Gc<Object>| {
                let mut values = op::GetObjectPropertyIteratorValues {
                    dst_keys: Value::EMPTY,
                    dst_receiver: Value::EMPTY,
                    object: Value::from_object(object),
                };
                get_object_property_iterator(&vm, 0, &instruction, &mut values);
                assert!(values.dst_receiver == Value::from_object(object));
                object_property_iterator_cache_data_of(values.dst_keys)
            };
            let keys_of = |keys: Gc<ObjectPropertyIteratorCacheData>, receiver: Gc<Object>| {
                let mut collected = Vec::new();
                let mut values = op::ObjectPropertyIteratorNextValues {
                    dst_value: Value::EMPTY,
                    dst_done: Value::EMPTY,
                    receiver: Value::from_object(receiver),
                    keys: Value::with_cell_tag(nan_box::IS_CELL_BIT, keys),
                    cursor: int(0),
                };
                loop {
                    object_property_iterator_next(&vm, 0, &mut values);
                    if values.dst_done == Value::TRUE {
                        assert_eq!(values.dst_value, Value::UNDEFINED);
                        return collected.join(",");
                    }
                    collected.push(string_of(values.dst_value));
                }
            };

            let keys = iterate(object);
            assert!(keys.fast_path() == ObjectPropertyIteratorFastPath::PlainNamed);
            assert_eq!(keys.property_value_count(), 2);
            assert_eq!(string_of(keys.property_value(1)), "inherited");
            assert!(executable.object_property_iterator_cache(0).data.get() == Some(keys));
            assert!(object.shape().property_iterator_cache() == Some(keys));

            // Another object of the same shape shares the snapshot, and one of another shape gets its own.
            let sibling = Object::create(&vm, test_realm.realm, Some(prototype));
            sibling.set(&vm, &key("own"), int(3), ShouldThrowExceptions::Yes).must();
            assert!(sibling.shape() == object.shape());
            vm.heap().collect_garbage();
            assert!(iterate(object) == keys);
            assert_eq!(keys_of(keys, object), "own,inherited");
            assert!(iterate(sibling) == keys);
            let other = test_realm.object();
            other.set(&vm, &key("o"), int(1), ShouldThrowExceptions::Yes).must();
            let other_keys = iterate(other);
            assert!(other_keys != keys);
            assert!(iterate(object) == keys, "the snapshot is found on the shape again");

            // A deleted key is skipped, and a prototype change makes a new snapshot.
            object.internal_delete(&vm, &key("own")).must();
            let after_delete = iterate(object);
            assert_eq!(keys_of(after_delete, object), "inherited");
            assert_eq!(keys_of(keys, sibling), "own,inherited");
            prototype
                .set(&vm, &key("added"), int(4), ShouldThrowExceptions::Yes)
                .must();
            let after_prototype_change = iterate(sibling);
            assert!(after_prototype_change != keys);
            assert_eq!(keys_of(after_prototype_change, sibling), "own,inherited,added");
            assert_eq!(
                keys_of(keys, sibling),
                "own,inherited",
                "an old snapshot keeps its keys"
            );

            // Indexed properties above the receiver need the generic enumeration, whose snapshot is not cached.
            prototype
                .set(&vm, &PropertyKey::from(0u32), int(5), ShouldThrowExceptions::Yes)
                .must();
            let slow_keys = iterate(sibling);
            assert!(slow_keys.fast_path() == ObjectPropertyIteratorFastPath::None);
            assert!(slow_keys.shape().is_none() && slow_keys.property_value_count() == 0);
            assert_eq!(keys_of(slow_keys, sibling), "own,0,inherited,added");
            assert!(iterate(sibling) != slow_keys);

            // A packed receiver lists its indices first.
            let array = test_realm.array(&[int(1), int(2)]);
            let array_keys = iterate(array.upcast());
            assert!(array_keys.fast_path() == ObjectPropertyIteratorFastPath::PackedIndexed);
            assert_eq!(keys_of(array_keys, array.upcast()), "0,1");
        });
    }

    #[test]
    fn nullish_bases_throw_what_the_cpp_runtime_throws() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let executable = test_executable(&vm, &["x"], &["foo", "length"], 2, 0);
        let keyless_object = test_realm.object();
        in_frame(&vm, &test_realm, executable, None, || {
            let get_by_value_message = |base: Value, property: Value, base_identifier: Option<u32>| {
                let instruction = op::GetByValue {
                    header: header(false),
                    dst: Operand(0),
                    base: Operand(0),
                    property: Operand(0),
                    base_identifier: optional_identifier_index(base_identifier),
                };
                let mut values = op::GetByValueValues {
                    dst: Value::EMPTY,
                    base,
                    property,
                };
                thrown_message(|| get_by_value(&vm, 0, &instruction, &mut values))
            };
            // The base is checked before the key is converted, which would throw for an object without toString.
            let message = get_by_value_message(Value::NULL, Value::from_object(keyless_object), None);
            assert!(
                message.contains("Cannot access property \"[object Object]\" on null object\""),
                "{message}"
            );
            let message = get_by_value_message(Value::UNDEFINED, Value::from_f64(1.5), Some(0));
            assert!(
                message.contains("Cannot access property \"1.5\" on undefined object \"x\""),
                "{message}"
            );

            let put_by_value_message = |strict: bool| {
                let instruction = op::PutByValue {
                    header: header(strict),
                    base: Operand(0),
                    property: Operand(0),
                    src: Operand(0),
                    kind: PutKind::Normal as u32,
                    base_identifier: optional_identifier_index(Some(0)),
                };
                let mut values = op::PutByValueValues {
                    base: Value::NULL,
                    property: Value::from_f64(1.5),
                    src: int(2),
                };
                thrown_message(|| put_by_value(&vm, 0, &instruction, &mut values))
            };
            let message = put_by_value_message(false);
            assert!(
                message.contains("Cannot access property \"1.5\" on null object \"x\""),
                "{message}"
            );
            let message = put_by_value_message(true);
            assert!(message.contains("Cannot set property '1.5' of null"), "{message}");

            let instruction = op::GetById {
                header: header(false),
                dst: Operand(0),
                base: Operand(0),
                property: PropertyKeyTableIndex(0),
                base_identifier: optional_identifier_index(Some(0)),
                cache: 0,
            };
            let message = thrown_message(|| {
                let mut values = op::GetByIdValues {
                    dst: Value::EMPTY,
                    base: Value::UNDEFINED,
                };
                get_by_id(&vm, 0, &instruction, &mut values)
            });
            assert!(
                message.contains("Cannot access property \"foo\" on undefined object \"x\""),
                "{message}"
            );
            let instruction = op::GetLength {
                header: header(false),
                dst: Operand(0),
                base: Operand(0),
                base_identifier: optional_identifier_index(Some(0)),
                cache: 1,
            };
            let message = thrown_message(|| {
                let mut values = op::GetLengthValues {
                    dst: Value::EMPTY,
                    base: Value::UNDEFINED,
                };
                get_length(&vm, 0, &instruction, &mut values)
            });
            assert!(
                message.contains("Cannot access property \"length\" on undefined object \"x\""),
                "{message}"
            );

            // A length that is not magical is read through the cache like any other property.
            let object = test_realm.object();
            object
                .set(&vm, &key("length"), int(5), ShouldThrowExceptions::Yes)
                .must();
            let mut values = op::GetLengthValues {
                dst: Value::EMPTY,
                base: Value::from_object(object),
            };
            get_length(&vm, 0, &instruction, &mut values);
            assert_eq!(values.dst, int(5));
            assert!(executable.property_lookup_cache(1).first_entry().unwrap().shape == Some(object.shape()));
        });
    }

    fn cache_tiers(executable: &Executable) -> Vec<usize> {
        (0..executable.head.property_lookup_caches.size.get())
            .map(|index| executable.property_lookup_cache(index).data.get() & PROPERTY_LOOKUP_CACHE_DATA_TAG_MASK)
            .collect()
    }

    #[test]
    fn sites_that_see_many_shapes_stay_correct_in_every_cache_tier() {
        const MONOMORPHIC: usize = 0;
        const POLYMORPHIC: usize = 1;
        const MEGAMORPHIC: usize = 2;
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        for (shape_count, expected_tier) in [(1, MONOMORPHIC), (3, POLYMORPHIC), (12, MEGAMORPHIC)] {
            let this_object = test_realm.object();
            let objects = test_realm.object();
            for index in 0..shape_count {
                let object = test_realm.object();
                object
                    .set(&vm, &key(&format!("p{index}")), int(0), ShouldThrowExceptions::Yes)
                    .must();
                object
                    .set(&vm, &key("x"), int(index), ShouldThrowExceptions::Yes)
                    .must();
                objects
                    .set(
                        &vm,
                        &PropertyKey::from(index as u32),
                        Value::from_object(object),
                        ShouldThrowExceptions::Yes,
                    )
                    .must();
            }
            this_object
                .set(
                    &vm,
                    &key("objs"),
                    Value::from_object(objects),
                    ShouldThrowExceptions::Yes,
                )
                .must();
            this_object
                .set(&vm, &key("count"), int(shape_count), ShouldThrowExceptions::Yes)
                .must();
            let executable = compile(
                &vm,
                "this.r = {}; this.n = 0; \
                 for (this.j = 0; this.j < 3; this.j++) for (this.i = 0; this.i < this.count; this.i++) { this.r[this.n++] = this.objs[this.i].x; this.objs[this.i].x = this.n; }",
            );
            run_executable(&vm, &test_realm, this_object, executable);

            let mut expected = Vec::new();
            let mut values: Vec<i32> = (0..shape_count).collect();
            let mut n = 0;
            for _ in 0..3 {
                for value in &mut values {
                    expected.push(value.to_string());
                    n += 1;
                    *value = n;
                }
            }
            assert_eq!(collected_values(&vm, this_object), expected.join(","));
            let tiers = cache_tiers(&executable);
            assert!(tiers.contains(&expected_tier), "{shape_count} shapes: {tiers:?}");
            assert_eq!(
                tiers.contains(&MEGAMORPHIC),
                expected_tier == MEGAMORPHIC,
                "{shape_count} shapes: {tiers:?}"
            );
        }
    }
}
