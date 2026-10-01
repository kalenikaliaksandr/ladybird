/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The functions the Rust frontend (Libraries/LibJS/Rust) calls on the runtime that embeds it, which the C++ runtime
//! defines in Libraries/LibJS/RustIntegration.cpp: the hooks the parser calls, and the callbacks through which the
//! frontend's bytecode dumper asks for the names and constants of an executable.

use core::alloc::Layout;
use core::ffi::c_void;
use core::{ptr, slice};
use std::alloc::{alloc, dealloc, handle_alloc_error};

use libjs_rust::bytecode::dump::{
    FFIBytecodeDumpCallbacks, FFIBytecodeDumpMetadata, FFIDumpExceptionHandler, FFIInterpreterHandlerRange,
    count_basic_blocks,
};
use libregex_rust::ast::Flags;
use libregex_rust::regex::Regex;

use crate::bytecode::executable::{Executable, append_double_formatted_like_ak, append_value_formatted_like_ak};
use crate::interpreter::run::should_dump_interpreter_assembly;
use crate::layout::value::Value;
use crate::runtime::regexp_object::parse_regex_pattern;
use crate::runtime::value::number_to_string;
use crate::utf16::Utf16View;

/// What the frontend holds for each regular expression literal it compiled: the pattern as ParsePattern rewrote it.
pub struct CompiledRegex {
    pub parsed_pattern: Vec<u16>,
}

/// Compiles a regular expression literal, so that the frontend can report an invalid one as an early error.
///
/// Returns an owned CompiledRegex for rust_free_compiled_regex to release, or null with an error message in error_out
/// and error_len_out for rust_free_error_string to release.
///
/// # Safety
///
/// pattern_data and flags_data must point to pattern_len and flags_len UTF-16 code units (or may dangle when the
/// length is zero), and error_out and error_len_out must be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_compile_regex(
    pattern_data: *const u16,
    pattern_len: usize,
    flags_data: *const u16,
    flags_len: usize,
    error_out: *mut *const u16,
    error_len_out: *mut usize,
) -> *mut c_void {
    // SAFETY: The caller passes valid buffers of these lengths, and out-pointers valid for writes.
    unsafe {
        error_out.write(ptr::null());
        error_len_out.write(0);
        let (pattern, flags) = (code_units(pattern_data, pattern_len), code_units(flags_data, flags_len));
        match compile_regex(pattern, flags) {
            Ok(compiled) => Box::into_raw(Box::new(compiled)).cast(),
            Err(message) => {
                let (error, error_length) = allocate_error_string(&message);
                error_out.write(error);
                error_len_out.write(error_length);
                ptr::null_mut()
            }
        }
    }
}

/// # Safety
///
/// compiled_regex must be null or a pointer rust_compile_regex returned that has not been freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_free_compiled_regex(compiled_regex: *mut c_void) {
    if !compiled_regex.is_null() {
        // SAFETY: rust_compile_regex created this pointer with Box::into_raw.
        drop(unsafe { Box::from_raw(compiled_regex.cast::<CompiledRegex>()) });
    }
}

/// # Safety
///
/// message must be null or an error message rust_compile_regex stored that has not been freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_free_error_string(message: *const u16) {
    if message.is_null() {
        return;
    }
    // SAFETY: allocate_error_string placed the message right after its length, in an allocation of this layout.
    unsafe {
        let allocation = message.cast::<u8>().sub(ERROR_STRING_HEADER_SIZE).cast_mut();
        let length = allocation.cast::<usize>().read();
        dealloc(allocation, error_string_layout(length));
    }
}

/// Writes Number::toString(value) into buffer, truncated to buffer_len code units, and returns how many it wrote.
///
/// # Safety
///
/// buffer must be valid for writing buffer_len code units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_number_to_utf16(value: f64, buffer: *mut u16, buffer_len: usize) -> usize {
    let string = number_to_string(value);
    let length = string.len().min(buffer_len);
    for (index, byte) in string.bytes().take(length).enumerate() {
        // SAFETY: index is below buffer_len, for which the caller passes a writable buffer.
        unsafe { buffer.add(index).write(u16::from(byte)) };
    }
    length
}

/// # Safety
///
/// data must point to length code units, or may dangle when length is zero.
unsafe fn code_units<'a>(data: *const u16, length: usize) -> &'a [u16] {
    if length == 0 {
        return &[];
    }
    // SAFETY: The caller guarantees the buffer.
    unsafe { slice::from_raw_parts(data, length) }
}

