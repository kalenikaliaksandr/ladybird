/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Attaching a debugger, its breakpoints, and the callback through which it pauses execution.
//!
//! Everything here runs on the thread that owns the VM, and everything but js_debugger_enable() and
//! js_debugger_is_enabled() needs debugging to be enabled, as C++ reaches the debugger through VM::debugger(), which is
//! null otherwise.

use core::ffi::c_void;
use core::ptr::NonNull;
use std::rc::Rc;

use crate::breakpoint::BreakpointID;
use crate::debugger::{Debugger, PauseInfo, PauseOnExceptions, PauseReason, ResumeMode};
use crate::embedding::abi_types::{JSSourceCode, JSUtf16View, completion_into_abi, vm_from_abi, vm_into_abi};
use crate::embedding::execution_context::JSExecutionContext;
use crate::interpreter::vm::Vm;
use crate::layout::execution_context::ExecutionContext;
use crate::layout::host_class::{JSCompletion, JSVM, JSValue};
use crate::layout::value::Value;
use crate::source_code::SourceCode;
use crate::source_range::SourceRange;
use crate::utf16::Utf16View;

// Debugger::PauseReason.
pub const JS_PAUSE_REASON_ENTRY: u8 = 0;
pub const JS_PAUSE_REASON_BREAKPOINT: u8 = 1;
pub const JS_PAUSE_REASON_DEBUGGER_STATEMENT: u8 = 2;
pub const JS_PAUSE_REASON_EXCEPTION: u8 = 3;
pub const JS_PAUSE_REASON_STEP: u8 = 4;

// Debugger::PauseOnExceptions.
pub const JS_PAUSE_ON_EXCEPTIONS_NONE: u8 = 0;
pub const JS_PAUSE_ON_EXCEPTIONS_ALL: u8 = 1;
pub const JS_PAUSE_ON_EXCEPTIONS_UNCAUGHT: u8 = 2;

// Debugger::ResumeMode.
pub const JS_RESUME_MODE_CONTINUE: u8 = 0;
pub const JS_RESUME_MODE_STEP_INTO: u8 = 1;
pub const JS_RESUME_MODE_STEP_OUT: u8 = 2;
pub const JS_RESUME_MODE_STEP_OVER: u8 = 3;

/// An optional SourceRange: none when `source_code` is null. The source code is borrowed, and the line and column
/// count from 1.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSDebuggerSourceRange {
    pub source_code: *const JSSourceCode,
    pub line: u32,
    pub column: u32,
}

/// A StackTraceElement: a frame's execution context, and where in its source code it is.
#[repr(C)]
pub struct JSDebuggerStackFrame {
    pub execution_context: *mut JSExecutionContext,
    pub source_range: JSDebuggerSourceRange,
}

/// Debugger::PauseInfo, without the executable. The stack frames go from the paused frame outwards, and the
/// exception is only meaningful when has_exception is set. Everything it points to lives until the pause callback
/// returns, and the frames' execution contexts until execution continues.
#[repr(C)]
pub struct JSDebuggerPauseInfo {
    pub reason: u8,
    pub bytecode_offset: u32,
    pub source_range: JSDebuggerSourceRange,
    pub stack_frames: *const JSDebuggerStackFrame,
    pub stack_frame_count: usize,
    pub breakpoint_ids: *const u32,
    pub breakpoint_id_count: usize,
    pub exception: JSValue,
    pub has_exception: bool,
    pub exception_will_be_caught: bool,
}

/// Called each time the debugger pauses, with the context given to js_debugger_set_pause_callback(). It must resume
/// with js_debugger_continue_execution() or js_debugger_continue_execution_preserving_step_state() before it returns.
/// It may run JavaScript, which never pauses again while it runs.
pub type JSDebuggerPauseCallback =
    Option<unsafe extern "C" fn(context: *mut c_void, vm: *mut JSVM, pause_info: *const JSDebuggerPauseInfo)>;

/// The id of a new breakpoint, or why the debugger refused it: a static UTF-8 message, not null-terminated, which is
/// null when the breakpoint was added.
#[repr(C)]
pub struct JSDebuggerAddBreakpointResult {
    pub breakpoint_id: u32,
    pub error_message: *const u8,
    pub error_message_length: usize,
}

/// A Breakpoint: in the given source code, or in any source code with its filename when `source_code` is null. The
/// column is only meaningful when has_column is set.
#[repr(C)]
pub struct JSDebuggerBreakpoint {
    pub id: u32,
    pub line: u32,
    pub column: u32,
    pub has_column: bool,
    pub source_code: *const JSSourceCode,
    pub filename: JSUtf16View,
}

/// Receives each breakpoint, which lives for the call.
#[repr(C)]
pub struct JSDebuggerBreakpointSink {
    pub context: *mut c_void,
    pub append: Option<unsafe extern "C" fn(context: *mut c_void, breakpoint: *const JSDebuggerBreakpoint)>,
}

