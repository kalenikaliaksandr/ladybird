/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The host hooks, which are the embedder's table of callbacks and its data pointer, and the embedder's agent.

use core::ffi::c_void;
use core::mem::ManuallyDrop;
use core::ptr::NonNull;

use ak::Utf16String;

use crate::embedding::abi_types::{
    CompletionPayload, JSRealm, JSUtf16View, cell_from_abi, cell_into_abi, completion_from_abi, object_into_abi,
    optional_cell_into_abi, vm_into_abi,
};
use crate::embedding::realm::JSJobCallback;
use crate::gc::heap_function::HeapFunction;
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::{
    CompilationType, HandledByHost, OnUnimplementedPropertyAccess, Vm,
    default_host_enqueue_finalization_registry_cleanup_job, default_host_enqueue_promise_job,
    default_host_ensure_can_add_private_element, default_host_ensure_can_compile_strings,
    default_host_finalize_import_meta, default_host_get_code_for_eval, default_host_get_import_meta_properties,
    default_host_get_supported_import_attributes, default_host_grow_shared_array_buffer,
    default_host_promise_job_queue_is_empty, default_host_promise_rejection_tracker, default_host_resize_array_buffer,
    default_host_system_utc_epoch_nanoseconds, default_host_unrecognized_date_string,
};
use crate::layout::cell::Gc;
use crate::layout::function_object::FunctionObject;
use crate::layout::host_class::{JSCompletion, JSModule, JSObject, JSPropertyKey, JSStringSink, JSVM, JSValue};
use crate::layout::object::Object;
use crate::layout::realm::Realm;
use crate::layout::value::Value;
use crate::runtime::array_buffer::ArrayBuffer;
use crate::runtime::big_int::SignedBigInteger;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::cyclic_module::CyclicModule;
use crate::runtime::finalization_registry::FinalizationRegistry;
use crate::runtime::job_callback::{self, JobCallback};
use crate::runtime::module::GraphLoadingState;
use crate::runtime::module_loading::{ImportedModulePayload, ImportedModuleReferrer};
use crate::runtime::module_request::ModuleRequest;
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::promise::{Promise, RejectionOperation};
use crate::runtime::promise_capability::PromiseCapability;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::source_text_module::SourceTextModule;
use crate::script::Script;
use crate::utf16::Utf16View;

/// The [[Job]] of a PromiseJob Record, which the embedder runs once with js_promise_job_run(). It is a LibGC cell,
/// which the embedder keeps alive until then by visiting it from one of its cells or by rooting it.
// NB: Without repr(C), cbindgen declares the type without a definition, which is all C needs.
pub struct JSPromiseJob {
    _opaque: [u8; 0],
}

/// A ModuleRequest Record. The runtime lends one to a hook for the duration of its call, and the embedder owns the
/// ones that js_module_request_create() and js_module_request_clone() return until it destroys them.
pub struct JSModuleRequest {
    _opaque: [u8; 0],
}

/// The compilationType of HostEnsureCanCompileStrings, in the order of the C++ JS::CompilationType.
pub const JS_COMPILATION_TYPE_DIRECT_EVAL: u8 = 0;
pub const JS_COMPILATION_TYPE_INDIRECT_EVAL: u8 = 1;
pub const JS_COMPILATION_TYPE_FUNCTION: u8 = 2;
pub const JS_COMPILATION_TYPE_TIMER: u8 = 3;

/// The operation of HostPromiseRejectionTracker, in the order of the C++ JS::Promise::RejectionOperation.
pub const JS_PROMISE_REJECTION_OPERATION_REJECT: u8 = 0;
pub const JS_PROMISE_REJECTION_OPERATION_HANDLE: u8 = 1;

/// The normal payload of HostResizeArrayBuffer and HostGrowSharedArrayBuffer, an enumerator in the order of the C++
/// JS::HandledByHost.
pub const JS_HANDLED_BY_HOST_HANDLED: u8 = 0;
pub const JS_HANDLED_BY_HOST_UNHANDLED: u8 = 1;

/// What the record of a JSImportedModuleReferrer is, in the order of the C++ JS::ImportedModuleReferrer.
pub const JS_IMPORTED_MODULE_REFERRER_SCRIPT: u8 = 0;
pub const JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE: u8 = 1;
pub const JS_IMPORTED_MODULE_REFERRER_REALM: u8 = 2;

/// What the record of a JSImportedModulePayload is, in the order of the C++ JS::ImportedModulePayload.
pub const JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE: u8 = 0;
pub const JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY: u8 = 1;

/// The referrer of HostLoadImportedModule: a Script Record, a Cyclic Module Record (a JSModule) or a Realm Record (a
/// JSRealm), as `kind` says.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSImportedModuleReferrer {
    pub kind: u8,
    pub record: *mut c_void,
}

/// The payload of HostLoadImportedModule, a GraphLoadingState Record or a PromiseCapability Record as `kind` says,
/// which the embedder passes on to FinishLoadingImportedModule untouched.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSImportedModulePayload {
    pub kind: u8,
    pub record: *mut c_void,
}

/// The arguments of HostEnsureCanCompileStrings ( calleeRealm, parameterStrings, bodyString, codeString,
/// compilationType, parameterArgs, bodyArg ), with compilationType one of JS_COMPILATION_TYPE_*.
#[repr(C)]
pub struct JSEnsureCanCompileStringsArguments {
    pub callee_realm: *mut JSRealm,
    pub parameter_strings: *const JSUtf16View,
    pub parameter_string_count: usize,
    pub body_string: JSUtf16View,
    pub code_string: JSUtf16View,
    pub compilation_type: u8,
    pub parameter_args: *const JSValue,
    pub parameter_arg_count: usize,
    pub body_arg: JSValue,
}

/// Where HostGetImportMetaProperties appends each property that import.meta starts with. The key is borrowed for the
/// call, and the value has to be alive until it is appended.
#[repr(C)]
pub struct JSImportMetaPropertySink {
    pub context: *mut c_void,
    pub append: unsafe extern "C" fn(context: *mut c_void, key: JSPropertyKey, value: JSValue),
}

/// Whether the goal of a spin of the event loop is met, given the context that came with it.
pub type JSGoalCondition = unsafe extern "C" fn(goal_context: *mut c_void) -> bool;

/// The surrounding agent of the VM, which the embedder provides: the C++ runtime's JS::Agent.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSAgent {
    /// [[CanBlock]]: whether Atomics.wait() may block.
    pub can_block: bool,
    /// Agent::spin_event_loop_until(): runs the embedder's event loop, promise jobs included, until
    /// goal_condition(goal_context) is true, which await in native code waits for its promise with. It receives `data`
    /// and the VM, runs on the VM's thread, and may run any JavaScript, including another spin.
    pub spin_event_loop_until: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            goal_condition: JSGoalCondition,
            goal_context: *mut c_void,
        ),
    >,
    pub data: *mut c_void,
}

