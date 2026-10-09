/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Conversions between JS values and the Rust types of IDL values,
//! https://webidl.spec.whatwg.org/#es-type-mapping

use ak::{Utf16String, Utf16StringUnits, convert_valid_utf16_to_utf8, utf8_length_from_valid_utf16, validate_utf16};
use libjs_embedding::completion::ThrowCompletionOr;
use libjs_embedding::error::throw_type_error;
use libjs_embedding::string::{from_utf16_string, to_utf16_string};
use libjs_embedding::typed_array::TypedArray;
use libjs_embedding::value::Value;
use libjs_embedding::vm::Vm;

use crate::bindings::CallContext;
use crate::bindings::messages::{SHARED_ARRAY_BUFFER, not_an_object_of_type};

/// A USVString, a string without lone surrogates. Its UTF-16 code units are always well-formed.
pub(crate) struct UsvString(Utf16String);

impl UsvString {
    pub(crate) fn from_ascii(string: &[u8]) -> Self {
        Self(Utf16String::from_ascii(string))
    }

    pub(crate) fn units(&self) -> Utf16StringUnits<'_> {
        self.0.as_units()
    }

    pub(crate) fn length_in_code_units(&self) -> usize {
        match self.units() {
            Utf16StringUnits::Ascii(bytes) => bytes.len(),
            Utf16StringUnits::Utf16(code_units) => code_units.len(),
        }
    }

    /// The number of bytes of the string's UTF-8 encoding.
    pub(crate) fn utf8_length(&self) -> usize {
        match self.units() {
            Utf16StringUnits::Ascii(bytes) => bytes.len(),
            Utf16StringUnits::Utf16(code_units) => utf8_length_from_valid_utf16(code_units),
        }
    }

    /// Writes the string's UTF-8 encoding to `output`, which has exactly utf8_length() bytes.
    pub(crate) fn encode_utf8(&self, output: &mut [u8]) {
        match self.units() {
            Utf16StringUnits::Ascii(bytes) => output.copy_from_slice(bytes),
            Utf16StringUnits::Utf16(code_units) => {
                debug_assert_eq!(output.len(), utf8_length_from_valid_utf16(code_units));
                // SAFETY: A USVString is well-formed, and the caller gives the output room for its encoding.
                unsafe { convert_valid_utf16_to_utf8(code_units, output) };
            }
        }
    }

    pub(crate) fn into_utf16_string(self) -> Utf16String {
        self.0
    }
}

/// A JS value converted to a DOMString, https://webidl.spec.whatwg.org/#es-DOMString
pub(crate) fn to_dom_string(vm: Vm, value: Value) -> ThrowCompletionOr<Utf16String> {
    to_utf16_string(vm, value)
}

/// A JS value converted to a USVString, https://webidl.spec.whatwg.org/#es-USVString
pub(crate) fn to_usv_string(vm: Vm, value: Value) -> ThrowCompletionOr<UsvString> {
    // 1. Let string be the result of converting V to a DOMString.
    let string = to_dom_string(vm, value)?;

    // 2. Return an IDL USVString value that is the result of converting string to a sequence of scalar values.
    let well_formed = match string.as_units() {
        Utf16StringUnits::Ascii(_) => None,
        Utf16StringUnits::Utf16(units) => (!validate_utf16(units)).then(|| Utf16String::well_formed_from_utf16(units)),
    };
    Ok(UsvString(well_formed.unwrap_or(string)))
}

/// What the extended attributes of a buffer source type allow, https://webidl.spec.whatwg.org/#es-buffer-source-types
pub(crate) struct BufferSourceRules {
    pub(crate) allow_shared: bool,
    pub(crate) allow_resizable: bool,
}

/// A JS value converted to an IDL typed array value of the kind, one of the JS_LAYOUT_TYPED_ARRAY_KIND_ constants,
/// whose name is the type's name.
pub(crate) fn to_typed_array(
    vm: Vm,
    value: Value,
    kind: usize,
    name: &str,
    rules: &BufferSourceRules,
) -> ThrowCompletionOr<TypedArray> {
    // 1. Let T be the IDL type V is being converted to.
    // 2. If V is not an Object, or V does not have a [[TypedArrayName]] internal slot with a value equal to T's name,
    //    then throw a TypeError.
    let Some(typed_array) = value.as_object().and_then(|object| TypedArray::of_kind(object, kind)) else {
        return Err(throw_type_error(vm, &not_an_object_of_type(name)));
    };
    let buffer = typed_array.viewed_array_buffer();

    // 3. If the conversion is not to an IDL type associated with the [AllowShared] extended attribute, and
    //    IsSharedArrayBuffer(V.[[ViewedArrayBuffer]]) is true, then throw a TypeError.
    if !rules.allow_shared && buffer.is_shared() {
        return Err(throw_type_error(vm, SHARED_ARRAY_BUFFER));
    }

    // 4. If the conversion is not to an IDL type associated with the [AllowResizable] extended attribute, and
    //    IsFixedLengthArrayBuffer(V.[[ViewedArrayBuffer]]) is false, then throw a TypeError.
    if !rules.allow_resizable && !buffer.is_fixed_length() {
        return Err(throw_type_error(
            vm,
            &not_an_object_of_type(&format!("fixed-length {name}")),
        ));
    }

    // 5. Return the IDL value of type T that is a reference to the same object as V.
    Ok(typed_array)
}

/// An IDL value converted to a JS value, https://webidl.spec.whatwg.org/#js-type-mapping
pub(crate) trait ToJs {
    fn to_js(self, cx: &CallContext) -> ThrowCompletionOr<Value>;
}

impl ToJs for () {
    fn to_js(self, _: &CallContext) -> ThrowCompletionOr<Value> {
        Ok(Value::UNDEFINED)
    }
}

impl ToJs for bool {
    fn to_js(self, _: &CallContext) -> ThrowCompletionOr<Value> {
        Ok(Value::from_bool(self))
    }
}

/// https://webidl.spec.whatwg.org/#js-unsigned-long-long
impl ToJs for u64 {
    fn to_js(self, _: &CallContext) -> ThrowCompletionOr<Value> {
        #[allow(clippy::cast_precision_loss, reason = "the spec converts to the closest double")]
        Ok(Value::from_f64(self as f64))
    }
}

impl ToJs for Utf16String {
    fn to_js(self, cx: &CallContext) -> ThrowCompletionOr<Value> {
        Ok(from_utf16_string(cx.vm(), self))
    }
}

impl ToJs for UsvString {
    fn to_js(self, cx: &CallContext) -> ThrowCompletionOr<Value> {
        self.into_utf16_string().to_js(cx)
    }
}

impl ToJs for TypedArray {
    fn to_js(self, _: &CallContext) -> ThrowCompletionOr<Value> {
        Ok(self.object().value())
    }
}
