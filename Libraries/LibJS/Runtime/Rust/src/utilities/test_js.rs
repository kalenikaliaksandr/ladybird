/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! test-js-runtime-rust, the runner of the LibJS runtime tests in Tests/LibJS, as LibTest's JavaScriptTestRunner and
//! Tests/LibJS/test-js.cpp are for the C++ runtime.

use core::ffi::{c_char, c_int};

use crate::interpreter::runtime_functions::unimplemented_runtime_function;

/// The entry point the C++ main of test-js-runtime-rust calls.
///
/// # Safety
///
/// `argv` must point to `argc` valid C strings, as main() receives them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libjs_runtime_rust_test_js_main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    unimplemented_runtime_function("the LibJS runtime test runner", 0)
}
