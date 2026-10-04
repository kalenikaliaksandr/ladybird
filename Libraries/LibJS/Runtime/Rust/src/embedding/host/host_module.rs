/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Host modules, of kind JS_HOST_CLASS_MODULE: Cyclic Module Records whose abstract methods an embedder implements,
//! as LibWeb does for WebAssembly module records.
//!
//! The exported functions run on the thread that owns the VM and trust their arguments, as object.rs states: `vm` is
//! the embedder's VM, modules, realms and environments are live cells of its heap, a host class is static data that
//! outlives the VM, and host-defined and host data cells are null or live cells of the VM's heap.

#![allow(
    clippy::missing_safety_doc,
    reason = "the module documentation states the contract every exported function shares"
)]

use core::ffi::c_void;
use core::ops::Deref;

use ak::Utf16FlyString;
use libjs_runtime_macros::Trace;

use crate::embedding::abi_types::{
    JSRealm, JSUtf16View, cell_from_abi, cell_into_abi, completion_from_abi, optional_cell_from_abi, vm_from_abi,
};
use crate::embedding::environment::JSEnvironment;
use crate::embedding::hooks::{JSModuleRequest, module_request_from_abi};
use crate::embedding::host::class_table::host_class_from_abi;
use crate::embedding::host::registry::runtime_class_and_allocator_of_host_class;
use crate::embedding::module::promise_capability_into_abi;
use crate::embedding::realm::host_defined_slot_of;
use crate::gc::class::{Class, define_cell};
use crate::gc::foreign::ForeignCellSlot;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::host_class::{
    JS_HOST_CLASS_MODULE, JS_RESOLVED_BINDING_AMBIGUOUS, JS_RESOLVED_BINDING_BINDING_NAME,
    JS_RESOLVED_BINDING_NAMESPACE, JS_RESOLVED_BINDING_NULL, JSHostClass, JSHostModuleHooks, JSModule,
    JSResolvedBinding, JSStringSink, JSVM,
};
use crate::layout::realm::Realm;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::cyclic_module::{CYCLIC_MODULE_METHODS, CyclicModule};
use crate::runtime::module::{Module, ModuleMethods, ResolvedBinding, ResolvedBindingType};
use crate::runtime::module_environment::ModuleEnvironment;
use crate::runtime::module_request::ModuleRequest;
use crate::runtime::promise_capability::PromiseCapability;

/// A Cyclic Module Record whose abstract methods come from its host class, with a C++ GC cell for any state of its
/// own. It never has top-level await.
#[repr(C)]
#[derive(Trace)]
pub struct HostModule {
    base: CyclicModule,
    #[gc(untraced)]
    host_class: &'static JSHostClass,
    host_data: ForeignCellSlot,
}

define_cell!(HostModule, Other, extends: [CyclicModule, Module]);

impl Deref for HostModule {
    type Target = CyclicModule;

    fn deref(&self) -> &CyclicModule {
        &self.base
    }
}

pub const HOST_MODULE_METHODS: ModuleMethods = ModuleMethods {
    get_exported_names: |module, _, _| as_host_module(module).get_exported_names(),
    resolve_export: |module, _, export_name, _| as_host_module(module).resolve_export(export_name),
    initialize_environment: |module, _| as_host_module(module).initialize_environment(),
    execute_module: |module, _, capability| as_host_module(module).execute_module(capability),
    ..CYCLIC_MODULE_METHODS
};

/// The host module a method of a host module was called on.
fn as_host_module(module: &Module) -> &HostModule {
    module
        .downcast_ref::<HostModule>()
        .expect("only host modules have the methods of host modules")
}

/// The hooks of a host class of kind JS_HOST_CLASS_MODULE, all four of which it must have.
fn module_hooks_of(host_class: &'static JSHostClass) -> &'static JSHostModuleHooks {
    // SAFETY: The hooks of a module class are a JSHostModuleHooks, static like the class itself.
    let hooks =
        unsafe { host_class.hooks.cast::<JSHostModuleHooks>().as_ref() }.expect("a host module class has hooks");
    assert!(
        hooks.get_exported_names.is_some()
            && hooks.resolve_export.is_some()
            && hooks.initialize_environment.is_some()
            && hooks.execute_module.is_some(),
        "a host module class has all four hooks"
    );
    hooks
}

/// The class of the module records of `table`, named after it, which the registry derives from `parent`: the class of
/// the table's parent, or HostModule's for a table without a parent module class.
pub fn derive_host_module_class(table: &'static JSHostClass, parent: &'static Class) -> &'static Class {
    Class::derive_runtime_without_object_methods(parent, table.class_name())
}

