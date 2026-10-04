/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Execution contexts, the execution context stack and the interpreter stack.
//!
//! A JSExecutionContext is the runtime's ExecutionContext. LibJS/Embedding/Layout.h gives the offset and size of each
//! of its fields as JS_LAYOUT_EXECUTION_CONTEXT_*, and its value slots, the arguments last, follow it directly. An
//! embedder reads and writes the fields there, among them the realm, the ScriptOrModule, the this value and the
//! skip-when-determining-incumbent counter. It also finds the running context at
//! JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET in the VM, and with it the current realm and the arguments and this
//! value of the running code, without a call.
//!
//! Every function here must be called on the thread that runs the VM.

use core::ffi::c_void;
use core::ptr::NonNull;

use crate::bytecode::executable::Executable;
use crate::embedding::abi_types::{JSOwnedUtf16String, completion_into_abi, owned_utf16_string_into_abi, vm_from_abi};
use crate::gc::capi::GCVisitor;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::execution_context::OwnedExecutionContext;
use crate::layout::cell::Gc;
use crate::layout::execution_context::{
    ExecutionContext, SCRIPT_OR_MODULE_TAG_EMPTY, SCRIPT_OR_MODULE_TAG_MODULE, SCRIPT_OR_MODULE_TAG_SCRIPT,
    ScriptOrModule,
};
use crate::layout::host_class::{JSCompletion, JSVM};
use crate::runtime::module::Module;
use crate::script::Script;

/// An execution context, laid out as the JS_LAYOUT_EXECUTION_CONTEXT_* values of LibJS/Embedding/Layout.h describe.
pub struct JSExecutionContext {
    _opaque: [u8; 0],
}

/// A ScriptOrModule, laid out like the field of an execution context. The tag is one of the
/// JS_LAYOUT_SCRIPT_OR_MODULE_TAG_* values of LibJS/Embedding/Layout.h, and the cell is the Script or the Module, or
/// null for an empty one.
#[repr(C)]
pub struct JSScriptOrModule {
    pub tag: u8,
    pub cell: *mut c_void,
}

const _: () = assert!(size_of::<JSScriptOrModule>() == size_of::<ScriptOrModule>());
const _: () = assert!(align_of::<JSScriptOrModule>() == align_of::<ScriptOrModule>());

impl From<ScriptOrModule> for JSScriptOrModule {
    fn from(script_or_module: ScriptOrModule) -> Self {
        let (tag, cell) = match script_or_module {
            ScriptOrModule::Empty => (SCRIPT_OR_MODULE_TAG_EMPTY, core::ptr::null_mut()),
            ScriptOrModule::Script(script) => (SCRIPT_OR_MODULE_TAG_SCRIPT, script.as_ptr().cast()),
            ScriptOrModule::Module(module) => (SCRIPT_OR_MODULE_TAG_MODULE, module.as_ptr().cast()),
        };
        Self { tag, cell }
    }
}

/// # Safety
///
/// The cell of `script_or_module` must be a live Script or Module, as its tag says, unless the tag is the empty one.
pub unsafe fn script_or_module_from_abi(script_or_module: JSScriptOrModule) -> ScriptOrModule {
    let cell = || NonNull::new(script_or_module.cell).expect("a script or module has its cell");
    // SAFETY: The caller guarantees that the cell is a live record of the kind the tag names.
    unsafe {
        match script_or_module.tag {
            SCRIPT_OR_MODULE_TAG_EMPTY => ScriptOrModule::Empty,
            SCRIPT_OR_MODULE_TAG_SCRIPT => ScriptOrModule::Script(Gc::from_non_null(cell().cast::<Script>())),
            SCRIPT_OR_MODULE_TAG_MODULE => ScriptOrModule::Module(Gc::from_non_null(cell().cast::<Module>())),
            tag => panic!("{tag} is not the tag of a script or module"),
        }
    }
}

/// Decides whether an execution context is the one js_execution_context_last_matching() looks for.
pub type JSExecutionContextPredicate =
    Option<unsafe extern "C" fn(predicate_context: *mut c_void, execution_context: *mut JSExecutionContext) -> bool>;

fn execution_context_from_abi(execution_context: *mut JSExecutionContext) -> NonNull<ExecutionContext> {
    NonNull::new(execution_context.cast()).expect("the embedder passes an execution context")
}

fn execution_context_to_abi(execution_context: Option<NonNull<ExecutionContext>>) -> *mut JSExecutionContext {
    execution_context.map_or(core::ptr::null_mut(), |execution_context| {
        execution_context.as_ptr().cast()
    })
}

