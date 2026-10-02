/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use ak::Utf16String;

use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::get_options_object;
use crate::runtime::array_buffer::{ArrayBuffer, Order, Shared};
use crate::runtime::completion::{Must, Throw, ThrowCompletionOr};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::native_function::raw_native;
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::realm::Realm;
use crate::runtime::typed_array::{
    Kind, TypedArrayBase, Uint8Array, is_typed_array_out_of_bounds, make_typed_array_with_buffer_witness_record,
    typed_array_length,
};
use crate::runtime::value_conversions::MAX_ARRAY_LIKE_INDEX;
use crate::utf16::{Utf16StringBuilder, Utf16View};

fn append_lowercase_hex_byte(builder: &mut Utf16StringBuilder, byte: u8) {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    builder.append_code_unit(u16::from(HEX_DIGITS[usize::from(byte >> 4)]));
    builder.append_code_unit(u16::from(HEX_DIGITS[usize::from(byte & 0xf)]));
}

/// The Uint8Array constructor's functions, which C++ defines with Uint8ArrayConstructorHelpers.
pub struct Uint8ArrayConstructorHelpers;

/// The functions of %Uint8Array.prototype%, which C++ defines with Uint8ArrayPrototypeHelpers.
pub struct Uint8ArrayPrototypeHelpers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alphabet {
    Base64,
    Base64URL,
}

/// AK::LastChunkHandling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastChunkHandling {
    Loose,
    Strict,
    StopBeforePartial,
}

/// AK::OmitPadding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmitPadding {
    No,
    Yes,
}

/// AK::Base64DecodeError, which picks the message of the SyntaxError a failed decode throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base64DecodeError {
    ExtraBits,
    InputRemainder,
    InvalidCharacter,
}

fn base64_decode_error_message(error: Base64DecodeError) -> &'static str {
    match error {
        Base64DecodeError::ExtraBits => "Extra bits found at end of chunk",
        Base64DecodeError::InputRemainder => "Invalid trailing data",
        Base64DecodeError::InvalidCharacter => "Invalid base64 character",
    }
}

pub struct DecodeResult {
    pub read: usize,          // [[Read]]
    pub bytes: Vec<u8>,       // [[Bytes]]
    pub error: Option<Throw>, // [[Error]]
}

