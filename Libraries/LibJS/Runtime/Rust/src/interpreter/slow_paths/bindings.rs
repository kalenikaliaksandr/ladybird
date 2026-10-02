/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The slow paths that create, look up and change bindings, and their helpers.

use core::cell::Cell;

use ak::Utf16FlyString;
use libjs_abi::{EnvironmentMode, register};

use crate::bytecode::executable::{Executable, PropertyLookupCacheEntryType};
use crate::bytecode::op;
use crate::bytecode::operand::{IdentifierTableIndex, InstructionHeader};
use crate::bytecode::property_access::{Strict, get_cached_property_value};
use crate::interpreter::runtime_functions::{SlowPathControl, asm_try, handle_asm_exception};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::execution_context::{ExecutionContext, ScriptOrModule};
use crate::layout::value::Value;
use crate::runtime::abstract_operations::{
    call, get_this_environment, new_declarative_environment, new_object_environment, new_private_environment,
    perform_import_call,
};
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::declarative_environment::DeclarativeEnvironment;
use crate::runtime::ecmascript_function_object::as_ecmascript_function_object;
use crate::runtime::environment::{Environment, InitializeBindingHint};
use crate::runtime::environment_coordinate::EnvironmentCoordinate;
use crate::runtime::environment_shape::EnvironmentShapeCache;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::function_environment::FunctionEnvironment;
use crate::runtime::global_environment::GlobalEnvironment;
use crate::runtime::object::{
    CacheableGetPropertyMetadata, CacheableGetPropertyMetadataType, CacheableSetPropertyMetadata,
    CacheableSetPropertyMetadataType, PropertyLookupPhase,
};
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::reference::{BaseType, Reference};
use crate::runtime::source_text_module::SourceTextModule;
use crate::utf16::{Utf16Display, Utf16View, utf16_formatted};

/// The environment `coordinate` refers to from `environment`, if the interpreter may keep finding the binding there:
/// every environment on the way must be declarative and not open to eval adding bindings.
pub fn get_cacheable_environment(
    environment: Gc<Environment>,
    coordinate: EnvironmentCoordinate,
) -> Option<Gc<DeclarativeEnvironment>> {
    assert!(coordinate.is_valid());

    let mut environment = environment;
    for _ in 0..coordinate.hops {
        if !environment.is_declarative_environment() || environment.is_permanently_screwed_by_eval() {
            return None;
        }
        environment = environment.outer_environment()?;
    }
    if environment.is_declarative_environment() && !environment.is_permanently_screwed_by_eval() {
        return environment.downcast::<DeclarativeEnvironment>();
    }
    None
}

/// The environment a dynamic binding access found its binding in last time, if it still applies. A cache that no
/// longer applies is cleared, so the access resolves the binding again.
pub fn get_cached_environment(
    environment: Gc<Environment>,
    cache: &Cell<EnvironmentCoordinate>,
) -> Option<Gc<DeclarativeEnvironment>> {
    if !cache.get().is_valid() {
        return None;
    }

    if let Some(cached_environment) = get_cacheable_environment(environment, cache.get()) {
        return Some(cached_environment);
    }

    cache.set(EnvironmentCoordinate::invalid());
    None
}

/// Remembers where a binding was resolved from `environment`, given the coordinate of the Reference that
/// ResolveBinding produced, if any.
pub fn update_environment_coordinate_cache(
    environment: Gc<Environment>,
    reference_environment_coordinate: Option<EnvironmentCoordinate>,
    cache: &Cell<EnvironmentCoordinate>,
) {
    let Some(candidate) = reference_environment_coordinate else {
        return;
    };
    if get_cacheable_environment(environment, candidate).is_some() {
        cache.set(candidate);
    }
}

/// The environment a static coordinate refers to. The bytecode only carries static coordinates of declarative
/// environments.
pub fn environment_at_coordinate(
    environment: Gc<Environment>,
    coordinate: EnvironmentCoordinate,
) -> Gc<DeclarativeEnvironment> {
    assert!(coordinate.is_valid());

    let mut environment = environment;
    for _ in 0..coordinate.hops {
        environment = environment
            .outer_environment()
            .expect("a static coordinate stays within the environment chain");
    }
    environment
        .downcast::<DeclarativeEnvironment>()
        .expect("static coordinates refer to declarative environments")
}

/// What CreateLexicalEnvironment does besides storing the new environment as the running context's lexical
/// environment and in its destination.
pub fn create_lexical_environment(
    vm: &Vm,
    parent: Gc<Environment>,
    shape_cache: EnvironmentShapeCache,
    capacity: u32,
    is_catch_environment: bool,
) -> Gc<DeclarativeEnvironment> {
    let environment = new_declarative_environment(vm, parent);
    environment.set_environment_shape_cache(shape_cache, capacity as usize);
    environment.ensure_capacity(capacity as usize);
    environment.set_is_catch_environment(is_catch_environment);
    environment
}

/// What CreateVariableEnvironment does besides storing the new environment as the running context's variable and
/// lexical environment. The shape cache is the active function's var environment shape, when `capacity` is the
/// number of var bindings that function declares.
pub fn create_variable_environment(
    vm: &Vm,
    lexical_environment: Gc<Environment>,
    shape_cache: Option<EnvironmentShapeCache>,
    capacity: u32,
) -> Gc<DeclarativeEnvironment> {
    let var_environment = new_declarative_environment(vm, lexical_environment);
    if let Some(shape_cache) = shape_cache {
        var_environment.set_environment_shape_cache(shape_cache, capacity as usize);
    }
    var_environment.ensure_capacity(capacity as usize);
    var_environment
}

fn running_execution_context_environment(vm: &Vm, mode: EnvironmentMode) -> Gc<Environment> {
    let context = vm
        .running_execution_context()
        .expect("bindings are created in an execution context");
    // SAFETY: The running execution context is live.
    let context = unsafe { context.as_ref() };
    match mode {
        EnvironmentMode::Lexical => context.lexical_environment.get(),
        EnvironmentMode::Var => context.variable_environment.get(),
    }
    .expect("the running execution context has its environments")
}

/// What CreateVariable does.
pub fn create_variable(
    vm: &Vm,
    name: &Utf16FlyString,
    mode: EnvironmentMode,
    is_global: bool,
    is_immutable: bool,
    is_strict: bool,
) -> ThrowCompletionOr<()> {
    if mode == EnvironmentMode::Lexical {
        assert!(!is_global);

        let lexical_environment = running_execution_context_environment(vm, EnvironmentMode::Lexical);

        // Note: This is papering over an issue where "FunctionDeclarationInstantiation" creates these bindings for us.
        //       Instead of crashing in there, we'll just raise an exception here.
        if lexical_environment.has_binding(vm, name, None)? {
            return vm.throw_completion_with_utf16_message(
                ErrorKind::InternalError,
                utf16_formatted("Lexical environment already has binding '{}'", &[name]),
            );
        }

        if is_immutable {
            return lexical_environment.create_immutable_binding(vm, name, is_strict);
        }
        return lexical_environment.create_mutable_binding(vm, name, is_strict);
    }

    if !is_global {
        let variable_environment = running_execution_context_environment(vm, EnvironmentMode::Var);
        if is_immutable {
            return variable_environment.create_immutable_binding(vm, name, is_strict);
        }
        return variable_environment.create_mutable_binding(vm, name, is_strict);
    }

    // NOTE: CreateVariable with m_is_global set to true is expected to only be used in GlobalDeclarationInstantiation currently, which only uses "false" for "can_be_deleted".
    //       The only area that sets "can_be_deleted" to true is EvalDeclarationInstantiation, which is currently fully implemented in C++ and not in Bytecode.
    running_execution_context_environment(vm, EnvironmentMode::Var)
        .downcast::<GlobalEnvironment>()
        .expect("a global CreateVariable runs with the global environment as its variable environment")
        .create_global_var_binding(vm, name, false)
}

fn running_execution_context(vm: &Vm) -> &ExecutionContext {
    let context = vm
        .running_execution_context()
        .expect("slow paths run in an execution context");
    // SAFETY: The running execution context is live while its slow paths run.
    unsafe { context.as_ref() }
}

/// VM::current_executable(): the executable of the running frame.
fn current_executable(vm: &Vm) -> Gc<Executable> {
    let executable = running_execution_context(vm)
        .executable
        .get()
        .expect("a running frame has an executable");
    // SAFETY: An executable starts with its head, which is all the frame points to.
    unsafe { Gc::from_non_null(executable.as_non_null().cast()) }
}

