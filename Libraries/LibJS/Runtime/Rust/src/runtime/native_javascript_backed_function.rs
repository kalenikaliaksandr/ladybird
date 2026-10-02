/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ops::Deref;

use libjs_runtime_macros::Trace;

use crate::bytecode::executable::Executable;
use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::run::should_dump_bytecode;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::async_function_driver_wrapper::AsyncFunctionDriverWrapper;
use crate::runtime::async_generator::AsyncGenerator;
use crate::runtime::class_field_definition::ClassElementName;
use crate::runtime::completion::{Throw, ThrowCompletionOr};
use crate::runtime::function_object::FunctionObject;
use crate::runtime::generator_object::{GeneratingFunction, GeneratorObject};
use crate::runtime::native_function::{
    NATIVE_FUNCTION_METHODS, NATIVE_FUNCTION_VIRTUAL_METHODS, NativeFunction, NativeFunctionMethods,
};
use crate::runtime::object::{ObjectMethods, StackFrameInfo};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::realm::Realm;
use crate::runtime::shared_function_instance_data::{FunctionKind, SharedFunctionInstanceData, ThisMode};

/// A built-in function written in JavaScript, whose body the frontend compiles with the builtin abstract operations
/// enabled the first time it is called.
#[repr(C)]
#[derive(Trace)]
pub struct NativeJavaScriptBackedFunction {
    base: NativeFunction,
    shared_function_instance_data: Cell<Gc<SharedFunctionInstanceData>>,
}

static NATIVE_JAVASCRIPT_BACKED_FUNCTION_VIRTUAL_METHODS: NativeFunctionMethods = NativeFunctionMethods {
    call: NativeJavaScriptBackedFunction::call,
    function_environment_needed: |function| {
        as_native_javascript_backed_function(function).function_environment_needed()
    },
    function_environment_bindings_count: |function| {
        as_native_javascript_backed_function(function).function_environment_bindings_count()
    },
    ..NATIVE_FUNCTION_VIRTUAL_METHODS
};

static NATIVE_JAVASCRIPT_BACKED_FUNCTION_METHODS: ObjectMethods = ObjectMethods {
    is_strict_mode: |object| as_native_javascript_backed_function(object).is_strict_mode(),
    get_stack_frame_info: NativeJavaScriptBackedFunction::get_stack_frame_info,
    native_function: Some(&NATIVE_JAVASCRIPT_BACKED_FUNCTION_VIRTUAL_METHODS),
    ..NATIVE_FUNCTION_METHODS
};

define_cell!(
    NativeJavaScriptBackedFunction,
    Object,
    extends: [NativeFunction, FunctionObject, Object],
    methods: NATIVE_JAVASCRIPT_BACKED_FUNCTION_METHODS
);

impl Deref for NativeJavaScriptBackedFunction {
    type Target = NativeFunction;

    fn deref(&self) -> &NativeFunction {
        &self.base
    }
}

/// The function an internal method of a NativeJavaScriptBackedFunction was called on.
fn as_native_javascript_backed_function(object: &Object) -> &NativeJavaScriptBackedFunction {
    assert!(object.is::<NativeJavaScriptBackedFunction>());
    // SAFETY: The object is a NativeJavaScriptBackedFunction, which starts with its Object.
    unsafe { &*core::ptr::from_ref(object).cast::<NativeJavaScriptBackedFunction>() }
}