impl Uint8ArrayConstructorHelpers {
    pub fn initialize(vm: &Vm, realm: Gc<Realm>, constructor: &Object) {
        let attr = PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE);
        constructor.define_native_function(
            vm,
            realm,
            &vm.names.fromBase64,
            raw_native!(Uint8ArrayConstructorHelpers::from_base64),
            1,
            attr,
            None,
        );
        constructor.define_native_function(
            vm,
            realm,
            &vm.names.fromHex,
            raw_native!(Uint8ArrayConstructorHelpers::from_hex),
            1,
            attr,
            None,
        );
    }

    // 23.3.1.1 Uint8Array.fromBase64 ( string [ , options ] ), https://tc39.es/ecma262/#sec-uint8array.frombase64
    fn from_base64(vm: &Vm) -> ThrowCompletionOr<Value> {
        let realm = vm.current_realm().expect("a builtin runs in a realm");

        let string_value = vm.argument(0);
        let options_value = vm.argument(1);

        // 1. If string is not a String, throw a TypeError exception.
        if !string_value.is_string() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAString, &[&string_value]);
        }

        // OPTIMIZATION: Avoid allocating an empty options object if none was provided.
        let mut alphabet = Alphabet::Base64;
        let mut last_chunk_handling = LastChunkHandling::Loose;
        if !options_value.is_undefined() {
            // 2. Let opts be ? GetOptionsObject(options).
            let options = get_options_object(vm, options_value)?;

            // 3. Let alphabet be ? Get(opts, "alphabet").
            // 4. If alphabet is undefined, set alphabet to "base64".
            // 5. If alphabet is neither "base64" nor "base64url", throw a TypeError exception.
            alphabet = parse_alphabet(vm, &options)?;

            // 6. Let lastChunkHandling be ? Get(opts, "lastChunkHandling").
            // 7. If lastChunkHandling is undefined, set lastChunkHandling to "loose".
            // 8. If lastChunkHandling is not one of "loose", "strict", or "stop-before-partial", throw a TypeError exception.
            last_chunk_handling = parse_last_chunk_handling(vm, &options)?;
        }

        // 9. Let result be FromBase64(string, alphabet, lastChunkHandling).
        let string = string_value.as_string().utf16_string();
        let result = from_base64(vm, Utf16View::of_string(&string), alphabet, last_chunk_handling, None);

        // 10. If result.[[Error]] is not NONE, then
        if let Some(error) = result.error {
            // a. Return ThrowCompletion(result.[[Error]]).
            return Err(error);
        }

        // 11. Let resultLength be the number of elements in result.[[Bytes]].
        let result_length = result.bytes.len();

        // 12. Let ta be ? AllocateTypedArray("Uint8Array", %Uint8Array%, "%Uint8Array.prototype%", resultLength).
        // 14. Set the value at each index of ta.[[ViewedArrayBuffer]].[[ArrayBufferData]] to the value at the corresponding
        //     index of result.[[Bytes]].
        let array_buffer = ArrayBuffer::create_from_bytes(vm, realm, &result.bytes, Shared::No);
        let typed_array = Uint8Array::create_on_buffer(vm, realm, result_length as u32, array_buffer);

        // 13. Assert: ta.[[ViewedArrayBuffer]].[[ArrayBufferByteLength]] is the number of elements in result.[[Bytes]].
        assert!(typed_array.viewed_array_buffer().byte_length() == result_length);

        // 15. Return ta.
        Ok(Value::from_object(typed_array))
    }

    // 23.3.1.2 Uint8Array.fromHex ( string ), https://tc39.es/ecma262/#sec-uint8array.fromhex
    fn from_hex(vm: &Vm) -> ThrowCompletionOr<Value> {
        let realm = vm.current_realm().expect("a builtin runs in a realm");

        let string_value = vm.argument(0);

        // 1. If string is not a String, throw a TypeError exception.
        if !string_value.is_string() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAString, &[&string_value]);
        }

        // 2. Let result be FromHex(string).
        let string = string_value.as_string().utf16_string();
        let result = from_hex(vm, Utf16View::of_string(&string), None);

        // 3. If result.[[Error]] is not NONE, then
        if let Some(error) = result.error {
            // a. Return ThrowCompletion(result.[[Error]]).
            return Err(error);
        }

        // 4. Let resultLength be the number of elements in result.[[Bytes]].
        let result_length = result.bytes.len();

        // 5. Let ta be ? AllocateTypedArray("Uint8Array", %Uint8Array%, "%Uint8Array.prototype%", resultLength).
        let typed_array = Uint8Array::create(vm, realm, result_length as u32)?;

        // 6. Assert: ta.[[ViewedArrayBuffer]].[[ArrayBufferByteLength]] is the number of elements in result.[[Bytes]].
        assert!(typed_array.viewed_array_buffer().byte_length() == result_length);

        // 7. Set the value at each index of ta.[[ViewedArrayBuffer]].[[ArrayBufferData]] to the value at the corresponding
        //    index of result.[[Bytes]].
        typed_array.viewed_array_buffer().overwrite(0, &result.bytes);

        // 8. Return ta.
        Ok(Value::from_object(typed_array))
    }
}

fn parse_alphabet(vm: &Vm, options: &Object) -> ThrowCompletionOr<Alphabet> {
    // Let alphabet be ? Get(opts, "alphabet").
    let alphabet = options.get(vm, &vm.names.alphabet)?;

    // If alphabet is undefined, set alphabet to "base64".
    if alphabet.is_undefined() {
        return Ok(Alphabet::Base64);
    }

    // If alphabet is neither "base64" nor "base64url", throw a TypeError exception.
    if alphabet.is_string() {
        let string = alphabet.as_string().utf16_string();
        if Utf16View::of_string(&string) == "base64" {
            return Ok(Alphabet::Base64);
        }
        if Utf16View::of_string(&string) == "base64url" {
            return Ok(Alphabet::Base64URL);
        }
    }

    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::OptionIsNotValidValue,
        &[&alphabet, &"alphabet"],
    )
}

