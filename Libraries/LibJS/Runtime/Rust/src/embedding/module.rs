/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Module records: creating them, loading, linking and evaluating module graphs, the module requests that
//! HostLoadImportedModule receives and FinishLoadingImportedModule completes, and module namespaces.
//!
//! The exported functions run on the thread that owns the VM and trust their arguments, as object.rs states: `vm` is
//! the embedder's VM; modules, realms and the records of referrers and payloads are live cells of its heap;
//! host-defined cells are null or live cells of its heap; module requests are ones the runtime lent or the embedder
//! owns; and views and out parameters are valid. Cells these functions return are not rooted.
//!
//! Loading, linking and evaluating create promises and run JavaScript, so like HTML, which prepares to run script
//! first, the embedder calls them with an execution context of the module's realm running.

#![allow(
    clippy::missing_safety_doc,
    reason = "the module documentation states the contract every exported function shares"
)]

use core::ffi::c_void;

use crate::embedding::abi_types::{
    CellAbi, JSRealm, JSUtf16View, append_to_string_sink, cell_from_abi, cell_into_abi, completion_from_abi,
    completion_into_abi, object_into_abi, optional_cell_into_abi, vm_from_abi,
};
use crate::embedding::environment::JSEnvironment;
use crate::embedding::hooks::{
    JSImportedModulePayload, JSImportedModuleReferrer, JSModuleRequest, imported_module_payload_from_abi,
    imported_module_referrer_from_abi, module_request_from_abi, module_request_into_abi,
};
use crate::embedding::realm::host_defined_slot_of;
use crate::embedding::script::{JSParserErrorSink, append_to_parser_error_sink};
use crate::layout::cell::Gc;
use crate::layout::host_class::{
    JS_RESOLVED_BINDING_AMBIGUOUS, JS_RESOLVED_BINDING_BINDING_NAME, JS_RESOLVED_BINDING_NAMESPACE,
    JS_RESOLVED_BINDING_NULL, JSCompletion, JSModule, JSObject, JSPromiseCapability, JSResolvedBinding, JSStringSink,
    JSVM, JSValue,
};
use crate::layout::value::Value;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::cyclic_module::CyclicModule;
use crate::runtime::module::{Module, ResolvedBindingType, finish_loading_imported_module};
use crate::runtime::module_request::{ImportAttribute, ModuleRequest};
use crate::runtime::promise_capability::PromiseCapability;
use crate::runtime::source_text_module::SourceTextModule;
use crate::runtime::synthetic_module::{SyntheticModule, create_text_module, parse_json_module};
use crate::source_code::SourceCode;
use crate::utf16::Utf16View;

impl CellAbi for JSModule {
    type Cell = Module;
}

pub(crate) fn promise_capability_into_abi(capability: Gc<PromiseCapability>) -> *mut JSPromiseCapability {
    capability.as_ptr().cast()
}

fn cyclic_module_of(module: &Module) -> &CyclicModule {
    module
        .as_cyclic_module()
        .expect("only Cyclic Module Records have requested modules")
}

// Module requests

/// An ImportAttribute Record of a module request, as views of its key and value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSImportAttribute {
    pub key: JSUtf16View,
    pub value: JSUtf16View,
}

fn owned_module_request_into_abi(module_request: ModuleRequest) -> *mut JSModuleRequest {
    Box::into_raw(Box::new(module_request)).cast()
}

/// A new ModuleRequest Record with the specifier and the `attribute_count` attributes of `attributes`, in the order
/// given, which the embedder owns until it destroys it. Copies the strings. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_create(
    specifier: JSUtf16View,
    attributes: *const JSImportAttribute,
    attribute_count: usize,
) -> *mut JSModuleRequest {
    let attributes = if attribute_count == 0 {
        &[]
    } else {
        // SAFETY: The embedder passes that many attributes.
        unsafe { core::slice::from_raw_parts(attributes, attribute_count) }
    };
    // SAFETY: See the module documentation.
    let module_request = unsafe {
        ModuleRequest {
            module_specifier: specifier.as_view().to_utf16_fly_string(),
            attributes: attributes
                .iter()
                .map(|attribute| {
                    ImportAttribute::new(
                        attribute.key.as_view().to_utf16_string(),
                        attribute.value.as_view().to_utf16_string(),
                    )
                })
                .collect(),
        }
    };
    owned_module_request_into_abi(module_request)
}

/// A copy of the module request that the embedder owns until it destroys it, such as one to keep while it loads the
/// module that a request the runtime lent it asks for. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_clone(module_request: *const JSModuleRequest) -> *mut JSModuleRequest {
    // SAFETY: See the module documentation.
    owned_module_request_into_abi(unsafe { module_request_from_abi(module_request) }.clone())
}

/// Destroys a module request that js_module_request_create() or js_module_request_clone() returned. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_destroy(module_request: *mut JSModuleRequest) {
    assert!(!module_request.is_null(), "the embedder passes a module request");
    // SAFETY: The embedder passes a module request it owns, which came out of a Box.
    drop(unsafe { Box::from_raw(module_request.cast::<ModuleRequest>()) });
}

/// The [[Specifier]] of the module request, valid while the request lives. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_specifier(module_request: *const JSModuleRequest) -> JSUtf16View {
    // SAFETY: See the module documentation.
    let module_request = unsafe { module_request_from_abi(module_request) };
    JSUtf16View::of(Utf16View::of_fly_string(&module_request.module_specifier))
}

