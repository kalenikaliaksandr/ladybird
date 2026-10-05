/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The source code that scripts, modules and functions are compiled from, which embedders share with the runtime by
//! reference counting, as C++ shares RefPtr<SourceCode const>.
//!
//! A JSSourceCode is the SourceCode of an Rc, and every function that hands one out returns the same pointer for the
//! same source code, so its address identifies it. Its reference count is not atomic: only the VM's thread may retain or
//! release it, or read it. The code itself is an AK::Utf16String, whose storage any thread may read and release, so an
//! embedder hands the code to a worker thread as the owned string js_source_code_code() returns.

use std::rc::Rc;

use crate::bytecode::executable::Executable;
use crate::embedding::abi_types::{
    JSOwnedUtf16String, JSSourceCode, owned_utf16_string_from_abi, owned_utf16_string_into_abi,
};
use crate::embedding::execution_context::JSExecutionContext;
use crate::layout::execution_context::ExecutionContext;
use crate::source_code::SourceCode;

pub fn source_code_into_abi(source_code: &Rc<SourceCode>) -> *const JSSourceCode {
    Rc::as_ptr(source_code).cast()
}

/// # Safety
///
/// `source_code` must be the SourceCode of a live Rc, such as one the ABI handed out, which stays alive for `'a`.
pub unsafe fn source_code_from_abi<'a>(source_code: *const JSSourceCode) -> &'a SourceCode {
    assert!(!source_code.is_null(), "the embedder passes source code");
    // SAFETY: The caller guarantees that the pointer is that of a live SourceCode.
    unsafe { &*source_code.cast::<SourceCode>() }
}

/// # Safety
///
/// As for source_code_from_abi(). The returned Rc is one more owner of the source code.
pub unsafe fn shared_source_code_from_abi(source_code: *const JSSourceCode) -> Rc<SourceCode> {
    let source_code = source_code.cast::<SourceCode>();
    assert!(!source_code.is_null(), "the embedder passes source code");
    // SAFETY: The caller guarantees that the pointer is that of a live Rc, which then has one more owner.
    unsafe {
        Rc::increment_strong_count(source_code);
        Rc::from_raw(source_code)
    }
}

/// SourceCode::create(filename, code): new source code whose one reference the caller owns and gives up with
/// js_source_code_release(). Adopts both strings, which the caller gives up with AK::Utf16String::into_raw(). Only the
/// VM's thread may call this.
///
/// # Safety
///
/// Both strings must be raw AK::Utf16Strings whose references the caller gives up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_create(
    filename: JSOwnedUtf16String,
    code: JSOwnedUtf16String,
) -> *const JSSourceCode {
    // SAFETY: The caller transfers both references.
    let (filename, code) = unsafe { (owned_utf16_string_from_abi(filename), owned_utf16_string_from_abi(code)) };
    Rc::into_raw(SourceCode::create(filename, code)).cast()
}

/// Adds a reference to the source code, which the caller gives up with js_source_code_release(). Only the VM's thread
/// may call this.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_retain(source_code: *const JSSourceCode) {
    assert!(!source_code.is_null(), "the embedder passes source code");
    // SAFETY: The caller passes the SourceCode of a live Rc.
    unsafe { Rc::increment_strong_count(source_code.cast::<SourceCode>()) };
}

/// Gives up a reference to the source code that js_source_code_create() or js_source_code_retain() gave the caller.
/// Only the VM's thread may call this.
///
/// # Safety
///
/// `source_code` must be live source code of which the caller owns a reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_release(source_code: *const JSSourceCode) {
    assert!(!source_code.is_null(), "the embedder passes source code");
    // SAFETY: The caller gives up one of its references to the SourceCode of a live Rc.
    unsafe { Rc::decrement_strong_count(source_code.cast::<SourceCode>()) };
}

/// SourceCode::filename(): the name that the code's stack frames report, as an owned AK::Utf16String the caller adopts
/// with AK::Utf16String::adopt_raw(). Only the VM's thread may call this; the string may then go to any thread.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_filename(source_code: *const JSSourceCode) -> JSOwnedUtf16String {
    // SAFETY: The caller passes live source code.
    let source_code = unsafe { source_code_from_abi(source_code) };
    owned_utf16_string_into_abi(source_code.filename().clone())
}

/// SourceCode::code(): the source text, as an owned AK::Utf16String that shares the source code's storage and that the
/// caller adopts with AK::Utf16String::adopt_raw(). Only the VM's thread may call this; the string may then go to any
/// thread, such as a worker that parses it with js_compile_parse().
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_code(source_code: *const JSSourceCode) -> JSOwnedUtf16String {
    // SAFETY: The caller passes live source code.
    let source_code = unsafe { source_code_from_abi(source_code) };
    owned_utf16_string_into_abi(source_code.code().clone())
}