fn parse_last_chunk_handling(vm: &Vm, options: &Object) -> ThrowCompletionOr<LastChunkHandling> {
    // Let lastChunkHandling be ? Get(opts, "lastChunkHandling").
    let last_chunk_handling = options.get(vm, &vm.names.lastChunkHandling)?;

    // If lastChunkHandling is undefined, set lastChunkHandling to "loose".
    if last_chunk_handling.is_undefined() {
        return Ok(LastChunkHandling::Loose);
    }

    // If lastChunkHandling is not one of "loose", "strict", or "stop-before-partial", throw a TypeError exception.
    if last_chunk_handling.is_string() {
        let string = last_chunk_handling.as_string().utf16_string();
        let view = Utf16View::of_string(&string);
        if view == "loose" {
            return Ok(LastChunkHandling::Loose);
        }
        if view == "strict" {
            return Ok(LastChunkHandling::Strict);
        }
        if view == "stop-before-partial" {
            return Ok(LastChunkHandling::StopBeforePartial);
        }
    }

    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::OptionIsNotValidValue,
        &[&last_chunk_handling, &"lastChunkHandling"],
    )
}

impl Uint8ArrayPrototypeHelpers {
    pub fn initialize(vm: &Vm, realm: Gc<Realm>, prototype: &Object) {
        let attr = PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE);
        prototype.define_native_function(
            vm,
            realm,
            &vm.names.setFromBase64,
            raw_native!(Uint8ArrayPrototypeHelpers::set_from_base64),
            1,
            attr,
            None,
        );
        prototype.define_native_function(
            vm,
            realm,
            &vm.names.setFromHex,
            raw_native!(Uint8ArrayPrototypeHelpers::set_from_hex),
            1,
            attr,
            None,
        );
        prototype.define_native_function(
            vm,
            realm,
            &vm.names.toBase64,
            raw_native!(Uint8ArrayPrototypeHelpers::to_base64),
            0,
            attr,
            None,
        );
        prototype.define_native_function(
            vm,
            realm,
            &vm.names.toHex,
            raw_native!(Uint8ArrayPrototypeHelpers::to_hex),
            0,
            attr,
            None,
        );
    }

    // 23.3.2.1 Uint8Array.prototype.setFromBase64 ( string [ , options ] ), https://tc39.es/ecma262/#sec-uint8array.prototype.setfrombase64
    fn set_from_base64(vm: &Vm) -> ThrowCompletionOr<Value> {
        let realm = vm.current_realm().expect("a builtin runs in a realm");

        let string_value = vm.argument(0);
        let options_value = vm.argument(1);

        // 1. Let into be the this value.
        // 2. Perform ? ValidateUint8Array(into).
        let into = validate_uint8_array(vm)?;

        // 3. If string is not a String, throw a TypeError exception.
        if !string_value.is_string() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAString, &[&string_value]);
        }

        // 4. Let opts be ? GetOptionsObject(options).
        let options = get_options_object(vm, options_value)?;

        // 5. Let alphabet be ? Get(opts, "alphabet").
        // 6. If alphabet is undefined, set alphabet to "base64".
        // 7. If alphabet is neither "base64" nor "base64url", throw a TypeError exception.
        let alphabet = parse_alphabet(vm, &options)?;

        // 8. Let lastChunkHandling be ? Get(opts, "lastChunkHandling").
        // 9. If lastChunkHandling is undefined, set lastChunkHandling to "loose".
        // 10. If lastChunkHandling is not one of "loose", "strict", or "stop-before-partial", throw a TypeError exception.
        let last_chunk_handling = parse_last_chunk_handling(vm, &options)?;

        // 11. Let taRecord be MakeTypedArrayWithBufferWitnessRecord(into, SEQ-CST).
        let typed_array_record = make_typed_array_with_buffer_witness_record(&into, Order::SeqCst);

        // 12. If IsTypedArrayOutOfBounds(taRecord) is true, throw a TypeError exception.
        if is_typed_array_out_of_bounds(&typed_array_record) {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::BufferOutOfBounds, &[&"TypedArray"]);
        }

        // 13. Let byteLength be TypedArrayLength(taRecord).
        let byte_length = typed_array_length(&typed_array_record) as usize;

        // 14. Let result be FromBase64(string, alphabet, lastChunkHandling, byteLength).
        let string = string_value.as_string().utf16_string();
        let result = from_base64(
            vm,
            Utf16View::of_string(&string),
            alphabet,
            last_chunk_handling,
            Some(byte_length),
        );

        // 15. Let bytes be result.[[Bytes]].
        let bytes = result.bytes;

        // 16. Let written be the number of elements in bytes.
        let written = bytes.len();

        // 17. NOTE: FromBase64 does not invoke any user code, so the ArrayBuffer backing into cannot have been detached or shrunk.
        // 18. Assert: written ≤ byteLength.
        assert!(written <= byte_length);

        // 19. Perform SetUint8ArrayBytes(into, bytes).
        set_uint8_array_bytes(vm, &into, &bytes);

        // 20. If result.[[Error]] is not NONE, then
        if let Some(error) = result.error {
            // a. Return ThrowCompletion(result.[[Error]]).
            return Err(error);
        }

        // 21. Let resultObject be OrdinaryObjectCreate(%Object.prototype%).
        let result_object = Object::create(vm, realm, Some(realm.object_prototype()));

        // 22. Perform ! CreateDataPropertyOrThrow(resultObject, "read", 𝔽(result.[[Read]])).
        result_object
            .create_data_property(vm, &vm.names.read, Value::from_f64(result.read as f64), None, None)
            .must();

        // 23. Perform ! CreateDataPropertyOrThrow(resultObject, "written", 𝔽(written)).
        result_object
            .create_data_property(vm, &vm.names.written, Value::from_f64(written as f64), None, None)
            .must();

        // 24. Return resultObject.
        Ok(Value::from_object(result_object))
    }

    // 23.3.2.2 Uint8Array.prototype.setFromHex ( string ), https://tc39.es/ecma262/#sec-uint8array.prototype.setfromhex
    fn set_from_hex(vm: &Vm) -> ThrowCompletionOr<Value> {
        let realm = vm.current_realm().expect("a builtin runs in a realm");

        let string_value = vm.argument(0);

        // 1. Let into be the this value.
        // 2. Perform ? ValidateUint8Array(into).
        let into = validate_uint8_array(vm)?;

        // 3. If string is not a String, throw a TypeError exception.
        if !string_value.is_string() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAString, &[&string_value]);
        }

        // 4. Let taRecord be MakeTypedArrayWithBufferWitnessRecord(into, SEQ-CST).
        let typed_array_record = make_typed_array_with_buffer_witness_record(&into, Order::SeqCst);

        // 5. If IsTypedArrayOutOfBounds(taRecord) is true, throw a TypeError exception.
        if is_typed_array_out_of_bounds(&typed_array_record) {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::BufferOutOfBounds, &[&"TypedArray"]);
        }

        // 6. Let byteLength be TypedArrayLength(taRecord).
        let byte_length = typed_array_length(&typed_array_record) as usize;

        // 7. Let result be FromHex(string, byteLength).
        let string = string_value.as_string().utf16_string();
        let result = from_hex(vm, Utf16View::of_string(&string), Some(byte_length));

        // 8. Let bytes be result.[[Bytes]].
        let bytes = result.bytes;

        // 9. Let written be the number of elements in bytes.
        let written = bytes.len();

        // 10. NOTE: FromHex does not invoke any user code, so the ArrayBuffer backing into cannot have been detached or shrunk.
        // 11. Assert: written ≤ byteLength.
        assert!(written <= byte_length);

        // 12. Perform SetUint8ArrayBytes(into, bytes).
        set_uint8_array_bytes(vm, &into, &bytes);

        // 13. If result.[[Error]] is not NONE, then
        if let Some(error) = result.error {
            // a. Return ThrowCompletion(result.[[Error]]).
            return Err(error);
        }

        // 14. Let resultObject be OrdinaryObjectCreate(%Object.prototype%).
        let result_object = Object::create(vm, realm, Some(realm.object_prototype()));

        // 15. Perform ! CreateDataPropertyOrThrow(resultObject, "read", 𝔽(result.[[Read]])).
        result_object
            .create_data_property(vm, &vm.names.read, Value::from_f64(result.read as f64), None, None)
            .must();

        // 16. Perform ! CreateDataPropertyOrThrow(resultObject, "written", 𝔽(written)).
        result_object
            .create_data_property(vm, &vm.names.written, Value::from_f64(written as f64), None, None)
            .must();

        // 17. Return resultObject.
        Ok(Value::from_object(result_object))
    }

    // 23.3.2.3 Uint8Array.prototype.toBase64 ( [ options ] ), https://tc39.es/ecma262/#sec-uint8array.prototype.tobase64
    fn to_base64(vm: &Vm) -> ThrowCompletionOr<Value> {
        let options_value = vm.argument(0);

        // 1. Let O be the this value.
        // 2. Perform ? ValidateUint8Array(O).
        let typed_array = validate_uint8_array(vm)?;

        // OPTIMIZATION: Avoid allocating an empty options object if none was provided.
        let mut alphabet = Alphabet::Base64;
        let mut omit_padding = OmitPadding::No;
        if !options_value.is_undefined() {
            // 3. Let opts be ? GetOptionsObject(options).
            let options = get_options_object(vm, options_value)?;

            // 4. Let alphabet be ? Get(opts, "alphabet").
            // 5. If alphabet is undefined, set alphabet to "base64".
            // 6. If alphabet is neither "base64" nor "base64url", throw a TypeError exception.
            alphabet = parse_alphabet(vm, &options)?;

            // 7. Let omitPadding be ToBoolean(? Get(opts, "omitPadding")).
            let omit_padding_value = options.get(vm, &vm.names.omitPadding)?.to_boolean();
            omit_padding = if omit_padding_value {
                OmitPadding::Yes
            } else {
                OmitPadding::No
            };
        }

        // 8. Let toEncode be ? GetUint8ArrayBytes(O).
        let typed_array_record = make_typed_array_with_buffer_witness_record(&typed_array, Order::SeqCst);
        if is_typed_array_out_of_bounds(&typed_array_record) {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::BufferOutOfBounds, &[&"TypedArray"]);
        }

        let length = typed_array_length(&typed_array_record) as usize;
        let byte_offset = typed_array.byte_offset() as usize;

        let to_encode = typed_array
            .viewed_array_buffer()
            .copy_to_byte_buffer(byte_offset, length);

        // 9. If alphabet is "base64", then
        //     a. Let outAscii be the sequence of code points which results from encoding toEncode according to the base64
        //        encoding specified in section 4 of RFC 4648. Padding is included if and only if omitPadding is false.
        // 10. Else,
        //     a. Assert: alphabet is "base64url".
        //     b. Let outAscii be the sequence of code points which results from encoding toEncode according to the base64url
        //        encoding specified in section 5 of RFC 4648. Padding is included if and only if omitPadding is false.
        let out_ascii = encode_base64(&to_encode, alphabet, omit_padding);

        // 11. Return CodePointsToString(outAscii).
        Ok(Value::from_string(PrimitiveString::create(
            vm,
            Utf16String::from_utf8(&out_ascii),
        )))
    }

    // 23.3.2.4 Uint8Array.prototype.toHex ( ), https://tc39.es/ecma262/#sec-uint8array.prototype.tohex
    fn to_hex(vm: &Vm) -> ThrowCompletionOr<Value> {
        // 1. Let O be the this value.
        // 2. Perform ? ValidateUint8Array(O).
        let typed_array = validate_uint8_array(vm)?;

        // 3. Let toEncode be ? GetUint8ArrayBytes(O).
        let typed_array_record = make_typed_array_with_buffer_witness_record(&typed_array, Order::SeqCst);
        if is_typed_array_out_of_bounds(&typed_array_record) {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::BufferOutOfBounds, &[&"TypedArray"]);
        }

        let length = typed_array_length(&typed_array_record) as usize;
        let byte_offset = typed_array.byte_offset() as usize;

        // 4. Let out be the empty String.
        let mut out = Utf16StringBuilder::with_capacity(length * 2);

        let to_encode = typed_array
            .viewed_array_buffer()
            .copy_to_byte_buffer(byte_offset, length);

        // 5. For each byte byte of toEncode, do
        for byte in to_encode {
            // a. Let hex be Number::toString(𝔽(byte), 16).
            // b. Set hex to StringPad(hex, 2, "0", START).
            // c. Set out to the string-concatenation of out and hex.
            append_lowercase_hex_byte(&mut out, byte);
        }

        // 6. Return out.
        Ok(Value::from_string(PrimitiveString::create(vm, out.to_utf16_string())))
    }
}