/// The number of [[Attributes]] of the module request. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_attribute_count(module_request: *const JSModuleRequest) -> usize {
    // SAFETY: See the module documentation.
    unsafe { module_request_from_abi(module_request) }.attributes.len()
}

/// The attribute at `index` of the module request's [[Attributes]], whose strings are valid while the request lives.
/// Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_attribute(
    module_request: *const JSModuleRequest,
    index: usize,
) -> JSImportAttribute {
    // SAFETY: See the module documentation.
    let attribute = &unsafe { module_request_from_abi(module_request) }.attributes[index];
    JSImportAttribute {
        key: JSUtf16View::of(Utf16View::of_string(&attribute.key)),
        value: JSUtf16View::of(Utf16View::of_string(&attribute.value)),
    }
}

/// Whether the two module requests have the same specifier and the same attributes in the same order, as the C++
/// ModuleRequest's operator== compares them. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_request_equals(left: *const JSModuleRequest, right: *const JSModuleRequest) -> bool {
    // SAFETY: See the module documentation.
    unsafe { module_request_from_abi(left) == module_request_from_abi(right) }
}

// Creating module records

/// SourceTextModule::parse(source_text, realm, filename, display_filename, host_defined, line_number_offset):
/// ParseModule ( sourceText, realm, hostDefined ) for `source`, which starts `line_number_offset` lines into the text
/// it was taken from. Module loading resolves the module's imports against `filename`, and its code reports
/// `display_filename`, or the filename if that is empty, in its stack frames and errors. `host_defined` is null or one
/// of the embedder's cells, which the module keeps alive as its [[HostDefined]]. Returns an unrooted module, or null
/// after appending the syntax errors to `errors` (which may be null). Main thread only.
#[unsafe(no_mangle)]
#[allow(
    clippy::too_many_arguments,
    reason = "C++ SourceTextModule::parse takes all of these"
)]
pub unsafe extern "C" fn js_module_parse_source_text_module(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    source: JSUtf16View,
    filename: JSUtf16View,
    display_filename: JSUtf16View,
    host_defined: *mut c_void,
    line_number_offset: usize,
    errors: *const JSParserErrorSink,
) -> *mut JSModule {
    // SAFETY: See the module documentation.
    let (vm, realm, source, filename, display_filename, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            source.as_view(),
            filename.as_view(),
            display_filename.as_view(),
            host_defined_slot_of(host_defined),
        )
    };
    let display_filename = if display_filename.is_empty() {
        filename.to_utf16_string()
    } else {
        display_filename.to_utf16_string()
    };
    let source_code = SourceCode::create(display_filename, source.to_utf16_string());
    match SourceTextModule::parse_with_host_defined(
        vm,
        source_code,
        realm,
        &filename.to_utf8(),
        host_defined,
        line_number_offset,
    ) {
        Ok(module) => cell_into_abi(module.upcast::<Module>()),
        Err(parser_errors) => {
            // SAFETY: See the module documentation.
            unsafe { append_to_parser_error_sink(errors, &parser_errors) };
            core::ptr::null_mut()
        }
    }
}

/// ParseJSONModule ( source ): a normal completion whose payload is the new Synthetic Module Record, which exports the
/// parsed value as its default, or the throw completion of ParseJSON. Like ParseJSON, it creates the value and any
/// SyntaxError in the current realm, so the embedder calls it with an execution context of `realm` running, as HTML
/// does with a TemporaryExecutionContext. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_parse_json_module(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    source: JSUtf16View,
    filename: JSUtf16View,
) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, realm, source, filename) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            source.as_view(),
            filename.as_view().to_utf8(),
        )
    };
    let module = parse_json_module(vm, realm, source, filename);
    completion_into_abi(module.map(|module| cell_into_abi::<JSModule>(module.upcast())))
}

/// CreateTextModule ( source ): a new Synthetic Module Record whose default export is the text. Returns an unrooted
/// module. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_create_text_module(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    text: JSUtf16View,
    filename: JSUtf16View,
) -> *mut JSModule {
    // SAFETY: See the module documentation.
    let (vm, realm, text, filename) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            text.as_view(),
            filename.as_view().to_utf8(),
        )
    };
    cell_into_abi(create_text_module(vm, realm, text, filename).upcast::<Module>())
}

/// CreateDefaultExportSyntheticModule ( defaultExport ): a new Synthetic Module Record whose default export is the
/// value, as CSS module scripts export their style sheet. Returns an unrooted module. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_create_default_export_synthetic_module(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    default_export: JSValue,
    filename: JSUtf16View,
) -> *mut JSModule {
    // SAFETY: See the module documentation.
    let (vm, realm, filename) = unsafe { (vm_from_abi(vm), cell_from_abi(realm), filename.as_view().to_utf8()) };
    let module = SyntheticModule::create_default_export_synthetic_module(vm, realm, Value(default_export), filename);
    cell_into_abi(module.upcast::<Module>())
}

// The fields of module records

/// The id of the module's class, one of Layout.h's JS_LAYOUT_CLASS_ID_* values: SOURCE_TEXT_MODULE,
/// SYNTHETIC_MODULE or HOST_MODULE. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_class_id(module: *mut JSModule) -> u16 {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSModule>(module) }.class().id as u16
}

/// The module's [[Realm]]. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_realm(module: *mut JSModule) -> *mut JSRealm {
    // SAFETY: See the module documentation.
    cell_into_abi(unsafe { cell_from_abi::<JSModule>(module) }.realm())
}