/// The embedder's table of host hooks, which the VM calls in place of its own host-defined behavior. A null hook keeps
/// the runtime's own behavior, which js_vm_default_host_*() exports wherever it does more than return a fixed answer.
/// Every hook receives the data pointer the embedder installed the table with and the VM, runs on the VM's thread, and
/// may run JavaScript unless it says otherwise. The engine objects and values it receives stay alive for the duration
/// of the call.
#[repr(C)]
pub struct JSVmHostHooks {
    /// HostEnsureCanAddPrivateElement ( O ): a throw completion keeps the private element from being added to the object.
    /// The runtime's own completes normally.
    pub ensure_can_add_private_element:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, object: *mut JSObject) -> JSCompletion>,
    /// HostEnsureCanCompileStrings: a throw completion keeps the strings from being compiled. The runtime's own
    /// completes normally.
    pub ensure_can_compile_strings: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            arguments: *const JSEnsureCanCompileStringsArguments,
        ) -> JSCompletion,
    >,
    /// HostGetCodeForEval ( argument ): returns true and writes a String to `code` for an object that eval() is to run
    /// the code of, and returns false for NO-CODE, as the runtime's own always does.
    pub get_code_for_eval: Option<
        unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, argument: *mut JSObject, code: *mut JSValue) -> bool,
    >,
    /// HostPromiseRejectionTracker ( promise, operation ), with operation one of JS_PROMISE_REJECTION_OPERATION_*.
    /// Without it, the runtime tracks no rejections for the embedder.
    pub promise_rejection_tracker:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, promise: *mut JSObject, operation: u8)>,
    /// HostCallJobCallback ( jobCallback, V, argumentsList ): the completion of calling the callback.
    /// js_vm_default_host_call_job_callback() is the runtime's own.
    pub call_job_callback: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            job_callback: *mut JSJobCallback,
            this_value: JSValue,
            arguments: *const JSValue,
            argument_count: usize,
        ) -> JSCompletion,
    >,
    /// HostEnqueueFinalizationRegistryCleanupJob ( finalizationRegistry ). It runs at the end of a garbage
    /// collection, which any allocation can start, so it queues the cleanup and must not run JavaScript itself.
    /// js_vm_default_host_enqueue_finalization_registry_cleanup_job() is the runtime's own.
    pub enqueue_finalization_registry_cleanup_job:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, finalization_registry: *mut JSObject)>,
    /// HostEnqueuePromiseJob ( job, realm ), where the realm may be null. The embedder runs the job later with
    /// js_promise_job_run(), and keeps it alive until then. js_vm_default_host_enqueue_promise_job() is the runtime's
    /// own.
    pub enqueue_promise_job:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, job: *mut JSPromiseJob, realm: *mut JSRealm)>,
    /// Whether the queue that enqueue_promise_job fills is empty, which an async function asks before it resumes
    /// right after an await. js_vm_default_host_promise_job_queue_is_empty() is the runtime's own.
    pub promise_job_queue_is_empty: Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM) -> bool>,
    /// HostMakeJobCallback ( callable ): a new JobCallback Record of the callable. The runtime's own creates the record
    /// that js_realm_job_callback_create(vm, callable, NULL) does.
    pub make_job_callback:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, callable: *mut JSObject) -> *mut JSJobCallback>,
    /// HostGetImportMetaProperties ( moduleRecord ): appends the properties that import.meta starts with, of which the
    /// runtime's own has none.
    pub get_import_meta_properties: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            module: *mut JSModule,
            properties: *mut JSImportMetaPropertySink,
        ),
    >,
    /// HostFinalizeImportMeta ( importMeta, moduleRecord ). The runtime's own does nothing.
    pub finalize_import_meta: Option<
        unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, import_meta: *mut JSObject, module: *mut JSModule),
    >,
    /// HostGetSupportedImportAttributes ( ): appends the key of each supported import attribute. The runtime's own
    /// supports "type".
    pub get_supported_import_attributes:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, attribute_keys: *mut JSStringSink)>,
    /// HostLoadImportedModule ( referrer, moduleRequest, hostDefined, payload ), which finishes loading the module
    /// now or later with FinishLoadingImportedModule. hostDefined is the cell that LoadRequestedModules was given, or
    /// null for EMPTY. js_vm_default_host_load_imported_module() is the runtime's own, which loads files.
    pub load_imported_module: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            referrer: JSImportedModuleReferrer,
            module_request: *const JSModuleRequest,
            host_defined: *mut c_void,
            payload: JSImportedModulePayload,
        ),
    >,
    /// The Date constructor or Date.parse() found no date in a string, which is borrowed for the call. The runtime's
    /// own does nothing.
    pub unrecognized_date_string:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, date_string: JSUtf16View)>,
    /// HostResizeArrayBuffer ( buffer, newByteLength ): a normal completion carries one of JS_HANDLED_BY_HOST_*.
    /// js_vm_default_host_resize_array_buffer() is the runtime's own.
    pub resize_array_buffer: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            buffer: *mut JSObject,
            new_byte_length: usize,
        ) -> JSCompletion,
    >,
    /// HostGrowSharedArrayBuffer ( buffer, newByteLength ): a normal completion carries one of
    /// JS_HANDLED_BY_HOST_*. js_vm_default_host_grow_shared_array_buffer() is the runtime's own.
    pub grow_shared_array_buffer: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            vm: *mut JSVM,
            buffer: *mut JSObject,
            new_byte_length: usize,
        ) -> JSCompletion,
    >,
    /// HostSystemUTCEpochNanoseconds ( global ): the current time in nanoseconds since the epoch.
    /// js_vm_default_host_system_utc_epoch_nanoseconds() is the runtime's own.
    pub system_utc_epoch_nanoseconds:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, global: *mut JSObject) -> i64>,
    /// VM::on_unimplemented_property_access: code looked for a property of the object that the embedder declared
    /// unimplemented. The key is borrowed for the call. Without it, nothing happens.
    pub on_unimplemented_property_access:
        Option<unsafe extern "C" fn(data: *mut c_void, vm: *mut JSVM, object: *mut JSObject, key: JSPropertyKey)>,
}

/// What an embedder installs on the VM: its hooks, and the data pointer they receive.
#[derive(Clone, Copy)]
pub struct Embedder {
    pub hooks: &'static JSVmHostHooks,
    pub data: *mut c_void,
}

/// Has the VM call the hooks of `embedder` from now on, in place of those of the embedder it had before. Every hook
/// that the new table does not have, like every hook once there is no embedder, gets the runtime's own behavior back.
pub(crate) fn install_embedder(vm: &Vm, embedder: Option<Embedder>) {
    vm.set_embedder(embedder);
    let hooks = embedder.map(|embedder| embedder.hooks);
    macro_rules! install {
        ($($hook:ident, $set_hook_slot:ident, $runtime_default:path;)*) => {$(
            vm.$set_hook_slot(if hooks.is_some_and(|hooks| hooks.$hook.is_some()) {
                $hook
            } else {
                $runtime_default
            });
        )*};
    }
    install! {
        ensure_can_add_private_element, set_host_ensure_can_add_private_element,
            default_host_ensure_can_add_private_element;
        ensure_can_compile_strings, set_host_ensure_can_compile_strings, default_host_ensure_can_compile_strings;
        get_code_for_eval, set_host_get_code_for_eval, default_host_get_code_for_eval;
        promise_rejection_tracker, set_host_promise_rejection_tracker, default_host_promise_rejection_tracker;
        call_job_callback, set_host_call_job_callback, job_callback::call_job_callback;
        enqueue_finalization_registry_cleanup_job, set_host_enqueue_finalization_registry_cleanup_job,
            default_host_enqueue_finalization_registry_cleanup_job;
        enqueue_promise_job, set_host_enqueue_promise_job, default_host_enqueue_promise_job;
        promise_job_queue_is_empty, set_host_promise_job_queue_is_empty, default_host_promise_job_queue_is_empty;
        make_job_callback, set_host_make_job_callback, job_callback::make_job_callback;
        get_import_meta_properties, set_host_get_import_meta_properties, default_host_get_import_meta_properties;
        finalize_import_meta, set_host_finalize_import_meta, default_host_finalize_import_meta;
        get_supported_import_attributes, set_host_get_supported_import_attributes,
            default_host_get_supported_import_attributes;
        load_imported_module, set_host_load_imported_module, Vm::load_imported_module;
        unrecognized_date_string, set_host_unrecognized_date_string, default_host_unrecognized_date_string;
        resize_array_buffer, set_host_resize_array_buffer, default_host_resize_array_buffer;
        grow_shared_array_buffer, set_host_grow_shared_array_buffer, default_host_grow_shared_array_buffer;
        system_utc_epoch_nanoseconds, set_host_system_utc_epoch_nanoseconds,
            default_host_system_utc_epoch_nanoseconds;
    }
    let has_on_unimplemented_property_access =
        hooks.is_some_and(|hooks| hooks.on_unimplemented_property_access.is_some());
    vm.set_on_unimplemented_property_access(
        has_on_unimplemented_property_access
            .then_some(on_unimplemented_property_access as OnUnimplementedPropertyAccess),
    );
}

/// The event loop of the agent that an embedder provides, which the C++ runtime reaches through VM::agent().
#[derive(Clone, Copy)]
pub struct EmbedderAgent {
    spin_event_loop_until: unsafe extern "C" fn(*mut c_void, *mut JSVM, JSGoalCondition, *mut c_void),
    data: *mut c_void,
}

impl EmbedderAgent {
    pub fn of(agent: &JSAgent) -> Self {
        Self {
            spin_event_loop_until: agent
                .spin_event_loop_until
                .expect("the agent of an embedder spins its event loop"),
            data: agent.data,
        }
    }

    /// Agent::spin_event_loop_until(goal_condition), which may run any JavaScript, including another spin.
    pub fn spin_event_loop_until(&self, vm: &Vm, goal_condition: &dyn Fn() -> bool) {
        unsafe extern "C" fn goal_condition_is_met(goal_condition: *mut c_void) -> bool {
            // SAFETY: The goal context is the goal condition below, which outlives the spin.
            let goal_condition = unsafe { &*goal_condition.cast::<&dyn Fn() -> bool>() };
            goal_condition()
        }
        let goal_context = core::ptr::from_ref(&goal_condition).cast_mut().cast();
        // SAFETY: The embedder's agent takes its data and the VM, and only calls the goal condition during the spin.
        unsafe { (self.spin_event_loop_until)(self.data, vm_into_abi(vm), goal_condition_is_met, goal_context) };
    }
}

/// Calls a hook of the embedder with its data, the VM and `arguments`. The VM holds a hook's thunk only while the
/// embedder's table has the hook, and the caller holds no borrow of VM state, as the hook may run JavaScript.
macro_rules! call_embedder_hook {
    ($vm:expr, $hook:ident $(, $argument:expr)* $(,)?) => {{
        let vm: &Vm = $vm;
        let embedder = vm.embedder().expect("the VM has the embedder whose hook it calls");
        let hook = embedder.hooks.$hook.expect("the VM only calls the hooks the embedder has");
        // SAFETY: The embedder's hook takes its data, the VM, and arguments that are alive for the call.
        unsafe { hook(embedder.data, vm_into_abi(vm) $(, $argument)*) }
    }};
}

/// A key the embedder borrows for the duration of a call.
pub(crate) fn lend_property_key_to_abi(key: &PropertyKey) -> JSPropertyKey {
    // SAFETY: A property key is one word, the bits C sees, and copying them out does not affect its ownership.
    JSPropertyKey {
        bits: unsafe { core::mem::transmute_copy::<PropertyKey, usize>(key) },
    }
}

/// A key of the runtime with the bits of a key that the embedder lends.
///
/// # Safety
///
/// `key` must have the bits of a live property key.
pub(crate) unsafe fn clone_lent_property_key_from_abi(key: JSPropertyKey) -> PropertyKey {
    assert!(key.bits != 0, "the embedder passes a property key");
    // SAFETY: The bits are those of a live key, which the borrowed copy never releases.
    let borrowed = ManuallyDrop::new(unsafe { core::mem::transmute::<usize, PropertyKey>(key.bits) });
    PropertyKey::clone(&borrowed)
}