// 23.3.3.1 ValidateUint8Array ( ta ), https://tc39.es/ecma262/#sec-validateuint8array
pub fn validate_uint8_array(vm: &Vm) -> ThrowCompletionOr<Gc<TypedArrayBase>> {
    let this_object = vm.this_value().to_object(vm)?;

    // 1. Perform ? RequireInternalSlot(ta, [[TypedArrayName]]).
    if !this_object.is_typed_array() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObjectOfType, &[&"Uint8Array"]);
    }

    let typed_array = this_object
        .downcast::<TypedArrayBase>()
        .expect("an object with the typed array flag is a typed array");

    // 2. If ta.[[TypedArrayName]] is not "Uint8Array", throw a TypeError exception.
    if typed_array.kind() != Kind::Uint8Array {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObjectOfType, &[&"Uint8Array"]);
    }

    // 3. Return UNUSED.
    Ok(typed_array)
}

// 23.3.3.2 GetUint8ArrayBytes ( ta ), https://tc39.es/ecma262/#sec-getuint8arraybytes

// 23.3.3.3 SetUint8ArrayBytes ( into, bytes ), https://tc39.es/ecma262/#sec-setuint8arraybytes
pub fn set_uint8_array_bytes(vm: &Vm, into: &TypedArrayBase, bytes: &[u8]) {
    // 1. Let offset be into.[[ByteOffset]].
    let offset = into.byte_offset();

    // 2. Let len be the number of elements in bytes.
    // 3. Let index be 0.
    // 4. Repeat, while index < len,
    for (index, byte) in bytes.iter().enumerate() {
        // a. Let byte be bytes[index].
        // b. Let byteIndexInBuffer be index + offset.
        let byte_index_in_buffer = index as u32 + offset;

        // c. Perform SetValueInBuffer(into.[[ViewedArrayBuffer]], byteIndexInBuffer, uint8, 𝔽(byte), true, unordered).
        into.set_value_in_buffer(
            vm,
            byte_index_in_buffer as usize,
            Value::from_i32(i32::from(*byte)),
            Order::Unordered,
        );

        // d. Set index to index + 1.
    }

    // 5. Return UNUSED.
}