/// VM::get_identifier(), copied out of the executable's identifier table.
fn get_identifier(vm: &Vm, index: IdentifierTableIndex) -> Utf16FlyString {
    current_executable(vm).identifier_table[index.0 as usize].clone()
}

fn strict_of(header: &InstructionHeader) -> Strict {
    if header.strict { Strict::Yes } else { Strict::No }
}

fn environment_mode_of(raw_mode: u32) -> EnvironmentMode {
    match raw_mode {
        0 => EnvironmentMode::Lexical,
        1 => EnvironmentMode::Var,
        _ => unreachable!("{raw_mode} is not an EnvironmentMode"),
    }
}

/// Throws a new error and hands it to the interpreter.
fn throw_error(
    vm: &Vm,
    pc: u32,
    kind: ErrorKind,
    error_type: ErrorType,
    arguments: &[&dyn Utf16Display],
) -> SlowPathControl {
    match vm.throw_completion::<()>(kind, error_type, arguments) {
        Err(throw) => handle_asm_exception(vm, pc, throw.value()),
        Ok(()) => unreachable!("throw_completion always throws"),
    }
}

/// What a binding helper returns to continue after its instruction, like the C++ helpers that return `pc`.
fn continue_after_instruction(pc: u32) -> SlowPathControl {
    SlowPathControl::dispatch_at(pc)
}

fn advance_or_continue(pc: u32, next_pc: SlowPathControl, instruction_length: u32) -> SlowPathControl {
    if next_pc != continue_after_instruction(pc) {
        return next_pc;
    }
    SlowPathControl::continue_at(pc + instruction_length)
}

/// An environment coordinate cache of the running executable. It is borrowed afresh at each use, since the binding
/// accesses in between can run arbitrary code.
struct EnvironmentCoordinateCacheSlot {
    executable: Gc<Executable>,
    index: u32,
}

impl EnvironmentCoordinateCacheSlot {
    fn of_running_executable(vm: &Vm, index: u32) -> Self {
        Self {
            executable: current_executable(vm),
            index,
        }
    }

    fn get(&self) -> &Cell<EnvironmentCoordinate> {
        self.executable.environment_coordinate_cache(self.index)
    }
}

/// The environment `hops` steps out from `environment`, which a static coordinate says is there.
fn environment_at_hops(environment: Gc<Environment>, hops: u32) -> Gc<Environment> {
    let mut environment = environment;
    for _ in 0..hops {
        environment = environment
            .outer_environment()
            .expect("a static coordinate stays within the environment chain");
    }
    environment
}

/// static_cast<DeclarativeEnvironment&>, for an environment a coordinate refers to.
fn as_declarative(environment: &Environment) -> &DeclarativeEnvironment {
    environment
        .as_declarative_environment()
        .expect("environment coordinates refer to declarative environments")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AsmBindingIsKnownToBeInitialized {
    No,
    Yes,
}

/// Mirrors Op::BindingInitializationMode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingInitializationMode {
    Initialize,
    Set,
}

fn asm_get_binding(
    vm: &Vm,
    pc: u32,
    dst: &mut Value,
    cache: EnvironmentCoordinate,
    binding_is_known_to_be_initialized: AsmBindingIsKnownToBeInitialized,
) -> SlowPathControl {
    assert!(cache.is_valid());

    let environment = environment_at_hops(running_lexical_environment(vm), cache.hops);

    let value = if binding_is_known_to_be_initialized == AsmBindingIsKnownToBeInitialized::No {
        asm_try!(
            vm,
            pc,
            as_declarative(&environment).get_binding_value_direct(vm, cache.index as usize)
        )
    } else {
        as_declarative(&environment).get_initialized_binding_value_direct(cache.index as usize)
    };
    *dst = value;
    continue_after_instruction(pc)
}

fn asm_dynamic_get_binding(
    vm: &Vm,
    pc: u32,
    dst: &mut Value,
    identifier_index: IdentifierTableIndex,
    strict: Strict,
    cache: &EnvironmentCoordinateCacheSlot,
    binding_is_known_to_be_initialized: AsmBindingIsKnownToBeInitialized,
) -> SlowPathControl {
    let current_environment = running_lexical_environment(vm);
    if let Some(cached_environment) = get_cached_environment(current_environment, cache.get()) {
        let index = cache.get().get().index as usize;
        let value = if binding_is_known_to_be_initialized == AsmBindingIsKnownToBeInitialized::No {
            asm_try!(vm, pc, cached_environment.get_binding_value_direct(vm, index))
        } else {
            cached_environment.get_initialized_binding_value_direct(index)
        };
        *dst = value;
        return continue_after_instruction(pc);
    }

    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&get_identifier(vm, identifier_index), strict, None)
    );
    update_environment_coordinate_cache(current_environment, reference.environment_coordinate(), cache.get());

    *dst = asm_try!(vm, pc, reference.get_value(vm));
    continue_after_instruction(pc)
}

fn asm_dynamic_get_callee_and_this_from_environment(
    vm: &Vm,
    pc: u32,
    callee_dst: &mut Value,
    this_value_dst: &mut Value,
    identifier_index: IdentifierTableIndex,
    strict: Strict,
    cache: &EnvironmentCoordinateCacheSlot,
) -> SlowPathControl {
    let current_environment = running_lexical_environment(vm);
    if let Some(cached_environment) = get_cached_environment(current_environment, cache.get()) {
        let callee = asm_try!(
            vm,
            pc,
            cached_environment.get_binding_value_direct(vm, cache.get().get().index as usize)
        );
        *callee_dst = callee;
        *this_value_dst = Value::UNDEFINED;
        return continue_after_instruction(pc);
    }

    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&get_identifier(vm, identifier_index), strict, None)
    );
    update_environment_coordinate_cache(current_environment, reference.environment_coordinate(), cache.get());

    let callee = asm_try!(vm, pc, reference.get_value(vm));

    let mut this_value = Value::UNDEFINED;
    if reference.is_property_reference() {
        this_value = reference.get_this_value();
    } else if reference.is_environment_reference()
        && let Some(base_object) = reference.base_environment().with_base_object()
    {
        this_value = Value::from_object(base_object);
    }

    *callee_dst = callee;
    *this_value_dst = this_value;
    continue_after_instruction(pc)
}

fn running_lexical_environment(vm: &Vm) -> Gc<Environment> {
    running_execution_context_environment(vm, EnvironmentMode::Lexical)
}

fn asm_initialize_or_set_binding(
    vm: &Vm,
    pc: u32,
    environment_mode: EnvironmentMode,
    initialization_mode: BindingInitializationMode,
    strict: Strict,
    value: Value,
    cache: EnvironmentCoordinate,
) -> SlowPathControl {
    assert!(cache.is_valid());

    let environment = environment_at_hops(running_execution_context_environment(vm, environment_mode), cache.hops);

    if initialization_mode == BindingInitializationMode::Initialize {
        asm_try!(
            vm,
            pc,
            as_declarative(&environment).initialize_binding_direct(
                vm,
                cache.index as usize,
                value,
                InitializeBindingHint::Normal
            )
        );
    } else {
        asm_try!(
            vm,
            pc,
            as_declarative(&environment).set_mutable_binding_direct(
                vm,
                cache.index as usize,
                value,
                strict == Strict::Yes
            )
        );
    }
    continue_after_instruction(pc)
}

#[allow(clippy::too_many_arguments)]
fn asm_dynamic_initialize_or_set_binding(
    vm: &Vm,
    pc: u32,
    environment_mode: EnvironmentMode,
    initialization_mode: BindingInitializationMode,
    identifier_index: IdentifierTableIndex,
    strict: Strict,
    value: Value,
    cache: &EnvironmentCoordinateCacheSlot,
) -> SlowPathControl {
    let environment = running_execution_context_environment(vm, environment_mode);

    if let Some(cached_environment) = get_cached_environment(environment, cache.get()) {
        let index = cache.get().get().index as usize;
        match initialization_mode {
            BindingInitializationMode::Initialize => {
                asm_try!(
                    vm,
                    pc,
                    cached_environment.initialize_binding_direct(vm, index, value, InitializeBindingHint::Normal)
                );
            }
            BindingInitializationMode::Set => {
                asm_try!(
                    vm,
                    pc,
                    cached_environment.set_mutable_binding_direct(vm, index, value, strict == Strict::Yes)
                );
            }
        }
        return continue_after_instruction(pc);
    }

    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&get_identifier(vm, identifier_index), strict, Some(environment))
    );
    update_environment_coordinate_cache(environment, reference.environment_coordinate(), cache.get());
    match initialization_mode {
        BindingInitializationMode::Initialize => {
            asm_try!(
                vm,
                pc,
                reference.initialize_referenced_binding(vm, value, InitializeBindingHint::Normal)
            );
        }
        BindingInitializationMode::Set => {
            asm_try!(vm, pc, reference.put_value(vm, value));
        }
    }
    continue_after_instruction(pc)
}

