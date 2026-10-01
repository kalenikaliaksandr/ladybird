/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/AbstractOperations.cpp the runtime has so far.

use ak::{ScopeGuard, Utf16FlyString};

use crate::bytecode::executable::StaticPropertyLookupCacheSite;
use crate::gc::gc_ref_cell::GcRefCell;
use crate::gc::root::MarkedVec;
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::function_object::EcmascriptFunctionObject;
use crate::layout::function_object::FunctionObject;
use crate::layout::value::Value;
use crate::runtime::accessor::Accessor;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::declarative_environment::DeclarativeEnvironment;
use crate::runtime::environment::{Environment, InitializeBindingHint, ThisBindingStatus};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::function_environment::FunctionEnvironment;
use crate::runtime::indexed_properties::ValueAndAttributes;
use crate::runtime::object::{Object, StackFrameInfo};
use crate::runtime::private_environment::PrivateEnvironment;
use crate::runtime::property_attributes::PropertyAttributes;
use crate::runtime::property_descriptor::PropertyDescriptor;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::value::same_value;
use libjs_runtime_macros::Trace;

/// The Object a function object starts with.
pub fn function_object_as_object(function: Gc<FunctionObject>) -> Gc<Object> {
    // SAFETY: FunctionObject is #[repr(C)] and starts with its Object.
    unsafe { Gc::from_non_null(function.as_non_null().cast()) }
}

// 7.2.1 RequireObjectCoercible ( argument ), https://tc39.es/ecma262/#sec-requireobjectcoercible
pub fn require_object_coercible(vm: &Vm, value: Value) -> ThrowCompletionOr<Value> {
    if value.is_nullish() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotObjectCoercible, &[&value]);
    }
    Ok(value)
}

// 7.2.3 IsCallable ( argument ), https://tc39.es/ecma262/#sec-iscallable
pub fn is_callable(argument: Value) -> bool {
    argument.is_function()
}

// 7.2.4 IsConstructor ( argument ), https://tc39.es/ecma262/#sec-isconstructor
pub fn is_constructor(argument: Value) -> bool {
    argument.is_constructor()
}

/// Allocates the callee's frame on the interpreter stack, as call_impl and construct_impl do, and runs `body` with
/// it. The frame is freed when `body` returns.
fn with_callee_context<T>(
    vm: &Vm,
    function: &Object,
    arguments_list: &[Value],
    body: impl FnOnce(&crate::layout::execution_context::ExecutionContext) -> ThrowCompletionOr<T>,
) -> ThrowCompletionOr<T> {
    let mut stack_frame_info = StackFrameInfo {
        argument_count: u32::try_from(arguments_list.len()).expect("the argument count fits in u32"),
        ..Default::default()
    };
    function.get_stack_frame_info(&mut stack_frame_info);

    let stack = vm.interpreter_stack();
    let stack_mark = stack.top.get();
    let Some(callee_context) = stack.allocate(
        stack_frame_info.registers_and_locals_count,
        stack_frame_info.constant_count,
        stack_frame_info.argument_count,
    ) else {
        return vm.throw_completion(ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
    };
    let _deallocate_guard = ScopeGuard::new(|| stack.deallocate(stack_mark));

    // SAFETY: The frame was just allocated and stays allocated until the guard frees it.
    let callee_context = unsafe { callee_context.as_ref() };
    for (index, argument) in callee_context.arguments().iter().enumerate() {
        argument.set(arguments_list.get(index).copied().unwrap_or(Value::UNDEFINED));
    }
    callee_context.passed_argument_count.set(arguments_list.len() as u32);

    body(callee_context)
}

// 7.3.14 Call ( F, V [ , argumentsList ] ), https://tc39.es/ecma262/#sec-call
pub fn call(vm: &Vm, function: Value, this_value: Value, arguments_list: &[Value]) -> ThrowCompletionOr<Value> {
    // 1. If argumentsList is not present, set argumentsList to a new empty List.

    // 2. If IsCallable(F) is false, throw a TypeError exception.
    if !function.is_function() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAFunction, &[&function]);
    }

    // 3. Return ? F.[[Call]](V, argumentsList).
    call_function_object(vm, function.as_function(), this_value, arguments_list)
}

