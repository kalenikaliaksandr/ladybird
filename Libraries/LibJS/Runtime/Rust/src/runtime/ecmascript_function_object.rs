/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ops::Deref;
use core::ptr::NonNull;
use std::rc::Rc;

use ak::{Utf16FlyString, Utf16String};
use libjs_runtime_macros::Trace;

use crate::bytecode::executable::Executable;
use crate::gc::class::{Extends, GcCell, define_cell};
use crate::gc::gc_ref_cell::GcRefCell;
use crate::gc::root::MarkedVec;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::run::should_dump_bytecode;
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::environment::{Environment, PrivateEnvironment};
use crate::layout::execution_context::{ExecutionContext, ScriptOrModule};
pub use crate::layout::function_object::EcmascriptFunctionObject;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::layout_forward::FlyStringSlot;
use crate::runtime::abstract_operations::{
    create_unmapped_arguments_object, new_function_environment, ordinary_create_from_constructor,
};
use crate::runtime::class_field_definition::{ClassElementName, ClassFieldDefinition};
use crate::runtime::completion::{Must, Throw, ThrowCompletionOr};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::function_environment::FunctionEnvironment;
use crate::runtime::function_object::{FUNCTION_OBJECT_METHODS, FunctionObject};
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::object::{
    MayInterfereWithIndexedPropertyAccess, ObjectMethods, PrivateElement, StackFrameInfo, allocate_object,
};
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::property_descriptor::PropertyDescriptor;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;
use crate::runtime::shared_function_instance_data::{
    ClassFieldInitializerName, ConstructorKind, FunctionKind, SharedFunctionInstanceData, ThisMode,
};
use crate::source_code::SourceCode;
use crate::utf16::{Utf16View, to_utf16_fly_string};

#[derive(Default, Trace)]
struct ClassData {
    fields: Vec<ClassFieldDefinition>,    // [[Fields]]
    private_methods: Vec<PrivateElement>, // [[PrivateMethods]]
}

/// The parts of an ECMAScript function object the interpreter does not read.
#[derive(Default, Trace)]
pub struct EcmascriptFunctionObjectStorage {
    class_data: GcRefCell<Option<Box<ClassData>>>,
    may_need_lazy_prototype_instantiation: Cell<bool>,
    is_method: Cell<bool>,
}

pub static ECMASCRIPT_FUNCTION_OBJECT_METHODS: ObjectMethods = ObjectMethods {
    internal_get_own_property: EcmascriptFunctionObject::internal_get_own_property,
    is_cacheable_for_property_absence: |_| false,
    internal_own_property_keys: EcmascriptFunctionObject::internal_own_property_keys,
    internal_call: Some(EcmascriptFunctionObject::internal_call),
    internal_construct: Some(EcmascriptFunctionObject::internal_construct),
    has_constructor: |object| as_ecmascript_function(object).has_constructor(),
    is_strict_mode: |object| as_ecmascript_function(object).shared_data().strict(),
    get_stack_frame_info: EcmascriptFunctionObject::get_stack_frame_info,
    function_realm: |object| Some(object.shape().realm()),
    name_for_call_stack: |object| as_ecmascript_function(object).name_for_call_stack(),
    ..FUNCTION_OBJECT_METHODS
};

define_cell!(
    EcmascriptFunctionObject,
    Object,
    extends: [FunctionObject, Object],
    methods: ECMASCRIPT_FUNCTION_OBJECT_METHODS
);

// SAFETY: Visits every cell an ECMAScript function object holds, besides those of its FunctionObject.
unsafe impl Trace for EcmascriptFunctionObject {
    fn trace(&self, visitor: &mut Visitor) {
        self.base.trace(visitor);
        self.environment.trace(visitor);
        self.private_environment.trace(visitor);
        self.home_object.trace(visitor);
        self.name_string.trace(visitor);
        self.shared_data.trace(visitor);
        self.storage.trace(visitor);
        match self.script_or_module.get() {
            ScriptOrModule::Empty => {}
            ScriptOrModule::Script(script) => script.trace(visitor),
            ScriptOrModule::Module(module) => module.trace(visitor),
        }
    }
}

impl Deref for EcmascriptFunctionObject {
    type Target = FunctionObject;

    fn deref(&self) -> &FunctionObject {
        &self.base
    }
}

/// The function an internal method of an ECMAScript function object was called on.
fn as_ecmascript_function(object: &Object) -> &EcmascriptFunctionObject {
    assert!(object.is_ecmascript_function_object());
    // SAFETY: Only ECMAScript function objects have the flag, and they start with their Object.
    unsafe { &*core::ptr::from_ref(object).cast::<EcmascriptFunctionObject>() }
}

/// Like C++ as_if<ECMAScriptFunctionObject>, which checks the object's flag.
pub fn as_ecmascript_function_object<T: Extends<Object>>(cell: Gc<T>) -> Option<Gc<EcmascriptFunctionObject>> {
    let object = cell.upcast::<Object>();
    object
        .is_ecmascript_function_object()
        // SAFETY: Only ECMAScript function objects have the flag.
        .then(|| unsafe { Gc::from_non_null(object.as_non_null().cast()) })
}

/// Value::as_if<ECMAScriptFunctionObject>().
pub fn value_as_ecmascript_function_object(value: Value) -> Option<Gc<EcmascriptFunctionObject>> {
    if !value.is_object() {
        return None;
    }
    as_ecmascript_function_object(value.as_object())
}

fn prototype_for_function_kind(realm: Gc<Realm>, kind: FunctionKind) -> Gc<Object> {
    match kind {
        FunctionKind::Normal => realm.function_prototype(),
        FunctionKind::Generator => realm.generator_function_prototype(),
        FunctionKind::Async => realm.async_function_prototype(),
        FunctionKind::AsyncGenerator => realm.async_generator_function_prototype(),
    }
}

fn display_fly_string(string: &Utf16FlyString) -> String {
    Utf16View::of_fly_string(string).to_utf8()
}

impl EcmascriptFunctionObject {
    pub fn create_from_function_data_with_prototype(
        vm: &Vm,
        realm: Gc<Realm>,
        shared_data: Gc<SharedFunctionInstanceData>,
        parent_environment: Option<Gc<Environment>>,
        private_environment: Option<Gc<PrivateEnvironment>>,
        prototype: Gc<Object>,
    ) -> Gc<EcmascriptFunctionObject> {
        let function = allocate_object(
            vm,
            Self::new(vm, shared_data, parent_environment, private_environment, prototype),
        );
        function.initialize(vm, realm);
        function
    }