/// Debugger::FrameBinding: a binding a paused frame can see, and its value, which is the empty value while it is
/// uninitialized.
#[repr(C)]
pub struct JSDebuggerFrameBinding {
    pub name: JSUtf16View,
    pub value: JSValue,
    pub is_mutable: bool,
}

/// Receives each binding, which lives for the call.
#[repr(C)]
pub struct JSDebuggerFrameBindingSink {
    pub context: *mut c_void,
    pub append: Option<unsafe extern "C" fn(context: *mut c_void, binding: *const JSDebuggerFrameBinding)>,
}

fn pause_reason_into_abi(reason: PauseReason) -> u8 {
    match reason {
        PauseReason::Entry => JS_PAUSE_REASON_ENTRY,
        PauseReason::Breakpoint => JS_PAUSE_REASON_BREAKPOINT,
        PauseReason::DebuggerStatement => JS_PAUSE_REASON_DEBUGGER_STATEMENT,
        PauseReason::Exception => JS_PAUSE_REASON_EXCEPTION,
        PauseReason::Step => JS_PAUSE_REASON_STEP,
    }
}

fn pause_on_exceptions_from_abi(mode: u8) -> PauseOnExceptions {
    match mode {
        JS_PAUSE_ON_EXCEPTIONS_NONE => PauseOnExceptions::None,
        JS_PAUSE_ON_EXCEPTIONS_ALL => PauseOnExceptions::All,
        JS_PAUSE_ON_EXCEPTIONS_UNCAUGHT => PauseOnExceptions::Uncaught,
        _ => panic!("{mode} is not a mode of pausing on exceptions"),
    }
}

fn resume_mode_from_abi(mode: u8) -> ResumeMode {
    match mode {
        JS_RESUME_MODE_CONTINUE => ResumeMode::Continue,
        JS_RESUME_MODE_STEP_INTO => ResumeMode::StepInto,
        JS_RESUME_MODE_STEP_OUT => ResumeMode::StepOut,
        JS_RESUME_MODE_STEP_OVER => ResumeMode::StepOver,
        _ => panic!("{mode} is not a resume mode"),
    }
}

pub fn source_code_into_abi(source_code: &Rc<SourceCode>) -> *const JSSourceCode {
    Rc::as_ptr(source_code).cast()
}

/// # Safety
///
/// `source_code` must be the SourceCode of a live Rc, such as one the ABI handed out.
unsafe fn source_code_from_abi(source_code: *const JSSourceCode) -> Rc<SourceCode> {
    let source_code = source_code.cast::<SourceCode>();
    assert!(!source_code.is_null(), "source code is not null");
    // SAFETY: The caller guarantees that the pointer is that of a live Rc, which then has one more owner.
    unsafe {
        Rc::increment_strong_count(source_code);
        Rc::from_raw(source_code)
    }
}

fn source_range_into_abi(source_range: Option<&SourceRange>) -> JSDebuggerSourceRange {
    source_range.map_or(
        JSDebuggerSourceRange {
            source_code: core::ptr::null(),
            line: 0,
            column: 0,
        },
        |source_range| JSDebuggerSourceRange {
            source_code: source_code_into_abi(&source_range.code),
            line: source_range.start.line,
            column: source_range.start.column,
        },
    )
}

/// # Safety
///
/// `vm` must point to a live VM.
unsafe fn debugger_of<'a>(vm: *mut JSVM) -> (&'a Vm, Rc<Debugger>) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    (vm, vm.debugger().expect("debugging is enabled"))
}

/// # Safety
///
/// `execution_context` must point to a live execution context.
unsafe fn execution_context_from_abi<'a>(execution_context: *const JSExecutionContext) -> &'a ExecutionContext {
    // SAFETY: The caller passes a live execution context.
    unsafe { execution_context.cast::<ExecutionContext>().as_ref() }.expect("an execution context is not null")
}

fn call_pause_callback(
    callback: unsafe extern "C" fn(*mut c_void, *mut JSVM, *const JSDebuggerPauseInfo),
    context: *mut c_void,
    vm: &Vm,
    pause_info: &PauseInfo,
) {
    let stack_frames: Vec<JSDebuggerStackFrame> = pause_info
        .stack_trace
        .iter()
        .map(|element| JSDebuggerStackFrame {
            execution_context: element.execution_context.as_ptr().cast(),
            source_range: source_range_into_abi(element.source_range.as_ref()),
        })
        .collect();
    let pause_info_for_c = JSDebuggerPauseInfo {
        reason: pause_reason_into_abi(pause_info.reason),
        bytecode_offset: pause_info.bytecode_offset,
        source_range: source_range_into_abi(pause_info.source_range.as_ref()),
        stack_frames: stack_frames.as_ptr(),
        stack_frame_count: stack_frames.len(),
        breakpoint_ids: pause_info.breakpoint_ids.as_ptr(),
        breakpoint_id_count: pause_info.breakpoint_ids.len(),
        exception: pause_info.exception.unwrap_or(Value::UNDEFINED).0,
        has_exception: pause_info.exception.is_some(),
        exception_will_be_caught: pause_info.exception_will_be_caught,
    };
    // SAFETY: The embedder's callback receives the context it gave the debugger, and a pause info that outlives the
    //         call, whose exception the PauseInfo on the stack keeps alive.
    unsafe { callback(context, vm_into_abi(vm), &raw const pause_info_for_c) };
}