/// ExecutionContext::create(): a new execution context with room for `registers_and_locals_count` registers and locals,
/// `constant_count` constants and `argument_count` arguments. The caller owns it and frees it with
/// js_execution_context_destroy(). Its argument slots are uninitialized, and the caller fills them before the context
/// is pushed or visited. The garbage collector only sees what the context holds while the context is on an execution
/// context stack, or when its owner visits it with js_execution_context_visit().
///
/// # Safety
///
/// Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_create(
    registers_and_locals_count: u32,
    constant_count: u32,
    argument_count: u32,
) -> *mut JSExecutionContext {
    let execution_context = OwnedExecutionContext::create(registers_and_locals_count, constant_count, argument_count);
    execution_context_to_abi(Some(execution_context.into_raw()))
}

/// ExecutionContext::copy(): a new execution context with the fields, the slot counts and the live slots of
/// `execution_context`, which the caller owns and frees with js_execution_context_destroy().
///
/// # Safety
///
/// `execution_context` must be a live execution context. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_copy(
    execution_context: *const JSExecutionContext,
) -> *mut JSExecutionContext {
    let execution_context = execution_context_from_abi(execution_context.cast_mut());
    // SAFETY: The caller passes a live execution context.
    let copy = unsafe { execution_context.as_ref() }.copy();
    execution_context_to_abi(Some(copy.into_raw()))
}

/// Frees an execution context that js_execution_context_create() or js_execution_context_copy() returned.
///
/// # Safety
///
/// `execution_context` must come from one of those functions, must not be freed already, and must not be on an
/// execution context stack, a saved one included. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_destroy(execution_context: *mut JSExecutionContext) {
    // SAFETY: The caller passes a context that one of the creating functions gave up ownership of.
    drop(unsafe { OwnedExecutionContext::from_raw(execution_context_from_abi(execution_context)) });
}

/// ExecutionContext::visit_edges(): visits every cell the execution context holds, as the owner of a context that
/// js_execution_context_create() or js_execution_context_copy() returned does from its own visit_edges().
///
/// # Safety
///
/// `execution_context` must be a live execution context, and `visitor` the visitor LibGC passed to the caller. Must be
/// called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_visit(
    execution_context: *const JSExecutionContext,
    visitor: *mut GCVisitor,
) {
    let execution_context = execution_context_from_abi(execution_context.cast_mut());
    // SAFETY: The caller passes the visitor LibGC is visiting with, which outlives this call.
    let mut visitor = unsafe { Visitor::from_raw(visitor) };
    // SAFETY: The caller passes a live execution context.
    unsafe { execution_context.as_ref() }.trace(&mut visitor);
}

/// InterpreterStack::allocate(): an execution context on the interpreter stack with room for
/// `registers_and_locals_count` registers and locals, `constant_count` constants and `argument_count` arguments, or
/// null if the stack is full. The context lives until js_execution_context_interpreter_stack_deallocate() is passed a
/// mark taken before it was allocated. Its argument slots are uninitialized, and the caller fills them before the
/// context is pushed.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_interpreter_stack_allocate(
    vm: *mut JSVM,
    registers_and_locals_count: u32,
    constant_count: u32,
    argument_count: u32,
) -> *mut JSExecutionContext {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    execution_context_to_abi(vm.interpreter_stack().allocate(
        registers_and_locals_count,
        constant_count,
        argument_count,
    ))
}

/// InterpreterStack::top(): the mark to pass to js_execution_context_interpreter_stack_deallocate() to free every
/// context allocated after this call.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_interpreter_stack_top(vm: *mut JSVM) -> *mut c_void {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.interpreter_stack().top.get().cast()
}

/// InterpreterStack::deallocate(): frees every context allocated on the interpreter stack since `mark` was taken.
///
/// # Safety
///
/// `vm` must be a live VM, and `mark` a mark js_execution_context_interpreter_stack_top() returned since, with none of
/// the contexts it frees on an execution context stack. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_interpreter_stack_deallocate(vm: *mut JSVM, mark: *mut c_void) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.interpreter_stack().deallocate(mark.cast());
}

/// VM::push_execution_context(): pushes `execution_context` onto the execution context stack, which makes it the
/// running execution context. The stack does not own it.
///
/// # Safety
///
/// `vm` must be a live VM, and `execution_context` a live execution context that stays live until it is popped. Must be
/// called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_push(vm: *mut JSVM, execution_context: *mut JSExecutionContext) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.push_execution_context(execution_context_from_abi(execution_context));
}