/// SourceCode::length_in_code_units(). Only the VM's thread may call this.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_length_in_code_units(source_code: *const JSSourceCode) -> usize {
    // SAFETY: The caller passes live source code.
    unsafe { source_code_from_abi(source_code) }.length_in_code_units()
}

/// SourceCode::filename() as C++ returns it, by reference: the address of the source code's AK::Utf16String, which
/// stays where it is, unchanged, for as long as the source code lives. Only the VM's thread may call this.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_filename_address(
    source_code: *const JSSourceCode,
) -> *const JSOwnedUtf16String {
    // SAFETY: The caller passes live source code.
    let source_code = unsafe { source_code_from_abi(source_code) };
    core::ptr::from_ref(source_code.filename()).cast()
}

/// SourceCode::code() as C++ returns it, by reference: the address of the source code's AK::Utf16String, which stays
/// where it is, unchanged, for as long as the source code lives. Only the VM's thread may call this.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_code_address(source_code: *const JSSourceCode) -> *const JSOwnedUtf16String {
    // SAFETY: The caller passes live source code.
    let source_code = unsafe { source_code_from_abi(source_code) };
    core::ptr::from_ref(source_code.code()).cast()
}

/// SourceCode::utf16_data(): the js_source_code_length_in_code_units() UTF-16 code units of the code, widened on the
/// first call for code in the ASCII storage kind. They stay where they are, unchanged, for as long as the source code
/// lives, so a worker thread may read them while the VM's thread keeps the source code alive, as it may parse them with
/// js_compile_parse(). Null for empty code. Only the VM's thread may call this.
///
/// # Safety
///
/// `source_code` must be live source code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_utf16_data(source_code: *const JSSourceCode) -> *const u16 {
    // SAFETY: The caller passes live source code.
    let code_units = unsafe { source_code_from_abi(source_code) }.utf16_code_units();
    if code_units.is_empty() {
        return core::ptr::null();
    }
    code_units.as_ptr()
}