/// The module's [[HostDefined]] cell, or null if it has none. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_host_defined(module: *mut JSModule) -> *mut c_void {
    // SAFETY: See the module documentation.
    unsafe { cell_from_abi::<JSModule>(module) }
        .host_defined()
        .map_or(core::ptr::null_mut(), core::ptr::NonNull::as_ptr)
}

/// The module's [[Environment]], a module environment, or null before linking creates it. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_environment(module: *mut JSModule) -> *mut JSEnvironment {
    // SAFETY: See the module documentation.
    let environment = unsafe { cell_from_abi::<JSModule>(module) }.environment();
    optional_cell_into_abi::<JSEnvironment>(environment.map(Gc::upcast))
}

/// The number of [[RequestedModules]] of a Cyclic Module Record. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_requested_module_count(module: *mut JSModule) -> usize {
    // SAFETY: See the module documentation.
    let module = unsafe { cell_from_abi::<JSModule>(module) };
    cyclic_module_of(&module).requested_modules().len()
}

/// The module request at `index` of a Cyclic Module Record's [[RequestedModules]], which the module lends for as long
/// as it lives. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_requested_module(module: *mut JSModule, index: usize) -> *const JSModuleRequest {
    // SAFETY: See the module documentation.
    let module = unsafe { cell_from_abi::<JSModule>(module) };
    module_request_into_abi(&cyclic_module_of(&module).requested_modules()[index])
}

/// GetImportedModule ( referrer, request ): the module that loading the requested modules of the Cyclic Module Record
/// `referrer` found for `module_request`. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_get_imported_module(
    referrer: *mut JSModule,
    module_request: *const JSModuleRequest,
) -> *mut JSModule {
    // SAFETY: See the module documentation.
    let (referrer, module_request) = unsafe {
        (
            cell_from_abi::<JSModule>(referrer),
            module_request_from_abi(module_request),
        )
    };
    cell_into_abi(cyclic_module_of(&referrer).get_imported_module(module_request))
}

// Loading, linking and evaluating module graphs

/// LoadRequestedModules ( [ hostDefined ] ) with `host_defined`, or without it for null: loads the module's graph
/// through the embedder's load_imported_module hook, which receives `host_defined` for every module of the graph.
/// Returns the promise capability, unrooted, whose promise settles once loading finishes or fails. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_load_requested_modules(
    vm: *mut JSVM,
    module: *mut JSModule,
    host_defined: *mut c_void,
) -> *mut JSPromiseCapability {
    // SAFETY: See the module documentation.
    let (vm, module, host_defined) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSModule>(module),
            host_defined_slot_of(host_defined),
        )
    };
    promise_capability_into_abi(module.load_requested_modules(vm, host_defined))
}

/// Link ( ) of a module whose graph has loaded, with an unused payload. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_link(vm: *mut JSVM, module: *mut JSModule) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, module) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSModule>(module)) };
    completion_into_abi(module.link(vm))
}

/// Evaluate ( ) of a linked module: a normal completion whose payload is the unrooted promise capability whose promise
/// settles once the module's graph has evaluated. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_evaluate(vm: *mut JSVM, module: *mut JSModule) -> JSCompletion {
    // SAFETY: See the module documentation.
    let (vm, module) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSModule>(module)) };
    completion_into_abi(module.evaluate(vm).map(promise_capability_into_abi))
}

/// FinishLoadingImportedModule ( referrer, moduleRequest, payload, result ), for a referrer, module request and payload
/// that the load_imported_module hook received, sooner or later after it did, and that the embedder kept alive in
/// between. A normal `result` carries the loaded module as its payload, and a throw completion the reason loading
/// failed. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_finish_loading_imported_module(
    vm: *mut JSVM,
    referrer: JSImportedModuleReferrer,
    module_request: *const JSModuleRequest,
    payload: JSImportedModulePayload,
    result: JSCompletion,
) {
    let result: ThrowCompletionOr<Gc<Module>> = completion_from_abi(result).map(|module| {
        let module = core::ptr::with_exposed_provenance_mut::<JSModule>(module.0 as usize);
        // SAFETY: A normal result carries a live module.
        unsafe { cell_from_abi(module) }
    });
    // SAFETY: See the module documentation.
    let (vm, referrer, module_request, payload) = unsafe {
        (
            vm_from_abi(vm),
            imported_module_referrer_from_abi(referrer),
            module_request_from_abi(module_request),
            imported_module_payload_from_abi(payload),
        )
    };
    finish_loading_imported_module(vm, referrer, module_request, payload, result);
}

// Exports and namespaces

/// GetModuleNamespace ( module ): the module's namespace object, created on first use. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_get_module_namespace(vm: *mut JSVM, module: *mut JSModule) -> *mut JSObject {
    // SAFETY: See the module documentation.
    let (vm, module) = unsafe { (vm_from_abi(vm), cell_from_abi::<JSModule>(module)) };
    object_into_abi(module.get_module_namespace(vm))
}

/// GetExportedNames ( ) of a module whose graph has loaded: appends each name to `names`. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_get_exported_names(
    vm: *mut JSVM,
    module: *mut JSModule,
    names: *const JSStringSink,
) {
    // SAFETY: See the module documentation.
    let (vm, module, names) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSModule>(module),
            names.as_ref().expect("the embedder passes a sink"),
        )
    };
    for name in module.get_exported_names(vm) {
        append_to_string_sink(names, Utf16View::of_fly_string(&name));
    }
}