const BASE64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// AK::encode_base64() and encode_base64url().
fn encode_base64(input: &[u8], alphabet: Alphabet, omit_padding: OmitPadding) -> String {
    let characters = match alphabet {
        Alphabet::Base64 => BASE64_ALPHABET,
        Alphabet::Base64URL => BASE64URL_ALPHABET,
    };
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let sextets = [
            first >> 2,
            ((first & 0x03) << 4) | (second >> 4),
            ((second & 0x0f) << 2) | (third >> 6),
            third & 0x3f,
        ];
        let encoded_count = chunk.len() + 1;
        for sextet in &sextets[..encoded_count] {
            output.push(char::from(characters[usize::from(*sextet)]));
        }
        if omit_padding == OmitPadding::No {
            for _ in encoded_count..4 {
                output.push('=');
            }
        }
    }
    output
}

fn base64_value(code_unit: u16) -> Option<u8> {
    let character = u8::try_from(code_unit).ok()?;
    BASE64_ALPHABET
        .iter()
        .position(|candidate| *candidate == character)
        .map(|position| position as u8)
}

// SkipAsciiWhitespace ( string, index ), https://tc39.es/ecma262/#sec-skipasciiwhitespace
fn skip_ascii_whitespace(string: Utf16View<'_>, mut index: usize) -> usize {
    // 1. Let length be the length of string.
    let length = string.length_in_code_units();

    // 2. Repeat, while index < length,
    while index < length {
        // a. Let char be the code unit at index index of string.
        let character = string.code_unit_at(index);

        // b. If char is neither 0x0009 (TAB), 0x000A (LF), 0x000C (FF), 0x000D (CR), nor 0x0020 (SPACE), then
        if !matches!(character, 0x09 | 0x0a | 0x0c | 0x0d | 0x20) {
            // i. Return index.
            return index;
        }

        // c. Set index to index + 1.
        index += 1;
    }

    // 3. Return index.
    index
}

