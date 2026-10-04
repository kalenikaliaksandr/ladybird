/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Classic scripts: Script Records, which an embedder parses with a [[HostDefined]] of its own and runs with
//! ScriptEvaluation. compile.rs creates them from programs parsed and compiled on other threads.

use core::ffi::c_void;
use core::ptr::NonNull;

use crate::embedding::abi_types::{
    CellAbi, JSOwnedUtf16String, JSRealm, JSUtf16View, cell_from_abi, cell_into_abi, completion_into_abi,
    optional_cell_from_abi, owned_utf16_string_into_abi, vm_from_abi,
};
use crate::embedding::environment::JSEnvironment;
use crate::gc::foreign::ForeignCellSlot;
use crate::layout::host_class::{JSCompletion, JSVM};
use crate::parser_error::ParserError;
use crate::runtime::error::ErrorKind;
use crate::script::Script;

/// A Script Record, which C only ever sees behind a pointer. It is the cell that a JSScriptOrModule with the script tag
/// holds.
pub struct JSScript {
    _opaque: [u8; 0],
}

impl CellAbi for JSScript {
    type Cell = Script;
}

/// Where the runtime reports the syntax errors of a source text, in source order. `append` receives each error's
/// message, which it adopts with AK::Utf16String::adopt_raw(), and the line and column it points at, as C++ ParserError
/// holds them. The runtime calls it on the thread that asked for the errors, holding nothing of the VM, so on the VM's
/// thread it may run JavaScript.
#[repr(C)]
pub struct JSParserErrorSink {
    pub context: *mut c_void,
    pub append: Option<unsafe extern "C" fn(context: *mut c_void, message: JSOwnedUtf16String, line: u32, column: u32)>,
}

/// Hands every error to the embedder's sink, or drops them if it passed none.
///
/// # Safety
///
/// `sink` must be null or point to a sink with an append function.
pub unsafe fn append_to_parser_error_sink(sink: *const JSParserErrorSink, errors: &[ParserError]) {
    // SAFETY: The caller passes null or a valid sink.
    let Some(sink) = (unsafe { sink.as_ref() }) else {
        return;
    };
    let append = sink.append.expect("a parser error sink has an append function");
    for error in errors {
        let message = owned_utf16_string_into_abi(ak::Utf16String::from_utf8(&error.message));
        // SAFETY: The embedder's sink takes the errors with the context it came with, and adopts each message.
        unsafe { append(sink.context, message, error.line, error.column) };
    }
}

/// A slot holding the embedder's [[HostDefined]] cell, or an empty one for null.
///
/// # Safety
///
/// `host_defined` must be null or the address of a live cell of the VM's heap.
pub unsafe fn host_defined_slot_from_abi(host_defined: *mut c_void) -> ForeignCellSlot {
    let slot = ForeignCellSlot::empty();
    // SAFETY: The caller passes null or a live cell, which the slot keeps alive from now on.
    unsafe { slot.set(NonNull::new(host_defined)) };
    slot
}

/// Script::parse(source_text, realm, filename, display_filename, host_defined, line_number_offset): ParseScript of
/// `source` in `realm`, as C++ runs it for a host. The script's code reports `display_filename`, or `filename` if that
/// is empty, in its stack frames and errors, and counts its lines from `line_number_offset`. Its dynamic imports resolve
/// against `filename`. `host_defined` is null or one of the embedder's GC cells, which the script keeps alive as its
/// [[HostDefined]]. Returns the script, which the caller keeps alive, or null after appending the syntax errors to
/// `errors` (which may be null). Borrows the views. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm` and `realm` must be live, the views valid, `host_defined` null or a live cell, and `errors` null or a valid
/// sink.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments, reason = "C++ Script::parse takes all of these")]
pub unsafe extern "C" fn js_script_parse(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    source: JSUtf16View,
    filename: JSUtf16View,
    display_filename: JSUtf16View,
    host_defined: *mut c_void,
    line_number_offset: usize,
    errors: *const JSParserErrorSink,
) -> *mut JSScript {
    // SAFETY: The caller passes a live VM, realm and host-defined cell, and valid views.
    let (vm, realm, source, filename, display_filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            source.as_view(),
            filename.as_view(),
            display_filename.as_view(),
            host_defined_slot_from_abi(host_defined),
        )
    };
    let source: Vec<u16> = source.code_units().collect();
    match Script::parse_with_host_defined(
        vm,
        &source,
        realm,
        &filename.to_utf8(),
        display_filename.to_utf16_string(),
        host_defined,
        line_number_offset,
    ) {
        Ok(script) => cell_into_abi(script),
        Err(parser_errors) => {
            // SAFETY: The caller passes null or a valid sink.
            unsafe { append_to_parser_error_sink(errors, &parser_errors) };
            core::ptr::null_mut()
        }
    }
}