/// # Safety
///
/// `job` must be a live promise job of the runtime.
pub(crate) unsafe fn promise_job_from_abi(job: *mut JSPromiseJob) -> Gc<HeapFunction> {
    let job = NonNull::new(job.cast()).expect("the embedder passes a promise job");
    // SAFETY: The caller passes a live promise job.
    unsafe { Gc::from_non_null(job) }
}

pub(crate) fn imported_module_referrer_into_abi(referrer: ImportedModuleReferrer) -> JSImportedModuleReferrer {
    let (kind, record) = match referrer {
        ImportedModuleReferrer::Script(script) => (JS_IMPORTED_MODULE_REFERRER_SCRIPT, script.as_ptr().cast()),
        ImportedModuleReferrer::CyclicModule(module) => {
            (JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE, module.as_ptr().cast())
        }
        ImportedModuleReferrer::Realm(realm) => (JS_IMPORTED_MODULE_REFERRER_REALM, realm.as_ptr().cast()),
    };
    JSImportedModuleReferrer { kind, record }
}

/// # Safety
///
/// `referrer` must be one that the runtime passed to load_imported_module, whose record is still alive.
pub unsafe fn imported_module_referrer_from_abi(referrer: JSImportedModuleReferrer) -> ImportedModuleReferrer {
    let record = NonNull::new(referrer.record).expect("a referrer has its record");
    // SAFETY: The caller passes a referrer of the runtime, whose kind says what its live record is.
    unsafe {
        match referrer.kind {
            JS_IMPORTED_MODULE_REFERRER_SCRIPT => {
                ImportedModuleReferrer::Script(Gc::from_non_null(record.cast::<Script>()))
            }
            JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE => {
                ImportedModuleReferrer::CyclicModule(Gc::from_non_null(record.cast::<CyclicModule>()))
            }
            JS_IMPORTED_MODULE_REFERRER_REALM => {
                ImportedModuleReferrer::Realm(Gc::from_non_null(record.cast::<Realm>()))
            }
            kind => panic!("the embedder passes a referrer of kind {kind}"),
        }
    }
}

pub(crate) fn imported_module_payload_into_abi(payload: ImportedModulePayload) -> JSImportedModulePayload {
    let (kind, record) = match payload {
        ImportedModulePayload::GraphLoadingState(state) => {
            (JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE, state.as_ptr().cast())
        }
        ImportedModulePayload::PromiseCapability(capability) => (
            JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY,
            capability.as_ptr().cast(),
        ),
    };
    JSImportedModulePayload { kind, record }
}

/// # Safety
///
/// `payload` must be one that the runtime passed to load_imported_module, whose record is still alive.
pub unsafe fn imported_module_payload_from_abi(payload: JSImportedModulePayload) -> ImportedModulePayload {
    let record = NonNull::new(payload.record).expect("a payload has its record");
    // SAFETY: The caller passes a payload of the runtime, whose kind says what its live record is.
    unsafe {
        match payload.kind {
            JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE => {
                ImportedModulePayload::GraphLoadingState(Gc::from_non_null(record.cast::<GraphLoadingState>()))
            }
            JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY => {
                ImportedModulePayload::PromiseCapability(Gc::from_non_null(record.cast::<PromiseCapability>()))
            }
            kind => panic!("the embedder passes a payload of kind {kind}"),
        }
    }
}

pub(crate) fn module_request_into_abi(module_request: &ModuleRequest) -> *const JSModuleRequest {
    core::ptr::from_ref(module_request).cast()
}

/// # Safety
///
/// `module_request` must be one that the runtime lent to load_imported_module, for the duration of that call, or one
/// that the embedder owns and keeps for `'a`.
pub unsafe fn module_request_from_abi<'a>(module_request: *const JSModuleRequest) -> &'a ModuleRequest {
    // SAFETY: The caller passes a module request that the runtime lent it.
    unsafe { &*module_request.cast::<ModuleRequest>() }
}

fn handled_by_host_from_abi(completion: JSCompletion) -> ThrowCompletionOr<HandledByHost> {
    match completion_from_abi(completion)?.0 {
        payload if payload == u64::from(JS_HANDLED_BY_HOST_HANDLED) => Ok(HandledByHost::Handled),
        payload if payload == u64::from(JS_HANDLED_BY_HOST_UNHANDLED) => Ok(HandledByHost::Unhandled),
        payload => panic!("the embedder answered {payload} for whether it handled the buffer"),
    }
}

/// Whether the host handled a buffer, as one of JS_HANDLED_BY_HOST_*.
impl CompletionPayload for HandledByHost {
    fn into_payload(self) -> u64 {
        u64::from(match self {
            HandledByHost::Handled => JS_HANDLED_BY_HOST_HANDLED,
            HandledByHost::Unhandled => JS_HANDLED_BY_HOST_UNHANDLED,
        })
    }
}

/// The address of an object that the VM lends a hook.
fn lent_object_into_abi(object: &Object) -> *mut JSObject {
    // SAFETY: The VM only lends a hook objects that are cells of its heap.
    object_into_abi(unsafe { Gc::from_ref(object) })
}

fn ensure_can_add_private_element(vm: &Vm, object: &Object) -> ThrowCompletionOr<()> {
    completion_from_abi(call_embedder_hook!(
        vm,
        ensure_can_add_private_element,
        lent_object_into_abi(object)
    ))?;
    Ok(())
}

#[allow(clippy::too_many_arguments, reason = "the hook takes the spec's arguments")]
fn ensure_can_compile_strings(
    vm: &Vm,
    callee_realm: Gc<Realm>,
    parameter_strings: &[Utf16String],
    body_string: Utf16View<'_>,
    code_string: Utf16View<'_>,
    compilation_type: CompilationType,
    parameter_args: &[Value],
    body_arg: Value,
) -> ThrowCompletionOr<()> {
    let parameter_strings: Vec<JSUtf16View> = parameter_strings
        .iter()
        .map(|string| JSUtf16View::of(Utf16View::of_string(string)))
        .collect();
    let arguments = JSEnsureCanCompileStringsArguments {
        callee_realm: cell_into_abi(callee_realm),
        parameter_strings: parameter_strings.as_ptr(),
        parameter_string_count: parameter_strings.len(),
        body_string: JSUtf16View::of(body_string),
        code_string: JSUtf16View::of(code_string),
        compilation_type: match compilation_type {
            CompilationType::DirectEval => JS_COMPILATION_TYPE_DIRECT_EVAL,
            CompilationType::IndirectEval => JS_COMPILATION_TYPE_INDIRECT_EVAL,
            CompilationType::Function => JS_COMPILATION_TYPE_FUNCTION,
            CompilationType::Timer => JS_COMPILATION_TYPE_TIMER,
        },
        parameter_args: parameter_args.as_ptr().cast(),
        parameter_arg_count: parameter_args.len(),
        body_arg: body_arg.0,
    };
    completion_from_abi(call_embedder_hook!(
        vm,
        ensure_can_compile_strings,
        &raw const arguments
    ))?;
    Ok(())
}

fn get_code_for_eval(vm: &Vm, argument: &Object) -> Option<Gc<PrimitiveString>> {
    let mut code = Value::UNDEFINED.0;
    if !call_embedder_hook!(vm, get_code_for_eval, lent_object_into_abi(argument), &raw mut code) {
        return None;
    }
    let code = Value(code);
    assert!(code.is_string(), "the code of an object for eval() is a String");
    Some(code.as_string())
}

fn promise_rejection_tracker(vm: &Vm, promise: Gc<Promise>, operation: RejectionOperation) {
    let operation = match operation {
        RejectionOperation::Reject => JS_PROMISE_REJECTION_OPERATION_REJECT,
        RejectionOperation::Handle => JS_PROMISE_REJECTION_OPERATION_HANDLE,
    };
    call_embedder_hook!(vm, promise_rejection_tracker, promise.as_ptr().cast(), operation);
}

fn call_job_callback(
    vm: &Vm,
    job_callback: Gc<JobCallback>,
    this_value: Value,
    arguments: &[Value],
) -> ThrowCompletionOr<Value> {
    let completion = call_embedder_hook!(
        vm,
        call_job_callback,
        cell_into_abi::<JSJobCallback>(job_callback),
        this_value.0,
        arguments.as_ptr().cast(),
        arguments.len(),
    );
    completion_from_abi(completion)
}

fn enqueue_finalization_registry_cleanup_job(vm: &Vm, finalization_registry: Gc<FinalizationRegistry>) {
    call_embedder_hook!(
        vm,
        enqueue_finalization_registry_cleanup_job,
        finalization_registry.as_ptr().cast()
    );
}

fn enqueue_promise_job(vm: &Vm, job: Gc<HeapFunction>, realm: Option<Gc<Realm>>) {
    call_embedder_hook!(
        vm,
        enqueue_promise_job,
        job.as_ptr().cast(),
        optional_cell_into_abi::<JSRealm>(realm)
    );
}

fn promise_job_queue_is_empty(vm: &Vm) -> bool {
    call_embedder_hook!(vm, promise_job_queue_is_empty)
}