// DecodeBase64Chunk ( chunk ), https://tc39.es/ecma262/#sec-decodebase64chunk
fn decode_base64_chunk(chunk: &[u8; 4]) -> [u8; 3] {
    let value =
        (u32::from(chunk[0]) << 18) | (u32::from(chunk[1]) << 12) | (u32::from(chunk[2]) << 6) | u32::from(chunk[3]);
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

// DecodeFinalBase64Chunk ( chunk, throwOnExtraBits ), https://tc39.es/ecma262/#sec-decodefinalbase64chunk
fn decode_final_base64_chunk(chunk: &[u8], throw_on_extra_bits: bool) -> Result<Vec<u8>, Base64DecodeError> {
    let mut padded = [0u8; 4];
    padded[..chunk.len()].copy_from_slice(chunk);
    let byte_sequence = decode_base64_chunk(&padded);
    if chunk.len() == 2 {
        if throw_on_extra_bits && byte_sequence[1] != 0 {
            return Err(Base64DecodeError::ExtraBits);
        }
        return Ok(vec![byte_sequence[0]]);
    }
    assert!(chunk.len() == 3);
    if throw_on_extra_bits && byte_sequence[2] != 0 {
        return Err(Base64DecodeError::ExtraBits);
    }
    Ok(vec![byte_sequence[0], byte_sequence[1]])
}

/// FromBase64 without the error objects: the bytes read and decoded, and the error that stopped the decoding. The C++
/// runtime decodes with simdutf, whose error codes pick the messages.
fn decode_base64(
    string: Utf16View<'_>,
    alphabet: Alphabet,
    last_chunk_handling: LastChunkHandling,
    max_length: usize,
) -> (usize, Vec<u8>, Option<Base64DecodeError>) {
    let mut read = 0;
    let mut bytes = Vec::new();
    if max_length == 0 {
        return (0, bytes, None);
    }

    let mut chunk: Vec<u8> = Vec::with_capacity(4);
    let mut index = 0;
    let length = string.length_in_code_units();
    loop {
        index = skip_ascii_whitespace(string, index);
        if index == length {
            if !chunk.is_empty() {
                match last_chunk_handling {
                    LastChunkHandling::StopBeforePartial => return (read, bytes, None),
                    LastChunkHandling::Loose => {
                        if chunk.len() == 1 {
                            return (read, bytes, Some(Base64DecodeError::InputRemainder));
                        }
                        bytes.extend(decode_final_base64_chunk(&chunk, false).expect("a loose chunk decodes"));
                    }
                    LastChunkHandling::Strict => return (read, bytes, Some(Base64DecodeError::InputRemainder)),
                }
            }
            return (length, bytes, None);
        }

        let mut character = string.code_unit_at(index);
        index += 1;

        if character == u16::from(b'=') {
            if chunk.len() < 2 {
                let error = if chunk.is_empty() {
                    Base64DecodeError::InvalidCharacter
                } else {
                    Base64DecodeError::InputRemainder
                };
                return (read, bytes, Some(error));
            }
            index = skip_ascii_whitespace(string, index);
            if chunk.len() == 2 {
                if index == length {
                    return match last_chunk_handling {
                        LastChunkHandling::StopBeforePartial => (read, bytes, None),
                        LastChunkHandling::Strict => (read, bytes, Some(Base64DecodeError::InputRemainder)),
                        LastChunkHandling::Loose => (read, bytes, Some(Base64DecodeError::InvalidCharacter)),
                    };
                }
                if string.code_unit_at(index) == u16::from(b'=') {
                    index = skip_ascii_whitespace(string, index + 1);
                }
            }
            if index < length {
                return (read, bytes, Some(Base64DecodeError::InvalidCharacter));
            }
            let throw_on_extra_bits = last_chunk_handling == LastChunkHandling::Strict;
            match decode_final_base64_chunk(&chunk, throw_on_extra_bits) {
                Ok(decoded) => bytes.extend(decoded),
                Err(error) => return (read, bytes, Some(error)),
            }
            return (length, bytes, None);
        }

        if alphabet == Alphabet::Base64URL {
            if character == u16::from(b'+') || character == u16::from(b'/') {
                return (read, bytes, Some(Base64DecodeError::InvalidCharacter));
            } else if character == u16::from(b'-') {
                character = u16::from(b'+');
            } else if character == u16::from(b'_') {
                character = u16::from(b'/');
            }
        }

        let Some(value) = base64_value(character) else {
            return (read, bytes, Some(Base64DecodeError::InvalidCharacter));
        };

        let remaining = max_length - bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return (read, bytes, None);
        }

        chunk.push(value);
        if chunk.len() == 4 {
            let chunk_values: [u8; 4] = chunk[..].try_into().expect("the chunk has four values");
            bytes.extend(decode_base64_chunk(&chunk_values));
            chunk.clear();
            read = index;
            if bytes.len() == max_length {
                return (read, bytes, None);
            }
        }
    }
}

