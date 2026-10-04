/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ptr::NonNull;
use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};

use crate::gc::visitor::{Trace, Visitor};
use crate::layout::execution_context::{ExecutionContext, ScriptOrModule};
use crate::layout::value::Value;
use libjs_abi::register::RESERVED_REGISTER_COUNT;

impl ExecutionContext {
    /// Constructs a context for a frame of `slot_count` slots, the last `argument_count` of which hold arguments, at
    /// `context`.
    ///
    /// # Safety
    ///
    /// `context` must point to writable memory for the context and its slots, which nothing else uses.
    pub(crate) unsafe fn initialize_at(
        context: *mut ExecutionContext,
        registers_and_locals_count: u32,
        slot_count: u32,
        argument_count: u32,
        frame_id: u64,
    ) {
        // SAFETY: The caller passes memory for the context and its slots.
        unsafe {
            context.write(ExecutionContext {
                function: Cell::new(None),
                realm: Cell::new(None),
                script_or_module: Cell::new(ScriptOrModule::Empty),
                lexical_environment: Cell::new(None),
                variable_environment: Cell::new(None),
                private_environment: Cell::new(None),
                frame_id: Cell::new(frame_id),
                program_counter: Cell::new(0),
                skip_when_determining_incumbent_counter: Cell::new(0),
                yield_continuation: Cell::new(ExecutionContext::NO_YIELD_CONTINUATION),
                yield_is_await: Cell::new(false),
                yield_value_is_iterator_result: Cell::new(false),
                caller_is_construct: Cell::new(false),
                frame_initialized: Cell::new(false),
                this_value: Cell::new(Value::EMPTY),
                executable: Cell::new(None),
                caller_frame: Cell::new(core::ptr::null_mut()),
                passed_argument_count: Cell::new(0),
                caller_return_pc: Cell::new(0),
                caller_dst_raw: Cell::new(0),
                registers_and_constants_and_locals_and_arguments_count: Cell::new(slot_count),
                argument_count: Cell::new(argument_count),
            });
            // NB: Enter initializes the remaining registers, locals, and constants.
            for slot in (*context)
                .slots()
                .iter()
                .take(registers_and_locals_count.min(RESERVED_REGISTER_COUNT) as usize)
            {
                slot.set(Value::EMPTY);
            }
        }
    }

    /// The frame's registers, locals, constants and arguments, which follow the context directly.
    pub fn slots(&self) -> &[Cell<Value>] {
        let count = self.registers_and_constants_and_locals_and_arguments_count.get() as usize;
        // SAFETY: Contexts are only ever allocated with their slots following them.
        unsafe { core::slice::from_raw_parts(core::ptr::from_ref(self).add(1).cast::<Cell<Value>>(), count) }
    }

    pub fn register(&self, index: u32) -> &Cell<Value> {
        &self.slots()[index as usize]
    }

    pub fn arguments(&self) -> &[Cell<Value>] {
        let slots = self.slots();
        &slots[slots.len() - self.argument_count.get() as usize..]
    }

    pub fn argument(&self, index: usize) -> Value {
        self.arguments().get(index).map_or(Value::UNDEFINED, Cell::get)
    }

    /// ExecutionContext::copy(), for the generator that keeps the context of the call that created it.
    pub fn copy(&self) -> OwnedExecutionContext {
        let slot_count = self.registers_and_constants_and_locals_and_arguments_count.get();
        let argument_count = self.argument_count.get();
        // NB: We pass the entire non-argument count as registers_and_locals_count with 0 constants.
        let copy = OwnedExecutionContext::create(slot_count - argument_count, 0, argument_count);
        copy.function.set(self.function.get());
        copy.realm.set(self.realm.get());
        copy.script_or_module.set(self.script_or_module.get());
        copy.lexical_environment.set(self.lexical_environment.get());
        copy.variable_environment.set(self.variable_environment.get());
        copy.private_environment.set(self.private_environment.get());
        copy.program_counter.set(self.program_counter.get());
        copy.frame_id.set(self.frame_id.get());
        copy.yield_continuation.set(self.yield_continuation.get());
        copy.yield_is_await.set(self.yield_is_await.get());
        copy.yield_value_is_iterator_result
            .set(self.yield_value_is_iterator_result.get());
        copy.caller_is_construct.set(self.caller_is_construct.get());
        copy.frame_initialized.set(self.frame_initialized.get());
        copy.this_value.set(self.this_value.get());
        copy.executable.set(self.executable.get());
        copy.passed_argument_count.set(self.passed_argument_count.get());
        let frame_initialized = self.frame_initialized.get();
        let non_argument_count = (slot_count - argument_count) as usize;
        for (index, (copied_slot, slot)) in copy.slots().iter().zip(self.slots()).enumerate() {
            if !frame_initialized && index >= RESERVED_REGISTER_COUNT as usize && index < non_argument_count {
                continue;
            }
            copied_slot.set(slot.get());
        }
        copy
    }
}

