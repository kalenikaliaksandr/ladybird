/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Logging every exception where it is thrown, which WebContent's --log-all-js-exceptions turns on through
//! JS::set_log_all_js_exceptions().
//!
//! The runtime logs where the C++ runtime calls JS::throw_completion(): when it throws an error of its own, when an
//! exception leaves the bytecode it runs, and where it turns a value it did not throw into a throw completion, such as
//! the rejection that resumes an async function or that a promise reaction without a handler passes on. The embedder
//! logs the throws it starts itself with js_completion_log_exception. The log is that of the C++ runtime: "THROW!" and
//! the thrown value, or the "message" of a thrown object followed by the call stack, one line per execution context
//! from the running one down.

use core::cell::Cell;
use core::ffi::c_void;
use core::ops::ControlFlow;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::bytecode::executable::Executable;
use crate::embedding::abi_types::{JSByteSink, value_from_abi, vm_from_abi};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSVM, JSValue};
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::utf16::Utf16View;

static LOG_ALL_EXCEPTIONS: AtomicBool = AtomicBool::new(false);

std::thread_local! {
    static IS_READING_MESSAGE_OF_THROWN_OBJECT: Cell<bool> = const { Cell::new(false) };
}

/// Where the lines of the log go: the embedder's writer, or the standard error without one.
static EXCEPTION_LOG_LINE_WRITER: Mutex<Option<ExceptionLogLineWriter>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct ExceptionLogLineWriter {
    context: *mut c_void,
    append: unsafe extern "C" fn(context: *mut c_void, bytes: *const u8, length: usize) -> bool,
}

// SAFETY: Every thread that throws writes to the one writer the embedder installed for the process, as the C++
//         runtime's dbgln() is shared by every thread.
unsafe impl Send for ExceptionLogLineWriter {}

/// Turns logging every exception where it is thrown on or off for the whole process. Each line of the log goes to
/// `line_writer`, without a line terminator, or to the standard error when `line_writer` is null. The writer is copied,
/// and its context must stay valid until logging is turned off or another writer replaces it.
///
/// # Safety
///
/// `line_writer` must be null or point to a sink with an append function that any thread may call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_completion_set_log_all_exceptions(enabled: bool, line_writer: *const JSByteSink) {
    // SAFETY: The caller passes null or a readable sink.
    let line_writer = unsafe { line_writer.as_ref() }.map(|sink| ExceptionLogLineWriter {
        context: sink.context,
        append: sink.append.expect("a byte sink has an append function"),
    });
    *EXCEPTION_LOG_LINE_WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = line_writer;
    LOG_ALL_EXCEPTIONS.store(enabled, Ordering::Relaxed);
}

/// Logs `value` as a thrown exception, if exceptions are logged, for a throw that the embedder starts itself, as C++
/// code does with JS::throw_completion(). Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM and `value` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_completion_log_exception(vm: *mut JSVM, value: JSValue) {
    // SAFETY: The caller passes its VM.
    log_exception_if_enabled(unsafe { vm_from_abi(vm) }, value_from_abi(value));
}

/// Where the runtime throws, logs the thrown value if exceptions are logged.
#[inline]
pub fn log_exception_if_enabled(vm: &Vm, value: Value) {
    if LOG_ALL_EXCEPTIONS.load(Ordering::Relaxed) {
        log_exception(vm, value);
    }
}

#[cold]
#[inline(never)]
fn log_exception(vm: &Vm, value: Value) {
    let lines = exception_log_lines(vm, value);
    let line_writer = EXCEPTION_LOG_LINE_WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for line in &lines {
        match *line_writer {
            // SAFETY: The embedder's writer takes bytes that outlive the call, with the context it came with.
            Some(writer) => unsafe {
                (writer.append)(writer.context, line.as_ptr(), line.len());
            },
            None => eprintln!("{line}"),
        }
    }
}