// 23.3.3.7 FromBase64 ( string, alphabet, lastChunkHandling [ , maxLength ] ), https://tc39.es/ecma262/#sec-frombase64
pub fn from_base64(
    vm: &Vm,
    string: Utf16View<'_>,
    alphabet: Alphabet,
    last_chunk_handling: LastChunkHandling,
    max_length: Option<usize>,
) -> DecodeResult {
    let (read, bytes, error) = decode_base64(string, alphabet, last_chunk_handling, max_length.unwrap_or(usize::MAX));

    let error = error.map(|error| {
        vm.throw_completion_with_message::<()>(ErrorKind::SyntaxError, base64_decode_error_message(error).to_string())
            .expect_err("throwing a completion throws")
    });
    DecodeResult { read, bytes, error }
}

// 23.3.3.8 FromHex ( string [ , maxLength ] ), https://tc39.es/ecma262/#sec-fromhex
pub fn from_hex(vm: &Vm, string: Utf16View<'_>, max_length: Option<usize>) -> DecodeResult {
    // 1. If maxLength is not present, set maxLength to 2**53 - 1.
    let max_length = max_length.unwrap_or(MAX_ARRAY_LIKE_INDEX as usize);

    // 2. Let length be the length of string.
    let length = string.length_in_code_units();

    // 3. Let bytes be a new empty List.
    let mut bytes = Vec::new();

    // 4. Let read be 0.
    let mut read = 0;

    // 5. If length modulo 2 ≠ 0, then
    if !length.is_multiple_of(2) {
        // a. Let error be a newly created SyntaxError object.
        let error = vm
            .throw_completion_with_message::<()>(
                ErrorKind::SyntaxError,
                "Hex string must have an even length".to_string(),
            )
            .expect_err("throwing a completion throws");

        // b. Return the Record { [[Read]]: read, [[Bytes]]: bytes, [[Error]]: error }.
        return DecodeResult {
            read,
            bytes,
            error: Some(error),
        };
    }

    // 6. Repeat, while read < length and the number of elements in bytes < maxLength,
    while read < length && bytes.len() < max_length {
        // a. Let hexits be the substring of string from read to read + 2.
        // d. Let byte be the integer value represented by hexits in base-16 notation, using the letters A through F and
        //    a through f for digits with values 10 through 15.
        // NOTE: We do this early so that we don't have to effectively parse hexits twice.
        let hexit = |index: usize| char::from_u32(u32::from(string.code_unit_at(index)))?.to_digit(16);
        let byte = hexit(read)
            .zip(hexit(read + 1))
            .map(|(high, low)| (high * 16 + low) as u8);

        // b. If hexits contains any code units which are not in "0123456789abcdefABCDEF", then
        let Some(byte) = byte else {
            // i. Let error be a newly created SyntaxError object.
            let error = vm
                .throw_completion_with_message::<()>(
                    ErrorKind::SyntaxError,
                    "Hex string must only contain hex characters".to_string(),
                )
                .expect_err("throwing a completion throws");

            // ii. Return the Record { [[Read]]: read, [[Bytes]]: bytes, [[Error]]: error }.
            return DecodeResult {
                read,
                bytes,
                error: Some(error),
            };
        };

        // c. Set read to read + 2.
        read += 2;

        // e. Append byte to bytes.
        bytes.push(byte);
    }

    // 7. Return the Record { [[Read]]: read, [[Bytes]]: bytes, [[Error]]: none }.
    DecodeResult {
        read,
        bytes,
        error: None,
    }
}