/// VM::push_execution_context(context, CheckStackSpaceLimitTag): pushes `execution_context` like
/// js_execution_context_push(), unless so little of the native stack is left that running more JavaScript could
/// overflow it. Then it throws an InternalError instead and leaves the stack as it is.
///
/// # Safety
///
/// As for js_execution_context_push().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_push_checking_stack_space(
    vm: *mut JSVM,
    execution_context: *mut JSExecutionContext,
) -> JSCompletion {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    completion_into_abi(vm.push_execution_context_checking_stack_space(execution_context_from_abi(execution_context)))
}

/// VM::pop_execution_context(): pops the execution context stack and returns the context it popped. The context that
/// was running when that one was pushed is running again.
///
/// # Safety
///
/// `vm` must be a live VM whose execution context stack is not empty. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_pop(vm: *mut JSVM) -> *mut JSExecutionContext {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    execution_context_to_abi(Some(vm.pop_execution_context()))
}

/// The running execution context, or null if nothing runs.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_running(vm: *mut JSVM) -> *mut JSExecutionContext {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    execution_context_to_abi(vm.running_execution_context())
}

/// The number of contexts on the execution context stack, VM::execution_context_stack().size(). The frames of
/// JavaScript functions that the interpreter calls directly are not on it.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_stack_size(vm: *mut JSVM) -> usize {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.execution_context_stack_size()
}

/// VM::save_execution_context_stack(): sets the execution context stack aside and leaves an empty one with nothing
/// running. The garbage collector keeps tracing the contexts on the saved stack until
/// js_execution_context_restore_stack() brings it back.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_save_stack(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.save_execution_context_stack();
}

/// VM::clear_execution_context_stack(): removes every context from the execution context stack.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_clear_stack(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.clear_execution_context_stack();
}

/// VM::restore_execution_context_stack(): replaces the execution context stack with the one the last unmatched
/// js_execution_context_save_stack() set aside.
///
/// # Safety
///
/// `vm` must be a live VM with a saved execution context stack. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_restore_stack(vm: *mut JSVM) {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.restore_execution_context_stack();
}

/// VM::last_execution_context_matching(): calls `predicate` with `predicate_context` and each execution context from
/// the running one down, the frames of JavaScript functions that the interpreter calls directly included, and returns
/// the first context it accepts, or null if it accepts none. The predicate may run JavaScript, as long as it leaves the
/// execution context stack as it found it.
///
/// # Safety
///
/// `vm` must be a live VM, and `predicate` a function that may be called with `predicate_context`. Must be called on the
/// thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_last_matching(
    vm: *mut JSVM,
    predicate: JSExecutionContextPredicate,
    predicate_context: *mut c_void,
) -> *mut JSExecutionContext {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    let predicate = predicate.expect("the embedder passes a predicate");
    execution_context_to_abi(vm.last_execution_context_matching(|execution_context| {
        // SAFETY: The caller passes a predicate that takes its context, and the execution context is live.
        unsafe { predicate(predicate_context, execution_context_to_abi(Some(execution_context))) }
    }))
}

/// GetActiveScriptOrModule(): the ScriptOrModule of the topmost execution context that has one, or an empty one.
///
/// # Safety
///
/// `vm` must be a live VM. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_get_active_script_or_module(vm: *mut JSVM) -> JSScriptOrModule {
    // SAFETY: The caller passes a live VM.
    let vm = unsafe { vm_from_abi(vm) };
    vm.get_active_script_or_module().into()
}

