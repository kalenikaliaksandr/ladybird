/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The runtime's implementation of the slow paths the interpreter calls, as in Libraries/LibJS/Interpreter/SlowPaths.cpp.
//! Each group lives in its own module, and the methods here hand each call to it.

pub mod bindings;
pub mod calls;
pub mod control;
pub mod operators;
pub mod property_access;

use super::runtime_functions::{Runtime, RuntimeFunctions, SlowPathControl, handle_asm_exception};
use super::vm::Vm;
use crate::bytecode::op;

impl RuntimeFunctions for Runtime {
    // Arithmetic, comparisons, conversions and the jumps on comparisons: operators.rs.

    // Property access and its inline caches: property_access.rs.

    // Bindings and environments: bindings.rs.

    // Calls, functions and classes: calls.rs.

    // Literals, iterators, generators and control flow: control.rs.

    fn throw(vm: &Vm, pc: u32, _instruction: &op::Throw, values: &mut op::ThrowValues) -> SlowPathControl {
        handle_asm_exception(vm, pc, values.src)
    }

    fn throw_if_tdz(
        vm: &Vm,
        pc: u32,
        _instruction: &op::ThrowIfTDZ,
        values: &mut op::ThrowIfTDZValues,
    ) -> SlowPathControl {
        control::throw_if_tdz(vm, pc, values)
    }

    fn throw_if_not_object(
        vm: &Vm,
        pc: u32,
        _instruction: &op::ThrowIfNotObject,
        values: &mut op::ThrowIfNotObjectValues,
    ) -> SlowPathControl {
        control::throw_if_not_object(vm, pc, values)
    }

    fn throw_if_nullish(
        vm: &Vm,
        pc: u32,
        _instruction: &op::ThrowIfNullish,
        values: &mut op::ThrowIfNullishValues,
    ) -> SlowPathControl {
        control::throw_if_nullish(vm, pc, values)
    }

    fn throw_const_assignment(
        vm: &Vm,
        pc: u32,
        _instruction: &op::ThrowConstAssignment,
        _values: &mut op::ThrowConstAssignmentValues,
    ) -> SlowPathControl {
        control::throw_const_assignment(vm, pc)
    }
}