// 7.3.14 Call ( F, V [ , argumentsList ] ), https://tc39.es/ecma262/#sec-call
pub fn call_function_object(
    vm: &Vm,
    function: Gc<FunctionObject>,
    this_value: Value,
    arguments_list: &[Value],
) -> ThrowCompletionOr<Value> {
    // 1. If argumentsList is not present, set argumentsList to a new empty List.

    // 2. If IsCallable(F) is false, throw a TypeError exception.
    // Note: Called with a FunctionObject ref

    // 3. Return ? F.[[Call]](V, argumentsList).
    let function = function_object_as_object(function);
    let Some(internal_call) = function.internal_call_method() else {
        unimplemented_runtime_function(&format!("[[Call]] of a {}", function.class().name), 0);
    };
    with_callee_context(vm, &function, arguments_list, |callee_context| {
        internal_call(&function, vm, callee_context, this_value)
    })
}

// 7.3.15 Construct ( F [ , argumentsList [ , newTarget ] ] ), https://tc39.es/ecma262/#sec-construct
pub fn construct(
    vm: &Vm,
    function: Gc<FunctionObject>,
    arguments_list: &[Value],
    new_target: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<Gc<Object>> {
    // 1. If newTarget is not present, set newTarget to F.
    let new_target = new_target.unwrap_or(function);

    // 2. If argumentsList is not present, set argumentsList to a new empty List.

    // 3. Return ? F.[[Construct]](argumentsList, newTarget).
    let function = function_object_as_object(function);
    let Some(internal_construct) = function.internal_construct_method() else {
        unimplemented_runtime_function(&format!("[[Construct]] of a {}", function.class().name), 0);
    };
    with_callee_context(vm, &function, arguments_list, |callee_context| {
        internal_construct(&function, vm, callee_context, new_target)
    })
}

// 7.3.19 LengthOfArrayLike ( obj ), https://tc39.es/ecma262/#sec-lengthofarraylike
pub fn length_of_array_like(vm: &Vm, object: &Object) -> ThrowCompletionOr<u64> {
    // OPTIMIZATION: For Array objects with a magical "length" property, it should always reflect the size of indexed property storage.
    if object.has_magical_length_property() {
        return Ok(u64::from(object.indexed_array_like_size()));
    }

    // 1. Return ℝ(? ToLength(? Get(obj, "length"))).
    object
        .get_with_cache(
            vm,
            &vm.names.length,
            vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::LengthOfArrayLike),
        )?
        .to_length(vm)
}

// 7.3.20 CreateListFromArrayLike ( obj [ , elementTypes ] ), https://tc39.es/ecma262/#sec-createlistfromarraylike
pub fn create_list_from_array_like<'vm>(
    vm: &'vm Vm,
    value: Value,
    check_value: Option<&dyn Fn(Value) -> ThrowCompletionOr<()>>,
) -> ThrowCompletionOr<MarkedVec<'vm, Value>> {
    // 1. If elementTypes is not present, set elementTypes to « Undefined, Null, Boolean, String, Symbol, Number, BigInt, Object ».

    // 2. If Type(obj) is not Object, throw a TypeError exception.
    if !value.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&value]);
    }

    let array_like = value.as_object();

    // 3. Let len be ? LengthOfArrayLike(obj).
    let length = length_of_array_like(vm, &array_like)?;

    // 4. Let list be a new empty List.
    let list = MarkedVec::with_capacity(vm, length as usize);

    // 5. Let index be 0.
    // 6. Repeat, while index < len,
    for index in 0..length {
        // a. Let indexName be ! ToString(𝔽(index)).
        let index_name = PropertyKey::from_number(index);

        // b. Let next be ? Get(obj, indexName).
        let next = array_like.get(vm, &index_name)?;

        // c. If Type(next) is not an element of elementTypes, throw a TypeError exception.
        if let Some(check_value) = check_value {
            check_value(next)?;
        }

        // d. Append next as the last element of list.
        list.push(next);
    }

    // 7. Return list.
    Ok(list)
}