fn make_job_callback(vm: &Vm, callable: Gc<FunctionObject>) -> Gc<JobCallback> {
    let job_callback = call_embedder_hook!(vm, make_job_callback, callable.as_ptr().cast());
    // SAFETY: The embedder returns a JobCallback Record, which its stack keeps alive until it is stored.
    unsafe { cell_from_abi(job_callback) }
}

unsafe extern "C" fn append_import_meta_property(properties: *mut c_void, key: JSPropertyKey, value: JSValue) {
    // SAFETY: The sink's context is the list of properties that get_import_meta_properties() collects, and the
    //         embedder lends a live key.
    let (properties, key) = unsafe {
        (
            &*properties.cast::<MarkedVec<'_, (PropertyKey, Value)>>(),
            clone_lent_property_key_from_abi(key),
        )
    };
    properties.push((key, Value(value)));
}

fn get_import_meta_properties(vm: &Vm, module: Gc<SourceTextModule>) -> MarkedVec<'_, (PropertyKey, Value)> {
    let properties = MarkedVec::new(vm);
    let mut sink = JSImportMetaPropertySink {
        context: core::ptr::from_ref(&properties).cast_mut().cast(),
        append: append_import_meta_property,
    };
    call_embedder_hook!(vm, get_import_meta_properties, module.as_ptr().cast(), &raw mut sink);
    properties
}

fn finalize_import_meta(vm: &Vm, import_meta: Gc<Object>, module: Gc<SourceTextModule>) {
    call_embedder_hook!(
        vm,
        finalize_import_meta,
        import_meta.as_ptr().cast(),
        module.as_ptr().cast()
    );
}

unsafe extern "C" fn append_utf16_string(strings: *mut c_void, code_units: *const u16, length: usize) {
    // SAFETY: The sink's context is the list of strings being collected, and the embedder passes `length` code units.
    let (strings, code_units) = unsafe {
        (
            &mut *strings.cast::<Vec<Utf16String>>(),
            JSUtf16View {
                data: code_units.cast(),
                length_in_code_units: length,
                has_ascii_storage: false,
            }
            .as_view(),
        )
    };
    strings.push(code_units.to_utf16_string());
}

fn get_supported_import_attributes(vm: &Vm) -> Vec<Utf16String> {
    let mut attribute_keys = Vec::new();
    let mut sink = JSStringSink {
        context: (&raw mut attribute_keys).cast(),
        append: Some(append_utf16_string),
    };
    call_embedder_hook!(vm, get_supported_import_attributes, &raw mut sink);
    attribute_keys
}

fn load_imported_module(
    vm: &Vm,
    referrer: ImportedModuleReferrer,
    module_request: &ModuleRequest,
    host_defined: Option<NonNull<c_void>>,
    payload: ImportedModulePayload,
) {
    call_embedder_hook!(
        vm,
        load_imported_module,
        imported_module_referrer_into_abi(referrer),
        module_request_into_abi(module_request),
        host_defined.map_or(core::ptr::null_mut(), NonNull::as_ptr),
        imported_module_payload_into_abi(payload),
    );
}

fn unrecognized_date_string(vm: &Vm, date_string: Utf16View<'_>) {
    call_embedder_hook!(vm, unrecognized_date_string, JSUtf16View::of(date_string));
}

fn resize_array_buffer(vm: &Vm, buffer: &ArrayBuffer, new_byte_length: usize) -> ThrowCompletionOr<HandledByHost> {
    let buffer = core::ptr::from_ref(buffer).cast_mut().cast();
    handled_by_host_from_abi(call_embedder_hook!(vm, resize_array_buffer, buffer, new_byte_length))
}

fn grow_shared_array_buffer(vm: &Vm, buffer: &ArrayBuffer, new_byte_length: usize) -> ThrowCompletionOr<HandledByHost> {
    let buffer = core::ptr::from_ref(buffer).cast_mut().cast();
    handled_by_host_from_abi(call_embedder_hook!(
        vm,
        grow_shared_array_buffer,
        buffer,
        new_byte_length
    ))
}

fn system_utc_epoch_nanoseconds(vm: &Vm, global: &Object) -> SignedBigInteger {
    // NB: Every i64 lies between nsMinInstant and nsMaxInstant, so there is nothing to clamp.
    SignedBigInteger::from(call_embedder_hook!(
        vm,
        system_utc_epoch_nanoseconds,
        lent_object_into_abi(global)
    ))
}