impl NativeJavaScriptBackedFunction {
    // 10.3.3 CreateBuiltinFunction ( behaviour, length, name, additionalInternalSlotsList [ , realm [ , prototype [ , prefix ] ] ] ), https://tc39.es/ecma262/#sec-createbuiltinfunction
    pub fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        shared_data: Gc<SharedFunctionInstanceData>,
        name: &PropertyKey,
        length: i32,
    ) -> Gc<NativeJavaScriptBackedFunction> {
        // 1. If realm is not present, set realm to the current Realm Record.
        // 2. If prototype is not present, set prototype to realm.[[Intrinsics]].[[%Function.prototype%]].
        let prototype = realm.function_prototype();

        // 3. Let internalSlotsList be a List containing the names of all the internal slots that 10.3 requires for the built-in function object that is about to be created.
        // 4. Append to internalSlotsList the elements of additionalInternalSlotsList.

        // 5. Let func be a new built-in function object that, when called, performs the action described by behaviour using the provided arguments as the values of the corresponding parameters specified by behaviour. The new function object has internal slots whose names are the elements of internalSlotsList, and an [[InitialName]] internal slot.
        // 6. Set func.[[Prototype]] to prototype.
        // 7. Set func.[[Extensible]] to true.
        // 8. Set func.[[Realm]] to realm.
        // 9. Set func.[[InitialName]] to null.
        let function = realm.create_object(
            vm,
            NativeJavaScriptBackedFunction {
                base: NativeFunction::new_with_name(vm, Self::CLASS, shared_data.name(), prototype),
                shared_function_instance_data: Cell::new(shared_data),
            },
        );

        function.unsafe_set_shape(realm.native_function_shape());

        // 10. Perform SetFunctionLength(func, length).
        function.put_direct(realm.native_function_length_offset(), Value::from_i32(length));

        // 11. If prefix is not present, then
        //     a. Perform SetFunctionName(func, name).
        // 12. Else,
        //     a. Perform SetFunctionName(func, name, prefix).
        let function_name = function.make_function_name(vm, &ClassElementName::PropertyKey(name.clone()), None);
        function.put_direct(realm.native_function_name_offset(), Value::from_string(function_name));

        // 13. Return func.
        function
    }

    pub fn as_native_javascript_backed_function_gc(&self) -> Gc<NativeJavaScriptBackedFunction> {
        // SAFETY: NativeJavaScriptBackedFunctions only exist as cells once constructed.
        unsafe { Gc::from_ref(self) }
    }

    fn get_stack_frame_info(object: &Object, vm: &Vm, stack_frame_info: &mut StackFrameInfo) {
        let function = as_native_javascript_backed_function(object);
        let bytecode_executable = function.bytecode_executable(vm);
        stack_frame_info.registers_and_locals_count = bytecode_executable.registers_and_locals_count();
        stack_frame_info.constant_count =
            u32::try_from(bytecode_executable.constants().len()).expect("the constant count fits in u32");
        // NB: C++ makes room for as many arguments as the function's length, where an ECMAScript function makes room
        //     for its formal parameters. The builtin files only declare functions whose two counts are the same.
        let function_length =
            u32::try_from(function.shared_data().function_length()).expect("a builtin's length is not negative");
        stack_frame_info.argument_count = stack_frame_info.argument_count.max(function_length);
    }

    fn call(function: &NativeFunction, vm: &Vm) -> ThrowCompletionOr<Value> {
        let function = as_native_javascript_backed_function(function);

        let running_execution_context = vm
            .running_execution_context()
            .expect("a NativeJavaScriptBackedFunction runs in its own execution context");
        let result = vm
            .run_executable(running_execution_context, function.bytecode_executable(vm), 0)
            .map_err(Throw::new)?;

        let kind = function.kind();
        if kind == FunctionKind::Normal {
            return Ok(result);
        }

        let realm = vm
            .current_realm()
            .expect("a NativeJavaScriptBackedFunction runs in a realm");
        let generating_function =
            GeneratingFunction::NativeJavaScriptBacked(function.as_native_javascript_backed_function_gc());
        // SAFETY: The running execution context is live.
        let running_execution_context = unsafe { running_execution_context.as_ref() };
        if kind == FunctionKind::AsyncGenerator {
            return Ok(Value::from_object(AsyncGenerator::create(
                vm,
                realm,
                generating_function,
                running_execution_context.copy(),
            )));
        }

        let generator_object =
            GeneratorObject::create(vm, realm, generating_function, running_execution_context.copy());

        // NOTE: Async functions are entirely transformed to generator functions, and wrapped in a custom driver that returns a promise.
        if kind == FunctionKind::Async {
            return Ok(Value::from_object(AsyncFunctionDriverWrapper::create(
                vm,
                realm,
                generator_object,
            )));
        }

        assert!(kind == FunctionKind::Generator);
        Ok(Value::from_object(generator_object))
    }

    pub fn bytecode_executable(&self, vm: &Vm) -> Gc<Executable> {
        let shared_data = self.shared_data();
        if let Some(executable) = shared_data.executable() {
            return executable;
        }

        let rust_executable = SharedFunctionInstanceData::compile_function(vm, shared_data, true)
            .expect("a builtin written in JavaScript compiles to an executable");
        shared_data.set_executable(Some(rust_executable));
        rust_executable.set_name(shared_data.name());
        if should_dump_bytecode() {
            rust_executable.dump();
        }
        shared_data.clear_compile_inputs();
        rust_executable
    }

    pub fn shared_data(&self) -> Gc<SharedFunctionInstanceData> {
        self.shared_function_instance_data.get()
    }

    pub fn kind(&self) -> FunctionKind {
        self.shared_data().kind()
    }

    pub fn this_mode(&self) -> ThisMode {
        self.shared_data().this_mode()
    }

    pub fn function_environment_needed(&self) -> bool {
        self.shared_data().function_environment_needed()
    }

    pub fn function_environment_bindings_count(&self) -> usize {
        self.shared_data().function_environment_bindings_count()
    }

    pub fn is_strict_mode(&self) -> bool {
        self.shared_data().strict()
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::runtime::completion::Must;
    use crate::runtime::ecmascript_function_object::test_functions::{property, string};
    use crate::runtime::realm::test_realm::own_keys;
    use crate::script::Script;
    use crate::utilities::initialize_realm;

    fn run_script(vm: &Vm, realm: Gc<Realm>, source: &str) -> ThrowCompletionOr<Value> {
        let source: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse(vm, &source, realm).expect("the script parses");
        vm.run_script(script, None)
    }

    // Array.fromAsync through each of its paths: async and sync iterables, array-likes, a mapper, and the abstract
    // operations it calls, AsyncIteratorClose among them when the mapper throws.
    const FROM_ASYNC: &str = r#"
var log = [];
async function* numbers() {
    try {
        yield 1;
        yield { n: 2 };
        yield 3;
    } finally {
        log.push("closed");
    }
}
(async () => {
    log.push((await Array.fromAsync(numbers(), (value, index) => [value, index])).join("/"));
    log.push((await Array.fromAsync([Promise.resolve("a"), "b"])).join());
    log.push((await Array.fromAsync({ length: 2, 0: 4, 1: Promise.resolve(5) }, async value => value * 2)).join());
    try {
        await Array.fromAsync(numbers(), value => { if (value !== 1) throw new Error("mapper"); return value; });
    } catch (error) {
        log.push(error.message);
    }
    try {
        await Array.fromAsync([], 1);
    } catch (error) {
        log.push(error.message);
    }
})();
"#;

    fn create_arrays_from_async_values(vm: &Vm) {
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let intrinsics = realm.intrinsics();

        let from_async = intrinsics.from_async_array_constructor_function(vm);
        assert!(from_async == intrinsics.from_async_array_constructor_function(vm));
        assert_eq!(own_keys(vm, &from_async), "length,name");
        assert!(property(vm, from_async.upcast(), "length") == Value::from_i32(1));
        assert_eq!(string(property(vm, from_async.upcast(), "name")), "fromAsync");
        assert!(from_async.is_strict_mode() && from_async.kind() == FunctionKind::Async);
        assert!(from_async.shared_data().executable().is_none());

        run_script(vm, realm, FROM_ASYNC).must();
        let result = run_script(vm, realm, "log.join(' ')").must();
        assert_eq!(
            string(result),
            "closed 1,0/[object Object],1/3,2 a,b 8,10 closed mapper mapper must be a function"
        );

        assert!(from_async.shared_data().executable().is_some());
        let get_method = intrinsics.get_method_abstract_operation_function(vm);
        assert!(get_method.shared_data().executable().is_some());
        assert_eq!(string(property(vm, get_method.upcast(), "name")), "GetMethod");
        assert!(property(vm, get_method.upcast(), "length") == Value::from_i32(2));
    }

    #[test]
    fn array_from_async_runs_the_builtin_written_in_javascript() {
        let vm = Vm::create();
        create_arrays_from_async_values(&vm);
    }

    #[test]
    fn array_from_async_keeps_its_frames_alive_when_collecting_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        create_arrays_from_async_values(&vm);
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    // The builtin files of the C++ runtime have neither closures nor generators, so these paths only run for
    // builtins a test declares.
    const BUILTINS_WITH_CLOSURES_AND_GENERATORS: &str = "
function Counter(start) {
    let count = start;
    const increment = () => ++count;
    increment();
    return increment();
}
function* Numbers(first) {
    yield first;
    yield ToBoolean(first);
}
";

    fn run_builtins_with_closures_and_generators(vm: &Vm) {
        use crate::runtime::abstract_operations::call;
        use crate::runtime::intrinsics::parse_builtin_file;
        use crate::runtime::realm::test_realm::key;

        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let shared_data_list =
            parse_builtin_file(vm, ak::Utf16String::from_utf8(BUILTINS_WITH_CLOSURES_AND_GENERATORS));
        assert_eq!(shared_data_list.len(), 2);
        let function = |index: usize, name: &str| {
            let shared_data = shared_data_list.get(index).expect("the file declares the function");
            Value::from_object(NativeJavaScriptBackedFunction::create(
                vm,
                realm,
                shared_data,
                &key(name),
                1,
            ))
        };

        let counter = function(0, "Counter");
        assert!(call(vm, counter, Value::UNDEFINED, &[Value::from_i32(5)]).must() == Value::from_i32(7));
        let counter_shared_data = shared_data_list.get(0).expect("the file declares Counter");
        assert!(counter_shared_data.function_environment_needed());
        assert_eq!(counter_shared_data.function_environment_bindings_count(), 1);
        assert!(call(vm, counter, Value::UNDEFINED, &[Value::from_i32(-3)]).must() == Value::from_i32(-1));

        let numbers = function(1, "Numbers");
        let generator = call(vm, numbers, Value::UNDEFINED, &[Value::from_i32(9)]).must();
        assert!(generator.as_object().is::<GeneratorObject>());
        let next_value = || {
            let result = generator.invoke(vm, &key("next"), &[]).must();
            property(vm, result.as_object(), "value")
        };
        assert!(next_value() == Value::from_i32(9));
        assert!(next_value() == Value::TRUE);
        assert!(next_value() == Value::UNDEFINED);
    }

    #[test]
    fn builtins_create_function_environments_and_generators() {
        let vm = Vm::create();
        run_builtins_with_closures_and_generators(&vm);
    }

    #[test]
    fn builtins_create_function_environments_and_generators_when_collecting_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        run_builtins_with_closures_and_generators(&vm);
        vm.heap().set_should_collect_on_every_allocation(false);
    }
}
