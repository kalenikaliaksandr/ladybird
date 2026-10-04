/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The frontend's work, which needs no VM: parsing and compiling programs, which an embedder may do on any thread, as
//! C++ LibJS/ScriptCompilation.h allows, and then turning them into Script and Source Text Module Records on the VM's
//! thread.
//!
//! A JSParsedProgram and a JSCompiledProgram hold nothing of the VM or its heap, so any thread may create, inspect,
//! compile or destroy them, and they may move between threads. Everything that takes a JSVM runs on the VM's thread.

use std::borrow::Cow;

use crate::embedding::abi_types::{JSRealm, JSSourceCode, JSUtf16View, cell_from_abi, cell_into_abi, vm_from_abi};
use crate::embedding::script::{JSParserErrorSink, JSScript, append_to_parser_error_sink, host_defined_slot_from_abi};
use crate::embedding::source_code::{shared_source_code_from_abi, source_code_into_abi};
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSModule, JSVM};
use crate::parser_error::ParserError;
use crate::runtime::module::Module;
use crate::runtime::source_text_module::SourceTextModule;
use crate::script::Script;
use crate::utf16::Utf16View;
use core::ffi::c_void;
use libjs_rust::ast::ProgramType;
use libjs_rust::compile::{
    CompiledProgram, FunctionPrecompileMode, ParsedProgram, compile_module, compile_parsed_program_off_thread,
    compile_script, parse,
};

/// A program that the frontend parsed, with or without syntax errors, which no VM owns: C++ JS::ParsedProgram.
pub struct JSParsedProgram {
    _opaque: [u8; 0],
}

/// A program compiled to bytecode, which no VM owns yet: C++ JS::CompiledProgram.
pub struct JSCompiledProgram {
    _opaque: [u8; 0],
}

/// JS::ProgramType: whether source text is a classic script or a module.
pub type JSProgramType = u8;

pub const JS_PROGRAM_TYPE_SCRIPT: JSProgramType = 0;
pub const JS_PROGRAM_TYPE_MODULE: JSProgramType = 1;

const _: () = assert!(JS_PROGRAM_TYPE_SCRIPT == ProgramType::Script as u8);
const _: () = assert!(JS_PROGRAM_TYPE_MODULE == ProgramType::Module as u8);

pub fn program_type_from_abi(program_type: JSProgramType) -> ProgramType {
    match program_type {
        JS_PROGRAM_TYPE_SCRIPT => ProgramType::Script,
        JS_PROGRAM_TYPE_MODULE => ProgramType::Module,
        program_type => panic!("{program_type} is not a program type"),
    }
}

/// What the frontend parses from: UTF-16 code units, which a view in the ASCII storage kind is widened to.
///
/// # Safety
///
/// The view must be valid for `'a`.
pub unsafe fn code_units_of<'a>(source: JSUtf16View) -> Cow<'a, [u16]> {
    // SAFETY: The caller passes a valid view.
    match unsafe { source.as_view() } {
        Utf16View::Utf16(code_units) => Cow::Borrowed(code_units),
        ascii @ Utf16View::Ascii(_) => Cow::Owned(ascii.code_units().collect()),
    }
}

/// A parsed program together with the length of the source it was parsed from, which compiling it needs.
struct ParsedSourceText {
    program: ParsedProgram,
    source_length_in_code_units: usize,
}

fn parsed_program_into_abi(parsed: ParsedSourceText) -> *mut JSParsedProgram {
    Box::into_raw(Box::new(parsed)).cast()
}

/// # Safety
///
/// `parsed` must be a parsed program the ABI handed out and that is still alive for `'a`.
unsafe fn parsed_program_from_abi<'a>(parsed: *const JSParsedProgram) -> &'a ParsedSourceText {
    assert!(!parsed.is_null(), "the embedder passes a parsed program");
    // SAFETY: The caller passes a live parsed program, which is a boxed ParsedSourceText.
    unsafe { &*parsed.cast::<ParsedSourceText>() }
}

/// # Safety
///
/// `parsed` must be a parsed program the ABI handed out, which the caller gives up.
unsafe fn take_parsed_program_from_abi(parsed: *mut JSParsedProgram) -> ParsedSourceText {
    assert!(!parsed.is_null(), "the embedder passes a parsed program");
    // SAFETY: The caller gives up a boxed ParsedSourceText.
    *unsafe { Box::from_raw(parsed.cast::<ParsedSourceText>()) }
}

