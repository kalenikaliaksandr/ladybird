/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Realms.

use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::abi::{JSIntrinsic, JSRealm, js_realm_get_function_realm, js_realm_intrinsic};
use crate::completion::{ThrowCompletionOr, pointer_from_abi};
use crate::layout::JS_LAYOUT_REALM_GLOBAL_OBJECT_OFFSET;
use crate::object::{Object, read_field};
use crate::vm::Vm;

/// A realm. Like a value, it keeps the realm alive only while it is on the stack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Realm {
    pointer: NonNull<JSRealm>,
    _not_send: PhantomData<*const ()>,
}

impl Realm {
    /// # Safety
    ///
    /// `pointer` must be the address of a live realm.
    pub const unsafe fn from_non_null(pointer: NonNull<JSRealm>) -> Self {
        Self {
            pointer,
            _not_send: PhantomData,
        }
    }

    pub const fn as_raw(self) -> *mut JSRealm {
        self.pointer.as_ptr()
    }

    pub fn global_object(self) -> Object {
        // SAFETY: Every realm has a global object once it is set up.
        unsafe {
            Object::from_non_null(NonNull::new_unchecked(read_field(
                self.as_raw().cast::<u8>(),
                JS_LAYOUT_REALM_GLOBAL_OBJECT_OFFSET,
            )))
        }
    }

    pub fn intrinsic(self, vm: Vm, intrinsic: JSIntrinsic) -> Object {
        // SAFETY: The realm is live, and every intrinsic exists.
        unsafe { Object::from_raw(js_realm_intrinsic(vm.as_raw(), self.as_raw(), intrinsic)) }
            .expect("the realm has the intrinsic")
    }

    /// GetFunctionRealm(function), https://tc39.es/ecma262/#sec-getfunctionrealm
    pub fn of_function(vm: Vm, function: Object) -> ThrowCompletionOr<Realm> {
        // SAFETY: The function is live.
        let realm =
            pointer_from_abi::<JSRealm>(unsafe { js_realm_get_function_realm(vm.as_raw(), function.as_raw()) })?;
        // SAFETY: A normal completion carries a live realm.
        Ok(unsafe { Realm::from_non_null(NonNull::new(realm).expect("a function has a realm")) })
    }
}