unsafe extern "C" fn append_exported_name(names: *mut c_void, code_units: *const u16, length_in_code_units: usize) {
    // SAFETY: The sink's context is the list that get_exported_names() collects, and the embedder passes that many code
    //         units.
    let (names, code_units) = unsafe {
        (
            &mut *names.cast::<Vec<Utf16FlyString>>(),
            code_units_of(code_units, length_in_code_units),
        )
    };
    names.push(Utf16FlyString::from_utf16(code_units));
}

unsafe extern "C" fn set_binding_name(binding_name: *mut c_void, code_units: *const u16, length_in_code_units: usize) {
    // SAFETY: The sink's context is the binding name that resolve_export() waits for, and the embedder passes that many
    //         code units.
    let (binding_name, code_units) = unsafe {
        (
            &mut *binding_name.cast::<Option<Utf16FlyString>>(),
            code_units_of(code_units, length_in_code_units),
        )
    };
    assert!(binding_name.is_none(), "a resolved binding has one name");
    *binding_name = Some(Utf16FlyString::from_utf16(code_units));
}

/// # Safety
///
/// Unless `length` is 0, `code_units` must point to that many code units, which stay unchanged for `'a`.
unsafe fn code_units_of<'a>(code_units: *const u16, length: usize) -> &'a [u16] {
    if length == 0 {
        return &[];
    }
    // SAFETY: The caller passes that many code units.
    unsafe { core::slice::from_raw_parts(code_units, length) }
}

fn resolved_binding_type_from_abi(binding_type: u8) -> ResolvedBindingType {
    match binding_type {
        JS_RESOLVED_BINDING_BINDING_NAME => ResolvedBindingType::BindingName,
        JS_RESOLVED_BINDING_NAMESPACE => ResolvedBindingType::Namespace,
        JS_RESOLVED_BINDING_AMBIGUOUS => ResolvedBindingType::Ambiguous,
        JS_RESOLVED_BINDING_NULL => ResolvedBindingType::Null,
        binding_type => panic!("{binding_type} is not a type of resolved binding"),
    }
}

impl HostModule {
    /// A host module of `host_class`, a Cyclic Module Record that requests `requested_modules`. `host_defined` is its
    /// [[HostDefined]], and `host_data` the embedder's cell for its own state.
    pub fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        host_class: &'static JSHostClass,
        filename: String,
        requested_modules: Vec<ModuleRequest>,
        host_defined: ForeignCellSlot,
        host_data: ForeignCellSlot,
    ) -> Gc<HostModule> {
        let (class, allocator) = runtime_class_and_allocator_of_host_class(vm, host_class, JS_HOST_CLASS_MODULE);
        module_hooks_of(host_class);
        vm.heap().allocate_in(
            allocator,
            HostModule {
                base: CyclicModule::new(class, realm, filename, false, requested_modules, host_defined),
                host_class,
                host_data,
            },
        )
    }

    pub fn host_class(&self) -> &'static JSHostClass {
        self.host_class
    }

    pub fn host_data(&self) -> &ForeignCellSlot {
        &self.host_data
    }

    fn hooks(&self) -> &'static JSHostModuleHooks {
        // SAFETY: create() checked that the hooks of the class are those of a module class.
        unsafe { &*self.host_class.hooks.cast::<JSHostModuleHooks>() }
    }

    fn as_abi(&self) -> *mut JSModule {
        core::ptr::from_ref(self).cast_mut().cast()
    }

    fn get_exported_names(&self) -> Vec<Utf16FlyString> {
        let mut exported_names: Vec<Utf16FlyString> = Vec::new();
        let mut names = JSStringSink {
            context: (&raw mut exported_names).cast(),
            append: Some(append_exported_name),
        };
        let get_exported_names = self
            .hooks()
            .get_exported_names
            .expect("a host module class has all four hooks");
        // SAFETY: The hook takes the module and a sink that outlives the call.
        unsafe { get_exported_names(self.as_abi(), &raw mut names) };
        exported_names
    }

    fn resolve_export(&self, export_name: &Utf16FlyString) -> ResolvedBinding {
        let mut binding_name: Option<Utf16FlyString> = None;
        let mut resolved_binding = JSResolvedBinding {
            module: core::ptr::null_mut(),
            binding_name: JSStringSink {
                context: (&raw mut binding_name).cast(),
                append: Some(set_binding_name),
            },
            r#type: JS_RESOLVED_BINDING_NULL,
        };
        let export_name_code_units = export_name.to_utf16();
        let resolve_export = self
            .hooks()
            .resolve_export
            .expect("a host module class has all four hooks");
        // SAFETY: The hook takes the module, the code units of the name and an out record, all of which outlive the
        //         call.
        unsafe {
            resolve_export(
                self.as_abi(),
                export_name_code_units.as_ptr(),
                export_name_code_units.len(),
                &raw mut resolved_binding,
            );
        };
        let binding_type = resolved_binding_type_from_abi(resolved_binding.r#type);
        ResolvedBinding {
            binding_type,
            // SAFETY: A hook resolving to a binding names a live module.
            module: unsafe { optional_cell_from_abi::<JSModule>(resolved_binding.module) },
            export_name: if binding_type == ResolvedBindingType::BindingName {
                binding_name.expect("a hook resolving to a binding appends its name")
            } else {
                Utf16FlyString::default()
            },
        }
    }

    fn initialize_environment(&self) -> ThrowCompletionOr<()> {
        let initialize_environment = self
            .hooks()
            .initialize_environment
            .expect("a host module class has all four hooks");
        // SAFETY: The hook takes the module.
        completion_from_abi(unsafe { initialize_environment(self.as_abi()) }).map(|_| ())
    }

    fn execute_module(&self, capability: Option<Gc<PromiseCapability>>) -> ThrowCompletionOr<()> {
        let execute_module = self
            .hooks()
            .execute_module
            .expect("a host module class has all four hooks");
        let capability = capability.map_or(core::ptr::null_mut(), promise_capability_into_abi);
        // SAFETY: The hook takes the module and its capability, if it has one.
        completion_from_abi(unsafe { execute_module(self.as_abi(), capability) }).map(|_| ())
    }
}