fn add_breakpoint_result_into_abi(result: Result<BreakpointID, &'static str>) -> JSDebuggerAddBreakpointResult {
    match result {
        Ok(breakpoint_id) => JSDebuggerAddBreakpointResult {
            breakpoint_id,
            error_message: core::ptr::null(),
            error_message_length: 0,
        },
        Err(message) => JSDebuggerAddBreakpointResult {
            breakpoint_id: 0,
            error_message: message.as_ptr(),
            error_message_length: message.len(),
        },
    }
}

/// VM::enable_debugging(): attaches a debugger, unless one is attached. The interpreter checks for breakpoints from
/// the next function or script it enters. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_enable(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    unsafe { vm_from_abi(vm) }.enable_debugging();
}

/// VM::disable_debugging(): detaches the debugger, with its breakpoints, pause callback and the callback's context.
/// It must not be paused. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_disable(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    unsafe { vm_from_abi(vm) }.disable_debugging();
}

/// VM::debugging_enabled(). Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_is_enabled(vm: *mut JSVM) -> bool {
    // SAFETY: The caller passes a live VM.
    unsafe { vm_from_abi(vm) }.debugging_enabled()
}

/// Debugger::set_pause_callback(): `callback` is called with `context` each time the debugger pauses, in place of
/// the previous callback. A null callback leaves pauses unreported. The debugger keeps `context` alive until the
/// callback is replaced or debugging is disabled. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM, and `context` must be null or a live C++ GC cell of the VM's heap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_set_pause_callback(
    vm: *mut JSVM,
    callback: JSDebuggerPauseCallback,
    context: *mut c_void,
) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    match callback {
        Some(callback) => debugger.set_pause_callback(move |vm, pause_info| {
            call_pause_callback(callback, context, vm, pause_info);
        }),
        None => debugger.clear_pause_callback(),
    }
    // SAFETY: The caller passes null or a live cell of the heap, which the slot keeps alive from now on.
    unsafe { debugger.pause_callback_context().set(NonNull::new(context)) };
}

/// Debugger::continue_execution(): resumes a paused debugger, stepping as `resume_mode` says. Only on the VM's
/// thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_continue_execution(vm: *mut JSVM, resume_mode: u8) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.continue_execution(resume_mode_from_abi(resume_mode));
}

/// Debugger::continue_execution_preserving_step_state(): resumes after the embedder filters out a pause, without
/// cancelling a step in progress. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_continue_execution_preserving_step_state(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.continue_execution_preserving_step_state();
}

/// Debugger::is_paused(). Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_is_paused(vm: *mut JSVM) -> bool {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.is_paused()
}

/// Debugger::request_pause_on_next_bytecode_execution(): pauses at the next instruction with a source position that
/// any script or function runs. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_request_pause_on_next_bytecode_execution(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.request_pause_on_next_bytecode_execution();
}

/// Debugger::set_pause_on_exceptions(), with a JS_PAUSE_ON_EXCEPTIONS_* mode. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_set_pause_on_exceptions(vm: *mut JSVM, mode: u8) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.set_pause_on_exceptions(pause_on_exceptions_from_abi(mode));
}

/// Debugger::did_finish_exception_propagation(): an exception the debugger paused at reached the embedder, so a
/// later throw of the same value pauses again. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_did_finish_exception_propagation(vm: *mut JSVM, exception: JSValue) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.did_finish_exception_propagation(Value(exception));
}

/// Debugger::add_breakpoint(filename, line, column): a breakpoint in every source code with that filename, at the
/// first position at or after the line, and the column if there is one. Adding a breakpoint that exists returns its
/// id. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM and `filename` must be valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_add_breakpoint(
    vm: *mut JSVM,
    filename: JSUtf16View,
    line: u32,
    has_column: bool,
    column: u32,
) -> JSDebuggerAddBreakpointResult {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    // SAFETY: The caller guarantees that the view is valid for the call.
    let filename = unsafe { filename.as_view() };
    add_breakpoint_result_into_abi(debugger.add_breakpoint(filename, line, has_column.then_some(column)))
}

/// Debugger::add_breakpoint(source_code, line, column): like js_debugger_add_breakpoint(), but only in that source
/// code, which the breakpoint keeps alive. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM and `source_code` to a live source code, such as one a pause or a breakpoint reported.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_add_breakpoint_for_source_code(
    vm: *mut JSVM,
    source_code: *const JSSourceCode,
    line: u32,
    has_column: bool,
    column: u32,
) -> JSDebuggerAddBreakpointResult {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    // SAFETY: The caller passes a live source code.
    let source_code = unsafe { source_code_from_abi(source_code) };
    add_breakpoint_result_into_abi(debugger.add_breakpoint_for_source_code(
        source_code,
        line,
        has_column.then_some(column),
    ))
}