    pub fn create_from_function_data(
        vm: &Vm,
        realm: Gc<Realm>,
        shared_data: Gc<SharedFunctionInstanceData>,
        parent_environment: Option<Gc<Environment>>,
        private_environment: Option<Gc<PrivateEnvironment>>,
    ) -> Gc<EcmascriptFunctionObject> {
        let prototype = prototype_for_function_kind(realm, shared_data.kind());
        Self::create_from_function_data_with_prototype(
            vm,
            realm,
            shared_data,
            parent_environment,
            private_environment,
            prototype,
        )
    }

    fn new(
        vm: &Vm,
        shared_data: Gc<SharedFunctionInstanceData>,
        parent_environment: Option<Gc<Environment>>,
        private_environment: Option<Gc<PrivateEnvironment>>,
        prototype: Gc<Object>,
    ) -> Self {
        let base =
            FunctionObject::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No);
        base.set_is_ecmascript_function_object();

        // OPTIMIZATION: Start from a premade shape that already has this function kind's own properties in spec order.
        //               Arrow functions use the same shape as other functions of their kind, but never get a lazy prototype.
        let realm = base.shape().realm();
        let function_shape = match shared_data.kind() {
            FunctionKind::Normal => realm.normal_function_shape(),
            FunctionKind::Generator => realm.generator_function_shape(),
            FunctionKind::Async => realm.async_function_shape(),
            FunctionKind::AsyncGenerator => realm.async_generator_function_shape(),
        };
        // NB: unsafe_set_shape(); the named storage gets room for the shape's properties when the function is allocated.
        if function_shape.prototype() == Some(prototype) {
            base.shape.set(function_shape);
        } else {
            base.shape
                .set(function_shape.create_prototype_transition(vm, Some(prototype)));
        }

        // 15. Set F.[[ScriptOrModule]] to GetActiveScriptOrModule().
        let script_or_module = vm.get_active_script_or_module();