/// The host class of the module, if it is a host module.
pub fn host_class_of(module: &Module) -> Option<&'static JSHostClass> {
    module.downcast_ref::<HostModule>().map(HostModule::host_class)
}

/// # Safety
///
/// `module` must be a live host module.
unsafe fn host_module_from_abi<'a>(module: *mut JSModule) -> &'a HostModule {
    // SAFETY: The caller passes a live module, which stays alive while the embedder holds it.
    let module = unsafe { &*cell_from_abi::<JSModule>(module).as_ptr() };
    as_host_module(module)
}

/// HostModule::create(): a host module of `host_class`, a JS_HOST_CLASS_MODULE class with all four hooks, in `realm`,
/// named `filename`, which module loading resolves its imports against. It copies the `requested_module_count`
/// module requests of `requested_modules`, its [[RequestedModules]]. `host_defined` is its [[HostDefined]] and
/// `host_data` the embedder's cell for its own state, each a cell of the VM's heap or null, which the module keeps
/// alive. Each host class gets a cell allocator of its own unless it has JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT.
/// Returns an unrooted module. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_module_create(
    vm: *mut JSVM,
    realm: *mut JSRealm,
    host_class: *const JSHostClass,
    filename: JSUtf16View,
    requested_modules: *const *const JSModuleRequest,
    requested_module_count: usize,
    host_defined: *mut c_void,
    host_data: *mut c_void,
) -> *mut JSModule {
    // SAFETY: See the module documentation.
    let (vm, realm, host_class, filename) = unsafe {
        (
            vm_from_abi(vm),
            cell_from_abi(realm),
            host_class_from_abi(host_class),
            filename.as_view().to_utf8(),
        )
    };
    let requested_modules = if requested_module_count == 0 {
        Vec::new()
    } else {
        // SAFETY: The embedder passes that many module requests.
        unsafe { core::slice::from_raw_parts(requested_modules, requested_module_count) }
            .iter()
            // SAFETY: Each is a module request that outlives the call.
            .map(|&request| unsafe { module_request_from_abi(request) }.clone())
            .collect()
    };
    // SAFETY: The cells are null or live cells of the VM's heap.
    let (host_defined, host_data) = unsafe { (host_defined_slot_of(host_defined), host_defined_slot_of(host_data)) };
    let module = HostModule::create(
        vm,
        realm,
        host_class,
        filename,
        requested_modules,
        host_defined,
        host_data,
    );
    cell_into_abi(module.upcast::<Module>())
}