/// VM::run(Script&, lexical_environment_override): ScriptEvaluation of the script, whose execution context gets the
/// override as its LexicalEnvironment unless that is null. As in C++, the override only reaches what resolves through
/// the LexicalEnvironment, such as typeof and direct eval: identifiers that the script compiles to global variable
/// accesses still read and write the global environment directly. Like every script, it runs on top of the execution
/// context stack, which must not be empty, as a host runs it in a context of the script's realm. Only the VM's thread
/// may call this.
///
/// # Safety
///
/// `vm` and `script` must be live, and `lexical_environment_override` null or a live environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_script_run(
    vm: *mut JSVM,
    script: *mut JSScript,
    lexical_environment_override: *mut JSEnvironment,
) -> JSCompletion {
    // SAFETY: The caller passes a live VM, script and environment, if any.
    let (vm, script, lexical_environment_override) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(script),
            optional_cell_from_abi(lexical_environment_override),
        )
    };
    completion_into_abi(vm.run_script(script, lexical_environment_override))
}

/// Script::host_defined(): the [[HostDefined]] cell the script was created with, or null for none. Only the VM's thread
/// may call this.
///
/// # Safety
///
/// `script` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_script_host_defined(script: *mut JSScript) -> *mut c_void {
    // SAFETY: The caller passes a live script.
    let script = unsafe { cell_from_abi(script) };
    script.host_defined().map_or(core::ptr::null_mut(), NonNull::as_ptr)
}

/// Script::realm(): the script's [[Realm]]. Only the VM's thread may call this.
///
/// # Safety
///
/// `script` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_script_realm(script: *mut JSScript) -> *mut JSRealm {
    // SAFETY: The caller passes a live script.
    let script = unsafe { cell_from_abi(script) };
    cell_into_abi(script.realm())
}

