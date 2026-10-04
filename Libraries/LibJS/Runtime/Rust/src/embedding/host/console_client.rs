/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! A console client that the embedder implements: the C++ subclasses of ConsoleClient, such as WebContent's
//! DevToolsConsoleClient, become a HostConsoleClient with a table of C methods and a C++ GC cell as their context.
//!
//! Everything here runs on the thread that owns the VM.

use core::ffi::c_void;
use core::ptr::NonNull;

use libjs_runtime_macros::Trace;

use crate::console::{ConsoleClient, ConsoleClientMethods, LogLevel, PrinterArguments, Trace as ConsoleTrace};
use crate::embedding::abi_types::{
    JSErrorData, JSOwnedUtf16String, JSUtf16View, completion_from_abi, completion_writing_result_to,
    error_data_into_abi, owned_utf16_string_into_abi, vm_from_abi, vm_into_abi,
};
use crate::embedding::console::{JSConsole, log_level_into_abi};
use crate::gc::class::{GcCell, class_of, define_cell};
use crate::gc::foreign::ForeignCellSlot;
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSVM, JSValue};
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::error_data::ErrorData;
use crate::utf16::Utf16View;

/// A ConsoleClient, whichever class implements it.
pub struct JSConsoleClient {
    _opaque: [u8; 0],
}

// The alternative of ConsoleClient::PrinterArguments that a printer receives, numbered like the Variant's types.
pub const JS_CONSOLE_PRINTER_ARGUMENTS_GROUP: u8 = 0;
pub const JS_CONSOLE_PRINTER_ARGUMENTS_TRACE: u8 = 1;
pub const JS_CONSOLE_PRINTER_ARGUMENTS_VALUES: u8 = 2;

/// Console::TraceFrame. The source file, line and column are only meaningful when their flags say they are present.
#[repr(C)]
pub struct JSConsoleTraceFrame {
    pub function_name: JSUtf16View,
    pub source_file: JSUtf16View,
    pub line: usize,
    pub column: usize,
    pub has_source_file: bool,
    pub has_line: bool,
    pub has_column: bool,
}

/// ConsoleClient::PrinterArguments, which lives for the duration of the printer call. A group has a label, a trace a
/// label and its frames, from the caller of console.trace() outwards, and values have values, which stay alive for the
/// call.
#[repr(C)]
pub struct JSConsolePrinterArguments {
    pub kind: u8,
    pub label: JSUtf16View,
    pub trace_frames: *const JSConsoleTraceFrame,
    pub trace_frame_count: usize,
    pub values: *const JSValue,
    pub value_count: usize,
}

/// The virtual methods of a ConsoleClient that the embedder implements. Each receives the context of the client, and
/// may run JavaScript. A null method does what the C++ base class does: nothing, and a printer that prints nothing
/// returns undefined.
#[repr(C)]
pub struct JSConsoleClientMethods {
    /// Printer(logLevel, args): returns a completion with a value, or throws what console method threw.
    pub printer: Option<
        unsafe extern "C" fn(
            context: *mut c_void,
            vm: *mut JSVM,
            log_level: u8,
            arguments: *const JSConsolePrinterArguments,
        ) -> JSCompletion,
    >,
    /// The CSS style of a %c directive, which applies to the rest of the message being formatted.
    pub add_css_style_to_current_message: Option<unsafe extern "C" fn(context: *mut c_void, style: JSUtf16View)>,
    /// An uncaught exception, with its name, message and the error data of the error object, and whether a promise
    /// rejected with it.
    pub report_exception: Option<
        unsafe extern "C" fn(
            context: *mut c_void,
            name: JSUtf16View,
            message: JSUtf16View,
            error_data: *const JSErrorData,
            in_promise: bool,
        ),
    >,
    pub clear: Option<unsafe extern "C" fn(context: *mut c_void)>,
    pub end_group: Option<unsafe extern "C" fn(context: *mut c_void)>,
}