/// The module's host class, or null for a module record that the runtime implements. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_module_host_class(module: *mut JSModule) -> *const JSHostClass {
    // SAFETY: See the module documentation.
    let module = unsafe { cell_from_abi::<JSModule>(module) };
    host_class_of(&module).map_or(core::ptr::null(), core::ptr::from_ref)
}

/// The cell the embedder keeps the host module's own state in, or null if it has none or is not a host module. Main
/// thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_module_host_data(module: *mut JSModule) -> *mut c_void {
    // SAFETY: See the module documentation.
    let module = unsafe { cell_from_abi::<JSModule>(module) };
    module
        .downcast_ref::<HostModule>()
        .map_or(core::ptr::null_mut(), |module| module.host_data().as_ptr())
}

/// Replaces the cell of the host module's own state with `host_data`, a cell of the VM's heap or null. Main thread
/// only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_module_set_host_data(module: *mut JSModule, host_data: *mut c_void) {
    // SAFETY: See the module documentation.
    let module = unsafe { host_module_from_abi(module) };
    // SAFETY: The cell is null or a live cell of the VM's heap, which the module keeps alive from now on.
    unsafe { module.host_data().set(core::ptr::NonNull::new(host_data)) };
}

/// Sets the host module's [[Environment]] to `environment`, a module environment, as its initialize_environment hook
/// does. Main thread only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_host_module_set_environment(module: *mut JSModule, environment: *mut JSEnvironment) {
    // SAFETY: See the module documentation.
    let (module, environment) = unsafe {
        (
            host_module_from_abi(module),
            cell_from_abi::<JSEnvironment>(environment),
        )
    };
    let environment = environment
        .downcast::<ModuleEnvironment>()
        .expect("the environment of a module is a module environment");
    module.set_environment(environment);
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod tests {
    use core::cell::{Cell, RefCell};
    use core::ptr::NonNull;

    use super::*;
    use crate::embedding::abi_types::{completion_into_abi, vm_into_abi};
    use crate::embedding::environment::environment_to_abi;
    use crate::embedding::environment::{
        JS_INITIALIZE_BINDING_HINT_NORMAL, js_environment_create_immutable_binding, js_environment_initialize_binding,
        js_environment_new_module_environment,
    };
    use crate::gc::capi::gc_cell_type_info;
    use crate::gc::class::{GcCell, class_of};
    use crate::gc::class_id::ClassId;
    use crate::layout::host_class::{
        JS_HOST_ABI_VERSION, JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT, JSCompletion, JSPromiseCapability,
    };
    use crate::layout::object::Object;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::cyclic_module::{ModuleStatus, promise_of};
    use crate::runtime::error::ErrorKind;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::runtime::promise::{Promise, PromiseState};
    use crate::runtime::property_key::PropertyKey;
    use crate::runtime::realm::test_realm::{
        TestRealm, check_that_host_defined_slots_keep_their_cells_alive, slot_holding,
    };
    use crate::utilities::initialize_realm;

    /// The export names of the test modules, one of which is not ASCII.
    const EXPORTED_NAMES: [&str; 2] = ["answer", "名前"];

    /// What the hooks run while a test asks them to re-enter the VM, which would find any borrow of VM state held
    /// across a hook call.
    const REENTRANT_SCRIPT: &str = r#"
        globalThis.reentries = (globalThis.reentries ?? 0) + 1;
        [3, 1, 2].sort().map(value => ({ value }));
        JSON.parse("[1, 2]");
        Promise.resolve(1).then(() => {});
    "#;

    std::thread_local! {
        static TEST_VM: Cell<*const Vm> = const { Cell::new(core::ptr::null()) };
        static ENVIRONMENT_INITIALIZATION_THROWS: Cell<bool> = const { Cell::new(false) };
        static EXECUTION_THROWS: Cell<bool> = const { Cell::new(false) };
        static HOOKS_REENTER: Cell<bool> = const { Cell::new(false) };
        static HOOK_CALLS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    /// The VM of a test, which the hooks reach the way an embedder reaches its one VM.
    pub(crate) struct TestVm(pub(crate) Box<Vm>);

    impl TestVm {
        pub(crate) fn create() -> Self {
            let test_vm = Self(Vm::create());
            TEST_VM.set(&raw const *test_vm.0);
            test_vm
        }
    }

    impl Drop for TestVm {
        fn drop(&mut self) {
            TEST_VM.set(core::ptr::null());
            HOOK_CALLS.take();
            ENVIRONMENT_INITIALIZATION_THROWS.set(false);
            EXECUTION_THROWS.set(false);
            HOOKS_REENTER.set(false);
        }
    }

    fn vm_of_hooks<'vm>() -> &'vm Vm {
        // SAFETY: Hooks only run while the TestVm of a test is alive.
        unsafe { TEST_VM.get().as_ref() }.expect("a test VM exists")
    }

    fn module_of_hook(module: *mut JSModule) -> Gc<Module> {
        // SAFETY: Hooks receive live modules.
        unsafe { cell_from_abi(module) }
    }

    /// Records the hook's call and, while the test asks for it, runs a script in the module's realm and collects
    /// garbage.
    fn hook_called(name: &'static str, module: *mut JSModule) {
        HOOK_CALLS.with_borrow_mut(|calls| calls.push(name));
        if HOOKS_REENTER.get() {
            let vm = vm_of_hooks();
            run_script(vm, module_of_hook(module).realm(), REENTRANT_SCRIPT).must();
            vm.heap().collect_garbage();
        }
    }

    fn hook_calls_named(name: &str) -> usize {
        HOOK_CALLS.with_borrow(|calls| calls.iter().filter(|call| **call == name).count())
    }

    /// The code units of `text` and a view of them, which lives as long as they do.
    fn view_of(code_units: &[u16]) -> JSUtf16View {
        JSUtf16View {
            data: code_units.as_ptr().cast(),
            length_in_code_units: code_units.len(),
            has_ascii_storage: false,
        }
    }

    fn code_units_of_str(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn throw_from_hook(name: &str) -> JSCompletion {
        completion_into_abi(
            vm_of_hooks().throw_completion_with_message::<()>(ErrorKind::Error, format!("{name} threw")),
        )
    }

    /// # Safety
    ///
    /// `sink` must be a live sink.
    unsafe fn append_to_sink(sink: &JSStringSink, code_units: &[u16]) {
        let append = sink.append.expect("the sink has an append function");
        // SAFETY: The sink takes code units for the duration of the call.
        unsafe { append(sink.context, code_units.as_ptr(), code_units.len()) };
    }

    unsafe extern "C" fn exporting_module_get_exported_names(module: *mut JSModule, names: *mut JSStringSink) {
        hook_called("get_exported_names", module);
        for name in EXPORTED_NAMES {
            // SAFETY: The runtime passes a live sink.
            unsafe { append_to_sink(&*names, &code_units_of_str(name)) };
        }
    }

    unsafe extern "C" fn exporting_module_resolve_export(
        module: *mut JSModule,
        export_name: *const u16,
        export_name_length: usize,
        out: *mut JSResolvedBinding,
    ) {
        hook_called("resolve_export", module);
        // SAFETY: The runtime passes the code units of the name and an out record for the duration of the call.
        let (export_name, out) = unsafe { (code_units_of(export_name, export_name_length), &mut *out) };
        assert_eq!(out.r#type, JS_RESOLVED_BINDING_NULL);
        assert!(out.module.is_null());
        if EXPORTED_NAMES
            .iter()
            .any(|name| name.encode_utf16().eq(export_name.iter().copied()))
        {
            out.r#type = JS_RESOLVED_BINDING_BINDING_NAME;
            out.module = module;
            // SAFETY: The runtime passes a live sink.
            unsafe { append_to_sink(&out.binding_name, export_name) };
        }
    }

    /// Creates the module's environment through the ABI, as LibWeb does for a WebAssembly module record.
    unsafe extern "C" fn exporting_module_initialize_environment(module: *mut JSModule) -> JSCompletion {
        hook_called("initialize_environment", module);
        if ENVIRONMENT_INITIALIZATION_THROWS.get() {
            return throw_from_hook("initialize_environment");
        }
        let vm = vm_into_abi(vm_of_hooks());
        // SAFETY: The VM and the module are live, and the views outlive the calls.
        unsafe {
            let environment = js_environment_new_module_environment(vm, core::ptr::null_mut());
            js_host_module_set_environment(module, environment);
            for name in EXPORTED_NAMES {
                let name = code_units_of_str(name);
                let completion = js_environment_create_immutable_binding(vm, environment, view_of(&name), true);
                completion_from_abi(completion).must();
            }
        }
        completion_into_abi(Ok(()))
    }

    /// Fills in the bindings. While a test asks the hooks to re-enter the VM, it first creates the module's namespace,
    /// which calls the other two hooks while this one runs.
    unsafe extern "C" fn exporting_module_execute_module(
        module: *mut JSModule,
        capability: *mut JSPromiseCapability,
    ) -> JSCompletion {
        assert!(
            capability.is_null(),
            "a module without top-level await executes without a capability"
        );
        hook_called("execute_module", module);
        if EXECUTION_THROWS.get() {
            return throw_from_hook("execute_module");
        }
        let vm = vm_of_hooks();
        if HOOKS_REENTER.get() {
            module_of_hook(module).get_module_namespace(vm);
        }
        let environment = environment_to_abi(
            module_of_hook(module)
                .environment()
                .expect("initialize_environment gave the module its environment"),
        );
        // SAFETY: The VM and the environment are live, and the views outlive the calls.
        unsafe {
            let values = [
                Value::from_i32(42),
                Value::from_string(PrimitiveString::create_from_utf8(vm, "value")),
            ];
            for (name, value) in EXPORTED_NAMES.into_iter().zip(values) {
                let name = code_units_of_str(name);
                let completion = js_environment_initialize_binding(
                    vm_into_abi(vm),
                    environment,
                    view_of(&name),
                    value.0,
                    JS_INITIALIZE_BINDING_HINT_NORMAL,
                );
                completion_from_abi(completion).must();
            }
        }
        completion_into_abi(Ok(()))
    }

    static EXPORTING_MODULE_HOOKS: JSHostModuleHooks = JSHostModuleHooks {
        get_exported_names: Some(exporting_module_get_exported_names),
        resolve_export: Some(exporting_module_resolve_export),
        initialize_environment: Some(exporting_module_initialize_environment),
        execute_module: Some(exporting_module_execute_module),
    };

    /// A host class in static data, as embedders define them.
    pub(crate) struct StaticHostClass(pub(crate) JSHostClass);

    // SAFETY: The class only points to static data, which nothing changes.
    unsafe impl Sync for StaticHostClass {}

    const fn exporting_module_class(
        name: &'static core::ffi::CStr,
        flags: u32,
        parent: *const JSHostClass,
    ) -> StaticHostClass {
        StaticHostClass(JSHostClass {
            abi_version: JS_HOST_ABI_VERSION,
            kind: JS_HOST_CLASS_MODULE,
            reserved: 0,
            flags,
            name: name.as_ptr(),
            name_length: name.count_bytes(),
            parent,
            hooks: (&raw const EXPORTING_MODULE_HOOKS).cast(),
            user_data: core::ptr::null(),
        })
    }

    /// A class of modules that export "answer", 42, and "名前", "value", like a WebAssembly module record.
    pub(crate) static EXPORTING_MODULE_CLASS: StaticHostClass =
        exporting_module_class(c"ExportingModule", 0, core::ptr::null());
    static MODULE_CLASS_WITH_ITS_OWN_ALLOCATOR: StaticHostClass =
        exporting_module_class(c"ModuleWithItsOwnAllocator", 0, &raw const EXPORTING_MODULE_CLASS.0);
    static MODULE_CLASS_SHARING_ITS_PARENTS_ALLOCATOR: StaticHostClass = exporting_module_class(
        c"ModuleSharingItsParentsAllocator",
        JS_HOST_CLASS_SHARES_ALLOCATOR_WITH_PARENT,
        &raw const MODULE_CLASS_WITH_ITS_OWN_ALLOCATOR.0,
    );

    fn create_exporting_module(vm: &Vm, realm: Gc<Realm>, host_class: &'static StaticHostClass) -> Gc<HostModule> {
        HostModule::create(
            vm,
            realm,
            &host_class.0,
            "exporting.wasm".to_string(),
            Vec::new(),
            ForeignCellSlot::empty(),
            ForeignCellSlot::empty(),
        )
    }

    fn fly_string(text: &str) -> Utf16FlyString {
        Utf16FlyString::from_utf8(text)
    }

    fn define_global(vm: &Vm, realm: Gc<Realm>, name: &str, value: Gc<Object>) {
        realm
            .global_object()
            .create_data_property_or_throw(vm, &PropertyKey::from_utf8(name), Value::from_object(value))
            .must();
    }

    fn evaluate(vm: &Vm, realm: Gc<Realm>, source: &str) -> String {
        utf8(run_script(vm, realm, source).must())
    }

    fn message_of(vm: &Vm, error: Value) -> String {
        utf8(error.as_object().get(vm, &PropertyKey::from_utf8("message")).must())
    }

    /// Loads, links and evaluates the module, and returns the promise of its evaluation.
    fn link_and_evaluate(vm: &Vm, module: Gc<HostModule>) -> ThrowCompletionOr<Gc<Promise>> {
        let module = module.upcast::<Module>();
        let loading = module.load_requested_modules(vm, ForeignCellSlot::empty());
        assert_eq!(promise_of(loading).state(), PromiseState::Fulfilled);
        module.link(vm)?;
        Ok(promise_of(module.evaluate(vm).must()))
    }

    #[test]
    fn a_host_module_exports_the_bindings_its_hooks_create() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        vm.heap().set_should_collect_on_every_allocation(true);
        let module = create_exporting_module(vm, realm, &EXPORTING_MODULE_CLASS);

        assert_eq!(class_of(module).class_name(), "ExportingModule");
        assert_eq!(class_of(module).id, ClassId::HostModule);
        assert!(core::ptr::eq(
            host_class_of(&module).expect("a host module has a host class"),
            &raw const EXPORTING_MODULE_CLASS.0
        ));
        assert!(module.get_exported_names() == [fly_string("answer"), fly_string("名前")]);
        let binding = module.resolve_export(&fly_string("名前"));
        assert_eq!(binding.binding_type, ResolvedBindingType::BindingName);
        assert_eq!(binding.module, Some(module.upcast::<Module>()));
        assert!(binding.export_name == fly_string("名前"));
        assert_eq!(
            module.resolve_export(&fly_string("missing")).binding_type,
            ResolvedBindingType::Null
        );

        let evaluation = link_and_evaluate(vm, module).must();
        assert_eq!(evaluation.state(), PromiseState::Fulfilled);
        assert_eq!(module.status(), ModuleStatus::Evaluated);
        define_global(vm, realm, "namespaceObject", module.get_module_namespace(vm));
        vm.heap().set_should_collect_on_every_allocation(false);
        assert_eq!(
            evaluate(
                vm,
                realm,
                "Object.keys(namespaceObject).join() + ' ' + namespaceObject.answer + ' ' + namespaceObject['名前']"
            ),
            "answer,名前 42 value"
        );
        assert_eq!(hook_calls_named("initialize_environment"), 1);
        assert_eq!(hook_calls_named("execute_module"), 1);
    }

    #[test]
    fn hooks_may_reenter_the_vm_and_call_back_into_their_module() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let module = create_exporting_module(vm, realm, &EXPORTING_MODULE_CLASS);

        HOOKS_REENTER.set(true);
        let evaluation = link_and_evaluate(vm, module).must();
        let namespace = module.get_module_namespace(vm);
        HOOKS_REENTER.set(false);
        assert_eq!(evaluation.state(), PromiseState::Fulfilled);

        // execute_module created the namespace, which called the other two hooks while it ran.
        let calls = HOOK_CALLS.take();
        let execution = calls
            .iter()
            .position(|call| *call == "execute_module")
            .expect("the module was executed");
        assert!(calls[execution + 1..].contains(&"get_exported_names"));
        assert!(calls[execution + 1..].contains(&"resolve_export"));
        define_global(vm, realm, "namespaceObject", namespace);
        assert_eq!(
            evaluate(vm, realm, "namespaceObject.answer + ' ' + globalThis.reentries"),
            format!("42 {}", calls.len())
        );
    }

    #[test]
    fn throwing_hooks_fail_linking_and_reject_evaluation() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();

        let unlinkable = create_exporting_module(vm, realm, &EXPORTING_MODULE_CLASS);
        ENVIRONMENT_INITIALIZATION_THROWS.set(true);
        let link_error = link_and_evaluate(vm, unlinkable).expect_err("linking throws");
        ENVIRONMENT_INITIALIZATION_THROWS.set(false);
        assert_eq!(message_of(vm, link_error.value()), "initialize_environment threw");
        assert_eq!(unlinkable.status(), ModuleStatus::Unlinked);

        let failing = create_exporting_module(vm, realm, &EXPORTING_MODULE_CLASS);
        EXECUTION_THROWS.set(true);
        let evaluation = link_and_evaluate(vm, failing).must();
        EXECUTION_THROWS.set(false);
        assert_eq!(evaluation.state(), PromiseState::Rejected);
        assert_eq!(message_of(vm, evaluation.result()), "execute_module threw");
        let evaluated_again = promise_of(failing.upcast::<Module>().evaluate(vm).must());
        assert!(evaluated_again.result() == evaluation.result());
        assert_eq!(hook_calls_named("execute_module"), 1);
    }

    /// The address of the type info of the allocator the module's cell came from.
    fn allocator_type_info_address(module: Gc<HostModule>) -> usize {
        // SAFETY: The module is a live cell.
        unsafe { gc_cell_type_info(module.as_ptr().cast()) }.addr()
    }

    #[test]
    fn host_classes_get_classes_of_their_own_and_allocators_of_their_own_unless_they_share_their_parents() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let test_realm = TestRealm::new(vm);
        let exporting = create_exporting_module(vm, test_realm.realm, &EXPORTING_MODULE_CLASS);
        let with_its_own_allocator =
            create_exporting_module(vm, test_realm.realm, &MODULE_CLASS_WITH_ITS_OWN_ALLOCATOR);
        let sharing_its_parents_allocator =
            create_exporting_module(vm, test_realm.realm, &MODULE_CLASS_SHARING_ITS_PARENTS_ALLOCATOR);

        assert_eq!(class_of(exporting).class_name(), "ExportingModule");
        assert_eq!(
            class_of(with_its_own_allocator).class_name(),
            "ModuleWithItsOwnAllocator"
        );
        assert_eq!(
            class_of(sharing_its_parents_allocator).class_name(),
            "ModuleSharingItsParentsAllocator"
        );
        assert!(class_of(sharing_its_parents_allocator).is_subclass_of(class_of(with_its_own_allocator)));
        assert!(class_of(with_its_own_allocator).is_subclass_of(class_of(exporting)));
        for module in [exporting, with_its_own_allocator, sharing_its_parents_allocator] {
            assert!(class_of(module).is_subclass_of(HostModule::CLASS));
            assert!(class_of(module).is_subclass_of(CyclicModule::CLASS));
            assert_eq!(class_of(module).id, ClassId::HostModule);
        }

        assert_ne!(
            allocator_type_info_address(exporting),
            allocator_type_info_address(with_its_own_allocator)
        );
        assert_eq!(
            allocator_type_info_address(sharing_its_parents_allocator),
            allocator_type_info_address(with_its_own_allocator)
        );
        assert_eq!(
            allocator_type_info_address(with_its_own_allocator),
            core::ptr::from_ref(&class_of(with_its_own_allocator).type_info).addr()
        );
        assert!(core::ptr::eq(
            host_class_of(&sharing_its_parents_allocator).expect("a host module has a host class"),
            &raw const MODULE_CLASS_SHARING_ITS_PARENTS_ALLOCATOR.0
        ));
        let another_exporting = create_exporting_module(vm, test_realm.realm, &EXPORTING_MODULE_CLASS);
        assert!(core::ptr::eq(class_of(another_exporting), class_of(exporting)));
        assert_eq!(
            allocator_type_info_address(another_exporting),
            allocator_type_info_address(exporting)
        );
    }

    #[test]
    fn a_host_module_keeps_its_host_defined_cell_and_host_data_alive() {
        let test_vm = TestVm::create();
        let vm = &test_vm.0;
        let test_realm = TestRealm::new(vm);
        let create_holding = |host_defined, host_data| {
            HostModule::create(
                vm,
                test_realm.realm,
                &EXPORTING_MODULE_CLASS.0,
                String::new(),
                Vec::new(),
                host_defined,
                host_data,
            )
        };
        check_that_host_defined_slots_keep_their_cells_alive(
            vm,
            &test_realm,
            |host_defined| create_holding(host_defined, ForeignCellSlot::empty()),
            |module| module.host_defined(),
        );
        check_that_host_defined_slots_keep_their_cells_alive(
            vm,
            &test_realm,
            |host_data| create_holding(ForeignCellSlot::empty(), host_data),
            |module| module.host_data().get(),
        );

        let module = cell_into_abi::<JSModule>(
            create_holding(ForeignCellSlot::empty(), slot_holding(test_realm.object())).upcast(),
        );
        let replacement = test_realm.object();
        // SAFETY: The module and the object are live.
        unsafe { js_host_module_set_host_data(module, replacement.as_ptr().cast()) };
        // SAFETY: The module is live.
        let host_data = unsafe { js_host_module_host_data(module) };
        assert_eq!(NonNull::new(host_data), Some(replacement.as_non_null().cast()));
    }
}