pub fn get_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetBinding,
    values: &mut op::GetBindingValues,
) -> SlowPathControl {
    let next_pc = asm_get_binding(
        vm,
        pc,
        &mut values.dst,
        instruction.cache,
        AsmBindingIsKnownToBeInitialized::No,
    );
    advance_or_continue(pc, next_pc, op::GetBinding::LENGTH)
}

pub fn dynamic_get_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicGetBinding,
    values: &mut op::DynamicGetBindingValues,
) -> SlowPathControl {
    let cache = EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache);
    let next_pc = asm_dynamic_get_binding(
        vm,
        pc,
        &mut values.dst,
        instruction.identifier,
        strict_of(&instruction.header),
        &cache,
        AsmBindingIsKnownToBeInitialized::No,
    );
    advance_or_continue(pc, next_pc, op::DynamicGetBinding::LENGTH)
}

pub fn dynamic_get_initialized_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicGetInitializedBinding,
    values: &mut op::DynamicGetInitializedBindingValues,
) -> SlowPathControl {
    let cache = EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache);
    let next_pc = asm_dynamic_get_binding(
        vm,
        pc,
        &mut values.dst,
        instruction.identifier,
        strict_of(&instruction.header),
        &cache,
        AsmBindingIsKnownToBeInitialized::Yes,
    );
    advance_or_continue(pc, next_pc, op::DynamicGetInitializedBinding::LENGTH)
}

pub fn get_callee_and_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::GetCalleeAndThisFromEnvironment,
    values: &mut op::GetCalleeAndThisFromEnvironmentValues,
) -> SlowPathControl {
    let cache = instruction.cache;
    assert!(cache.is_valid());

    let environment = environment_at_hops(running_lexical_environment(vm), cache.hops);

    let callee = asm_try!(
        vm,
        pc,
        as_declarative(&environment).get_binding_value_direct(vm, cache.index as usize)
    );
    values.callee = callee;
    let mut this_value = Value::UNDEFINED;
    if let Some(base_object) = environment.with_base_object() {
        this_value = Value::from_object(base_object);
    }
    values.this_value = this_value;
    SlowPathControl::continue_at(pc + op::GetCalleeAndThisFromEnvironment::LENGTH)
}

pub fn dynamic_get_callee_and_this(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicGetCalleeAndThisFromEnvironment,
    values: &mut op::DynamicGetCalleeAndThisFromEnvironmentValues,
) -> SlowPathControl {
    let cache = EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache);
    let next_pc = asm_dynamic_get_callee_and_this_from_environment(
        vm,
        pc,
        &mut values.callee,
        &mut values.this_value,
        instruction.identifier,
        strict_of(&instruction.header),
        &cache,
    );
    advance_or_continue(pc, next_pc, op::DynamicGetCalleeAndThisFromEnvironment::LENGTH)
}

pub fn get_import_meta(
    vm: &Vm,
    pc: u32,
    _instruction: &op::GetImportMeta,
    values: &mut op::GetImportMetaValues,
) -> SlowPathControl {
    values.dst = Value::from_object(vm.get_import_meta());
    SlowPathControl::continue_at(pc + op::GetImportMeta::LENGTH)
}

pub fn get_import(vm: &Vm, pc: u32, instruction: &op::GetImport, values: &mut op::GetImportValues) -> SlowPathControl {
    let ScriptOrModule::Module(module) = running_execution_context(vm).script_or_module.get() else {
        unreachable!("GetImport only runs in the code of a module");
    };
    let module = module
        .downcast::<SourceTextModule>()
        .expect("GetImport only runs in the code of a Source Text Module Record");
    let identifier = get_identifier(vm, instruction.identifier);
    values.dst = asm_try!(
        vm,
        pc,
        module.get_imported_binding_value(vm, instruction.import_index, &identifier)
    );
    SlowPathControl::continue_at(pc + op::GetImport::LENGTH)
}

pub fn get_new_target(
    vm: &Vm,
    pc: u32,
    _instruction: &op::GetNewTarget,
    values: &mut op::GetNewTargetValues,
) -> SlowPathControl {
    values.dst = vm.get_new_target();
    SlowPathControl::continue_at(pc + op::GetNewTarget::LENGTH)
}

pub fn get_global(vm: &Vm, pc: u32, instruction: &op::GetGlobal, values: &mut op::GetGlobalValues) -> SlowPathControl {
    let binding_object = vm.global_object();
    let declarative_record = vm.global_declarative_environment();
    let executable = current_executable(vm);
    let cache = || executable.global_variable_cache(instruction.cache);
    let strict = strict_of(&instruction.header);

    let shape = binding_object.shape();
    if cache().environment_serial_number.get() == declarative_record.environment_serial_number() {
        if let Some(entry) = cache().first_entry()
            && entry.shape == Some(shape)
            && (!shape.is_dictionary() || shape.dictionary_generation() == entry.shape_dictionary_generation)
        {
            let value = binding_object.get_direct(entry.property_offset);
            values.dst = asm_try!(
                vm,
                pc,
                get_cached_property_value(vm, value, Value::from_object(binding_object))
            );
            return SlowPathControl::continue_at(pc + op::GetGlobal::LENGTH);
        }

        if cache().has_environment_binding_index.get() {
            values.dst = asm_try!(
                vm,
                pc,
                declarative_record.get_binding_value_direct(vm, cache().environment_binding_index.get() as usize)
            );
            return SlowPathControl::continue_at(pc + op::GetGlobal::LENGTH);
        }
    }

    cache().reset();
    cache()
        .environment_serial_number
        .set(declarative_record.environment_serial_number());

    let identifier = get_identifier(vm, instruction.identifier);

    let mut offset = None;
    if asm_try!(vm, pc, declarative_record.has_binding(&identifier, Some(&mut offset))) {
        let offset = offset.expect("the global declarative record reports the index of its bindings");
        cache().environment_binding_index.set(offset as u32);
        cache().has_environment_binding_index.set(true);
        values.dst = asm_try!(
            vm,
            pc,
            declarative_record.get_binding_value(vm, &identifier, strict == Strict::Yes)
        );
        return SlowPathControl::continue_at(pc + op::GetGlobal::LENGTH);
    }

    let identifier_key = PropertyKey::from(identifier.clone());
    if asm_try!(vm, pc, binding_object.has_property(vm, &identifier_key)) {
        let dictionary_generation = shape.dictionary_generation();
        let mut cacheable_metadata = CacheableGetPropertyMetadata::default();
        let value = asm_try!(
            vm,
            pc,
            binding_object.internal_get(
                vm,
                &identifier_key,
                Value::from_object(binding_object),
                Some(&mut cacheable_metadata),
                PropertyLookupPhase::OwnProperty,
            )
        );
        if cacheable_metadata.r#type == CacheableGetPropertyMetadataType::GetOwnProperty
            && shape == binding_object.shape()
            && shape.dictionary_generation() == dictionary_generation
        {
            cache().update(PropertyLookupCacheEntryType::GetOwnProperty, |entry| {
                entry.shape = Some(shape);
                entry.property_offset = cacheable_metadata
                    .property_offset
                    .expect("an own property get reports its offset");

                if shape.is_dictionary() {
                    entry.shape_dictionary_generation = shape.dictionary_generation();
                }
            });
        }
        values.dst = value;
        return SlowPathControl::continue_at(pc + op::GetGlobal::LENGTH);
    }

    throw_error(
        vm,
        pc,
        ErrorKind::ReferenceError,
        ErrorType::UnknownIdentifier,
        &[&identifier],
    )
}