// 7.3.23 SpeciesConstructor ( O, defaultConstructor ), https://tc39.es/ecma262/#sec-speciesconstructor
pub fn species_constructor(
    vm: &Vm,
    object: &Object,
    default_constructor: Gc<FunctionObject>,
) -> ThrowCompletionOr<Gc<FunctionObject>> {
    // 1. Let C be ? Get(O, "constructor").
    let constructor = object.get_with_cache(
        vm,
        &vm.names.constructor,
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::SpeciesConstructorConstructor),
    )?;

    // 2. If C is undefined, return defaultConstructor.
    if constructor.is_undefined() {
        return Ok(default_constructor);
    }

    // 3. If Type(C) is not Object, throw a TypeError exception.
    if !constructor.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAConstructor, &[&constructor]);
    }

    // 4. Let S be ? Get(C, @@species).
    let species = constructor.as_object().get_with_cache(
        vm,
        &PropertyKey::from(vm.well_known_symbols().species),
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::SpeciesConstructorSpecies),
    )?;

    // 5. If S is either undefined or null, return defaultConstructor.
    if species.is_nullish() {
        return Ok(default_constructor);
    }

    // 6. If IsConstructor(S) is true, return S.
    if species.is_constructor() {
        return Ok(species.as_function());
    }

    // 7. Throw a TypeError exception.
    vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAConstructor, &[&species])
}

// 10.1.6.2 IsCompatiblePropertyDescriptor ( Extensible, Desc, Current ), https://tc39.es/ecma262/#sec-iscompatiblepropertydescriptor
pub fn is_compatible_property_descriptor(
    vm: &Vm,
    extensible: bool,
    descriptor: &mut PropertyDescriptor,
    current: &Option<PropertyDescriptor>,
) -> bool {
    // 1. Return ValidateAndApplyPropertyDescriptor(undefined, "", Extensible, Desc, Current).
    validate_and_apply_property_descriptor(
        vm,
        None,
        &PropertyKey::from(Utf16FlyString::default()),
        extensible,
        descriptor,
        current,
    )
}