        Self {
            base,
            shared_data: Cell::new(shared_data),
            name: FlyStringSlot::new(None),
            name_string: Cell::new(None),
            environment: Cell::new(parent_environment),
            private_environment: Cell::new(private_environment),
            script_or_module: Cell::new(script_or_module),
            home_object: Cell::new(None),
            storage: EcmascriptFunctionObjectStorage::default(),
        }
    }

    fn initialize(&self, vm: &Vm, realm: Gc<Realm>) {
        // Note: The ordering of these properties must be: length, name, prototype which is the order
        //       they are defined in the spec: https://tc39.es/ecma262/#sec-function-instances .
        //       This is observable through something like: https://tc39.es/ecma262/#sec-ordinaryownpropertykeys
        //       which must give the properties in chronological order which in this case is the order they
        //       are defined in the spec.

        let name_string = PrimitiveString::create_from_fly_string(vm, &self.name());
        self.name_string.set(Some(name_string));

        // NOTE: The constructor gave us a premade shape with "length" and "name" (and "prototype" for generator kinds) at
        //       these offsets, with the attributes the spec requires, so we only have to store the values.
        self.put_direct(
            realm.normal_function_length_offset(),
            Value::from_i32(self.function_length()),
        );
        self.put_direct(realm.normal_function_name_offset(), Value::from_string(name_string));

        match self.kind() {
            FunctionKind::Normal => {
                if !self.is_arrow_function() {
                    self.storage.may_need_lazy_prototype_instantiation.set(true);
                }
            }
            FunctionKind::Generator => {
                // prototype is "g1.prototype" in figure-2 (https://tc39.es/ecma262/img/figure-2.png)
                self.put_direct(
                    realm.generator_function_prototype_property_offset(),
                    Value::from_object(Object::create_prototype(
                        vm,
                        realm,
                        Some(realm.generator_function_prototype_prototype()),
                    )),
                );
            }
            FunctionKind::Async => {
                // 27.7.4 AsyncFunction Instances, https://tc39.es/ecma262/#sec-async-function-instances
                // AsyncFunction instances do not have a prototype property as they are not constructible.
            }
            FunctionKind::AsyncGenerator => {
                self.put_direct(
                    realm.generator_function_prototype_property_offset(),
                    Value::from_object(Object::create_prototype(
                        vm,
                        realm,
                        Some(realm.async_generator_function_prototype_prototype()),
                    )),
                );
            }
        }
    }

    pub fn as_ecmascript_function_gc(&self) -> Gc<EcmascriptFunctionObject> {
        // SAFETY: ECMAScript function objects only exist as cells once constructed.
        unsafe { Gc::from_ref(self) }
    }

    fn get_stack_frame_info(object: &Object, vm: &Vm, stack_frame_info: &mut StackFrameInfo) {
        let function = as_ecmascript_function(object);
        let shared_data = function.shared_data();
        let executable = match shared_data.executable() {
            Some(executable) => executable,
            None => {
                let rust_executable = SharedFunctionInstanceData::compile_function(vm, shared_data, false)
                    .expect("an ECMAScript function compiles to an executable");
                shared_data.set_executable(Some(rust_executable));
                rust_executable.set_name(function.name());
                if should_dump_bytecode() {
                    rust_executable.dump();
                }
                shared_data.clear_compile_inputs();
                rust_executable
            }
        };
        stack_frame_info.registers_and_locals_count = executable.registers_and_locals_count();
        stack_frame_info.constant_count =
            u32::try_from(executable.constants().len()).expect("the constant count fits in u32");
        stack_frame_info.argument_count = stack_frame_info.argument_count.max(function.formal_parameter_count());
    }

    // 10.2.1 [[Call]] ( thisArgument, argumentsList ), https://tc39.es/ecma262/#sec-ecmascript-function-objects-call-thisargument-argumentslist
    fn internal_call(
        object: &Object,
        vm: &Vm,
        callee_context: &ExecutionContext,
        this_argument: Value,
    ) -> ThrowCompletionOr<Value> {
        let function = as_ecmascript_function(object);

        debug_assert!(function.bytecode_executable().is_some());

        // 1. Let callerContext be the running execution context.
        // NOTE: No-op, kept by the VM in its execution context stack.

        // 2. Let calleeContext be PrepareForOrdinaryCall(F, undefined).
        function.prepare_for_ordinary_call(vm, callee_context, None);

        // 3. Assert: calleeContext is now the running execution context.
        debug_assert!(vm.running_execution_context() == Some(NonNull::from(callee_context)));

        // 4. If F.[[IsClassConstructor]] is true, then
        if function.is_class_constructor() {
            // a. Let error be a newly created TypeError object.
            // b. NOTE: error is created in calleeContext with F's associated Realm Record.
            let throw_completion = vm.throw_completion(
                ErrorKind::TypeError,
                ErrorType::ClassConstructorWithoutNew,
                &[&display_fly_string(&function.name())],
            );

            // c. Remove calleeContext from the execution context stack and restore callerContext as the running execution context.
            vm.pop_execution_context();

            // d. Return ThrowCompletion(error).
            return throw_completion;
        }

        // 5. Perform OrdinaryCallBindThis(F, calleeContext, thisArgument).
        if function.uses_this() {
            function.ordinary_call_bind_this(vm, callee_context, this_argument);
        }

        // 6. Let result be Completion(OrdinaryCallEvaluateBody(F, argumentsList)).
        let result = function.ordinary_call_evaluate_body(vm, callee_context);

        // 7. Remove calleeContext from the execution context stack and restore callerContext as the running execution context.
        vm.pop_execution_context();

        // 8. If result.[[Type]] is return, return result.[[Value]].
        // 9. Assert: result is a throw completion.
        // 10. Return ? result.
        result
    }

    // 10.2.2 [[Construct]] ( argumentsList, newTarget ), https://tc39.es/ecma262/#sec-ecmascript-function-objects-construct-argumentslist-newtarget
    fn internal_construct(
        object: &Object,
        vm: &Vm,
        callee_context: &ExecutionContext,
        new_target: Gc<FunctionObject>,
    ) -> ThrowCompletionOr<Gc<Object>> {
        let function = as_ecmascript_function(object);

        debug_assert!(function.bytecode_executable().is_some());

        // 1. Let callerContext be the running execution context.
        // NOTE: No-op, kept by the VM in its execution context stack.

        // 2. Let kind be F.[[ConstructorKind]].
        let kind = function.constructor_kind();

        let mut this_argument: Option<Gc<Object>> = None;

        // 3. If kind is base, then
        if kind == ConstructorKind::Base {
            // a. Let thisArgument be ? OrdinaryCreateFromConstructor(newTarget, "%Object.prototype%").
            this_argument = Some(ordinary_create_from_constructor(
                vm,
                function.realm().expect("an ECMAScript function has a realm"),
                new_target,
                Intrinsics::object_prototype,
            )?);
        }

        // 4. Let calleeContext be PrepareForOrdinaryCall(F, newTarget).
        function.prepare_for_ordinary_call(vm, callee_context, Some(new_target.upcast()));

        // 5. Assert: calleeContext is now the running execution context.
        debug_assert!(vm.running_execution_context() == Some(NonNull::from(callee_context)));

        // 6. If kind is base, then
        if kind == ConstructorKind::Base {
            let this_argument = this_argument.expect("a base constructor has a this argument");

            // a. Perform OrdinaryCallBindThis(F, calleeContext, thisArgument).
            if function.uses_this() {
                function.ordinary_call_bind_this(vm, callee_context, Value::from_object(this_argument));
            }

            // b. Let initializeResult be Completion(InitializeInstanceElements(thisArgument, F)).
            let initialize_result =
                this_argument.initialize_instance_elements(vm, function.as_ecmascript_function_gc());

            // c. If initializeResult is an abrupt completion, then
            if let Err(throw) = initialize_result {
                // i. Remove calleeContext from the execution context stack and restore callerContext as the running execution context.
                vm.pop_execution_context();

                // ii. Return ? initializeResult.
                return Err(throw);
            }
        }

        // 7. Let constructorEnv be the LexicalEnvironment of calleeContext.
        let constructor_env = callee_context.lexical_environment.get();

        // 8. Let result be Completion(OrdinaryCallEvaluateBody(F, argumentsList)).
        let result = function.ordinary_call_evaluate_body(vm, callee_context);

        // 9. Remove calleeContext from the execution context stack and restore callerContext as the running execution context.
        vm.pop_execution_context();

        // 10. If result is a throw completion, then
        //     a. Return ? result.
        let result = result?;

        // 11. Assert: result is a return completion.
        // NOTE: We already checked !is_error() above.

        // 12. If Type(result.[[Value]]) is Object, return result.[[Value]].
        if result.is_object() {
            return Ok(result.as_object());
        }

        // 13. If kind is base, return thisArgument.
        if kind == ConstructorKind::Base {
            return Ok(this_argument.expect("a base constructor has a this argument"));
        }

        // 14. If result.[[Value]] is not undefined, throw a TypeError exception.
        if !result.is_undefined() {
            return vm.throw_completion(
                ErrorKind::TypeError,
                ErrorType::DerivedConstructorReturningInvalidValue,
                &[],
            );
        }

        // 15. Let thisBinding be ? constructorEnv.GetThisBinding().
        let this_binding = constructor_env
            .expect("a constructor runs in an environment")
            .get_this_binding(vm)?;

        // 16. Assert: Type(thisBinding) is Object.
        debug_assert!(this_binding.is_object());

        // 17. Return thisBinding.
        Ok(this_binding.as_object())
    }

    // 10.2.7 MakeMethod ( F, homeObject ), https://tc39.es/ecma262/#sec-makemethod
    pub fn make_method(&self, home_object: Gc<Object>) {
        // 1. Set F.[[HomeObject]] to homeObject.
        self.home_object.set(Some(home_object));
        self.storage.is_method.set(true);
        self.storage.may_need_lazy_prototype_instantiation.set(false);

        // 2. Return unused.
    }

    // 10.2.1.1 PrepareForOrdinaryCall ( F, newTarget ), https://tc39.es/ecma262/#sec-prepareforordinarycall
    pub fn prepare_for_ordinary_call(
        &self,
        vm: &Vm,
        callee_context: &ExecutionContext,
        new_target: Option<Gc<Object>>,
    ) {
        // 1. Let callerContext be the running execution context.
        // 2. Let calleeContext be a new ECMAScript code execution context.

        // 3. Set the Function of calleeContext to F.
        callee_context.function.set(Some(self.as_function_object_gc()));

        // 4. Let calleeRealm be F.[[Realm]].
        // 5. Set the Realm of calleeContext to calleeRealm.
        callee_context.realm.set(self.realm());

        // 6. Set the ScriptOrModule of calleeContext to F.[[ScriptOrModule]].
        callee_context.script_or_module.set(self.script_or_module.get());

        if self.function_environment_needed() {
            // 7. Let localEnv be NewFunctionEnvironment(F, newTarget).
            let local_environment = new_function_environment(vm, self.as_ecmascript_function_gc(), new_target);
            let shared_data = self.shared_data();
            let function_environment_bindings_count = shared_data.function_environment_bindings_count();
            local_environment.set_environment_shape_cache(
                shared_data.function_environment_shape_cache(),
                function_environment_bindings_count,
            );
            local_environment.ensure_capacity(function_environment_bindings_count);

            // 8. Set the LexicalEnvironment of calleeContext to localEnv.
            callee_context.lexical_environment.set(Some(local_environment.upcast()));

            // 9. Set the VariableEnvironment of calleeContext to localEnv.
            callee_context
                .variable_environment
                .set(Some(local_environment.upcast()));
        } else {
            callee_context.lexical_environment.set(self.environment());
            callee_context.variable_environment.set(self.environment());
        }

        // 10. Set the PrivateEnvironment of calleeContext to F.[[PrivateEnvironment]].
        callee_context.private_environment.set(self.private_environment.get());

        // 11. If callerContext is not already suspended, suspend callerContext.
        // 12. Push calleeContext onto the execution context stack; calleeContext is now the running execution context.

        // NOTE: We don't check for stack overflow here. The bytecode interpreter will do it anyway
        //       when entering the function we're about to call.
        vm.push_execution_context(NonNull::from(callee_context));

        // 13. NOTE: Any exception objects produced after this point are associated with calleeRealm.
        // 14. Return calleeContext.
        // NOTE: See the comment after step 2 above about how contexts are allocated on the C++ stack.
    }

    // 10.2.1.2 OrdinaryCallBindThis ( F, calleeContext, thisArgument ), https://tc39.es/ecma262/#sec-ordinarycallbindthis
    pub fn ordinary_call_bind_this(&self, vm: &Vm, callee_context: &ExecutionContext, this_argument: Value) {
        // 1. Let thisMode be F.[[ThisMode]].
        // If thisMode is lexical, return unused.
        if self.this_mode() == ThisMode::Lexical {
            return;
        }

        // 3. Let calleeRealm be F.[[Realm]].
        let callee_realm = self.realm().expect("an ECMAScript function has a realm");

        // 4. Let localEnv be the LexicalEnvironment of calleeContext.
        let local_env = callee_context.lexical_environment.get();

        // 5. If thisMode is strict, let thisValue be thisArgument.
        let this_value = if self.this_mode() == ThisMode::Strict {
            this_argument
        }
        // 6. Else,
        else {
            // a. If thisArgument is undefined or null, then
            if this_argument.is_nullish() {
                // i. Let globalEnv be calleeRealm.[[GlobalEnv]].
                // ii. Assert: globalEnv is a global Environment Record.
                // iii. Let thisValue be globalEnv.[[GlobalThisValue]].
                Value::from_object(callee_realm.global_environment().global_this_value())
            }
            // b. Else,
            else {
                // i. Let thisValue be ! ToObject(thisArgument).
                let this_value = Value::from_object(this_argument.to_object(vm).must());

                // ii. NOTE: ToObject produces wrapper objects using calleeRealm.
                debug_assert!(vm.current_realm() == Some(callee_realm));

                this_value
            }
        };

        // 7. Assert: localEnv is a function Environment Record.
        // 8. Assert: The next step never returns an abrupt completion because localEnv.[[ThisBindingStatus]] is not initialized.
        // 9. Perform ! localEnv.BindThisValue(thisValue).
        callee_context.this_value.set(this_value);
        if self.function_environment_needed() {
            local_env
                .and_then(|environment| environment.downcast::<FunctionEnvironment>())
                .expect("the lexical environment of the call is a function environment")
                .bind_this_value(vm, this_value)
                .must();
        }

        // 10. Return unused.
    }

    // 10.2.1.4 OrdinaryCallEvaluateBody ( F, argumentsList ), https://tc39.es/ecma262/#sec-ordinarycallevaluatebody
    // 15.8.4 Runtime Semantics: EvaluateAsyncFunctionBody, https://tc39.es/ecma262/#sec-runtime-semantics-evaluatefunctionbody
    fn ordinary_call_evaluate_body(&self, vm: &Vm, context: &ExecutionContext) -> ThrowCompletionOr<Value> {
        let executable = self
            .bytecode_executable()
            .expect("a function is compiled before it is called");
        let result = vm
            .run_executable(NonNull::from(context), executable, 0)
            .map_err(Throw::new)?;

        // NOTE: Running the bytecode should eventually return a completion.
        // Until it does, we assume "return" and include the undefined fallback from the call site.
        if self.kind() == FunctionKind::Normal {
            return Ok(result);
        }

        if self.kind() == FunctionKind::AsyncGenerator {
            unimplemented_runtime_function("AsyncGenerator::create, for calling an async generator function", 0);
        }

        // NOTE: Async functions are entirely transformed to generator functions, and wrapped in a custom driver that returns a promise.
        unimplemented_runtime_function("GeneratorObject::create, for calling a generator or async function", 0)
    }

    pub fn set_name(&self, vm: &Vm, name: &Utf16FlyString) {
        self.name.set(Some(name.clone()));
        let name_string = PrimitiveString::create_from_fly_string(vm, name);
        self.name_string.set(Some(name_string));
        let mut descriptor = PropertyDescriptor {
            value: Some(Value::from_string(name_string)),
            writable: Some(false),
            enumerable: Some(false),
            configurable: Some(true),
            ..Default::default()
        };
        self.define_property_or_throw(vm, &vm.names.name, &mut descriptor)
            .must();
    }

    pub fn set_inferred_name(&self, vm: &Vm, name: &ClassElementName, prefix: Option<&str>) {
        let function_name = self.make_function_name(vm, name, prefix);
        self.set_name(vm, &to_utf16_fly_string(&function_name.utf16_string()));
    }

    pub fn name_for_call_stack(&self) -> Utf16String {
        self.name_string
            .get()
            .expect("an ECMAScript function has its name string once initialized")
            .utf16_string()
    }

    pub fn name(&self) -> Utf16FlyString {
        self.name.get().unwrap_or_else(|| self.shared_data().name())
    }

    fn ensure_class_data(&self) {
        let mut class_data = self.storage.class_data.borrow_mut();
        if class_data.is_none() {
            *class_data = Some(Box::default());
        }
    }

    pub fn has_class_data(&self) -> bool {
        self.storage.class_data.borrow().is_some()
    }

    pub fn fields_count(&self) -> usize {
        self.storage
            .class_data
            .borrow()
            .as_ref()
            .map_or(0, |class_data| class_data.fields.len())
    }

    /// A copy of the field record at `index` of [[Fields]].
    pub fn field(&self, index: usize) -> ClassFieldDefinition {
        self.storage
            .class_data
            .borrow()
            .as_ref()
            .expect("the function has class data")
            .fields[index]
            .clone()
    }

    pub fn add_field(&self, field: ClassFieldDefinition) {
        self.ensure_class_data();
        self.storage
            .class_data
            .borrow_mut()
            .as_mut()
            .expect("the function has class data")
            .fields
            .push(field);
    }

    pub fn private_methods_count(&self) -> usize {
        self.storage
            .class_data
            .borrow()
            .as_ref()
            .map_or(0, |class_data| class_data.private_methods.len())
    }

    /// A copy of the element at `index` of [[PrivateMethods]].
    pub fn private_method(&self, index: usize) -> PrivateElement {
        self.storage
            .class_data
            .borrow()
            .as_ref()
            .expect("the function has class data")
            .private_methods[index]
            .clone()
    }

    pub fn add_private_method(&self, method: PrivateElement) {
        self.ensure_class_data();
        self.storage
            .class_data
            .borrow_mut()
            .as_mut()
            .expect("the function has class data")
            .private_methods
            .push(method);
    }

    pub fn shared_data(&self) -> Gc<SharedFunctionInstanceData> {
        self.shared_data.get()
    }

    pub fn is_module_wrapper(&self) -> bool {
        self.shared_data().is_module_wrapper()
    }

    pub fn set_is_module_wrapper(&self, is_module_wrapper: bool) {
        self.shared_data().set_is_module_wrapper(is_module_wrapper);
    }

    pub fn formal_parameter_count(&self) -> u32 {
        self.shared_data().formal_parameter_count()
    }

    /// A copy of the parameter names, since the arguments object they are for is allocated while they are in use.
    pub fn parameter_names_for_mapped_arguments(&self) -> Vec<Utf16FlyString> {
        self.shared_data().parameter_names_for_mapped_arguments().to_vec()
    }

    pub fn set_is_class_constructor(&self) {
        self.shared_data().set_is_class_constructor();
    }

    pub fn bytecode_executable(&self) -> Option<Gc<Executable>> {
        self.shared_data().executable()
    }

    pub fn can_inline_call(&self) -> bool {
        self.shared_data().can_inline_call()
    }

    pub fn inline_call_executable(&self) -> Gc<Executable> {
        assert!(self.can_inline_call());
        self.shared_data()
            .executable()
            .expect("a function that can be called inline has an executable")
    }

    pub fn environment(&self) -> Option<Gc<Environment>> {
        self.environment.get()
    }

    pub fn private_environment(&self) -> Option<Gc<PrivateEnvironment>> {
        self.private_environment.get()
    }

    pub fn script_or_module(&self) -> ScriptOrModule {
        self.script_or_module.get()
    }

    pub fn constructor_kind(&self) -> ConstructorKind {
        self.shared_data().constructor_kind()
    }

    pub fn set_constructor_kind(&self, constructor_kind: ConstructorKind) {
        self.shared_data().set_constructor_kind(constructor_kind);
    }

    pub fn this_mode(&self) -> ThisMode {
        self.shared_data().this_mode()
    }

    pub fn is_arrow_function(&self) -> bool {
        self.shared_data().is_arrow_function()
    }

    pub fn is_class_constructor(&self) -> bool {
        self.shared_data().is_class_constructor()
    }

    pub fn uses_this(&self) -> bool {
        self.shared_data().uses_this()
    }

    pub fn this_value_needs_environment_resolution(&self) -> bool {
        self.shared_data().this_value_needs_environment_resolution()
    }

    pub fn function_length(&self) -> i32 {
        self.shared_data().function_length()
    }

    pub fn home_object(&self) -> Option<Gc<Object>> {
        self.home_object.get()
    }

    pub fn set_home_object(&self, home_object: Option<Gc<Object>>) {
        self.home_object.set(home_object);
    }

    pub fn source_text(&self) -> Utf16String {
        self.shared_data().source_text()
    }

    pub fn set_source_text(&self, source_text: Utf16String) {
        self.shared_data().set_source_text(source_text);
    }

    pub fn set_source_text_range(
        &self,
        source_code: &Rc<SourceCode>,
        source_text_offset: usize,
        source_text_length: usize,
    ) {
        self.shared_data()
            .set_source_text_range(source_code, source_text_offset, source_text_length);
    }

    // This is for IsSimpleParameterList (static semantics)
    pub fn has_simple_parameter_list(&self) -> bool {
        self.shared_data().has_simple_parameter_list()
    }

    // Equivalent to absence of [[Construct]]
    pub fn has_constructor(&self) -> bool {
        self.kind() == FunctionKind::Normal && !self.is_arrow_function() && !self.storage.is_method.get()
    }

    pub fn kind(&self) -> FunctionKind {
        self.shared_data().kind()
    }

    // This is used by LibWeb to disassociate event handler attribute callback functions from the nearest script on the call stack.
    // https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler Step 3.11
    pub fn set_script_or_module(&self, script_or_module: ScriptOrModule) {
        self.script_or_module.set(script_or_module);
    }

    pub fn class_field_initializer_name(&self) -> ClassFieldInitializerName {
        self.shared_data().class_field_initializer_name()
    }

    pub fn allocates_function_environment(&self) -> bool {
        self.shared_data().function_environment_needed()
    }

    pub fn function_environment_needed(&self) -> bool {
        self.shared_data().function_environment_needed()
    }

    fn supports_legacy_caller_or_arguments(&self) -> bool {
        // https://tc39.es/ecma262/#sec-forbidden-extensions
        //
        // ECMAScript function objects defined using syntactic constructors in strict mode code must not be created with own
        // properties named *"caller"* or *"arguments"*. Such own properties also must not be created for function objects
        // defined using an |ArrowFunction|, |MethodDefinition|, |GeneratorDeclaration|, |GeneratorExpression|,
        // |AsyncGeneratorDeclaration|, |AsyncGeneratorExpression|, |ClassDeclaration|, |ClassExpression|,
        // |AsyncFunctionDeclaration|, |AsyncFunctionExpression|, or |AsyncArrowFunction| regardless of whether the definition
        // is contained in strict mode code.
        // Built-in functions, strict functions created using the Function constructor, generator functions created using
        // the Generator constructor, async functions created using the AsyncFunction constructor, and functions created
        // using the `bind` method also must not be created with such own properties.
        self.kind() == FunctionKind::Normal
            && !self.is_arrow_function()
            && !self.is_class_constructor()
            && !self.storage.is_method.get()
            && !self.is_strict_mode()
    }

    fn legacy_caller(&self, vm: &Vm) -> Value {
        let this_function = self.as_function_object_gc();
        let mut caller: Option<Gc<EcmascriptFunctionObject>> = None;
        let mut found_this_function = false;
        let mut done = false;

        vm.for_each_execution_context_top_to_bottom(|context| {
            if done {
                return;
            }
            if !found_this_function {
                if context.function.get() == Some(this_function) {
                    found_this_function = true;
                }
                return;
            }

            let Some(function) = context.function.get() else {
                return;
            };

            caller = as_ecmascript_function_object(function);
            done = true;
        });

        // https://tc39.es/ecma262/#sec-forbidden-extensions
        //
        // If an implementation extends any function object with an own property named *"caller"* the value of that property,
        // as observed using [[Get]] or [[GetOwnProperty]], must not be a strict function object. If it is an accessor
        // property, the function that is the value of the property's [[Get]] attribute must never return a strict function
        // when called.
        match caller {
            Some(caller) if caller.supports_legacy_caller_or_arguments() => Value::from_object(caller),
            _ => Value::NULL,
        }
    }

    fn legacy_arguments(&self, vm: &Vm) -> Value {
        let this_function = self.as_function_object_gc();
        let mut active_context: Option<NonNull<ExecutionContext>> = None;

        vm.for_each_execution_context_top_to_bottom(|context| {
            if active_context.is_some() || context.function.get() != Some(this_function) {
                return;
            }

            active_context = Some(NonNull::from(context));
        });

        let Some(active_context) = active_context else {
            return Value::NULL;
        };

        // SAFETY: The context is a frame of a call of this function that has not returned yet.
        let active_context = unsafe { active_context.as_ref() };
        let arguments = active_context.arguments();
        let passed_arguments = &arguments[..active_context.passed_argument_count.get() as usize];
        let arguments_object = create_unmapped_arguments_object(vm, passed_arguments);
        if self.has_simple_parameter_list() {
            arguments_object.define_direct_property(
                vm,
                &vm.names.callee,
                Value::from_object(self.as_function_object_gc()),
                PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
            );
        }
        Value::from_object(arguments_object)
    }

    fn internal_get_own_property(
        object: &Object,
        vm: &Vm,
        property_key: &PropertyKey,
    ) -> ThrowCompletionOr<Option<PropertyDescriptor>> {
        let function = as_ecmascript_function(object);

        if function.supports_legacy_caller_or_arguments() {
            let descriptor = object.ordinary_get_own_property(vm, property_key)?;
            if descriptor.is_some() {
                return Ok(descriptor);
            }

            if *property_key == vm.names.caller {
                return Ok(Some(PropertyDescriptor {
                    value: Some(function.legacy_caller(vm)),
                    writable: Some(false),
                    enumerable: Some(false),
                    configurable: Some(false),
                    ..Default::default()
                }));
            }
            if *property_key == vm.names.arguments {
                return Ok(Some(PropertyDescriptor {
                    value: Some(function.legacy_arguments(vm)),
                    writable: Some(false),
                    enumerable: Some(false),
                    configurable: Some(false),
                    ..Default::default()
                }));
            }
        }

        if function.storage.may_need_lazy_prototype_instantiation.get() && *property_key == vm.names.prototype {
            let realm = function.realm().expect("an ECMAScript function has a realm");
            let metadata = object.shape().lookup(property_key);
            if metadata.is_none() {
                let prototype = Object::create_with_premade_shape(vm, realm.normal_function_prototype_shape());
                prototype.put_direct(
                    realm.normal_function_prototype_constructor_offset(),
                    Value::from_object(function.as_function_object_gc()),
                );
                object.define_direct_property(
                    vm,
                    &vm.names.prototype,
                    Value::from_object(prototype),
                    PropertyAttributes::new(Attribute::WRITABLE),
                );
            }
            function.storage.may_need_lazy_prototype_instantiation.set(false);
        }

        object.ordinary_get_own_property(vm, property_key)
    }

    fn internal_own_property_keys<'vm>(object: &Object, vm: &'vm Vm) -> ThrowCompletionOr<MarkedVec<'vm, Value>> {
        let function = as_ecmascript_function(object);

        if function.storage.may_need_lazy_prototype_instantiation.get() {
            Self::internal_get_own_property(object, vm, &vm.names.prototype)?;
        }

        let keys = object.ordinary_own_property_keys(vm)?;
        if !function.supports_legacy_caller_or_arguments() {
            return Ok(keys);
        }

        let mut insertion_index = keys.len();
        let name = Utf16String::from(vm.names.name.as_string());
        for index in 0..keys.len() {
            let key = keys.get(index).expect("the index is in bounds");
            if key.is_string() && key.as_string().utf16_string() == name {
                insertion_index = index + 1;
                break;
            }
        }

        if object.ordinary_get_own_property(vm, &vm.names.arguments)?.is_none() {
            keys.insert(
                insertion_index,
                Value::from_string(PrimitiveString::create_from_fly_string(
                    vm,
                    vm.names.arguments.as_string(),
                )),
            );
            insertion_index += 1;
        }
        if object.ordinary_get_own_property(vm, &vm.names.caller)?.is_none() {
            keys.insert(
                insertion_index,
                Value::from_string(PrimitiveString::create_from_fly_string(vm, vm.names.caller.as_string())),
            );
        }

        Ok(keys)
    }
}