/// A ConsoleClient whose virtual methods are the embedder's C methods. It keeps its context, a C++ GC cell, alive.
#[repr(C)]
#[derive(Trace)]
pub struct HostConsoleClient {
    base: ConsoleClient,
    #[gc(untraced)]
    host_methods: &'static JSConsoleClientMethods,
    context: ForeignCellSlot,
}

define_cell!(HostConsoleClient, Other, extends: [ConsoleClient]);

static HOST_CONSOLE_CLIENT_METHODS: ConsoleClientMethods = ConsoleClientMethods {
    printer: HostConsoleClient::printer,
    add_css_style_to_current_message: HostConsoleClient::add_css_style_to_current_message,
    report_exception: HostConsoleClient::report_exception,
    clear: HostConsoleClient::clear,
    end_group: HostConsoleClient::end_group,
};

/// What a trace frame's strings are while the printer runs.
fn trace_frames_into_abi(trace: &ConsoleTrace) -> Vec<JSConsoleTraceFrame> {
    trace
        .stack
        .iter()
        .map(|frame| JSConsoleTraceFrame {
            function_name: JSUtf16View::of(Utf16View::of_string(&frame.function_name)),
            source_file: JSUtf16View::of(
                frame
                    .source_file
                    .as_ref()
                    .map_or(Utf16View::EMPTY, Utf16View::of_string),
            ),
            line: frame.line.unwrap_or(0),
            column: frame.column.unwrap_or(0),
            has_source_file: frame.source_file.is_some(),
            has_line: frame.line.is_some(),
            has_column: frame.column.is_some(),
        })
        .collect()
}

impl HostConsoleClient {
    fn of(client: &ConsoleClient) -> &HostConsoleClient {
        // SAFETY: Only HostConsoleClient has these methods.
        unsafe { &*core::ptr::from_ref(client).cast::<HostConsoleClient>() }
    }

    fn printer<'vm>(
        client: &ConsoleClient,
        vm: &'vm Vm,
        log_level: LogLevel,
        arguments: PrinterArguments<'vm>,
    ) -> ThrowCompletionOr<Value> {
        let this = Self::of(client);
        let Some(printer) = this.host_methods.printer else {
            return Ok(Value::UNDEFINED);
        };

        // The views point into `arguments` and these copies, which all live until the printer returns. The values
        // stay alive in the MarkedVec of `arguments`.
        let trace_frames;
        let values;
        let printer_arguments = match &arguments {
            PrinterArguments::Group(group) => JSConsolePrinterArguments {
                kind: JS_CONSOLE_PRINTER_ARGUMENTS_GROUP,
                label: JSUtf16View::of(Utf16View::of_string(&group.label)),
                trace_frames: core::ptr::null(),
                trace_frame_count: 0,
                values: core::ptr::null(),
                value_count: 0,
            },
            PrinterArguments::Trace(trace) => {
                trace_frames = trace_frames_into_abi(trace);
                JSConsolePrinterArguments {
                    kind: JS_CONSOLE_PRINTER_ARGUMENTS_TRACE,
                    label: JSUtf16View::of(Utf16View::of_string(&trace.label)),
                    trace_frames: trace_frames.as_ptr(),
                    trace_frame_count: trace_frames.len(),
                    values: core::ptr::null(),
                    value_count: 0,
                }
            }
            PrinterArguments::Values(marked_values) => {
                values = marked_values.to_vec();
                JSConsolePrinterArguments {
                    kind: JS_CONSOLE_PRINTER_ARGUMENTS_VALUES,
                    label: JSUtf16View::of(Utf16View::EMPTY),
                    trace_frames: core::ptr::null(),
                    trace_frame_count: 0,
                    values: values.as_ptr().cast::<JSValue>(),
                    value_count: values.len(),
                }
            }
        };

        // SAFETY: The embedder's printer receives the context it gave the client, and arguments whose strings and
        //         values outlive the call.
        let completion = unsafe {
            printer(
                this.context.as_ptr(),
                vm_into_abi(vm),
                log_level_into_abi(log_level),
                &raw const printer_arguments,
            )
        };
        completion_from_abi(completion)
    }

    fn add_css_style_to_current_message(client: &ConsoleClient, style: Utf16View<'_>) {
        let this = Self::of(client);
        if let Some(add_css_style_to_current_message) = this.host_methods.add_css_style_to_current_message {
            // SAFETY: The embedder's method receives the context it gave the client, and a view that outlives the
            //         call.
            unsafe { add_css_style_to_current_message(this.context.as_ptr(), JSUtf16View::of(style)) };
        }
    }

    fn report_exception(
        client: &ConsoleClient,
        name: Utf16View<'_>,
        message: Utf16View<'_>,
        error_data: &ErrorData,
        in_promise: bool,
    ) {
        let this = Self::of(client);
        if let Some(report_exception) = this.host_methods.report_exception {
            // SAFETY: The embedder's method receives the context it gave the client, and views and error data that
            //         outlive the call.
            unsafe {
                report_exception(
                    this.context.as_ptr(),
                    JSUtf16View::of(name),
                    JSUtf16View::of(message),
                    error_data_into_abi(error_data),
                    in_promise,
                );
            }
        }
    }

    fn clear(client: &ConsoleClient) {
        let this = Self::of(client);
        if let Some(clear) = this.host_methods.clear {
            // SAFETY: The embedder's method receives the context it gave the client.
            unsafe { clear(this.context.as_ptr()) };
        }
    }

    fn end_group(client: &ConsoleClient) {
        let this = Self::of(client);
        if let Some(end_group) = this.host_methods.end_group {
            // SAFETY: The embedder's method receives the context it gave the client.
            unsafe { end_group(this.context.as_ptr()) };
        }
    }
}

