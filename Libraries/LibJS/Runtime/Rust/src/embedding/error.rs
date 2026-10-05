/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Errors: creating and throwing them with messages the embedder formats, and the [[ErrorData]] of objects. Every
//! function runs on the thread that owns the VM.

use core::ffi::c_void;

use crate::embedding::abi_types::{
    JSErrorData, JSErrorDataCell, JSErrorKind, JSOwnedUtf16String, JSRealm, JSUtf16View, cell_from_abi, cell_into_abi,
    completion_into_abi, error_data_from_abi, error_data_into_abi, error_kind_from_abi, optional_cell_from_abi,
    optional_cell_into_abi, owned_utf16_string_from_abi, owned_utf16_string_into_abi, value_from_abi, vm_from_abi,
};
use crate::gc::class::{Class, GcCell};
use crate::interpreter::vm::TypeErrorRealmOverride;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSCompletion, JSObject, JSVM, JSValue};
use crate::layout::object::Object;
use crate::runtime::error::Error;
use crate::runtime::error_data::{CompactTraceback, ErrorData, ErrorDataCell};
use crate::utf16::Utf16View;

/// The error data that the embedder passes as a JSErrorData: an address js_error_data_of or
/// js_error_data_cell_error_data gave it, or a JSErrorDataCell itself, which is where a pointer to the C++
/// ErrorDataCell points as a pointer to the ErrorData it derives from.
///
/// # Safety
///
/// `error_data` must be the error data or the error data cell of a live cell.
unsafe fn error_data_or_error_data_cell_from_abi<'cell>(error_data: *const JSErrorData) -> &'cell ErrorData {
    assert!(!error_data.is_null(), "the embedder passes error data");
    // A cell starts with its class. Error data starts with a part of its traceback's Vec or a cell pointer, neither
    // of which is ever the address of a class.
    // SAFETY: Both start with an initialized word.
    let first_word = unsafe { error_data.cast::<*const Class>().read_unaligned() };
    if core::ptr::eq(first_word, ErrorDataCell::CLASS) {
        // SAFETY: The pointer is the address of a live error data cell.
        return unsafe { &*error_data.cast::<ErrorDataCell>() }.error_data();
    }
    // SAFETY: The caller passes error data of a live cell.
    unsafe { error_data_from_abi(error_data) }
}

/// The error data that the error_data hook of a host class returned for `object`: error data or an error data cell
/// as js_error_data_stack_string takes it, or null for none.
///
/// # Safety
///
/// `error_data` must be null or the error data or error data cell of a cell that `object` keeps alive.
pub unsafe fn error_data_from_host_hook(_object: &Object, error_data: *mut c_void) -> Option<&ErrorData> {
    // SAFETY: The caller guarantees that the object keeps the error data alive, as long as the object itself.
    (!error_data.is_null()).then(|| unsafe { error_data_or_error_data_cell_from_abi(error_data.cast_const().cast()) })
}

/// Throws a new error of `kind` whose message is a copy of the code units `message` views, created the way the
/// runtime creates the errors it throws: in the current realm, or for a TypeError in the realm a
/// js_error_type_error_realm_scope_enter overrides it with, and with the current call stack as its error data. Call on
/// the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, with a realm on its execution context stack, and `message` a view of code units
/// that stay unchanged during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_throw(vm: *mut JSVM, kind: JSErrorKind, message: JSUtf16View) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a view of code units that stay unchanged during the call.
    let message = unsafe { message.as_view() }.to_utf16_string();
    completion_into_abi::<()>(vm.throw_completion_with_utf16_message(error_kind_from_abi(kind), message))
}

/// js_error_throw with a message the caller gives up, whose storage the error's message adopts. Call on the VM's
/// thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, with a realm on its execution context stack, and `message` an AK::Utf16String
/// whose reference the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_throw_with_owned_message(
    vm: *mut JSVM,
    kind: JSErrorKind,
    message: JSOwnedUtf16String,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller gives up its reference to the message.
    let message = unsafe { owned_utf16_string_from_abi(message) };
    completion_into_abi::<()>(vm.throw_completion_with_utf16_message(error_kind_from_abi(kind), message))
}