pub fn set_global(vm: &Vm, pc: u32, instruction: &op::SetGlobal, values: &mut op::SetGlobalValues) -> SlowPathControl {
    let binding_object = vm.global_object();
    let declarative_record = vm.global_declarative_environment();
    let executable = current_executable(vm);
    let cache = || executable.global_variable_cache(instruction.cache);
    let shape = binding_object.shape();
    let src = values.src;
    let strict = strict_of(&instruction.header);

    if cache().environment_serial_number.get() == declarative_record.environment_serial_number() {
        if let Some(entry) = cache().first_entry()
            && entry.shape == Some(shape)
            && (!shape.is_dictionary() || shape.dictionary_generation() == entry.shape_dictionary_generation)
        {
            let value = binding_object.get_direct(entry.property_offset);
            if value.is_accessor() {
                let setter = value.as_accessor().setter().map_or(Value::NULL, Value::from_object);
                asm_try!(vm, pc, call(vm, setter, Value::from_object(binding_object), &[src]));
                return SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH);
            }
            if entry.writes_data_property {
                binding_object.put_direct(entry.property_offset, src);
                return SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH);
            }
        }

        if cache().has_environment_binding_index.get() {
            asm_try!(
                vm,
                pc,
                declarative_record.set_mutable_binding_direct(
                    vm,
                    cache().environment_binding_index.get() as usize,
                    src,
                    strict == Strict::Yes
                )
            );
            return SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH);
        }
    }

    cache().reset();
    cache()
        .environment_serial_number
        .set(declarative_record.environment_serial_number());

    let identifier = get_identifier(vm, instruction.identifier);

    let mut offset = None;
    if asm_try!(vm, pc, declarative_record.has_binding(&identifier, Some(&mut offset))) {
        let offset = offset.expect("the global declarative record reports the index of its bindings");
        cache().environment_binding_index.set(offset as u32);
        cache().has_environment_binding_index.set(true);
        asm_try!(
            vm,
            pc,
            declarative_record.set_mutable_binding(vm, &identifier, src, strict == Strict::Yes)
        );
        return SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH);
    }

    let identifier_key = PropertyKey::from(identifier.clone());
    if asm_try!(vm, pc, binding_object.has_property(vm, &identifier_key)) {
        let dictionary_generation = shape.dictionary_generation();
        let mut cacheable_metadata = CacheableSetPropertyMetadata::default();
        let success = asm_try!(
            vm,
            pc,
            binding_object.internal_set(
                vm,
                &identifier_key,
                src,
                Value::from_object(binding_object),
                Some(&mut cacheable_metadata),
                PropertyLookupPhase::OwnProperty,
            )
        );
        if !success && strict == Strict::Yes {
            let property_or_error = binding_object.internal_get_own_property(vm, &identifier_key);
            if let Ok(property) = property_or_error
                && let Some(property) = property
                && !property.writable.unwrap_or(true)
            {
                return throw_error(
                    vm,
                    pc,
                    ErrorKind::TypeError,
                    ErrorType::DescWriteNonWritable,
                    &[&identifier],
                );
            }
            return throw_error(vm, pc, ErrorKind::TypeError, ErrorType::ObjectSetReturnedFalse, &[]);
        }
        if cacheable_metadata.r#type == CacheableSetPropertyMetadataType::ChangeOwnProperty
            && shape == binding_object.shape()
            && shape.dictionary_generation() == dictionary_generation
        {
            cache().update(PropertyLookupCacheEntryType::ChangeOwnProperty, |entry| {
                entry.shape = Some(shape);
                entry.property_offset = cacheable_metadata
                    .property_offset
                    .expect("an own property change reports its offset");
                entry.writes_data_property = cacheable_metadata.writes_data_property;

                if shape.is_dictionary() {
                    entry.shape_dictionary_generation = shape.dictionary_generation();
                }
            });
        }
        return SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH);
    }

    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&identifier, strict, Some(declarative_record.upcast()))
    );
    asm_try!(vm, pc, reference.put_value(vm, src));
    SlowPathControl::continue_at(pc + op::SetGlobal::LENGTH)
}

pub fn import_call(
    vm: &Vm,
    pc: u32,
    _instruction: &op::ImportCall,
    values: &mut op::ImportCallValues,
) -> SlowPathControl {
    let specifier = values.specifier;
    let options_value = values.options;
    values.dst = asm_try!(vm, pc, perform_import_call(vm, specifier, options_value));
    SlowPathControl::continue_at(pc + op::ImportCall::LENGTH)
}

pub fn dynamic_initialize_lexical_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicInitializeLexicalBinding,
    values: &mut op::DynamicInitializeLexicalBindingValues,
) -> SlowPathControl {
    let next_pc = asm_dynamic_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Lexical,
        BindingInitializationMode::Initialize,
        instruction.identifier,
        strict_of(&instruction.header),
        values.src,
        &EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache),
    );
    advance_or_continue(pc, next_pc, op::DynamicInitializeLexicalBinding::LENGTH)
}

pub fn dynamic_initialize_variable_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicInitializeVariableBinding,
    values: &mut op::DynamicInitializeVariableBindingValues,
) -> SlowPathControl {
    let next_pc = asm_dynamic_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Var,
        BindingInitializationMode::Initialize,
        instruction.identifier,
        strict_of(&instruction.header),
        values.src,
        &EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache),
    );
    advance_or_continue(pc, next_pc, op::DynamicInitializeVariableBinding::LENGTH)
}

pub fn set_lexical_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::SetLexicalBinding,
    values: &mut op::SetLexicalBindingValues,
) -> SlowPathControl {
    let next_pc = asm_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Lexical,
        BindingInitializationMode::Set,
        strict_of(&instruction.header),
        values.src,
        instruction.cache,
    );
    advance_or_continue(pc, next_pc, op::SetLexicalBinding::LENGTH)
}

pub fn dynamic_set_lexical_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicSetLexicalBinding,
    values: &mut op::DynamicSetLexicalBindingValues,
) -> SlowPathControl {
    let next_pc = asm_dynamic_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Lexical,
        BindingInitializationMode::Set,
        instruction.identifier,
        strict_of(&instruction.header),
        values.src,
        &EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache),
    );
    advance_or_continue(pc, next_pc, op::DynamicSetLexicalBinding::LENGTH)
}

pub fn set_variable_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::SetVariableBinding,
    values: &mut op::SetVariableBindingValues,
) -> SlowPathControl {
    let next_pc = asm_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Var,
        BindingInitializationMode::Set,
        strict_of(&instruction.header),
        values.src,
        instruction.cache,
    );
    advance_or_continue(pc, next_pc, op::SetVariableBinding::LENGTH)
}

pub fn dynamic_set_variable_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicSetVariableBinding,
    values: &mut op::DynamicSetVariableBindingValues,
) -> SlowPathControl {
    let next_pc = asm_dynamic_initialize_or_set_binding(
        vm,
        pc,
        EnvironmentMode::Var,
        BindingInitializationMode::Set,
        instruction.identifier,
        strict_of(&instruction.header),
        values.src,
        &EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache),
    );
    advance_or_continue(pc, next_pc, op::DynamicSetVariableBinding::LENGTH)
}

pub fn resolve_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::ResolveBinding,
    values: &mut op::ResolveBindingValues,
) -> SlowPathControl {
    let identifier = get_identifier(vm, instruction.identifier);
    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&identifier, strict_of(&instruction.header), None)
    );
    if reference.is_unresolvable() {
        values.dst = Value::NULL;
        return SlowPathControl::continue_at(pc + op::ResolveBinding::LENGTH);
    }

    assert!(reference.is_environment_reference());
    values.dst = Value::from_environment(reference.base_environment());
    SlowPathControl::continue_at(pc + op::ResolveBinding::LENGTH)
}

pub fn resolve_super_base(
    vm: &Vm,
    pc: u32,
    _instruction: &op::ResolveSuperBase,
    values: &mut op::ResolveSuperBaseValues,
) -> SlowPathControl {
    let environment = get_this_environment(vm)
        .downcast::<FunctionEnvironment>()
        .expect("super is only resolved in a function environment");
    assert!(environment.has_super_binding());
    let base_value = asm_try!(vm, pc, environment.get_super_base(vm));
    values.dst = base_value;
    SlowPathControl::continue_at(pc + op::ResolveSuperBase::LENGTH)
}

pub fn set_resolved_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::SetResolvedBinding,
    values: &mut op::SetResolvedBindingValues,
) -> SlowPathControl {
    let identifier = get_identifier(vm, instruction.identifier);
    let strict = strict_of(&instruction.header);
    let environment = values.environment;
    let reference = if environment.is_null() {
        Reference::with_base_type(BaseType::Unresolvable, PropertyKey::from(identifier), strict)
    } else {
        Reference::with_base_environment(environment.as_environment(), identifier, strict, None)
    };
    asm_try!(vm, pc, reference.put_value(vm, values.src));
    SlowPathControl::continue_at(pc + op::SetResolvedBinding::LENGTH)
}