fn resolved_binding_type_into_abi(binding_type: ResolvedBindingType) -> u8 {
    match binding_type {
        ResolvedBindingType::BindingName => JS_RESOLVED_BINDING_BINDING_NAME,
        ResolvedBindingType::Namespace => JS_RESOLVED_BINDING_NAMESPACE,
        ResolvedBindingType::Ambiguous => JS_RESOLVED_BINDING_AMBIGUOUS,
        ResolvedBindingType::Null => JS_RESOLVED_BINDING_NULL,
    }
}

/// ResolveExport ( exportName ) of a module whose graph has loaded, which fills in `out` as a host module's
/// resolve_export hook does: it sets the type and the module, and appends the binding name to out->binding_name once
/// for a binding of type JS_RESOLVED_BINDING_BINDING_NAME. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_module_resolve_export(
    vm: *mut JSVM,
    module: *mut JSModule,
    export_name: JSUtf16View,
    out: *mut JSResolvedBinding,
) {
    // SAFETY: See the module documentation.
    let (vm, module, export_name, out) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi::<JSModule>(module),
            export_name.as_view().to_utf16_fly_string(),
            out.as_mut().expect("the embedder passes an out record"),
        )
    };
    let binding = module.resolve_export(vm, &export_name);
    out.r#type = resolved_binding_type_into_abi(binding.binding_type);
    out.module = optional_cell_into_abi::<JSModule>(binding.module);
    if binding.binding_type == ResolvedBindingType::BindingName {
        append_to_string_sink(&out.binding_name, Utf16View::of_fly_string(&binding.export_name));
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::RefCell;

    use super::*;
    use crate::embedding::abi_types::{JSOwnedUtf16String, owned_utf16_string_from_abi, vm_into_abi};
    use crate::embedding::hooks::{
        JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE, JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY,
        JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE, JS_IMPORTED_MODULE_REFERRER_SCRIPT, JSVmHostHooks,
    };
    use crate::embedding::host::host_module::js_host_module_create;
    use crate::embedding::host::host_module::tests::{EXPORTING_MODULE_CLASS, TestVm};
    use crate::embedding::vm::js_vm_set_embedder;
    use crate::gc::class_id::ClassId;
    use crate::gc::root::MarkedVec;
    use crate::interpreter::vm::Vm;
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::layout::object::Object;
    use crate::layout::realm::Realm;
    use crate::runtime::completion::Must;
    use crate::runtime::cyclic_module::promise_of;
    use crate::runtime::error::ErrorKind;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::module_loading::{ImportedModulePayload, ImportedModuleReferrer};
    use crate::runtime::promise::{Promise, PromiseState};
    use crate::runtime::property_key::PropertyKey;
    use crate::utilities::initialize_realm;

    /// Runs from the callbacks while the VM waits for them, which would find any borrow of VM state held across the
    /// call.
    const REENTRANT_SCRIPT: &str = r#"
        globalThis.reentries = (globalThis.reentries ?? 0) + 1;
        [3, 1, 2].sort().map(value => ({ value }));
        Promise.resolve(1).then(() => {});
    "#;

    fn view_of(code_units: &[u16]) -> JSUtf16View {
        JSUtf16View {
            data: code_units.as_ptr().cast(),
            length_in_code_units: code_units.len(),
            has_ascii_storage: false,
        }
    }

    fn code_units_of(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn utf8_of_view(view: JSUtf16View) -> String {
        // SAFETY: The tests only take views of strings that outlive them.
        unsafe { view.as_view() }.to_utf8()
    }

    fn evaluate(vm: &Vm, realm: Gc<Realm>, source: &str) -> String {
        utf8(run_script(vm, realm, source).must())
    }

    fn message_of(vm: &Vm, error: Value) -> String {
        utf8(error.as_object().get(vm, &PropertyKey::from_utf8("message")).must())
    }

    fn promise_from_abi(capability: *mut JSPromiseCapability) -> Gc<Promise> {
        let capability = core::ptr::NonNull::new(capability.cast::<PromiseCapability>()).expect("a capability");
        // SAFETY: The runtime returned a live promise capability.
        promise_of(unsafe { Gc::from_non_null(capability) })
    }

    fn pointer_of_payload<T>(completion: JSCompletion) -> *mut T {
        assert!(completion.variant == JS_COMPLETION_NORMAL, "the completion is normal");
        core::ptr::with_exposed_provenance_mut(completion.payload as usize)
    }

    fn parse(
        vm: &Vm,
        realm: Gc<Realm>,
        source: &str,
        filename: &str,
        host_defined: Option<Gc<Object>>,
    ) -> *mut JSModule {
        let (source, filename) = (code_units_of(source), code_units_of(filename));
        // SAFETY: The VM, realm and host-defined cell are live, and the views outlive the call.
        let module = unsafe {
            js_module_parse_source_text_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&source),
                view_of(&filename),
                view_of(&[]),
                host_defined.map_or(core::ptr::null_mut(), |cell| cell.as_ptr().cast()),
                0,
                core::ptr::null(),
            )
        };
        assert!(!module.is_null(), "the module parses");
        module
    }

    /// A load that the embedder's load_imported_module hook received, which it finishes later, as a fetch does.
    struct PendingLoad {
        referrer: JSImportedModuleReferrer,
        module_request: *mut JSModuleRequest,
        specifier: String,
        host_defined: *mut c_void,
        payload: JSImportedModulePayload,
    }

    /// An embedder that loads modules the way HTML does: its hook keeps each load until the test finishes it, with the
    /// referrer and the payload alive and its own copy of the module request. It finishes the loads of specifiers
    /// that start with "./now" inside the hook instead, after running a script and collecting garbage.
    struct TestLoader<'vm> {
        vm: &'vm Vm,
        realm: Gc<Realm>,
        pending_loads: RefCell<Vec<PendingLoad>>,
        records_of_pending_loads: MarkedVec<'vm, (ImportedModuleReferrer, ImportedModulePayload)>,
    }

    const NO_HOOKS: JSVmHostHooks = JSVmHostHooks {
        ensure_can_add_private_element: None,
        ensure_can_compile_strings: None,
        get_code_for_eval: None,
        promise_rejection_tracker: None,
        call_job_callback: None,
        enqueue_finalization_registry_cleanup_job: None,
        enqueue_promise_job: None,
        promise_job_queue_is_empty: None,
        make_job_callback: None,
        get_import_meta_properties: None,
        finalize_import_meta: None,
        get_supported_import_attributes: None,
        load_imported_module: None,
        unrecognized_date_string: None,
        resize_array_buffer: None,
        grow_shared_array_buffer: None,
        system_utc_epoch_nanoseconds: None,
        on_unimplemented_property_access: None,
    };

    static LOADER_HOOKS: JSVmHostHooks = JSVmHostHooks {
        load_imported_module: Some(load_imported_module),
        ..NO_HOOKS
    };

    unsafe extern "C" fn load_imported_module(
        data: *mut c_void,
        vm: *mut JSVM,
        referrer: JSImportedModuleReferrer,
        module_request: *const JSModuleRequest,
        host_defined: *mut c_void,
        payload: JSImportedModulePayload,
    ) {
        // SAFETY: The embedder data is the installed loader, and the runtime passes a live referrer, request and
        //         payload.
        let (loader, specifier) = unsafe {
            (
                &*data.cast::<TestLoader<'_>>(),
                utf8_of_view(js_module_request_specifier(module_request)),
            )
        };
        if specifier.starts_with("./now") {
            run_script(loader.vm, loader.realm, REENTRANT_SCRIPT).must();
            loader.vm.heap().collect_garbage();
            let module = loader.create_exporting_module(&specifier, host_defined);
            // SAFETY: The VM is live and these are the referrer, request and payload of this call.
            unsafe { js_module_finish_loading_imported_module(vm, referrer, module_request, payload, module) };
            return;
        }
        // SAFETY: The runtime passes a live referrer and payload, and lends the request.
        let (referrer_record, payload_record, module_request) = unsafe {
            (
                imported_module_referrer_from_abi(referrer),
                imported_module_payload_from_abi(payload),
                js_module_request_clone(module_request),
            )
        };
        loader.records_of_pending_loads.push((referrer_record, payload_record));
        loader.pending_loads.borrow_mut().push(PendingLoad {
            referrer,
            module_request,
            specifier,
            host_defined,
            payload,
        });
    }

    impl<'vm> TestLoader<'vm> {
        fn install(vm: &'vm Vm, realm: Gc<Realm>) -> Box<Self> {
            let loader = Box::new(Self {
                vm,
                realm,
                pending_loads: RefCell::default(),
                records_of_pending_loads: MarkedVec::new(vm),
            });
            let data = core::ptr::from_ref(&*loader).cast_mut().cast();
            // SAFETY: The VM is live, and the loader outlives every module load of the test.
            unsafe { js_vm_set_embedder(vm_into_abi(vm), &raw const LOADER_HOOKS, data) };
            loader
        }

        fn specifiers_of_pending_loads(&self) -> Vec<String> {
            self.pending_loads
                .borrow()
                .iter()
                .map(|load| load.specifier.clone())
                .collect()
        }

        fn pending_load(&self, specifier: &str) -> PendingLoad {
            let mut pending_loads = self.pending_loads.borrow_mut();
            let index = pending_loads
                .iter()
                .position(|load| load.specifier == specifier)
                .expect("the load is pending");
            pending_loads.remove(index)
        }

        fn finish(&self, load: PendingLoad, result: JSCompletion) {
            // SAFETY: The records of the load stayed alive in records_of_pending_loads, and the loader owns the
            //         request.
            unsafe {
                js_module_finish_loading_imported_module(
                    vm_into_abi(self.vm),
                    load.referrer,
                    load.module_request,
                    load.payload,
                    result,
                );
                js_module_request_destroy(load.module_request);
            }
        }

        fn create_exporting_module(&self, filename: &str, host_defined: *mut c_void) -> JSCompletion {
            let filename = code_units_of(filename);
            // SAFETY: The VM, realm, class and host-defined cell are live, and the view outlives the call.
            let module = unsafe {
                js_host_module_create(
                    vm_into_abi(self.vm),
                    cell_into_abi(self.realm),
                    &raw const EXPORTING_MODULE_CLASS.0,
                    view_of(&filename),
                    core::ptr::null(),
                    0,
                    host_defined,
                    core::ptr::null_mut(),
                )
            };
            completion_into_abi(Ok(module))
        }
    }

    impl Drop for TestLoader<'_> {
        fn drop(&mut self) {
            for load in self.pending_loads.take() {
                // SAFETY: The loader owns the request.
                unsafe { js_module_request_destroy(load.module_request) };
            }
            // SAFETY: The VM is live.
            unsafe { js_vm_set_embedder(vm_into_abi(self.vm), core::ptr::null(), core::ptr::null_mut()) };
        }
    }

    #[test]
    fn a_module_graph_loads_through_the_embedder_after_it_finishes_each_load_later() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let loader = TestLoader::install(vm, realm);
        let host_defined = Object::create(vm, realm, None);
        vm.heap().set_should_collect_on_every_allocation(true);

        let entry = parse(
            vm,
            realm,
            r#"
                import { answer, "名前" as name } from "./exporting.wasm";
                import data from "./data.json" with { type: "json" };
                export { answer as reexported } from "./exporting.wasm";
                globalThis.result = [answer, name, data.list.join("")].join();
            "#,
            "entry.mjs",
            Some(host_defined),
        );
        // SAFETY: The VM, the module and the host-defined cell are live.
        let loading = promise_from_abi(unsafe {
            js_module_load_requested_modules(vm_into_abi(vm), entry, host_defined.as_ptr().cast())
        });
        // Like the C++ runtime, which shares the frontend that lists them, the module requests "./exporting.wasm" once
        // for each declaration that names it.
        assert_eq!(
            loader.specifiers_of_pending_loads(),
            ["./exporting.wasm", "./data.json", "./exporting.wasm"]
        );
        for load in loader.pending_loads.borrow().iter() {
            assert_eq!(load.referrer.kind, JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE);
            assert_eq!(load.referrer.record, entry.cast());
            assert_eq!(load.host_defined, host_defined.as_ptr().cast());
            assert_eq!(load.payload.kind, JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE);
        }
        vm.heap().collect_garbage();
        assert_eq!(loading.state(), PromiseState::Pending);

        let (json, json_filename) = (code_units_of(r#"{ "list": [1, 2] }"#), code_units_of("data.json"));
        let json_load = loader.pending_load("./data.json");
        // SAFETY: The request is the loader's own copy.
        let json_attribute = unsafe { js_module_request_attribute(json_load.module_request, 0) };
        assert_eq!(
            (utf8_of_view(json_attribute.key), utf8_of_view(json_attribute.value)),
            ("type".to_string(), "json".to_string())
        );
        // SAFETY: The VM and realm are live, and the views outlive the call.
        let json_module = unsafe {
            js_module_parse_json_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&json),
                view_of(&json_filename),
            )
        };
        loader.finish(json_load, json_module);
        assert_eq!(loading.state(), PromiseState::Pending);
        let exporting_load = loader.pending_load("./exporting.wasm");
        let exporting_module = loader.create_exporting_module("exporting.wasm", exporting_load.host_defined);
        loader.finish(exporting_load, exporting_module);
        assert_eq!(loading.state(), PromiseState::Pending);
        loader.finish(loader.pending_load("./exporting.wasm"), exporting_module);
        assert_eq!(loading.state(), PromiseState::Fulfilled);

        // SAFETY: The VM and the module are live.
        unsafe {
            assert_eq!(js_module_link(vm_into_abi(vm), entry).variant, JS_COMPLETION_NORMAL);
            let evaluation = promise_from_abi(pointer_of_payload(js_module_evaluate(vm_into_abi(vm), entry)));
            assert_eq!(evaluation.state(), PromiseState::Fulfilled);
        }
        vm.heap().set_should_collect_on_every_allocation(false);
        assert_eq!(evaluate(vm, realm, "globalThis.result"), "42,value,12");

        let exporting_module = pointer_of_payload::<JSModule>(exporting_module);
        // SAFETY: The modules are live, and the out record and the sinks outlive the calls.
        unsafe {
            assert_eq!(js_module_requested_module_count(entry), 3);
            assert_eq!(
                js_module_get_imported_module(entry, js_module_requested_module(entry, 0)),
                exporting_module
            );
            assert_eq!(js_module_host_defined(entry), host_defined.as_ptr().cast());
            assert_eq!(js_module_host_defined(exporting_module), host_defined.as_ptr().cast());
            assert_eq!(js_module_realm(exporting_module), cell_into_abi(realm));
            assert_eq!(js_module_class_id(entry), ClassId::SourceTextModule as u16);
            assert_eq!(js_module_class_id(exporting_module), ClassId::HostModule as u16);
            assert!(!js_module_environment(exporting_module).is_null());

            let mut names: Vec<String> = Vec::new();
            let sink = JSStringSink {
                context: (&raw mut names).cast(),
                append: Some(collect_utf8),
            };
            js_module_get_exported_names(vm_into_abi(vm), entry, &raw const sink);
            assert_eq!(names, ["reexported"]);

            let mut binding_names: Vec<String> = Vec::new();
            let mut binding = JSResolvedBinding {
                module: core::ptr::null_mut(),
                binding_name: JSStringSink {
                    context: (&raw mut binding_names).cast(),
                    append: Some(collect_utf8),
                },
                r#type: JS_RESOLVED_BINDING_NULL,
            };
            let reexported = code_units_of("reexported");
            js_module_resolve_export(vm_into_abi(vm), entry, view_of(&reexported), &raw mut binding);
            assert_eq!(binding.r#type, JS_RESOLVED_BINDING_BINDING_NAME);
            assert_eq!(binding.module, exporting_module);
            assert_eq!(binding_names, ["answer"]);
        }
    }

    unsafe extern "C" fn collect_utf8(strings: *mut c_void, code_units: *const u16, length: usize) {
        // SAFETY: The tests pass a Vec<String> as the context, and the runtime passes `length` code units.
        let (strings, code_units) = unsafe {
            (
                &mut *strings.cast::<Vec<String>>(),
                core::slice::from_raw_parts(code_units, length),
            )
        };
        strings.push(String::from_utf16(code_units).expect("the string is well-formed"));
    }

    #[test]
    fn a_failed_load_rejects_the_graph_and_a_load_finished_inside_the_hook_continues_it() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let loader = TestLoader::install(vm, realm);
        vm.heap().set_should_collect_on_every_allocation(true);

        let entry = parse(
            vm,
            realm,
            "import \"./now.wasm\"; import \"./missing.mjs\";",
            "entry.mjs",
            None,
        );
        // SAFETY: The VM and the module are live.
        let loading = promise_from_abi(unsafe {
            js_module_load_requested_modules(vm_into_abi(vm), entry, core::ptr::null_mut())
        });
        assert_eq!(loader.specifiers_of_pending_loads(), ["./missing.mjs"]);
        assert_eq!(loading.state(), PromiseState::Pending);
        let failure = vm.throw_completion_with_message::<()>(ErrorKind::TypeError, "no such module".to_string());
        loader.finish(loader.pending_load("./missing.mjs"), completion_into_abi(failure));
        vm.heap().set_should_collect_on_every_allocation(false);
        assert_eq!(loading.state(), PromiseState::Rejected);
        assert_eq!(message_of(vm, loading.result()), "no such module");
        assert_eq!(evaluate(vm, realm, "String(globalThis.reentries)"), "1");
    }

    #[test]
    fn dynamic_imports_load_text_and_default_export_synthetic_modules_through_the_embedder() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let loader = TestLoader::install(vm, realm);

        run_script(
            vm,
            realm,
            r#"
                globalThis.imported = [];
                import("./text.txt", { with: { type: "text" } }).then(namespace => imported.push(namespace.default));
                import("./sheet.css", { with: { type: "css" } }).then(namespace => imported.push(namespace.default.rules));
            "#,
        )
        .must();
        assert_eq!(loader.specifiers_of_pending_loads(), ["./text.txt", "./sheet.css"]);
        for load in loader.pending_loads.borrow().iter() {
            assert_eq!(load.referrer.kind, JS_IMPORTED_MODULE_REFERRER_SCRIPT);
            assert!(load.host_defined.is_null());
            assert_eq!(load.payload.kind, JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY);
        }

        let (text, text_filename, sheet_filename) = (
            code_units_of("plain text"),
            code_units_of("text.txt"),
            code_units_of("sheet.css"),
        );
        // SAFETY: The VM and realm are live, and the view outlives the call.
        let text_module = unsafe {
            js_module_create_text_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&text),
                view_of(&text_filename),
            )
        };
        loader.finish(loader.pending_load("./text.txt"), completion_into_abi(Ok(text_module)));
        let sheet = run_script(vm, realm, "({ rules: 'a {}' })").must();
        // SAFETY: The VM, realm and value are live, and the view outlives the call.
        let sheet_module = unsafe {
            js_module_create_default_export_synthetic_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                sheet.0,
                view_of(&sheet_filename),
            )
        };
        loader.finish(
            loader.pending_load("./sheet.css"),
            completion_into_abi(Ok(sheet_module)),
        );
        vm.run_queued_promise_jobs();
        assert_eq!(evaluate(vm, realm, "imported.join('|')"), "plain text|a {}");
        // SAFETY: The module is live.
        assert_eq!(
            unsafe { js_module_class_id(text_module) },
            ClassId::SyntheticModule as u16
        );
    }

    #[test]
    fn module_requests_keep_their_attributes_in_order_and_compare_by_value() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();

        let (specifier, type_key, json, b_key, one) = (
            code_units_of("./a.json"),
            code_units_of("type"),
            code_units_of("json"),
            code_units_of("b"),
            code_units_of("1"),
        );
        let type_json = JSImportAttribute {
            key: view_of(&type_key),
            value: view_of(&json),
        };
        let b_one = JSImportAttribute {
            key: view_of(&b_key),
            value: view_of(&one),
        };
        let in_source_order = [type_json, b_one];
        let sorted_by_key = [b_one, type_json];
        // SAFETY: The views outlive the calls, and each request is destroyed once.
        unsafe {
            let request = js_module_request_create(view_of(&specifier), in_source_order.as_ptr(), 2);
            assert_eq!(utf8_of_view(js_module_request_specifier(request)), "./a.json");
            assert_eq!(js_module_request_attribute_count(request), 2);
            assert_eq!(utf8_of_view(js_module_request_attribute(request, 0).key), "type");
            assert_eq!(utf8_of_view(js_module_request_attribute(request, 1).value), "1");
            let copy = js_module_request_clone(request);
            assert!(js_module_request_equals(request, copy));
            let reordered = js_module_request_create(view_of(&specifier), sorted_by_key.as_ptr(), 2);
            assert!(!js_module_request_equals(request, reordered));

            // A module's own requests have their attributes sorted by key, as WithClauseToAttributes sorts them.
            let entry = parse(
                vm,
                realm,
                "import data from './a.json' with { type: 'json', b: '1' };",
                "entry.mjs",
                None,
            );
            let requested = js_module_requested_module(entry, 0);
            assert!(js_module_request_equals(requested, reordered));
            assert!(!js_module_request_equals(requested, request));

            for owned in [request, copy, reordered] {
                js_module_request_destroy(owned);
            }
        }
    }

    /// A parser error sink's context, whose append function re-enters the VM before it keeps each error.
    struct ReenteringParserErrorSink<'a> {
        vm: &'a Vm,
        realm: Gc<Realm>,
        errors: Vec<(String, u32, u32)>,
    }

    unsafe extern "C" fn reenter_and_collect_parser_error(
        sink: *mut c_void,
        message: JSOwnedUtf16String,
        line: u32,
        column: u32,
    ) {
        // SAFETY: The context is the test's ReenteringParserErrorSink, and the runtime hands over the message.
        let (sink, message) = unsafe {
            (
                &mut *sink.cast::<ReenteringParserErrorSink<'_>>(),
                owned_utf16_string_from_abi(message),
            )
        };
        run_script(sink.vm, sink.realm, REENTRANT_SCRIPT).must();
        sink.vm.heap().collect_garbage();
        sink.errors
            .push((Utf16View::of_string(&message).to_utf8(), line, column));
    }

    #[test]
    fn a_source_text_module_that_does_not_parse_reports_its_errors_where_its_text_starts() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let (source, filename, display_filename) = (
            code_units_of("export let = 1;"),
            code_units_of("page.html"),
            code_units_of("inline.js"),
        );
        let mut reentering_sink = ReenteringParserErrorSink {
            vm,
            realm,
            errors: Vec::new(),
        };
        let errors = JSParserErrorSink {
            context: (&raw mut reentering_sink).cast(),
            append: Some(reenter_and_collect_parser_error),
        };
        // SAFETY: The VM and realm are live, and the views and the sink outlive the call.
        let module = unsafe {
            js_module_parse_source_text_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&source),
                view_of(&filename),
                view_of(&display_filename),
                core::ptr::null_mut(),
                10,
                &raw const errors,
            )
        };
        assert!(module.is_null());
        let first_error = reentering_sink.errors.first().expect("the source has a syntax error");
        assert_eq!((first_error.1, first_error.2), (10, 12));
        assert!(!first_error.0.contains("line:"), "{}", first_error.0);
        assert_eq!(
            evaluate(vm, realm, "String(globalThis.reentries)"),
            reentering_sink.errors.len().to_string()
        );

        // Without a sink, the errors are dropped.
        // SAFETY: The VM and realm are live, and the views outlive the call.
        let module = unsafe {
            js_module_parse_source_text_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&source),
                view_of(&filename),
                view_of(&[]),
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            )
        };
        assert!(module.is_null());
    }

    #[test]
    fn a_json_module_that_does_not_parse_throws_a_syntax_error() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let (json, filename) = (code_units_of("{ \"list\": [1, 2 }"), code_units_of("data.json"));
        // SAFETY: The VM and realm are live, and the views outlive the call.
        let completion = unsafe {
            js_module_parse_json_module(
                vm_into_abi(vm),
                cell_into_abi(realm),
                view_of(&json),
                view_of(&filename),
            )
        };
        assert_eq!(completion.variant, JS_COMPLETION_THROW);
        let error = Value(completion.payload).as_object();
        assert_eq!(
            utf8(error.get(vm, &PropertyKey::from_utf8("name")).must()),
            "SyntaxError"
        );
    }

    #[test]
    fn sinks_of_exported_names_and_binding_names_may_reenter_the_vm() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let _loader = TestLoader::install(vm, realm);
        let entry = parse(
            vm,
            realm,
            "export * from './now.wasm'; export let local = 1;",
            "entry.mjs",
            None,
        );
        // SAFETY: The VM and the module are live.
        let loading = promise_from_abi(unsafe {
            js_module_load_requested_modules(vm_into_abi(vm), entry, core::ptr::null_mut())
        });
        assert_eq!(loading.state(), PromiseState::Fulfilled);

        struct ReenteringSink<'a> {
            vm: &'a Vm,
            realm: Gc<Realm>,
            names: Vec<String>,
        }
        unsafe extern "C" fn reenter_and_collect(sink: *mut c_void, code_units: *const u16, length: usize) {
            // SAFETY: The context is the test's ReenteringSink, and the runtime passes `length` code units.
            let (sink, code_units) = unsafe {
                (
                    &mut *sink.cast::<ReenteringSink<'_>>(),
                    core::slice::from_raw_parts(code_units, length),
                )
            };
            run_script(sink.vm, sink.realm, REENTRANT_SCRIPT).must();
            sink.vm.heap().collect_garbage();
            sink.names
                .push(String::from_utf16(code_units).expect("the name is well-formed"));
        }
        let mut reentering_sink = ReenteringSink {
            vm,
            realm,
            names: Vec::new(),
        };
        let context = (&raw mut reentering_sink).cast();
        let local = code_units_of("local");
        // SAFETY: The VM and the module are live, and the sinks and views outlive the calls.
        unsafe {
            let names = JSStringSink {
                context,
                append: Some(reenter_and_collect),
            };
            js_module_get_exported_names(vm_into_abi(vm), entry, &raw const names);
            let mut binding = JSResolvedBinding {
                module: core::ptr::null_mut(),
                binding_name: JSStringSink {
                    context,
                    append: Some(reenter_and_collect),
                },
                r#type: JS_RESOLVED_BINDING_NULL,
            };
            js_module_resolve_export(vm_into_abi(vm), entry, view_of(&local), &raw mut binding);
            assert_eq!(binding.module, entry);
        }
        assert_eq!(reentering_sink.names, ["local", "answer", "名前", "local"]);
        assert_eq!(evaluate(vm, realm, "String(globalThis.reentries)"), "5");
    }
}