// 10.1.6.3 ValidateAndApplyPropertyDescriptor ( O, P, extensible, Desc, current ), https://tc39.es/ecma262/#sec-validateandapplypropertydescriptor
pub fn validate_and_apply_property_descriptor(
    vm: &Vm,
    object: Option<&Object>,
    property_key: &PropertyKey,
    extensible: bool,
    descriptor: &mut PropertyDescriptor,
    current: &Option<PropertyDescriptor>,
) -> bool {
    // 1. Assert: IsPropertyKey(P) is true.

    // 2. If current is undefined, then
    let Some(current) = current else {
        // a. If extensible is false, return false.
        if !extensible {
            return false;
        }

        // b. If O is undefined, return true.
        let Some(object) = object else {
            return true;
        };

        // c. If IsAccessorDescriptor(Desc) is true, then
        if descriptor.is_accessor_descriptor() {
            // i. Create an own accessor property named P of object O whose [[Get]], [[Set]], [[Enumerable]], and [[Configurable]] attributes are set to the value of the corresponding field in Desc if Desc has that field, or to the attribute's default value otherwise.
            let accessor = Accessor::create(vm, descriptor.get.unwrap_or(None), descriptor.set.unwrap_or(None), None);
            let offset = object.storage_add(
                vm,
                property_key,
                ValueAndAttributes::new(Value::from_accessor(accessor), descriptor.attributes()),
            );
            descriptor.property_offset = offset;
        }
        // d. Else,
        else {
            // i. Create an own data property named P of object O whose [[Value]], [[Writable]], [[Enumerable]], and [[Configurable]] attributes are set to the value of the corresponding field in Desc if Desc has that field, or to the attribute's default value otherwise.
            let value = descriptor.value.unwrap_or(Value::UNDEFINED);
            let offset = object.storage_add(
                vm,
                property_key,
                ValueAndAttributes::new(value, descriptor.attributes()),
            );
            descriptor.property_offset = offset;
        }

        // e. Return true.
        return true;
    };

    // 3. Assert: current is a fully populated Property Descriptor.
    let fully_populated = |field: Option<bool>| field.expect("the current descriptor is fully populated");

    // 4. If Desc does not have any fields, return true.
    if descriptor.is_empty() {
        return true;
    }

    let current_configurable = fully_populated(current.configurable);

    // 5. If current.[[Configurable]] is false, then
    if !current_configurable {
        // a. If Desc has a [[Configurable]] field and Desc.[[Configurable]] is true, return false.
        if descriptor.configurable == Some(true) {
            return false;
        }

        // b. If Desc has an [[Enumerable]] field and SameValue(Desc.[[Enumerable]], current.[[Enumerable]]) is false, return false.
        if descriptor
            .enumerable
            .is_some_and(|enumerable| enumerable != fully_populated(current.enumerable))
        {
            return false;
        }

        // c. If IsGenericDescriptor(Desc) is false and SameValue(IsAccessorDescriptor(Desc), IsAccessorDescriptor(current)) is false, return false.
        if !descriptor.is_generic_descriptor()
            && (descriptor.is_accessor_descriptor() != current.is_accessor_descriptor())
        {
            return false;
        }

        // d. If IsAccessorDescriptor(current) is true, then
        if current.is_accessor_descriptor() {
            // i. If Desc has a [[Get]] field and SameValue(Desc.[[Get]], current.[[Get]]) is false, return false.
            if descriptor
                .get
                .is_some_and(|get| get != current.get.expect("the current descriptor is fully populated"))
            {
                return false;
            }

            // ii. If Desc has a [[Set]] field and SameValue(Desc.[[Set]], current.[[Set]]) is false, return false.
            if descriptor
                .set
                .is_some_and(|set| set != current.set.expect("the current descriptor is fully populated"))
            {
                return false;
            }
        }
        // e. Else if current.[[Writable]] is false, then
        else if !fully_populated(current.writable) {
            // i. If Desc has a [[Writable]] field and Desc.[[Writable]] is true, return false.
            if descriptor.writable == Some(true) {
                return false;
            }

            // ii. If Desc has a [[Value]] field and SameValue(Desc.[[Value]], current.[[Value]]) is false, return false.
            if descriptor.value.is_some_and(|value| {
                !same_value(value, current.value.expect("the current descriptor is fully populated"))
            }) {
                return false;
            }
        }
    }

    // 6. If O is not undefined, then
    if let Some(object) = object {
        // a. If IsDataDescriptor(current) is true and IsAccessorDescriptor(Desc) is true, then
        if current.is_data_descriptor() && descriptor.is_accessor_descriptor() {
            // i. If Desc has a [[Configurable]] field, let configurable be Desc.[[Configurable]], else let configurable be current.[[Configurable]].
            let configurable = descriptor.configurable.unwrap_or(current_configurable);

            // ii. If Desc has a [[Enumerable]] field, let enumerable be Desc.[[Enumerable]], else let enumerable be current.[[Enumerable]].
            let enumerable = descriptor.enumerable.unwrap_or(fully_populated(current.enumerable));

            // iii. Replace the property named P of object O with an accessor property having [[Configurable]] and [[Enumerable]] attributes set to configurable and enumerable, respectively, and each other attribute set to its corresponding value in Desc if present, otherwise to its default value.
            let accessor = Accessor::create(vm, descriptor.get.unwrap_or(None), descriptor.set.unwrap_or(None), None);
            let mut attributes = PropertyAttributes::default();
            attributes.set_enumerable(enumerable);
            attributes.set_configurable(configurable);
            let offset = object.storage_set(
                vm,
                property_key,
                ValueAndAttributes::new(Value::from_accessor(accessor), attributes),
            );
            descriptor.property_offset = offset;
        }
        // b. Else if IsAccessorDescriptor(current) is true and IsDataDescriptor(Desc) is true, then
        else if current.is_accessor_descriptor() && descriptor.is_data_descriptor() {
            // i. If Desc has a [[Configurable]] field, let configurable be Desc.[[Configurable]], else let configurable be current.[[Configurable]].
            let configurable = descriptor.configurable.unwrap_or(current_configurable);

            // ii. If Desc has a [[Enumerable]] field, let enumerable be Desc.[[Enumerable]], else let enumerable be current.[[Enumerable]].
            let enumerable = descriptor.enumerable.unwrap_or(fully_populated(current.enumerable));

            // iii. Replace the property named P of object O with a data property having [[Configurable]] and [[Enumerable]] attributes set to configurable and enumerable, respectively, and each other attribute set to its corresponding value in Desc if present, otherwise to its default value.
            let value = descriptor.value.unwrap_or(Value::UNDEFINED);
            let mut attributes = PropertyAttributes::default();
            attributes.set_writable(descriptor.writable.unwrap_or(false));
            attributes.set_enumerable(enumerable);
            attributes.set_configurable(configurable);
            let offset = object.storage_set(vm, property_key, ValueAndAttributes::new(value, attributes));
            descriptor.property_offset = offset;
        }
        // c. Else,
        else {
            // i. For each field of Desc, set the corresponding attribute of the property named P of object O to the value of the field.
            let value = if descriptor.is_accessor_descriptor()
                || (current.is_accessor_descriptor() && !descriptor.is_data_descriptor())
            {
                let getter = descriptor.get.unwrap_or(current.get.unwrap_or(None));
                let setter = descriptor.set.unwrap_or(current.set.unwrap_or(None));
                Value::from_accessor(Accessor::create(vm, getter, setter, None))
            } else {
                descriptor.value.unwrap_or(current.value.unwrap_or(Value::UNDEFINED))
            };
            let mut attributes = PropertyAttributes::default();
            attributes.set_writable(descriptor.writable.unwrap_or(current.writable.unwrap_or(false)));
            attributes.set_enumerable(descriptor.enumerable.unwrap_or(current.enumerable.unwrap_or(false)));
            attributes.set_configurable(descriptor.configurable.unwrap_or(current.configurable.unwrap_or(false)));
            let offset = object.storage_set(vm, property_key, ValueAndAttributes::new(value, attributes));
            descriptor.property_offset = offset;
        }
    }

    // 7. Return true.
    true
}

