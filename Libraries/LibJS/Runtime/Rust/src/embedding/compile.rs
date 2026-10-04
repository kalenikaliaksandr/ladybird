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

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod tests {
    use super::*;
    use crate::embedding::abi_types::{
        JSOwnedUtf16String, owned_utf16_string_from_abi, owned_utf16_string_into_abi, vm_into_abi,
    };
    use crate::embedding::script::js_script_run;
    use crate::embedding::script::tests::{ReenteringErrorCollector, ascii_view_of};
    use crate::embedding::source_code::{js_source_code_code, js_source_code_create, js_source_code_release};
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::JS_COMPLETION_NORMAL;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::utilities::initialize_realm;

    pub fn source_code_of(filename: &str, code: &str) -> *const JSSourceCode {
        // SAFETY: Both strings give up their references.
        unsafe {
            js_source_code_create(
                owned_utf16_string_into_abi(ak::Utf16String::from_utf8(filename)),
                owned_utf16_string_into_abi(ak::Utf16String::from_utf8(code)),
            )
        }
    }

    /// The code of the source code, as an embedder hands it to a worker thread.
    pub fn code_for_another_thread(source_code: *const JSSourceCode) -> ak::Utf16String {
        // SAFETY: The source code is live, and the code is an owned string the caller adopts.
        unsafe { owned_utf16_string_from_abi(js_source_code_code(source_code)) }
    }

    /// Parses and compiles on another thread, as an embedder does while it fetches a script, and hands back the
    /// compiled program, or the parsed one if it has syntax errors, as the address that crosses back to the VM's thread.
    pub fn parse_and_compile_on_another_thread(
        code: ak::Utf16String,
        program_type: JSProgramType,
        line_number_offset: usize,
    ) -> Result<usize, usize> {
        std::thread::spawn(move || {
            let view = JSUtf16View::of(Utf16View::of_string(&code));
            // SAFETY: The view is of a string that outlives the parse.
            let parsed = unsafe { js_compile_parse(view, program_type, line_number_offset) };
            // SAFETY: The program was just parsed, and compiling it consumes it.
            unsafe {
                if js_compile_parsed_program_has_errors(parsed) {
                    return Err(parsed.expose_provenance());
                }
                Ok(js_compile_parsed_program(parsed).expose_provenance())
            }
        })
        .join()
        .expect("the worker thread finishes")
    }

    unsafe extern "C" fn collect_error_without_reentering(
        context: *mut c_void,
        message: JSOwnedUtf16String,
        line: u32,
        column: u32,
    ) {
        // SAFETY: The sink's context is a vector of errors, and the message is an owned string the sink adopts.
        let (errors, message) = unsafe {
            (
                &mut *context.cast::<Vec<(String, u32, u32)>>(),
                owned_utf16_string_from_abi(message),
            )
        };
        errors.push((Utf16View::of_string(&message).to_utf8(), line, column));
    }

    #[test]
    fn scripts_compiled_on_another_thread_run_on_the_vms_thread() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source_code = source_code_of(
            "worker.js",
            "function twice(x) { return 2 * x }\n(function () { return twice(21) })()",
        );
        let compiled =
            parse_and_compile_on_another_thread(code_for_another_thread(source_code), JS_PROGRAM_TYPE_SCRIPT, 1)
                .expect("the script parses");
        // SAFETY: The program came from the worker, and the VM, source code and realm are live.
        let script = unsafe {
            js_compile_create_script_from_compiled_program(
                vm_into_abi(&vm),
                core::ptr::with_exposed_provenance_mut(compiled),
                source_code,
                cell_into_abi(realm),
                ascii_view_of("https://example.com/worker.js"),
                core::ptr::null_mut(),
            )
        };
        // SAFETY: The script is live.
        unsafe {
            assert_eq!(js_compile_top_level_source_code_of_script(script), source_code);
            assert_eq!(cell_from_abi(script).filename(), "https://example.com/worker.js");
            let completion = js_script_run(vm_into_abi(&vm), script, core::ptr::null_mut());
            assert!(completion.variant == JS_COMPLETION_NORMAL);
            assert!(Value(completion.payload) == Value::from_i32(42));
            js_source_code_release(source_code);
        }
        // The script keeps the source code alive once the embedder lets go of it.
        assert_eq!(
            utf8(run_script(&vm, realm, "twice.toString()").must()),
            "function twice(x) { return 2 * x }"
        );
    }

    #[test]
    fn syntax_errors_of_programs_reach_sinks_on_either_thread() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source_code = source_code_of("broken.js", "let ok = 1;\nlet broken = ;");
        let parsed =
            parse_and_compile_on_another_thread(code_for_another_thread(source_code), JS_PROGRAM_TYPE_SCRIPT, 3)
                .expect_err("the script does not parse");
        let parsed = core::ptr::with_exposed_provenance_mut::<JSParsedProgram>(parsed);

        let errors_on_another_thread = std::thread::scope(|scope| {
            let parsed = parsed.expose_provenance();
            scope
                .spawn(move || {
                    let mut errors: Vec<(String, u32, u32)> = Vec::new();
                    let sink = JSParserErrorSink {
                        context: (&raw mut errors).cast(),
                        append: Some(collect_error_without_reentering),
                    };
                    // SAFETY: The program is live, and the sink outlives the call.
                    unsafe {
                        js_compile_parsed_program_errors(core::ptr::with_exposed_provenance(parsed), &raw const sink);
                    };
                    errors
                })
                .join()
                .expect("the worker thread finishes")
        });
        assert_eq!(errors_on_another_thread.len(), 1);
        assert_eq!((errors_on_another_thread[0].1, errors_on_another_thread[0].2), (4, 14));

        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let sink = errors.sink();
        // SAFETY: The program, VM, source code and realm are live, and the sink outlives the call.
        let script = unsafe {
            js_compile_create_script_from_parsed_program(
                vm_into_abi(&vm),
                parsed,
                source_code,
                cell_into_abi(realm),
                ascii_view_of("broken.js"),
                core::ptr::null_mut(),
                &raw const sink,
            )
        };
        assert!(script.is_null());
        assert_eq!(errors.errors, errors_on_another_thread);
        assert_eq!(errors.results_of_scripts_run_while_appending, ["2,4"]);

        let parsed_without_errors = parse_ascii_script("1 + 1");
        // SAFETY: The program is live, and destroying it gives it up.
        unsafe {
            assert!(!js_compile_parsed_program_has_errors(parsed_without_errors));
            js_compile_parsed_program_destroy(parsed_without_errors);
            js_compile_parsed_program_destroy(core::ptr::null_mut());
            js_compile_compiled_program_destroy(js_compile_parsed_program(parse_ascii_script("/a+/g")));
            js_compile_compiled_program_destroy(core::ptr::null_mut());
            js_source_code_release(source_code);
        }
    }

    fn parse_ascii_script(source: &str) -> *mut JSParsedProgram {
        // SAFETY: The view is of a string that outlives the parse.
        unsafe { js_compile_parse(ascii_view_of(source), JS_PROGRAM_TYPE_SCRIPT, 1) }
    }

    #[test]
    fn modules_compiled_on_another_thread_run_and_report_their_top_level_source_code() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source_code = source_code_of(
            "module.mjs",
            "function twice(x) { return 2 * x }\nglobalThis.fromModule = twice(21);\nexport default 1;",
        );
        let compiled =
            parse_and_compile_on_another_thread(code_for_another_thread(source_code), JS_PROGRAM_TYPE_MODULE, 1)
                .expect("the module parses");
        // SAFETY: The program came from the worker, and the VM, source code and realm are live.
        let module = unsafe {
            js_compile_create_module_from_compiled_program(
                vm_into_abi(&vm),
                core::ptr::with_exposed_provenance_mut(compiled),
                source_code,
                cell_into_abi(realm),
                ascii_view_of("https://example.com/module.mjs"),
                core::ptr::null_mut(),
            )
        };
        // SAFETY: The module is live.
        let source_text_module = unsafe {
            assert_eq!(js_compile_top_level_source_code_of_module(module), source_code);
            source_text_module_from_abi(module)
        };
        vm.run_module(source_text_module).must();
        assert_eq!(utf8(run_script(&vm, realm, "fromModule").must()), "42");

        let with_top_level_await = source_code_of("await.mjs", "await null;\nglobalThis.afterAwait = 'ran';");
        let parsed = parse_module(&code_for_another_thread(with_top_level_await));
        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let sink = errors.sink();
        // SAFETY: The program, VM, source code and realm are live, and the sink outlives the call.
        let module = unsafe {
            js_compile_create_module_from_parsed_program(
                vm_into_abi(&vm),
                parsed,
                with_top_level_await,
                cell_into_abi(realm),
                ascii_view_of("https://example.com/await.mjs"),
                core::ptr::null_mut(),
                &raw const sink,
            )
        };
        assert!(errors.errors.is_empty());
        // SAFETY: The module is live.
        unsafe {
            assert!(js_compile_top_level_source_code_of_module(module).is_null());
            vm.run_module(source_text_module_from_abi(module)).must();
        }
        assert_eq!(utf8(run_script(&vm, realm, "afterAwait").must()), "ran");

        let broken = source_code_of("broken.mjs", "export export;");
        let parsed = parse_module(&code_for_another_thread(broken));
        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let sink = errors.sink();
        // SAFETY: As above.
        let module = unsafe {
            js_compile_create_module_from_parsed_program(
                vm_into_abi(&vm),
                parsed,
                broken,
                cell_into_abi(realm),
                ascii_view_of("https://example.com/broken.mjs"),
                core::ptr::null_mut(),
                &raw const sink,
            )
        };
        assert!(module.is_null());
        assert!(!errors.errors.is_empty());
        assert_eq!(errors.results_of_scripts_run_while_appending.len(), errors.errors.len());
        // SAFETY: The test owns a reference to each source code.
        unsafe {
            js_source_code_release(source_code);
            js_source_code_release(with_top_level_await);
            js_source_code_release(broken);
        }
    }

    fn parse_module(code: &ak::Utf16String) -> *mut JSParsedProgram {
        // SAFETY: The view is of a string that outlives the parse.
        unsafe { js_compile_parse(JSUtf16View::of(Utf16View::of_string(code)), JS_PROGRAM_TYPE_MODULE, 1) }
    }
}