/// What the unit tests of functions share: compiling scripts and making functions of what they declare.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_functions {
    use ak::Utf16String;
    use libjs_rust::ast::ProgramType;

    use super::*;
    use crate::runtime::declarative_environment::DeclarativeEnvironment;

    /// Compiles the script `source` without running it. The shared function data of its executable are the functions
    /// its top level code creates, in source order.
    pub fn compile_script(vm: &Vm, source: &str) -> Gc<Executable> {
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let parsed = libjs_rust::compile::parse(&code_units, ProgramType::Script, 1);
        assert!(!parsed.has_errors(), "the test script parses");
        let compiled = libjs_rust::compile::compile_script(parsed, code_units.len());
        let source_code = SourceCode::create(Utf16String::default(), Utf16String::from_utf16(&code_units));
        Executable::create_with_source_code(vm, compiled.executable, Some(&source_code))
    }

    /// The function `source` creates at `index` among the functions of its top level code, closing over a new empty
    /// declarative environment.
    pub fn function_from_script(vm: &Vm, realm: Gc<Realm>, source: &str, index: u32) -> Gc<EcmascriptFunctionObject> {
        let executable = compile_script(vm, source);
        let environment = DeclarativeEnvironment::create(vm, None);
        EcmascriptFunctionObject::create_from_function_data(
            vm,
            realm,
            executable.shared_function_data(index),
            Some(environment.upcast()),
            None,
        )
    }

    pub fn property(vm: &Vm, object: Gc<Object>, name: &str) -> Value {
        object.get(vm, &PropertyKey::from_utf8(name)).must()
    }

    pub fn string(value: Value) -> String {
        value.as_string().to_utf8()
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::test_functions::{compile_script, function_from_script, property, string};
    use super::*;
    use crate::layout::function_object::asm_call_metadata;
    use crate::runtime::abstract_operations::{call, construct};
    use crate::runtime::declarative_environment::DeclarativeEnvironment;
    use crate::runtime::environment::InitializeBindingHint;
    use crate::runtime::native_function::{RawNativeFunction, raw_native};
    use crate::runtime::realm::test_realm::{TestRealm, key, own_keys};

    fn int(value: i32) -> Value {
        Value::from_i32(value)
    }

    fn call_function(vm: &Vm, function: Gc<impl Extends<Object>>, this: Value, arguments: &[Value]) -> Value {
        call(vm, Value::from_object(function), this, arguments).must()
    }

    fn element(object: Gc<Object>, index: u32) -> Value {
        object.indexed_get(index).expect("the object has the element").value
    }

    fn parameters_defaults_rest_and_this(vm: &Vm, test_realm: &TestRealm) {
        let realm = test_realm.realm;

        let defaults = function_from_script(vm, realm, "(function (a, b = 10) { return b; })", 0);
        assert!(call_function(vm, defaults, Value::UNDEFINED, &[int(1)]) == int(10));
        assert!(call_function(vm, defaults, Value::UNDEFINED, &[int(1), int(2)]) == int(2));
        assert!(property(vm, defaults.upcast(), "length") == int(1));
        assert_eq!(string(property(vm, defaults.upcast(), "name")), "");
        assert_eq!(own_keys(vm, &defaults), "length,name,arguments,caller,prototype");

        let named = function_from_script(vm, realm, "(function named(a, b) { return a; })", 0);
        assert!(call_function(vm, named, Value::UNDEFINED, &[]) == Value::UNDEFINED);
        assert!(property(vm, named.upcast(), "length") == int(2));
        assert_eq!(string(property(vm, named.upcast(), "name")), "named");
        assert_eq!(
            named.name_for_call_stack().to_utf16(),
            "named".encode_utf16().collect::<Vec<_>>()
        );

        let strict = function_from_script(vm, realm, "(function (a) { \"use strict\"; })", 0);
        assert_eq!(own_keys(vm, &strict), "length,name,prototype");
        let arrow = function_from_script(vm, realm, "((a, b, c) => c)", 0);
        assert_eq!(own_keys(vm, &arrow), "length,name");
        assert!(call_function(vm, arrow, Value::UNDEFINED, &[int(1), int(2), int(3)]) == int(3));

        let rest = function_from_script(vm, realm, "(function (a, ...rest) { return rest; })", 0);
        let rest_array = call_function(vm, rest, Value::UNDEFINED, &[int(1), int(2), int(3)]).as_object();
        assert_eq!(own_keys(vm, &rest_array), "0,1,length");
        assert!(element(rest_array, 0) == int(2) && element(rest_array, 1) == int(3));
        assert!(rest_array.prototype() == Some(realm.array_prototype()));
        let empty_rest = call_function(vm, rest, Value::UNDEFINED, &[]).as_object();
        assert_eq!(own_keys(vm, &empty_rest), "length");

        let strict_this = function_from_script(vm, realm, "(function () { \"use strict\"; return this; })", 0);
        assert!(call_function(vm, strict_this, int(5), &[]) == int(5));
        assert!(call_function(vm, strict_this, Value::UNDEFINED, &[]) == Value::UNDEFINED);
        let receiver = Value::from_object(test_realm.object());
        let sloppy_this = function_from_script(vm, realm, "(function () { return this; })", 0);
        assert!(call_function(vm, sloppy_this, receiver, &[]) == receiver);
    }

    #[test]
    fn compiled_functions_bind_parameters_defaults_rest_and_this_like_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        parameters_defaults_rest_and_this(&vm, &test_realm);
    }

    #[test]
    fn compiled_functions_keep_everything_alive_when_collecting_on_every_allocation() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        vm.heap().set_should_collect_on_every_allocation(true);
        parameters_defaults_rest_and_this(&vm, &test_realm);
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn unmapped_arguments_objects_hold_the_passed_arguments() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;
        let function = function_from_script(&vm, realm, "(function (a) { \"use strict\"; return arguments; })", 0);

        let arguments = call_function(&vm, function, Value::UNDEFINED, &[int(1), int(2)]).as_object();
        assert_eq!(own_keys(&vm, &arguments), "0,1,length,callee,Symbol(Symbol.iterator)");
        assert!(property(&vm, arguments, "length") == int(2));
        assert!(element(arguments, 0) == int(1) && element(arguments, 1) == int(2));
        assert!(arguments.prototype() == Some(realm.object_prototype()));
        let callee = arguments
            .storage_get(&vm, &vm.names.callee)
            .expect("the arguments object has a callee");
        assert!(callee.value == Value::from_accessor(realm.throw_type_error_accessor()));
        assert!(!callee.attributes.is_configurable() && !callee.attributes.is_enumerable());
        let iterator = arguments
            .get(&vm, &PropertyKey::from(vm.well_known_symbols().iterator))
            .must();
        assert!(iterator == Value::from_object(realm.array_prototype_values_function()));

        let no_arguments = call_function(&vm, function, Value::UNDEFINED, &[]).as_object();
        assert_eq!(own_keys(&vm, &no_arguments), "length,callee,Symbol(Symbol.iterator)");
    }

    #[test]
    fn functions_compile_on_their_first_call_and_tell_the_interpreter_how_to_call_them() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;

        let function = function_from_script(&vm, realm, "(function (a, b) { return a; })", 0);
        let shared_data = function.shared_data();
        assert!(shared_data.executable().is_none());
        assert!(!function.can_inline_call());
        assert_eq!(shared_data.asm_call_metadata.get(), 2);

        assert!(call_function(&vm, function, Value::UNDEFINED, &[int(4)]) == int(4));
        assert!(shared_data.executable().is_some());
        assert!(function.can_inline_call());
        assert_eq!(
            shared_data.asm_call_metadata.get(),
            2 | asm_call_metadata::CAN_INLINE_CALL
        );

        let strict_this = function_from_script(&vm, realm, "(function (a) { \"use strict\"; return this; })", 0);
        call_function(&vm, strict_this, Value::UNDEFINED, &[]);
        assert_eq!(
            strict_this.shared_data().asm_call_metadata.get(),
            1 | asm_call_metadata::CAN_INLINE_CALL | asm_call_metadata::USES_THIS | asm_call_metadata::STRICT
        );

        let class_constructor = function_from_script(&vm, realm, "(function () {})", 0);
        class_constructor.get_stack_frame_info(&vm, &mut StackFrameInfo::default());
        assert!(class_constructor.can_inline_call());
        class_constructor.set_is_class_constructor();
        assert!(!class_constructor.can_inline_call());
        assert_eq!(class_constructor.shared_data().asm_call_metadata.get(), 0);
    }

    #[test]
    fn the_interpreter_calls_compiled_functions_and_raw_natives_inline() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;

        let caller = function_from_script(&vm, realm, "(function (g, x) { return g(x); })", 0);
        let catcher = function_from_script(
            &vm,
            realm,
            "(function (g) { try { g(); } catch (e) { return e; } return 0; })",
            0,
        );

        let identity = function_from_script(&vm, realm, "(function (y) { return y; })", 0);
        // The interpreter only enters a function inline once it is compiled.
        call_function(&vm, identity, Value::UNDEFINED, &[]);
        assert!(call_function(&vm, caller, Value::UNDEFINED, &[Value::from_object(identity), int(7)]) == int(7));

        let thrower = function_from_script(&vm, realm, "(function () { throw 5; })", 0);
        let thrown = call(&vm, Value::from_object(thrower), Value::UNDEFINED, &[]).expect_err("the function throws");
        assert!(thrown.value() == int(5));
        assert!(call_function(&vm, catcher, Value::UNDEFINED, &[Value::from_object(thrower)]) == int(5));

        let increment = RawNativeFunction::create(
            &vm,
            raw_native!(|vm| Ok(Value::from_i32(vm.argument(0).as_i32() + 1))),
            1,
            &key("increment"),
            None,
            None,
            None,
        );
        assert!(call_function(&vm, caller, Value::UNDEFINED, &[Value::from_object(increment), int(41)]) == int(42));

        let raw_thrower = RawNativeFunction::create(
            &vm,
            raw_native!(|_| Err(Throw::new(Value::from_i32(9)))),
            0,
            &key("thrower"),
            None,
            None,
            None,
        );
        assert!(call_function(&vm, catcher, Value::UNDEFINED, &[Value::from_object(raw_thrower)]) == int(9));
        let uncaught = call(
            &vm,
            Value::from_object(caller),
            Value::UNDEFINED,
            &[Value::from_object(raw_thrower), int(0)],
        )
        .expect_err("the native function's exception propagates");
        assert!(uncaught.value() == int(9));
    }

    #[test]
    fn nested_functions_close_over_the_environment_they_are_created_in() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;

        // NB: The inner function is anonymous: a named function expression has an environment of its own for its name.
        let outer = function_from_script(
            &vm,
            realm,
            "(function outer(a) { return function () { return a; }; })",
            0,
        );
        let mut stack_frame_info = StackFrameInfo::default();
        outer.get_stack_frame_info(&vm, &mut stack_frame_info);
        let outer_shared_data = outer.shared_data();
        assert!(outer_shared_data.function_environment_needed());
        assert_eq!(outer_shared_data.function_environment_bindings_count(), 1);

        let inner_shared_data = outer_shared_data
            .executable()
            .expect("the outer function was compiled")
            .shared_function_data(0);
        assert!(inner_shared_data.executable().is_none() && !inner_shared_data.is_arrow_function());

        // The environment a call of the outer function would create for its parameter.
        let a = Utf16FlyString::from_utf8("a");
        let environment = DeclarativeEnvironment::create(&vm, None);
        environment.create_mutable_binding(&vm, &a, false).must();
        environment
            .initialize_binding(&vm, &a, int(42), InitializeBindingHint::Normal)
            .must();
        let inner = EcmascriptFunctionObject::create_from_function_data(
            &vm,
            realm,
            inner_shared_data,
            Some(environment.upcast()),
            None,
        );
        assert!(call_function(&vm, inner, Value::UNDEFINED, &[]) == int(42));
        environment.set_mutable_binding(&vm, &a, int(43), true).must();
        assert!(call_function(&vm, inner, Value::UNDEFINED, &[]) == int(43));
    }

    #[test]
    fn constructing_creates_this_from_the_lazily_created_prototype() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;

        let constructor = function_from_script(&vm, realm, "(function (a) { return a; })", 0);
        let instance = construct(&vm, constructor.upcast(), &[int(1)], None).must();
        let prototype = property(&vm, constructor.upcast(), "prototype").as_object();
        assert!(instance.prototype() == Some(prototype));
        assert_eq!(own_keys(&vm, &prototype), "constructor");
        assert!(property(&vm, prototype, "constructor") == Value::from_object(constructor));
        assert!(prototype.prototype() == Some(realm.object_prototype()));

        let returned = test_realm.object();
        let returned_instance = construct(&vm, constructor.upcast(), &[Value::from_object(returned)], None).must();
        assert!(returned_instance == returned);

        let arrow = function_from_script(&vm, realm, "(() => 1)", 0);
        assert!(!Value::from_object(arrow).is_constructor());
        assert!(Value::from_object(constructor).is_constructor());
    }

    #[test]
    fn inline_frames_link_to_their_caller_and_unwind_to_it() {
        let vm = Vm::create();
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let realm = test_realm.realm;
        let caller_context = vm.running_execution_context().expect("the test realm runs a context");

        let outer = function_from_script(
            &vm,
            realm,
            "(function outer(a, b) { \"use strict\"; this; return function () { return a; }; })",
            0,
        );
        outer.get_stack_frame_info(&vm, &mut StackFrameInfo::default());
        assert!(outer.can_inline_call() && outer.function_environment_needed());

        let receiver = Value::from_object(test_realm.object());
        let callee_context = vm
            .push_inline_frame(
                outer,
                outer.inline_call_executable(),
                &[int(1)],
                64,
                3,
                receiver,
                None,
                false,
            )
            .expect("the interpreter stack has room");
        assert!(vm.running_execution_context() == Some(callee_context));
        // SAFETY: The frame is live until it is unwound below.
        let callee = unsafe { callee_context.as_ref() };
        assert!(callee.caller_frame.get() == caller_context.as_ptr());
        assert_eq!((callee.caller_return_pc.get(), callee.caller_dst_raw.get()), (64, 3));
        assert!(callee.function.get() == Some(outer.as_function_object_gc()));
        assert!(callee.realm.get() == Some(realm));
        assert_eq!(callee.passed_argument_count.get(), 1);
        assert!(callee.argument(0) == int(1) && callee.argument(1) == Value::UNDEFINED);
        assert!(callee.this_value.get() == receiver);
        assert!(callee.register(libjs_abi::register::THIS_VALUE).get() == receiver);
        let environment = callee.lexical_environment.get().expect("the frame has an environment");
        let function_environment = environment
            .downcast::<FunctionEnvironment>()
            .expect("the function needs a function environment");
        assert!(function_environment.get_this_binding(&vm).must() == receiver);
        assert!(environment.outer_environment() == outer.environment());

        vm.unwind_inline_frame_for_exception();
        assert!(vm.running_execution_context() == Some(caller_context));
    }

    #[test]
    fn executables_keep_the_functions_they_declare_alive() {
        let vm = Vm::create();
        let _test_realm = TestRealm::with_function_intrinsics(&vm);
        let executable = compile_script(&vm, "(function a() {}); (function b() {}); (() => 1);");
        vm.heap().collect_garbage();
        assert_eq!(executable.shared_function_data_count(), 3);
        let names: Vec<String> = (0..3)
            .map(|index| Utf16View::of_fly_string(&executable.shared_function_data(index).name()).to_utf8())
            .collect();
        assert_eq!(names, ["a", "b", ""]);
        assert!(executable.shared_function_data(2).is_arrow_function());
        assert_eq!(executable.shared_function_data(2).this_mode(), ThisMode::Lexical);
        assert_eq!(
            executable.shared_function_data(0).source_text().to_utf16().into_owned(),
            "function a() {}".encode_utf16().collect::<Vec<_>>()
        );
    }
}
