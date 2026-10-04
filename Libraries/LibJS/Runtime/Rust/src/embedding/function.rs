/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Native functions, and calling and constructing functions.
//!
//! The functions here follow the contract object.rs states for the embedding module.

#![allow(
    clippy::missing_safety_doc,
    reason = "object.rs states the contract every exported function shares"
)]

use core::ffi::{c_char, c_void};
use core::mem::offset_of;
use core::ptr::NonNull;

use crate::embedding::abi_types::{
    JSOwnedUtf16String, JSRealm, JSUtf16View, cell_from_abi, completion_from_abi, completion_into_abi, object_into_abi,
    optional_cell_from_abi, optional_cell_into_abi, owned_utf16_string_into_abi, property_key_from_abi, vm_from_abi,
    vm_into_abi,
};
use crate::embedding::object::{function_from_abi, optional_function_from_abi, values_from_abi};
use crate::gc::foreign::ForeignCellSlot;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::host_class::{JSCompletion, JSObject, JSPropertyKey, JSVM, JSValue};
use crate::layout::value::Value;
use crate::layout_forward::{RawNativeFunctionPointer, RawNativeFunctionResult};
use crate::runtime::abstract_operations::{call, construct};
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::native_function::{
    DirectGetterConfiguration, DirectGetterFunction, NativeFunction, RawNativeFunction,
};

/// A raw native function: the C++ runtime's NativeFunctionPointer, ThrowCompletionOr<Value> (*)(VM&). It reads its
/// arguments, this value and callee from the running execution context. C++ returns the 16-byte ThrowCompletionOr in
/// two registers, as C returns a JSCompletion, except on Mach-O x86_64 and Windows, where it returns it through a
/// buffer the caller passes first.
#[cfg(not(any(all(target_arch = "x86_64", target_vendor = "apple"), target_os = "windows")))]
pub type JSNativeFunction = Option<unsafe extern "C" fn(vm: *mut JSVM) -> JSCompletion>;

/// A raw native function: the C++ runtime's NativeFunctionPointer, ThrowCompletionOr<Value> (*)(VM&). It reads its
/// arguments, this value and callee from the running execution context. C++ returns the 16-byte ThrowCompletionOr in
/// two registers, as C returns a JSCompletion, except on Mach-O x86_64 and Windows, where it returns it through a
/// buffer the caller passes first.
#[cfg(any(all(target_arch = "x86_64", target_vendor = "apple"), target_os = "windows"))]
pub type JSNativeFunction = Option<unsafe extern "C" fn(result: *mut JSCompletion, vm: *mut JSVM)>;

const _: () = assert!(size_of::<JSNativeFunction>() == size_of::<RawNativeFunctionPointer>());
const _: () = assert!(size_of::<JSCompletion>() == size_of::<RawNativeFunctionResult>());
const _: () = assert!(offset_of!(JSCompletion, payload) == offset_of!(RawNativeFunctionResult, payload));
const _: () = assert!(offset_of!(JSCompletion, variant) == offset_of!(RawNativeFunctionResult, variant));

pub(crate) fn raw_native_function_from_abi(function: JSNativeFunction) -> RawNativeFunctionPointer {
    // SAFETY: The two types differ only in the names of the VM pointer and of the completion struct, which have the
    //         same layout, so they describe the same calling convention.
    unsafe { core::mem::transmute::<JSNativeFunction, RawNativeFunctionPointer>(function) }
}

/// The behaviour of a closure function: called with the context the closure was created with and the VM, it reads its
/// arguments like a raw native function.
pub type JSClosureCall = Option<unsafe extern "C" fn(context: *mut c_void, vm: *mut JSVM) -> JSCompletion>;

/// An embedder's closure: a call and the C++ GC cell it captures, which the function keeps alive.
struct HostClosure {
    call: unsafe extern "C" fn(context: *mut c_void, vm: *mut JSVM) -> JSCompletion,
    context: ForeignCellSlot,
}