// SAFETY: Visits every cell a frame can reach. Until the frame is initialized only its reserved registers and its
// arguments hold values; the other slots are left over from earlier frames.
unsafe impl Trace for ExecutionContext {
    fn trace(&self, visitor: &mut Visitor) {
        self.function.trace(visitor);
        self.realm.trace(visitor);
        self.variable_environment.trace(visitor);
        self.lexical_environment.trace(visitor);
        self.private_environment.trace(visitor);
        self.this_value.trace(visitor);
        self.executable.trace(visitor);
        match self.script_or_module.get() {
            ScriptOrModule::Empty => {}
            ScriptOrModule::Script(script) => script.trace(visitor),
            ScriptOrModule::Module(module) => module.trace(visitor),
        }
        let slots = self.slots();
        if self.frame_initialized.get() {
            trace_slots(slots, visitor);
        } else {
            let non_argument_count = slots.len() - self.argument_count.get() as usize;
            trace_slots(
                &slots[..non_argument_count.min(RESERVED_REGISTER_COUNT as usize)],
                visitor,
            );
            trace_slots(self.arguments(), visitor);
        }
    }
}

fn trace_slots(slots: &[Cell<Value>], visitor: &mut Visitor) {
    // SAFETY: Cell<Value> has the same layout as Value, and nothing writes the slots while they are visited.
    let values = unsafe { core::slice::from_raw_parts(slots.as_ptr().cast::<Value>(), slots.len()) };
    visitor.visit_values(values);
}

/// An execution context that lives outside the interpreter stack, like the ones C++ ExecutionContext::create()
/// allocates for realms. It must be popped off the execution context stack before it is dropped.
pub struct OwnedExecutionContext {
    context: NonNull<ExecutionContext>,
    layout: Layout,
}

impl OwnedExecutionContext {
    /// ExecutionContext::create(registers_and_locals_count, constants, arguments_count)
    pub fn create(registers_and_locals_count: u32, constant_count: u32, argument_count: u32) -> Self {
        let slot_count = registers_and_locals_count
            .checked_add(constant_count)
            .and_then(|count| count.checked_add(argument_count))
            .expect("the slot count of an execution context fits in u32");
        let layout = Layout::from_size_align(
            size_of::<ExecutionContext>() + slot_count as usize * size_of::<Value>(),
            align_of::<ExecutionContext>(),
        )
        .expect("the layout of an execution context is valid");
        // SAFETY: The layout has room for at least the context.
        let memory = unsafe { alloc(layout) }.cast::<ExecutionContext>();
        let Some(context) = NonNull::new(memory) else {
            handle_alloc_error(layout);
        };
        // SAFETY: The memory was just allocated with room for the context and its slots.
        unsafe {
            ExecutionContext::initialize_at(
                context.as_ptr(),
                registers_and_locals_count,
                slot_count,
                argument_count,
                0,
            );
        }
        Self { context, layout }
    }

    pub fn as_non_null(&self) -> NonNull<ExecutionContext> {
        self.context
    }
}

// SAFETY: Visits every cell the context reaches.
unsafe impl Trace for OwnedExecutionContext {
    fn trace(&self, visitor: &mut Visitor) {
        (**self).trace(visitor);
    }
}

impl core::ops::Deref for OwnedExecutionContext {
    type Target = ExecutionContext;

    fn deref(&self) -> &ExecutionContext {
        // SAFETY: The context lives until this is dropped.
        unsafe { self.context.as_ref() }
    }
}