pub fn typeof_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::TypeofBinding,
    values: &mut op::TypeofBindingValues,
) -> SlowPathControl {
    assert!(instruction.cache.is_valid());

    let environment = environment_at_hops(running_lexical_environment(vm), instruction.cache.hops);

    let value = asm_try!(
        vm,
        pc,
        as_declarative(&environment).get_binding_value_direct(vm, instruction.cache.index as usize)
    );
    values.dst = Value::from_string(value.typeof_(vm));
    SlowPathControl::continue_at(pc + op::TypeofBinding::LENGTH)
}

pub fn dynamic_typeof_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::DynamicTypeofBinding,
    values: &mut op::DynamicTypeofBindingValues,
) -> SlowPathControl {
    let cache = EnvironmentCoordinateCacheSlot::of_running_executable(vm, instruction.cache);
    let current_environment = running_lexical_environment(vm);
    if let Some(environment) = get_cached_environment(current_environment, cache.get()) {
        let value = asm_try!(
            vm,
            pc,
            environment.get_binding_value_direct(vm, cache.get().get().index as usize)
        );
        values.dst = Value::from_string(value.typeof_(vm));
        return SlowPathControl::continue_at(pc + op::DynamicTypeofBinding::LENGTH);
    }

    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(
            &get_identifier(vm, instruction.identifier),
            strict_of(&instruction.header),
            None
        )
    );
    if reference.is_unresolvable() {
        values.dst = Value::from_string(PrimitiveString::create_from_fly_string(
            vm,
            &Utf16FlyString::from_utf8("undefined"),
        ));
        return SlowPathControl::continue_at(pc + op::DynamicTypeofBinding::LENGTH);
    }

    update_environment_coordinate_cache(current_environment, reference.environment_coordinate(), cache.get());
    let value = asm_try!(vm, pc, reference.get_value(vm));
    values.dst = Value::from_string(value.typeof_(vm));
    SlowPathControl::continue_at(pc + op::DynamicTypeofBinding::LENGTH)
}

#[cold]
fn report_wrong_environment_coordinate(
    name: &Utf16FlyString,
    coordinate: EnvironmentCoordinate,
    hop: u32,
    reason: &str,
) -> ! {
    panic!(
        "Environment coordinate for '{}' ({} hops, index {}) is wrong at hop {hop}: {reason}",
        Utf16View::of_fly_string(name).to_utf8(),
        coordinate.hops,
        coordinate.index
    )
}

pub fn verify_environment_coordinate(
    vm: &Vm,
    pc: u32,
    instruction: &op::VerifyEnvironmentCoordinate,
    _values: &mut op::VerifyEnvironmentCoordinateValues,
) -> SlowPathControl {
    let coordinate = instruction.coordinate;
    assert!(coordinate.is_valid());

    let name = get_identifier(vm, instruction.identifier);
    let context = running_execution_context(vm);
    let mut environment = match environment_mode_of(instruction.mode) {
        EnvironmentMode::Lexical => context.lexical_environment.get(),
        EnvironmentMode::Var => context.variable_environment.get(),
    };

    let mut hop = 0;
    loop {
        let Some(current_environment) = environment else {
            report_wrong_environment_coordinate(&name, coordinate, hop, "the environment chain ends");
        };
        let Some(declarative_environment) = current_environment.as_declarative_environment() else {
            report_wrong_environment_coordinate(&name, coordinate, hop, "the environment is not declarative");
        };
        if hop == coordinate.hops {
            if declarative_environment.binding_index(&name) != Some(coordinate.index as usize) {
                report_wrong_environment_coordinate(
                    &name,
                    coordinate,
                    hop,
                    "the environment has no binding at the index",
                );
            }
            break;
        }
        if current_environment.has_binding(vm, &name, None).must() {
            report_wrong_environment_coordinate(&name, coordinate, hop, "a closer environment binds the name");
        }
        environment = current_environment.outer_environment();
        hop += 1;
    }

    SlowPathControl::continue_at(pc + op::VerifyEnvironmentCoordinate::LENGTH)
}

pub fn create_variable_slow_path(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateVariable,
    _values: &mut op::CreateVariableValues,
) -> SlowPathControl {
    let name = get_identifier(vm, instruction.identifier);
    asm_try!(
        vm,
        pc,
        create_variable(
            vm,
            &name,
            environment_mode_of(instruction.mode),
            instruction.is_global,
            instruction.is_immutable,
            instruction.is_strict,
        )
    );
    SlowPathControl::continue_at(pc + op::CreateVariable::LENGTH)
}

pub fn enter_object_environment(
    vm: &Vm,
    pc: u32,
    _instruction: &op::EnterObjectEnvironment,
    values: &mut op::EnterObjectEnvironmentValues,
) -> SlowPathControl {
    let object = asm_try!(vm, pc, values.object.to_object(vm));
    let context = running_execution_context(vm);
    let old_environment = context.lexical_environment.get();
    let new_environment = new_object_environment(vm, object, true, old_environment);
    values.dst = Value::from_environment(new_environment);
    context.lexical_environment.set(Some(new_environment.upcast()));
    SlowPathControl::continue_at(pc + op::EnterObjectEnvironment::LENGTH)
}

pub fn create_immutable_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateImmutableBinding,
    values: &mut op::CreateImmutableBindingValues,
) -> SlowPathControl {
    let environment = values.environment.as_environment();
    asm_try!(
        vm,
        pc,
        environment.create_immutable_binding(
            vm,
            &get_identifier(vm, instruction.identifier),
            instruction.strict_binding
        )
    );
    SlowPathControl::continue_at(pc + op::CreateImmutableBinding::LENGTH)
}

pub fn create_mutable_binding(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateMutableBinding,
    values: &mut op::CreateMutableBindingValues,
) -> SlowPathControl {
    let environment = values.environment.as_environment();
    asm_try!(
        vm,
        pc,
        environment.create_mutable_binding(
            vm,
            &get_identifier(vm, instruction.identifier),
            instruction.can_be_deleted
        )
    );
    SlowPathControl::continue_at(pc + op::CreateMutableBinding::LENGTH)
}

pub fn create_lexical_environment_slow_path(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateLexicalEnvironment,
    values: &mut op::CreateLexicalEnvironmentValues,
) -> SlowPathControl {
    let parent = values.parent.as_environment();
    let executable = current_executable(vm);
    let environment = create_lexical_environment(
        vm,
        parent,
        executable.environment_shape_cache(instruction.shape_cache),
        instruction.capacity,
        instruction.is_catch_environment,
    );
    values.dst = Value::from_environment(environment);
    running_execution_context(vm)
        .lexical_environment
        .set(Some(environment.upcast()));
    SlowPathControl::continue_at(pc + op::CreateLexicalEnvironment::LENGTH)
}

pub fn create_private_environment(
    vm: &Vm,
    pc: u32,
    _instruction: &op::CreatePrivateEnvironment,
    _values: &mut op::CreatePrivateEnvironmentValues,
) -> SlowPathControl {
    let running_execution_context = running_execution_context(vm);
    let outer_private_environment = running_execution_context.private_environment.get();
    running_execution_context
        .private_environment
        .set(Some(new_private_environment(vm, outer_private_environment)));
    SlowPathControl::continue_at(pc + op::CreatePrivateEnvironment::LENGTH)
}

/// The var environment shape cache of the active function's shared data, when `capacity` is its var binding count:
/// VM::active_shared_function_data() with SharedFunctionInstanceData::m_var_environment_bindings_count and
/// m_var_environment_shape. Only code that runs without a function object has no shared data.
/// The active function's var environment shape cache, if this environment is the one its shared data describes.
fn var_environment_shape_cache_of_active_function(vm: &Vm, capacity: u32) -> Option<EnvironmentShapeCache> {
    let shared_data = vm.active_shared_function_data()?;
    (capacity as usize == shared_data.var_environment_bindings_count())
        .then(|| shared_data.var_environment_shape_cache())
}

pub fn create_variable_environment_slow_path(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateVariableEnvironment,
    _values: &mut op::CreateVariableEnvironmentValues,
) -> SlowPathControl {
    let running_execution_context = running_execution_context(vm);
    let shape_cache = var_environment_shape_cache_of_active_function(vm, instruction.capacity);
    let var_environment = create_variable_environment(
        vm,
        running_execution_context
            .lexical_environment
            .get()
            .expect("the running execution context has a lexical environment"),
        shape_cache,
        instruction.capacity,
    );
    running_execution_context
        .variable_environment
        .set(Some(var_environment.upcast()));
    running_execution_context
        .lexical_environment
        .set(Some(var_environment.upcast()));
    SlowPathControl::continue_at(pc + op::CreateVariableEnvironment::LENGTH)
}