// 9.2.1.1 NewPrivateEnvironment ( outerPrivEnv ), https://tc39.es/ecma262/#sec-newprivateenvironment
pub fn new_private_environment(vm: &Vm, outer: Option<Gc<PrivateEnvironment>>) -> Gc<PrivateEnvironment> {
    // 1. Let names be a new empty List.
    // 2. Return the PrivateEnvironment Record { [[OuterPrivateEnvironment]]: outerPrivEnv, [[Names]]: names }.
    PrivateEnvironment::create(vm, outer)
}

// 9.1.2.2 NewDeclarativeEnvironment ( E ), https://tc39.es/ecma262/#sec-newdeclarativeenvironment
// 4.1.2.1 NewDeclarativeEnvironment ( E ), https://tc39.es/proposal-explicit-resource-management/#sec-declarative-environment-records-initializebinding-n-v
pub fn new_declarative_environment(vm: &Vm, environment: Gc<Environment>) -> Gc<DeclarativeEnvironment> {
    // 1. Let env be a new Declarative Environment Record containing no bindings.
    // 2. Set env.[[OuterEnv]] to E.
    // 3. Set env.[[DisposeCapability]] to NewDisposeCapability().
    // 4. Return env.
    DeclarativeEnvironment::create(vm, Some(environment))
}

/// ECMAScriptFunctionObject::environment(), until ECMAScript function objects are cells of the runtime.
fn ecmascript_function_object_environment(function: Gc<EcmascriptFunctionObject>) -> Option<Gc<Environment>> {
    // SAFETY: A Gc points to a live cell.
    unsafe { function.as_non_null().as_ref() }.environment.get()
}

/// Whether F.[[ThisMode]] is lexical, which an ECMAScript function object keeps in its shared data.
fn ecmascript_function_object_this_mode_is_lexical(_function: Gc<EcmascriptFunctionObject>) -> bool {
    unimplemented_runtime_function("ECMAScriptFunctionObject::this_mode, for NewFunctionEnvironment", 0)
}

fn native_javascript_backed_function_this_mode_is_lexical(_function: Gc<FunctionObject>) -> bool {
    unimplemented_runtime_function(
        "NativeJavaScriptBackedFunction::this_mode, for NewFunctionEnvironment",
        0,
    )
}