// SAFETY: The context is the only cell a closure holds.
unsafe impl Trace for HostClosure {
    fn trace(&self, visitor: &mut Visitor) {
        self.context.trace(visitor);
    }
}

/// # Safety
///
/// `context` must be null or a live cell of the VM's heap.
unsafe fn host_closure_from_abi(call: JSClosureCall, context: *mut c_void) -> HostClosure {
    let closure = HostClosure {
        call: call.expect("the closure has a call"),
        context: ForeignCellSlot::empty(),
    };
    // SAFETY: The caller passes null or a live cell, which the closure keeps alive from here on.
    unsafe { closure.context.set(NonNull::new(context)) };
    closure
}

fn call_host_closure(vm: &Vm, closure: &HostClosure) -> ThrowCompletionOr<Value> {
    // SAFETY: The embedder created the closure with a call that takes this context and the VM. Nothing of the
    //         runtime's state is borrowed while the call runs, as it may call back into the VM.
    let completion = unsafe { (closure.call)(closure.context.as_ptr(), vm_into_abi(vm)) };
    completion_from_abi(completion)
}

/// # Safety
///
/// `prefix` must be null or point to `prefix_length` bytes of UTF-8.
unsafe fn prefix_from_abi<'prefix>(prefix: *const c_char, prefix_length: usize) -> Option<&'prefix str> {
    if prefix.is_null() {
        return None;
    }
    // SAFETY: The caller passes `prefix_length` bytes at `prefix`.
    let bytes = unsafe { core::slice::from_raw_parts(prefix.cast::<u8>(), prefix_length) };
    Some(core::str::from_utf8(bytes).expect("the prefix is UTF-8"))
}

// Creating native functions

/// CreateBuiltinFunction with a raw native function as its behaviour, as C++ NativeFunction::create() does: "length"
/// is the length, "name" is the key, after the prefix (null for none, or `prefix_length` bytes of UTF-8 such as
/// "get") and a space. Its [[Realm]] is the realm, or the current realm when that is null. Borrows the key. Returns an
/// unrooted function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_create_native(
    vm: *mut JSVM,
    behaviour: JSNativeFunction,
    length: i32,
    name: *const JSPropertyKey,
    realm: *mut JSRealm,
    prefix: *const c_char,
    prefix_length: usize,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, name, realm, prefix) = unsafe {
        (
            vm_from_abi(vm),
            property_key_from_abi(name),
            optional_cell_from_abi::<JSRealm>(realm),
            prefix_from_abi(prefix, prefix_length),
        )
    };
    object_into_abi(RawNativeFunction::create(
        vm,
        raw_native_function_from_abi(behaviour),
        length,
        name,
        realm,
        prefix,
        None,
    ))
}

/// A raw native function of the realm with the name that call stacks show, and no "length" or "name" property, as C++
/// NativeFunction::create(realm, name, behaviour) creates one. Borrows the name. Returns an unrooted function. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_create_native_with_name(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    name: JSUtf16View,
    behaviour: JSNativeFunction,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm, name) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSRealm>(realm), name.as_view()) };
    object_into_abi(RawNativeFunction::create_with_name(
        vm,
        realm,
        &name.to_utf16_fly_string(),
        raw_native_function_from_abi(behaviour),
    ))
}

/// js_function_create_native() for a closure: the function calls `call` with `context`, which is null or a C++ GC cell
/// that the function keeps alive for as long as it lives, such as the GC::Function the facade wraps an AK::Function
/// in. Borrows the key. Returns an unrooted function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_create_closure(
    vm: *mut JSVM,
    call: JSClosureCall,
    context: *mut c_void,
    length: i32,
    name: *const JSPropertyKey,
    realm: *mut JSRealm,
    prefix: *const c_char,
    prefix_length: usize,
) -> *mut JSObject {
    // SAFETY: See the module documentation. The context is null or a live cell.
    let (vm, closure, name, realm, prefix) = unsafe {
        (
            vm_from_abi(vm),
            host_closure_from_abi(call, context),
            property_key_from_abi(name),
            optional_cell_from_abi::<JSRealm>(realm),
            prefix_from_abi(prefix, prefix_length),
        )
    };
    object_into_abi(NativeFunction::create(
        vm,
        closure,
        call_host_closure,
        length,
        name,
        realm,
        prefix,
        None,
    ))
}

