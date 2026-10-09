/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Typed arrays.

use crate::abi::{
    JSTypedArrayWithBufferWitness, js_typed_array_byte_length_of_witness, js_typed_array_create_on_buffer,
    js_typed_array_is_out_of_bounds, js_typed_array_make_witness_record, js_typed_array_viewed_array_buffer,
};
use crate::array_buffer::ArrayBuffer;
use crate::layout::{
    JS_LAYOUT_OBJECT_FLAG_IS_TYPED_ARRAY, JS_LAYOUT_TYPED_ARRAY_BYTE_OFFSET_OFFSET, JS_LAYOUT_TYPED_ARRAY_KIND_OFFSET,
};
use crate::object::{Object, read_field};
use crate::realm::Realm;
use crate::vm::Vm;

const _: () = assert!(crate::layout::JS_LAYOUT_TYPED_ARRAY_KIND_SIZE == size_of::<u8>());
const _: () = assert!(crate::layout::JS_LAYOUT_TYPED_ARRAY_BYTE_OFFSET_SIZE == size_of::<u32>());

#[derive(Clone, Copy)]
pub struct TypedArray(Object);

impl TypedArray {
    /// The object as a typed array whose [[TypedArrayName]] is that of the kind, one of the
    /// JS_LAYOUT_TYPED_ARRAY_KIND_ constants.
    pub fn of_kind(object: Object, kind: usize) -> Option<Self> {
        if usize::from(object.flags()) & JS_LAYOUT_OBJECT_FLAG_IS_TYPED_ARRAY == 0 {
            return None;
        }
        // SAFETY: The object is a typed array, which has a kind.
        let object_kind = unsafe { read_field::<u8>(object.as_raw().cast::<u8>(), JS_LAYOUT_TYPED_ARRAY_KIND_OFFSET) };
        (usize::from(object_kind) == kind).then_some(Self(object))
    }

    pub const fn object(self) -> Object {
        self.0
    }

    /// A new typed array of the kind that views `length` elements from the start of `buffer`, which must have room
    /// for them.
    pub fn create_on_buffer(vm: Vm, realm: Realm, kind: usize, length: u32, buffer: ArrayBuffer) -> Self {
        let kind = u8::try_from(kind).expect("a typed array kind fits in a byte");
        // SAFETY: The VM, the realm and the buffer are live.
        let object = unsafe {
            js_typed_array_create_on_buffer(vm.as_raw(), realm.as_raw(), kind, length, buffer.object().as_raw())
        };
        // SAFETY: The runtime returns the new typed array.
        Self(unsafe { Object::from_raw(object) }.expect("the runtime creates the typed array"))
    }

    pub fn viewed_array_buffer(self) -> ArrayBuffer {
        // SAFETY: The object is a live typed array.
        let buffer = unsafe { Object::from_raw(js_typed_array_viewed_array_buffer(self.0.as_raw())) };
        // SAFETY: A typed array views an ArrayBuffer or a SharedArrayBuffer.
        unsafe { ArrayBuffer::from_object_unchecked(buffer.expect("a typed array views a buffer")) }
    }

    /// [[ByteOffset]].
    pub fn byte_offset(self) -> usize {
        // SAFETY: The object is a typed array, which has a byte offset.
        unsafe { read_field::<u32>(self.0.as_raw().cast::<u8>(), JS_LAYOUT_TYPED_ARRAY_BYTE_OFFSET_OFFSET) as usize }
    }

    /// MakeTypedArrayWithBufferWitnessRecord(obj, order), with an order of JS_ARRAY_BUFFER_ORDER_*.
    pub fn witness_record(self, order: u8) -> TypedArrayWithBufferWitness {
        // SAFETY: The object is a live typed array.
        TypedArrayWithBufferWitness(unsafe { js_typed_array_make_witness_record(self.0.as_raw(), order) })
    }
}

/// A Typed Array With Buffer Witness Record, https://tc39.es/ecma262/#sec-typedarray-with-buffer-witness-records
pub struct TypedArrayWithBufferWitness(JSTypedArrayWithBufferWitness);

impl TypedArrayWithBufferWitness {
    /// IsTypedArrayOutOfBounds(taRecord).
    pub fn is_out_of_bounds(&self) -> bool {
        // SAFETY: The record is of a live typed array.
        unsafe { js_typed_array_is_out_of_bounds(&raw const self.0) }
    }

    /// TypedArrayByteLength(taRecord).
    pub fn byte_length(&self) -> usize {
        // SAFETY: The record is of a live typed array.
        unsafe { js_typed_array_byte_length_of_witness(&raw const self.0) as usize }
    }
}