/// Debugger::remove_breakpoint(): whether there was a breakpoint with that id. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_remove_breakpoint(vm: *mut JSVM, breakpoint_id: u32) -> bool {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.remove_breakpoint(breakpoint_id)
}

/// Debugger::is_breakpoint_resolved(): whether code the VM has compiled has a position for the breakpoint. Only on the
/// VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_is_breakpoint_resolved(vm: *mut JSVM, breakpoint_id: u32) -> bool {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    debugger.is_breakpoint_resolved(breakpoint_id)
}

/// Debugger::breakpoints(): hands each breakpoint to the sink, in the order they were added. The sink may add and
/// remove breakpoints, which the list it receives does not reflect. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM and `sink` to a sink with an append function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_breakpoints(vm: *mut JSVM, sink: *const JSDebuggerBreakpointSink) {
    // SAFETY: The caller passes a live VM.
    let (_, debugger) = unsafe { debugger_of(vm) };
    // SAFETY: The caller passes a live sink.
    let sink = unsafe { sink.as_ref() }.expect("a sink is not null");
    let append = sink.append.expect("a sink has an append function");
    // The list is a copy, which keeps the source codes alive while the sink runs.
    for breakpoint in debugger.breakpoints() {
        let breakpoint_for_c = JSDebuggerBreakpoint {
            id: breakpoint.id,
            line: breakpoint.line,
            column: breakpoint.column.unwrap_or(0),
            has_column: breakpoint.column.is_some(),
            source_code: breakpoint
                .source_code
                .as_ref()
                .map_or(core::ptr::null(), source_code_into_abi),
            filename: JSUtf16View::of(Utf16View::of_string(&breakpoint.filename)),
        };
        // SAFETY: The embedder's sink receives its context and a breakpoint that outlives the call.
        unsafe { append(sink.context, &raw const breakpoint_for_c) };
    }
}

/// Debugger::evaluate_in_frame(): runs `source_text` as a direct eval in a frame of the paused stack, which sees and
/// may assign the frame's arguments and locals. Only while paused, on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM, `execution_context` to a context of the paused stack, and `source_text` must be valid
/// for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_evaluate_in_frame(
    vm: *mut JSVM,
    execution_context: *mut JSExecutionContext,
    source_text: JSUtf16View,
) -> JSCompletion {
    // SAFETY: The caller passes a live VM.
    let (vm, debugger) = unsafe { debugger_of(vm) };
    // SAFETY: The caller passes a context of the paused stack.
    let execution_context = unsafe { execution_context_from_abi(execution_context) };
    // SAFETY: The caller guarantees that the view is valid for the call.
    let source_text = unsafe { source_text.as_view() };
    completion_into_abi(debugger.evaluate_in_frame(vm, execution_context, source_text))
}