/// js_function_create_native_with_name() for a closure, whose context is null or a C++ GC cell that the function
/// keeps alive. Borrows the name. Returns an unrooted function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_create_closure_with_name(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    name: JSUtf16View,
    call: JSClosureCall,
    context: *mut c_void,
) -> *mut JSObject {
    // SAFETY: See the module documentation. The context is null or a live cell.
    let (vm, realm, name, closure) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            name.as_view(),
            host_closure_from_abi(call, context),
        )
    };
    object_into_abi(NativeFunction::create_with_name(
        vm,
        realm,
        &name.to_utf16_fly_string(),
        closure,
        call_host_closure,
    ))
}

/// Where a direct getter finds the value it returns, as byte offsets of pointer-sized fields: the wrapped
/// implementation in the wrapper (JS_HOST_OBJECT_WRAPPABLE_OFFSET for a host object), the field of the implementation
/// that holds the value's GC::Weak implementation, the main world wrapper in that value, and the cell in a
/// GC::WeakImpl. This is C++ JS::DirectGetterConfiguration.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSDirectGetterConfiguration {
    pub wrapper_implementation_offset: usize,
    pub implementation_value_offset: usize,
    pub main_world_wrapper_offset: usize,
    pub weak_impl_value_offset: usize,
}

/// js_function_create_native() for an attribute getter that the interpreter can answer without calling the behaviour,
/// by following the configuration's offsets from the this value. Every offset must be a multiple of the pointer size.
/// The realm must not be null. Borrows the key and the configuration. Returns an unrooted function. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_create_direct_getter(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    behaviour: JSNativeFunction,
    length: i32,
    name: *const JSPropertyKey,
    configuration: *const JSDirectGetterConfiguration,
    prefix: *const c_char,
    prefix_length: usize,
) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, realm, name, configuration, prefix) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSRealm>(realm),
            property_key_from_abi(name),
            *configuration,
            prefix_from_abi(prefix, prefix_length),
        )
    };
    object_into_abi(DirectGetterFunction::create(
        vm,
        realm,
        raw_native_function_from_abi(behaviour),
        length,
        name,
        DirectGetterConfiguration {
            wrapper_implementation_offset: configuration.wrapper_implementation_offset,
            implementation_value_offset: configuration.implementation_value_offset,
            main_world_wrapper_offset: configuration.main_world_wrapper_offset,
            weak_impl_value_offset: configuration.weak_impl_value_offset,
        },
        prefix,
    ))
}

// What a function is

/// The function's [[Realm]], or null for a function without one, such as a bound function or a proxy. Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_realm(function: *mut JSObject) -> *mut JSRealm {
    // SAFETY: See the module documentation.
    let function = unsafe { function_from_abi(function) };
    optional_cell_into_abi::<JSRealm>(function.realm())
}

/// The name of the function that call stacks show, which the caller owns. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_name_for_call_stack(function: *mut JSObject) -> JSOwnedUtf16String {
    // SAFETY: See the module documentation.
    let function = unsafe { function_from_abi(function) };
    owned_utf16_string_into_abi(
        function
            .upcast::<crate::runtime::object::Object>()
            .name_for_call_stack(),
    )
}

// Calling functions

/// Call(F, V, argumentsList), which throws a TypeError when the function is not callable. The arguments, `count`
/// values at `arguments`, must stay reachable until the call returns, as they do on the stack or in a root vector.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_call(
    vm: *mut JSVM,
    function: JSValue,
    this_value: JSValue,
    arguments: *const JSValue,
    count: usize,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, arguments) = unsafe { (vm_from_abi(vm), values_from_abi(arguments, count)) };
    completion_into_abi(call(vm, Value(function), Value(this_value), arguments))
}