fn compiled_program_into_abi(compiled: CompiledProgram) -> *mut JSCompiledProgram {
    Box::into_raw(Box::new(compiled)).cast()
}

/// # Safety
///
/// `compiled` must be a compiled program the ABI handed out, which the caller gives up.
unsafe fn take_compiled_program_from_abi(compiled: *mut JSCompiledProgram) -> CompiledProgram {
    assert!(!compiled.is_null(), "the embedder passes a compiled program");
    // SAFETY: The caller gives up a boxed CompiledProgram.
    *unsafe { Box::from_raw(compiled.cast::<CompiledProgram>()) }
}

fn module_into_abi(module: Gc<SourceTextModule>) -> *mut JSModule {
    module.upcast::<Module>().as_ptr().cast()
}

/// # Safety
///
/// `module` must be a live Source Text Module Record.
pub unsafe fn source_text_module_from_abi(module: *mut JSModule) -> Gc<SourceTextModule> {
    let module = core::ptr::NonNull::new(module.cast::<Module>()).expect("the embedder passes a module");
    // SAFETY: The caller passes a live module.
    unsafe { Gc::from_non_null(module) }
        .downcast::<SourceTextModule>()
        .expect("the embedder passes a Source Text Module Record")
}

/// ParsedProgram::parse(source_text, type, line_number_offset): parses `source` as a classic script or a module, with
/// its lines counted from `line_number_offset`, except that a module's lines start at 1 or later. The program holds its
/// syntax errors, if it has any. The caller owns the program. Borrows the view. Any thread may call this.
///
/// # Safety
///
/// The view must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_parse(
    source: JSUtf16View,
    program_type: JSProgramType,
    line_number_offset: usize,
) -> *mut JSParsedProgram {
    // SAFETY: The caller passes a valid view.
    let source = unsafe { code_units_of(source) };
    parsed_program_into_abi(ParsedSourceText {
        program: parse(&source, program_type_from_abi(program_type), line_number_offset),
        source_length_in_code_units: source.len(),
    })
}

/// ParsedProgram::has_errors(). Any thread may call this.
///
/// # Safety
///
/// `parsed` must be a live parsed program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_parsed_program_has_errors(parsed: *const JSParsedProgram) -> bool {
    // SAFETY: The caller passes a live parsed program.
    unsafe { parsed_program_from_abi(parsed) }.program.has_errors()
}

/// Appends the syntax errors of the program, if it has any, to `errors`, on the calling thread. Any thread may call
/// this.
///
/// # Safety
///
/// `parsed` must be a live parsed program, and `errors` a valid sink.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_parsed_program_errors(
    parsed: *const JSParsedProgram,
    errors: *const JSParserErrorSink,
) {
    // SAFETY: The caller passes a live parsed program.
    let parsed = unsafe { parsed_program_from_abi(parsed) };
    let parser_errors = ParserError::all_from_parsed_program(&parsed.program);
    // SAFETY: The caller passes a valid sink.
    unsafe { append_to_parser_error_sink(errors, &parser_errors) };
}

/// Destroys a parsed program that the caller owns. Null does nothing. Any thread may call this.
///
/// # Safety
///
/// `parsed` must be null or a live parsed program, which the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_parsed_program_destroy(parsed: *mut JSParsedProgram) {
    if !parsed.is_null() {
        // SAFETY: The caller gives up a live parsed program.
        drop(unsafe { take_parsed_program_from_abi(parsed) });
    }
}

/// CompiledProgram::compile(ParsedProgram): compiles a program that parsed without errors, which this consumes. The
/// bytecode covers the top level and the functions it invokes right away; every other function compiles on its first
/// call. The caller owns the compiled program. Any thread may call this.
///
/// # Safety
///
/// `parsed` must be a live parsed program without errors, which the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_parsed_program(parsed: *mut JSParsedProgram) -> *mut JSCompiledProgram {
    // SAFETY: The caller gives up a live parsed program.
    let parsed = unsafe { take_parsed_program_from_abi(parsed) };
    compiled_program_into_abi(compile_parsed_program_off_thread(
        parsed.program,
        parsed.source_length_in_code_units,
        FunctionPrecompileMode::EagerOnly,
    ))
}