/// Debugger::bindings_for_frame(): hands the sink each argument and local of the frame's function that is in scope at
/// the position where the frame is. The values stay alive until the call returns. Only on the VM's thread.
///
/// # Safety
///
/// `vm` must point to a live VM, `execution_context` to a live context that runs an executable, and `sink` to a sink
/// with an append function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_debugger_bindings_for_frame(
    vm: *mut JSVM,
    execution_context: *const JSExecutionContext,
    sink: *const JSDebuggerFrameBindingSink,
) {
    // SAFETY: The caller passes a live VM.
    let (vm, debugger) = unsafe { debugger_of(vm) };
    // SAFETY: The caller passes a live execution context.
    let execution_context = unsafe { execution_context_from_abi(execution_context) };
    // SAFETY: The caller passes a live sink.
    let sink = unsafe { sink.as_ref() }.expect("a sink is not null");
    let append = sink.append.expect("a sink has an append function");
    let bindings = debugger.bindings_for_frame(vm, execution_context);
    // The copy leaves the marked list unborrowed while the sink runs, and the list keeps the values alive.
    for binding in bindings.to_vec() {
        let binding_for_c = JSDebuggerFrameBinding {
            name: JSUtf16View::of(Utf16View::of_fly_string(&binding.name)),
            value: binding.value.0,
            is_mutable: binding.is_mutable,
        };
        // SAFETY: The embedder's sink receives its context and a binding that outlives the call.
        unsafe { append(sink.context, &raw const binding_for_c) };
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::{Cell, RefCell};

    use super::*;
    use crate::embedding::abi_types::completion_from_abi;
    use crate::gc::weak::GcWeak;
    use crate::layout::cell::Gc;
    use crate::layout::realm::Realm;
    use crate::runtime::completion::ThrowCompletionOr;
    use crate::runtime::object::Object;
    use crate::script::Script;
    use crate::utilities::initialize_realm;

    /// What a test does at a pause: it receives the VM and the pause, and must continue execution.
    type PauseHandler = Rc<dyn Fn(*mut JSVM, &JSDebuggerPauseInfo)>;

    std::thread_local! {
        static PAUSE_HANDLER: RefCell<Option<PauseHandler>> = const { RefCell::new(None) };
        static CONTEXTS_SEEN_BY_THE_CALLBACK: RefCell<Vec<*mut c_void>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn handle_pause(context: *mut c_void, vm: *mut JSVM, pause_info: *const JSDebuggerPauseInfo) {
        CONTEXTS_SEEN_BY_THE_CALLBACK.with_borrow_mut(|contexts| contexts.push(context));
        let handler = PAUSE_HANDLER
            .with_borrow(Clone::clone)
            .expect("the test has a pause handler");
        // SAFETY: The debugger passes a pause info that is valid for the call.
        handler(vm, unsafe { &*pause_info });
    }

    /// Installs `handler` as what the pause callback runs.
    fn on_pause(vm: &Vm, handler: impl Fn(*mut JSVM, &JSDebuggerPauseInfo) + 'static) {
        PAUSE_HANDLER.set(Some(Rc::new(handler)));
        // SAFETY: The VM is live and a null context is allowed.
        unsafe { js_debugger_set_pause_callback(vm_into_abi(vm), Some(handle_pause), core::ptr::null_mut()) };
    }

    fn run_script(vm: &Vm, realm: Gc<Realm>, source: &str, filename: &str) -> ThrowCompletionOr<Value> {
        let source: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse_with_filename(vm, &source, realm, filename).expect("the script parses");
        vm.run_script(script, None)
    }

    fn enabled_vm() -> Box<Vm> {
        let vm = Vm::create();
        // SAFETY: The VM is live.
        unsafe { js_debugger_enable(vm_into_abi(&vm)) };
        vm
    }

    fn add_breakpoint(vm: *mut JSVM, filename: &str, line: u32) -> JSDebuggerAddBreakpointResult {
        let filename = ak::Utf16String::from_utf8(filename);
        // SAFETY: The VM is live and the view outlives the call.
        unsafe { js_debugger_add_breakpoint(vm, JSUtf16View::of(Utf16View::of_string(&filename)), line, false, 0) }
    }

    fn continue_execution(vm: *mut JSVM, resume_mode: u8) {
        // SAFETY: The VM is live.
        unsafe { js_debugger_continue_execution(vm, resume_mode) };
    }

    fn stack_frames(pause_info: &JSDebuggerPauseInfo) -> &[JSDebuggerStackFrame] {
        // SAFETY: The pause info has that many frames.
        unsafe { core::slice::from_raw_parts(pause_info.stack_frames, pause_info.stack_frame_count) }
    }

    fn breakpoint_ids(pause_info: &JSDebuggerPauseInfo) -> &[u32] {
        if pause_info.breakpoint_id_count == 0 {
            return &[];
        }
        // SAFETY: The pause info has that many breakpoint ids.
        unsafe { core::slice::from_raw_parts(pause_info.breakpoint_ids, pause_info.breakpoint_id_count) }
    }

    fn string_of(view: JSUtf16View) -> String {
        // SAFETY: Every view the debugger hands out is valid for the call.
        unsafe { view.as_view() }.to_utf8()
    }

    unsafe extern "C" fn collect_binding(context: *mut c_void, binding: *const JSDebuggerFrameBinding) {
        // SAFETY: The test passes its list as the context, and the debugger a binding that is valid for the call.
        let (bindings, binding) = unsafe { (&mut *context.cast::<Vec<(String, Value, bool)>>(), &*binding) };
        bindings.push((string_of(binding.name), Value(binding.value), binding.is_mutable));
    }

    fn bindings_of(vm: *mut JSVM, frame: &JSDebuggerStackFrame) -> Vec<(String, Value, bool)> {
        let mut bindings: Vec<(String, Value, bool)> = Vec::new();
        let sink = JSDebuggerFrameBindingSink {
            context: (&raw mut bindings).cast(),
            append: Some(collect_binding),
        };
        // SAFETY: The VM and the frame's context are live while paused.
        unsafe { js_debugger_bindings_for_frame(vm, frame.execution_context, &raw const sink) };
        bindings
    }

    fn evaluate(vm: *mut JSVM, frame: &JSDebuggerStackFrame, source: &str) -> ThrowCompletionOr<Value> {
        let source = ak::Utf16String::from_utf8(source);
        // SAFETY: The VM and the frame's context are live while paused, and the view outlives the call.
        completion_from_abi(unsafe {
            js_debugger_evaluate_in_frame(
                vm,
                frame.execution_context,
                JSUtf16View::of(Utf16View::of_string(&source)),
            )
        })
    }

    #[test]
    fn a_pause_at_a_breakpoint_reports_its_frames_and_bindings() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        // The function is compiled before the breakpoint is set, which then resolves inside it rather than sliding to
        // the call after it.
        let source = "function answer(argument) {\n    let value = 41;\n    let sum = value + argument;\n    return sum;\n}\n\
                      answer(0);\n";
        assert!(run_script(&vm, realm, source, "pause.js").is_ok());
        let breakpoint = add_breakpoint(vm_into_abi(&vm), "pause.js", 4);
        assert!(breakpoint.error_message.is_null());

        let pause_count = Rc::new(Cell::new(0));
        let pause_count_in_handler = Rc::clone(&pause_count);
        on_pause(&vm, move |vm, pause_info| {
            pause_count_in_handler.set(pause_count_in_handler.get() + 1);
            assert_eq!(pause_info.reason, JS_PAUSE_REASON_BREAKPOINT);
            assert_eq!(breakpoint_ids(pause_info), [breakpoint.breakpoint_id]);
            assert!(!pause_info.has_exception);
            assert!(!pause_info.source_range.source_code.is_null());
            assert_eq!(pause_info.source_range.line, 4);
            // SAFETY: The VM is live.
            assert!(unsafe { js_debugger_is_paused(vm) });

            let frames = stack_frames(pause_info);
            assert!(frames.len() >= 2);
            assert_eq!(frames[0].source_range.line, 4);
            assert_eq!(frames[1].source_range.line, 1);
            assert_eq!(frames[0].source_range.source_code, pause_info.source_range.source_code);
            assert_ne!(frames[1].source_range.source_code, pause_info.source_range.source_code);
            assert_eq!(
                bindings_of(vm, &frames[0]),
                [
                    ("argument".to_owned(), Value::from_i32(1), true),
                    ("value".to_owned(), Value::from_i32(41), true),
                    ("sum".to_owned(), Value::from_i32(42), true),
                ]
            );
            assert_eq!(
                evaluate(vm, &frames[0], "sum = value + argument + 8").ok(),
                Some(Value::from_i32(50))
            );
            continue_execution(vm, JS_RESUME_MODE_CONTINUE);
        });

        assert_eq!(
            run_script(&vm, realm, "answer(1);", "call.js").ok(),
            Some(Value::from_i32(50))
        );
        assert_eq!(pause_count.get(), 1);
    }

    unsafe extern "C" fn collect_breakpoint(context: *mut c_void, breakpoint: *const JSDebuggerBreakpoint) {
        // SAFETY: The test passes its list as the context, and the debugger a breakpoint that is valid for the call.
        let (breakpoints, breakpoint) = unsafe { (&mut *context.cast::<Vec<String>>(), &*breakpoint) };
        let column = if breakpoint.has_column {
            format!(":{}", breakpoint.column)
        } else {
            String::new()
        };
        let source_code = if breakpoint.source_code.is_null() { "any" } else { "one" };
        breakpoints.push(format!(
            "{} {}:{}{column} in {source_code} source",
            breakpoint.id,
            string_of(breakpoint.filename),
            breakpoint.line
        ));
    }

    fn breakpoints_of(vm: *mut JSVM) -> Vec<String> {
        let mut breakpoints: Vec<String> = Vec::new();
        let sink = JSDebuggerBreakpointSink {
            context: (&raw mut breakpoints).cast(),
            append: Some(collect_breakpoint),
        };
        // SAFETY: The VM is live and the sink outlives the call.
        unsafe { js_debugger_breakpoints(vm, &raw const sink) };
        breakpoints
    }

    #[test]
    fn breakpoints_can_be_listed_resolved_and_removed() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let js_vm = vm_into_abi(&vm);

        let by_filename = add_breakpoint(js_vm, "list.js", 2).breakpoint_id;
        // SAFETY: The VM is live.
        assert!(!unsafe { js_debugger_is_breakpoint_resolved(js_vm, by_filename) });

        let source: Vec<u16> = "let first = 1;\nlet second = 2;\n".encode_utf16().collect();
        let script = Script::parse_with_filename(&vm, &source, realm, "list.js").expect("the script parses");
        assert!(vm.run_script(script, None).is_ok());
        // SAFETY: The VM is live.
        assert!(unsafe { js_debugger_is_breakpoint_resolved(js_vm, by_filename) });

        let executable = script.cached_executable();
        let source_code = source_code_into_abi(executable.source_code().expect("the script has source code"));
        // SAFETY: The VM and the source code are live.
        let in_source_code = unsafe { js_debugger_add_breakpoint_for_source_code(js_vm, source_code, 1, true, 5) };
        assert!(in_source_code.error_message.is_null());

        let refused = add_breakpoint(js_vm, "list.js", 0);
        // SAFETY: An error message is that many bytes of static UTF-8.
        let error_message = unsafe { core::slice::from_raw_parts(refused.error_message, refused.error_message_length) };
        assert_eq!(error_message, b"Breakpoint line must be greater than zero");

        assert_eq!(
            breakpoints_of(js_vm),
            [
                format!("{by_filename} list.js:2 in any source"),
                format!("{} list.js:1:5 in one source", in_source_code.breakpoint_id),
            ]
        );
        // SAFETY: The VM is live.
        unsafe {
            assert!(js_debugger_remove_breakpoint(js_vm, by_filename));
            assert!(!js_debugger_remove_breakpoint(js_vm, by_filename));
        }
        assert_eq!(
            breakpoints_of(js_vm),
            [format!("{} list.js:1:5 in one source", in_source_code.breakpoint_id)]
        );
    }

    #[test]
    fn pauses_report_exceptions_steps_and_entries() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let js_vm = vm_into_abi(&vm);

        let pauses = Rc::new(RefCell::new(Vec::new()));
        let pauses_in_handler = Rc::clone(&pauses);
        on_pause(&vm, move |vm, pause_info| {
            let exception = pause_info.has_exception.then(|| {
                (
                    Value(pause_info.exception).as_i32(),
                    pause_info.exception_will_be_caught,
                )
            });
            pauses_in_handler
                .borrow_mut()
                .push((pause_info.reason, pause_info.source_range.line, exception));
            let resume_mode = if pause_info.reason == JS_PAUSE_REASON_DEBUGGER_STATEMENT {
                JS_RESUME_MODE_STEP_OVER
            } else {
                JS_RESUME_MODE_CONTINUE
            };
            continue_execution(vm, resume_mode);
        });

        // SAFETY: The VM is live.
        unsafe { js_debugger_set_pause_on_exceptions(js_vm, JS_PAUSE_ON_EXCEPTIONS_ALL) };
        assert!(run_script(&vm, realm, "try {\n    throw 42;\n} catch {}\n", "exception.js").is_ok());
        // SAFETY: The VM is live.
        unsafe {
            js_debugger_did_finish_exception_propagation(js_vm, Value::from_i32(42).0);
            js_debugger_set_pause_on_exceptions(js_vm, JS_PAUSE_ON_EXCEPTIONS_NONE);
        }

        assert!(run_script(&vm, realm, "debugger;\nlet after = 1;\n", "step.js").is_ok());

        // SAFETY: The VM is live.
        unsafe { js_debugger_request_pause_on_next_bytecode_execution(js_vm) };
        assert!(run_script(&vm, realm, "let entered = 1;\n", "entry.js").is_ok());

        assert_eq!(
            *pauses.borrow(),
            [
                (JS_PAUSE_REASON_EXCEPTION, 2, Some((42, true))),
                (JS_PAUSE_REASON_DEBUGGER_STATEMENT, 1, None),
                (JS_PAUSE_REASON_STEP, 2, None),
                (JS_PAUSE_REASON_ENTRY, 1, None),
            ]
        );
    }

    std::thread_local! {
        static LINES_SEEN_BY_THE_REPLACEMENT: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn record_the_line_of_the_pause(
        _: *mut c_void,
        vm: *mut JSVM,
        pause_info: *const JSDebuggerPauseInfo,
    ) {
        // SAFETY: The debugger passes a pause info that is valid for the call.
        let line = unsafe { &*pause_info }.source_range.line;
        LINES_SEEN_BY_THE_REPLACEMENT.with_borrow_mut(|lines| lines.push(line));
        continue_execution(vm, JS_RESUME_MODE_CONTINUE);
    }

    #[test]
    fn the_pause_callback_can_run_javascript_and_replace_itself() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let vm_pointer: *const Vm = &raw const *vm;

        on_pause(&vm, move |vm, pause_info| {
            // SAFETY: The VM outlives the pause.
            let vm_in_handler = unsafe { &*vm_pointer };
            // A nested script runs to completion, without pausing at its own debugger statement.
            assert_eq!(
                run_script(vm_in_handler, realm, "debugger; 6 * 7;", "nested.js").ok(),
                Some(Value::from_i32(42))
            );
            assert_eq!(
                evaluate(vm, &stack_frames(pause_info)[0], "first + 1").ok(),
                Some(Value::from_i32(2))
            );
            // SAFETY: The VM is live. The replacement takes over from the next pause on.
            unsafe { js_debugger_set_pause_callback(vm, Some(record_the_line_of_the_pause), core::ptr::null_mut()) };
            continue_execution(vm, JS_RESUME_MODE_CONTINUE);
        });

        let source = "let first = 1;\ndebugger;\ndebugger;\n";
        assert!(run_script(&vm, realm, source, "replace.js").is_ok());
        assert_eq!(CONTEXTS_SEEN_BY_THE_CALLBACK.take(), [core::ptr::null_mut()]);
        assert_eq!(LINES_SEEN_BY_THE_REPLACEMENT.take(), [3]);
    }

    /// A breakpoint sink that removes each breakpoint it receives and adds one on a later line in its place.
    unsafe extern "C" fn replace_breakpoint(context: *mut c_void, breakpoint: *const JSDebuggerBreakpoint) {
        let vm = context.cast::<JSVM>();
        // SAFETY: The debugger passes a breakpoint that is valid for the call, and the test the VM as the context.
        let breakpoint = unsafe { &*breakpoint };
        // SAFETY: The VM is live.
        assert!(unsafe { js_debugger_remove_breakpoint(vm, breakpoint.id) });
        let filename = string_of(breakpoint.filename);
        assert!(
            add_breakpoint(vm, &filename, breakpoint.line + 10)
                .error_message
                .is_null()
        );
    }

    struct BindingEvaluations {
        vm: *mut JSVM,
        frame: JSDebuggerStackFrame,
        each_binding_evaluates_to_its_value: Vec<bool>,
    }

    /// A binding sink that reads each binding again by evaluating its name in the paused frame.
    unsafe extern "C" fn evaluate_binding(context: *mut c_void, binding: *const JSDebuggerFrameBinding) {
        // SAFETY: The test passes its evaluations as the context, and the debugger a binding that is valid for the call.
        let (evaluations, binding) = unsafe { (&mut *context.cast::<BindingEvaluations>(), &*binding) };
        let evaluated = evaluate(evaluations.vm, &evaluations.frame, &string_of(binding.name));
        evaluations
            .each_binding_evaluates_to_its_value
            .push(evaluated.ok() == Some(Value(binding.value)));
    }

    #[test]
    fn sinks_can_reenter_the_debugger() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let js_vm = vm_into_abi(&vm);

        add_breakpoint(js_vm, "first.js", 1);
        add_breakpoint(js_vm, "second.js", 2);
        let sink = JSDebuggerBreakpointSink {
            context: js_vm.cast(),
            append: Some(replace_breakpoint),
        };
        // SAFETY: The VM is live and the sink outlives the call.
        unsafe { js_debugger_breakpoints(js_vm, &raw const sink) };
        let breakpoints: Vec<String> = breakpoints_of(js_vm)
            .iter()
            .map(|breakpoint| breakpoint.split_once(' ').expect("a breakpoint has an id").1.to_owned())
            .collect();
        assert_eq!(breakpoints, ["first.js:11 in any source", "second.js:12 in any source"]);

        let evaluations: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        let evaluations_in_handler = Rc::clone(&evaluations);
        on_pause(&vm, move |vm, pause_info| {
            let execution_context = stack_frames(pause_info)[0].execution_context;
            let mut binding_evaluations = BindingEvaluations {
                vm,
                frame: JSDebuggerStackFrame {
                    execution_context,
                    source_range: source_range_into_abi(None),
                },
                each_binding_evaluates_to_its_value: Vec::new(),
            };
            let sink = JSDebuggerFrameBindingSink {
                context: (&raw mut binding_evaluations).cast(),
                append: Some(evaluate_binding),
            };
            // SAFETY: The VM and the frame are live while paused, and the sink outlives the call.
            unsafe { js_debugger_bindings_for_frame(vm, execution_context, &raw const sink) };
            evaluations_in_handler
                .borrow_mut()
                .extend(binding_evaluations.each_binding_evaluates_to_its_value);
            continue_execution(vm, JS_RESUME_MODE_CONTINUE);
        });
        let source =
            "function check(first, second) {\n    let third = first + second;\n    debugger;\n}\ncheck(1, 2);\n";
        assert!(run_script(&vm, root_execution_context.realm(), source, "sinks.js").is_ok());
        assert_eq!(*evaluations.borrow(), [true, true, true]);
    }

    const CONTEXT_COUNT: usize = 32;

    /// Installs a pause callback whose context only the debugger holds, and returns it with objects that nothing
    /// holds.
    #[inline(never)]
    fn install_callback_with_a_context(vm: &Vm, realm: Gc<Realm>) -> (GcWeak<Object>, Vec<GcWeak<Object>>) {
        let context = Object::create(vm, realm, None);
        // SAFETY: The VM is live and the context is a live cell of its heap.
        unsafe { js_debugger_set_pause_callback(vm_into_abi(vm), Some(handle_pause), context.as_ptr().cast()) };
        let objects_nothing_holds = (0..CONTEXT_COUNT)
            .map(|_| GcWeak::new(vm.heap(), Object::create(vm, realm, None)))
            .collect();
        (GcWeak::new(vm.heap(), context), objects_nothing_holds)
    }

    #[test]
    fn the_debugger_keeps_the_context_of_its_pause_callback_alive() {
        let vm = enabled_vm();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        PAUSE_HANDLER.set(Some(Rc::new(|vm, _| {
            continue_execution(vm, JS_RESUME_MODE_CONTINUE);
        })));

        let (context, objects_nothing_holds) = install_callback_with_a_context(&vm, realm);
        vm.heap().collect_garbage();
        let dead_objects = objects_nothing_holds
            .iter()
            .filter(|object| object.get().is_none())
            .count();
        assert!(
            dead_objects >= CONTEXT_COUNT / 2,
            "only {dead_objects} of the objects nothing holds were collected"
        );
        let context = context.get().expect("the debugger keeps the context alive");

        assert!(run_script(&vm, realm, "debugger;", "context.js").is_ok());
        assert_eq!(CONTEXTS_SEEN_BY_THE_CALLBACK.take(), [context.as_ptr().cast()]);

        // SAFETY: The VM is live.
        unsafe { js_debugger_set_pause_callback(vm_into_abi(&vm), None, core::ptr::null_mut()) };
        assert!(run_script(&vm, realm, "debugger;", "unreported.js").is_ok());
        assert!(CONTEXTS_SEEN_BY_THE_CALLBACK.take().is_empty());
        assert!(
            vm.debugger()
                .is_some_and(|debugger| debugger.pause_callback_context().get().is_none())
        );

        // SAFETY: The VM is live and not paused.
        unsafe {
            js_debugger_disable(vm_into_abi(&vm));
            assert!(!js_debugger_is_enabled(vm_into_abi(&vm)));
        }
    }
}