/// Construct(F, argumentsList, newTarget), whose payload is the new object. The function must be a constructor, and
/// a null new target stands for the function itself. The arguments must stay reachable as for js_function_call().
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_construct(
    vm: *mut JSVM,
    function: *mut JSObject,
    arguments: *const JSValue,
    count: usize,
    new_target: *mut JSObject,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, function, arguments, new_target) = unsafe {
        (
            vm_from_abi(vm),
            function_from_abi(function),
            values_from_abi(arguments, count),
            optional_function_from_abi(new_target),
        )
    };
    completion_into_abi(construct(vm, function, arguments, new_target))
}

/// GetMethod(V, P), whose payload is the function, or null when the property is undefined or null. Borrows the key.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_get_method(
    vm: *mut JSVM,
    value: JSValue,
    key: *const JSPropertyKey,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, key) = unsafe { (vm_from_abi(vm), property_key_from_abi(key)) };
    completion_into_abi(Value(value).get_method(vm, key))
}

/// Invoke(V, P, argumentsList). Borrows the key. The arguments must stay reachable as for js_function_call(). Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_function_invoke(
    vm: *mut JSVM,
    value: JSValue,
    key: *const JSPropertyKey,
    arguments: *const JSValue,
    count: usize,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, key, arguments) = unsafe {
        (
            vm_from_abi(vm),
            property_key_from_abi(key),
            values_from_abi(arguments, count),
        )
    };
    completion_into_abi(Value(value).invoke(vm, key, arguments))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;

    use super::*;
    use crate::embedding::abi_types::cell_into_abi;
    use crate::gc::weak::GcWeak;
    use crate::layout::cell::Gc;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::object::Object;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::property_key::PropertyKey;
    use crate::runtime::realm::test_realm::key;
    use crate::utilities::initialize_realm;

    fn abi_key(key: &PropertyKey) -> *const JSPropertyKey {
        core::ptr::from_ref(key).cast()
    }

    fn ascii_view_of(ascii: &str) -> JSUtf16View {
        JSUtf16View {
            data: ascii.as_ptr().cast(),
            length_in_code_units: ascii.len(),
            has_ascii_storage: true,
        }
    }

    std::thread_local! {
        static CALLS_SEEN_BY_THE_CLOSURE: Cell<u32> = const { Cell::new(0) };
    }

    /// A closure that calls back into the VM: it runs a script that calls the closure's own function again, until the
    /// recursion reaches the depth its first argument asks for, and returns the depth it reached.
    unsafe extern "C" fn reentering_closure(context: *mut c_void, vm: *mut JSVM) -> JSCompletion {
        CALLS_SEEN_BY_THE_CLOSURE.with(|calls| calls.set(calls.get() + 1));
        // SAFETY: The function passes the VM it runs on.
        let vm = unsafe { vm_from_abi(vm) };
        assert!(!context.is_null());
        let depth = vm.argument(0).as_i32();
        if depth == 0 {
            return completion_into_abi(Ok(Value::from_i32(0)));
        }
        vm.heap().collect_garbage();
        let realm = vm.current_realm().expect("the closure runs in a realm");
        let result = run_script(vm, realm, &format!("closure({}) + 1", depth - 1));
        completion_into_abi(result)
    }

    unsafe extern "C" fn throwing_closure(_: *mut c_void, vm: *mut JSVM) -> JSCompletion {
        // SAFETY: The function passes the VM it runs on.
        let vm = unsafe { vm_from_abi(vm) };
        completion_into_abi::<()>(Err(crate::runtime::completion::Throw::new(vm.argument(0))))
    }

    #[test]
    fn closures_can_call_back_into_the_vm() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let context = Object::create(&vm, realm, None);
        let name = key("closure");
        // SAFETY: The arguments are live, and the context is a cell of the VM's heap.
        let closure = unsafe {
            js_function_create_closure(
                vm_into_abi(&vm),
                Some(reentering_closure),
                object_into_abi(context).cast(),
                1,
                abi_key(&name),
                core::ptr::null_mut(),
                core::ptr::null(),
                0,
            )
        };
        // SAFETY: The closure is a live function.
        let closure = unsafe { cell_from_abi::<JSObject>(closure) };
        realm
            .global_object()
            .define_direct_property(&vm, &name, Value::from_object(closure), DEFAULT_ATTRIBUTES);
        CALLS_SEEN_BY_THE_CLOSURE.set(0);
        assert_eq!(utf8(run_script(&vm, realm, "closure(3)").must()), "3");
        assert_eq!(CALLS_SEEN_BY_THE_CLOSURE.get(), 4);
        assert_eq!(
            utf8(run_script(&vm, realm, "closure.name + ' ' + closure.length").must()),
            "closure 1"
        );

        let thrower_name = key("thrower");
        // SAFETY: As above, without a context.
        let thrower = unsafe {
            js_function_create_closure(
                vm_into_abi(&vm),
                Some(throwing_closure),
                core::ptr::null_mut(),
                1,
                abi_key(&thrower_name),
                core::ptr::null_mut(),
                c"get".as_ptr(),
                3,
            )
        };
        // SAFETY: As above.
        let thrower = unsafe { cell_from_abi::<JSObject>(thrower) };
        realm.global_object().define_direct_property(
            &vm,
            &thrower_name,
            Value::from_object(thrower),
            DEFAULT_ATTRIBUTES,
        );
        assert_eq!(
            utf8(run_script(&vm, realm, "try { thrower(42) } catch (e) { e + ' ' + thrower.name }").must()),
            "42 get thrower"
        );
    }

    const CLOSURE_COUNT: usize = 64;

    unsafe extern "C" fn returning_closure(_: *mut c_void, _: *mut JSVM) -> JSCompletion {
        completion_into_abi(Ok(Value::from_i32(1)))
    }

    struct Closures {
        held_contexts: Vec<GcWeak<Object>>,
        unheld_contexts: Vec<GcWeak<Object>>,
    }

    #[inline(never)]
    fn create_closures(vm: &Vm, realm: Gc<crate::runtime::realm::Realm>, holder: Gc<Object>) -> Closures {
        let mut closures = Closures {
            held_contexts: Vec::new(),
            unheld_contexts: Vec::new(),
        };
        for index in 0..CLOSURE_COUNT {
            let context = Object::create(vm, realm, None);
            // SAFETY: The arguments are live, and the context is a cell of the VM's heap.
            let closure = unsafe {
                js_function_create_closure_with_name(
                    vm_into_abi(vm),
                    cell_into_abi::<JSRealm>(realm),
                    ascii_view_of("held"),
                    Some(returning_closure),
                    object_into_abi(context).cast(),
                )
            };
            // SAFETY: The closure is a live function.
            let closure = unsafe { cell_from_abi::<JSObject>(closure) };
            holder.define_direct_property(
                vm,
                &PropertyKey::from(index as u32),
                Value::from_object(closure),
                DEFAULT_ATTRIBUTES,
            );
            closures.held_contexts.push(GcWeak::new(vm.heap(), context));
            closures
                .unheld_contexts
                .push(GcWeak::new(vm.heap(), Object::create(vm, realm, None)));
        }
        closures
    }

    #[test]
    fn a_closure_keeps_its_context_alive() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let holder = Object::create(&vm, realm, None);
        realm.global_object().define_direct_property(
            &vm,
            &key("holder"),
            Value::from_object(holder),
            DEFAULT_ATTRIBUTES,
        );
        let closures = create_closures(&vm, realm, holder);
        vm.heap().collect_garbage();

        let collected_unheld_contexts = closures
            .unheld_contexts
            .iter()
            .filter(|context| context.get().is_none())
            .count();
        assert!(
            collected_unheld_contexts >= CLOSURE_COUNT / 2,
            "only {collected_unheld_contexts} of the contexts nothing holds were collected"
        );
        assert!(closures.held_contexts.iter().all(|context| context.get().is_some()));
        assert_eq!(utf8(run_script(&vm, realm, "holder[0]() + holder[63]()").must()), "2");
    }

    #[allow(clippy::unnecessary_wraps, reason = "raw native functions return a completion")]
    fn add_and_call_back(vm: &Vm) -> ThrowCompletionOr<Value> {
        let realm = vm.current_realm().expect("the native runs in a realm");
        let from_script = run_script(vm, realm, "globalThis.base ?? 0")?;
        Ok(Value::from_i32(
            vm.argument(0).as_i32() + vm.argument(1).as_i32() + from_script.as_i32(),
        ))
    }

    #[test]
    fn raw_natives_and_direct_getters_are_created_from_the_abi_types() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let raw_native = crate::runtime::native_function::raw_native!(add_and_call_back);
        // SAFETY: Both types describe the same calling convention, which the conversion back checks.
        let behaviour = unsafe { core::mem::transmute::<RawNativeFunctionPointer, JSNativeFunction>(raw_native) };
        let name = key("add");
        // SAFETY: The arguments are live.
        let add = unsafe {
            js_function_create_native(
                vm_into_abi(&vm),
                behaviour,
                2,
                abi_key(&name),
                cell_into_abi::<JSRealm>(realm),
                core::ptr::null(),
                0,
            )
        };
        let arguments = [Value::from_i32(1).0, Value::from_i32(2).0];
        // SAFETY: As above.
        let sum = unsafe {
            js_function_call(
                vm_into_abi(&vm),
                Value::from_object(cell_from_abi::<JSObject>(add)).0,
                Value::UNDEFINED.0,
                arguments.as_ptr(),
                arguments.len(),
            )
        };
        assert_eq!((sum.variant, sum.payload), (JS_COMPLETION_NORMAL, Value::from_i32(3).0));
        // SAFETY: As above.
        let not_callable = unsafe {
            js_function_call(
                vm_into_abi(&vm),
                Value::from_i32(1).0,
                Value::UNDEFINED.0,
                core::ptr::null(),
                0,
            )
        };
        assert_eq!(not_callable.variant, JS_COMPLETION_THROW);

        // Like C++, CreateBuiltinFunction only names the function through its "name" property, and a function created
        // with a name only has the name call stacks show.
        // SAFETY: As above.
        unsafe {
            assert_eq!(js_function_realm(add), cell_into_abi::<JSRealm>(realm));
            let name_for_call_stack = ak::Utf16String::from_raw_owned(js_function_name_for_call_stack(add));
            assert_eq!(crate::utf16::Utf16View::of_string(&name_for_call_stack).to_utf8(), "");
            let named = js_function_create_native_with_name(
                vm_into_abi(&vm),
                cell_into_abi::<JSRealm>(realm),
                ascii_view_of("named"),
                behaviour,
            );
            let name_for_call_stack = ak::Utf16String::from_raw_owned(js_function_name_for_call_stack(named));
            assert_eq!(
                crate::utf16::Utf16View::of_string(&name_for_call_stack).to_utf8(),
                "named"
            );
        }

        let configuration = JSDirectGetterConfiguration {
            wrapper_implementation_offset: 80,
            implementation_value_offset: 16,
            main_world_wrapper_offset: 24,
            weak_impl_value_offset: 8,
        };
        // SAFETY: As above.
        let getter = unsafe {
            js_function_create_direct_getter(
                vm_into_abi(&vm),
                cell_into_abi::<JSRealm>(realm),
                behaviour,
                0,
                abi_key(&name),
                &raw const configuration,
                c"get".as_ptr(),
                3,
            )
        };
        // SAFETY: As above.
        let getter = unsafe { cell_from_abi::<JSObject>(getter) };
        assert!(getter.is_direct_getter_function());
        realm.global_object().define_direct_property(
            &vm,
            &key("getter"),
            Value::from_object(getter),
            DEFAULT_ATTRIBUTES,
        );
        assert_eq!(
            utf8(run_script(&vm, realm, "getter.name + ' ' + getter(4, 5)").must()),
            "get add 9"
        );
    }
}