/// Destroys a compiled program that the caller owns and that will not run, freeing what the frontend compiled for it.
/// Null does nothing. Any thread may call this.
///
/// # Safety
///
/// `compiled` must be null or a live compiled program, which the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_compiled_program_destroy(compiled: *mut JSCompiledProgram) {
    if !compiled.is_null() {
        // SAFETY: The caller gives up a live compiled program.
        unsafe { take_compiled_program_from_abi(compiled) }.discard();
    }
}

/// The parsed program, if it has no syntax errors. Otherwise appends them to `errors` and destroys the program.
///
/// # Safety
///
/// `errors` must be null or a valid sink.
unsafe fn parsed_program_without_errors(
    parsed: ParsedSourceText,
    errors: *const JSParserErrorSink,
) -> Option<ParsedProgram> {
    if !parsed.program.has_errors() {
        return Some(parsed.program);
    }
    let parser_errors = ParserError::all_from_parsed_program(&parsed.program);
    drop(parsed);
    // SAFETY: The caller passes null or a valid sink.
    unsafe { append_to_parser_error_sink(errors, &parser_errors) };
    None
}

/// create_script(ParsedProgram, source_code, realm, filename, host_defined): the Script Record of a classic script
/// parsed from the code of `source_code`, which this compiles now and which reports the filename of the source code.
/// Its dynamic imports resolve against `filename`, and `host_defined` (null for none) is its [[HostDefined]]. Consumes
/// the program. Returns the script, which the caller keeps alive, or null after appending the program's syntax errors
/// to `errors` (which may be null). Only the VM's thread may call this.
///
/// # Safety
///
/// `vm`, `source_code` and `realm` must be live, `parsed` a live parsed script, which the caller gives up, `filename`
/// a valid view, `host_defined` null or a live cell, and `errors` null or a valid sink.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments, reason = "C++ create_script takes all of these")]
pub unsafe extern "C" fn js_compile_create_script_from_parsed_program(
    vm: *mut JSVM,
    parsed: *mut JSParsedProgram,
    source_code: *const JSSourceCode,
    realm: *mut JSRealm,
    filename: JSUtf16View,
    host_defined: *mut c_void,
    errors: *const JSParserErrorSink,
) -> *mut JSScript {
    // SAFETY: The caller passes a live VM, program, source code, realm and host-defined cell, and a valid view.
    let (vm, parsed, source_code, realm, filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            take_parsed_program_from_abi(parsed),
            shared_source_code_from_abi(source_code),
            cell_from_abi(realm),
            filename.as_view().to_utf8(),
            host_defined_slot_from_abi(host_defined),
        )
    };
    // SAFETY: The caller passes null or a valid sink.
    let Some(parsed) = (unsafe { parsed_program_without_errors(parsed, errors) }) else {
        return core::ptr::null_mut();
    };
    let source_length = source_code.length_in_code_units();
    cell_into_abi(Script::create(
        vm,
        realm,
        compile_script(parsed, source_length),
        source_code,
        &filename,
        host_defined,
    ))
}

/// create_script(CompiledProgram, source_code, realm, filename, host_defined): the Script Record of a classic script
/// compiled, on any thread, from the code of `source_code`, which reports the filename of the source code. Its dynamic
/// imports resolve against `filename`, and `host_defined` (null for none) is its [[HostDefined]]. Consumes the program.
/// Returns the script, which the caller keeps alive. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm`, `source_code` and `realm` must be live, `compiled` a live compiled script, which the caller gives up,
/// `filename` a valid view, and `host_defined` null or a live cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_create_script_from_compiled_program(
    vm: *mut JSVM,
    compiled: *mut JSCompiledProgram,
    source_code: *const JSSourceCode,
    realm: *mut JSRealm,
    filename: JSUtf16View,
    host_defined: *mut c_void,
) -> *mut JSScript {
    // SAFETY: The caller passes a live VM, program, source code, realm and host-defined cell, and a valid view.
    let (vm, compiled, source_code, realm, filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            take_compiled_program_from_abi(compiled),
            shared_source_code_from_abi(source_code),
            cell_from_abi(realm),
            filename.as_view().to_utf8(),
            host_defined_slot_from_abi(host_defined),
        )
    };
    cell_into_abi(Script::create(
        vm,
        realm,
        compiled.into_script(),
        source_code,
        &filename,
        host_defined,
    ))
}