/// A new error of `kind` in `realm`, without a message, whose error data is the current call stack. Call on the VM's
/// thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `realm` a realm of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_create(vm: *mut JSVM, realm: *mut JSRealm, kind: JSErrorKind) -> *mut JSObject {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a realm of the VM.
    let realm = unsafe { cell_from_abi(realm) };
    cell_into_abi(error_kind_from_abi(kind).create_without_message(vm, realm).upcast())
}

/// A new Error, without a message, whose prototype is `prototype` rather than an intrinsic one, for the error classes
/// an embedder defines, such as WebAssembly.CompileError. It is allocated in `realm`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `realm` a realm of it, and `prototype` an object of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_create_with_prototype(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    prototype: *mut JSObject,
) -> *mut JSObject {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a realm and an object of the VM.
    let (realm, prototype) = unsafe { (cell_from_abi(realm), cell_from_abi(prototype)) };
    let error = realm.create_object(vm, Error::new(vm, Error::CLASS, prototype));
    cell_into_abi(error.upcast())
}

/// # Safety
///
/// `error` must be an Error of the embedder's VM.
unsafe fn error_from_abi(error: *mut JSObject) -> Gc<Error> {
    // SAFETY: The caller passes an object of the VM.
    let object = unsafe { cell_from_abi(error) };
    object.downcast::<Error>().expect("the embedder passes an Error")
}

/// Defines the "message" of `error` as a copy of the code units `message` views, like the Error constructors do. Call
/// on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `error` an Error of it, and `message` a view of code units that stay unchanged
/// during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_set_message(vm: *mut JSVM, error: *mut JSObject, message: JSUtf16View) {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a view of code units that stay unchanged during the call.
    let message = unsafe { message.as_view() }.to_utf16_string();
    // SAFETY: The caller passes an Error of the VM.
    unsafe { error_from_abi(error) }.set_message(vm, message);
}

/// js_error_set_message with a message the caller gives up, whose storage the message adopts. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `error` an Error of it, and `message` an AK::Utf16String whose reference the caller
/// gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_set_owned_message(vm: *mut JSVM, error: *mut JSObject, message: JSOwnedUtf16String) {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller gives up its reference to the message.
    let message = unsafe { owned_utf16_string_from_abi(message) };
    // SAFETY: The caller passes an Error of the VM.
    unsafe { error_from_abi(error) }.set_message(vm, message);
}

/// InstallErrorCause(error, options), which can throw while reading "cause" from `options`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, `error` an Error of it, and `options` a value of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_install_error_cause(
    vm: *mut JSVM,
    error: *mut JSObject,
    options: JSValue,
) -> JSCompletion {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes an Error of the VM.
    let result = unsafe { error_from_abi(error) }.install_error_cause(vm, value_from_abi(options));
    completion_into_abi(result)
}

/// Whether `object` is an Error object, with an [[ErrorData]] slot of its own, as opposed to a host object whose
/// error data lives in a JSErrorDataCell. Call on the VM's thread.
///
/// # Safety
///
/// `object` must be an object of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_is_error(object: *mut JSObject) -> bool {
    // SAFETY: The caller passes an object of the VM.
    unsafe { cell_from_abi(object) }.is::<Error>()
}

/// The error data of `object`, which an Error has and a host object may have through its class's error_data hook, or
/// null. It stays valid for as long as `object` lives. Call on the VM's thread.
///
/// # Safety
///
/// `object` must be an object of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_of(object: *mut JSObject) -> *const JSErrorData {
    // SAFETY: The caller passes an object of the VM.
    let object = unsafe { cell_from_abi(object) };
    object.error_data().map_or(core::ptr::null(), error_data_into_abi)
}

/// The stack of `error_data` as Error.prototype.stack shows it after the name and message, one "    at" line per frame
/// but the outermost, which the caller owns. With `compact` set, more than five consecutive frames of the same function
/// show as one with a count. Like the other functions that read error data, it also takes the JSErrorDataCell that
/// holds it. Call on the VM's thread.
///
/// # Safety
///
/// `error_data` must be error data, or an error data cell, of a live cell of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_stack_string(
    error_data: *const JSErrorData,
    compact: bool,
) -> JSOwnedUtf16String {
    // SAFETY: The caller passes error data of a live cell.
    let error_data = unsafe { error_data_or_error_data_cell_from_abi(error_data) };
    let compact = if compact {
        CompactTraceback::Yes
    } else {
        CompactTraceback::No
    };
    owned_utf16_string_into_abi(error_data.stack_string(compact))
}

