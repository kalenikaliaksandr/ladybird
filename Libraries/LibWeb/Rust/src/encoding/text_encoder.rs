/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! TextEncoder, https://encoding.spec.whatwg.org/#interface-textencoder

use libjs_embedding::gc::Trace;

/// A TextEncoder has no state: it always encodes UTF-8.
#[derive(Trace)]
#[gc(crate = libjs_embedding)]
pub(crate) struct TextEncoder;

#[cfg(web_idl_bindings)]
mod implementation {
    use ak::{Utf16String, Utf16StringUnits};
    use libjs_embedding::abi::JS_ARRAY_BUFFER_ORDER_SEQ_CST;
    use libjs_embedding::array_buffer::ArrayBuffer;
    use libjs_embedding::layout::JS_LAYOUT_TYPED_ARRAY_KIND_UINT8;
    use libjs_embedding::typed_array::TypedArray;

    use super::TextEncoder;
    use crate::bindings::conversions::UsvString;
    use crate::bindings::generated::text_encoder::{TextEncoderEncodeIntoResult, TextEncoderInterface};
    use crate::bindings::messages::invalid_length;
    use crate::bindings::{CallContext, Exception, WebIdlResult};
    use crate::encoding::utf8::{self, EncodeIntoProgress};

    fn encode_into_result(progress: EncodeIntoProgress) -> TextEncoderEncodeIntoResult {
        TextEncoderEncodeIntoResult {
            read: Some(progress.read as u64),
            written: Some(progress.written as u64),
        }
    }

    impl TextEncoderInterface for TextEncoder {
        // https://encoding.spec.whatwg.org/#dom-textencoder
        fn construct(_: &mut CallContext) -> WebIdlResult<Self> {
            // The new TextEncoder() constructor steps are to do nothing.
            Ok(Self)
        }

        // https://encoding.spec.whatwg.org/#dom-textencoder-encoding
        fn encoding(&self, _: &mut CallContext) -> WebIdlResult<Utf16String> {
            // The encoding getter steps are to return "utf-8".
            Ok(Utf16String::from_ascii(b"utf-8"))
        }

        // https://encoding.spec.whatwg.org/#dom-textencoder-encode
        fn encode(&self, cx: &mut CallContext, input: UsvString) -> WebIdlResult<TypedArray> {
            // 1. Convert input to an I/O queue of scalar values.
            // 2. Let output be the I/O queue of bytes « end-of-queue ».
            // 3. While true:
            //     1. Let item be the result of reading from input.
            //     2. Let result be the result of processing an item with item, an instance of the UTF-8 encoder,
            //        input, output, and "fatal".
            //     3. Assert: result is not an error.
            //     4. If result is finished, then convert output into a byte sequence and return a Uint8Array object
            //        wrapping an ArrayBuffer containing output.
            let length = input.utf8_length();
            let array_length =
                u32::try_from(length).map_err(|_| Exception::RangeError(invalid_length("typed array")))?;
            let buffer = match input.units() {
                // ASCII storage already is the UTF-8 encoding.
                Utf16StringUnits::Ascii(bytes) => ArrayBuffer::create_from_bytes(cx.vm(), cx.realm(), bytes),
                Utf16StringUnits::Utf16(_) => {
                    let buffer = ArrayBuffer::create(cx.vm(), cx.realm(), length)?;
                    if length > 0 {
                        let (data, byte_length) = buffer.data();
                        assert!(!data.is_null() && byte_length == length);
                        // SAFETY: The new buffer has `length` bytes, which nothing else references yet.
                        input.encode_utf8(unsafe { core::slice::from_raw_parts_mut(data, length) });
                    }
                    buffer
                }
            };
            Ok(TypedArray::create_on_buffer(
                cx.vm(),
                cx.realm(),
                JS_LAYOUT_TYPED_ARRAY_KIND_UINT8,
                array_length,
                buffer,
            ))
        }