// The C++ runtime reads the message with [[Get]], which runs getters such as DOMException's. Where the getter throws,
// C++ crashes; here the message shows as the object stores it. Throws while the message is read are logged too, but
// read no message with a getter, so that a getter that throws its own object cannot recurse.
fn message_of_thrown_object(vm: &Vm, object: Gc<Object>) -> Value {
    let stored_message = || object.get_without_side_effects(vm, &vm.names.message);
    if IS_READING_MESSAGE_OF_THROWN_OBJECT.get() {
        return stored_message();
    }
    IS_READING_MESSAGE_OF_THROWN_OBJECT.set(true);
    let message = object.get(vm, &vm.names.message);
    IS_READING_MESSAGE_OF_THROWN_OBJECT.set(false);
    message.unwrap_or_else(|_| stored_message())
}

// What the C++ runtime's log_exception() and VM::dump_backtrace() print.
fn exception_log_lines(vm: &Vm, value: Value) -> Vec<String> {
    let throw_line = |shown_value: Value| {
        format!(
            "\x1b[31;1mTHROW!\x1b[0m {}",
            Utf16View::of_string(&shown_value.to_utf16_string_without_side_effects()).to_utf8()
        )
    };

    if !value.is_object() {
        return vec![throw_line(value)];
    }

    let mut lines = vec![throw_line(message_of_thrown_object(vm, value.as_object()))];
    vm.for_each_execution_context_top_to_bottom(|context| {
        let function_name = context
            .function
            .get()
            .map(|function| function.name_for_call_stack())
            .unwrap_or_default();
        let function_name = Utf16View::of_string(&function_name).to_utf8();
        let source_range = context
            .executable
            .get()
            .and_then(|executable| Executable::from_head(executable).source_range_at(context.program_counter.get()));
        lines.push(match source_range {
            Some(source_range) => format!(
                "-> {function_name} @ {}:{},{}",
                Utf16View::of_string(source_range.filename()).to_utf8(),
                source_range.start.line,
                source_range.start.column
            ),
            None => format!("-> {function_name}"),
        });
        ControlFlow::Continue(())
    });
    lines
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::vm_into_abi;
    use crate::runtime::error::test_scripts::run_script;
    use crate::utilities::initialize_realm;

    // Logging is process-wide and other tests throw on other threads while it is on, so the test looks only at the
    // logs of its own throws, whose messages it marks.
    static LOGGED_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe extern "C" fn collect_line(_: *mut c_void, bytes: *const u8, length: usize) -> bool {
        // SAFETY: The log passes `length` bytes of UTF-8.
        let line = String::from_utf8(unsafe { core::slice::from_raw_parts(bytes, length) }.to_vec())
            .expect("the log writes UTF-8");
        LOGGED_LINES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line);
        true
    }

    /// The logs of the throws whose "THROW!" line contains `marker`, each with the lines of its call stack.
    fn logs_of_throws_marked(marker: &str) -> Vec<Vec<String>> {
        let lines = LOGGED_LINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut logs: Vec<Vec<String>> = Vec::new();
        for line in lines.iter() {
            if line.contains("THROW!") {
                logs.push(Vec::new());
            }
            if let Some(log) = logs.last_mut() {
                log.push(line.clone());
            }
        }
        logs.retain(|log| log[0].contains(marker));
        logs
    }

    /// The logs of the throws with a line that contains `marker`, such as the name of a function on the call stack.
    fn logs_mentioning(marker: &str) -> Vec<Vec<String>> {
        let mut logs = logs_of_throws_marked("");
        logs.retain(|log| log.iter().any(|line| line.contains(marker)));
        logs
    }

    #[test]
    fn logs_exceptions_where_they_are_thrown() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let writer = JSByteSink {
            context: core::ptr::null_mut(),
            append: Some(collect_line),
        };

        // SAFETY: The writer is a function that any thread may call, with a context it does not use.
        unsafe { js_completion_set_log_all_exceptions(true, &raw const writer) };
        let thrown = run_script(
            &vm,
            realm,
            "function thrower() { throw new TypeError('first logged failure'); } thrower();",
        );
        let runtime_error = run_script(&vm, realm, "null.second_logged_failure");
        let thrown_primitive = run_script(&vm, realm, "throw 'third logged failure'");
        // SAFETY: The VM is live, and the value is one of its strings.
        unsafe {
            js_completion_log_exception(
                vm_into_abi(&vm),
                run_script(&vm, realm, "'fourth logged failure'")
                    .expect("the script runs")
                    .0,
            );
        }
        let message_from_getter = run_script(
            &vm,
            realm,
            "throw { get message() { return 'fifth logged failure'; } };",
        );
        let message_getter_throws = run_script(
            &vm,
            realm,
            "function sixth_logged_failure() { throw { get message() { throw 'getter of the sixth'; } }; }
             sixth_logged_failure();",
        );
        let message_getter_throws_its_object = run_script(
            &vm,
            realm,
            "function seventh_logged_failure() { var thrown = { get message() { throw thrown; } }; throw thrown; }
             seventh_logged_failure();",
        );
        // SAFETY: A null writer is allowed.
        unsafe { js_completion_set_log_all_exceptions(false, core::ptr::null()) };
        let unlogged = run_script(&vm, realm, "throw 'unlogged failure'");
        assert!(thrown.is_err() && runtime_error.is_err() && thrown_primitive.is_err() && unlogged.is_err());
        assert!(
            message_from_getter.is_err() && message_getter_throws.is_err() && message_getter_throws_its_object.is_err()
        );

        // A thrown object shows its message and the call stack, each time it leaves the bytecode of a function or a
        // script.
        assert_eq!(
            logs_of_throws_marked("first logged failure"),
            [
                vec![
                    "\x1b[31;1mTHROW!\x1b[0m first logged failure",
                    "-> thrower @ eval:1,22",
                    "->  @ eval:1,76",
                    "-> "
                ],
                vec!["\x1b[31;1mTHROW!\x1b[0m first logged failure", "->  @ eval:1,76", "-> "],
            ]
        );

        // The runtime's own errors are logged when they are created as well.
        let runtime_error_log = [
            "\x1b[31;1mTHROW!\x1b[0m Cannot access property \"second_logged_failure\" on null object",
            "->  @ eval:1,5",
            "-> ",
        ];
        assert_eq!(
            logs_of_throws_marked("second_logged_failure"),
            [runtime_error_log, runtime_error_log]
        );

        // Other thrown values show without a call stack.
        assert_eq!(
            logs_of_throws_marked("third logged failure"),
            [["\x1b[31;1mTHROW!\x1b[0m third logged failure"]]
        );
        assert_eq!(
            logs_of_throws_marked("fourth logged failure"),
            [["\x1b[31;1mTHROW!\x1b[0m fourth logged failure"]]
        );
        assert!(logs_of_throws_marked("unlogged failure").is_empty());

        // The message of a thrown object comes from its getter, as in the C++ runtime.
        assert_eq!(
            logs_of_throws_marked("fifth logged failure"),
            [["\x1b[31;1mTHROW!\x1b[0m fifth logged failure", "->  @ eval:1,1", "-> "]]
        );

        // Where the getter throws, which crashes the C++ runtime, its throw is logged, and the message shows as stored.
        // The thrown object is logged twice, so its getter throws twice.
        let getter_throw_log = ["\x1b[31;1mTHROW!\x1b[0m getter of the sixth"];
        assert_eq!(
            logs_of_throws_marked("getter of the sixth"),
            [getter_throw_log, getter_throw_log]
        );
        assert_eq!(
            logs_mentioning("sixth_logged_failure"),
            [[
                "\x1b[31;1mTHROW!\x1b[0m <accessor>",
                "-> sixth_logged_failure @ eval:1,35",
                "->  @ eval:2,34",
                "-> "
            ]]
        );

        // A getter that throws its own object does not read the message again.
        let logs_of_seventh = logs_mentioning("seventh_logged_failure");
        assert!(!logs_of_seventh.is_empty());
        assert!(
            logs_of_seventh
                .iter()
                .all(|log| log[0] == "\x1b[31;1mTHROW!\x1b[0m <accessor>")
        );
    }
}