/// # Safety
///
/// `client` must point to a live console client.
unsafe fn console_client_from_abi(client: *mut JSConsoleClient) -> Gc<ConsoleClient> {
    let client = NonNull::new(client.cast::<ConsoleClient>()).expect("a console client is not null");
    // SAFETY: The caller guarantees that the pointer is to a live cell, whose class the assertion checks.
    let client = unsafe { Gc::from_non_null(client) };
    assert!(class_of(client).is_subclass_of(ConsoleClient::CLASS));
    client
}

/// Creates a console client of `console` whose virtual methods are `methods`, which are called with `context`. The
/// client keeps `context` alive. Install it with js_console_set_client(); until something holds it, the caller must
/// keep it alive, as it would any other cell. Only on the VM's thread.
///
/// # Safety
///
/// `console` must point to a live console, `methods` to a table that outlives every client created with it, which is
/// static data in practice, and `context` must be null or a live C++ GC cell of the VM's heap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_console_client_create(
    vm: *mut JSVM,
    console: *mut JSConsole,
    methods: *const JSConsoleClientMethods,
    context: *mut c_void,
) -> *mut JSConsoleClient {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    let console = NonNull::new(console.cast()).expect("a console is not null");
    // SAFETY: The caller passes a live console.
    let console = unsafe { Gc::from_non_null(console) };
    // SAFETY: The caller guarantees that the table outlives every client created with it.
    let host_methods = unsafe { methods.as_ref() }.expect("a console client has methods");
    let client = vm.heap().allocate(HostConsoleClient {
        base: ConsoleClient::new(HostConsoleClient::CLASS, &HOST_CONSOLE_CLIENT_METHODS, console),
        host_methods,
        context: ForeignCellSlot::empty(),
    });
    // SAFETY: The caller passes null or a live cell of the heap, which the slot keeps alive from now on.
    unsafe { client.context.set(NonNull::new(context)) };
    client.as_ptr().cast()
}