impl Drop for OwnedExecutionContext {
    fn drop(&mut self) {
        // SAFETY: The context was allocated with this layout, and contexts hold nothing that needs dropping.
        unsafe { dealloc(self.context.as_ptr().cast(), self.layout) };
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::ops::ControlFlow;
    use core::ptr::NonNull;

    use super::OwnedExecutionContext;
    use crate::gc::weak::GcWeak;
    use crate::interpreter::vm::{TypeErrorRealmScope, Vm};
    use crate::layout::execution_context::ExecutionContext;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::run_script;
    use crate::runtime::object::Object;
    use crate::runtime::realm::Realm;
    use crate::runtime::realm::test_realm::TestRealm;
    use crate::utilities::initialize_realm;

    const OBJECT_COUNT: u32 = 64;
    const REALM_COUNT: usize = 16;

    fn contexts_top_to_bottom(vm: &Vm) -> Vec<NonNull<ExecutionContext>> {
        let mut contexts = Vec::new();
        vm.for_each_execution_context_top_to_bottom(|context| {
            contexts.push(NonNull::from(context));
            ControlFlow::Continue(())
        });
        contexts
    }

    #[inline(never)]
    fn context_holding_new_objects(vm: &Vm, test_realm: &TestRealm) -> (OwnedExecutionContext, Vec<GcWeak<Object>>) {
        let context = OwnedExecutionContext::create(0, 0, OBJECT_COUNT);
        context.realm.set(Some(test_realm.realm));
        let objects = context
            .arguments()
            .iter()
            .map(|argument| {
                let object = test_realm.object();
                argument.set(Value::from_object(object));
                GcWeak::new(vm.heap(), object)
            })
            .collect();
        (context, objects)
    }

    #[inline(never)]
    fn override_type_error_realm_with_new_realm(vm: &Vm) -> (TypeErrorRealmScope<'_>, GcWeak<Realm>) {
        let realm = Realm::create(vm);
        (vm.type_error_realm_scope(realm), GcWeak::new(vm.heap(), realm))
    }

    #[test]
    fn a_saved_stack_comes_back_as_it_was_after_another_one_ran_and_was_cleared() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let host_context = OwnedExecutionContext::create(0, 0, 0);
        host_context.realm.set(Some(realm));
        vm.push_execution_context(host_context.as_non_null());
        let type_error_realm = Realm::create(&vm);
        let type_error_realm_scope = vm.type_error_realm_scope(type_error_realm);
        let saved_contexts = contexts_top_to_bottom(&vm);
        assert_eq!(saved_contexts.len(), 2);

        vm.save_execution_context_stack();
        assert!(vm.running_execution_context().is_none());
        assert_eq!(vm.execution_context_stack_size(), 0);
        assert!(vm.type_error_realm().is_none());

        let nested_realm_execution_context = Realm::initialize_host_defined_realm(&vm, None, None).must();
        let nested_realm = nested_realm_execution_context
            .realm
            .get()
            .expect("the context has the new realm");
        assert_eq!(
            run_script(&vm, nested_realm, "[1, 2, 3].length").must(),
            Value::from_i32(3)
        );
        assert_eq!(vm.execution_context_stack_size(), 1);
        vm.clear_execution_context_stack();
        assert!(vm.running_execution_context().is_none());
        assert_eq!(vm.execution_context_stack_size(), 0);

        vm.restore_execution_context_stack();
        assert_eq!(contexts_top_to_bottom(&vm), saved_contexts);
        assert_eq!(vm.execution_context_stack_size(), 2);
        assert!(vm.type_error_realm() == Some(type_error_realm));
        drop(type_error_realm_scope);
        assert!(vm.pop_execution_context() == host_context.as_non_null());
        assert_eq!(run_script(&vm, realm, "1 + 1").must(), Value::from_i32(2));
    }

    #[test]
    fn a_saved_stack_keeps_what_its_contexts_hold_alive() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let (host_context, objects) = context_holding_new_objects(&vm, &test_realm);
        vm.push_execution_context(host_context.as_non_null());

        vm.save_execution_context_stack();
        vm.heap().collect_garbage();
        assert!(objects.iter().all(|object| object.get().is_some()));

        vm.restore_execution_context_stack();
        assert!(vm.pop_execution_context() == host_context.as_non_null());
        drop(host_context);
        vm.heap().collect_garbage();
        let dead_objects = objects.iter().filter(|object| object.get().is_none()).count();
        assert!(
            dead_objects >= OBJECT_COUNT as usize / 2,
            "only {dead_objects} of the objects nothing holds any more were collected"
        );
    }

    #[test]
    fn a_saved_stack_keeps_its_type_error_realm_alive() {
        let vm = Vm::create();
        let mut scopes_and_realms = Vec::new();
        for _ in 0..REALM_COUNT {
            scopes_and_realms.push(override_type_error_realm_with_new_realm(&vm));
            vm.save_execution_context_stack();
        }
        vm.heap().collect_garbage();
        assert!(scopes_and_realms.iter().all(|(_, realm)| realm.get().is_some()));

        let mut realms = Vec::new();
        for (scope, realm) in scopes_and_realms.into_iter().rev() {
            vm.restore_execution_context_stack();
            assert!(vm.type_error_realm() == realm.get());
            drop(scope);
            realms.push(realm);
        }
        vm.heap().collect_garbage();
        let dead_realms = realms.iter().filter(|realm| realm.get().is_none()).count();
        assert!(
            dead_realms >= REALM_COUNT / 2,
            "only {dead_realms} of the realms nothing holds any more were collected"
        );
    }

    #[test]
    fn the_topmost_matching_context_is_found_while_the_predicate_runs_javascript() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let realm_execution_context = vm.running_execution_context().expect("the realm's context runs");
        let host_context = OwnedExecutionContext::create(0, 0, 0);
        host_context.realm.set(Some(realm));
        vm.push_execution_context(host_context.as_non_null());

        let mut searched_contexts = Vec::new();
        let matching_context = vm.last_execution_context_matching(|context| {
            searched_contexts.push(context);
            assert_eq!(run_script(&vm, realm, "6 * 7").must(), Value::from_i32(42));
            context == realm_execution_context
        });
        assert!(matching_context == Some(realm_execution_context));
        assert_eq!(searched_contexts, [host_context.as_non_null(), realm_execution_context]);
        assert!(vm.last_execution_context_matching(|_| false).is_none());

        assert!(vm.pop_execution_context() == host_context.as_non_null());
    }
}
