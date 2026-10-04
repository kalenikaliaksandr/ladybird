/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host functions, of kind JS_HOST_CLASS_FUNCTION.

use core::ffi::c_void;
use core::ops::Deref;
use core::ptr::NonNull;

use ak::Utf16FlyString;
use libjs_runtime_macros::Trace;

use crate::embedding::abi_types::{completion_from_abi, object_into_abi, vm_into_abi};
use crate::embedding::host::class_table::{
    copy_host_class_flags_into_object, lend_object_to_hook, object_completion_from_hook,
};
use crate::embedding::host::registry::runtime_class_and_allocator_of_host_class;
use crate::gc::class::{Class, Finalize, define_cell};
use crate::gc::foreign::ForeignCellSlot;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JS_HOST_CLASS_FUNCTION, JS_HOST_CLASS_HAS_CONSTRUCTOR, JSHostClass};
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::function_object::FunctionObject;
use crate::runtime::native_function::{
    NATIVE_FUNCTION_METHODS, NATIVE_FUNCTION_VIRTUAL_METHODS, NativeFunction, NativeFunctionMethods,
};
use crate::runtime::object::{Object, ObjectMethods, allocate_object_in};
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::realm::Realm;

/// A built-in function whose [[Call]] and [[Construct]] come from its host class, with a C++ GC cell for any state
/// of its own.
#[repr(C)]
#[derive(Trace)]
pub struct HostFunction {
    pub base: NativeFunction,
    #[gc(untraced)]
    pub host_class: &'static JSHostClass,
    pub host_data: ForeignCellSlot,
}

static HOST_FUNCTION_VIRTUAL_METHODS: NativeFunctionMethods = NativeFunctionMethods {
    call: call_through_hook,
    construct: construct_through_hook,
    ..NATIVE_FUNCTION_VIRTUAL_METHODS
};

/// The internal methods of every host function class, which read the hooks and flags of the function's own table.
static HOST_FUNCTION_METHODS: ObjectMethods = ObjectMethods {
    has_constructor: host_function_has_constructor,
    native_function: Some(&HOST_FUNCTION_VIRTUAL_METHODS),
    ..NATIVE_FUNCTION_METHODS
};

// The class that the class of every host class table of this kind extends. No function has it as its class.
define_cell!(
    HostFunction,
    Object,
    extends: [NativeFunction, FunctionObject, Object],
    methods: HOST_FUNCTION_METHODS,
    finalize: finalize
);

impl Finalize for HostFunction {
    fn finalize(&self) {
        if let Some(finalize) = self.host_class.host_function_hooks().finalize {
            // SAFETY: The hook takes a dying function of its class, which stays intact while it runs.
            unsafe { finalize(lend_object_to_hook(self)) };
        }
    }
}

impl Deref for HostFunction {
    type Target = NativeFunction;

    fn deref(&self) -> &NativeFunction {
        &self.base
    }
}

/// The class of the functions of `table`, which extends `parent`.
pub fn derive_host_function_class(table: &'static JSHostClass, parent: &'static Class) -> &'static Class {
    Class::derive_runtime(parent, table.class_name(), &HOST_FUNCTION_METHODS)
}

/// The host function an internal method of a host function class was called on.
pub(crate) fn as_host_function(object: &Object) -> &HostFunction {
    debug_assert!(object.is::<HostFunction>());
    // SAFETY: The object is a HostFunction, which starts with its Object.
    unsafe { &*core::ptr::from_ref(object).cast::<HostFunction>() }
}

fn host_function_has_constructor(object: &Object) -> bool {
    as_host_function(object)
        .host_class
        .has_flag(JS_HOST_CLASS_HAS_CONSTRUCTOR)
}

// [[Call]] and [[Construct]] run the hooks in the function's own execution context, which NativeFunction pushes, so
// the hooks read the arguments and the this value from the VM. The runtime holds no borrow of its state across them.

fn call_through_hook(function: &NativeFunction, vm: &Vm) -> ThrowCompletionOr<Value> {
    let hook = as_host_function(function)
        .host_class
        .host_function_hooks()
        .call
        .expect("a host function class has a call hook");
    // SAFETY: The hook takes a function of its class and the VM.
    completion_from_abi(unsafe { hook(lend_object_to_hook(function), vm_into_abi(vm)) })
}

