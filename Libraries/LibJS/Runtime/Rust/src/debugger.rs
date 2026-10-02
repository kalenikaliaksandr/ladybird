/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Libraries/LibJS/Debugger.h and Debugger.cpp: pauses the code a VM runs at breakpoints, debugger statements,
//! exceptions and steps, so that its host can inspect the paused frames and evaluate code in them.

use core::cell::{Cell, RefCell};
use core::ops::ControlFlow;
use std::collections::HashMap;
use std::rc::Rc;

use ak::{ScopeGuard, Utf16FlyString};
use indexmap::IndexMap;
use libjs_runtime_macros::Trace;

use crate::breakpoint::{Breakpoint, BreakpointID};
use crate::bytecode::executable::{Executable, LocalVariableMetadata};
use crate::gc::heap::cell_is_dead;
use crate::gc::root::MarkedVec;
use crate::gc::visitor::{Trace, Visitor};
use crate::gc::weak::GcWeak;
use crate::interpreter::vm::{EvalMode, StackTraceElement, Vm};
use crate::layout::cell::Gc;
use crate::layout::execution_context::ExecutionContext;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::{CallerMode, new_declarative_environment, perform_eval};
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::environment::InitializeBindingHint;
use crate::runtime::primitive_string::PrimitiveString;
use crate::source_code::SourceCode;
use crate::source_range::{Position, SourceRange};
use crate::utf16::Utf16View;
use libjs_rust::bytecode::basic_block::SourceMapEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    Entry,
    Breakpoint,
    DebuggerStatement,
    Exception,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseOnExceptions {
    None,
    All,
    Uncaught,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeMode {
    Continue,
    StepInto,
    StepOut,
    StepOver,
}

/// What the debugger tells its pause callback about where execution paused. It only lives on the stack while the
/// callback runs, where the collector finds the cells it holds.
pub struct PauseInfo {
    pub executable: Gc<Executable>,
    pub bytecode_offset: u32,
    pub source_range: Option<SourceRange>,
    /// The frames from the paused one down, the first of which is where execution paused.
    pub stack_trace: Vec<StackTraceElement>,
    pub breakpoint_ids: Vec<BreakpointID>,
    pub exception: Option<Value>,
    pub exception_will_be_caught: bool,
    pub reason: PauseReason,
}

#[derive(Clone, Trace)]
pub struct FrameBinding {
    pub name: Utf16FlyString,
    pub value: Value,
    pub is_mutable: bool,
}

/// The callback must call continue_execution() before it returns. It may evaluate JavaScript, in which case any pause
/// triggered by that evaluation is ignored.
pub type PauseCallback = Rc<dyn Fn(&Vm, &PauseInfo)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingStorage {
    Argument,
    Local,
}

#[derive(Clone, Copy)]
struct BindingLocation {
    storage: BindingStorage,
    index: usize,
    is_mutable: bool,
    scope_start: Option<Position>,
}

struct StepState {
    mode: ResumeMode,
    frame_id: u64,
    source_range: Option<SourceRange>,
}

pub struct Debugger {
    pause_callback: RefCell<Option<PauseCallback>>,
    /// GC::WeakHashSet<Executable>, by the address of each executable, which a new one may take over once the old
    /// one died. The VM forgets the ones that die after each collection.
    executables: RefCell<HashMap<usize, GcWeak<Executable>, foldhash::fast::RandomState>>,
    breakpoints: RefCell<Vec<Breakpoint>>,
    next_breakpoint_id: Cell<BreakpointID>,
    step_state: RefCell<Option<StepState>>,
    pause_on_exceptions: Cell<PauseOnExceptions>,
    /// The VM keeps it alive, as C++ keeps it in a GC::Root.
    last_paused_exception: Cell<Option<Value>>,
    paused_execution_context: Cell<*const ExecutionContext>,
    paused_source_range: RefCell<Option<SourceRange>>,
    is_paused: Cell<bool>,
    pause_on_next_bytecode_execution: Cell<bool>,
    /// Set before each instruction is executed, so that a `debugger` statement doesn't pause a second time when we've
    /// already paused at a breakpoint on that same instruction.
    did_pause_before_current_instruction: Cell<bool>,
}

impl Default for Debugger {
    fn default() -> Self {
        Self::new()
    }
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            pause_callback: RefCell::new(None),
            executables: RefCell::new(HashMap::default()),
            breakpoints: RefCell::new(Vec::new()),
            next_breakpoint_id: Cell::new(1),
            step_state: RefCell::new(None),
            pause_on_exceptions: Cell::new(PauseOnExceptions::None),
            last_paused_exception: Cell::new(None),
            paused_execution_context: Cell::new(core::ptr::null()),
            paused_source_range: RefCell::new(None),
            is_paused: Cell::new(false),
            pause_on_next_bytecode_execution: Cell::new(false),
            did_pause_before_current_instruction: Cell::new(false),
        }
    }

    pub fn set_pause_callback(&self, callback: impl Fn(&Vm, &PauseInfo) + 'static) {
        *self.pause_callback.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn pause_execution(
        &self,
        vm: &Vm,
        executable: Gc<Executable>,
        bytecode_offset: u32,
        reason: PauseReason,
        exception: Option<Value>,
        exception_will_be_caught: bool,
    ) -> bool {
        // The pause callback is free to evaluate JavaScript, which may pause again. Only the outermost
        // pause is reported; nested ones are ignored so that the callback can't deadlock against itself.
        if self.is_paused.get() {
            return false;
        }

        let Some(pause_callback) = self.pause_callback.borrow().clone() else {
            return false;
        };

        if reason == PauseReason::Entry {
            self.pause_on_next_bytecode_execution.set(false);
        }
        self.is_paused.set(true);

        let mut stack_trace = vm.stack_trace();
        assert!(!stack_trace.is_empty());
        let source_range = executable.source_range_at(bytecode_offset);
        stack_trace[0].source_range = source_range.clone();
        self.paused_execution_context
            .set(stack_trace[0].execution_context.as_ptr().cast_const());
        *self.paused_source_range.borrow_mut() = source_range.clone();
        pause_callback(
            vm,
            &PauseInfo {
                executable,
                bytecode_offset,
                source_range,
                stack_trace,
                breakpoint_ids: executable.debugger_breakpoints_at(bytecode_offset),
                exception,
                exception_will_be_caught,
                reason,
            },
        );
        assert!(!self.is_paused.get(), "the pause callback continues execution");
        self.paused_execution_context.set(core::ptr::null());
        *self.paused_source_range.borrow_mut() = None;
        true
    }

    /// Continue after the host filters out a pause without cancelling an active step operation.
    pub fn continue_execution_preserving_step_state(&self) {
        assert!(self.is_paused.get());
        self.is_paused.set(false);
    }

    pub fn continue_execution(&self, mode: ResumeMode) {
        assert!(self.is_paused.get());
        if mode == ResumeMode::Continue {
            *self.step_state.borrow_mut() = None;
        } else {
            let paused_execution_context = self.paused_execution_context.get();
            assert!(!paused_execution_context.is_null());
            // SAFETY: The paused context is the running one, which stays live until execution continues.
            let frame_id = unsafe { &*paused_execution_context }.frame_id.get();
            *self.step_state.borrow_mut() = Some(StepState {
                mode,
                frame_id,
                source_range: self.paused_source_range.borrow().clone(),
            });
        }
        self.is_paused.set(false);
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused.get()
    }

    pub fn evaluate_in_frame(
        &self,
        vm: &Vm,
        execution_context: &ExecutionContext,
        source_text: Utf16View<'_>,
    ) -> ThrowCompletionOr<Value> {
        assert!(self.is_paused.get());
        let executable = Executable::from_head(
            execution_context
                .executable
                .get()
                .expect("a paused frame runs an executable"),
        );

        let context = execution_context.copy();
        let local_environment = new_declarative_environment(
            vm,
            context
                .lexical_environment
                .get()
                .expect("a paused frame has a lexical environment"),
        );

        let binding_locations = self.binding_locations_for_frame(execution_context);

        for (name, location) in &binding_locations {
            if location.is_mutable {
                local_environment.create_mutable_binding(vm, name, false)?;
            } else {
                local_environment.create_immutable_binding(vm, name, true)?;
            }
            let value = match location.storage {
                BindingStorage::Local => local_variables(&context, &executable)[location.index].get(),
                BindingStorage::Argument => context.argument(location.index),
            };
            if !value.is_empty() {
                local_environment.initialize_binding(vm, name, value, InitializeBindingHint::Normal)?;
            }
        }
        context.lexical_environment.set(Some(local_environment.upcast()));
        context.variable_environment.set(Some(local_environment.upcast()));
        vm.push_execution_context_checking_stack_space(context.as_non_null())?;
        let _pop_context = ScopeGuard::new(|| {
            vm.pop_execution_context();
        });

        let strict_caller = if executable.is_strict_mode {
            CallerMode::Strict
        } else {
            CallerMode::NonStrict
        };
        let result = perform_eval(
            vm,
            Value::from_string(PrimitiveString::create(vm, source_text.to_utf16_string())),
            strict_caller,
            EvalMode::Direct,
        );

        for (name, location) in &binding_locations {
            if !location.is_mutable {
                continue;
            }
            let Ok(value) = local_environment.get_binding_value(vm, name, false) else {
                continue;
            };
            let slot = match location.storage {
                BindingStorage::Local => local_variables(execution_context, &executable).get(location.index),
                BindingStorage::Argument => execution_context.arguments().get(location.index),
            };
            if let Some(slot) = slot {
                slot.set(value);
            }
        }
        result
    }

    fn binding_locations_for_frame(&self, context: &ExecutionContext) -> IndexMap<Utf16FlyString, BindingLocation> {
        let executable = Executable::from_head(context.executable.get().expect("a frame runs an executable"));

        let mut binding_locations = IndexMap::new();
        for (index, name) in executable.argument_variable_names.iter().enumerate() {
            if !name.is_empty() {
                binding_locations.insert(
                    name.clone(),
                    BindingLocation {
                        storage: BindingStorage::Argument,
                        index,
                        is_mutable: true,
                        scope_start: None,
                    },
                );
            }
        }

        let paused_source_range = if core::ptr::eq(self.paused_execution_context.get(), context) {
            self.paused_source_range.borrow().clone()
        } else {
            executable.source_range_at(context.program_counter.get())
        };
        let scope_is_active = |metadata: &LocalVariableMetadata| {
            let (Some(scope_range), Some(paused_source_range)) = (metadata.scope_range, &paused_source_range) else {
                return true;
            };
            let position = paused_source_range.start;
            scope_range.start <= position && position < scope_range.end
        };

        for (index, name) in executable.local_variable_names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            let metadata = executable.local_variable_metadata[index];
            if !scope_is_active(&metadata) {
                continue;
            }

            if let Some(existing) = binding_locations.get(name)
                && existing.storage == BindingStorage::Local
            {
                let Some(scope_range) = metadata.scope_range else {
                    continue;
                };
                if existing
                    .scope_start
                    .is_some_and(|scope_start| scope_start >= scope_range.start)
                {
                    continue;
                }
            }
            binding_locations.insert(
                name.clone(),
                BindingLocation {
                    storage: BindingStorage::Local,
                    index,
                    is_mutable: metadata.is_mutable,
                    scope_start: metadata.scope_range.map(|range| range.start),
                },
            );
        }

        binding_locations
    }

    pub fn bindings_for_frame<'vm>(&self, vm: &'vm Vm, context: &ExecutionContext) -> MarkedVec<'vm, FrameBinding> {
        let executable = Executable::from_head(context.executable.get().expect("a frame runs an executable"));
        let binding_locations = self.binding_locations_for_frame(context);
        let bindings = MarkedVec::with_capacity(vm, binding_locations.len());
        for (name, location) in binding_locations {
            let value = match location.storage {
                BindingStorage::Local => local_variables(context, &executable)[location.index].get(),
                BindingStorage::Argument => context.argument(location.index),
            };
            bindings.push(FrameBinding {
                name,
                value,
                is_mutable: location.is_mutable,
            });
        }
        bindings
    }

    /// Set before each instruction is executed, so that a `debugger` statement doesn't pause a second time when we've
    /// already paused at a breakpoint on that same instruction.
    pub fn set_did_pause_before_current_instruction(&self, value: bool) {
        self.did_pause_before_current_instruction.set(value);
    }

    pub fn did_pause_before_current_instruction(&self) -> bool {
        self.did_pause_before_current_instruction.get()
    }

    pub fn request_pause_on_next_bytecode_execution(&self) {
        self.pause_on_next_bytecode_execution.set(true);
    }

    pub fn should_pause_for_step(&self, vm: &Vm, executable: &Executable, bytecode_offset: u32) -> bool {
        let step_state = self.step_state.borrow();
        let Some(state) = step_state.as_ref() else {
            return false;
        };

        let Some(source_range) = executable.source_range_at(bytecode_offset) else {
            return false;
        };
        if source_range.start.line == 0 {
            return false;
        }

        let current_frame_id = vm.running_execution_context_ref().frame_id.get();

        let mut start_context_is_active = false;
        vm.for_each_execution_context_top_to_bottom(|context| {
            if context.frame_id.get() == state.frame_id {
                start_context_is_active = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });

        let has_moved = || {
            let Some(start_source_range) = &state.source_range else {
                return true;
            };
            !Rc::ptr_eq(&source_range.code, &start_source_range.code)
                || source_range.start.line != start_source_range.start.line
        };

        match state.mode {
            ResumeMode::Continue => unreachable!("continuing clears the step state"),
            ResumeMode::StepInto => current_frame_id != state.frame_id || has_moved(),
            ResumeMode::StepOver => {
                if current_frame_id == state.frame_id {
                    return has_moved();
                }
                !start_context_is_active
            }
            ResumeMode::StepOut => !start_context_is_active,
        }
    }

    pub fn pause_on_exception_if_needed(
        &self,
        vm: &Vm,
        executable: Gc<Executable>,
        bytecode_offset: u32,
        exception: Value,
        exception_will_be_caught: bool,
    ) -> bool {
        if self.pause_on_exceptions.get() == PauseOnExceptions::None {
            return false;
        }
        if self.pause_on_exceptions.get() == PauseOnExceptions::Uncaught && exception_will_be_caught {
            return false;
        }
        if self.last_paused_exception.get() == Some(exception) {
            return false;
        }

        if !self.pause_execution(
            vm,
            executable,
            bytecode_offset,
            PauseReason::Exception,
            Some(exception),
            exception_will_be_caught,
        ) {
            return false;
        }

        self.last_paused_exception.set(Some(exception));
        true
    }

    pub fn did_finish_exception_propagation(&self, exception: Value) {
        if self.last_paused_exception.get() == Some(exception) {
            self.last_paused_exception.set(None);
        }
    }

    pub fn did_finish_bytecode_execution(&self) {
        *self.step_state.borrow_mut() = None;
        self.last_paused_exception.set(None);
    }

    pub fn set_pause_on_exceptions(&self, mode: PauseOnExceptions) {
        self.pause_on_exceptions.set(mode);
    }

    pub fn add_breakpoint(
        &self,
        filename: Utf16View<'_>,
        line: u32,
        column: Option<u32>,
    ) -> Result<BreakpointID, &'static str> {
        self.add_breakpoint_impl(None, filename, line, column)
    }

    pub fn add_breakpoint_for_source_code(
        &self,
        source_code: Rc<SourceCode>,
        line: u32,
        column: Option<u32>,
    ) -> Result<BreakpointID, &'static str> {
        let filename = source_code.filename().clone();
        self.add_breakpoint_impl(Some(source_code), Utf16View::of_string(&filename), line, column)
    }

    fn add_breakpoint_impl(
        &self,
        source_code: Option<Rc<SourceCode>>,
        filename: Utf16View<'_>,
        line: u32,
        column: Option<u32>,
    ) -> Result<BreakpointID, &'static str> {
        if line == 0 {
            return Err("Breakpoint line must be greater than zero");
        }

        for breakpoint in self.breakpoints.borrow().iter() {
            if is_same_source_code(breakpoint.source_code.as_ref(), source_code.as_ref())
                && Utf16View::of_string(&breakpoint.filename) == filename
                && breakpoint.line == line
                && breakpoint.column == column
            {
                return Ok(breakpoint.id);
            }
        }

        if self.next_breakpoint_id.get() == BreakpointID::MAX {
            return Err("Too many breakpoints");
        }

        let breakpoint = Breakpoint {
            id: self.next_breakpoint_id.get(),
            source_code,
            filename: filename.to_utf16_string(),
            line,
            column,
        };
        self.next_breakpoint_id.set(breakpoint.id + 1);
        self.breakpoints.borrow_mut().push(breakpoint.clone());
        self.resolve_breakpoint(&breakpoint);
        Ok(breakpoint.id)
    }

    pub fn remove_breakpoint(&self, breakpoint_id: BreakpointID) -> bool {
        let mut breakpoints = self.breakpoints.borrow_mut();
        let Some(index) = breakpoints.iter().position(|breakpoint| breakpoint.id == breakpoint_id) else {
            return false;
        };
        breakpoints.remove(index);
        drop(breakpoints);

        for executable in self.executables.borrow().values().filter_map(GcWeak::get) {
            executable.remove_debugger_breakpoint(breakpoint_id);
        }
        true
    }

    pub fn breakpoints(&self) -> Vec<Breakpoint> {
        self.breakpoints.borrow().clone()
    }

    pub fn is_breakpoint_resolved(&self, breakpoint_id: BreakpointID) -> bool {
        self.executables
            .borrow()
            .values()
            .filter_map(GcWeak::get)
            .any(|executable| executable.has_debugger_breakpoint(breakpoint_id))
    }

    pub fn register_executable(&self, vm: &Vm, executable: Gc<Executable>) {
        let key = executable.as_ptr().addr();
        if self.executables.borrow().get(&key).and_then(GcWeak::get) == Some(executable) {
            return;
        }

        self.executables
            .borrow_mut()
            .insert(key, GcWeak::new(vm.heap(), executable));
        for breakpoint in self.breakpoints.borrow().iter() {
            self.resolve_breakpoint_in_executable(breakpoint, &executable);
        }
    }

    /// Forgets the executables that died in this collection, as LibGC prunes a WeakHashSet.
    pub fn remove_dead_executables(&self) {
        self.executables
            .borrow_mut()
            .retain(|_, executable| executable.get().is_some_and(|executable| !cell_is_dead(executable)));
    }

    fn resolve_breakpoint(&self, breakpoint: &Breakpoint) {
        let executables = self.executables.borrow();
        let mut resolved_positions: HashMap<*const SourceCode, Position> = HashMap::new();

        for executable in executables.values().filter_map(GcWeak::get) {
            executable.remove_debugger_breakpoint(breakpoint.id);
            let Some(candidate) = breakpoint_candidate_for_executable(breakpoint, &executable) else {
                continue;
            };

            let candidate_position = Position {
                line: candidate.line,
                column: candidate.column,
            };
            let resolved_position = resolved_positions
                .entry(source_code_pointer(&executable))
                .or_insert(candidate_position);
            if candidate_position < *resolved_position {
                *resolved_position = candidate_position;
            }
        }

        for executable in executables.values().filter_map(GcWeak::get) {
            let Some(candidate) = breakpoint_candidate_for_executable(breakpoint, &executable) else {
                continue;
            };
            let Some(resolved_position) = resolved_positions.get(&source_code_pointer(&executable)) else {
                continue;
            };

            if candidate.line == resolved_position.line && candidate.column == resolved_position.column {
                executable.add_debugger_breakpoint(candidate.bytecode_offset, breakpoint.id);
            }
        }
    }

    fn resolve_breakpoint_in_executable(&self, breakpoint: &Breakpoint, executable: &Executable) {
        let Some(candidate) = breakpoint_candidate_for_executable(breakpoint, executable) else {
            return;
        };

        let executables = self.executables.borrow();
        let mut resolved_candidate = None;
        for existing_executable in executables.values().filter_map(GcWeak::get) {
            if source_code_pointer(&existing_executable) != source_code_pointer(executable) {
                continue;
            }
            if existing_executable.has_debugger_breakpoint(breakpoint.id) {
                resolved_candidate = breakpoint_candidate_for_executable(breakpoint, &existing_executable);
                break;
            }
        }

        let position_is_before =
            |a: &SourceMapEntry, b: &SourceMapEntry| a.line < b.line || (a.line == b.line && a.column < b.column);
        if let Some(resolved_candidate) = &resolved_candidate {
            if position_is_before(resolved_candidate, &candidate) {
                return;
            }

            if position_is_before(&candidate, resolved_candidate) {
                for existing_executable in executables.values().filter_map(GcWeak::get) {
                    if source_code_pointer(&existing_executable) == source_code_pointer(executable) {
                        existing_executable.remove_debugger_breakpoint(breakpoint.id);
                    }
                }
            }
        }

        executable.add_debugger_breakpoint(candidate.bytecode_offset, breakpoint.id);
    }

    fn clear_executable_breakpoints(&self) {
        for executable in self.executables.borrow().values().filter_map(GcWeak::get) {
            executable.clear_debugger_breakpoints();
        }
    }

    pub fn should_pause_on_next_bytecode_execution(&self, executable: &Executable, bytecode_offset: u32) -> bool {
        if !self.pause_on_next_bytecode_execution.get() {
            return false;
        }

        if executable
            .source_range_at(bytecode_offset)
            .is_some_and(|source_range| source_range.start.line > 0)
        {
            return true;
        }

        for entry in &executable.source_map {
            if entry.bytecode_offset > bytecode_offset && entry.line > 0 {
                return false;
            }
        }

        true
    }
}