// 9.1.2.4 NewFunctionEnvironment ( F, newTarget ), https://tc39.es/ecma262/#sec-newfunctionenvironment
// 4.1.2.2 NewFunctionEnvironment ( F, newTarget ), https://tc39.es/proposal-explicit-resource-management/#sec-newfunctionenvironment
pub fn new_function_environment(
    vm: &Vm,
    function: Gc<EcmascriptFunctionObject>,
    new_target: Option<Gc<Object>>,
) -> Gc<FunctionEnvironment> {
    // 1. Let env be a new function Environment Record containing no bindings.
    let env = FunctionEnvironment::create(vm, ecmascript_function_object_environment(function));

    // 2. Set env.[[FunctionObject]] to F.
    // SAFETY: An ECMAScript function object starts with its FunctionObject.
    env.set_function_object(unsafe { Gc::from_non_null(function.as_non_null().cast()) });

    if ecmascript_function_object_this_mode_is_lexical(function) {
        // 3. If F.[[ThisMode]] is lexical, set env.[[ThisBindingStatus]] to lexical.
        env.set_this_binding_status(ThisBindingStatus::Lexical);
    } else {
        // 4. Else, set env.[[ThisBindingStatus]] to uninitialized.
        env.set_this_binding_status(ThisBindingStatus::Uninitialized);
    }

    // 5. Set env.[[NewTarget]] to newTarget.
    env.set_new_target(new_target.map_or(Value::UNDEFINED, Value::from_object));

    // 6. Set env.[[OuterEnv]] to F.[[Environment]].
    // 7. Set env.[[DisposeCapability]] to NewDisposeCapability().
    // NOTE: Done in step 1 via the FunctionEnvironment constructor.

    // 8. Return env.
    env
}

// 9.1.2.4 NewFunctionEnvironment ( F, newTarget ), https://tc39.es/ecma262/#sec-newfunctionenvironment
// 4.1.2.2 NewFunctionEnvironment ( F, newTarget ), https://tc39.es/proposal-explicit-resource-management/#sec-newfunctionenvironment
pub fn new_function_environment_for_native_javascript_backed_function(
    vm: &Vm,
    function: Gc<FunctionObject>,
    new_target: Option<Gc<Object>>,
) -> Gc<FunctionEnvironment> {
    // 1. Let env be a new function Environment Record containing no bindings.
    let env = FunctionEnvironment::create(vm, None);

    // 2. Set env.[[FunctionObject]] to F.
    env.set_function_object(function);

    if native_javascript_backed_function_this_mode_is_lexical(function) {
        // 3. If F.[[ThisMode]] is lexical, set env.[[ThisBindingStatus]] to lexical.
        env.set_this_binding_status(ThisBindingStatus::Lexical);
    } else {
        // 4. Else, set env.[[ThisBindingStatus]] to uninitialized.
        env.set_this_binding_status(ThisBindingStatus::Uninitialized);
    }

    // 5. Set env.[[NewTarget]] to newTarget.
    env.set_new_target(new_target.map_or(Value::UNDEFINED, Value::from_object));

    // 6. Set env.[[OuterEnv]] to F.[[Environment]].
    // 7. Set env.[[DisposeCapability]] to NewDisposeCapability().
    // NOTE: Done in step 1 via the FunctionEnvironment constructor.

    // 8. Return env.
    env
}

// 9.4.3 GetThisEnvironment ( ), https://tc39.es/ecma262/#sec-getthisenvironment
pub fn get_this_environment(vm: &Vm) -> Gc<Environment> {
    let context = vm
        .running_execution_context()
        .expect("GetThisEnvironment runs in an execution context");

    // 1. Let env be the running execution context's LexicalEnvironment.
    // SAFETY: The running execution context is live.
    let mut env = unsafe { context.as_ref() }.lexical_environment.get();

    // 2. Repeat,
    while let Some(environment) = env {
        // a. Let exists be env.HasThisBinding().
        // b. If exists is true, return env.
        if environment.has_this_binding() {
            return environment;
        }

        // c. Let outer be env.[[OuterEnv]].
        // d. Assert: outer is not null.
        // e. Set env to outer.
        env = environment.outer_environment();
    }
    unreachable!("the outermost environment has a this binding");
}