/// ParseScript and ScriptEvaluation of `source` in `realm`: the completion of the script, or a thrown SyntaxError of
/// the current realm if the source does not parse. `source_name` names the script in stack traces, and its dynamic
/// imports resolve against it. Like every script, it runs on top of the execution context stack, which must not be
/// empty: a host runs it in the realm's own execution context. Only the VM's thread may call this.
///
/// # Safety
///
/// `vm` and `realm` must be live, and the views must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_script_evaluate(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    source: JSUtf16View,
    source_name: JSUtf16View,
) -> JSCompletion {
    // SAFETY: The caller passes a live VM and realm, and valid views.
    let (vm, realm, source, source_name) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            source.as_view(),
            source_name.as_view(),
        )
    };
    let source: Vec<u16> = source.code_units().collect();
    let result = match Script::parse_with_filename(vm, &source, realm, &source_name.to_utf8()) {
        Ok(script) => vm.run_script(script, None),
        Err(errors) => vm.throw_completion_with_message(ErrorKind::SyntaxError, errors[0].to_string()),
    };
    completion_into_abi(result)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod tests {
    use super::*;
    use crate::embedding::abi_types::{object_into_abi, owned_utf16_string_from_abi, vm_into_abi};
    use crate::embedding::environment::environment_to_abi;
    use crate::interpreter::vm::Vm;
    use crate::layout::cell::Gc;
    use crate::layout::host_class::JS_COMPLETION_NORMAL;
    use crate::layout::realm::Realm;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::declarative_environment::DeclarativeEnvironment;
    use crate::runtime::environment::InitializeBindingHint;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::object::Object;
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    pub fn ascii_view_of(ascii: &str) -> JSUtf16View {
        JSUtf16View {
            data: ascii.as_ptr().cast(),
            length_in_code_units: ascii.len(),
            has_ascii_storage: true,
        }
    }

    /// Collects the errors a sink receives, and for each runs a script and collects garbage, as an embedder may.
    pub struct ReenteringErrorCollector<'vm> {
        pub vm: &'vm Vm,
        pub realm: Gc<Realm>,
        pub errors: Vec<(String, u32, u32)>,
        pub results_of_scripts_run_while_appending: Vec<String>,
    }

    impl<'vm> ReenteringErrorCollector<'vm> {
        pub fn new(vm: &'vm Vm, realm: Gc<Realm>) -> Self {
            Self {
                vm,
                realm,
                errors: Vec::new(),
                results_of_scripts_run_while_appending: Vec::new(),
            }
        }

        pub fn sink(&mut self) -> JSParserErrorSink {
            JSParserErrorSink {
                context: core::ptr::from_mut(self).cast(),
                append: Some(Self::append),
            }
        }

        unsafe extern "C" fn append(context: *mut c_void, message: JSOwnedUtf16String, line: u32, column: u32) {
            // SAFETY: The sink's context is the collector, and the message is an owned string the sink adopts.
            let (collector, message) = unsafe { (&mut *context.cast::<Self>(), owned_utf16_string_from_abi(message)) };
            collector.vm.heap().collect_garbage();
            let result = run_script(collector.vm, collector.realm, "[1, 2].map(x => x * 2).join()").must();
            collector.results_of_scripts_run_while_appending.push(utf8(result));
            collector
                .errors
                .push((Utf16View::of_string(&message).to_utf8(), line, column));
        }
    }

    fn parse(
        vm: &Vm,
        realm: Gc<Realm>,
        source: &str,
        display_filename: &str,
        host_defined: *mut c_void,
        line_number_offset: usize,
        errors: &mut ReenteringErrorCollector,
    ) -> *mut JSScript {
        let sink = errors.sink();
        // SAFETY: The VM, realm and host-defined cell are live, and the views and the sink outlive the call.
        unsafe {
            js_script_parse(
                vm_into_abi(vm),
                cell_into_abi(realm),
                ascii_view_of(source),
                ascii_view_of("https://example.com/script.js"),
                ascii_view_of(display_filename),
                host_defined,
                line_number_offset,
                &raw const sink,
            )
        }
    }

    #[test]
    fn scripts_keep_their_host_defined_cell_and_report_their_display_filename() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let host_defined = Object::create(&vm, realm, None);
        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let script = parse(
            &vm,
            realm,
            "var where = new Error().stack; where",
            "display.js",
            object_into_abi(host_defined).cast(),
            1,
            &mut errors,
        );
        assert!(!script.is_null() && errors.errors.is_empty());
        // SAFETY: The script is live: the conservative scan sees it on the stack.
        let script_cell = unsafe { cell_from_abi(script) };
        assert_eq!(
            script_cell.filename(),
            "https://example.com/script.js",
            "dynamic imports resolve against the filename"
        );
        // SAFETY: As above.
        unsafe {
            assert_eq!(js_script_host_defined(script), object_into_abi(host_defined).cast());
            assert!(cell_from_abi(js_script_realm(script)) == realm);
            let completion = js_script_run(vm_into_abi(&vm), script, core::ptr::null_mut());
            assert!(completion.variant == JS_COMPLETION_NORMAL);
            let stack = utf8(Value(completion.payload));
            assert!(stack.contains("display.js:1:"), "{stack}");
        }

        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let script = parse(&vm, realm, "1", "", core::ptr::null_mut(), 1, &mut errors);
        // SAFETY: As above.
        let script_cell = unsafe { cell_from_abi(script) };
        // SAFETY: As above.
        assert!(unsafe { js_script_host_defined(script) }.is_null());
        let source_code = script_cell.cached_executable().source_code.clone();
        assert_eq!(
            Utf16View::of_string(source_code.expect("the script has source code").filename()).to_utf8(),
            "https://example.com/script.js",
            "the code reports the filename without a display filename"
        );
    }

    #[test]
    fn syntax_errors_go_to_the_sink_with_lines_counted_from_the_offset() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let script = parse(
            &vm,
            realm,
            "let a = 1;\nlet b = ;",
            "",
            core::ptr::null_mut(),
            10,
            &mut errors,
        );
        assert!(script.is_null());
        assert_eq!(errors.errors.len(), 1);
        let (message, line, column) = &errors.errors[0];
        assert!(!message.is_empty());
        assert_eq!((*line, *column), (11, 9));
        assert_eq!(errors.results_of_scripts_run_while_appending, ["2,4"]);

        // SAFETY: The VM and realm are live, and a null sink drops the errors.
        let script = unsafe {
            js_script_parse(
                vm_into_abi(&vm),
                cell_into_abi(realm),
                ascii_view_of("("),
                ascii_view_of(""),
                ascii_view_of(""),
                core::ptr::null_mut(),
                1,
                core::ptr::null(),
            )
        };
        assert!(script.is_null());
    }

    #[test]
    fn an_overriding_lexical_environment_is_seen_by_typeof_but_not_by_global_accesses() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        run_script(&vm, realm, "var shadowed = 1").must();
        let environment = DeclarativeEnvironment::create(&vm, Some(realm.global_environment().upcast()));
        let name = ak::Utf16FlyString::from_utf8("shadowed");
        environment.create_mutable_binding(&vm, &name, false).must();
        environment
            .initialize_binding(
                &vm,
                &name,
                Value::from_string(PrimitiveString::create_from_utf8(&vm, "override")),
                InitializeBindingHint::Normal,
            )
            .must();

        let mut errors = ReenteringErrorCollector::new(&vm, realm);
        let script = parse(
            &vm,
            realm,
            "[typeof shadowed, String(shadowed), eval('shadowed')].join()",
            "",
            core::ptr::null_mut(),
            1,
            &mut errors,
        );
        // SAFETY: The VM, script and environment are live.
        let completion = unsafe { js_script_run(vm_into_abi(&vm), script, environment_to_abi(environment)) };
        assert!(completion.variant == JS_COMPLETION_NORMAL);
        assert_eq!(utf8(Value(completion.payload)), "string,1,override");
    }
}