/// ConsoleClient::generically_format_values(): formats the values the way a console prints them, separated by
/// spaces. On a normal completion, `*out_formatted` receives the raw word of an AK::Utf16String the caller owns, and
/// on a throw completion it is left alone. Only on the VM's thread.
///
/// # Safety
///
/// `client` must point to a live console client, `values` to `value_count` values, and `out_formatted` to writable
/// storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_console_client_generically_format_values(
    vm: *mut JSVM,
    client: *mut JSConsoleClient,
    values: *const JSValue,
    value_count: usize,
    out_formatted: *mut JSOwnedUtf16String,
) -> JSCompletion {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a live console client.
    let client = unsafe { console_client_from_abi(client) };
    let marked_values = MarkedVec::with_capacity(vm, value_count);
    if value_count > 0 {
        // SAFETY: The caller guarantees that `values` points to `value_count` values.
        for &value in unsafe { core::slice::from_raw_parts(values, value_count) } {
            marked_values.push(Value(value));
        }
    }
    let formatted = client
        .generically_format_values(vm, &marked_values)
        .map(owned_utf16_string_into_abi);
    // SAFETY: The caller provides writable storage for the string.
    unsafe { completion_writing_result_to(formatted, out_formatted) }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::{Cell, RefCell};

    use super::*;
    use crate::embedding::abi_types::completion_into_abi;
    use crate::embedding::abi_types::error_data_from_abi;
    use crate::embedding::console::{
        js_console_object_console, js_console_report_exception, js_console_set_client, log_level_from_abi,
    };
    use crate::gc::root::Root;
    use crate::gc::weak::GcWeak;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::layout::realm::Realm;
    use crate::runtime::object::Object;
    use crate::script::Script;
    use crate::utilities::initialize_realm;

    /// The console client a test installed, which the recording methods format values with and run scripts through.
    #[derive(Clone, Copy)]
    struct RecordingClient {
        vm: *const Vm,
        realm: Gc<Realm>,
        client: *mut JSConsoleClient,
        context: *mut c_void,
    }

    std::thread_local! {
        static RECORDING_CLIENT: Cell<Option<RecordingClient>> = const { Cell::new(None) };
        static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Scripts a method runs the first time it is called, by the method's name.
        static SCRIPTS_TO_RUN_FROM_METHODS: RefCell<Vec<(&'static str, &'static str)>> =
            const { RefCell::new(Vec::new()) };
    }

    fn recording_client() -> RecordingClient {
        RECORDING_CLIENT.get().expect("the test installed a recording client")
    }

    fn run_script(vm: &Vm, realm: Gc<Realm>, source: &str) -> ThrowCompletionOr<Value> {
        let source: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse_with_filename(vm, &source, realm, "console.js").expect("the script parses");
        vm.run_script(script, None)
    }

    /// Records what a method was called with, checks its context, and runs the script the test gave the method.
    fn record(method: &'static str, context: *mut c_void, event: String) {
        let recording_client = recording_client();
        assert_eq!(context, recording_client.context);
        EVENTS.with_borrow_mut(|events| events.push(event));
        let script = SCRIPTS_TO_RUN_FROM_METHODS.with_borrow_mut(|scripts| {
            let index = scripts.iter().position(|(name, _)| *name == method)?;
            Some(scripts.remove(index).1)
        });
        if let Some(script) = script {
            // SAFETY: The VM outlives the client the test installed.
            let vm = unsafe { &*recording_client.vm };
            assert!(run_script(vm, recording_client.realm, script).is_ok());
        }
    }

    fn string_of(view: JSUtf16View) -> String {
        // SAFETY: Every view a method receives is valid for the call.
        unsafe { view.as_view() }.to_utf8()
    }

    fn describe_trace_frame(frame: &JSConsoleTraceFrame) -> String {
        let mut description = string_of(frame.function_name);
        if frame.has_source_file {
            description.push_str(&format!(" at {}", string_of(frame.source_file)));
        }
        if frame.has_line && frame.has_column {
            description.push_str(&format!(":{}:{}", frame.line, frame.column));
        }
        description
    }

    unsafe extern "C" fn record_printer(
        context: *mut c_void,
        vm: *mut JSVM,
        log_level: u8,
        arguments: *const JSConsolePrinterArguments,
    ) -> JSCompletion {
        // SAFETY: The client passes arguments that are valid for the call.
        let arguments = unsafe { &*arguments };
        let log_level = log_level_from_abi(log_level);
        let event = match arguments.kind {
            JS_CONSOLE_PRINTER_ARGUMENTS_GROUP => format!("{log_level:?}: {}", string_of(arguments.label)),
            JS_CONSOLE_PRINTER_ARGUMENTS_TRACE => {
                // SAFETY: The client passes that many frames.
                let frames =
                    unsafe { core::slice::from_raw_parts(arguments.trace_frames, arguments.trace_frame_count) };
                let frames: Vec<String> = frames.iter().map(describe_trace_frame).collect();
                format!("{log_level:?}: {} | {}", string_of(arguments.label), frames.join(" | "))
            }
            JS_CONSOLE_PRINTER_ARGUMENTS_VALUES => {
                let mut formatted = 0;
                // SAFETY: Formats the values the printer received, which re-enters the VM from within the printer.
                let completion = unsafe {
                    js_console_client_generically_format_values(
                        vm,
                        recording_client().client,
                        arguments.values,
                        arguments.value_count,
                        &raw mut formatted,
                    )
                };
                if completion.variant != JS_COMPLETION_NORMAL {
                    return completion;
                }
                // SAFETY: A normal completion transfers an AK::Utf16String to the caller.
                let formatted = unsafe { ak::Utf16String::from_raw_owned(formatted) };
                format!("{log_level:?}: {}", Utf16View::of_string(&formatted).to_utf8())
            }
            kind => panic!("unknown printer arguments {kind}"),
        };
        let should_throw = event.contains("throw from the printer");
        record("printer", context, event);
        if should_throw {
            return JSCompletion {
                payload: Value::from_i32(7).0,
                variant: JS_COMPLETION_THROW,
            };
        }
        completion_into_abi(Ok(()))
    }

    unsafe extern "C" fn record_css_style(context: *mut c_void, style: JSUtf16View) {
        record("css", context, format!("css: {}", string_of(style)));
    }

    unsafe extern "C" fn record_exception(
        context: *mut c_void,
        name: JSUtf16View,
        message: JSUtf16View,
        error_data: *const JSErrorData,
        in_promise: bool,
    ) {
        // SAFETY: The client passes the error data of a live error.
        let error_data = unsafe { error_data_from_abi(error_data) };
        record(
            "exception",
            context,
            format!(
                "exception: {}: {} (in promise: {in_promise}, has traceback: {})",
                string_of(name),
                string_of(message),
                !error_data.traceback().is_empty()
            ),
        );
    }

    unsafe extern "C" fn record_clear(context: *mut c_void) {
        record("clear", context, "clear".to_owned());
    }

    unsafe extern "C" fn record_end_group(context: *mut c_void) {
        record("end group", context, "end group".to_owned());
    }

    static RECORDING_METHODS: JSConsoleClientMethods = JSConsoleClientMethods {
        printer: Some(record_printer),
        add_css_style_to_current_message: Some(record_css_style),
        report_exception: Some(record_exception),
        clear: Some(record_clear),
        end_group: Some(record_end_group),
    };

    fn console_of(vm: &Vm, realm: Gc<Realm>) -> *mut JSConsole {
        let console_object = realm.intrinsics().console_object(vm);
        // SAFETY: The console object is live.
        let console = unsafe { js_console_object_console(console_object.as_ptr().cast()) };
        assert!(!console.is_null());
        console
    }

    fn install_recording_client(vm: &Vm, realm: Gc<Realm>) {
        let console = console_of(vm, realm);
        // SAFETY: The console is live, the methods are static, and a null context is allowed.
        let client = unsafe {
            js_console_client_create(
                vm_into_abi(vm),
                console,
                &raw const RECORDING_METHODS,
                core::ptr::null_mut(),
            )
        };
        // SAFETY: Both are live.
        unsafe { js_console_set_client(console, client) };
        RECORDING_CLIENT.set(Some(RecordingClient {
            vm,
            realm,
            client,
            context: core::ptr::null_mut(),
        }));
    }

    fn report_exception(vm: &Vm, realm: Gc<Realm>, error: Gc<Object>, name: &str, in_promise: bool) {
        let message = error
            .get(
                vm,
                &crate::runtime::property_key::PropertyKey::from(ak::Utf16FlyString::from_utf8("message")),
            )
            .and_then(|message| message.to_utf16_string(vm))
            .expect("the error has a message");
        let name = ak::Utf16String::from_utf8(name);
        // SAFETY: The console and the error are live, and the views outlive the call.
        unsafe {
            js_console_report_exception(
                console_of(vm, realm),
                JSUtf16View::of(Utf16View::of_string(&name)),
                JSUtf16View::of(Utf16View::of_string(&message)),
                error_data_into_abi(error.error_data().expect("an error has error data")),
                in_promise,
            );
        }
    }

    fn run_from_method(method: &'static str, script: &'static str) {
        SCRIPTS_TO_RUN_FROM_METHODS.with_borrow_mut(|scripts| scripts.push((method, script)));
    }

    #[test]
    fn a_host_console_client_receives_what_the_console_logs() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        install_recording_client(&vm, realm);

        let source = "console.log('answer', 42);\nconsole.warn({ a: 1 });\nconsole.group('group %s', 'label');\n\
                      console.groupEnd();\nconsole.clear();\nconsole.log('%cstyled', 'color: red');";
        assert!(run_script(&vm, realm, source).is_ok());
        assert_eq!(
            EVENTS.take(),
            [
                "Log: \"answer\" 42",
                "Warn: Object{ \"a\": 1 }",
                "Group: group label",
                "end group",
                "clear",
                "css: color: red",
                "Log: \"styled\"",
            ]
        );
    }

    #[test]
    fn a_host_console_client_receives_traces() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        install_recording_client(&vm, realm);

        let source = "function traced() {\n    console.trace('label %d', 5);\n}\ntraced();";
        assert!(run_script(&vm, realm, source).is_ok());
        assert_eq!(
            EVENTS.take(),
            ["Trace: label 5 | traced at console.js:2:18 | <anonymous> at console.js:4:7 | <anonymous>"]
        );
    }

    #[test]
    fn a_throwing_printer_throws_from_the_console_method() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        install_recording_client(&vm, realm);

        let source = "try { console.log('throw from the printer'); 0; } catch (error) { error; }";
        assert_eq!(run_script(&vm, realm, source).ok(), Some(Value::from_i32(7)));
        assert_eq!(EVENTS.take(), ["Log: \"throw from the printer\""]);
    }

    #[test]
    fn a_host_console_client_reports_exceptions_with_their_error_data() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        install_recording_client(&vm, realm);

        let error = run_script(&vm, realm, "function fail() { return new TypeError('boom'); } fail();")
            .expect("the script returns an error")
            .as_object();
        report_exception(&vm, realm, error, "TypeError", true);
        assert_eq!(
            EVENTS.take(),
            ["exception: TypeError: boom (in promise: true, has traceback: true)"]
        );
    }

    #[test]
    fn every_method_can_run_javascript_that_logs_again() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        install_recording_client(&vm, realm);

        run_from_method("printer", "console.log('from the printer');");
        run_from_method("css", "console.log('from the css style');");
        run_from_method("clear", "console.count();");
        run_from_method("end group", "console.group('again');");
        run_from_method("exception", "console.log('from the exception');");

        let source = "console.log('first');\nconsole.log('%cstyled', 'color: red');\nconsole.clear();\n\
                      console.group('group');\nconsole.groupEnd();\nconsole.groupEnd();";
        assert!(run_script(&vm, realm, source).is_ok());
        let error = run_script(&vm, realm, "new Error('boom');")
            .expect("the script returns an error")
            .as_object();
        report_exception(&vm, realm, error, "Error", false);

        assert_eq!(
            EVENTS.take(),
            [
                "Log: \"first\"",
                "Log: \"from the printer\"",
                "css: color: red",
                "Log: \"from the css style\"",
                "Log: \"styled\"",
                "clear",
                "Count: \"default: 1\"",
                "Group: group",
                "end group",
                "Group: again",
                "end group",
                "exception: Error: boom (in promise: false, has traceback: true)",
                "Log: \"from the exception\"",
            ]
        );
    }

    #[test]
    fn a_client_without_methods_ignores_what_the_console_logs() {
        static NO_METHODS: JSConsoleClientMethods = JSConsoleClientMethods {
            printer: None,
            add_css_style_to_current_message: None,
            report_exception: None,
            clear: None,
            end_group: None,
        };

        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let console = console_of(&vm, realm);
        // SAFETY: The console is live, the methods are static, and a null context is allowed.
        unsafe {
            js_console_set_client(
                console,
                js_console_client_create(vm_into_abi(&vm), console, &raw const NO_METHODS, core::ptr::null_mut()),
            );
        }
        let source = "console.log('%cstyled', 'color: red'); console.group(); console.groupEnd(); console.clear(); \
                      console.trace(); 42;";
        assert_eq!(run_script(&vm, realm, source).ok(), Some(Value::from_i32(42)));
    }

    const CLIENT_COUNT: usize = 32;

    struct ClientsWithContexts<'vm> {
        clients: Root<'vm, Vec<ForeignCellSlot>>,
        contexts: Vec<GcWeak<Object>>,
        objects_nothing_holds: Vec<GcWeak<Object>>,
    }

    /// Clients whose contexts only they hold, and as many objects that nothing holds.
    #[inline(never)]
    fn create_clients_with_contexts(vm: &Vm, realm: Gc<Realm>) -> ClientsWithContexts<'_> {
        let console = console_of(vm, realm);
        let clients = Root::new(
            vm,
            (0..CLIENT_COUNT).map(|_| ForeignCellSlot::empty()).collect::<Vec<_>>(),
        );
        let mut contexts = Vec::new();
        let mut objects_nothing_holds = Vec::new();
        for slot in clients.get() {
            let context = Object::create(vm, realm, None);
            contexts.push(GcWeak::new(vm.heap(), context));
            objects_nothing_holds.push(GcWeak::new(vm.heap(), Object::create(vm, realm, None)));
            // SAFETY: The console is live, the methods are static, and the context is a live cell.
            let client = unsafe {
                js_console_client_create(
                    vm_into_abi(vm),
                    console,
                    &raw const RECORDING_METHODS,
                    context.as_ptr().cast(),
                )
            };
            // SAFETY: The client is a live cell of the VM's heap.
            unsafe { slot.set(NonNull::new(client.cast())) };
        }
        ClientsWithContexts {
            clients,
            contexts,
            objects_nothing_holds,
        }
    }

    #[test]
    fn a_client_keeps_its_context_alive() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let clients_with_contexts = create_clients_with_contexts(&vm, realm);
        vm.heap().collect_garbage();

        let dead_objects = clients_with_contexts
            .objects_nothing_holds
            .iter()
            .filter(|object| object.get().is_none())
            .count();
        assert!(
            dead_objects >= CLIENT_COUNT / 2,
            "only {dead_objects} of the objects nothing holds were collected"
        );
        assert!(
            clients_with_contexts
                .contexts
                .iter()
                .all(|context| context.get().is_some())
        );

        let client = clients_with_contexts
            .clients
            .get()
            .last()
            .expect("there are clients")
            .as_ptr()
            .cast::<JSConsoleClient>();
        let context = clients_with_contexts
            .contexts
            .last()
            .and_then(GcWeak::get)
            .expect("the context is alive");
        // SAFETY: The console and the client are live.
        unsafe { js_console_set_client(console_of(&vm, realm), client) };
        RECORDING_CLIENT.set(Some(RecordingClient {
            vm: &raw const *vm,
            realm,
            client,
            context: context.as_ptr().cast(),
        }));
        assert!(run_script(&vm, realm, "console.log('to the last client');").is_ok());
        assert_eq!(EVENTS.take(), ["Log: \"to the last client\""]);
    }
}