        // https://encoding.spec.whatwg.org/#dom-textencoder-encodeinto
        fn encode_into(
            &self,
            _: &mut CallContext,
            source: UsvString,
            destination: TypedArray,
        ) -> WebIdlResult<TextEncoderEncodeIntoResult> {
            // AD-HOC: Return early if destination is detached. This is not explicitly handled in the spec, however no
            //         bytes are copied as the destination's size is always zero in this case.
            //         See: https://github.com/whatwg/encoding/issues/324
            let buffer = destination.viewed_array_buffer();
            if buffer.is_detached() {
                return Ok(encode_into_result(EncodeIntoProgress { read: 0, written: 0 }));
            }
            let destination_record = destination.witness_record(JS_ARRAY_BUFFER_ORDER_SEQ_CST);
            if destination_record.is_out_of_bounds() {
                return Ok(encode_into_result(EncodeIntoProgress { read: 0, written: 0 }));
            }
            let destination_byte_length = destination_record.byte_length();
            if destination_byte_length == 0 {
                return Ok(encode_into_result(EncodeIntoProgress { read: 0, written: 0 }));
            }
            let (data, _) = buffer.data();
            assert!(!data.is_null());
            // SAFETY: The destination views `destination_byte_length` bytes of the buffer from its byte offset.
            let destination_bytes = unsafe { data.add(destination.byte_offset()) };

            // OPTIMIZATION: Encode the whole source at once into a destination that is not shared when it fits.
            //               Shared destinations stay on the incremental path, which keeps their writes the same.
            if !buffer.is_shared() {
                let length = source.utf8_length();
                if length <= destination_byte_length {
                    // SAFETY: The destination views at least `length` bytes of the buffer from its byte offset, which
                    //         nothing else accesses while this runs, as the buffer is not shared.
                    source.encode_utf8(unsafe { core::slice::from_raw_parts_mut(destination_bytes, length) });
                    return Ok(encode_into_result(EncodeIntoProgress {
                        read: source.length_in_code_units(),
                        written: length,
                    }));
                }
            }

            // 1. Let read be 0.
            // 2. Let written be 0.
            // 3. Let encoder be an instance of the UTF-8 encoder.
            // 4. Let unused be the I/O queue of scalar values « end-of-queue ».
            // 5. Convert source to an I/O queue of scalar values.
            // 6. While true:
            //     1. Let item be the result of reading from source.
            //     2. Let result be the result of running encoder's handler on unused and item.
            //     3. If result is finished, then break.
            //     4. Otherwise:
            //         1. If destination's byte length − written is greater than or equal to the number of bytes in
            //            result, then:
            //             1. If item is greater than U+FFFF, then increment read by 2.
            //             2. Otherwise, increment read by 1.
            //             3. Write the bytes in result into destination, with startingOffset set to written.
            //             4. Increment written by the number of bytes in result.
            //         2. Otherwise, break.
            // 7. Return «[ "read" → read, "written" → written ]».
            let units = source.units();
            let progress = if buffer.is_shared() {
                // WARNING: See the warning for SharedArrayBuffer objects at
                //          https://encoding.spec.whatwg.org/#sharedarraybuffer-warning. Another thread can access the
                //          bytes at any time, so they are only written through raw copies.
                utf8::encode_into(&units, destination_byte_length, |offset, bytes| {
                    // SAFETY: The encoder writes within the destination's byte length.
                    unsafe {
                        core::ptr::copy_nonoverlapping(bytes.as_ptr(), destination_bytes.add(offset), bytes.len());
                    }
                })
            } else {
                // SAFETY: Nothing else accesses the bytes of a buffer that is not shared while this runs.
                let output = unsafe { core::slice::from_raw_parts_mut(destination_bytes, destination_byte_length) };
                utf8::encode_into(&units, destination_byte_length, |offset, bytes| {
                    output[offset..offset + bytes.len()].copy_from_slice(bytes);
                })
            };
            Ok(encode_into_result(progress))
        }
    }
}