pub fn delete_variable(
    vm: &Vm,
    pc: u32,
    instruction: &op::DeleteVariable,
    values: &mut op::DeleteVariableValues,
) -> SlowPathControl {
    let string = get_identifier(vm, instruction.identifier);
    let reference = asm_try!(
        vm,
        pc,
        vm.resolve_binding(&string, strict_of(&instruction.header), None)
    );
    let result = asm_try!(vm, pc, reference.delete_(vm));
    values.dst = Value::from_bool(result);
    SlowPathControl::continue_at(pc + op::DeleteVariable::LENGTH)
}

pub fn resolve_this_binding(
    vm: &Vm,
    pc: u32,
    _instruction: &op::ResolveThisBinding,
    _values: &mut op::ResolveThisBindingValues,
) -> SlowPathControl {
    let running_execution_context = running_execution_context(vm);
    let cached_this_value = running_execution_context.register(register::THIS_VALUE);
    if !cached_this_value.get().is_empty() {
        return SlowPathControl::continue_at(pc + op::ResolveThisBinding::LENGTH);
    }

    if let Some(function) = running_execution_context.function.get()
        && let Some(ecmascript_function) = as_ecmascript_function_object(function)
        && !ecmascript_function.allocates_function_environment()
        && !ecmascript_function.this_value_needs_environment_resolution()
    {
        let this_value = running_execution_context.this_value.get();
        assert!(!this_value.is_empty(), "the frame has a this value");
        cached_this_value.set(this_value);
        return SlowPathControl::continue_at(pc + op::ResolveThisBinding::LENGTH);
    }
    cached_this_value.set(asm_try!(vm, pc, vm.resolve_this_binding()));
    SlowPathControl::continue_at(pc + op::ResolveThisBinding::LENGTH)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::bytecode::executable::{Executable, ExecutableCacheCounts};
    use crate::layout::value::Value;
    use crate::runtime::environment::InitializeBindingHint;
    use crate::runtime::function_environment::FunctionEnvironment;

    fn coordinate(hops: u32, index: u32) -> EnvironmentCoordinate {
        EnvironmentCoordinate { hops, index }
    }

    #[test]
    fn cached_coordinates_hold_while_the_chain_stays_declarative_and_free_of_eval() {
        let vm = Vm::create();
        let function = FunctionEnvironment::create(&vm, None);
        let block = DeclarativeEnvironment::create(&vm, Some(function.upcast()));
        let inner = DeclarativeEnvironment::create(&vm, Some(block.upcast()));
        let inner_environment: Gc<Environment> = inner.upcast();

        assert!(get_cacheable_environment(inner_environment, coordinate(0, 0)) == Some(inner));
        assert!(get_cacheable_environment(inner_environment, coordinate(2, 0)) == Some(function.upcast()));
        assert!(get_cacheable_environment(inner_environment, coordinate(3, 0)).is_none());
        assert!(environment_at_coordinate(inner_environment, coordinate(1, 5)) == block);

        let cache = Cell::new(EnvironmentCoordinate::invalid());
        assert!(get_cached_environment(inner_environment, &cache).is_none());
        update_environment_coordinate_cache(inner_environment, None, &cache);
        assert!(!cache.get().is_valid());
        update_environment_coordinate_cache(inner_environment, Some(coordinate(4, 0)), &cache);
        assert!(!cache.get().is_valid());
        update_environment_coordinate_cache(inner_environment, Some(coordinate(1, 3)), &cache);
        assert_eq!(cache.get(), coordinate(1, 3));
        assert!(get_cached_environment(inner_environment, &cache) == Some(block));

        // Once eval may add bindings along the way, the cache is dropped and the binding resolved again.
        inner.set_permanently_screwed_by_eval();
        assert!(get_cached_environment(inner_environment, &cache).is_none());
        assert!(!cache.get().is_valid());
    }

    #[test]
    fn variables_are_created_in_the_running_contexts_environments() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let counts = ExecutableCacheCounts {
            property_lookup_caches: 0,
            global_variable_caches: 0,
            environment_coordinate_caches: 1,
            environment_shape_caches: 1,
        };
        let executable = vm.heap().allocate(Executable::new(
            vec![0u8; 8].into_boxed_slice(),
            5,
            0,
            0,
            Box::new([]),
            &counts,
            true,
        ));
        let stack = vm.interpreter_stack();
        let mark = stack.top.get();
        let context = stack.allocate(0, 0, 0).expect("the stack has room");
        vm.push_execution_context(context);
        // SAFETY: The context was just pushed.
        let context_ref = unsafe { context.as_ref() };

        let function = FunctionEnvironment::create(&vm, None);
        let var_environment = create_variable_environment(&vm, function.upcast(), None, 1);
        context_ref.variable_environment.set(Some(var_environment.upcast()));
        let lexical_environment = create_lexical_environment(
            &vm,
            var_environment.upcast(),
            executable.environment_shape_cache(0),
            1,
            true,
        );
        context_ref.lexical_environment.set(Some(lexical_environment.upcast()));
        assert!(lexical_environment.is_catch_environment());

        let (x, y) = (Utf16FlyString::from_utf8("x"), Utf16FlyString::from_utf8("y"));
        create_variable(&vm, &x, EnvironmentMode::Lexical, false, true, true).unwrap();
        create_variable(&vm, &y, EnvironmentMode::Var, false, false, false).unwrap();
        assert!(lexical_environment.shape.get().is_some());
        assert!(executable.environment_shape_cache(0).shape() == lexical_environment.shape.get());
        assert_eq!(lexical_environment.binding_index(&x), Some(0));
        assert_eq!(var_environment.binding_index(&y), Some(0));
        assert!(!lexical_environment.binding_is_mutable_by_name(&x));

        executable.environment_coordinate_cache(0).set(coordinate(1, 0));
        let cached = get_cached_environment(lexical_environment.upcast(), executable.environment_coordinate_cache(0))
            .expect("the var environment is one hop out");
        cached
            .initialize_binding_direct(&vm, 0, Value::from_i32(9), InitializeBindingHint::Normal)
            .unwrap();
        assert_eq!(
            var_environment.get_binding_value(&vm, &y, false).unwrap(),
            Value::from_i32(9)
        );

        vm.pop_execution_context();
        stack.deallocate(mark);
    }
}