impl Drop for Debugger {
    fn drop(&mut self) {
        self.clear_executable_breakpoints();
    }
}

// SAFETY: The last exception the debugger paused at is the only cell it keeps alive.
unsafe impl Trace for Debugger {
    fn trace(&self, visitor: &mut Visitor) {
        self.last_paused_exception.trace(visitor);
    }
}

/// ExecutionContext::local_variables()
fn local_variables<'context>(context: &'context ExecutionContext, executable: &Executable) -> &'context [Cell<Value>] {
    let local_index_base = executable.local_index_base() as usize;
    &context.slots()[local_index_base..local_index_base + executable.local_variable_names.len()]
}

fn source_code_pointer(executable: &Executable) -> *const SourceCode {
    executable.source_code().map_or(core::ptr::null(), Rc::as_ptr)
}

fn is_same_source_code(a: Option<&Rc<SourceCode>>, b: Option<&Rc<SourceCode>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

fn breakpoint_candidate_for_executable(breakpoint: &Breakpoint, executable: &Executable) -> Option<SourceMapEntry> {
    if let Some(source_code) = &breakpoint.source_code {
        if source_code_pointer(executable) != Rc::as_ptr(source_code) {
            return None;
        }
    } else {
        let executable_filename = executable.source_code().map_or(Utf16View::EMPTY, |source_code| {
            Utf16View::of_string(source_code.filename())
        });
        if executable_filename != Utf16View::of_string(&breakpoint.filename) {
            return None;
        }
    }

    let mut matching_entry: Option<SourceMapEntry> = None;
    for entry in &executable.source_map {
        if entry.line == 0 || entry.line < breakpoint.line {
            continue;
        }
        if entry.line == breakpoint.line && breakpoint.column.is_some_and(|column| entry.column < column) {
            continue;
        }
        if matching_entry.is_none_or(|matching_entry| {
            entry.line < matching_entry.line
                || (entry.line == matching_entry.line && entry.column < matching_entry.column)
        }) {
            matching_entry = Some(*entry);
        }
    }
    matching_entry
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::bytecode::op;
    use crate::layout::realm::Realm;
    use crate::runtime::abstract_operations::call;
    use crate::runtime::native_function::raw_native;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::property_key::PropertyKey;
    use crate::script::Script;
    use crate::utilities::initialize_realm;

    /// What a test remembers of a pause after the callback returns.
    #[derive(Clone, Debug, PartialEq)]
    struct RecordedPause {
        reason: PauseReason,
        line: Option<u32>,
    }

    fn pause(reason: PauseReason, line: u32) -> RecordedPause {
        RecordedPause {
            reason,
            line: Some(line),
        }
    }

    fn parse(vm: &Vm, realm: Gc<Realm>, source: &str, filename: &str) -> Gc<Script> {
        let source: Vec<u16> = source.encode_utf16().collect();
        Script::parse_with_filename(vm, &source, realm, filename).expect("the script parses")
    }

    fn debugger(vm: &Vm) -> Rc<Debugger> {
        vm.debugger().expect("debugging is enabled")
    }

    fn add_breakpoint(vm: &Vm, filename: &str, line: u32, column: Option<u32>) -> BreakpointID {
        let filename = ak::Utf16String::from_utf8(filename);
        debugger(vm)
            .add_breakpoint(Utf16View::of_string(&filename), line, column)
            .expect("the breakpoint is added")
    }

    fn top_frame(pause_info: &PauseInfo) -> &ExecutionContext {
        // SAFETY: The paused frame stays live while the pause callback runs.
        unsafe { pause_info.stack_trace[0].execution_context.as_ref() }
    }

    fn evaluate(vm: &Vm, pause_info: &PauseInfo, source: &str) -> ThrowCompletionOr<Value> {
        let source = ak::Utf16String::from_utf8(source);
        debugger(vm).evaluate_in_frame(vm, top_frame(pause_info), Utf16View::of_string(&source))
    }

    fn line_of(pause_info: &PauseInfo) -> Option<u32> {
        pause_info
            .source_range
            .as_ref()
            .map(|source_range| source_range.start.line)
    }

    /// Has the debugger record each pause and resume as `resume_mode` says for the number of pauses so far.
    fn record_pauses(vm: &Vm, resume_mode: impl Fn(usize) -> ResumeMode + 'static) -> Rc<RefCell<Vec<RecordedPause>>> {
        let pauses = Rc::new(RefCell::new(Vec::new()));
        let pauses_in_callback = Rc::clone(&pauses);
        debugger(vm).set_pause_callback(move |vm, pause_info| {
            pauses_in_callback.borrow_mut().push(RecordedPause {
                reason: pause_info.reason,
                line: line_of(pause_info),
            });
            let pause_count = pauses_in_callback.borrow().len();
            debugger(vm).continue_execution(resume_mode(pause_count));
        });
        pauses
    }

    fn first_pause_then(resume_mode: ResumeMode) -> impl Fn(usize) -> ResumeMode {
        move |pause_count| {
            if pause_count == 1 {
                resume_mode
            } else {
                ResumeMode::Continue
            }
        }
    }

    /// Counts the calls of the pause callback, which checks each pause with `check` and continues.
    fn count_pauses(vm: &Vm, check: impl Fn(&Vm, &PauseInfo) + 'static) -> Rc<Cell<usize>> {
        let pause_count = Rc::new(Cell::new(0));
        let pause_count_in_callback = Rc::clone(&pause_count);
        debugger(vm).set_pause_callback(move |vm, pause_info| {
            pause_count_in_callback.set(pause_count_in_callback.get() + 1);
            check(vm, pause_info);
            debugger(vm).continue_execution(ResumeMode::Continue);
        });
        pause_count
    }

    #[test]
    fn debugger_statement_pauses_execution() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), "debugger; 42;", "debugger.js");

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |_, pause_info| {
            assert_eq!(pause_info.reason, PauseReason::DebuggerStatement);
            assert_eq!(pause_info.bytecode_offset, op::Enter::LENGTH);
            let source_range = pause_info.source_range.as_ref().expect("the pause has a source range");
            assert!(Utf16View::of_string(source_range.filename()) == "debugger.js");
            assert_eq!(source_range.start.line, 1);
            assert_eq!(source_range.start.column, 1);
        });

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_statement_continues_without_pause_callback() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), "debugger; 42;", "");

        vm.enable_debugging();

        assert!(vm.run_script(script, None).is_ok());
    }

    #[test]
    fn debugger_pause_reports_the_active_stack() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "\nfunction outer() {\n    inner();\n}\nfunction inner() {\n    debugger;\n}\nouter();\n",
            "stack.js",
        );

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |_, pause_info| {
            assert!(pause_info.stack_trace.len() >= 3);
            assert!(top_frame(pause_info).executable.get() == Some(Executable::head(pause_info.executable)));
            let source_range = pause_info.stack_trace[0]
                .source_range
                .as_ref()
                .expect("the paused frame has a source range");
            assert_eq!(source_range.start.line, 6);

            let script_frame_count = pause_info
                .stack_trace
                .iter()
                // SAFETY: The frames of the stack stay live while the pause callback runs.
                .filter(|frame| unsafe { frame.execution_context.as_ref() }.executable.get().is_some())
                .count();
            assert_eq!(script_frame_count, 3);
        });

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    fn check_evaluation_in_a_paused_frame(collect_on_every_allocation: bool) {
        let vm = Vm::create();
        vm.heap()
            .set_should_collect_on_every_allocation(collect_on_every_allocation);
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "\nfunction answer()\n{\n    let value = 41;\n    debugger;\n    return value;\n}\nanswer();\n",
            "evaluate.js",
        );

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |vm, pause_info| {
            assert_eq!(evaluate(vm, pause_info, "value + 1").ok(), Some(Value::from_i32(42)));
            assert_eq!(evaluate(vm, pause_info, "value = 50").ok(), Some(Value::from_i32(50)));
        });

        let result = vm.run_script(script, None);
        vm.heap().set_should_collect_on_every_allocation(false);
        assert_eq!(result.ok(), Some(Value::from_i32(50)));
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_can_evaluate_in_a_paused_frame() {
        check_evaluation_in_a_paused_frame(false);
    }

    #[test]
    fn debugger_can_evaluate_in_a_paused_frame_while_collecting_on_every_allocation() {
        check_evaluation_in_a_paused_frame(true);
    }

    fn check_parameter_update(source: &str, filename: &str) {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), source, filename);

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |vm, pause_info| {
            assert_eq!(evaluate(vm, pause_info, "value += 1").ok(), Some(Value::from_i32(42)));
        });

        assert_eq!(vm.run_script(script, None).ok(), Some(Value::from_i32(42)));
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_frame_evaluation_exposes_and_updates_parameters() {
        check_parameter_update(
            "function update(value) { debugger; return value; } update(41);",
            "parameters.js",
        );
    }

    #[test]
    fn debugger_frame_evaluation_exposes_non_simple_parameters() {
        check_parameter_update(
            "function update(value = 41) { debugger; return value; } update();",
            "non-simple-parameters.js",
        );
    }

    #[test]
    fn debugger_frame_bindings_include_active_arguments_and_locals() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "\nfunction inspect(argument) {\n    const immutable = 2;\n    {\n        let inactive = 3;\n    }\n    \
             let mutable = 4;\n    debugger;\n}\ninspect(1);\n",
            "bindings.js",
        );

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |vm, pause_info| {
            let bindings = debugger(vm).bindings_for_frame(vm, top_frame(pause_info));
            let find_binding = |name: &str| {
                bindings.with_values(|bindings| {
                    bindings
                        .iter()
                        .find(|binding| Utf16View::of_fly_string(&binding.name) == name)
                        .map(|binding| (binding.value, binding.is_mutable))
                })
            };

            assert_eq!(find_binding("argument"), Some((Value::from_i32(1), true)));
            assert_eq!(find_binding("immutable"), Some((Value::from_i32(2), false)));
            assert_eq!(find_binding("mutable"), Some((Value::from_i32(4), true)));
            assert_eq!(find_binding("inactive"), None);
        });

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_frame_evaluation_preserves_const_bindings() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "function read() { const value = 41; debugger; return value; } read();",
            "const.js",
        );

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |vm, pause_info| {
            assert!(evaluate(vm, pause_info, "value = 42").is_err());
        });

        assert_eq!(vm.run_script(script, None).ok(), Some(Value::from_i32(41)));
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_frame_evaluation_does_not_overwrite_shadowed_locals() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "\nfunction readOuterValue()\n{\n    let value = 1;\n    {\n        let value = 2;\n        debugger;\n    \
             }\n    return value;\n}\nreadOuterValue();\n",
            "shadowed-locals.js",
        );

        vm.enable_debugging();
        let pause_count = count_pauses(&vm, |vm, pause_info| {
            assert_eq!(evaluate(vm, pause_info, "value").ok(), Some(Value::from_i32(2)));
        });

        assert_eq!(vm.run_script(script, None).ok(), Some(Value::from_i32(1)));
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn debugger_frame_evaluation_uses_the_active_shadowed_binding() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(
            &vm,
            root_execution_context.realm(),
            "\nfunction readValue()\n{\n    let value = 1;\n    debugger;\n    {\n        let value = 2;\n        \
             debugger;\n    }\n    debugger;\n}\nreadValue();\n",
            "shadowed-live-ranges.js",
        );

        let values = Rc::new(RefCell::new(Vec::new()));
        let values_in_callback = Rc::clone(&values);
        vm.enable_debugging();
        count_pauses(&vm, move |vm, pause_info| {
            let value = evaluate(vm, pause_info, "value").expect("the evaluation succeeds");
            values_in_callback.borrow_mut().push(value.as_i32());
        });

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(*values.borrow(), [1, 2, 1]);
    }

    #[test]
    fn breakpoints_resolve_to_source_map_entries() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "breakpoint.js", 2, None);
        assert_eq!(add_breakpoint(&vm, "breakpoint.js", 2, None), breakpoint_id);

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "let first = 1;\nlet second = 2;\n",
            "breakpoint.js",
        );
        assert!(debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        let executable = script.cached_executable();
        let source_map_entry = *executable
            .source_map
            .iter()
            .find(|entry| entry.line == 2)
            .expect("the source map has an entry on the second line");
        assert!(executable.has_debugger_breakpoint_at(source_map_entry.bytecode_offset));

        assert!(debugger(&vm).remove_breakpoint(breakpoint_id));
        assert!(!executable.has_debugger_breakpoint_at(source_map_entry.bytecode_offset));
        assert!(!debugger(&vm).remove_breakpoint(breakpoint_id));
    }

    #[test]
    fn source_specific_breakpoints_do_not_match_other_sources_with_the_same_filename() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        vm.enable_debugging();
        let first_executable = parse(&vm, realm, "let first = 1;\n", "shared.js").cached_executable();

        let breakpoint_id = debugger(&vm)
            .add_breakpoint_for_source_code(
                Rc::clone(first_executable.source_code().expect("the script has source code")),
                1,
                None,
            )
            .expect("the breakpoint is added");
        assert!(first_executable.has_debugger_breakpoint(breakpoint_id));

        let second_executable = parse(&vm, realm, "let second = 2;\n", "shared.js").cached_executable();
        assert!(!second_executable.has_debugger_breakpoint(breakpoint_id));
    }

    #[test]
    fn breakpoints_resolve_when_an_existing_executable_runs() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), "let value = 1;\n", "existing.js");

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "existing.js", 1, None);
        assert!(!debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        assert!(vm.run_script(script, None).is_ok());
        assert!(debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        let executable = script.cached_executable();
        vm.disable_debugging();
        assert!(!executable.has_debugger_breakpoint(breakpoint_id));
    }

    #[test]
    fn breakpoints_resolve_when_lazy_functions_are_compiled() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "lazy.js", 2, None);

        let declaration = parse(
            &vm,
            realm,
            "function lazy() {\n    let value = 1;\n    return value;\n}\n",
            "lazy.js",
        );
        assert!(!debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        assert!(vm.run_script(declaration, None).is_ok());
        assert!(!debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        let call = parse(&vm, realm, "lazy();", "caller.js");
        assert!(vm.run_script(call, None).is_ok());
        assert!(debugger(&vm).is_breakpoint_resolved(breakpoint_id));
    }

    #[test]
    fn breakpoints_slide_to_the_next_source_position() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "slide.js", 2, None);

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "let first = 1;\n\nlet second = 2;\n",
            "slide.js",
        );
        assert!(debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        let executable = script.cached_executable();
        let source_map_entry = *executable
            .source_map
            .iter()
            .find(|entry| entry.line == 3)
            .expect("the source map has an entry on the third line");
        assert!(executable.has_debugger_breakpoint_at(source_map_entry.bytecode_offset));
    }

    #[test]
    fn breakpoints_slide_to_the_closest_position_across_nested_executables() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        let declaration = parse(
            &vm,
            realm,
            "function target() {\n    // Breakpoint requested here.\n    let inside = 1;\n}\nlet outside = 2;\n",
            "nested-slide.js",
        );

        vm.enable_debugging();
        assert!(vm.run_script(declaration, None).is_ok());

        let breakpoint_id = add_breakpoint(&vm, "nested-slide.js", 2, None);
        let top_level_executable = declaration.cached_executable();
        assert!(top_level_executable.has_debugger_breakpoint(breakpoint_id));

        let pauses = record_pauses(&vm, |_| ResumeMode::Continue);

        let call = parse(&vm, realm, "target();", "caller.js");
        assert!(vm.run_script(call, None).is_ok());

        assert_eq!(*pauses.borrow(), [pause(PauseReason::Breakpoint, 3)]);
        assert!(!top_level_executable.has_debugger_breakpoint(breakpoint_id));
    }

    #[test]
    fn breakpoints_resolve_when_precompiled_functions_are_called_inline() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        let declaration = parse(&vm, realm, "function target() {\n    return 42;\n}\n", "inline.js");
        assert!(vm.run_script(declaration, None).is_ok());

        let call = parse(&vm, realm, "target();", "caller.js");
        assert!(vm.run_script(call, None).is_ok());

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "inline.js", 2, None);
        assert!(!debugger(&vm).is_breakpoint_resolved(breakpoint_id));

        assert!(vm.run_script(call, None).is_ok());
        assert!(debugger(&vm).is_breakpoint_resolved(breakpoint_id));
    }

    #[test]
    fn manual_breakpoints_pause_execution() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "manual.js", 2, None);

        let pause_count = count_pauses(&vm, move |_, pause_info| {
            assert_eq!(pause_info.reason, PauseReason::Breakpoint);
            assert_eq!(line_of(pause_info), Some(2));
            assert_eq!(pause_info.breakpoint_ids, [breakpoint_id]);
        });

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "var first = 1;\nvar second = 2;\n",
            "manual.js",
        );
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);

        assert!(debugger(&vm).remove_breakpoint(breakpoint_id));
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn column_breakpoints_resolve_at_or_after_their_column() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        assert_eq!(
            debugger(&vm).add_breakpoint(Utf16View::of_string(&ak::Utf16String::from_utf8("column.js")), 0, None),
            Err("Breakpoint line must be greater than zero")
        );
        let breakpoint_id = add_breakpoint(&vm, "column.js", 1, Some(12));

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "let a = 1; let b = 2;\n",
            "column.js",
        );
        let executable = script.cached_executable();
        let expected_entry = *executable
            .source_map
            .iter()
            .filter(|entry| entry.line == 1 && entry.column >= 12)
            .min_by_key(|entry| entry.column)
            .expect("the second declaration has a source position");

        let pause_count = count_pauses(&vm, move |_, pause_info| {
            assert_eq!(pause_info.breakpoint_ids, [breakpoint_id]);
            assert_eq!(pause_info.bytecode_offset, expected_entry.bytecode_offset);
        });
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn pause_on_next_bytecode_execution_is_one_shot() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        debugger(&vm).request_pause_on_next_bytecode_execution();
        let pauses = record_pauses(&vm, |_| ResumeMode::Continue);

        let script = parse(&vm, root_execution_context.realm(), "42;", "entry.js");
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pauses.borrow().len(), 1);
        assert_eq!(pauses.borrow()[0].reason, PauseReason::Entry);
        assert!(pauses.borrow()[0].line.is_some());

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pauses.borrow().len(), 1);
    }

    #[test]
    fn pause_on_empty_script_waits_for_frame_initialization() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), "", "empty.js");

        vm.enable_debugging();
        debugger(&vm).request_pause_on_next_bytecode_execution();

        let pause_count = count_pauses(&vm, |vm, pause_info| {
            assert!(vm.running_execution_context_ref().frame_initialized.get());
            assert_eq!(pause_info.bytecode_offset, op::Enter::LENGTH);
        });

        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pause_count.get(), 1);
    }

    #[test]
    fn pause_on_next_bytecode_execution_waits_for_a_callback() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let script = parse(&vm, root_execution_context.realm(), "42;", "entry.js");

        vm.enable_debugging();
        debugger(&vm).request_pause_on_next_bytecode_execution();
        assert!(vm.run_script(script, None).is_ok());

        let pauses = record_pauses(&vm, |_| ResumeMode::Continue);
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(pauses.borrow().len(), 1);
    }

    #[test]
    fn nested_execution_does_not_consume_a_pause_request() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        let outer_script = parse(&vm, realm, "let first = 1;\nlet second = 2;\n", "outer.js");
        let nested_script = parse(&vm, realm, "1 + 1;", "watch.js");

        vm.enable_debugging();
        debugger(&vm).request_pause_on_next_bytecode_execution();

        let pause_count = Rc::new(Cell::new(0));
        let pause_count_in_callback = Rc::clone(&pause_count);
        debugger(&vm).set_pause_callback(move |vm, _| {
            pause_count_in_callback.set(pause_count_in_callback.get() + 1);
            if pause_count_in_callback.get() == 1 {
                debugger(vm).request_pause_on_next_bytecode_execution();
                assert!(vm.run_script(nested_script, None).is_ok());
                assert_eq!(pause_count_in_callback.get(), 1);
            }
            debugger(vm).continue_execution(ResumeMode::Continue);
        });

        assert!(vm.run_script(outer_script, None).is_ok());
        assert_eq!(pause_count.get(), 2);
    }

    #[test]
    fn manual_breakpoints_replace_debugger_statement_pauses() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        add_breakpoint(&vm, "combined.js", 1, None);
        let pauses = record_pauses(&vm, |_| ResumeMode::Continue);

        let script = parse(&vm, root_execution_context.realm(), "debugger;", "combined.js");
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(*pauses.borrow(), [pause(PauseReason::Breakpoint, 1)]);
    }

    /// Pauses at a breakpoint on the call on line 4, which the first pause removes before it resumes as
    /// `first_resume_mode` says.
    fn pauses_when_resuming_from_the_call_on_line_4(
        filename: &str,
        first_resume_mode: ResumeMode,
    ) -> Vec<RecordedPause> {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, filename, 4, None);

        let pauses = Rc::new(RefCell::new(Vec::new()));
        let pauses_in_callback = Rc::clone(&pauses);
        debugger(&vm).set_pause_callback(move |vm, pause_info| {
            pauses_in_callback.borrow_mut().push(RecordedPause {
                reason: pause_info.reason,
                line: line_of(pause_info),
            });
            if pauses_in_callback.borrow().len() == 1 {
                assert!(debugger(vm).remove_breakpoint(breakpoint_id));
                debugger(vm).continue_execution(first_resume_mode);
            } else {
                debugger(vm).continue_execution(ResumeMode::Continue);
            }
        });

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "function callee() {\n    let inside = 1;\n}\ncallee();\nlet after = 2;\n",
            filename,
        );
        assert!(vm.run_script(script, None).is_ok());
        pauses.borrow().clone()
    }

    #[test]
    fn step_into_pauses_in_a_called_function() {
        assert_eq!(
            pauses_when_resuming_from_the_call_on_line_4("step-into.js", ResumeMode::StepInto),
            [pause(PauseReason::Breakpoint, 4), pause(PauseReason::Step, 2)]
        );
    }

    #[test]
    fn step_over_does_not_pause_in_called_functions() {
        assert_eq!(
            pauses_when_resuming_from_the_call_on_line_4("step-over.js", ResumeMode::StepOver),
            [pause(PauseReason::Breakpoint, 4), pause(PauseReason::Step, 5)]
        );
    }

    #[test]
    fn step_out_pauses_after_the_current_function_returns() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let pauses = record_pauses(&vm, first_pause_then(ResumeMode::StepOut));

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "function callee() {\n    debugger;\n}\ncallee();\nlet after = 2;\n",
            "step-out.js",
        );
        assert!(vm.run_script(script, None).is_ok());

        assert_eq!(
            *pauses.borrow(),
            [pause(PauseReason::DebuggerStatement, 2), pause(PauseReason::Step, 5)]
        );
    }

    fn run_both(vm: &Vm) -> ThrowCompletionOr<Value> {
        call(vm, vm.argument(0), Value::UNDEFINED, &[])?;
        call(vm, vm.argument(1), Value::UNDEFINED, &[])
    }

    #[test]
    fn step_out_distinguishes_reused_inline_frames() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        vm.enable_debugging();

        realm.global_object().define_native_function(
            &vm,
            realm,
            &PropertyKey::from(Utf16FlyString::from_utf8("runBoth")),
            raw_native!(run_both),
            2,
            DEFAULT_ATTRIBUTES,
            None,
        );

        let pauses = record_pauses(&vm, first_pause_then(ResumeMode::StepOut));

        let script = parse(
            &vm,
            realm,
            "function first() { debugger; }\nfunction second() { let inside = 1; }\nrunBoth(first, \
             second);\nlet after = 2;\n",
            "step-out-reused-frame.js",
        );
        assert!(vm.run_script(script, None).is_ok());

        assert_eq!(
            *pauses.borrow(),
            [pause(PauseReason::DebuggerStatement, 1), pause(PauseReason::Step, 2)]
        );
    }

    #[test]
    fn ignored_step_pauses_preserve_the_active_step() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let breakpoint_id = add_breakpoint(&vm, "ignored-step.js", 4, None);

        let pauses = Rc::new(RefCell::new(Vec::new()));
        let pauses_in_callback = Rc::clone(&pauses);
        debugger(&vm).set_pause_callback(move |vm, pause_info| {
            pauses_in_callback.borrow_mut().push(RecordedPause {
                reason: pause_info.reason,
                line: line_of(pause_info),
            });
            let pause_count = pauses_in_callback.borrow().len();
            let debugger = debugger(vm);
            match pause_count {
                1 => {
                    assert!(debugger.remove_breakpoint(breakpoint_id));
                    debugger.continue_execution(ResumeMode::StepInto);
                }
                2 => debugger.continue_execution_preserving_step_state(),
                _ => debugger.continue_execution(ResumeMode::Continue),
            }
        });

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "function callee() {\n    let inside = 1;\n}\ncallee();\nlet after = 2;\n",
            "ignored-step.js",
        );
        assert!(vm.run_script(script, None).is_ok());

        let pauses = pauses.borrow();
        assert_eq!(pauses.len(), 3);
        assert_eq!(pauses[1..], [pause(PauseReason::Step, 2), pause(PauseReason::Step, 5)]);
    }

    #[test]
    fn step_state_does_not_survive_its_bytecode_execution() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        vm.enable_debugging();
        let pauses = record_pauses(&vm, |_| ResumeMode::StepInto);

        let first_script = parse(&vm, realm, "debugger;", "first.js");
        assert!(vm.run_script(first_script, None).is_ok());
        assert_eq!(pauses.borrow().len(), 1);

        let second_script = parse(&vm, realm, "let later = 1;", "second.js");
        assert!(vm.run_script(second_script, None).is_ok());
        assert_eq!(pauses.borrow().len(), 1);
    }

    /// The line of each exception pause, the exception, and whether it will be caught.
    type ExceptionPauses = Rc<RefCell<Vec<(Option<u32>, i32, bool)>>>;

    fn record_exception_pauses(vm: &Vm, mode: PauseOnExceptions) -> ExceptionPauses {
        debugger(vm).set_pause_on_exceptions(mode);
        let pauses = Rc::new(RefCell::new(Vec::new()));
        let pauses_in_callback = Rc::clone(&pauses);
        count_pauses(vm, move |_, pause_info| {
            assert_eq!(pause_info.reason, PauseReason::Exception);
            let exception = pause_info.exception.expect("an exception pause has the exception");
            pauses_in_callback.borrow_mut().push((
                line_of(pause_info),
                exception.as_i32(),
                pause_info.exception_will_be_caught,
            ));
        });
        pauses
    }

    #[test]
    fn pause_on_all_exceptions_reports_caught_exceptions() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let pauses = record_exception_pauses(&vm, PauseOnExceptions::All);

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "try { throw 42; } catch {}",
            "caught.js",
        );
        assert!(vm.run_script(script, None).is_ok());
        assert_eq!(*pauses.borrow(), [(Some(1), 42, true)]);
    }

    #[test]
    fn pause_on_uncaught_exceptions_ignores_caught_exceptions() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();

        vm.enable_debugging();
        let pauses = record_exception_pauses(&vm, PauseOnExceptions::Uncaught);

        let caught_script = parse(&vm, realm, "try { throw 42; } catch {}", "caught.js");
        assert!(vm.run_script(caught_script, None).is_ok());
        assert!(pauses.borrow().is_empty());

        let uncaught_script = parse(&vm, realm, "throw 43;", "uncaught.js");
        assert!(vm.run_script(uncaught_script, None).is_err());
        assert_eq!(*pauses.borrow(), [(Some(1), 43, false)]);
    }

    #[test]
    fn pause_on_uncaught_exceptions_reports_the_original_throw_through_finally() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        let pauses = record_exception_pauses(&vm, PauseOnExceptions::Uncaught);

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "try {\n    throw 42;\n} finally {\n    let cleanup = 1;\n}",
            "finally.js",
        );
        assert!(vm.run_script(script, None).is_err());
        assert_eq!(*pauses.borrow(), [(Some(2), 42, false)]);
    }

    #[test]
    fn pause_on_uncaught_exceptions_preserves_native_callback_frames() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);

        vm.enable_debugging();
        debugger(&vm).set_pause_on_exceptions(PauseOnExceptions::Uncaught);
        let pause_count = count_pauses(&vm, |_, pause_info| {
            assert!(!pause_info.exception_will_be_caught);
            assert_eq!(line_of(pause_info), Some(2));
            assert!(top_frame(pause_info).function.get().is_some());
        });

        let script = parse(
            &vm,
            root_execution_context.realm(),
            "[1].map(() => {\n    throw 42;\n});",
            "native-callback.js",
        );
        assert!(vm.run_script(script, None).is_err());
        assert_eq!(pause_count.get(), 1);
    }
}