/// ExecutionContext::function_name(): the name of the function whose bytecode the context runs, as an owned
/// AK::Utf16String the caller adopts with AK::Utf16String::adopt_raw(). It is empty for a context that runs no
/// bytecode, such as that of a native function, and for code that is not the body of a function.
///
/// # Safety
///
/// `execution_context` must be a live execution context. Must be called on the thread that runs the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_execution_context_function_name(
    execution_context: *const JSExecutionContext,
) -> JSOwnedUtf16String {
    assert!(!execution_context.is_null(), "the embedder passes an execution context");
    // SAFETY: The caller passes a live execution context.
    let execution_context = unsafe { &*execution_context.cast::<ExecutionContext>() };
    let name = execution_context
        .executable
        .get()
        .map(|executable| Executable::from_head(executable).name())
        .unwrap_or_default();
    owned_utf16_string_into_abi(name.into())
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::ffi::c_void;

    use super::*;
    use crate::embedding::abi_types::vm_into_abi;
    use crate::gc::root::Root;
    use crate::gc::weak::GcWeak;
    use crate::interpreter::vm::Vm;
    use crate::layout::cell::Gc;
    use crate::layout::host_class::JS_COMPLETION_NORMAL;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::run_script;
    use crate::runtime::object::Object;
    use crate::runtime::realm::Realm;
    use crate::runtime::realm::test_realm::TestRealm;
    use crate::script::Script;

    const OBJECT_COUNT: u32 = 64;

    /// # Safety
    ///
    /// `execution_context` must be live for as long as the returned reference is used.
    unsafe fn fields_of<'a>(execution_context: *mut JSExecutionContext) -> &'a ExecutionContext {
        // SAFETY: The caller passes a live execution context.
        unsafe { &*execution_context.cast::<ExecutionContext>() }
    }

    /// The execution contexts that a cell of an embedder owns and visits from its visit_edges().
    struct HostOwnedExecutionContexts(Vec<*mut JSExecutionContext>);

    // SAFETY: Visits what every context holds, through the ABI as an embedder does.
    unsafe impl Trace for HostOwnedExecutionContexts {
        fn trace(&self, visitor: &mut Visitor) {
            for &execution_context in &self.0 {
                // SAFETY: The test keeps the contexts alive while they are rooted, and LibGC passed the visitor.
                unsafe { js_execution_context_visit(execution_context, visitor.as_raw()) };
            }
        }
    }

    /// A context whose arguments hold new objects, as the embedder's copy of one it created and then freed.
    #[inline(never)]
    fn copied_context_holding_new_objects(
        vm: &Vm,
        test_realm: &TestRealm,
    ) -> (*mut JSExecutionContext, Vec<GcWeak<Object>>) {
        // SAFETY: Runs on the VM's thread, and the context is live until it is destroyed below.
        unsafe {
            let original = js_execution_context_create(2, 1, OBJECT_COUNT);
            fields_of(original).realm.set(Some(test_realm.realm));
            fields_of(original).program_counter.set(7);
            let objects = fields_of(original)
                .arguments()
                .iter()
                .map(|argument| {
                    let object = test_realm.object();
                    argument.set(Value::from_object(object));
                    GcWeak::new(vm.heap(), object)
                })
                .collect();
            let copy = js_execution_context_copy(original);
            js_execution_context_destroy(original);
            (copy, objects)
        }
    }

    #[test]
    fn an_owned_context_keeps_what_it_holds_alive_while_its_owner_visits_it() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let (copy, objects) = copied_context_holding_new_objects(&vm, &test_realm);
        // SAFETY: The copy is live until it is destroyed at the end.
        let copy_fields = unsafe { fields_of(copy) };
        assert_eq!(copy_fields.argument_count.get(), OBJECT_COUNT);
        assert_eq!(
            copy_fields.registers_and_constants_and_locals_and_arguments_count.get(),
            3 + OBJECT_COUNT
        );
        assert_eq!(copy_fields.program_counter.get(), 7);
        assert!(copy_fields.realm.get() == Some(test_realm.realm));

        let owner = Root::new(&vm, HostOwnedExecutionContexts(vec![copy]));
        vm.heap().collect_garbage();
        assert!(objects.iter().all(|object| object.get().is_some()));
        for (argument, object) in copy_fields.arguments().iter().zip(&objects) {
            assert!(argument.get().as_object() == object.get().expect("the object is alive").upcast());
        }

        drop(owner);
        vm.heap().collect_garbage();
        let dead_objects = objects.iter().filter(|object| object.get().is_none()).count();
        assert!(
            dead_objects >= OBJECT_COUNT as usize / 2,
            "only {dead_objects} of the objects nothing visits any more were collected"
        );
        // SAFETY: The copy is on no stack and nothing visits it any more.
        unsafe { js_execution_context_destroy(copy) };
    }

    /// What create_a_new_javascript_realm() does: InitializeHostDefinedRealm() pushes the realm execution context,
    /// which the embedder takes off the stack and owns from then on.
    #[inline(never)]
    fn create_embedder_realm(vm: &Vm) -> (*mut JSExecutionContext, GcWeak<Realm>) {
        let realm_execution_context = Realm::initialize_host_defined_realm(vm, None, None).must();
        assert!(vm.pop_execution_context() == realm_execution_context.as_non_null());
        let realm = realm_execution_context
            .realm
            .get()
            .expect("the context has the new realm");
        (
            execution_context_to_abi(Some(realm_execution_context.into_raw())),
            GcWeak::new(vm.heap(), realm),
        )
    }

    /// The entry execution context is the most recently pushed realm execution context.
    struct EntryExecutionContextSearch {
        realm_execution_contexts: Vec<*mut JSExecutionContext>,
    }

    unsafe extern "C" fn is_realm_execution_context(
        predicate_context: *mut c_void,
        execution_context: *mut JSExecutionContext,
    ) -> bool {
        // SAFETY: The test passes its search as the predicate's context.
        let search = unsafe { &*predicate_context.cast::<EntryExecutionContextSearch>() };
        search.realm_execution_contexts.contains(&execution_context)
    }

    fn entry_execution_context(vm: &Vm, search: &EntryExecutionContextSearch) -> *mut JSExecutionContext {
        // SAFETY: The predicate takes the search as its context.
        unsafe {
            js_execution_context_last_matching(
                vm_into_abi(vm),
                Some(is_realm_execution_context),
                core::ptr::from_ref(search).cast_mut().cast(),
            )
        }
    }

    fn realm_of(execution_context: *mut JSExecutionContext) -> Gc<Realm> {
        // SAFETY: The test only passes live contexts.
        unsafe { fields_of(execution_context) }
            .realm
            .get()
            .expect("the context has a realm")
    }

    #[test]
    fn embedder_contexts_on_a_saved_stack_survive_a_script_run_and_a_collection() {
        let vm = Vm::create();
        let vm_abi = vm_into_abi(&vm);
        let (first_realm_execution_context, _) = create_embedder_realm(&vm);
        let (second_realm_execution_context, second_realm) = create_embedder_realm(&vm);
        let search = EntryExecutionContextSearch {
            realm_execution_contexts: vec![first_realm_execution_context, second_realm_execution_context],
        };

        // SAFETY: Every context stays live while it is on the stack, and the module context's mark is taken first.
        unsafe {
            js_execution_context_push(vm_abi, first_realm_execution_context);
            js_execution_context_push(vm_abi, second_realm_execution_context);
            let stack_mark = js_execution_context_interpreter_stack_top(vm_abi);
            let module_context = js_execution_context_interpreter_stack_allocate(vm_abi, 0, 0, 0);
            assert!(!module_context.is_null());
            fields_of(module_context)
                .realm
                .set(Some(realm_of(first_realm_execution_context)));
            js_execution_context_push(vm_abi, module_context);
            assert_eq!(js_execution_context_stack_size(vm_abi), 3);
            assert_eq!(entry_execution_context(&vm, &search), second_realm_execution_context);

            js_execution_context_save_stack(vm_abi);
            js_execution_context_clear_stack(vm_abi);
            assert!(js_execution_context_running(vm_abi).is_null());
            assert_eq!(js_execution_context_stack_size(vm_abi), 0);
            assert!(entry_execution_context(&vm, &search).is_null());

            js_execution_context_push(vm_abi, first_realm_execution_context);
            assert_eq!(entry_execution_context(&vm, &search), first_realm_execution_context);
            let created_while_saved = run_script(
                &vm,
                realm_of(first_realm_execution_context),
                "var garbage = []; for (let i = 0; i < 1000; i++) garbage.push({ i }); garbage.length",
            );
            assert_eq!(created_while_saved.must(), Value::from_i32(1000));
            assert_eq!(js_execution_context_pop(vm_abi), first_realm_execution_context);
            vm.heap().collect_garbage();
            assert!(second_realm.get().is_some());

            js_execution_context_restore_stack(vm_abi);
            assert_eq!(js_execution_context_running(vm_abi), module_context);
            assert_eq!(js_execution_context_stack_size(vm_abi), 3);
            assert_eq!(entry_execution_context(&vm, &search), second_realm_execution_context);
            assert_eq!(
                run_script(&vm, realm_of(second_realm_execution_context), "[1, 2, 3].length").must(),
                Value::from_i32(3)
            );

            assert_eq!(js_execution_context_pop(vm_abi), module_context);
            js_execution_context_interpreter_stack_deallocate(vm_abi, stack_mark);
            assert_eq!(js_execution_context_pop(vm_abi), second_realm_execution_context);
            assert_eq!(js_execution_context_pop(vm_abi), first_realm_execution_context);
            js_execution_context_destroy(second_realm_execution_context);
            js_execution_context_destroy(first_realm_execution_context);
        }
    }

    /// A predicate that runs JavaScript and searches the stack again before it decides.
    struct ReentrantSearch<'vm> {
        vm: &'vm Vm,
        realm: Gc<Realm>,
        wanted_execution_context: *mut JSExecutionContext,
        topmost_execution_contexts_seen_from_inside: Vec<*mut JSExecutionContext>,
    }

    unsafe extern "C" fn accepts_every_context(_: *mut c_void, _: *mut JSExecutionContext) -> bool {
        true
    }

    unsafe extern "C" fn runs_javascript_then_looks_for_the_wanted_context(
        predicate_context: *mut c_void,
        execution_context: *mut JSExecutionContext,
    ) -> bool {
        // SAFETY: The test passes its search as the predicate's context.
        let search = unsafe { &mut *predicate_context.cast::<ReentrantSearch>() };
        assert_eq!(
            run_script(search.vm, search.realm, "[1, 2].map(value => value * 2).length").must(),
            Value::from_i32(2)
        );
        // SAFETY: The predicate takes no context.
        let topmost_execution_context = unsafe {
            js_execution_context_last_matching(
                vm_into_abi(search.vm),
                Some(accepts_every_context),
                core::ptr::null_mut(),
            )
        };
        search
            .topmost_execution_contexts_seen_from_inside
            .push(topmost_execution_context);
        execution_context == search.wanted_execution_context
    }

    #[test]
    fn the_predicate_may_run_javascript_and_search_the_stack_again() {
        let vm = Vm::create();
        let vm_abi = vm_into_abi(&vm);
        let (realm_execution_context, _) = create_embedder_realm(&vm);
        let realm = realm_of(realm_execution_context);
        // SAFETY: Both contexts stay live while they are on the stack, and the predicate takes the search.
        unsafe {
            js_execution_context_push(vm_abi, realm_execution_context);
            let host_context = js_execution_context_create(0, 0, 0);
            fields_of(host_context).realm.set(Some(realm));
            js_execution_context_push(vm_abi, host_context);

            let mut search = ReentrantSearch {
                vm: &vm,
                realm,
                wanted_execution_context: realm_execution_context,
                topmost_execution_contexts_seen_from_inside: Vec::new(),
            };
            let matching_context = js_execution_context_last_matching(
                vm_abi,
                Some(runs_javascript_then_looks_for_the_wanted_context),
                (&raw mut search).cast(),
            );
            assert_eq!(matching_context, realm_execution_context);
            assert_eq!(
                search.topmost_execution_contexts_seen_from_inside,
                [host_context, host_context]
            );

            assert_eq!(js_execution_context_pop(vm_abi), host_context);
            assert_eq!(js_execution_context_pop(vm_abi), realm_execution_context);
            js_execution_context_destroy(host_context);
            js_execution_context_destroy(realm_execution_context);
        }
    }

    #[test]
    fn the_active_script_or_module_is_that_of_the_topmost_context_that_has_one() {
        let vm = Vm::create();
        let vm_abi = vm_into_abi(&vm);
        let test_realm = TestRealm::new(&vm);
        let source: Vec<u16> = "1".encode_utf16().collect();
        let script = Script::parse(&vm, &source, test_realm.realm).expect("the script parses");
        // SAFETY: Both contexts stay live while they are on the stack.
        unsafe {
            let active = js_execution_context_get_active_script_or_module(vm_abi);
            assert_eq!(active.tag, SCRIPT_OR_MODULE_TAG_EMPTY);
            assert!(active.cell.is_null());

            let script_context = js_execution_context_create(0, 0, 0);
            fields_of(script_context).realm.set(Some(test_realm.realm));
            fields_of(script_context)
                .script_or_module
                .set(ScriptOrModule::Script(script));
            let completion = js_execution_context_push_checking_stack_space(vm_abi, script_context);
            assert_eq!(completion.variant, JS_COMPLETION_NORMAL);
            let host_context = js_execution_context_create(0, 0, 0);
            js_execution_context_push(vm_abi, host_context);

            let active = js_execution_context_get_active_script_or_module(vm_abi);
            assert_eq!(active.tag, SCRIPT_OR_MODULE_TAG_SCRIPT);
            assert_eq!(active.cell, script.as_ptr().cast());

            assert_eq!(js_execution_context_pop(vm_abi), host_context);
            assert_eq!(js_execution_context_pop(vm_abi), script_context);
            js_execution_context_destroy(host_context);
            js_execution_context_destroy(script_context);
        }
    }
}