fn on_unimplemented_property_access(vm: &Vm, object: &Object, key: &PropertyKey) {
    call_embedder_hook!(
        vm,
        on_unimplemented_property_access,
        lent_object_into_abi(object),
        lend_property_key_to_abi(key)
    );
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::{Cell, RefCell};
    use core::ffi::c_void;
    use std::collections::BTreeSet;

    use ak::{Utf16FlyString, Utf16String};

    use super::*;
    use crate::embedding::abi_types::completion_into_abi;
    use crate::embedding::promise::js_promise_job_run;
    use crate::embedding::realm::js_realm_job_callback_create;
    use crate::embedding::vm::{
        js_vm_default_host_call_job_callback, js_vm_default_host_enqueue_finalization_registry_cleanup_job,
        js_vm_default_host_enqueue_promise_job, js_vm_default_host_grow_shared_array_buffer,
        js_vm_default_host_load_imported_module, js_vm_default_host_promise_job_queue_is_empty,
        js_vm_default_host_resize_array_buffer, js_vm_default_host_system_utc_epoch_nanoseconds,
        js_vm_run_queued_finalization_registry_cleanup_jobs, js_vm_run_queued_promise_jobs, js_vm_set_agent,
        js_vm_set_embedder,
    };
    use crate::layout::host_class::{JS_COMPLETION_NORMAL, JS_COMPLETION_THROW};
    use crate::runtime::completion::Must;
    use crate::runtime::cyclic_module::promise_of;
    use crate::runtime::error::ErrorKind;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::module::{Module, finish_loading_imported_module};
    use crate::runtime::promise::PromiseState;
    use crate::runtime::realm::test_realm::{key, slot_holding};
    use crate::source_code::SourceCode;
    use crate::utilities::initialize_realm;

    /// Calls the hooks again from a script that a hook runs while the VM waits for it.
    const REENTRANT_SCRIPT: &str = r#"
        (() => {
            class Reentered { #field = 1; }
            new Reentered();
            eval("1");
            new Function("a", "return a");
            eval({ trustedCode: "2" });
            Promise.reject(1).catch(() => {});
            Promise.resolve(1).then(() => {});
            (async () => { await 1; await 2; })();
            new Date("no date in here either");
            new ArrayBuffer(1, { maxByteLength: 2 }).resize(2);
            new SharedArrayBuffer(1, { maxByteLength: 2 }).grow(2);
            Temporal.Now.instant();
            globalThis.stillNotImplemented;
        })();
    "#;

    const TEST_EPOCH_NANOSECONDS: i64 = 1_234_567_890_123_456_789;

    /// An embedder that records which of its hooks ran and queues promise jobs, as an event loop queues microtasks.
    /// When a test asks, the first call of each hook also runs REENTRANT_SCRIPT and acts on what it was given while the
    /// VM waits for it, which would find any borrow of VM state held across the call. Re-entering once per hook keeps
    /// the jobs that the script queues from queueing more forever.
    struct TestEmbedder<'vm> {
        vm: &'vm Vm,
        realm: Gc<Realm>,
        reenter: bool,
        reentering: Cell<bool>,
        calls: RefCell<BTreeSet<&'static str>>,
        reentered_from: RefCell<BTreeSet<&'static str>>,
        promise_jobs: MarkedVec<'vm, Gc<HeapFunction>>,
        promise_jobs_run: Cell<usize>,
        finalization_registries: MarkedVec<'vm, Gc<FinalizationRegistry>>,
        compiled_strings: RefCell<Vec<String>>,
        rejection_operations: RefCell<Vec<u8>>,
        unrecognized_date_strings: RefCell<Vec<String>>,
        unimplemented_property_keys: RefCell<Vec<String>>,
    }

    impl<'vm> TestEmbedder<'vm> {
        fn new(vm: &'vm Vm, realm: Gc<Realm>, reenter: bool) -> Self {
            Self {
                vm,
                realm,
                reenter,
                reentering: Cell::new(false),
                calls: RefCell::default(),
                reentered_from: RefCell::default(),
                promise_jobs: MarkedVec::new(vm),
                promise_jobs_run: Cell::new(0),
                finalization_registries: MarkedVec::new(vm),
                compiled_strings: RefCell::default(),
                rejection_operations: RefCell::default(),
                unrecognized_date_strings: RefCell::default(),
                unimplemented_property_keys: RefCell::default(),
            }
        }

        fn install(&self) {
            let data = core::ptr::from_ref(self).cast_mut().cast();
            // SAFETY: The VM is live, and the embedder outlives every script the test runs.
            unsafe { js_vm_set_embedder(vm_into_abi(self.vm), &raw const TEST_HOOKS, data) };
        }

        fn install_agent(&self) {
            let agent = JSAgent {
                can_block: true,
                spin_event_loop_until: Some(test_spin_event_loop_until),
                data: core::ptr::from_ref(self).cast_mut().cast(),
            };
            // SAFETY: The VM is live, and the embedder outlives every script the test runs.
            unsafe { js_vm_set_agent(vm_into_abi(self.vm), &raw const agent) };
        }

        /// # Safety
        ///
        /// `data` must be the embedder that the test installed.
        unsafe fn of<'a>(data: *mut c_void) -> &'a TestEmbedder<'a> {
            // SAFETY: The caller passes the installed embedder, which outlives the hook call.
            unsafe { &*data.cast() }
        }

        fn called(&self, hook: &'static str, reentry: impl FnOnce()) {
            self.calls.borrow_mut().insert(hook);
            if !self.reenter || self.reentering.get() || !self.reentered_from.borrow_mut().insert(hook) {
                return;
            }
            self.reentering.set(true);
            reentry();
            run_script(self.vm, self.realm, REENTRANT_SCRIPT).must();
            self.reentering.set(false);
        }

        /// Runs the oldest promise job that has not run yet, if there is one.
        fn run_promise_job(&self) -> bool {
            let Some(job) = self.promise_jobs.get(self.promise_jobs_run.get()) else {
                return false;
            };
            self.promise_jobs_run.set(self.promise_jobs_run.get() + 1);
            // SAFETY: The VM is live, and the queue keeps the job alive.
            let completion = unsafe { js_promise_job_run(vm_into_abi(self.vm), job.as_ptr().cast()) };
            assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
            true
        }

        fn run_promise_jobs(&self) {
            while self.run_promise_job() {}
        }
    }

    unsafe extern "C" fn test_ensure_can_add_private_element(
        data: *mut c_void,
        _: *mut JSVM,
        object: *mut JSObject,
    ) -> JSCompletion {
        // SAFETY: The VM passes the installed embedder and a live object.
        let (embedder, object) = unsafe { (TestEmbedder::of(data), cell_from_abi(object)) };
        embedder.called("ensure_can_add_private_element", || {});
        let vm = embedder.vm;
        // NB: Reading the property can run a getter.
        let refuses = object.get(vm, &key("refusePrivateElements"));
        completion_into_abi(refuses.and_then(|refuses| {
            if refuses.to_boolean() {
                return vm.throw_completion_with_message(ErrorKind::TypeError, "refused".to_string());
            }
            Ok(Value::UNDEFINED)
        }))
    }

    unsafe extern "C" fn test_ensure_can_compile_strings(
        data: *mut c_void,
        _: *mut JSVM,
        arguments: *const JSEnsureCanCompileStringsArguments,
    ) -> JSCompletion {
        // SAFETY: The VM passes the installed embedder and arguments that are valid for the call.
        let (embedder, arguments) = unsafe { (TestEmbedder::of(data), &*arguments) };
        // SAFETY: As above.
        let (parameter_strings, body_string, code_string) = unsafe {
            (
                core::slice::from_raw_parts(arguments.parameter_strings, arguments.parameter_string_count),
                arguments.body_string.as_view().to_utf8(),
                arguments.code_string.as_view().to_utf8(),
            )
        };
        let parameter_strings: Vec<String> = parameter_strings
            .iter()
            // SAFETY: As above.
            .map(|string| unsafe { string.as_view() }.to_utf8())
            .collect();
        // SAFETY: The callee realm is live.
        assert!(unsafe { cell_from_abi(arguments.callee_realm) } == embedder.realm);
        embedder.compiled_strings.borrow_mut().push(format!(
            "{}:{}:{}",
            arguments.compilation_type,
            parameter_strings.join(","),
            body_string.trim()
        ));
        embedder.called("ensure_can_compile_strings", || {});
        if code_string == "forbidden" {
            return JSCompletion {
                payload: arguments.body_arg,
                variant: JS_COMPLETION_THROW,
            };
        }
        completion_into_abi(Ok(()))
    }

    unsafe extern "C" fn test_get_code_for_eval(
        data: *mut c_void,
        _: *mut JSVM,
        argument: *mut JSObject,
        code: *mut JSValue,
    ) -> bool {
        // SAFETY: The VM passes the installed embedder and a live object.
        let (embedder, argument) = unsafe { (TestEmbedder::of(data), cell_from_abi(argument)) };
        embedder.called("get_code_for_eval", || {});
        // NB: Reading the property can run a getter.
        let trusted_code = argument.get(embedder.vm, &key("trustedCode")).must();
        if !trusted_code.is_string() {
            return false;
        }
        // SAFETY: The VM passes where the code goes.
        unsafe { code.write(trusted_code.0) };
        true
    }

    unsafe extern "C" fn test_promise_rejection_tracker(
        data: *mut c_void,
        _: *mut JSVM,
        promise: *mut JSObject,
        operation: u8,
    ) {
        // SAFETY: The VM passes the installed embedder and a live promise.
        let (embedder, promise) = unsafe { (TestEmbedder::of(data), cell_from_abi(promise)) };
        embedder.rejection_operations.borrow_mut().push(operation);
        embedder.called("promise_rejection_tracker", || {
            // Reacts to the promise while the VM settles it or adds a reaction to it.
            let vm = embedder.vm;
            embedder
                .realm
                .global_object()
                .create_data_property_or_throw(vm, &key("trackedPromise"), Value::from_object(promise))
                .must();
            run_script(vm, embedder.realm, "trackedPromise.then(() => {}, () => {});").must();
        });
    }

    unsafe extern "C" fn test_call_job_callback(
        data: *mut c_void,
        vm: *mut JSVM,
        job_callback: *mut JSJobCallback,
        this_value: JSValue,
        arguments: *const JSValue,
        argument_count: usize,
    ) -> JSCompletion {
        // SAFETY: The VM passes the installed embedder.
        unsafe { TestEmbedder::of(data) }.called("call_job_callback", || {});
        // SAFETY: The VM passes a live JobCallback Record and its arguments.
        unsafe { js_vm_default_host_call_job_callback(vm, job_callback, this_value, arguments, argument_count) }
    }

    unsafe extern "C" fn test_enqueue_finalization_registry_cleanup_job(
        data: *mut c_void,
        _: *mut JSVM,
        finalization_registry: *mut JSObject,
    ) {
        // SAFETY: The VM passes the installed embedder and a live finalization registry.
        let (embedder, finalization_registry) =
            unsafe { (TestEmbedder::of(data), cell_from_abi(finalization_registry)) };
        let finalization_registry = finalization_registry
            .downcast::<FinalizationRegistry>()
            .expect("the hook gets a finalization registry");
        let hook = "enqueue_finalization_registry_cleanup_job";
        embedder.calls.borrow_mut().insert(hook);
        if !embedder.reenter || embedder.reentering.get() || !embedder.reentered_from.borrow_mut().insert(hook) {
            embedder.finalization_registries.push(finalization_registry);
            return;
        }
        embedder.reentering.set(true);
        // NB: The tests collect garbage outside of any script, so the cleanup can run right away.
        finalization_registry.cleanup(embedder.vm, None).must();
        run_script(
            embedder.vm,
            embedder.realm,
            "registry.register({}, 'registered during the cleanup')",
        )
        .must();
        embedder.reentering.set(false);
    }

    unsafe extern "C" fn test_enqueue_promise_job(
        data: *mut c_void,
        _: *mut JSVM,
        job: *mut JSPromiseJob,
        realm: *mut JSRealm,
    ) {
        // SAFETY: The VM passes the installed embedder, a live job and a live realm or null.
        let (embedder, job) = unsafe { (TestEmbedder::of(data), promise_job_from_abi(job)) };
        // SAFETY: As above.
        assert!(realm.is_null() || unsafe { cell_from_abi(realm) } == embedder.realm);
        embedder.promise_jobs.push(job);
        embedder.called("enqueue_promise_job", || {});
    }

    unsafe extern "C" fn test_promise_job_queue_is_empty(data: *mut c_void, _: *mut JSVM) -> bool {
        // SAFETY: The VM passes the installed embedder.
        let embedder = unsafe { TestEmbedder::of(data) };
        embedder.called("promise_job_queue_is_empty", || {});
        embedder.promise_jobs_run.get() == embedder.promise_jobs.len()
    }

    unsafe extern "C" fn test_make_job_callback(
        data: *mut c_void,
        vm: *mut JSVM,
        callable: *mut JSObject,
    ) -> *mut JSJobCallback {
        // SAFETY: The VM passes the installed embedder.
        unsafe { TestEmbedder::of(data) }.called("make_job_callback", || {});
        // SAFETY: The VM passes a live function object.
        unsafe { js_realm_job_callback_create(vm, callable, core::ptr::null_mut()) }
    }

    unsafe extern "C" fn test_get_import_meta_properties(
        data: *mut c_void,
        _: *mut JSVM,
        _: *mut JSModule,
        properties: *mut JSImportMetaPropertySink,
    ) {
        // SAFETY: The VM passes the installed embedder and a sink for the call.
        let (embedder, properties) = unsafe { (TestEmbedder::of(data), &*properties) };
        embedder.called("get_import_meta_properties", || {});
        let url = Value::from_string(PrimitiveString::create_from_utf8(embedder.vm, "test://dependency"));
        // SAFETY: The sink takes a key that it copies and a live value.
        unsafe { (properties.append)(properties.context, lend_property_key_to_abi(&key("url")), url.0) };
    }

    unsafe extern "C" fn test_finalize_import_meta(
        data: *mut c_void,
        _: *mut JSVM,
        import_meta: *mut JSObject,
        _: *mut JSModule,
    ) {
        // SAFETY: The VM passes the installed embedder and a live object.
        let (embedder, import_meta) = unsafe { (TestEmbedder::of(data), cell_from_abi(import_meta)) };
        embedder.called("finalize_import_meta", || {});
        import_meta
            .create_data_property_or_throw(embedder.vm, &key("finalized"), Value::TRUE)
            .must();
    }

    unsafe extern "C" fn test_get_supported_import_attributes(
        data: *mut c_void,
        _: *mut JSVM,
        attribute_keys: *mut JSStringSink,
    ) {
        // SAFETY: The VM passes the installed embedder and a sink for the call.
        let (embedder, attribute_keys) = unsafe { (TestEmbedder::of(data), &*attribute_keys) };
        embedder.called("get_supported_import_attributes", || {});
        let type_key: Vec<u16> = "type".encode_utf16().collect();
        let append = attribute_keys.append.expect("the sink appends");
        // SAFETY: The sink copies the code units.
        unsafe { append(attribute_keys.context, type_key.as_ptr(), type_key.len()) };
    }

    unsafe extern "C" fn test_load_imported_module(
        data: *mut c_void,
        _: *mut JSVM,
        referrer: JSImportedModuleReferrer,
        module_request: *const JSModuleRequest,
        host_defined: *mut c_void,
        payload: JSImportedModulePayload,
    ) {
        // SAFETY: The VM passes the installed embedder, and a referrer, module request and payload of its own.
        let (embedder, referrer, module_request, payload) = unsafe {
            (
                TestEmbedder::of(data),
                imported_module_referrer_from_abi(referrer),
                module_request_from_abi(module_request),
                imported_module_payload_from_abi(payload),
            )
        };
        assert!(host_defined.is_null());
        embedder.called("load_imported_module", || {});
        let vm = embedder.vm;
        let specifier = Utf16View::of_fly_string(&module_request.module_specifier).to_utf8();
        let module: ThrowCompletionOr<Gc<Module>> = if specifier == "dependency" {
            let source_code = SourceCode::create(
                Utf16String::from_utf8("dependency"),
                Utf16String::from_utf8("export const answer = 42; export const meta = import.meta;"),
            );
            let Ok(module) = SourceTextModule::parse(vm, source_code, embedder.realm, "dependency") else {
                panic!("the dependency parses");
            };
            Ok(module.upcast())
        } else {
            vm.throw_completion_with_message(ErrorKind::TypeError, format!("there is no module {specifier}"))
        };
        finish_loading_imported_module(vm, referrer, module_request, payload, module);
    }

    unsafe extern "C" fn test_unrecognized_date_string(data: *mut c_void, _: *mut JSVM, date_string: JSUtf16View) {
        // SAFETY: The VM passes the installed embedder.
        let embedder = unsafe { TestEmbedder::of(data) };
        embedder.called("unrecognized_date_string", || {
            // Resolves the substring that the script parsed as a date, after which a collection may free the string
            // it was taken from, before the hook reads its argument.
            run_script(
                embedder.vm,
                embedder.realm,
                "({})[notADate] = 1; notADate = undefined; new Array(1000).fill().map((_, i) => 'filler ' + i);",
            )
            .must();
            embedder.vm.heap().collect_garbage();
        });
        // SAFETY: The string is borrowed for the call.
        let date_string = unsafe { date_string.as_view() }.to_utf8();
        embedder.unrecognized_date_strings.borrow_mut().push(date_string);
    }

    unsafe extern "C" fn test_resize_array_buffer(
        data: *mut c_void,
        vm: *mut JSVM,
        buffer: *mut JSObject,
        new_byte_length: usize,
    ) -> JSCompletion {
        // SAFETY: The VM passes the installed embedder.
        unsafe { TestEmbedder::of(data) }.called("resize_array_buffer", || {});
        // SAFETY: The VM passes a live ArrayBuffer.
        unsafe { js_vm_default_host_resize_array_buffer(vm, buffer, new_byte_length) }
    }

    unsafe extern "C" fn test_grow_shared_array_buffer(
        data: *mut c_void,
        vm: *mut JSVM,
        buffer: *mut JSObject,
        new_byte_length: usize,
    ) -> JSCompletion {
        // SAFETY: The VM passes the installed embedder.
        unsafe { TestEmbedder::of(data) }.called("grow_shared_array_buffer", || {});
        // SAFETY: The VM passes a live SharedArrayBuffer.
        unsafe { js_vm_default_host_grow_shared_array_buffer(vm, buffer, new_byte_length) }
    }

    unsafe extern "C" fn test_system_utc_epoch_nanoseconds(data: *mut c_void, _: *mut JSVM, _: *mut JSObject) -> i64 {
        // SAFETY: The VM passes the installed embedder.
        unsafe { TestEmbedder::of(data) }.called("system_utc_epoch_nanoseconds", || {});
        TEST_EPOCH_NANOSECONDS
    }

    unsafe extern "C" fn test_on_unimplemented_property_access(
        data: *mut c_void,
        _: *mut JSVM,
        object: *mut JSObject,
        key: JSPropertyKey,
    ) {
        // SAFETY: The VM passes the installed embedder, a live object and a key it lends.
        let (embedder, object, key) = unsafe {
            (
                TestEmbedder::of(data),
                cell_from_abi(object),
                clone_lent_property_key_from_abi(key),
            )
        };
        embedder
            .unimplemented_property_keys
            .borrow_mut()
            .push(Utf16View::of_string(&key.to_utf16_string()).to_utf8());
        embedder.called("on_unimplemented_property_access", || {
            object.get(embedder.vm, &key).must();
        });
    }

    unsafe extern "C" fn test_spin_event_loop_until(
        data: *mut c_void,
        _: *mut JSVM,
        goal_condition: JSGoalCondition,
        goal_context: *mut c_void,
    ) {
        // SAFETY: The VM passes the embedder's data.
        let embedder = unsafe { TestEmbedder::of(data) };
        embedder.called("spin_event_loop_until", || {});
        // SAFETY: The VM passes a goal condition with its context, for the duration of the spin.
        while !unsafe { goal_condition(goal_context) } {
            assert!(
                embedder.run_promise_job(),
                "the goal is met before the promise jobs run out"
            );
        }
    }

    static TEST_HOOKS: JSVmHostHooks = JSVmHostHooks {
        ensure_can_add_private_element: Some(test_ensure_can_add_private_element),
        ensure_can_compile_strings: Some(test_ensure_can_compile_strings),
        get_code_for_eval: Some(test_get_code_for_eval),
        promise_rejection_tracker: Some(test_promise_rejection_tracker),
        call_job_callback: Some(test_call_job_callback),
        enqueue_finalization_registry_cleanup_job: Some(test_enqueue_finalization_registry_cleanup_job),
        enqueue_promise_job: Some(test_enqueue_promise_job),
        promise_job_queue_is_empty: Some(test_promise_job_queue_is_empty),
        make_job_callback: Some(test_make_job_callback),
        get_import_meta_properties: Some(test_get_import_meta_properties),
        finalize_import_meta: Some(test_finalize_import_meta),
        get_supported_import_attributes: Some(test_get_supported_import_attributes),
        load_imported_module: Some(test_load_imported_module),
        unrecognized_date_string: Some(test_unrecognized_date_string),
        resize_array_buffer: Some(test_resize_array_buffer),
        grow_shared_array_buffer: Some(test_grow_shared_array_buffer),
        system_utc_epoch_nanoseconds: Some(test_system_utc_epoch_nanoseconds),
        on_unimplemented_property_access: Some(test_on_unimplemented_property_access),
    };

    const SCRIPT_REACHING_THE_HOOKS: &str = r#"
        class ReturnsItsArgument { constructor(object) { return object; } }
        class Stamper extends ReturnsItsArgument { #stamp = 1; static isStamped(object) { return #stamp in object; } }
        const stamped = {};
        new Stamper(stamped);
        if (!Stamper.isStamped(stamped))
            throw new Error("the stamp is missing");
        try {
            new Stamper({ get refusePrivateElements() { return true; } });
            throw new Error("a refused object was stamped");
        } catch (error) {
            if (!(error instanceof TypeError))
                throw error;
        }

        if (eval("1 + 1") !== 2 || (0, eval)("2 + 2") !== 4 || new Function("a", "b", "return a + b")(20, 22) !== 42)
            throw new Error("compiling strings");
        try {
            eval("forbidden");
            throw new Error("compiled forbidden code");
        } catch (error) {
            if (error !== "forbidden")
                throw error;
        }
        if (eval({ get trustedCode() { return "6 * 7"; } }) !== 42)
            throw new Error("the code of an object");
        const notCode = {};
        if (eval(notCode) !== notCode)
            throw new Error("an object without code");

        globalThis.log = [];
        Promise.reject(1).catch(() => {});
        Promise.resolve(1).then(value => log.push(value));
        (async () => { await 2; await 3; log.push("async"); })();
        import("missing", { with: { type: "json" } }).catch(error => log.push(error.constructor.name));

        // A substring, which views the string it was taken from until it resolves.
        globalThis.notADate = ("no date in ".repeat(20) + "not a date").slice(220);
        if (!isNaN(new Date(notADate)))
            throw new Error("a date that is not one");

        const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
        buffer.resize(12);
        const sharedBuffer = new SharedArrayBuffer(8, { maxByteLength: 16 });
        sharedBuffer.grow(12);
        if (buffer.byteLength !== 12 || sharedBuffer.byteLength !== 12)
            throw new Error("resizing buffers");

        if (Temporal.Now.instant().epochNanoseconds !== 1234567890123456789n)
            throw new Error("the time of the embedder");

        if (globalThis.notYetImplemented !== undefined)
            throw new Error("an unimplemented property");
    "#;

    fn run_the_script_reaching_the_hooks(reenter: bool) -> BTreeSet<&'static str> {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        for name in ["notYetImplemented", "stillNotImplemented"] {
            realm
                .global_object()
                .define_unimplemented_property(&vm, Utf16FlyString::from_utf8(name));
        }
        let embedder = TestEmbedder::new(&vm, realm, reenter);
        embedder.install();

        run_script(&vm, realm, SCRIPT_REACHING_THE_HOOKS).must();
        embedder.run_promise_jobs();
        let log = run_script(&vm, realm, "[...log].sort().join()").must();
        assert_eq!(utf8(log), "1,TypeError,async");

        if !reenter {
            assert_eq!(
                *embedder.compiled_strings.borrow(),
                ["0::1 + 1", "1::2 + 2", "2:a,b:return a + b", "0::forbidden", "0::6 * 7"]
            );
            assert_eq!(
                *embedder.rejection_operations.borrow(),
                [
                    JS_PROMISE_REJECTION_OPERATION_REJECT,
                    JS_PROMISE_REJECTION_OPERATION_HANDLE,
                    JS_PROMISE_REJECTION_OPERATION_REJECT,
                    JS_PROMISE_REJECTION_OPERATION_HANDLE,
                ]
            );
            assert_eq!(*embedder.unrecognized_date_strings.borrow(), ["not a date"]);
            assert_eq!(*embedder.unimplemented_property_keys.borrow(), ["notYetImplemented"]);
        } else {
            assert!(*embedder.reentered_from.borrow() == *embedder.calls.borrow());
            assert!(
                embedder
                    .unrecognized_date_strings
                    .borrow()
                    .iter()
                    .any(|date_string| date_string == "not a date")
            );
        }
        embedder.calls.take()
    }

    const HOOKS_THE_SCRIPT_REACHES: [&str; 15] = [
        "call_job_callback",
        "enqueue_promise_job",
        "ensure_can_add_private_element",
        "ensure_can_compile_strings",
        "get_code_for_eval",
        "get_supported_import_attributes",
        "grow_shared_array_buffer",
        "load_imported_module",
        "make_job_callback",
        "on_unimplemented_property_access",
        "promise_job_queue_is_empty",
        "promise_rejection_tracker",
        "resize_array_buffer",
        "system_utc_epoch_nanoseconds",
        "unrecognized_date_string",
    ];

    #[test]
    fn scripts_reach_the_hooks_of_the_embedder() {
        let calls = run_the_script_reaching_the_hooks(false);
        assert_eq!(calls, BTreeSet::from(HOOKS_THE_SCRIPT_REACHES));
    }

    #[test]
    fn every_hook_can_run_scripts_while_the_vm_waits_for_it() {
        let calls = run_the_script_reaching_the_hooks(true);
        assert_eq!(calls, BTreeSet::from(HOOKS_THE_SCRIPT_REACHES));
    }

    #[test]
    fn module_hooks_load_modules_and_fill_import_meta() {
        for reenter in [false, true] {
            let vm = Vm::create();
            let root_execution_context = initialize_realm(&vm);
            let realm = root_execution_context.realm();
            let embedder = TestEmbedder::new(&vm, realm, reenter);
            embedder.install();

            let source_code = SourceCode::create(
                Utf16String::from_utf8("main"),
                Utf16String::from_utf8(
                    "import { answer, meta } from 'dependency'; globalThis.result = [answer, meta.url, meta.finalized].join();",
                ),
            );
            let Ok(module) = SourceTextModule::parse(&vm, source_code, realm, "main") else {
                panic!("the module parses");
            };
            vm.run_module(module).must();
            embedder.run_promise_jobs();
            let result = run_script(&vm, realm, "result").must();
            assert_eq!(utf8(result), "42,test://dependency,true");
            for hook in [
                "finalize_import_meta",
                "get_import_meta_properties",
                "get_supported_import_attributes",
                "load_imported_module",
            ] {
                assert!(embedder.calls.borrow().contains(hook), "{hook} ran");
                assert_eq!(embedder.reentered_from.borrow().contains(hook), reenter);
            }
        }
    }

    #[test]
    fn finalization_registries_leave_their_cleanup_to_the_embedder() {
        for reenter in [false, true] {
            let vm = Vm::create();
            let root_execution_context = initialize_realm(&vm);
            let realm = root_execution_context.realm();
            let embedder = TestEmbedder::new(&vm, realm, reenter);
            embedder.install();

            run_script(
                &vm,
                realm,
                r#"
                    var cleaned = [];
                    var registry = new FinalizationRegistry(held => cleaned.push(held));
                    (() => {
                        for (let i = 0; i < 100; ++i)
                            registry.register({}, i);
                    })();
                "#,
            )
            .must();
            vm.heap().collect_garbage();
            assert!(
                embedder
                    .calls
                    .borrow()
                    .contains("enqueue_finalization_registry_cleanup_job")
            );
            for index in 0..embedder.finalization_registries.len() {
                let finalization_registry = embedder
                    .finalization_registries
                    .get(index)
                    .expect("the index is in bounds");
                finalization_registry.cleanup(&vm, None).must();
            }
            let cleaned = run_script(&vm, realm, "cleaned.length > 0").must();
            assert_eq!(cleaned, Value::TRUE);
            assert_eq!(
                embedder
                    .reentered_from
                    .borrow()
                    .contains("enqueue_finalization_registry_cleanup_job"),
                reenter
            );
        }
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

    static ONLY_THE_TIME_OF_THE_EMBEDDER: JSVmHostHooks = JSVmHostHooks {
        system_utc_epoch_nanoseconds: Some(test_system_utc_epoch_nanoseconds),
        ..NO_HOOKS
    };

    #[test]
    fn installing_hooks_again_gives_the_ones_left_out_back_to_the_runtime() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let embedder = TestEmbedder::new(&vm, realm, false);
        embedder.install();
        let data = core::ptr::from_ref(&embedder).cast_mut().cast();
        const SCRIPT: &str = r#"
            Promise.resolve().then(() => log.push("job"));
            log.push(Temporal.Now.instant().epochNanoseconds === 1234567890123456789n);
        "#;

        run_script(&vm, realm, "globalThis.log = [];").must();
        run_script(&vm, realm, SCRIPT).must();
        assert_eq!(embedder.promise_jobs.len(), 1);

        // SAFETY: The VM is live, and the embedder outlives every script the test runs.
        unsafe { js_vm_set_embedder(vm_into_abi(&vm), &raw const ONLY_THE_TIME_OF_THE_EMBEDDER, data) };
        run_script(&vm, realm, SCRIPT).must();
        assert_eq!(embedder.promise_jobs.len(), 1);

        // SAFETY: The VM is live.
        unsafe { js_vm_set_embedder(vm_into_abi(&vm), core::ptr::null(), core::ptr::null_mut()) };
        run_script(&vm, realm, SCRIPT).must();
        assert!(vm.embedder().is_none());

        embedder.run_promise_jobs();
        let log = run_script(&vm, realm, "log.join()").must();
        assert_eq!(utf8(log), "true,true,job,false,job,job");
    }

    unsafe extern "C" fn forward_call_job_callback(
        _: *mut c_void,
        vm: *mut JSVM,
        job_callback: *mut JSJobCallback,
        this_value: JSValue,
        arguments: *const JSValue,
        argument_count: usize,
    ) -> JSCompletion {
        // SAFETY: The VM passes what the runtime's own hook takes.
        unsafe { js_vm_default_host_call_job_callback(vm, job_callback, this_value, arguments, argument_count) }
    }

    unsafe extern "C" fn forward_enqueue_finalization_registry_cleanup_job(
        _: *mut c_void,
        vm: *mut JSVM,
        finalization_registry: *mut JSObject,
    ) {
        // SAFETY: As above.
        unsafe { js_vm_default_host_enqueue_finalization_registry_cleanup_job(vm, finalization_registry) };
    }

    unsafe extern "C" fn forward_enqueue_promise_job(
        _: *mut c_void,
        vm: *mut JSVM,
        job: *mut JSPromiseJob,
        realm: *mut JSRealm,
    ) {
        // SAFETY: As above.
        unsafe { js_vm_default_host_enqueue_promise_job(vm, job, realm) };
    }

    unsafe extern "C" fn forward_promise_job_queue_is_empty(_: *mut c_void, vm: *mut JSVM) -> bool {
        // SAFETY: As above.
        unsafe { js_vm_default_host_promise_job_queue_is_empty(vm) }
    }

    unsafe extern "C" fn forward_make_job_callback(
        _: *mut c_void,
        vm: *mut JSVM,
        callable: *mut JSObject,
    ) -> *mut JSJobCallback {
        // SAFETY: As above.
        unsafe { js_realm_job_callback_create(vm, callable, core::ptr::null_mut()) }
    }

    unsafe extern "C" fn forward_load_imported_module(
        _: *mut c_void,
        vm: *mut JSVM,
        referrer: JSImportedModuleReferrer,
        module_request: *const JSModuleRequest,
        host_defined: *mut c_void,
        payload: JSImportedModulePayload,
    ) {
        // SAFETY: As above.
        unsafe { js_vm_default_host_load_imported_module(vm, referrer, module_request, host_defined, payload) };
    }

    unsafe extern "C" fn forward_resize_array_buffer(
        _: *mut c_void,
        vm: *mut JSVM,
        buffer: *mut JSObject,
        new_byte_length: usize,
    ) -> JSCompletion {
        // SAFETY: As above.
        unsafe { js_vm_default_host_resize_array_buffer(vm, buffer, new_byte_length) }
    }

    unsafe extern "C" fn forward_grow_shared_array_buffer(
        _: *mut c_void,
        vm: *mut JSVM,
        buffer: *mut JSObject,
        new_byte_length: usize,
    ) -> JSCompletion {
        // SAFETY: As above.
        unsafe { js_vm_default_host_grow_shared_array_buffer(vm, buffer, new_byte_length) }
    }

    unsafe extern "C" fn forward_system_utc_epoch_nanoseconds(
        _: *mut c_void,
        vm: *mut JSVM,
        global: *mut JSObject,
    ) -> i64 {
        // SAFETY: As above.
        unsafe { js_vm_default_host_system_utc_epoch_nanoseconds(vm, global) }
    }

    /// The hooks of an embedder that leaves everything to the runtime's own hooks, which C++ JS::VM does for every hook
    /// that its embedder does not replace.
    static HOOKS_FALLING_BACK_TO_THE_RUNTIME: JSVmHostHooks = JSVmHostHooks {
        call_job_callback: Some(forward_call_job_callback),
        enqueue_finalization_registry_cleanup_job: Some(forward_enqueue_finalization_registry_cleanup_job),
        enqueue_promise_job: Some(forward_enqueue_promise_job),
        promise_job_queue_is_empty: Some(forward_promise_job_queue_is_empty),
        make_job_callback: Some(forward_make_job_callback),
        load_imported_module: Some(forward_load_imported_module),
        resize_array_buffer: Some(forward_resize_array_buffer),
        grow_shared_array_buffer: Some(forward_grow_shared_array_buffer),
        system_utc_epoch_nanoseconds: Some(forward_system_utc_epoch_nanoseconds),
        ..NO_HOOKS
    };

    const SCRIPT_REACHING_THE_HOOKS_WITH_RUNTIME_DEFAULTS: &str = r#"
        globalThis.log = [];
        Promise.resolve(1).then(value => log.push(value));
        (async () => { await 2; await 3; log.push("async"); })();
        import("./there-is-no-such-module.mjs").catch(error => log.push(error.constructor.name));

        const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
        buffer.resize(12);
        const sharedBuffer = new SharedArrayBuffer(8, { maxByteLength: 16 });
        sharedBuffer.grow(12);
        log.push(buffer.byteLength, sharedBuffer.byteLength, Temporal.Now.instant().epochNanoseconds > 0n);

        var cleaned = [];
        var registry = new FinalizationRegistry(held => cleaned.push(held));
        (() => {
            for (let i = 0; i < 100; ++i)
                registry.register({}, i);
        })();
    "#;

    fn run_the_script_reaching_the_hooks_with_runtime_defaults(hooks: Option<&'static JSVmHostHooks>) -> String {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        if let Some(hooks) = hooks {
            // SAFETY: The VM is live, and the hooks take no data.
            unsafe { js_vm_set_embedder(vm_into_abi(&vm), hooks, core::ptr::null_mut()) };
        }
        run_script(&vm, realm, SCRIPT_REACHING_THE_HOOKS_WITH_RUNTIME_DEFAULTS).must();
        vm.heap().collect_garbage();
        // SAFETY: The VM is live.
        unsafe {
            js_vm_run_queued_finalization_registry_cleanup_jobs(vm_into_abi(&vm));
            js_vm_run_queued_promise_jobs(vm_into_abi(&vm));
        }
        let log = run_script(&vm, realm, "[...log, cleaned.length > 0].join()").must();
        utf8(log)
    }

    #[test]
    fn hooks_that_fall_back_to_the_runtime_behave_like_no_hooks() {
        let log_without_hooks = run_the_script_reaching_the_hooks_with_runtime_defaults(None);
        assert_eq!(log_without_hooks, "12,12,true,1,InternalError,async,true");
        assert_eq!(
            run_the_script_reaching_the_hooks_with_runtime_defaults(Some(&HOOKS_FALLING_BACK_TO_THE_RUNTIME)),
            log_without_hooks
        );
    }

    std::thread_local! {
        /// The hostDefined of every call of record_host_defined_and_load_files().
        static HOST_DEFINED_OF_LOADS: RefCell<Vec<*mut c_void>> = const { RefCell::new(Vec::new()) };
    }

    /// A load_imported_module hook that records its hostDefined and hands the load, with that hostDefined, to the
    /// runtime's own hook, which loads files.
    unsafe extern "C" fn record_host_defined_and_load_files(
        _: *mut c_void,
        vm: *mut JSVM,
        referrer: JSImportedModuleReferrer,
        module_request: *const JSModuleRequest,
        host_defined: *mut c_void,
        payload: JSImportedModulePayload,
    ) {
        HOST_DEFINED_OF_LOADS.with_borrow_mut(|loads| loads.push(host_defined));
        // SAFETY: The VM passes what the runtime's own hook takes.
        unsafe { js_vm_default_host_load_imported_module(vm, referrer, module_request, host_defined, payload) };
    }

    static HOOKS_RECORDING_HOST_DEFINED: JSVmHostHooks = JSVmHostHooks {
        load_imported_module: Some(record_host_defined_and_load_files),
        ..NO_HOOKS
    };

    #[test]
    fn load_imported_module_receives_the_host_defined_cell_of_the_graph() {
        let directory = std::env::temp_dir().join(format!("libjs-embedding-host-defined-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is created");
        std::fs::write(
            directory.join("dependency.mjs"),
            "import './leaf.mjs'; export const answer = 42;",
        )
        .expect("the dependency is written");
        std::fs::write(directory.join("leaf.mjs"), "export const leaf = 1;").expect("the leaf is written");

        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        // SAFETY: The VM is live, and the hooks take no data.
        unsafe {
            js_vm_set_embedder(
                vm_into_abi(&vm),
                &raw const HOOKS_RECORDING_HOST_DEFINED,
                core::ptr::null_mut(),
            );
        }
        let main_filename = directory.join("main.mjs").to_string_lossy().into_owned();
        let source_code = SourceCode::create(
            Utf16String::from_utf8(&main_filename),
            Utf16String::from_utf8("import './dependency.mjs';"),
        );
        let Ok(main) = SourceTextModule::parse(&vm, source_code, realm, &main_filename) else {
            panic!("the module parses");
        };
        let host_defined = run_script(&vm, realm, "({})").must().as_object();

        // The runtime's own hook finishes each load before it returns, so the graph is loaded right away.
        let promise_capability = main.load_requested_modules(&vm, slot_holding(host_defined));
        std::fs::remove_dir_all(&directory).expect("the directory is removed");
        assert_eq!(promise_of(promise_capability).state(), PromiseState::Fulfilled);
        let host_defined: *mut c_void = host_defined.as_ptr().cast();
        assert_eq!(HOST_DEFINED_OF_LOADS.take(), [host_defined, host_defined]);
    }

    /// Awaits in native code twice: AsyncDisposableStack.prototype.disposeAsync() awaits what an async dispose method
    /// returns, and the job that settles the outer one awaits a nested one, which spins the event loop inside the
    /// first spin.
    const SCRIPT_AWAITING_IN_NATIVE_CODE: &str = r#"
        var log = [];
        function disposeAsyncOf(name, settle) {
            const stack = new AsyncDisposableStack();
            stack.use({
                [Symbol.asyncDispose]() {
                    log.push(name);
                    return Promise.resolve().then(() => { settle(); log.push(name + " settled"); });
                },
            });
            return stack.disposeAsync();
        }
        Promise.resolve().then(() => log.push("queued before"));
        disposeAsyncOf("outer", () => disposeAsyncOf("nested", () => {})).then(() => log.push("outer disposed"));
        log.push("after");
    "#;

    const LOG_OF_THE_SCRIPT_AWAITING_IN_NATIVE_CODE: &str =
        "outer,queued before,nested,nested settled,outer settled,after,outer disposed";

    #[test]
    fn await_spins_the_event_loop_of_the_embedder_agent() {
        for reenter in [false, true] {
            let vm = Vm::create();
            let root_execution_context = initialize_realm(&vm);
            let realm = root_execution_context.realm();
            let embedder = TestEmbedder::new(&vm, realm, reenter);
            embedder.install();
            embedder.install_agent();

            run_script(&vm, realm, SCRIPT_AWAITING_IN_NATIVE_CODE).must();
            assert!(embedder.calls.borrow().contains("spin_event_loop_until"));
            assert_eq!(
                embedder.reentered_from.borrow().contains("spin_event_loop_until"),
                reenter
            );
            embedder.run_promise_jobs();
            let log = run_script(&vm, realm, "log.join()").must();
            assert_eq!(utf8(log), LOG_OF_THE_SCRIPT_AWAITING_IN_NATIVE_CODE);
        }
    }

    #[test]
    fn await_runs_the_promise_jobs_of_the_vm_without_an_embedder_agent() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let embedder = TestEmbedder::new(&vm, realm, false);
        embedder.install_agent();
        // SAFETY: The VM is live.
        unsafe { js_vm_set_agent(vm_into_abi(&vm), core::ptr::null()) };

        run_script(&vm, realm, SCRIPT_AWAITING_IN_NATIVE_CODE).must();
        assert!(embedder.calls.borrow().is_empty());
        let log = run_script(&vm, realm, "log.join()").must();
        assert_eq!(utf8(log), LOG_OF_THE_SCRIPT_AWAITING_IN_NATIVE_CODE);
    }
}