/// create_module(ParsedProgram, source_code, realm, filename, host_defined): like
/// js_compile_create_script_from_parsed_program(), but for a module, whose imports resolve against `filename`. Returns
/// the Source Text Module Record, or null after appending the program's syntax errors to `errors`. Only the VM's
/// thread may call this.
///
/// # Safety
///
/// As for js_compile_create_script_from_parsed_program(), with a parsed module.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments, reason = "C++ create_module takes all of these")]
pub unsafe extern "C" fn js_compile_create_module_from_parsed_program(
    vm: *mut JSVM,
    parsed: *mut JSParsedProgram,
    source_code: *const JSSourceCode,
    realm: *mut JSRealm,
    filename: JSUtf16View,
    host_defined: *mut c_void,
    errors: *const JSParserErrorSink,
) -> *mut JSModule {
    // SAFETY: The caller passes a live VM, program, source code, realm and host-defined cell, and a valid view.
    let (vm, parsed, source_code, realm, filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            take_parsed_program_from_abi(parsed),
            shared_source_code_from_abi(source_code),
            cell_from_abi(realm),
            filename.as_view().to_utf8(),
            host_defined_slot_from_abi(host_defined),
        )
    };
    // SAFETY: The caller passes null or a valid sink.
    let Some(parsed) = (unsafe { parsed_program_without_errors(parsed, errors) }) else {
        return core::ptr::null_mut();
    };
    let source_length = source_code.length_in_code_units();
    module_into_abi(SourceTextModule::create(
        vm,
        realm,
        &filename,
        compile_module(parsed, source_length),
        source_code,
        host_defined,
    ))
}

/// create_module(CompiledProgram, source_code, realm, filename, host_defined): like
/// js_compile_create_script_from_compiled_program(), but for a module, whose imports resolve against `filename`.
/// Returns the Source Text Module Record. Only the VM's thread may call this.
///
/// # Safety
///
/// As for js_compile_create_script_from_compiled_program(), with a compiled module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_create_module_from_compiled_program(
    vm: *mut JSVM,
    compiled: *mut JSCompiledProgram,
    source_code: *const JSSourceCode,
    realm: *mut JSRealm,
    filename: JSUtf16View,
    host_defined: *mut c_void,
) -> *mut JSModule {
    // SAFETY: The caller passes a live VM, program, source code, realm and host-defined cell, and a valid view.
    let (vm, compiled, source_code, realm, filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            take_compiled_program_from_abi(compiled),
            shared_source_code_from_abi(source_code),
            cell_from_abi(realm),
            filename.as_view().to_utf8(),
            host_defined_slot_from_abi(host_defined),
        )
    };
    module_into_abi(SourceTextModule::create(
        vm,
        realm,
        &filename,
        compiled.into_module(),
        source_code,
        host_defined,
    ))
}

/// top_level_source_code(Script const&): the source code that the script's top-level code was compiled from, which the
/// script keeps alive, and a caller that keeps it longer retains. Only the VM's thread may call this.
///
/// # Safety
///
/// `script` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_top_level_source_code_of_script(script: *mut JSScript) -> *const JSSourceCode {
    // SAFETY: The caller passes a live script.
    let script = unsafe { cell_from_abi(script) };
    script
        .cached_executable()
        .source_code
        .as_ref()
        .map_or(core::ptr::null(), source_code_into_abi)
}

/// top_level_source_code(SourceTextModule const&): like js_compile_top_level_source_code_of_script(), but null for a
/// module with top-level await, whose body is compiled as an async function instead, as in C++. Only the VM's thread
/// may call this.
///
/// # Safety
///
/// `module` must be a live Source Text Module Record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_compile_top_level_source_code_of_module(module: *mut JSModule) -> *const JSSourceCode {
    // SAFETY: The caller passes a live Source Text Module Record.
    let module = unsafe { source_text_module_from_abi(module) };
    module
        .cached_executable()
        .and_then(|executable| executable.source_code.as_ref().map(source_code_into_abi))
        .unwrap_or(core::ptr::null())
}
