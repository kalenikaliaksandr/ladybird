/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Classic scripts.

use crate::embedding::abi_types::{JSRealm, JSUtf16View, cell_from_abi, completion_into_abi, vm_from_abi};
use crate::layout::host_class::{JSCompletion, JSVM};
use crate::runtime::error::ErrorKind;
use crate::script::Script;

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