fn compile_regex(pattern: &[u16], flags: &[u16]) -> Result<CompiledRegex, String> {
    let has_flag = |flag: u8| flags.contains(&u16::from(flag));

    let parsed_pattern = parse_regex_pattern(pattern, has_flag(b'u'), has_flag(b'v'))
        .map_err(|error| format!("RegExp compile error: {}", error.error))?;

    let compile_flags = Flags {
        global: has_flag(b'g'),
        ignore_case: has_flag(b'i'),
        multiline: has_flag(b'm'),
        dot_all: has_flag(b's'),
        unicode: has_flag(b'u'),
        unicode_sets: has_flag(b'v'),
        sticky: has_flag(b'y'),
        has_indices: has_flag(b'd'),
    };
    // Converted the way LibRegex's rust_regex_compile converts a UTF-16 pattern, although ParsePattern only produces
    // ASCII.
    let pattern_characters = char::decode_utf16(parsed_pattern.iter().copied())
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    Regex::compile_chars(pattern_characters, compile_flags)
        .map_err(|error| format!("RegExp compile error: {error}"))?;

    Ok(CompiledRegex { parsed_pattern })
}

/// Error messages carry their length in front of them, since rust_free_error_string only receives the message.
const ERROR_STRING_HEADER_SIZE: usize = size_of::<usize>();

fn error_string_layout(length: usize) -> Layout {
    Layout::from_size_align(
        ERROR_STRING_HEADER_SIZE + length * size_of::<u16>(),
        align_of::<usize>(),
    )
    .expect("an error message fits the address space")
}

fn allocate_error_string(message: &str) -> (*const u16, usize) {
    let code_units: Vec<u16> = message.encode_utf16().collect();
    let layout = error_string_layout(code_units.len());
    // SAFETY: The layout has a non-zero size, and the header and message are written within it.
    unsafe {
        let allocation = alloc(layout);
        if allocation.is_null() {
            handle_alloc_error(layout);
        }
        allocation.cast::<usize>().write(code_units.len());
        let message_data = allocation.add(ERROR_STRING_HEADER_SIZE).cast::<u16>();
        ptr::copy_nonoverlapping(code_units.as_ptr(), message_data, code_units.len());
        (message_data, code_units.len())
    }
}

// --- Bytecode dump callbacks ---

unsafe extern "C" {
    /// The native code of the handler of each opcode, which flapc emits with the interpreter.
    static js_interpreter_handler_ranges: [FFIInterpreterHandlerRange; 256];
}

struct BytecodeDumpBuilder<'a> {
    output: &'a mut Vec<u8>,
    executable: &'a Executable,
}

/// # Safety
///
/// ctx must be the BytecodeDumpBuilder dump_bytecode() passes the dumper, which outlives the dump.
unsafe fn bytecode_dump_builder<'a>(ctx: *mut c_void) -> &'a mut BytecodeDumpBuilder<'a> {
    // SAFETY: The caller guarantees the context, which nothing else refers to while a callback runs.
    unsafe { &mut *ctx.cast::<BytecodeDumpBuilder<'a>>() }
}

unsafe extern "C" fn bytecode_dump_append(ctx: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: The dumper passes our context and len bytes of UTF-8 at data, which comes from a Rust string.
    let (builder, text) = unsafe { (bytecode_dump_builder(ctx), slice::from_raw_parts(data, len)) };
    builder.output.extend_from_slice(text);
}

unsafe extern "C" fn bytecode_dump_append_local(ctx: *mut c_void, index: u32) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    Utf16View::of_fly_string(&builder.executable.local_variable_names[index as usize])
        .append_as_wtf8_to(builder.output);
}

unsafe extern "C" fn bytecode_dump_append_identifier(ctx: *mut c_void, index: u32, quoted: bool) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    let identifier = Utf16View::of_fly_string(&builder.executable.identifier_table[index as usize]);
    if quoted {
        builder.output.extend_from_slice(b"\x1b[36m`");
        identifier.append_as_wtf8_to(builder.output);
        builder.output.extend_from_slice(b"`\x1b[0m");
    } else {
        identifier.append_as_wtf8_to(builder.output);
    }
}

/// The table holds the property keys as the strings they were made from. Formatting a PropertyKey writes an array
/// index key as its number, which is the same text, since only canonical index strings become numbers.
unsafe extern "C" fn bytecode_dump_append_property_key(ctx: *mut c_void, index: u32, quoted: bool) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    let property_key = Utf16View::of_fly_string(&builder.executable.property_key_table[index as usize]);
    if quoted {
        builder.output.extend_from_slice(b"\x1b[36m`");
        property_key.append_as_wtf8_to(builder.output);
        builder.output.extend_from_slice(b"`\x1b[0m");
    } else {
        property_key.append_as_wtf8_to(builder.output);
    }
}

