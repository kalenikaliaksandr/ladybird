/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! ArrayBuffer and SharedArrayBuffer objects.

use crate::abi::{
    js_array_buffer_create, js_array_buffer_create_from_bytes, js_array_buffer_data, js_array_buffer_is_detached,
    js_array_buffer_is_fixed_length, js_array_buffer_is_shared_array_buffer,
};
use crate::completion::{ThrowCompletionOr, pointer_from_abi};
use crate::object::Object;
use crate::realm::Realm;
use crate::vm::Vm;

#[derive(Clone, Copy)]
pub struct ArrayBuffer(Object);

impl ArrayBuffer {
    /// # Safety
    ///
    /// `object` must be an ArrayBuffer or a SharedArrayBuffer.
    pub const unsafe fn from_object_unchecked(object: Object) -> Self {
        Self(object)
    }

    pub const fn object(self) -> Object {
        self.0
    }

    /// A new zero-filled ArrayBuffer. Throws a RangeError if its storage cannot be allocated.
    pub fn create(vm: Vm, realm: Realm, byte_length: usize) -> ThrowCompletionOr<Self> {
        // SAFETY: The VM and the realm are live.
        let buffer =
            pointer_from_abi(unsafe { js_array_buffer_create(vm.as_raw(), realm.as_raw(), byte_length, false) })?;
        // SAFETY: A normal completion carries the new buffer.
        Ok(Self(
            unsafe { Object::from_raw(buffer) }.expect("the runtime creates the buffer"),
        ))
    }

    /// A new ArrayBuffer with a copy of the bytes. Aborts if its storage cannot be allocated.
    pub fn create_from_bytes(vm: Vm, realm: Realm, bytes: &[u8]) -> Self {
        let data = if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        };
        // SAFETY: The VM and the realm are live, and the bytes are readable for their length.
        let buffer =
            unsafe { js_array_buffer_create_from_bytes(vm.as_raw(), realm.as_raw(), data, bytes.len(), false) };
        // SAFETY: The runtime returns the new buffer.
        Self(unsafe { Object::from_raw(buffer) }.expect("the runtime creates the buffer"))
    }

    pub fn is_detached(self) -> bool {
        // SAFETY: The object is a live ArrayBuffer.
        unsafe { js_array_buffer_is_detached(self.0.as_raw()) }
    }

    pub fn is_shared(self) -> bool {
        // SAFETY: The object is a live ArrayBuffer.
        unsafe { js_array_buffer_is_shared_array_buffer(self.0.as_raw()) }
    }

    pub fn is_fixed_length(self) -> bool {
        // SAFETY: The object is a live ArrayBuffer.
        unsafe { js_array_buffer_is_fixed_length(self.0.as_raw()) }
    }

    /// The address and the length of the buffer's bytes, which stay valid until the buffer is detached, resized or
    /// given new storage. Another thread may change the bytes of a SharedArrayBuffer at any time.
    pub fn data(self) -> (*mut u8, usize) {
        let mut byte_length = 0;
        // SAFETY: The object is a live ArrayBuffer, and the out parameter is valid for writes.
        let data = unsafe { js_array_buffer_data(self.0.as_raw(), &raw mut byte_length) };
        (data, byte_length)
    }
}