/// How many frames the call stack of `error_data` has, the outermost execution context included. Call on the VM's
/// thread.
///
/// # Safety
///
/// `error_data` must be error data, or an error data cell, of a live cell of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_traceback_length(error_data: *const JSErrorData) -> usize {
    // SAFETY: The caller passes error data of a live cell.
    unsafe { error_data_or_error_data_cell_from_abi(error_data) }
        .traceback()
        .len()
}

/// A frame of the call stack of an error, as C++ TracebackFrame: the name of the frame's function, and where in its
/// source the frame was, which is an empty filename at line and column 0 without a source range. The views stay valid
/// for as long as the error data does.
#[repr(C)]
pub struct JSTracebackFrame {
    pub function_name: JSUtf16View,
    pub filename: JSUtf16View,
    pub line: u32,
    pub column: u32,
    pub has_source_range: bool,
}

/// Writes frame `index` of the call stack of `error_data`, counting from the innermost, to `out`. Call on the VM's
/// thread.
///
/// # Safety
///
/// `error_data` must be error data, or an error data cell, of a live cell of the embedder's VM, `index` less than its
/// traceback length, and `out` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_traceback_frame(
    error_data: *const JSErrorData,
    index: usize,
    out: *mut JSTracebackFrame,
) {
    // SAFETY: The caller passes error data of a live cell.
    let error_data = unsafe { error_data_or_error_data_cell_from_abi(error_data) };
    let frame = &error_data.traceback()[index];
    let (filename, line, column) = frame.source_position();
    assert!(!out.is_null(), "the embedder passes an out parameter");
    // SAFETY: The caller passes an out parameter valid for writing.
    unsafe {
        out.write(JSTracebackFrame {
            function_name: JSUtf16View::of(Utf16View::of_string(&frame.function_name)),
            filename: JSUtf16View::of(filename),
            line,
            column,
            has_source_range: frame.cached_source_range.is_some(),
        });
    }
}

/// ErrorDataCell::capture(vm): error data with the current call stack, for a host object that is not an Error, such
/// as a DOMException, to keep alive and return from its class's error_data hook as js_error_data_cell_error_data of
/// the cell. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_cell_capture(vm: *mut JSVM) -> *mut JSErrorDataCell {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    cell_into_abi(ErrorDataCell::capture(vm))
}

/// The error data in `cell`, which stays valid for as long as the cell lives. Call on the VM's thread.
///
/// # Safety
///
/// `cell` must be an error data cell of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_data_cell_error_data(cell: *mut JSErrorDataCell) -> *const JSErrorData {
    // SAFETY: The caller passes a live error data cell.
    let cell = unsafe { cell_from_abi(cell) };
    error_data_into_abi(cell.error_data())
}

/// The TypeError realm override that js_error_type_error_realm_scope_enter replaced, which
/// js_error_type_error_realm_scope_exit puts back. `previous_realm` is null for no override.
#[repr(C)]
pub struct JSTypeErrorRealmScope {
    pub previous_realm: *mut JSRealm,
    pub previous_depth: usize,
}

/// VM::TypeErrorRealmScope: has TypeErrors thrown at the current execution context stack depth created in `realm`,
/// until js_error_type_error_realm_scope_exit with the result. Callees that push execution contexts are unaffected.
/// Scopes nest, and must exit in the reverse order they entered. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `realm` a realm of it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_type_error_realm_scope_enter(
    vm: *mut JSVM,
    realm: *mut JSRealm,
) -> JSTypeErrorRealmScope {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    // SAFETY: The caller passes a realm of the VM.
    let previous = vm.override_type_error_realm(unsafe { cell_from_abi(realm) });
    JSTypeErrorRealmScope {
        previous_realm: optional_cell_into_abi(previous.realm),
        previous_depth: previous.depth,
    }
}