unsafe extern "C" fn bytecode_dump_append_string(ctx: *mut c_void, index: u32) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    Utf16View::of_fly_string(&builder.executable.string_table[index as usize]).append_as_wtf8_to(builder.output);
}

unsafe extern "C" fn bytecode_dump_append_value_double(ctx: *mut c_void, value: f64) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    append_double_formatted_like_ak(builder.output, value);
}

unsafe extern "C" fn bytecode_dump_append_value_string(ctx: *mut c_void, encoded: u64) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    Utf16View::of_string(&Value(encoded).as_string().utf16_string()).append_as_wtf8_to(builder.output);
}

unsafe extern "C" fn bytecode_dump_append_value_bigint(ctx: *mut c_void, encoded: u64) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    Utf16View::of_string(&Value(encoded).as_bigint().to_utf16_string()).append_as_wtf8_to(builder.output);
}

unsafe extern "C" fn bytecode_dump_append_value_fallback(ctx: *mut c_void, encoded: u64) {
    // SAFETY: The dumper passes our context.
    let builder = unsafe { bytecode_dump_builder(ctx) };
    append_value_formatted_like_ak(builder.output, Value(encoded));
}

fn make_ffi_exception_handlers(executable: &Executable) -> Vec<FFIDumpExceptionHandler> {
    executable
        .exception_handlers
        .iter()
        .map(|handler| FFIDumpExceptionHandler {
            start_offset: handler.start_offset as usize,
            end_offset: handler.end_offset as usize,
            handler_offset: handler.handler_offset as usize,
        })
        .collect()
}

pub fn dump_bytecode(output: &mut Vec<u8>, executable: &Executable) {
    let exception_handlers = make_ffi_exception_handlers(executable);
    let constants = executable.constants();
    // SAFETY: A Value is its encoded u64.
    let encoded_constants = unsafe { slice::from_raw_parts(constants.as_ptr().cast::<u64>(), constants.len()) };
    let mut builder = BytecodeDumpBuilder { output, executable };
    let callbacks = FFIBytecodeDumpCallbacks {
        append: bytecode_dump_append,
        append_local: bytecode_dump_append_local,
        append_identifier: bytecode_dump_append_identifier,
        append_property_key: bytecode_dump_append_property_key,
        append_string: bytecode_dump_append_string,
        append_value_double: bytecode_dump_append_value_double,
        append_value_string: bytecode_dump_append_value_string,
        append_value_bigint: bytecode_dump_append_value_bigint,
        append_value_fallback: bytecode_dump_append_value_fallback,
    };
    let metadata = FFIBytecodeDumpMetadata {
        number_of_registers: executable.number_of_registers,
        registers_and_locals_count: executable.registers_and_locals_count(),
        local_index_base: executable.local_index_base(),
        argument_index_base: executable.argument_index_base(),
        constants: encoded_constants.as_ptr(),
        constant_count: encoded_constants.len(),
        interpreter_handler_ranges: (&raw const js_interpreter_handler_ranges).cast(),
        dump_interpreter: should_dump_interpreter_assembly(),
    };

    // SAFETY: The frontend validated the bytecode it compiled, the callbacks only read the executable, and the
    // interpreter's handler ranges delimit its code, which is mapped for as long as the process runs.
    unsafe {
        libjs_rust::bytecode::dump::dump_bytecode(
            executable.bytecode(),
            &exception_handlers,
            &metadata,
            encoded_constants,
            ptr::from_mut(&mut builder).cast(),
            &callbacks,
        );
    }
}