/// The slow paths called directly, the way the interpreter calls them, in a frame of a script's global code.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod slow_path_tests {
    use super::*;
    use crate::bytecode::executable::ExecutableCacheCounts;
    use crate::bytecode::operand::Operand;
    use crate::layout::object::Object;
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::{TestRealm, key, thrown_message};
    use libjs_abi::register::RESERVED_REGISTER_COUNT;

    const PC: u32 = 16;

    /// A running frame of global code whose executable has `identifiers` and one cache of each kind.
    struct GlobalCodeFrame<'vm> {
        vm: &'vm Vm,
        _test_realm: TestRealm<'vm>,
        global: Gc<Object>,
        executable: Gc<Executable>,
        stack_mark: *mut u8,
    }

    impl<'vm> GlobalCodeFrame<'vm> {
        fn new(vm: &'vm Vm, identifiers: &[&str]) -> Self {
            let test_realm = TestRealm::new(vm);
            let global = set_up_global_object(vm, test_realm.realm);
            let counts = ExecutableCacheCounts {
                property_lookup_caches: 0,
                global_variable_caches: 8,
                environment_coordinate_caches: 2,
                environment_shape_caches: 1,
            };
            let mut executable = Executable::new(
                vec![0u8; 64].into_boxed_slice(),
                RESERVED_REGISTER_COUNT + 2,
                0,
                0,
                Box::new([]),
                &counts,
                false,
            );
            executable.identifier_table = identifiers.iter().map(|name| Utf16FlyString::from_utf8(name)).collect();
            let executable = Executable::create_from_parts(vm, executable);

            let stack = vm.interpreter_stack();
            let stack_mark = stack.top.get();
            let context = stack
                .allocate(RESERVED_REGISTER_COUNT + 2, 0, 0)
                .expect("the interpreter stack has room");
            // SAFETY: The context was just allocated.
            let context_ref = unsafe { context.as_ref() };
            // SAFETY: An executable starts with its head.
            context_ref
                .executable
                .set(Some(unsafe { Gc::from_non_null(executable.as_non_null().cast()) }));
            context_ref.realm.set(Some(test_realm.realm));
            let global_environment = test_realm.realm.global_environment().upcast();
            context_ref.lexical_environment.set(Some(global_environment));
            context_ref.variable_environment.set(Some(global_environment));
            vm.push_execution_context(context);
            Self {
                vm,
                _test_realm: test_realm,
                global,
                executable,
                stack_mark,
            }
        }

        fn context(&self) -> &ExecutionContext {
            running_execution_context(self.vm)
        }

        fn global_environment(&self) -> Gc<GlobalEnvironment> {
            self.context()
                .realm
                .get()
                .expect("the frame has a realm")
                .global_environment()
        }
    }

    impl Drop for GlobalCodeFrame<'_> {
        fn drop(&mut self) {
            self.vm.pop_execution_context();
            self.vm.interpreter_stack().deallocate(self.stack_mark);
        }
    }

    fn header(strict: bool) -> InstructionHeader {
        InstructionHeader { opcode: 0, strict }
    }

    fn get_global_instruction(identifier: u32, strict: bool) -> op::GetGlobal {
        op::GetGlobal {
            header: header(strict),
            dst: Operand(0),
            identifier: IdentifierTableIndex(identifier),
            cache: identifier * 2,
        }
    }

    fn set_global_instruction(identifier: u32, strict: bool) -> op::SetGlobal {
        op::SetGlobal {
            header: header(strict),
            identifier: IdentifierTableIndex(identifier),
            src: Operand(0),
            cache: identifier * 2 + 1,
        }
    }

    fn run_get_global(vm: &Vm, identifier: u32) -> Value {
        let mut values = op::GetGlobalValues { dst: Value::EMPTY };
        assert_eq!(
            get_global(vm, PC, &get_global_instruction(identifier, false), &mut values),
            SlowPathControl::continue_at(PC + op::GetGlobal::LENGTH)
        );
        values.dst
    }

    fn run_set_global(vm: &Vm, identifier: u32, value: Value, strict: bool) {
        let mut values = op::SetGlobalValues { src: value };
        assert_eq!(
            set_global(vm, PC, &set_global_instruction(identifier, strict), &mut values),
            SlowPathControl::continue_at(PC + op::SetGlobal::LENGTH)
        );
    }

    #[test]
    fn global_variable_caches_remember_global_object_properties() {
        let vm = Vm::create();
        let frame = GlobalCodeFrame::new(&vm, &["on_object", "undeclared"]);
        frame
            .global
            .define_direct_property(&vm, &key("on_object"), Value::from_i32(1), DEFAULT_ATTRIBUTES);
        let get_cache = frame.executable.global_variable_cache(0);
        let set_cache = frame.executable.global_variable_cache(1);

        assert_eq!(run_get_global(&vm, 0), Value::from_i32(1));
        let entry = get_cache.first_entry().expect("the get filled the cache");
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::GetOwnProperty);
        assert!(entry.shape == Some(frame.global.shape()));
        assert!(!get_cache.has_environment_binding_index.get());

        run_set_global(&vm, 0, Value::from_i32(2), true);
        let entry = set_cache.first_entry().expect("the set filled the cache");
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::ChangeOwnProperty);
        assert!(entry.writes_data_property);

        // The cached paths read and write the property in place.
        run_set_global(&vm, 0, Value::from_i32(3), true);
        assert_eq!(run_get_global(&vm, 0), Value::from_i32(3));
        assert_eq!(frame.global.get(&vm, &key("on_object")).must(), Value::from_i32(3));

        // A sloppy assignment to an undeclared name creates a global object property; a strict one throws, and so
        // does reading the name before it exists.
        assert!(
            thrown_message(|| run_get_global(&vm, 1))
                .contains("creating a ReferenceError with the message \"'undeclared' is not defined\"")
        );
        assert!(
            thrown_message(|| run_set_global(&vm, 1, Value::TRUE, true))
                .contains("creating a ReferenceError with the message \"'undeclared' is not defined\"")
        );
        run_set_global(&vm, 1, Value::TRUE, false);
        assert_eq!(frame.global.get(&vm, &key("undeclared")).must(), Value::TRUE);
    }

    #[test]
    fn global_variable_caches_remember_lexical_bindings_until_the_record_changes() {
        let vm = Vm::create();
        let frame = GlobalCodeFrame::new(&vm, &["lexical", "constant", "later"]);
        let global_environment = frame.global_environment();
        let declarative_record = global_environment.declarative_record();
        let lexical = Utf16FlyString::from_utf8("lexical");
        global_environment.create_mutable_binding(&vm, &lexical, false).must();
        global_environment
            .create_immutable_binding(&vm, &Utf16FlyString::from_utf8("constant"), true)
            .must();
        global_environment
            .initialize_binding(&vm, &lexical, Value::from_i32(1), InitializeBindingHint::Normal)
            .must();

        let get_cache = frame.executable.global_variable_cache(0);
        assert_eq!(run_get_global(&vm, 0), Value::from_i32(1));
        assert!(get_cache.has_environment_binding_index.get());
        assert_eq!(get_cache.environment_binding_index.get(), 0);
        assert_eq!(
            get_cache.environment_serial_number.get(),
            declarative_record.environment_serial_number()
        );

        run_set_global(&vm, 0, Value::from_i32(2), false);
        assert_eq!(run_get_global(&vm, 0), Value::from_i32(2));

        // A new lexical declaration changes the serial number, so the next access resolves the name again.
        global_environment
            .create_mutable_binding(&vm, &Utf16FlyString::from_utf8("later"), false)
            .must();
        assert_ne!(
            get_cache.environment_serial_number.get(),
            declarative_record.environment_serial_number()
        );
        assert_eq!(run_get_global(&vm, 0), Value::from_i32(2));
        assert_eq!(
            get_cache.environment_serial_number.get(),
            declarative_record.environment_serial_number()
        );

        // Bindings in their TDZ throw, and so do assignments to constants.
        assert!(
            thrown_message(|| run_get_global(&vm, 2))
                .contains("creating a ReferenceError with the message \"Binding later is not initialized\"")
        );
        global_environment
            .initialize_binding(
                &vm,
                &Utf16FlyString::from_utf8("constant"),
                Value::NULL,
                InitializeBindingHint::Normal,
            )
            .must();
        assert!(
            thrown_message(|| run_set_global(&vm, 1, Value::TRUE, false))
                .contains("creating a TypeError with the message \"Invalid assignment to const variable\"")
        );
    }

    #[test]
    fn bindings_are_created_resolved_and_deleted_through_their_environments() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let frame = GlobalCodeFrame::new(&vm, &["a", "b", "missing"]);
        let global_environment = frame.global_environment();
        let global_environment_value = Value::from_environment(global_environment);

        let mut values = op::CreateMutableBindingValues {
            environment: global_environment_value,
        };
        let instruction = op::CreateMutableBinding {
            header: header(false),
            environment: Operand(0),
            identifier: IdentifierTableIndex(0),
            can_be_deleted: false,
        };
        assert_eq!(
            create_mutable_binding(&vm, PC, &instruction, &mut values),
            SlowPathControl::continue_at(PC + op::CreateMutableBinding::LENGTH)
        );
        let mut values = op::CreateImmutableBindingValues {
            environment: global_environment_value,
        };
        let instruction = op::CreateImmutableBinding {
            header: header(false),
            environment: Operand(0),
            identifier: IdentifierTableIndex(1),
            strict_binding: true,
        };
        create_immutable_binding(&vm, PC, &instruction, &mut values);
        assert!(global_environment.has_lexical_declaration(&Utf16FlyString::from_utf8("a")));
        assert!(global_environment.has_lexical_declaration(&Utf16FlyString::from_utf8("b")));

        let resolve = |identifier: u32| {
            let mut values = op::ResolveBindingValues { dst: Value::EMPTY };
            let instruction = op::ResolveBinding {
                header: header(false),
                dst: Operand(0),
                identifier: IdentifierTableIndex(identifier),
            };
            resolve_binding(&vm, PC, &instruction, &mut values);
            values.dst
        };
        assert!(resolve(0).as_environment() == global_environment.upcast());
        assert_eq!(resolve(2), Value::NULL);

        // SetResolvedBinding puts through what ResolveBinding found, which for nothing is the global object.
        let set_resolved = |environment: Value, identifier: u32, value: Value| {
            let mut values = op::SetResolvedBindingValues {
                environment,
                src: value,
            };
            let instruction = op::SetResolvedBinding {
                header: header(false),
                environment: Operand(0),
                identifier: IdentifierTableIndex(identifier),
                src: Operand(1),
            };
            set_resolved_binding(&vm, PC, &instruction, &mut values)
        };
        let mut initialize_values = op::DynamicInitializeLexicalBindingValues {
            src: Value::from_i32(4),
        };
        let initialize = op::DynamicInitializeLexicalBinding {
            header: header(false),
            identifier: IdentifierTableIndex(0),
            src: Operand(0),
            cache: 0,
        };
        assert_eq!(
            dynamic_initialize_lexical_binding(&vm, PC, &initialize, &mut initialize_values),
            SlowPathControl::continue_at(PC + op::DynamicInitializeLexicalBinding::LENGTH)
        );
        set_resolved(resolve(0), 0, Value::from_i32(5));
        set_resolved(Value::NULL, 2, Value::from_i32(6));
        assert_eq!(run_get_global(&vm, 0), Value::from_i32(5));
        assert_eq!(frame.global.get(&vm, &key("missing")).must(), Value::from_i32(6));

        let typeof_of = |identifier: u32| {
            let mut values = op::DynamicTypeofBindingValues { dst: Value::EMPTY };
            let instruction = op::DynamicTypeofBinding {
                header: header(false),
                dst: Operand(0),
                identifier: IdentifierTableIndex(identifier),
                cache: 1,
            };
            dynamic_typeof_binding(&vm, PC, &instruction, &mut values);
            values.dst.as_string().to_utf8()
        };
        assert_eq!(typeof_of(0), "number");
        assert!(frame.global.internal_delete(&vm, &key("missing")).must());
        assert_eq!(typeof_of(2), "undefined");

        let delete = |identifier: u32| {
            let mut values = op::DeleteVariableValues { dst: Value::EMPTY };
            let instruction = op::DeleteVariable {
                header: header(false),
                dst: Operand(0),
                identifier: IdentifierTableIndex(identifier),
            };
            delete_variable(&vm, PC, &instruction, &mut values);
            values.dst
        };
        assert_eq!(delete(0), Value::FALSE);
        assert_eq!(delete(2), Value::TRUE);
    }

    #[test]
    fn with_statements_resolve_through_an_object_environment() {
        let vm = Vm::create();
        let frame = GlobalCodeFrame::new(&vm, &["property"]);
        let test_object = Object::create(&vm, frame.context().realm.get().expect("the frame has a realm"), None);
        test_object.define_direct_property(&vm, &key("property"), Value::from_i32(8), DEFAULT_ATTRIBUTES);

        let mut values = op::EnterObjectEnvironmentValues {
            dst: Value::EMPTY,
            object: Value::from_object(test_object),
        };
        let instruction = op::EnterObjectEnvironment {
            header: header(false),
            dst: Operand(0),
            object: Operand(1),
        };
        enter_object_environment(&vm, PC, &instruction, &mut values);
        let with_environment = values.dst.as_environment();
        assert!(frame.context().lexical_environment.get() == Some(with_environment));
        assert!(with_environment.is_object_environment());
        assert!(with_environment.with_base_object() == Some(test_object));
        assert!(with_environment.outer_environment() == Some(frame.global_environment().upcast()));

        // A call through a with binding gets the binding object as its this value.
        let mut values = op::DynamicGetCalleeAndThisFromEnvironmentValues {
            callee: Value::EMPTY,
            this_value: Value::EMPTY,
        };
        let instruction = op::DynamicGetCalleeAndThisFromEnvironment {
            header: header(false),
            callee: Operand(0),
            this_value: Operand(1),
            identifier: IdentifierTableIndex(0),
            cache: 0,
        };
        assert_eq!(
            dynamic_get_callee_and_this(&vm, PC, &instruction, &mut values),
            SlowPathControl::continue_at(PC + op::DynamicGetCalleeAndThisFromEnvironment::LENGTH)
        );
        assert_eq!(values.callee, Value::from_i32(8));
        assert_eq!(values.this_value, Value::from_object(test_object));
        // Object environments are never cached.
        assert!(!frame.executable.environment_coordinate_cache(0).get().is_valid());
    }

    #[test]
    fn dynamic_accesses_cache_declarative_coordinates() {
        let vm = Vm::create();
        let frame = GlobalCodeFrame::new(&vm, &["inner"]);
        let block = create_lexical_environment(
            &vm,
            frame.global_environment().upcast(),
            frame.executable.environment_shape_cache(0),
            1,
            false,
        );
        let inner = Utf16FlyString::from_utf8("inner");
        block.create_mutable_binding(&vm, &inner, false).must();
        block
            .initialize_binding(&vm, &inner, Value::from_i32(3), InitializeBindingHint::Normal)
            .must();
        let nested = DeclarativeEnvironment::create(&vm, Some(block.upcast()));
        frame.context().lexical_environment.set(Some(nested.upcast()));

        let get = || {
            let mut values = op::DynamicGetBindingValues { dst: Value::EMPTY };
            let instruction = op::DynamicGetBinding {
                header: header(false),
                dst: Operand(0),
                identifier: IdentifierTableIndex(0),
                cache: 1,
            };
            assert_eq!(
                dynamic_get_binding(&vm, PC, &instruction, &mut values),
                SlowPathControl::continue_at(PC + op::DynamicGetBinding::LENGTH)
            );
            values.dst
        };
        assert_eq!(get(), Value::from_i32(3));
        assert_eq!(
            frame.executable.environment_coordinate_cache(1).get(),
            EnvironmentCoordinate { hops: 1, index: 0 }
        );
        block
            .set_mutable_binding_direct(&vm, 0, Value::from_i32(4), true)
            .must();
        assert_eq!(get(), Value::from_i32(4));

        let mut values = op::SetLexicalBindingValues {
            src: Value::from_i32(5),
        };
        let instruction = op::SetLexicalBinding {
            header: header(true),
            identifier: IdentifierTableIndex(0),
            src: Operand(0),
            cache: EnvironmentCoordinate { hops: 1, index: 0 },
        };
        set_lexical_binding(&vm, PC, &instruction, &mut values);
        let mut values = op::GetBindingValues { dst: Value::EMPTY };
        let instruction = op::GetBinding {
            header: header(false),
            dst: Operand(0),
            identifier: IdentifierTableIndex(0),
            cache: EnvironmentCoordinate { hops: 1, index: 0 },
        };
        get_binding(&vm, PC, &instruction, &mut values);
        assert_eq!(values.dst, Value::from_i32(5));

        let verify = |hops: u32, index: u32| {
            let instruction = op::VerifyEnvironmentCoordinate {
                header: header(false),
                identifier: IdentifierTableIndex(0),
                coordinate: EnvironmentCoordinate { hops, index },
                mode: EnvironmentMode::Lexical as u32,
            };
            verify_environment_coordinate(&vm, PC, &instruction, &mut op::VerifyEnvironmentCoordinateValues {})
        };
        assert_eq!(
            verify(1, 0),
            SlowPathControl::continue_at(PC + op::VerifyEnvironmentCoordinate::LENGTH)
        );
        assert!(
            thrown_message(|| verify(0, 0)).contains("is wrong at hop 0: the environment has no binding at the index")
        );
        assert!(thrown_message(|| verify(2, 0)).contains("is wrong at hop 1: a closer environment binds the name"));
    }

    #[test]
    fn global_code_resolves_this_and_creates_private_environments() {
        let vm = Vm::create();
        let frame = GlobalCodeFrame::new(&vm, &[]);
        let this_register = frame.context().register(register::THIS_VALUE);
        assert!(this_register.get().is_empty());
        resolve_this_binding(
            &vm,
            PC,
            &op::ResolveThisBinding { header: header(false) },
            &mut op::ResolveThisBindingValues {},
        );
        assert_eq!(this_register.get(), Value::from_object(frame.global));

        assert!(frame.context().private_environment.get().is_none());
        let instruction = op::CreatePrivateEnvironment { header: header(false) };
        create_private_environment(&vm, PC, &instruction, &mut op::CreatePrivateEnvironmentValues {});
        let outer = frame
            .context()
            .private_environment
            .get()
            .expect("a private environment was created");
        create_private_environment(&vm, PC, &instruction, &mut op::CreatePrivateEnvironmentValues {});
        let inner = frame.context().private_environment.get().expect("a nested one too");
        assert!(inner.outer_environment() == Some(outer));

        // Global code has no function whose shared data could give the var environment a shape.
        let mut values = op::CreateVariableEnvironmentValues {};
        let instruction = op::CreateVariableEnvironment {
            header: header(false),
            capacity: 2,
        };
        create_variable_environment_slow_path(&vm, PC, &instruction, &mut values);
        let var_environment = frame
            .context()
            .variable_environment
            .get()
            .expect("the var environment was created");
        assert!(frame.context().lexical_environment.get() == Some(var_environment));
        assert!(var_environment.outer_environment() == Some(frame.global_environment().upcast()));
    }
}