/// Ends the TypeError realm scope that js_error_type_error_realm_scope_enter returned `scope` for. Call on the VM's
/// thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `scope` the result of its innermost js_error_type_error_realm_scope_enter that
/// has not exited.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_error_type_error_realm_scope_exit(vm: *mut JSVM, scope: JSTypeErrorRealmScope) {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.restore_type_error_realm_override(TypeErrorRealmOverride {
        // SAFETY: The scope holds the realm that was overriding before, which its embedder kept alive.
        realm: unsafe { optional_cell_from_abi(scope.previous_realm) },
        depth: scope.previous_depth,
    });
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;

    use ak::Utf16String;

    use super::*;
    use crate::embedding::abi_types::{
        JS_ERROR_KIND_AGGREGATE_ERROR, JS_ERROR_KIND_ERROR, JS_ERROR_KIND_RANGE_ERROR, JS_ERROR_KIND_SUPPRESSED_ERROR,
        JS_ERROR_KIND_TYPE_ERROR, completion_from_abi, owned_utf16_string_into_abi, value_into_abi, vm_into_abi,
    };
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::JS_COMPLETION_THROW;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::native_function::NativeFunction;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::Realm;
    use crate::runtime::realm::test_realm::key;
    use crate::utilities::initialize_realm;

    fn view(text: &[u8]) -> JSUtf16View {
        JSUtf16View::of(Utf16View::Ascii(text))
    }

    fn thrown_object(completion: JSCompletion) -> Gc<Object> {
        assert!(completion.variant == JS_COMPLETION_THROW);
        completion_from_abi(completion)
            .expect_err("the completion throws")
            .value()
            .as_object()
    }

    fn name_and_message(vm: &Vm, error: Gc<Object>) -> String {
        utf8(Value::from_string(
            Value::from_object(error).to_primitive_string(vm).must(),
        ))
    }

    fn frame(error_data: *const JSErrorData, index: usize) -> (String, String, u32, bool) {
        let mut frame = core::mem::MaybeUninit::<JSTracebackFrame>::uninit();
        // SAFETY: The tests pass error data of live cells and indices within their tracebacks, and read the views
        //         while the error data lives.
        unsafe {
            js_error_data_traceback_frame(error_data, index, frame.as_mut_ptr());
            let frame = frame.assume_init();
            (
                frame.function_name.as_view().to_utf8(),
                frame.filename.as_view().to_utf8(),
                frame.line,
                frame.has_source_range,
            )
        }
    }

    fn stack_string(error_data: *const JSErrorData) -> String {
        // SAFETY: The tests pass error data of live cells, and take the string's reference.
        Utf16View::of_string(&unsafe { owned_utf16_string_from_abi(js_error_data_stack_string(error_data, true)) })
            .to_utf8()
    }

    #[test]
    fn thrown_errors_have_their_kind_and_message() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let intrinsics = root_execution_context.realm().intrinsics();
        let abi_vm = vm_into_abi(&vm);
        let kinds = [
            ("Error", intrinsics.error_prototype(&vm)),
            ("EvalError", intrinsics.eval_error_prototype(&vm)),
            ("InternalError", intrinsics.internal_error_prototype(&vm)),
            ("RangeError", intrinsics.range_error_prototype(&vm)),
            ("ReferenceError", intrinsics.reference_error_prototype(&vm)),
            ("SyntaxError", intrinsics.syntax_error_prototype(&vm)),
            ("TypeError", intrinsics.type_error_prototype(&vm)),
            ("URIError", intrinsics.uri_error_prototype(&vm)),
            ("AggregateError", intrinsics.aggregate_error_prototype(&vm)),
            ("SuppressedError", intrinsics.suppressed_error_prototype(&vm)),
        ];
        for (kind, (name, prototype)) in (JS_ERROR_KIND_ERROR..=JS_ERROR_KIND_SUPPRESSED_ERROR).zip(kinds) {
            // SAFETY: The VM has a realm, and the message outlives the call.
            let error = thrown_object(unsafe { js_error_throw(abi_vm, kind, view(b"formatted by the embedder")) });
            assert_eq!(error.class().class_name(), name);
            assert!(error.prototype() == Some(prototype));
            assert_eq!(
                name_and_message(&vm, error),
                format!("{name}: formatted by the embedder")
            );
            // SAFETY: The error is live.
            assert!(unsafe { js_error_is_error(cell_into_abi(error)) });
        }

        // An owned message becomes the error's message without a copy.
        let message = Utf16String::from_utf8("ünïcode message, long enough to live in its own allocation");
        let identity = message.raw_identity();
        // SAFETY: The VM has a realm, and the test gives up its reference to the message.
        let error = thrown_object(unsafe {
            js_error_throw_with_owned_message(abi_vm, JS_ERROR_KIND_TYPE_ERROR, owned_utf16_string_into_abi(message))
        });
        let message = error.get(&vm, &vm.names.message).must().as_string().utf16_string();
        assert_eq!(message.raw_identity(), identity);
    }

    std::thread_local! {
        static CAPTURED_ERROR_DATA_CELL: Cell<*mut JSErrorDataCell> = const { Cell::new(core::ptr::null_mut()) };
    }

    fn define_global_function(vm: &Vm, realm: Gc<Realm>, name: &str, function: Gc<NativeFunction>) {
        realm
            .global_object()
            .define_direct_property(vm, &key(name), Value::from_object(function), DEFAULT_ATTRIBUTES);
    }

    #[test]
    fn errors_and_error_data_cells_capture_the_call_stack() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let thrower = NativeFunction::create(
            &vm,
            (),
            |vm, ()| {
                // SAFETY: The VM runs the function in its realm.
                let completion = unsafe { js_error_throw(vm_into_abi(vm), JS_ERROR_KIND_RANGE_ERROR, view(b"r")) };
                completion_from_abi(completion)
            },
            0,
            &key("thrower"),
            None,
            None,
            None,
        );
        define_global_function(&vm, realm, "thrower", thrower);
        let capture = NativeFunction::create(
            &vm,
            (),
            |vm, ()| {
                // SAFETY: The VM is live.
                let cell = unsafe { js_error_data_cell_capture(vm_into_abi(vm)) };
                CAPTURED_ERROR_DATA_CELL.set(cell);
                Ok(Value::UNDEFINED)
            },
            0,
            &key("capture"),
            None,
            None,
            None,
        );
        define_global_function(&vm, realm, "capture", capture);

        let error = run_script(
            &vm,
            realm,
            "function outer() { thrower() }\ntry { outer() } catch (e) { e }",
        )
        .must()
        .as_object();
        // SAFETY: The error is live.
        let error_data = unsafe { js_error_data_of(cell_into_abi(error)) };
        assert!(!error_data.is_null());
        // SAFETY: The error is live.
        assert_eq!(unsafe { js_error_data_traceback_length(error_data) }, 4);
        // Native functions have no name on the call stack, as in C++.
        assert_eq!(frame(error_data, 0), (String::new(), String::new(), 0, false));
        assert_eq!(frame(error_data, 1), ("outer".into(), "eval".into(), 1, true));
        assert_eq!(frame(error_data, 2), (String::new(), "eval".into(), 2, true));
        assert_eq!(
            stack_string(error_data),
            "    at <unknown>\n    at outer (eval:1:27)\n    at eval:2:12\n"
        );

        run_script(&vm, realm, "function f() { capture() }\nf()").must();
        let cell = CAPTURED_ERROR_DATA_CELL.take();
        vm.heap().collect_garbage();
        // SAFETY: The cell is on the stack, so it outlived the collection.
        let cell_error_data = unsafe { js_error_data_cell_error_data(cell) };
        assert_eq!(frame(cell_error_data, 1).0, "f");
        assert!(stack_string(cell_error_data).starts_with("    at <unknown>\n    at f (eval:1:23)\n"));
        // The cell stands for its error data, as a C++ ErrorDataCell does for the ErrorData it derives from.
        let cell_as_error_data = cell.cast_const().cast::<JSErrorData>();
        assert_eq!(frame(cell_as_error_data, 1).0, "f");
        // SAFETY: The cell and its error data are live.
        let traceback_lengths = unsafe {
            (
                js_error_data_traceback_length(cell_as_error_data),
                js_error_data_traceback_length(cell_error_data),
            )
        };
        assert_eq!(traceback_lengths.0, traceback_lengths.1);
        assert_eq!(stack_string(cell_as_error_data), stack_string(cell_error_data));
        let host_object = realm.global_object();
        // SAFETY: A host object's error_data hook returns the cell's error data, which the object keeps alive.
        let from_hook = unsafe { error_data_from_host_hook(&host_object, cell_error_data.cast_mut().cast()) };
        assert!(from_hook.is_some_and(|from_hook| core::ptr::eq(error_data_into_abi(from_hook), cell_error_data)));
        // SAFETY: Or the cell itself.
        let from_hook = unsafe { error_data_from_host_hook(&host_object, cell.cast()) };
        assert!(from_hook.is_some_and(|from_hook| core::ptr::eq(error_data_into_abi(from_hook), cell_error_data)));
        // SAFETY: A null result of the hook is no error data.
        assert!(unsafe { error_data_from_host_hook(&host_object, core::ptr::null_mut()) }.is_none());
        // SAFETY: The global object is live.
        assert!(unsafe { js_error_data_of(cell_into_abi(realm.global_object())) }.is_null());
    }

    #[test]
    fn errors_are_created_without_throwing() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let abi_vm = vm_into_abi(&vm);
        let abi_realm = cell_into_abi(realm);
        let options = run_script(&vm, realm, "({ cause: 42 })").must();
        let throwing_options = run_script(&vm, realm, "({ get cause() { throw 7 } })").must();
        let prototype = run_script(
            &vm,
            realm,
            "Object.create(Error.prototype, { name: { value: 'WebAssembly.LinkError' } })",
        )
        .must()
        .as_object();

        // SAFETY: The VM, realm, objects and values are live, and the messages outlive the calls.
        unsafe {
            let error = js_error_create(abi_vm, abi_realm, JS_ERROR_KIND_AGGREGATE_ERROR);
            let error_object = cell_from_abi(error);
            assert_eq!(name_and_message(&vm, error_object), "AggregateError");
            assert!(!error_object.has_own_property(&vm, &vm.names.message).must());
            js_error_set_message(abi_vm, error, view(b"set later"));
            assert_eq!(name_and_message(&vm, error_object), "AggregateError: set later");
            let completion = js_error_install_error_cause(abi_vm, error, value_into_abi(options));
            assert!(completion_from_abi(completion).is_ok());
            assert!(error_object.get(&vm, &vm.names.cause).must() == Value::from_i32(42));
            let completion = js_error_install_error_cause(abi_vm, error, value_into_abi(throwing_options));
            assert!(completion_from_abi(completion).expect_err("the getter throws").value() == Value::from_i32(7));

            let link_error = js_error_create_with_prototype(abi_vm, abi_realm, cell_into_abi(prototype));
            assert!(js_error_is_error(link_error));
            let link_error_object = cell_from_abi(link_error);
            assert!(link_error_object.prototype() == Some(prototype));
            js_error_set_owned_message(
                abi_vm,
                link_error,
                owned_utf16_string_into_abi(Utf16String::from_utf8("import failed")),
            );
            assert_eq!(
                name_and_message(&vm, link_error_object),
                "WebAssembly.LinkError: import failed"
            );
            assert!(!js_error_is_error(cell_into_abi(prototype)));
        }
    }

    #[test]
    fn a_type_error_realm_scope_overrides_the_realm_of_type_errors_until_it_exits() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let other_realm_context = Realm::initialize_host_defined_realm(&vm, None, None).must();
        let other_realm = other_realm_context.realm.get().expect("the context has a realm");
        vm.pop_execution_context();
        let abi_vm = vm_into_abi(&vm);
        let thrown_prototype = |kind| {
            // SAFETY: The VM has a realm.
            thrown_object(unsafe { js_error_throw(abi_vm, kind, view(b"m")) }).prototype()
        };
        let type_error_prototype = |realm: Gc<Realm>| Some(realm.intrinsics().type_error_prototype(&vm));

        // SAFETY: The VM and realms are live, and the scopes exit in reverse order.
        unsafe {
            let outer_scope = js_error_type_error_realm_scope_enter(abi_vm, cell_into_abi(other_realm));
            assert!(outer_scope.previous_realm.is_null());
            assert!(thrown_prototype(JS_ERROR_KIND_TYPE_ERROR) == type_error_prototype(other_realm));
            assert!(thrown_prototype(JS_ERROR_KIND_RANGE_ERROR) == Some(realm.intrinsics().range_error_prototype(&vm)));

            let inner_scope = js_error_type_error_realm_scope_enter(abi_vm, cell_into_abi(realm));
            assert!(inner_scope.previous_realm == cell_into_abi(other_realm));
            assert!(thrown_prototype(JS_ERROR_KIND_TYPE_ERROR) == type_error_prototype(realm));
            js_error_type_error_realm_scope_exit(abi_vm, inner_scope);
            assert!(thrown_prototype(JS_ERROR_KIND_TYPE_ERROR) == type_error_prototype(other_realm));

            js_error_type_error_realm_scope_exit(abi_vm, outer_scope);
            assert!(thrown_prototype(JS_ERROR_KIND_TYPE_ERROR) == type_error_prototype(realm));
        }
    }
}
