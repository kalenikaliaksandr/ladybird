/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Native functions, which read their arguments and this value from the running execution context.

/// Defines a JSNativeFunction whose body runs with the VM bound to `$vm` and returns a ThrowCompletionOr<Value>.
///
/// C++ returns the 16-byte completion of a native function in two registers, as C returns a JSCompletion, except on
/// Mach-O x86_64 and Windows, where it returns it through a buffer the caller passes first. The macro defines the
/// function with the convention of the target.
#[macro_export]
macro_rules! native_function {
    ($(#[$attribute:meta])* $visibility:vis fn $name:ident($vm:ident) $body:block) => {
        $(#[$attribute])*
        #[cfg(not(any(all(target_arch = "x86_64", target_vendor = "apple"), target_os = "windows")))]
        $visibility unsafe extern "C" fn $name(vm: *mut $crate::abi::JSVM) -> $crate::abi::JSCompletion {
            // SAFETY: The runtime calls a native function with its VM, on the VM's thread.
            let $vm = unsafe { $crate::vm::Vm::from_raw(vm) };
            let result: $crate::completion::ThrowCompletionOr<$crate::value::Value> = (|| $body)();
            $crate::completion::into_abi(result)
        }

        $(#[$attribute])*
        #[cfg(any(all(target_arch = "x86_64", target_vendor = "apple"), target_os = "windows"))]
        $visibility unsafe extern "C" fn $name(result: *mut $crate::abi::JSCompletion, vm: *mut $crate::abi::JSVM) {
            // SAFETY: The runtime calls a native function with its VM, on the VM's thread.
            let $vm = unsafe { $crate::vm::Vm::from_raw(vm) };
            let completion: $crate::completion::ThrowCompletionOr<$crate::value::Value> = (|| $body)();
            // SAFETY: The runtime passes a buffer for the completion.
            unsafe { result.write($crate::completion::into_abi(completion)) };
        }
    };
}