pub fn count_bytecode_basic_blocks(executable: &Executable) -> usize {
    let exception_handlers = make_ffi_exception_handlers(executable);
    count_basic_blocks(executable.bytecode(), &exception_handlers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Calls rust_compile_regex the way the frontend does, returning the parsed pattern or the error message.
    fn compile(pattern: &[u16], flags: &str) -> Result<String, String> {
        let flags = utf16(flags);
        let mut error: *const u16 = ptr::null();
        let mut error_length = 0usize;
        // SAFETY: The buffers and out-pointers are valid.
        let handle = unsafe {
            rust_compile_regex(
                pattern.as_ptr(),
                pattern.len(),
                flags.as_ptr(),
                flags.len(),
                &raw mut error,
                &raw mut error_length,
            )
        };
        if error.is_null() {
            assert!(!handle.is_null());
            // SAFETY: A non-null handle is a CompiledRegex that is freed right after.
            let parsed_pattern = String::from_utf16(unsafe { &(*handle.cast::<CompiledRegex>()).parsed_pattern })
                .expect("a parsed pattern is ASCII");
            // SAFETY: The handle came from rust_compile_regex.
            unsafe { rust_free_compiled_regex(handle) };
            Ok(parsed_pattern)
        } else {
            assert!(handle.is_null());
            // SAFETY: The error message has error_length code units and is freed right after.
            let message = String::from_utf16(unsafe { slice::from_raw_parts(error, error_length) })
                .expect("error messages are valid UTF-16");
            // SAFETY: The message came from rust_compile_regex.
            unsafe { rust_free_error_string(error) };
            Err(message)
        }
    }

    #[test]
    fn compiles_valid_literals_into_their_parsed_pattern() {
        let cases = [
            ("a+b", "g", "a+b"),
            ("é", "", "\\u00e9"),
            ("😀", "u", r"\u{1f600}"),
            ("[😀-😂]", "u", r"[\u{1f600}-\u{1f602}]"),
            (r"\é", "u", r"\u{00e9}"),
            (r"\u{110000}", "", r"\u{110000}"),
            (r"\-", "", r"\-"),
            (r"(?<a>x)\k<a>", "dgimsy", r"(?<a>x)\k<a>"),
            ("[[a-z]&&[aeiou]]", "v", "[[a-z]&&[aeiou]]"),
        ];
        for (pattern, flags, parsed_pattern) in cases {
            assert_eq!(
                compile(&utf16(pattern), flags),
                Ok(parsed_pattern.to_owned()),
                "/{pattern}/{flags}"
            );
        }
        assert_eq!(compile(&[0xd835], "u"), Ok(r"\u{d835}".to_owned()));
    }

    #[test]
    fn reports_early_errors_with_the_messages_of_the_cpp_runtime() {
        let cases = [
            ("(", "", "unexpected end of pattern"),
            ("a**", "", "nothing to repeat"),
            ("a{3,2}", "", "invalid quantifier"),
            ("a", "uv", "RegExp flag 'u' is incompatible with flag 'v'"),
            (r"(?<\u{d835}\u{dc9c}>a)", "u", "invalid group name"),
            (r"(?<\u{1DF00}>a)", "", "invalid group name"),
            ("(?<a>x)(?<a>y)", "", "duplicate group name 'a'"),
            (r"\k<a>", "u", r"invalid named backreference \k<a>"),
            (r"\u{110000}", "u", "invalid Unicode escape"),
            (r"\-", "u", r"invalid escape '\-'"),
            ("[z-a]", "", "invalid character range 'z'-'a'"),
            ("[😀-😂]", "", r"invalid character range '\u{DE00}'-'\u{D83D}'"),
            ("(?i-i:a)", "", "invalid modifier flags"),
            ("[(]", "v", "invalid character class"),
        ];
        for (pattern, flags, error) in cases {
            assert_eq!(
                compile(&utf16(pattern), flags),
                Err(format!("RegExp compile error: {error}")),
                "/{pattern}/{flags}"
            );
        }
    }

    #[test]
    fn writes_numbers_as_utf16_truncated_to_the_buffer() {
        let mut buffer = [0u16; 8];
        // SAFETY: Each call passes at most the buffer's length.
        unsafe {
            let written = rust_number_to_utf16(-1.5e-7, buffer.as_mut_ptr(), buffer.len());
            assert_eq!(buffer[..written], utf16("-1.5e-7"));
            let written = rust_number_to_utf16(123456789.0, buffer.as_mut_ptr(), 4);
            assert_eq!(buffer[..written], utf16("1234"));
            assert_eq!(rust_number_to_utf16(1.0, ptr::null_mut(), 0), 0);
        }
    }

    #[test]
    fn freeing_null_does_nothing() {
        // SAFETY: Both functions accept null.
        unsafe {
            rust_free_compiled_regex(ptr::null_mut());
            rust_free_error_string(ptr::null());
        }
    }

    // LibRegex asks LibUnicode, which is C++, about non-ASCII identifier characters and Unicode properties. These tests
    // link without C++, so they answer as if nothing matched, and the cases above need neither.
    #[unsafe(no_mangle)]
    extern "C" fn unicode_code_point_has_identifier_start_property(_code_point: u32) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn unicode_code_point_has_identifier_continue_property(_code_point: u32) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn unicode_is_string_property(_name: *const u8, _name_length: usize) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn unicode_is_valid_ecma262_property(
        _name: *const u8,
        _name_length: usize,
        _value: *const u8,
        _value_length: usize,
    ) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn unicode_resolve_property(
        _name: *const u8,
        _name_length: usize,
        _value: *const u8,
        _value_length: usize,
        _kind: *mut u8,
        _id: *mut u32,
    ) -> bool {
        false
    }

    #[unsafe(no_mangle)]
    extern "C" fn unicode_get_string_property_data(
        _name: *const u8,
        _name_length: usize,
        _out: *mut u32,
        _capacity: u32,
    ) -> u32 {
        0
    }
}