fn construct_through_hook(
    function: &NativeFunction,
    vm: &Vm,
    new_target: Gc<FunctionObject>,
) -> ThrowCompletionOr<Gc<Object>> {
    let Some(hook) = as_host_function(function).host_class.host_function_hooks().construct else {
        return (NATIVE_FUNCTION_VIRTUAL_METHODS.construct)(function, vm, new_target);
    };
    // SAFETY: The hook takes a function of its class, the VM and the new target, and completes with a live object.
    unsafe {
        object_completion_from_hook(hook(
            lend_object_to_hook(function),
            vm_into_abi(vm),
            object_into_abi(new_target),
        ))
    }
}

impl HostFunction {
    /// HostFunction::create(): a function of the class `table` describes, which defines "length" and then "name",
    /// as CreateBuiltinFunction does. The prototype defaults to %Function.prototype%.
    ///
    /// # Safety
    ///
    /// `table` must be of kind JS_HOST_CLASS_FUNCTION, and `host_data` absent or a live cell of the VM's heap.
    pub unsafe fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        table: &'static JSHostClass,
        name: Utf16FlyString,
        length: i32,
        prototype: Option<Gc<Object>>,
        host_data: Option<NonNull<c_void>>,
    ) -> Gc<HostFunction> {
        // SAFETY: The caller's guarantees are those of this function.
        let function = unsafe { Self::create_without_own_properties(vm, realm, table, name, prototype, host_data) };
        let configurable = PropertyAttributes::new(Attribute::CONFIGURABLE);
        function.define_direct_property(vm, &vm.names.length.clone(), Value::from_i32(length), configurable);
        let name = Value::from_string(PrimitiveString::create_from_fly_string(vm, &function.name()));
        function.define_direct_property(vm, &vm.names.name.clone(), name, configurable);
        function
    }

    /// HostFunction::create_without_own_properties(): for a caller that defines the function's own properties
    /// itself, in an order of its own. The function's realm is that of the shape of its prototype, which defaults to
    /// %Function.prototype%, as for a C++ NativeFunction made from a name and a prototype.
    ///
    /// # Safety
    ///
    /// As for create().
    pub unsafe fn create_without_own_properties(
        vm: &Vm,
        realm: Gc<Realm>,
        table: &'static JSHostClass,
        name: Utf16FlyString,
        prototype: Option<Gc<Object>>,
        host_data: Option<NonNull<c_void>>,
    ) -> Gc<HostFunction> {
        let (class, allocator) = runtime_class_and_allocator_of_host_class(vm, table, JS_HOST_CLASS_FUNCTION);
        let prototype = prototype.unwrap_or_else(|| realm.function_prototype());
        let base = NativeFunction::new_with_name(vm, class, name, prototype);
        copy_host_class_flags_into_object(table, &base);
        let function = HostFunction {
            base,
            host_class: table,
            host_data: ForeignCellSlot::empty(),
        };
        // SAFETY: The caller passes an absent or live cell, which the slot then keeps alive.
        unsafe { function.host_data.set(host_data) };
        let function = allocate_object_in(vm, allocator, function);
        function.initialize(vm, realm);
        function
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;

    use super::*;
    use crate::embedding::abi_types::{cell_from_abi, completion_into_abi, throw_completion_into_abi, vm_from_abi};
    use crate::embedding::host::host_object::hook_tests::{
        HookTestEnvironment, REENTRANT_SCRIPT, hook_error, hooks_that_reentered_while, normal, reenter,
    };
    use crate::embedding::host::host_object::tests::leak_host_class;
    use crate::embedding::host::host_object::{HostObject, host_class_of, host_data_of};
    use crate::gc::weak::GcWeak;
    use crate::layout::host_class::{JSCompletion, JSHostFunctionHooks, JSObject, JSVM};
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::run_script;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;

    std::thread_local! {
        static FINALIZED_HOST_FUNCTIONS: Cell<usize> = const { Cell::new(0) };
    }

    /// Sums its arguments, and throws when there are none.
    unsafe extern "C" fn adder_call(_function: *mut JSObject, vm: *mut JSVM) -> JSCompletion {
        reenter("call");
        // SAFETY: The hook receives the VM.
        let vm = unsafe { vm_from_abi(vm) };
        if vm.argument_count() == 0 {
            return hook_error("call");
        }
        let mut sum = 0.0;
        for index in 0..vm.argument_count() {
            match vm.argument(index).to_number(vm) {
                Ok(number) => sum += number.as_f64(),
                Err(throw) => return throw_completion_into_abi(throw),
            }
        }
        normal(Value::from_f64(sum).0)
    }

    unsafe extern "C" fn count_finalized_host_function(_function: *mut JSObject) {
        FINALIZED_HOST_FUNCTIONS.set(FINALIZED_HOST_FUNCTIONS.get() + 1);
    }

    unsafe extern "C" fn throwing_call(_function: *mut JSObject, _vm: *mut JSVM) -> JSCompletion {
        hook_error("call")
    }

    /// Makes { value } objects from new.target's prototype, and throws without an argument.
    unsafe extern "C" fn maker_construct(
        _function: *mut JSObject,
        vm: *mut JSVM,
        new_target: *mut JSObject,
    ) -> JSCompletion {
        reenter("construct");
        // SAFETY: The hook receives the VM and a live new target.
        let (vm, new_target) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSObject>(new_target)) };
        if vm.argument_count() == 0 {
            return hook_error("construct");
        }
        let realm = vm.current_realm().expect("the function runs in a realm");
        let prototype = match new_target.get(vm, &vm.names.prototype.clone()) {
            Ok(prototype) => prototype,
            Err(throw) => return throw_completion_into_abi(throw),
        };
        let prototype = if prototype.is_object() {
            prototype.as_object()
        } else {
            realm.object_prototype()
        };
        let object = Object::create(vm, realm, Some(prototype));
        object.define_direct_property(vm, &key("value"), vm.argument(0), DEFAULT_ATTRIBUTES);
        completion_into_abi(Ok(object))
    }

    fn function_class(name: &'static str, hooks: JSHostFunctionHooks, flags: u32) -> &'static JSHostClass {
        let hooks: &'static JSHostFunctionHooks = Box::leak(Box::new(hooks));
        leak_host_class(
            JS_HOST_CLASS_FUNCTION,
            name,
            None,
            core::ptr::from_ref(hooks).cast(),
            flags,
        )
    }

    fn adder_class() -> &'static JSHostClass {
        function_class(
            "Adder",
            JSHostFunctionHooks {
                call: Some(adder_call),
                construct: None,
                finalize: Some(count_finalized_host_function),
            },
            0,
        )
    }

    fn maker_class() -> &'static JSHostClass {
        function_class(
            "Maker",
            JSHostFunctionHooks {
                call: Some(throwing_call),
                construct: Some(maker_construct),
                finalize: None,
            },
            JS_HOST_CLASS_HAS_CONSTRUCTOR,
        )
    }

    fn create(
        environment: &HookTestEnvironment<'_>,
        table: &'static JSHostClass,
        name: &str,
        length: i32,
        host_data: Option<NonNull<c_void>>,
    ) -> Gc<HostFunction> {
        let vm = crate::embedding::host::host_object::hook_tests::hook_vm();
        // SAFETY: The tables of this module are of kind JS_HOST_CLASS_FUNCTION, and the host data is a live cell.
        unsafe {
            HostFunction::create(
                vm,
                environment.realm(),
                table,
                Utf16FlyString::from_utf8(name),
                length,
                None,
                host_data,
            )
        }
    }

    #[test]
    fn host_function_calls() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let table = adder_class();
        let host_data = Object::create(&vm, environment.realm(), None);
        let adder = create(&environment, table, "add", 2, Some(host_data.as_non_null().cast()));
        environment.define_global("add", adder.upcast());

        assert_eq!(environment.evaluate("add(1, 2, 3)"), "6");
        assert_eq!(environment.evaluate("typeof add"), "function");
        assert_eq!(
            environment.evaluate("Object.getOwnPropertyNames(add).join()"),
            "length,name"
        );
        assert_eq!(environment.evaluate("add.name + add.length"), "add2");
        assert_eq!(
            environment.evaluate("Object.getPrototypeOf(add) === Function.prototype"),
            "true"
        );
        assert_eq!(environment.exception_from("add()"), "TypeError: call threw");
        assert!(environment.exception_from("new add(1)").starts_with("TypeError: "));

        assert_eq!(adder.class().class_name(), "Adder");
        assert!(adder.name() == Utf16FlyString::from_utf8("add"));
        assert!(!adder.has_constructor());
        assert_eq!(adder.realm(), environment.realm());
        assert!(host_class_of(&adder).is_some_and(|host_class| core::ptr::eq(host_class, table)));
        assert!(adder.is::<HostFunction>() && !adder.is::<HostObject>());
        assert_eq!(host_data_of(&adder), Some(host_data.as_non_null().cast()));
    }

    #[test]
    fn host_function_constructs() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let maker = create(&environment, maker_class(), "Maker", 1, None);
        let prototype = Object::create(&vm, environment.realm(), Some(environment.realm().object_prototype()));
        maker.define_direct_property(
            &vm,
            &vm.names.prototype.clone(),
            Value::from_object(prototype),
            PropertyAttributes::new(0),
        );
        environment.define_global("Maker", maker.upcast());

        assert!(maker.has_constructor());
        assert_eq!(
            environment.evaluate("const made = new Maker(5); made.value + ' ' + (made instanceof Maker)"),
            "5 true"
        );
        assert_eq!(
            environment.evaluate(
                "class Derived extends Maker {} const derived = new Derived(3); derived.value + ' ' + (derived instanceof Derived)"
            ),
            "3 true"
        );
        assert_eq!(environment.exception_from("new Maker()"), "TypeError: construct threw");
        assert_eq!(environment.exception_from("Maker(1)"), "TypeError: call threw");
    }

    #[test]
    fn host_function_without_own_properties() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        let error_constructor = run_script(&vm, environment.realm(), "Error").must().as_object();
        // SAFETY: The table is of the right kind.
        let function = unsafe {
            HostFunction::create_without_own_properties(
                &vm,
                environment.realm(),
                adder_class(),
                Utf16FlyString::from_utf8("bare"),
                Some(error_constructor),
                None,
            )
        };
        environment.define_global("bare", function.upcast());

        assert_eq!(environment.evaluate("Object.getOwnPropertyNames(bare).length"), "0");
        assert_eq!(environment.evaluate("Object.getPrototypeOf(bare) === Error"), "true");

        function.define_direct_property(
            &vm,
            &vm.names.prototype.clone(),
            Value::from_object(Object::create(&vm, environment.realm(), None)),
            PropertyAttributes::new(0),
        );
        function.define_direct_property(
            &vm,
            &vm.names.name.clone(),
            Value::from_string(PrimitiveString::create_from_utf8(&vm, "bare")),
            PropertyAttributes::new(Attribute::CONFIGURABLE),
        );
        function.define_direct_property(
            &vm,
            &vm.names.length.clone(),
            Value::from_i32(1),
            PropertyAttributes::new(Attribute::CONFIGURABLE),
        );
        assert_eq!(
            environment.evaluate("Object.getOwnPropertyNames(bare).join()"),
            "prototype,name,length"
        );
    }

    #[inline(never)]
    fn allocate_unreachable_host_functions(environment: &HookTestEnvironment<'_>) -> Vec<GcWeak<HostFunction>> {
        let table = adder_class();
        (0..32)
            .map(|_| {
                let function = create(environment, table, "add", 2, None);
                GcWeak::new(
                    crate::embedding::host::host_object::hook_tests::hook_vm().heap(),
                    function,
                )
            })
            .collect()
    }

    #[test]
    fn host_function_finalize_hook() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        FINALIZED_HOST_FUNCTIONS.set(0);
        let functions = allocate_unreachable_host_functions(&environment);
        vm.heap().collect_garbage();
        let collected_count = functions.iter().filter(|function| function.get().is_none()).count();
        assert!(collected_count > 0);
        assert_eq!(FINALIZED_HOST_FUNCTIONS.get(), collected_count);
    }

    #[test]
    fn call_and_construct_hooks_may_reenter_the_vm() {
        let vm = Vm::create();
        let environment = HookTestEnvironment::new(&vm);
        environment.define_global("add", create(&environment, adder_class(), "add", 2, None).upcast());
        environment.define_global("Maker", create(&environment, maker_class(), "Maker", 1, None).upcast());
        let hooks = hooks_that_reentered_while(REENTRANT_SCRIPT, || {
            assert_eq!(environment.evaluate("add(1, 2) + new Maker(3).value"), "6");
        });
        assert_eq!(hooks, ["call", "construct"]);
    }
}