/// ExecutionContext::source_code(): the source code of the bytecode the context runs, or null if it runs none, as for a
/// native function, or if that bytecode has no source code. The context's executable keeps it alive, and a caller that
/// keeps it longer retains it. Only the VM's thread may call this.
///
/// # Safety
///
/// `execution_context` must be a live execution context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_source_code_of_execution_context(
    execution_context: *const JSExecutionContext,
) -> *const JSSourceCode {
    assert!(!execution_context.is_null(), "the embedder passes an execution context");
    // SAFETY: The caller passes a live execution context.
    let execution_context = unsafe { &*execution_context.cast::<ExecutionContext>() };
    let Some(executable) = execution_context.executable.get() else {
        return core::ptr::null();
    };
    Executable::from_head(executable)
        .source_code
        .as_ref()
        .map_or(core::ptr::null(), source_code_into_abi)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;
    use core::ffi::c_void;

    use super::*;
    use crate::embedding::abi_types::vm_into_abi;
    use crate::embedding::execution_context::{js_execution_context_last_matching, js_execution_context_running};
    use crate::interpreter::vm::Vm;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::native_function::NativeFunction;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::test_realm::key;
    use crate::script::Script;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    std::thread_local! {
        static SOURCE_CODE_OF_THE_RUNNING_NATIVE_FUNCTION: Cell<*const JSSourceCode> = const { Cell::new(core::ptr::null()) };
        static SOURCE_CODE_OF_ITS_CALLER: Cell<*const JSSourceCode> = const { Cell::new(core::ptr::null()) };
    }

    unsafe extern "C" fn runs_bytecode_with_source_code(
        _: *mut c_void,
        execution_context: *mut JSExecutionContext,
    ) -> bool {
        // SAFETY: The predicate receives live execution contexts.
        !unsafe { js_source_code_of_execution_context(execution_context) }.is_null()
    }

    #[test]
    fn execution_contexts_report_the_source_code_of_their_bytecode() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let capture = NativeFunction::create(
            &vm,
            (),
            |vm, ()| {
                let vm = vm_into_abi(vm);
                // SAFETY: The VM is live and runs this function, whose caller is a script.
                unsafe {
                    SOURCE_CODE_OF_THE_RUNNING_NATIVE_FUNCTION
                        .set(js_source_code_of_execution_context(js_execution_context_running(vm)));
                    let caller = js_execution_context_last_matching(
                        vm,
                        Some(runs_bytecode_with_source_code),
                        core::ptr::null_mut(),
                    );
                    SOURCE_CODE_OF_ITS_CALLER.set(js_source_code_of_execution_context(caller));
                }
                Ok(Value::UNDEFINED)
            },
            0,
            &key("capture"),
            None,
            None,
            None,
        );
        realm.global_object().define_direct_property(
            &vm,
            &key("capture"),
            Value::from_object(capture),
            DEFAULT_ATTRIBUTES,
        );
        let source: Vec<u16> = "(function caller() { capture(); })()".encode_utf16().collect();
        let script = Script::parse_with_filename(&vm, &source, realm, "caller.js").expect("the script parses");
        vm.run_script(script, None).must();

        assert!(SOURCE_CODE_OF_THE_RUNNING_NATIVE_FUNCTION.get().is_null());
        let caller_source_code = SOURCE_CODE_OF_ITS_CALLER.get();
        let script_source_code = script
            .cached_executable()
            .source_code
            .clone()
            .expect("the script has source code");
        assert_eq!(caller_source_code, source_code_into_abi(&script_source_code));
        // SAFETY: The script keeps its source code alive.
        let filename = unsafe { js_source_code_filename(caller_source_code) };
        assert_eq!(utf8_of_owned(filename), "caller.js");
    }

    fn utf8_of_owned(string: JSOwnedUtf16String) -> String {
        // SAFETY: The string is a raw owned string the ABI handed out.
        let string = unsafe { owned_utf16_string_from_abi(string) };
        Utf16View::of_string(&string).to_utf8()
    }

    #[test]
    fn source_code_is_shared_by_reference_counting() {
        let code = ak::Utf16String::from_utf8("let \u{3b1} = 1;");
        let code_identity = code.raw_identity();
        // SAFETY: Both strings give up their references.
        let source_code = unsafe {
            js_source_code_create(
                owned_utf16_string_into_abi(ak::Utf16String::from_utf8("file.js")),
                owned_utf16_string_into_abi(code),
            )
        };
        // SAFETY: The source code is live, and the test owns a reference.
        unsafe {
            assert_eq!(utf8_of_owned(js_source_code_filename(source_code)), "file.js");
            let shared_code = js_source_code_code(source_code);
            assert_eq!(shared_code, code_identity, "the code is shared, not copied");
            assert_eq!(utf8_of_owned(shared_code), "let \u{3b1} = 1;");
            assert_eq!(js_source_code_length_in_code_units(source_code), 10);

            js_source_code_retain(source_code);
            let rc = shared_source_code_from_abi(source_code);
            assert_eq!(Rc::strong_count(&rc), 3);
            drop(rc);
            js_source_code_release(source_code);
            assert_eq!(Rc::strong_count(&shared_source_code_from_abi(source_code)), 2);
            js_source_code_release(source_code);
        }
    }

    fn source_code_from_strings(filename: &str, code: ak::Utf16String) -> *const JSSourceCode {
        // SAFETY: Both strings give up their references.
        unsafe {
            js_source_code_create(
                owned_utf16_string_into_abi(ak::Utf16String::from_utf8(filename)),
                owned_utf16_string_into_abi(code),
            )
        }
    }

    #[test]
    fn source_code_lends_its_strings_and_utf16_code_units_in_place() {
        let ascii_code = ak::Utf16String::from_utf8("let a = 1;");
        let ascii_code_identity = ascii_code.raw_identity();
        let ascii_source_code = source_code_from_strings("ascii.js", ascii_code);
        let utf16_code = ak::Utf16String::from_utf8("let \u{3b1} = 1;");
        let utf16_source_code = source_code_from_strings("utf16.js", utf16_code);
        let empty_source_code = source_code_from_strings("empty.js", ak::Utf16String::default());

        // SAFETY: The source codes are live, and the test owns a reference to each.
        unsafe {
            let code_address = js_source_code_code_address(ascii_source_code);
            assert_eq!(
                *code_address, ascii_code_identity,
                "the address is that of the code itself"
            );
            assert_eq!(js_source_code_code_address(ascii_source_code), code_address);
            let filename = &*js_source_code_filename_address(ascii_source_code).cast::<ak::Utf16String>();
            assert_eq!(Utf16View::of_string(filename).to_utf8(), "ascii.js");

            let widened = js_source_code_utf16_data(ascii_source_code);
            let widened_code_units = core::slice::from_raw_parts(widened, 10);
            assert_eq!(widened_code_units, "let a = 1;".encode_utf16().collect::<Vec<u16>>());
            assert_eq!(
                js_source_code_utf16_data(ascii_source_code),
                widened,
                "the widened code is kept"
            );

            let utf16_data = js_source_code_utf16_data(utf16_source_code);
            let Utf16View::Utf16(stored_code_units) =
                Utf16View::of_string(&*js_source_code_code_address(utf16_source_code).cast::<ak::Utf16String>())
            else {
                panic!("non-ASCII code is stored as UTF-16");
            };
            assert_eq!(utf16_data, stored_code_units.as_ptr(), "UTF-16 code is not copied");

            assert!(js_source_code_utf16_data(empty_source_code).is_null());

            js_source_code_release(ascii_source_code);
            js_source_code_release(utf16_source_code);
            js_source_code_release(empty_source_code);
        }
    }
}