// 2.1.1 DisposeCapability Records, https://tc39.es/proposal-explicit-resource-management/#sec-disposecapability-records
#[derive(Default, Trace)]
pub struct DisposeCapability {
    pub disposable_resource_stack: Option<Vec<DisposableResource>>, // [[DisposableResourceStack]]
}

// 2.1.2 DisposableResource Records, https://tc39.es/proposal-explicit-resource-management/#sec-disposableresource-records
#[derive(Clone, Copy, Trace)]
pub struct DisposableResource {
    pub resource_value: Option<Gc<Object>>, // [[ResourceValue]]
    #[gc(untraced)]
    pub hint: InitializeBindingHint, // [[Hint]]
    pub dispose_method: Option<Gc<FunctionObject>>, // [[DisposeMethod]]
}

// 2.1.3 NewDisposeCapability ( ), https://tc39.es/proposal-explicit-resource-management/#sec-newdisposecapability
pub fn new_dispose_capability() -> DisposeCapability {
    // 1. Let stack be a new empty List.
    // 2. Return the DisposeCapability Record { [[DisposableResourceStack]]: stack }.
    DisposeCapability::default()
}

// 2.1.4 AddDisposableResource ( disposeCapability, V, hint [ , method ] ), https://tc39.es/proposal-explicit-resource-management/#sec-adddisposableresource-disposable-v-hint-disposemethod
pub fn add_disposable_resource(
    vm: &Vm,
    dispose_capability: &GcRefCell<DisposeCapability>,
    value: Value,
    hint: InitializeBindingHint,
    method: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<()> {
    let resource = match method {
        // 1. If method is not present then,
        None => {
            // a. If V is either null or undefined and hint is sync-dispose, then
            if value.is_nullish() && hint == InitializeBindingHint::SyncDispose {
                // i. Return unused.
                return Ok(());
            }

            // b. NOTE: When V is either null or undefined and hint is async-dispose, we record that the resource was evaluated
            //    to ensure we will still perform an Await when resources are later disposed.

            // c. Let resource be ? CreateDisposableResource(V, hint).
            create_disposable_resource(vm, value, hint, None)?
        }
        // 2. Else,
        Some(method) => {
            // a. Assert: V is undefined.
            assert!(value.is_undefined());

            // b. Let resource be ? CreateDisposableResource(undefined, hint, method).
            create_disposable_resource(vm, Value::UNDEFINED, hint, Some(method))?
        }
    };

    // 3. Append resource to disposeCapability.[[DisposableResourceStack]].
    // NB: Creating the resource can run JavaScript, so the capability is only borrowed to append to it.
    dispose_capability
        .borrow_mut()
        .disposable_resource_stack
        .get_or_insert_with(Vec::new)
        .push(resource);

    // 4. Return unused.
    Ok(())
}

// 2.1.5 CreateDisposableResource ( V, hint [ , method ] ), https://tc39.es/proposal-explicit-resource-management/#sec-createdisposableresource
pub fn create_disposable_resource(
    vm: &Vm,
    value: Value,
    hint: InitializeBindingHint,
    method: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<DisposableResource> {
    let mut method = method;

    // 1. If method is not present, then
    // a. If V is either null or undefined, then
    //    i. Set V to undefined.
    //    ii. Set method to undefined.
    // b. Else,
    if method.is_none() && !value.is_nullish() {
        // i. If V is not an Object, throw a TypeError exception.
        if !value.is_object() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&value]);
        }

        // ii. Set method to ? GetDisposeMethod(V, hint).
        method = get_dispose_method(vm, value, hint)?;

        // iii. If method is undefined, throw a TypeError exception.
        if method.is_none() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NoDisposeMethod, &[&value]);
        }
    }
    // 2. Else,
    //    a. If IsCallable(method) is false, throw a TypeError exception.
    //    NOTE: This is guaranteed to never occur due to its type.

    // 3. Return the DisposableResource Record { [[ResourceValue]]: V, [[Hint]]: hint, [[DisposeMethod]]: method }.
    Ok(DisposableResource {
        resource_value: value.is_object().then(|| value.as_object()),
        hint,
        dispose_method: method,
    })
}

// 2.1.6 GetDisposeMethod ( V, hint ), https://tc39.es/proposal-explicit-resource-management/#sec-getdisposemethod
pub fn get_dispose_method(
    vm: &Vm,
    value: Value,
    hint: InitializeBindingHint,
) -> ThrowCompletionOr<Option<Gc<FunctionObject>>> {
    // 1. If hint is async-dispose, then
    if hint == InitializeBindingHint::AsyncDispose {
        // a. Let method be ? GetMethod(V, @@asyncDispose).
        let method = value.get_method(vm, &PropertyKey::from(vm.well_known_symbols().async_dispose))?;

        // b. If method is undefined, then
        if method.is_none() {
            // i. Set method to ? GetMethod(V, @@dispose).
            let method = value.get_method(vm, &PropertyKey::from(vm.well_known_symbols().dispose))?;

            // ii. If method is not undefined, then
            if method.is_some() {
                // 1. Let closure be a new Abstract Closure with no parameters that captures method and performs the
                //    following steps when called: ...
                // 3. Return CreateBuiltinFunction(closure, 0, "", « »).
                unimplemented_runtime_function("the async-dispose wrapper of a @@dispose method", 0);
            }
            return Ok(None);
        }

        // 3. Return method.
        return Ok(method);
    }

    // 2. Else,
    //    a. Let method be ? GetMethod(V, @@dispose).
    // 3. Return method.
    value.get_method(vm, &PropertyKey::from(vm.well_known_symbols().dispose))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use ak::Utf16FlyString;

    use super::*;

    #[test]
    fn using_null_or_undefined_adds_no_sync_resource_but_records_async_ones() {
        let vm = Vm::create();
        let environment = new_declarative_environment(&vm, DeclarativeEnvironment::create(&vm, None).upcast());
        let (sync, async_) = (Utf16FlyString::from_utf8("sync"), Utf16FlyString::from_utf8("async"));
        environment.create_immutable_binding(&vm, &sync, true).unwrap();
        environment.create_immutable_binding(&vm, &async_, true).unwrap();

        environment
            .initialize_binding(&vm, &sync, Value::NULL, InitializeBindingHint::SyncDispose)
            .unwrap();
        assert!(environment.dispose_capability_if_exists().is_none());

        environment
            .initialize_binding(&vm, &async_, Value::UNDEFINED, InitializeBindingHint::AsyncDispose)
            .unwrap();
        let dispose_capability = environment
            .dispose_capability_if_exists()
            .expect("the async resource was recorded");
        let dispose_capability = dispose_capability.borrow();
        let stack = dispose_capability.disposable_resource_stack.as_ref().unwrap();
        assert_eq!(stack.len(), 1);
        assert!(stack[0].resource_value.is_none() && stack[0].dispose_method.is_none());
        assert_eq!(stack[0].hint, InitializeBindingHint::AsyncDispose);
        assert_eq!(
            environment.get_binding_value(&vm, &async_, true).unwrap(),
            Value::UNDEFINED
        );
    }

    #[test]
    fn the_this_environment_is_the_closest_one_with_a_this_binding() {
        let vm = Vm::create();
        let stack = vm.interpreter_stack();
        let mark = stack.top.get();
        let context = stack.allocate(0, 0, 0).expect("the stack has room");
        vm.push_execution_context(context);

        let function = FunctionEnvironment::create(&vm, None);
        let arrow = FunctionEnvironment::create(&vm, Some(function.upcast()));
        arrow.set_this_binding_status(ThisBindingStatus::Lexical);
        let block = new_declarative_environment(&vm, arrow.upcast());
        // SAFETY: The context was just pushed.
        unsafe { context.as_ref() }
            .lexical_environment
            .set(Some(block.upcast()));
        assert!(get_this_environment(&vm) == function.upcast());

        vm.pop_execution_context();
        stack.deallocate(mark);
    }
}
