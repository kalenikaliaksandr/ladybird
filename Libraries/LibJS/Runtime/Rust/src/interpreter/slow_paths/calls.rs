/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The slow paths for calls, function and class creation, and their helpers.

use ak::ScopeGuard;
use libjs_abi::{ArgumentsKind, Builtin, FunctionNamePrefix};

use crate::bytecode::op;
use crate::bytecode::operand::{InstructionHeader, OptionalOperand, StringTableIndex};
use crate::bytecode::property_access::Strict;
use crate::interpreter::runtime_functions::{
    SlowPathControl, asm_try, handle_asm_exception, unimplemented_runtime_function,
};
use crate::interpreter::vm::{EvalMode, Vm};
use crate::layout::cell::Gc;
use crate::layout::execution_context::ExecutionContext;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::{
    self, CallerMode, create_mapped_arguments_object, create_unmapped_arguments_object, function_object_as_object,
    get_prototype_from_constructor, get_this_environment, length_of_array_like, perform_eval,
};
use crate::runtime::array::Array;
use crate::runtime::class_construction::construct_class;
use crate::runtime::class_field_definition::ClassElementName;
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::ecmascript_function_object::{
    EcmascriptFunctionObject, as_ecmascript_function_object, value_as_ecmascript_function_object,
};
use crate::runtime::environment::InitializeBindingHint;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::function_environment::FunctionEnvironment;
use crate::runtime::function_object::FunctionObject;
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::math_object;
use crate::runtime::object::{Object, StackFrameInfo};
use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::shared_function_instance_data::{ConstructorKind, FunctionKind};
use crate::runtime::string_constructor;
use crate::utf16::Utf16View;

/// Op::CallType: how a call instruction calls its callee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallType {
    Call,
    Construct,
    DirectEval,
}

fn running_execution_context(vm: &Vm) -> &ExecutionContext {
    let context = vm
        .running_execution_context()
        .expect("a slow path runs in a running frame");
    // SAFETY: The running execution context is live while the slow path runs in it. Callers only use the reference
    // before anything can unwind the frame.
    unsafe { context.as_ref() }
}

fn strict_of(header: &InstructionHeader) -> Strict {
    if header.strict { Strict::Yes } else { Strict::No }
}

fn caller_mode_of(strict: Strict) -> CallerMode {
    if strict == Strict::Yes {
        CallerMode::Strict
    } else {
        CallerMode::NonStrict
    }
}

/// Executable::get_string(), as the UTF-8 the error messages are formatted in.
fn get_string(vm: &Vm, index: StringTableIndex) -> String {
    Utf16View::of_fly_string(&vm.current_executable().string_table[index.0 as usize]).to_utf8()
}

/// Value::as_array_exotic_object(): the arguments the bytecode collected into an Array for a call.
fn as_array_exotic_object(value: Value) -> Gc<Array> {
    value
        .as_object()
        .downcast::<Array>()
        .expect("the arguments of the call are an Array")
}

/// Throws a new error and hands it to the interpreter, the ASM_TRY of a throw completion.
fn throw_error(
    vm: &Vm,
    pc: u32,
    kind: ErrorKind,
    error_type: ErrorType,
    arguments: &[&dyn core::fmt::Display],
) -> SlowPathControl {
    match vm.throw_completion::<()>(kind, error_type, arguments) {
        Err(throw) => handle_asm_exception(vm, pc, throw.value()),
        Ok(()) => unreachable!("throw_completion always throws"),
    }
}

/// FunctionObject::internal_call(), which every class of function object has.
fn internal_call(
    vm: &Vm,
    function: Gc<FunctionObject>,
    callee_context: &ExecutionContext,
    this_value: Value,
) -> ThrowCompletionOr<Value> {
    let function = function_object_as_object(function);
    let Some(internal_call) = function.internal_call_method() else {
        unimplemented_runtime_function(&format!("[[Call]] of a {}", function.class().name), 0);
    };
    internal_call(&function, vm, callee_context, this_value)
}

/// FunctionObject::internal_construct(), which every class of constructor has.
fn internal_construct(
    vm: &Vm,
    function: Gc<FunctionObject>,
    callee_context: &ExecutionContext,
    new_target: Gc<FunctionObject>,
) -> ThrowCompletionOr<Gc<Object>> {
    let function = function_object_as_object(function);
    let Some(internal_construct) = function.internal_construct_method() else {
        unimplemented_runtime_function(&format!("[[Construct]] of a {}", function.class().name), 0);
    };
    internal_construct(&function, vm, callee_context, new_target)
}

/// What FunctionObject::get_stack_frame_info() reports for a call of `function` with `argument_count` arguments.
fn stack_frame_info_for_call(vm: &Vm, function: Gc<FunctionObject>, argument_count: u32) -> StackFrameInfo {
    let mut stack_frame_info = StackFrameInfo {
        argument_count,
        ..Default::default()
    };
    function.get_stack_frame_info(vm, &mut stack_frame_info);
    stack_frame_info
}

fn argument_count_of(count: usize) -> u32 {
    u32::try_from(count).expect("the argument count fits in u32")
}

pub fn stack_overflow(vm: &Vm, pc: u32) -> SlowPathControl {
    throw_error(vm, pc, ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[])
}

pub fn get_super_constructor(vm: &Vm, pc: u32, values: &mut op::GetSuperConstructorValues) -> SlowPathControl {
    let super_constructor = abstract_operations::get_super_constructor(vm);
    values.dst = super_constructor.map_or(Value::NULL, Value::from_object);
    SlowPathControl::continue_at(pc + op::GetSuperConstructor::LENGTH)
}

/// The element key operands NewClass stores after its fixed fields.
fn element_key_operands(instruction: &op::NewClass) -> &[OptionalOperand] {
    // SAFETY: The bytecode stores the instruction's element_keys_count element key operands right after it.
    unsafe {
        core::slice::from_raw_parts(
            instruction.element_keys.as_ptr(),
            instruction.element_keys_count as usize,
        )
    }
}

pub fn new_class(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewClass,
    values: &mut op::NewClassValues,
    element_keys: &mut [Value],
) -> SlowPathControl {
    let mut super_class = Value::UNDEFINED;
    if instruction.super_class.get().is_some() {
        super_class = values.super_class;
    }
    // NB: The element keys stay in the operand record the interpreter passed, which lives on its stack, so the keys
    //     of elements without one become undefined in place.
    for (element_key, operand) in element_keys.iter_mut().zip(element_key_operands(instruction)) {
        if operand.get().is_none() {
            *element_key = Value::UNDEFINED;
        }
    }

    let class_environment = values.class_environment.as_environment();
    let outer_environment = running_execution_context(vm).lexical_environment.get();

    let executable = vm.current_executable();
    let blueprint = executable.class_blueprint(instruction.class_blueprint_index);

    let mut binding_name = None;
    let class_name;
    if !blueprint.has_name
        && let Some(lhs_name) = instruction.lhs_name.get()
    {
        class_name = executable.get_identifier(lhs_name).clone();
    } else {
        class_name = blueprint.name.clone();
        binding_name = Some(class_name.clone());
    }

    let retval = asm_try!(
        vm,
        pc,
        construct_class(
            vm,
            blueprint,
            executable,
            Some(class_environment),
            outer_environment,
            super_class,
            element_keys,
            binding_name.as_ref(),
            &class_name,
        )
    );
    values.dst = Value::from_object(retval);
    SlowPathControl::continue_at(pc + instruction.length())
}

#[cold]
fn throw_type_error_for_asm_callee(
    vm: &Vm,
    callee: Value,
    callee_type: &str,
    expression_string: Option<StringTableIndex>,
) -> ThrowCompletionOr<()> {
    if let Some(expression_string) = expression_string {
        return vm.throw_completion(
            ErrorKind::TypeError,
            ErrorType::IsNotAEvaluatedFrom,
            &[&callee, &callee_type, &get_string(vm, expression_string)],
        );
    }

    vm.throw_completion(ErrorKind::TypeError, ErrorType::IsNotA, &[&callee, &callee_type])
}

fn throw_if_needed_for_asm_call(
    vm: &Vm,
    callee: Value,
    call_type: CallType,
    expression_string: Option<StringTableIndex>,
) -> ThrowCompletionOr<()> {
    if (call_type == CallType::Call || call_type == CallType::DirectEval) && !callee.is_function() {
        return throw_type_error_for_asm_callee(vm, callee, "function", expression_string);
    }
    if call_type == CallType::Construct && !callee.is_constructor() {
        return throw_type_error_for_asm_callee(vm, callee, "constructor", expression_string);
    }
    Ok(())
}

/// Whether `callee` is the realm's %eval%, which makes a direct eval call evaluate its argument.
fn is_intrinsic_eval_function(vm: &Vm, callee: Value) -> bool {
    let realm = vm.current_realm().expect("a call runs in a realm");
    callee == Value::from_object(realm.eval_function())
}

/// The argument PerformEval evaluates: the callee frame's first argument, or undefined.
fn eval_argument(callee_context: &ExecutionContext) -> Value {
    if callee_context.argument_count.get() > 0 {
        callee_context.arguments()[0].get()
    } else {
        Value::UNDEFINED
    }
}

/// Fills the argument slots of a callee frame with `arguments`, and the remaining ones with undefined.
fn copy_arguments_into_callee_context(callee_context: &ExecutionContext, arguments: &[Value]) {
    let callee_context_argument_values = callee_context.arguments();
    for (slot, argument) in callee_context_argument_values.iter().zip(arguments) {
        slot.set(*argument);
    }
    for slot in &callee_context_argument_values[arguments.len()..] {
        slot.set(Value::UNDEFINED);
    }
    callee_context
        .passed_argument_count
        .set(argument_count_of(arguments.len()));
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn execute_asm_call(
    call_type: CallType,
    vm: &Vm,
    callee: Value,
    this_value: Value,
    arguments: &[Value],
    dst: &mut Value,
    expression_string: Option<StringTableIndex>,
    strict: Strict,
) -> ThrowCompletionOr<()> {
    throw_if_needed_for_asm_call(vm, callee, call_type, expression_string)?;

    let function = callee.as_function();

    let argument_count = argument_count_of(arguments.len());
    let stack_frame_info = stack_frame_info_for_call(vm, function, argument_count);

    let stack = vm.interpreter_stack();
    let stack_mark = stack.top.get();
    let Some(callee_context) = stack.allocate(
        stack_frame_info.registers_and_locals_count,
        stack_frame_info.constant_count,
        argument_count.max(stack_frame_info.argument_count),
    ) else {
        return vm.throw_completion(ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
    };
    let _deallocate_guard = ScopeGuard::new(|| {
        if stack.top.get() > stack_mark {
            stack.deallocate(stack_mark);
        }
    });
    // SAFETY: The frame was just allocated and stays allocated until the guard frees it.
    let callee_context = unsafe { callee_context.as_ref() };

    copy_arguments_into_callee_context(callee_context, arguments);

    let retval = if call_type == CallType::DirectEval {
        if is_intrinsic_eval_function(vm, callee) {
            perform_eval(
                vm,
                eval_argument(callee_context),
                caller_mode_of(strict),
                EvalMode::Direct,
            )?
        } else {
            internal_call(vm, function, callee_context, this_value)?
        }
    } else if call_type == CallType::Construct {
        Value::from_object(internal_construct(vm, function, callee_context, function)?)
    } else {
        internal_call(vm, function, callee_context, this_value)?
    };
    *dst = retval;
    Ok(())
}

pub fn call(
    vm: &Vm,
    pc: u32,
    instruction: &op::Call,
    values: &mut op::CallValues,
    arguments: &[Value],
) -> SlowPathControl {
    asm_try!(
        vm,
        pc,
        execute_asm_call(
            CallType::Call,
            vm,
            values.callee,
            values.this_value,
            arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + instruction.length())
}

fn call_direct_eval_impl(
    vm: &Vm,
    callee: Value,
    this_value: Value,
    arguments: &[Value],
    dst: &mut Value,
    expression_string: Option<StringTableIndex>,
    strict: Strict,
) -> ThrowCompletionOr<()> {
    throw_if_needed_for_asm_call(vm, callee, CallType::DirectEval, expression_string)?;

    let function = callee.as_function();

    let argument_count = argument_count_of(arguments.len());
    let stack_frame_info = stack_frame_info_for_call(vm, function, argument_count);

    let stack = vm.interpreter_stack();
    let stack_mark = stack.top.get();
    let Some(callee_context) = stack.allocate(
        stack_frame_info.registers_and_locals_count,
        stack_frame_info.constant_count,
        argument_count.max(stack_frame_info.argument_count),
    ) else {
        return vm.throw_completion(ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
    };
    let _deallocate_guard = ScopeGuard::new(|| stack.deallocate(stack_mark));
    // SAFETY: The frame was just allocated and stays allocated until the guard frees it.
    let callee_context = unsafe { callee_context.as_ref() };

    copy_arguments_into_callee_context(callee_context, arguments);

    let retval = if is_intrinsic_eval_function(vm, callee) {
        perform_eval(
            vm,
            eval_argument(callee_context),
            caller_mode_of(strict),
            EvalMode::Direct,
        )?
    } else {
        internal_call(vm, function, callee_context, this_value)?
    };
    *dst = retval;
    Ok(())
}

pub fn call_direct_eval(
    vm: &Vm,
    pc: u32,
    instruction: &op::CallDirectEval,
    values: &mut op::CallDirectEvalValues,
    arguments: &[Value],
) -> SlowPathControl {
    asm_try!(
        vm,
        pc,
        call_direct_eval_impl(
            vm,
            values.callee,
            values.this_value,
            arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + instruction.length())
}

#[allow(clippy::too_many_arguments)]
fn call_with_argument_array_impl(
    call_type: CallType,
    vm: &Vm,
    callee: Value,
    this_value: Value,
    arguments: Value,
    dst: &mut Value,
    expression_string: Option<StringTableIndex>,
    strict: Strict,
) -> ThrowCompletionOr<()> {
    throw_if_needed_for_asm_call(vm, callee, call_type, expression_string)?;

    let function = callee.as_function();

    let argument_array = as_array_exotic_object(arguments);
    let argument_array_length = argument_array.indexed_array_like_size();

    let stack_frame_info = stack_frame_info_for_call(vm, function, argument_array_length);

    let stack = vm.interpreter_stack();
    let stack_mark = stack.top.get();
    let Some(callee_context) = stack.allocate(
        stack_frame_info.registers_and_locals_count,
        stack_frame_info.constant_count,
        argument_array_length.max(stack_frame_info.argument_count),
    ) else {
        return vm.throw_completion(ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
    };
    let _deallocate_guard = ScopeGuard::new(|| {
        if stack.top.get() > stack_mark {
            stack.deallocate(stack_mark);
        }
    });
    // SAFETY: The frame was just allocated and stays allocated until the guard frees it.
    let callee_context = unsafe { callee_context.as_ref() };

    let callee_context_argument_values = callee_context.arguments();
    let insn_argument_count = argument_array_length as usize;

    for (index, slot) in callee_context_argument_values
        .iter()
        .take(insn_argument_count)
        .enumerate()
    {
        let index = u32::try_from(index).expect("the argument index fits in u32");
        if let Some(value) = argument_array.indexed_get(index) {
            slot.set(value.value);
        } else {
            slot.set(Value::UNDEFINED);
        }
    }
    for slot in &callee_context_argument_values[insn_argument_count..] {
        slot.set(Value::UNDEFINED);
    }
    callee_context.passed_argument_count.set(argument_array_length);

    let retval = if call_type == CallType::DirectEval && is_intrinsic_eval_function(vm, callee) {
        perform_eval(
            vm,
            eval_argument(callee_context),
            caller_mode_of(strict),
            EvalMode::Direct,
        )?
    } else if call_type == CallType::Construct {
        Value::from_object(internal_construct(vm, function, callee_context, function)?)
    } else {
        internal_call(vm, function, callee_context, this_value)?
    };

    *dst = retval;
    Ok(())
}

pub fn call_with_argument_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::CallWithArgumentArray,
    values: &mut op::CallWithArgumentArrayValues,
) -> SlowPathControl {
    asm_try!(
        vm,
        pc,
        call_with_argument_array_impl(
            CallType::Call,
            vm,
            values.callee,
            values.this_value,
            values.arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + op::CallWithArgumentArray::LENGTH)
}

pub fn call_direct_eval_with_argument_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::CallDirectEvalWithArgumentArray,
    values: &mut op::CallDirectEvalWithArgumentArrayValues,
) -> SlowPathControl {
    asm_try!(
        vm,
        pc,
        call_with_argument_array_impl(
            CallType::DirectEval,
            vm,
            values.callee,
            values.this_value,
            values.arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + op::CallDirectEvalWithArgumentArray::LENGTH)
}

/// The slow path of a call site that names a builtin taking one argument, which calls the builtin's implementation
/// directly when the callee is that builtin, and calls the callee otherwise.
macro_rules! define_unary_builtin_call_slow_path {
    ($snake_case_name:ident, $op:ident, $values:ident, $builtin:ident, $implementation:path) => {
        pub fn $snake_case_name(vm: &Vm, pc: u32, instruction: &op::$op, values: &mut op::$values) -> SlowPathControl {
            let arguments = [values.argument];
            let callee = values.callee;
            if callee.is_function() && callee.as_function().builtin() == Some(Builtin::$builtin) {
                values.dst = asm_try!(vm, pc, $implementation(vm, values.argument));
                return SlowPathControl::continue_at(pc + op::$op::LENGTH);
            }
            asm_try!(
                vm,
                pc,
                execute_asm_call(
                    CallType::Call,
                    vm,
                    callee,
                    values.this_value,
                    &arguments,
                    &mut values.dst,
                    instruction.expression_string.get(),
                    strict_of(&instruction.header),
                )
            );
            SlowPathControl::continue_at(pc + op::$op::LENGTH)
        }
    };
}

/// Like define_unary_builtin_call_slow_path, for a builtin taking two arguments.
macro_rules! define_binary_builtin_call_slow_path {
    ($snake_case_name:ident, $op:ident, $values:ident, $builtin:ident, $implementation:path) => {
        pub fn $snake_case_name(vm: &Vm, pc: u32, instruction: &op::$op, values: &mut op::$values) -> SlowPathControl {
            let arguments = [values.argument0, values.argument1];
            let callee = values.callee;
            if callee.is_function() && callee.as_function().builtin() == Some(Builtin::$builtin) {
                values.dst = asm_try!(vm, pc, $implementation(vm, values.argument0, values.argument1));
                return SlowPathControl::continue_at(pc + op::$op::LENGTH);
            }
            asm_try!(
                vm,
                pc,
                execute_asm_call(
                    CallType::Call,
                    vm,
                    callee,
                    values.this_value,
                    &arguments,
                    &mut values.dst,
                    instruction.expression_string.get(),
                    strict_of(&instruction.header),
                )
            );
            SlowPathControl::continue_at(pc + op::$op::LENGTH)
        }
    };
}

/// Like define_unary_builtin_call_slow_path, for a builtin taking no arguments whose implementation cannot throw.
macro_rules! define_nullary_builtin_call_slow_path {
    ($snake_case_name:ident, $op:ident, $values:ident, $builtin:ident, $implementation:path) => {
        pub fn $snake_case_name(vm: &Vm, pc: u32, instruction: &op::$op, values: &mut op::$values) -> SlowPathControl {
            let callee = values.callee;
            if callee.is_function() && callee.as_function().builtin() == Some(Builtin::$builtin) {
                values.dst = $implementation();
                return SlowPathControl::continue_at(pc + op::$op::LENGTH);
            }
            asm_try!(
                vm,
                pc,
                execute_asm_call(
                    CallType::Call,
                    vm,
                    callee,
                    values.this_value,
                    &[],
                    &mut values.dst,
                    instruction.expression_string.get(),
                    strict_of(&instruction.header),
                )
            );
            SlowPathControl::continue_at(pc + op::$op::LENGTH)
        }
    };
}

/// The slow path of a call site that names a builtin the interpreter only handles in its fast path, which calls the
/// callee with `$argument_fields` from the operand record.
macro_rules! define_generic_builtin_call_slow_path {
    ($snake_case_name:ident, $op:ident, $values:ident $(, $argument_field:ident)*) => {
        pub fn $snake_case_name(vm: &Vm, pc: u32, instruction: &op::$op, values: &mut op::$values) -> SlowPathControl {
            let arguments = [$(values.$argument_field),*];
            asm_try!(
                vm,
                pc,
                execute_asm_call(
                    CallType::Call,
                    vm,
                    values.callee,
                    values.this_value,
                    &arguments,
                    &mut values.dst,
                    instruction.expression_string.get(),
                    strict_of(&instruction.header),
                )
            );
            SlowPathControl::continue_at(pc + op::$op::LENGTH)
        }
    };
}

define_unary_builtin_call_slow_path!(
    call_builtin_math_abs,
    CallBuiltinMathAbs,
    CallBuiltinMathAbsValues,
    MathAbs,
    math_object::abs_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_log,
    CallBuiltinMathLog,
    CallBuiltinMathLogValues,
    MathLog,
    math_object::log_impl
);
define_binary_builtin_call_slow_path!(
    call_builtin_math_pow,
    CallBuiltinMathPow,
    CallBuiltinMathPowValues,
    MathPow,
    math_object::pow_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_exp,
    CallBuiltinMathExp,
    CallBuiltinMathExpValues,
    MathExp,
    math_object::exp_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_ceil,
    CallBuiltinMathCeil,
    CallBuiltinMathCeilValues,
    MathCeil,
    math_object::ceil_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_floor,
    CallBuiltinMathFloor,
    CallBuiltinMathFloorValues,
    MathFloor,
    math_object::floor_impl
);
define_binary_builtin_call_slow_path!(
    call_builtin_math_imul,
    CallBuiltinMathImul,
    CallBuiltinMathImulValues,
    MathImul,
    math_object::imul_impl
);
define_nullary_builtin_call_slow_path!(
    call_builtin_math_random,
    CallBuiltinMathRandom,
    CallBuiltinMathRandomValues,
    MathRandom,
    math_object::random_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_round,
    CallBuiltinMathRound,
    CallBuiltinMathRoundValues,
    MathRound,
    math_object::round_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_sqrt,
    CallBuiltinMathSqrt,
    CallBuiltinMathSqrtValues,
    MathSqrt,
    math_object::sqrt_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_sin,
    CallBuiltinMathSin,
    CallBuiltinMathSinValues,
    MathSin,
    math_object::sin_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_cos,
    CallBuiltinMathCos,
    CallBuiltinMathCosValues,
    MathCos,
    math_object::cos_impl
);
define_unary_builtin_call_slow_path!(
    call_builtin_math_tan,
    CallBuiltinMathTan,
    CallBuiltinMathTanValues,
    MathTan,
    math_object::tan_impl
);
define_generic_builtin_call_slow_path!(
    call_builtin_regexp_prototype_exec,
    CallBuiltinRegExpPrototypeExec,
    CallBuiltinRegExpPrototypeExecValues,
    argument
);
define_generic_builtin_call_slow_path!(
    call_builtin_regexp_prototype_replace,
    CallBuiltinRegExpPrototypeReplace,
    CallBuiltinRegExpPrototypeReplaceValues,
    argument0,
    argument1
);
define_generic_builtin_call_slow_path!(
    call_builtin_regexp_prototype_split,
    CallBuiltinRegExpPrototypeSplit,
    CallBuiltinRegExpPrototypeSplitValues,
    argument0,
    argument1
);
define_generic_builtin_call_slow_path!(
    call_builtin_ordinary_has_instance,
    CallBuiltinOrdinaryHasInstance,
    CallBuiltinOrdinaryHasInstanceValues,
    argument
);
define_generic_builtin_call_slow_path!(
    call_builtin_array_iterator_prototype_next,
    CallBuiltinArrayIteratorPrototypeNext,
    CallBuiltinArrayIteratorPrototypeNextValues
);
define_generic_builtin_call_slow_path!(
    call_builtin_map_iterator_prototype_next,
    CallBuiltinMapIteratorPrototypeNext,
    CallBuiltinMapIteratorPrototypeNextValues
);
define_generic_builtin_call_slow_path!(
    call_builtin_set_iterator_prototype_next,
    CallBuiltinSetIteratorPrototypeNext,
    CallBuiltinSetIteratorPrototypeNextValues
);
define_generic_builtin_call_slow_path!(
    call_builtin_string_iterator_prototype_next,
    CallBuiltinStringIteratorPrototypeNext,
    CallBuiltinStringIteratorPrototypeNextValues
);
define_unary_builtin_call_slow_path!(
    call_builtin_string_from_char_code,
    CallBuiltinStringFromCharCode,
    CallBuiltinStringFromCharCodeValues,
    StringFromCharCode,
    string_constructor::from_char_code_impl
);
define_generic_builtin_call_slow_path!(
    call_builtin_string_prototype_char_code_at,
    CallBuiltinStringPrototypeCharCodeAt,
    CallBuiltinStringPrototypeCharCodeAtValues,
    argument
);
define_generic_builtin_call_slow_path!(
    call_builtin_string_prototype_char_at,
    CallBuiltinStringPrototypeCharAt,
    CallBuiltinStringPrototypeCharAtValues,
    argument
);

pub fn call_construct(
    vm: &Vm,
    pc: u32,
    instruction: &op::CallConstruct,
    values: &mut op::CallConstructValues,
    arguments: &[Value],
) -> SlowPathControl {
    let callee = values.callee;
    if let Some(function) = value_as_ecmascript_function_object(callee)
        && function.can_inline_call()
        && callee.is_constructor()
        && function.constructor_kind() == ConstructorKind::Base
        && !function.has_class_data()
    {
        let prototype = asm_try!(
            vm,
            pc,
            get_prototype_from_constructor(vm, function.as_function_object_gc(), Intrinsics::object_prototype)
        );
        let this_object = Object::create(
            vm,
            function.realm().expect("an ECMAScript function has a realm"),
            Some(prototype),
        );
        let Some(context) = vm.push_inline_frame(
            function,
            function.inline_call_executable(),
            arguments,
            pc + instruction.length(),
            instruction.dst.0,
            Value::from_object(this_object),
            Some(function.upcast()),
            true,
        ) else {
            return throw_error(vm, pc, ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
        };
        // Constructors retain their receiver even when the body never reads this.
        // SAFETY: The frame was just entered, and the interpreter runs it next.
        unsafe { context.as_ref() }
            .this_value
            .set(Value::from_object(this_object));
        return SlowPathControl::dispatch_at(0);
    }
    asm_try!(
        vm,
        pc,
        execute_asm_call(
            CallType::Construct,
            vm,
            values.callee,
            Value::UNDEFINED,
            arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + instruction.length())
}

pub fn call_construct_with_argument_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::CallConstructWithArgumentArray,
    values: &mut op::CallConstructWithArgumentArrayValues,
) -> SlowPathControl {
    asm_try!(
        vm,
        pc,
        call_with_argument_array_impl(
            CallType::Construct,
            vm,
            values.callee,
            Value::UNDEFINED,
            values.arguments,
            &mut values.dst,
            instruction.expression_string.get(),
            strict_of(&instruction.header),
        )
    );
    SlowPathControl::continue_at(pc + op::CallConstructWithArgumentArray::LENGTH)
}

pub fn super_call_with_argument_array(
    vm: &Vm,
    pc: u32,
    instruction: &op::SuperCallWithArgumentArray,
    values: &mut op::SuperCallWithArgumentArrayValues,
) -> SlowPathControl {
    let new_target = vm.get_new_target();
    assert!(new_target.is_object());

    let super_constructor = values.super_constructor;
    if !super_constructor.is_constructor() {
        running_execution_context(vm).program_counter.set(pc);
        return throw_error(
            vm,
            pc,
            ErrorKind::TypeError,
            ErrorType::NotAConstructor,
            &[&"Super constructor"],
        );
    }

    let function = super_constructor.as_function();

    let argument_array = as_array_exotic_object(values.arguments);
    let argument_array_length = if instruction.is_synthetic {
        length_of_array_like(vm, &argument_array).must()
    } else {
        u64::from(argument_array.indexed_array_like_size())
    };
    let argument_array_length = u32::try_from(argument_array_length).expect("the argument count fits in u32");

    let stack_frame_info = stack_frame_info_for_call(vm, function, argument_array_length);

    let stack = vm.interpreter_stack();
    let stack_mark = stack.top.get();
    let Some(callee_context) = stack.allocate(
        stack_frame_info.registers_and_locals_count,
        stack_frame_info.constant_count,
        argument_array_length.max(stack_frame_info.argument_count),
    ) else {
        running_execution_context(vm).program_counter.set(pc);
        return throw_error(vm, pc, ErrorKind::InternalError, ErrorType::CallStackSizeExceeded, &[]);
    };
    let _deallocate_guard = ScopeGuard::new(|| {
        if stack.top.get() > stack_mark {
            stack.deallocate(stack_mark);
        }
    });
    // SAFETY: The frame was just allocated and stays allocated until the guard frees it.
    let callee_context = unsafe { callee_context.as_ref() };

    let callee_context_argument_values = callee_context.arguments();
    let insn_argument_count = argument_array_length as usize;

    if instruction.is_synthetic {
        for (index, slot) in callee_context_argument_values
            .iter()
            .take(insn_argument_count)
            .enumerate()
        {
            slot.set(argument_array.get_without_side_effects(vm, &PropertyKey::from_number(index as u64)));
        }
    } else {
        for (index, slot) in callee_context_argument_values
            .iter()
            .take(insn_argument_count)
            .enumerate()
        {
            let index = u32::try_from(index).expect("the argument index fits in u32");
            if let Some(value) = argument_array.indexed_get(index) {
                slot.set(value.value);
            } else {
                slot.set(Value::UNDEFINED);
            }
        }
    }
    for slot in &callee_context_argument_values[insn_argument_count..] {
        slot.set(Value::UNDEFINED);
    }
    callee_context.passed_argument_count.set(argument_array_length);

    let result = asm_try!(
        vm,
        pc,
        internal_construct(vm, function, callee_context, new_target.as_function())
    );

    let this_environment = get_this_environment(vm)
        .downcast::<FunctionEnvironment>()
        .expect("super() runs in a function environment");
    asm_try!(vm, pc, this_environment.bind_this_value(vm, Value::from_object(result)));

    let f = as_ecmascript_function_object(this_environment.function_object())
        .expect("super() runs in an ECMAScript function");
    asm_try!(vm, pc, result.initialize_instance_elements(vm, f));

    values.dst = Value::from_object(result);
    SlowPathControl::continue_at(pc + op::SuperCallWithArgumentArray::LENGTH)
}

// Try to inline a JS-to-JS call by building the callee frame through the
// shared VM::push_inline_frame() helper. Returns whether the callee frame
// was pushed; if not, the caller keeps handling the Call itself.
pub fn try_inline_call(vm: &Vm, pc: u32, instruction: &op::Call, values: &op::CallValues, arguments: &[Value]) -> bool {
    let callee = values.callee;
    let Some(callee_function) = value_as_ecmascript_function_object(callee) else {
        return false;
    };

    if !callee_function.can_inline_call() {
        return false;
    }

    vm.push_inline_frame(
        callee_function,
        callee_function.inline_call_executable(),
        arguments,
        pc + instruction.length(),
        instruction.dst.0,
        values.this_value,
        None,
        false,
    )
    .is_some()
}

fn function_name_prefix_of(raw_prefix: u32) -> FunctionNamePrefix {
    match raw_prefix {
        prefix if prefix == FunctionNamePrefix::None as u32 => FunctionNamePrefix::None,
        prefix if prefix == FunctionNamePrefix::Get as u32 => FunctionNamePrefix::Get,
        prefix if prefix == FunctionNamePrefix::Set as u32 => FunctionNamePrefix::Set,
        _ => unreachable!("{raw_prefix} is not a function name prefix"),
    }
}

fn asm_function_name_prefix_to_string(prefix: FunctionNamePrefix) -> Option<&'static str> {
    match prefix {
        FunctionNamePrefix::None => None,
        FunctionNamePrefix::Get => Some("get"),
        FunctionNamePrefix::Set => Some("set"),
    }
}

pub fn set_function_name(
    vm: &Vm,
    pc: u32,
    instruction: &op::SetFunctionName,
    values: &mut op::SetFunctionNameValues,
) -> SlowPathControl {
    let function = value_as_ecmascript_function_object(values.function);
    let Some(function) = function.filter(|function| function.name().is_empty()) else {
        return SlowPathControl::continue_at(pc + op::SetFunctionName::LENGTH);
    };

    let property_key = asm_try!(vm, pc, values.name.to_property_key(vm));
    function.set_inferred_name(
        vm,
        &ClassElementName::PropertyKey(property_key),
        asm_function_name_prefix_to_string(function_name_prefix_of(instruction.prefix)),
    );
    SlowPathControl::continue_at(pc + op::SetFunctionName::LENGTH)
}

pub fn create_rest_params(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateRestParams,
    values: &mut op::CreateRestParamsValues,
) -> SlowPathControl {
    let context = running_execution_context(vm);
    let arguments = context.arguments();
    let arguments_count = context.passed_argument_count.get() as usize;
    let realm = vm.current_realm().expect("there is a current realm");
    let array = Array::create(vm, realm, 0, None).must();
    for argument in arguments
        .iter()
        .take(arguments_count)
        .skip(instruction.rest_index as usize)
    {
        array.indexed_append(argument.get(), DEFAULT_ATTRIBUTES);
    }
    values.dst = Value::from_object(array);
    SlowPathControl::continue_at(pc + op::CreateRestParams::LENGTH)
}

pub fn create_arguments(
    vm: &Vm,
    pc: u32,
    instruction: &op::CreateArguments,
    values: &mut op::CreateArgumentsValues,
) -> SlowPathControl {
    let context = running_execution_context(vm);
    let function = context
        .function
        .get()
        .expect("an arguments object is created for a function");
    let arguments = context.arguments();
    let environment = context
        .lexical_environment
        .get()
        .expect("a function runs in an environment");

    let passed_arguments = &arguments[..context.passed_argument_count.get() as usize];
    let arguments_object = if instruction.kind == ArgumentsKind::Mapped as u32 {
        let ecmascript_function =
            as_ecmascript_function_object(function).expect("only ECMAScript functions have mapped arguments objects");
        create_mapped_arguments_object(
            vm,
            function,
            &ecmascript_function.parameter_names_for_mapped_arguments(),
            passed_arguments,
            environment,
        )
    } else {
        create_unmapped_arguments_object(vm, passed_arguments)
    };

    if instruction.dst.get().is_some() {
        values.dst = Value::from_object(arguments_object);
        return SlowPathControl::continue_at(pc + op::CreateArguments::LENGTH);
    }

    let arguments_name = vm.names.arguments.as_string();
    if instruction.is_immutable {
        environment.create_immutable_binding(vm, arguments_name, false).must();
    } else {
        environment.create_mutable_binding(vm, arguments_name, false).must();
    }
    environment
        .initialize_binding(
            vm,
            arguments_name,
            Value::from_object(arguments_object),
            InitializeBindingHint::Normal,
        )
        .must();
    SlowPathControl::continue_at(pc + op::CreateArguments::LENGTH)
}

pub fn new_function(
    vm: &Vm,
    pc: u32,
    instruction: &op::NewFunction,
    values: &mut op::NewFunctionValues,
) -> SlowPathControl {
    let shared_data = vm
        .current_executable()
        .shared_function_data(instruction.shared_function_data_index);
    let realm = vm.current_realm().expect("there is a current realm");

    let prototype = match shared_data.kind() {
        FunctionKind::Normal => realm.function_prototype(),
        FunctionKind::Generator => realm.generator_function_prototype(),
        FunctionKind::Async => realm.async_function_prototype(),
        FunctionKind::AsyncGenerator => realm.async_generator_function_prototype(),
    };

    let function = EcmascriptFunctionObject::create_from_function_data_with_prototype(
        vm,
        realm,
        shared_data,
        vm.lexical_environment(),
        running_execution_context(vm).private_environment.get(),
        prototype,
    );

    if instruction.home_object.get().is_some() {
        let home_object_value = values.home_object;
        function.make_method(home_object_value.as_object());
    }

    values.dst = Value::from_object(function);
    SlowPathControl::continue_at(pc + op::NewFunction::LENGTH)
}

/// Handles an exception a raw native function the interpreter called threw.
pub fn handle_raw_native_exception(vm: &Vm, exception: Value) -> SlowPathControl {
    let callee_frame = running_execution_context(vm);
    assert!(!callee_frame.caller_frame.get().is_null());

    // Raw-native asm calls keep their callee frame off the VM execution
    // context stack, so we have to unwind it manually before exception
    // dispatch. Match VM::handle_exception()'s inline-frame semantics by
    // probing the caller with a PC inside the Call instruction.
    let caller_pc = callee_frame.caller_return_pc.get();
    vm.unwind_inline_frame_for_exception();
    handle_asm_exception(vm, caller_pc - 1, exception)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use ak::Utf16FlyString;

    use super::*;
    use crate::bytecode::executable::{Executable, ExecutableCacheCounts};
    use crate::bytecode::operand::{Operand, OptionalIndex};
    use crate::layout::environment::Environment;
    use crate::runtime::abstract_operations::new_function_environment;
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::native_function::{NativeFunction, RawNativeFunction, raw_native};
    use crate::runtime::primitive_string::PrimitiveString;
    use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
    use crate::runtime::realm::test_realm::{TestRealm, key, thrown_message};
    use crate::runtime::symbol::{Kind, Symbol};
    use crate::script::Script;
    use libjs_abi::register::RESERVED_REGISTER_COUNT;

    const PC: u32 = 16;

    fn int(value: i32) -> Value {
        Value::from_i32(value)
    }

    fn string_of(value: Value) -> String {
        Utf16View::of_string(&value.to_utf16_string_without_side_effects()).to_utf8()
    }

    fn header(strict: bool) -> InstructionHeader {
        InstructionHeader { opcode: 0, strict }
    }

    fn optional_string_index(index: Option<u32>) -> OptionalIndex<StringTableIndex> {
        // SAFETY: An OptionalIndex is a transparent u32, with all bits set for no index.
        unsafe { core::mem::transmute::<u32, OptionalIndex<StringTableIndex>>(index.unwrap_or(u32::MAX)) }
    }

    /// A realm with the intrinsics functions are created with and a global object, whose global `eval` is the
    /// realm's %eval%. That stand-in returns its argument, since a direct eval never calls it.
    fn realm_with_global_object(vm: &Vm) -> TestRealm<'_> {
        let test_realm = TestRealm::with_function_intrinsics(vm);
        let realm = test_realm.realm;
        let global = set_up_global_object(vm, realm);
        let eval_function = NativeFunction::create(
            vm,
            (),
            |vm, _| Ok(vm.argument(0)),
            1,
            &key("eval"),
            Some(realm),
            None,
            None,
        );
        realm.intrinsics().set_eval_function_for_tests(eval_function.upcast());
        global.define_direct_property(
            vm,
            &key("eval"),
            Value::from_object(eval_function),
            PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
        );
        test_realm
    }

    fn run_in(vm: &Vm, test_realm: &TestRealm, source: &str) -> Value {
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse(vm, &code_units, test_realm.realm).expect("the script parses");
        vm.run_script(script, None)
            .unwrap_or_else(|exception| panic!("the script threw {}", string_of(exception.value())))
    }

    /// Runs `source` as the only script of a new realm, and returns what it completes with as a string.
    fn run(vm: &Vm, source: &str) -> String {
        let test_realm = realm_with_global_object(vm);
        string_of(run_in(vm, &test_realm, source))
    }

    fn global_value(vm: &Vm, test_realm: &TestRealm, name: &str) -> Value {
        test_realm.realm.global_object().get(vm, &key(name)).must()
    }

    /// Each script with what Build/release/bin/js -l prints for it, without the quotes around strings.
    const ORACLE_CASES: &[(&str, &str)] = &[
        (
            "function add(a, b) { return a + b; } function makeCounter() { var count = 0; return function () { count = count + 1; return count; }; } var c = makeCounter(); c(); c(); add(1, 2) + \",\" + c() + \",\" + add.length + \",\" + add.name",
            "3,3,2,add",
        ),
        (
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); } function isEven(n) { return n === 0 ? true : isOdd(n - 1); } function isOdd(n) { return n === 0 ? false : isEven(n - 1); } fib(20) + \",\" + fact(10) + \",\" + isEven(1000) + \",\" + isOdd(777)",
            "6765,3628800,true,true",
        ),
        (
            "function depth(n) { return n === 0 ? 0 : 1 + depth(n - 1); } depth(10000)",
            "10000",
        ),
        (
            "function Point(x, y) { this.x = x; this.y = y; } Point.prototype.sum = function () { return this.x + this.y; }; var p = new Point(3, 4); function R() { return {k: 5}; } function Prim() { this.a = 1; return 42; } p.sum() + \",\" + (p.sum === Point.prototype.sum) + \",\" + (p.constructor === Point) + \",\" + new R().k + \",\" + new Prim().a + \",\" + new Point(1).y",
            "7,true,true,5,1,undefined",
        ),
        (
            "class A { #secret = 10; pub = 1; static count = 0; static { A.count = 5; } constructor(v) { this.v = v; A.count++; } #double() { return this.#secret * 2; } get doubled() { return this.#double(); } set value(x) { this.v = x; } static create(v) { return new A(v); } static has(o) { return #secret in o; } method() { return this.v + this.pub; } } var a = A.create(3); var r = a.method() + \",\" + a.doubled + \",\" + A.count; a.value = 7; r + \",\" + a.v + \",\" + (typeof A) + \",\" + A.name + \",\" + A.has(a) + \",\" + A.has({})",
            "4,20,6,7,function,A,true,false",
        ),
        (
            "class Base { constructor(a, b) { this.s = a + b; } greet() { return \"base\" + this.s; } static who() { return \"Base\"; } } class Derived extends Base { greet() { return \"derived:\" + super.greet(); } static who() { return \"Derived<\" + super.who() + \">\"; } } var d = new Derived(2, 3); d.greet() + \",\" + Derived.who() + \",\" + (d.constructor === Derived) + \",\" + (d.greet === Derived.prototype.greet) + \",\" + d.s",
            "derived:base5,Derived<Base>,true,true,5",
        ),
        (
            "class Base { constructor(a) { this.a = a; } } class WithFields extends Base { f = this.a * 2; #p = 4; get p() { return this.#p; } } class Deeper extends WithFields { g = this.f + 1; } var w = new Deeper(5); w.a + \",\" + w.f + \",\" + w.g + \",\" + w.p",
            "5,10,11,4",
        ),
        (
            "function sloppy(a, b) { arguments[0] = 10; return a + \",\" + arguments.length + \",\" + arguments[1]; } function strict(a) { \"use strict\"; arguments[0] = 10; return a + \",\" + arguments.length; } function modify(a) { a = 5; return arguments[0]; } sloppy(1, 2) + \"|\" + strict(1, 2, 3) + \"|\" + modify(1) + \"|\" + (function () { return arguments.length; })(1, 2, 3, 4) + \"|\" + (function (a, a2) { return arguments[2]; })(1, 2, 3)",
            "10,2,2|1,3|5|4|3",
        ),
        (
            "function rest(a, ...more) { return a + \":\" + more.length + \":\" + more[0] + \":\" + more[1]; } rest(1, 2, 3) + \"|\" + rest(1) + \"|\" + rest.length",
            "1:2:2:3|1:0:undefined:undefined|1",
        ),
        (
            "var obj = { v: 4, m: function () { var f = () => this.v; return f(); } }; function withDefaults(a, b = a + 1, c = b * 2) { return a + b + c; } obj.m() + \",\" + withDefaults(1) + \",\" + withDefaults(1, 5) + \",\" + withDefaults.length",
            "4,7,16,1",
        ),
        (
            "var k = \"dyn\"; var o = { [k]: function () {}, [k + \"2\"]() {}, named: function () {}, arrow: () => 0, [k + \"3\"]: function inner() {} }; o.dyn.name + \",\" + o.dyn2.name + \",\" + o.named.name + \",\" + o.arrow.name + \",\" + o.dyn3.name",
            "dyn,dyn2,named,arrow,inner",
        ),
        (
            "var proto = { hi() { return \"proto hi\"; } }; var o = { __proto__: proto, hi() { return \"o:\" + super.hi(); } }; o.hi()",
            "o:proto hi",
        ),
        (
            "function thrower() { throw 7; } function catcher() { try { thrower(); } catch (e) { return \"caught \" + e; } } function rethrow() { try { catcher(); thrower(); } catch (e) { return \"again \" + e; } } catcher() + \",\" + rethrow()",
            "caught 7,again 7",
        ),
        (
            "var o = { _x: 1, get x() { return this._x * 10; }, set x(v) { this._x = v; } }; o.x = 3; o.x",
            "30",
        ),
        (
            "function NT() { return typeof new.target; } function NT2() { this.same = new.target === NT2; } NT() + \",\" + new NT2().same",
            "undefined,true",
        ),
        ("var eval = function (x) { return x * 2; }; eval(21)", "42"),
        (
            "eval(5) + \",\" + typeof eval({}) + \",\" + (function () { \"use strict\"; return eval(true); })() + \",\" + eval() + \",\" + eval(null, 1)",
            "5,object,true,undefined,null",
        ),
        (
            "var K = class {}; var K2 = class Named {}; var f = function () {}; var g = () => 1; K.name + \",\" + K2.name + \",\" + f.name + \",\" + g.name",
            "K,Named,f,g",
        ),
        (
            "class C { f = () => this.v; constructor() { this.v = 3; } static #priv() { return \"sp\"; } static callPriv() { return C.#priv(); } } new C().f() + \",\" + C.callPriv()",
            "3,sp",
        ),
        (
            "class Node { val = 0; constructor(n) { this.child = n > 0 ? new Node(n - 1) : null; } } var nd = new Node(50); var cnt = 0; while (nd) { cnt++; nd = nd.child; } cnt",
            "51",
        ),
        (
            "class Counter { static #count = 0; static inc() { return ++Counter.#count; } } Counter.inc(); Counter.inc(); Counter.inc()",
            "3",
        ),
        (
            "function outer() { var x = 1; function inner() { return x; } x = 2; return inner(); } function shadow(x) { { let x = 5; } return x; } outer() + \",\" + shadow(9)",
            "2,9",
        ),
        (
            "var calls = 0; var o = { m() { calls++; return this; } }; o.m().m().m(); calls",
            "3",
        ),
        (
            "class P { constructor() { this.kind = \"P\"; } } class Q extends P { constructor() { return {kind: \"override\"}; } } new Q().kind",
            "override",
        ),
        (
            "class Acc { static get s() { return \"sg\"; } static set s(v) { Acc.stored = v; } } Acc.s = 4; Acc.s + \",\" + Acc.stored",
            "sg,4",
        ),
        (
            "var Math = { abs: function (x) { return \"abs \" + x; }, pow: function (a, b) { return \"pow \" + a + \" \" + b; }, random: function () { return \"random \" + arguments.length; }, floor: function (x) { return \"floor \" + x + \" \" + this.tag; }, tag: \"M\" }; Math.abs(-3) + \",\" + Math.pow(2, 10) + \",\" + Math.random() + \",\" + Math.floor(1.5)",
            "abs -3,pow 2 10,random 0,floor 1.5 M",
        ),
        (
            "var o = { charAt: function (i) { return \"charAt \" + i; }, charCodeAt: function (i) { return \"code \" + i; }, next: function () { return \"next \" + arguments.length; } }; var ArrayIteratorPrototype = o; o.charAt(2) + \",\" + o.charCodeAt(3) + \",\" + ArrayIteratorPrototype.next(1, 2)",
            "charAt 2,code 3,next 2",
        ),
        (
            "var String = { fromCharCode: function (c) { return \"fcc \" + c; } }; String.fromCharCode(65)",
            "fcc 65",
        ),
        (
            "function* g() {} async function af() {} async function* ag() {} typeof g + \",\" + typeof af + \",\" + typeof ag + \",\" + (\"prototype\" in g) + \",\" + (\"prototype\" in af) + \",\" + (\"prototype\" in ag)",
            "function,function,function,true,false,true",
        ),
    ];

    /// Like ORACLE_CASES, for scripts that collect arguments into array literals for spread calls and super calls,
    /// which need the literal slow paths.
    const ORACLE_CASES_WITH_ARRAY_LITERALS: &[(&str, &str)] = &[
        (
            "function f(a, b, c) { return a + b + c; } var args = [1, 2, 3]; f(...args) + \",\" + f(0, ...args)",
            "6,3",
        ),
        (
            "class B { constructor(a, b) { this.s = a + b; } } class D extends B { constructor(x) { super(x, 10); this.t = this.s * 2; } } var d = new D(1); d.s + \",\" + d.t",
            "11,22",
        ),
        ("function P(a, b) { this.v = a * b; } new P(...[6, 7]).v", "42"),
        (
            "class B { constructor(...xs) { this.n = xs.length; } } class D extends B { constructor() { super(...[1, 2], 3); } } new D().n",
            "3",
        ),
        (
            "class B { constructor(v) { this.v = v; } } class D extends B { f = this.v + 1; constructor() { super(41); } } new D().f",
            "42",
        ),
        ("eval(...[5]) + \",\" + eval(...[\"x\"].length ? [6] : [])", "5,6"),
        (
            "var o = { m(...a) { return this === o && a.length; } }; o.m(...[1, 2, 3, 4])",
            "4",
        ),
        (
            "class B { constructor() { this.b = 1; } } class D extends B { constructor() { var arrow = () => super(); arrow(); this.d = 2; } } var x = new D(); x.b + x.d",
            "3",
        ),
    ];

    fn check_oracle_cases(vm: &Vm, cases: &[(&str, &str)]) {
        for (source, expected) in cases {
            assert_eq!(run(vm, source), *expected, "for {source}");
        }
    }

    #[test]
    fn scripts_call_construct_and_define_functions_and_classes_like_the_cpp_runtime() {
        let vm = Vm::create();
        check_oracle_cases(&vm, ORACLE_CASES);
    }

    #[test]
    fn scripts_keep_everything_alive_when_collecting_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        check_oracle_cases(&vm, ORACLE_CASES);
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn spread_and_super_calls_with_argument_arrays_behave_like_the_cpp_runtime() {
        let vm = Vm::create();
        check_oracle_cases(&vm, ORACLE_CASES_WITH_ARRAY_LITERALS);
        vm.heap().set_should_collect_on_every_allocation(true);
        check_oracle_cases(&vm, ORACLE_CASES_WITH_ARRAY_LITERALS);
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    /// A raw native function the realm's Math or String object holds for `builtin`, as the builtins will define them.
    /// Its own body is never meant to run: call sites that name it either take the interpreter's fast path or call
    /// the builtin's implementation from the slow path.
    fn install_builtin_stand_in(vm: &Vm, test_realm: &TestRealm, holder: Gc<Object>, builtin: Builtin) {
        let function = RawNativeFunction::create(
            vm,
            raw_native!(|vm| Ok(Value::from_string(PrimitiveString::create_from_utf8(
                vm,
                "the native body ran"
            )))),
            i32::try_from(builtin.argument_count()).expect("the argument count fits in i32"),
            &key(builtin.property()),
            Some(test_realm.realm),
            None,
            Some(builtin),
        );
        holder.define_direct_property(
            vm,
            &key(builtin.property()),
            Value::from_object(function),
            PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
        );
    }

    #[test]
    fn call_sites_naming_a_builtin_call_its_implementation_like_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        let global = test_realm.realm.global_object();
        let math = test_realm.object();
        let string = test_realm.object();
        for builtin in Builtin::ALL {
            match builtin.base() {
                "Math" if *builtin != Builtin::MathRandom => install_builtin_stand_in(&vm, &test_realm, math, *builtin),
                "String" => install_builtin_stand_in(&vm, &test_realm, string, *builtin),
                _ => {}
            }
        }
        let attributes = PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE);
        global.define_direct_property(&vm, &key("Math"), Value::from_object(math), attributes);
        global.define_direct_property(&vm, &key("String"), Value::from_object(string), attributes);

        // What Build/release/bin/js prints for the same script.
        let result = run_in(
            &vm,
            &test_realm,
            "Math.abs(\"-3\") + \",\" + Math.floor(\"2.5\") + \",\" + (1 / Math.ceil(\"-0.5\")) + \",\" + Math.round(\"2.5\") + \",\" + Math.round(-2.5) + \",\" + (1 / Math.round(-0.2)) + \",\" + Math.sqrt(\"16\") + \",\" + Math.exp(\"0\") + \",\" + Math.log(\"1\") + \",\" + Math.pow(\"2\", \"10\") + \",\" + Math.imul(\"3\", 4) + \",\" + Math.imul(0xffffffff, 5) + \",\" + Math.sin(\"0\") + \",\" + Math.cos(\"0\") + \",\" + Math.tan(\"0\") + \",\" + String.fromCharCode(\"65\") + \",\" + Math.abs(-2147483648) + \",\" + Math.log(-1) + \",\" + Math.log(0) + \",\" + Math.sqrt(-1)",
        );
        assert_eq!(
            string_of(result),
            "3,2,-Infinity,3,-2,-Infinity,4,1,0,1024,12,-5,0,1,0,A,2147483648,NaN,-Infinity,NaN"
        );

        // A callee that is some other function is called, with the call site's this value.
        let not_the_builtin = RawNativeFunction::create(
            &vm,
            raw_native!(|vm| Ok(Value::from_f64(vm.argument(0).as_f64() + 100.0))),
            1,
            &key("other"),
            None,
            None,
            Some(Builtin::MathFloor),
        );
        let instruction = op::CallBuiltinMathAbs {
            header: header(false),
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            argument: Operand(0),
            expression_string: optional_string_index(None),
        };
        let mut values = op::CallBuiltinMathAbsValues {
            dst: Value::EMPTY,
            callee: Value::from_object(not_the_builtin),
            this_value: Value::UNDEFINED,
            argument: int(-5),
        };
        assert_eq!(
            call_builtin_math_abs(&vm, PC, &instruction, &mut values),
            SlowPathControl::continue_at(PC + op::CallBuiltinMathAbs::LENGTH)
        );
        assert_eq!(values.dst, int(95));
    }

    #[test]
    fn the_math_implementations_handle_zeros_infinities_and_conversions() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let negative_zero = Value::from_f64(-0.0);
        let number = |value: Value| value.as_f64();
        assert!(math_object::abs_impl(&vm, negative_zero).must().is_positive_zero());
        assert!(
            math_object::abs_impl(&vm, Value::from_f64(f64::NEG_INFINITY)).must() == Value::from_f64(f64::INFINITY)
        );
        assert!(math_object::abs_impl(&vm, Value::UNDEFINED).must().is_nan());
        assert!(
            math_object::ceil_impl(&vm, Value::from_f64(-0.5))
                .must()
                .is_negative_zero()
        );
        assert!(math_object::ceil_impl(&vm, negative_zero).must().is_negative_zero());
        assert!(
            math_object::floor_impl(&vm, Value::from_f64(0.5))
                .must()
                .is_positive_zero()
        );
        assert!(
            math_object::round_impl(&vm, Value::from_f64(-0.5))
                .must()
                .is_negative_zero()
        );
        assert_eq!(number(math_object::round_impl(&vm, Value::from_f64(2.4)).must()), 2.0);
        assert!(math_object::sqrt_impl(&vm, negative_zero).must().is_negative_zero());
        assert!(math_object::sin_impl(&vm, negative_zero).must().is_negative_zero());
        assert!(
            math_object::tan_impl(&vm, Value::from_f64(f64::INFINITY))
                .must()
                .is_nan()
        );
        assert!(math_object::cos_impl(&vm, Value::from_f64(f64::NAN)).must().is_nan());
        assert!(math_object::exp_impl(&vm, Value::from_f64(f64::NEG_INFINITY)).must() == int(0));
        assert!(math_object::log_impl(&vm, negative_zero).must() == Value::from_f64(f64::NEG_INFINITY));
        assert_eq!(math_object::imul_impl(&vm, int(-1), int(-1)).must(), int(1));
        assert!(math_object::pow_impl(&vm, int(2), Value::from_f64(0.5)).must() == Value::from_f64(2f64.sqrt()));
        let character = string_constructor::from_char_code_impl(&vm, Value::from_f64(65.0 + 65536.0)).must();
        assert_eq!(string_of(character), "A");
        assert!(
            thrown_message(math_object::random_impl).contains("MathObject::random_impl"),
            "Math.random stops the process until it has a generator"
        );
    }

    /// A running frame whose executable has `strings` in its string table, with `lexical_environment` or the global
    /// environment as its environment.
    struct Frame<'vm> {
        vm: &'vm Vm,
        stack_mark: *mut u8,
    }

    impl<'vm> Frame<'vm> {
        fn new(
            vm: &'vm Vm,
            test_realm: &TestRealm,
            strings: &[&str],
            lexical_environment: Option<Gc<Environment>>,
        ) -> Self {
            let counts = ExecutableCacheCounts {
                property_lookup_caches: 0,
                global_variable_caches: 0,
                environment_coordinate_caches: 0,
                environment_shape_caches: 0,
            };
            let mut executable = Executable::new(
                vec![0u8; 64].into_boxed_slice(),
                RESERVED_REGISTER_COUNT,
                0,
                0,
                Box::new([]),
                &counts,
                false,
            );
            executable.string_table = strings.iter().map(|string| Utf16FlyString::from_utf8(string)).collect();
            let executable = Executable::create_from_parts(vm, executable);

            let stack = vm.interpreter_stack();
            let stack_mark = stack.top.get();
            let context = stack
                .allocate(RESERVED_REGISTER_COUNT, 0, 0)
                .expect("the interpreter stack has room");
            // SAFETY: The context was just allocated.
            let context_ref = unsafe { context.as_ref() };
            context_ref.executable.set(Some(Executable::head(executable)));
            context_ref.realm.set(Some(test_realm.realm));
            let environment = lexical_environment.unwrap_or_else(|| test_realm.realm.global_environment().upcast());
            context_ref.lexical_environment.set(Some(environment));
            context_ref.variable_environment.set(Some(environment));
            vm.push_execution_context(context);
            Self { vm, stack_mark }
        }
    }

    impl Drop for Frame<'_> {
        fn drop(&mut self) {
            self.vm.pop_execution_context();
            self.vm.interpreter_stack().deallocate(self.stack_mark);
        }
    }

    fn call_instruction(argument_count: u32, expression_string: Option<u32>) -> op::Call {
        op::Call {
            header: header(false),
            length: 32,
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            argument_count,
            expression_string: optional_string_index(expression_string),
            arguments: [],
        }
    }

    fn call_values(callee: Value, this_value: Value) -> op::CallValues {
        op::CallValues {
            dst: Value::EMPTY,
            callee,
            this_value,
            arguments: [],
        }
    }

    fn call_with_argument_array_instruction(expression_string: Option<u32>) -> op::CallWithArgumentArray {
        op::CallWithArgumentArray {
            header: header(false),
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            arguments: Operand(0),
            expression_string: optional_string_index(expression_string),
        }
    }

    fn type_error(message: &str) -> String {
        format!("creating a TypeError with the message \"{message}\"")
    }

    #[test]
    fn calls_of_values_that_cannot_be_called_report_the_expression_like_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(&vm, &test_realm, "this.arrow = () => 1;");
        let arrow = global_value(&vm, &test_realm, "arrow");
        let _frame = Frame::new(&vm, &test_realm, &["o.x", "f"], None);

        // The messages Build/release/bin/js reports for `var o = {}; o.x(1)`, `1()`, `var o = {}; new o.x()` and
        // `var f = () => 1; new f()`, apart from the C++ class name of function objects.
        let message = thrown_message(|| {
            call(
                &vm,
                PC,
                &call_instruction(1, Some(0)),
                &mut call_values(Value::UNDEFINED, Value::UNDEFINED),
                &[int(1)],
            )
        });
        assert!(
            message.contains(&type_error("undefined is not a function (evaluated from 'o.x')")),
            "{message}"
        );
        let message = thrown_message(|| {
            call(
                &vm,
                PC,
                &call_instruction(0, None),
                &mut call_values(int(1), Value::UNDEFINED),
                &[],
            )
        });
        assert!(message.contains(&type_error("1 is not a function")), "{message}");

        let construct_instruction = |expression_string| op::CallConstruct {
            header: header(false),
            length: 32,
            dst: Operand(0),
            callee: Operand(0),
            argument_count: 0,
            expression_string: optional_string_index(expression_string),
            arguments: [],
        };
        let construct_values = |callee| op::CallConstructValues {
            dst: Value::EMPTY,
            callee,
            arguments: [],
        };
        let message = thrown_message(|| {
            call_construct(
                &vm,
                PC,
                &construct_instruction(Some(0)),
                &mut construct_values(Value::UNDEFINED),
                &[],
            )
        });
        assert!(
            message.contains(&type_error("undefined is not a constructor (evaluated from 'o.x')")),
            "{message}"
        );
        let message = thrown_message(|| {
            call_construct(
                &vm,
                PC,
                &construct_instruction(Some(1)),
                &mut construct_values(arrow),
                &[],
            )
        });
        assert!(
            message.contains(&type_error(
                "[object ECMAScriptFunctionObject] is not a constructor (evaluated from 'f')"
            )),
            "{message}"
        );

        let message = thrown_message(|| {
            call_with_argument_array(
                &vm,
                PC,
                &call_with_argument_array_instruction(Some(0)),
                &mut op::CallWithArgumentArrayValues {
                    dst: Value::EMPTY,
                    callee: Value::NULL,
                    this_value: Value::UNDEFINED,
                    arguments: Value::from_object(test_realm.array(&[])),
                },
            )
        });
        assert!(
            message.contains(&type_error("null is not a function (evaluated from 'o.x')")),
            "{message}"
        );

        let eval_instruction = op::CallDirectEval {
            header: header(false),
            length: 32,
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            argument_count: 0,
            expression_string: optional_string_index(None),
            arguments: [],
        };
        let message = thrown_message(|| {
            call_direct_eval(
                &vm,
                PC,
                &eval_instruction,
                &mut op::CallDirectEvalValues {
                    dst: Value::EMPTY,
                    callee: int(1),
                    this_value: Value::UNDEFINED,
                    arguments: [],
                },
                &[],
            )
        });
        assert!(message.contains(&type_error("1 is not a function")), "{message}");
    }

    #[test]
    fn calls_with_argument_arrays_pass_holes_as_undefined() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(
            &vm,
            &test_realm,
            "this.collect = function (a, b, c) { return this.tag + \":\" + a + \",\" + b + \",\" + c + \",\" + arguments.length; }; function P(a, b) { this.v = a * b; } this.P = P;",
        );
        let collect = global_value(&vm, &test_realm, "collect");
        let constructor = global_value(&vm, &test_realm, "P");
        let _frame = Frame::new(&vm, &test_realm, &[], None);

        let receiver = test_realm.object();
        receiver.define_direct_property(
            &vm,
            &key("tag"),
            Value::from_string(PrimitiveString::create_from_utf8(&vm, "r")),
            DEFAULT_ATTRIBUTES,
        );
        let with_hole = Array::create(&vm, test_realm.realm, 0, None).must();
        with_hole.indexed_put(0, int(1), DEFAULT_ATTRIBUTES);
        with_hole.indexed_put(2, int(3), DEFAULT_ATTRIBUTES);
        let mut values = op::CallWithArgumentArrayValues {
            dst: Value::EMPTY,
            callee: collect,
            this_value: Value::from_object(receiver),
            arguments: Value::from_object(with_hole),
        };
        assert_eq!(
            call_with_argument_array(&vm, PC, &call_with_argument_array_instruction(None), &mut values),
            SlowPathControl::continue_at(PC + op::CallWithArgumentArray::LENGTH)
        );
        assert_eq!(string_of(values.dst), "r:1,undefined,3,3");

        let mut values = op::CallConstructWithArgumentArrayValues {
            dst: Value::EMPTY,
            callee: constructor,
            this_value: Value::UNDEFINED,
            arguments: Value::from_object(test_realm.array(&[int(6), int(7)])),
        };
        let instruction = op::CallConstructWithArgumentArray {
            header: header(false),
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            arguments: Operand(0),
            expression_string: optional_string_index(None),
        };
        assert_eq!(
            call_construct_with_argument_array(&vm, PC, &instruction, &mut values),
            SlowPathControl::continue_at(PC + op::CallConstructWithArgumentArray::LENGTH)
        );
        let instance = values.dst.as_object();
        assert_eq!(instance.get(&vm, &key("v")).must(), int(42));
        assert!(instance.prototype() == Some(constructor.as_object().get(&vm, &key("prototype")).must().as_object()));

        // A direct eval with an argument array evaluates its first argument when the callee is %eval%, and calls any
        // other callee.
        let eval_instruction = op::CallDirectEvalWithArgumentArray {
            header: header(true),
            dst: Operand(0),
            callee: Operand(0),
            this_value: Operand(0),
            arguments: Operand(0),
            expression_string: optional_string_index(None),
        };
        let mut values = op::CallDirectEvalWithArgumentArrayValues {
            dst: Value::EMPTY,
            callee: global_value(&vm, &test_realm, "eval"),
            this_value: Value::UNDEFINED,
            arguments: Value::from_object(test_realm.array(&[Value::from_object(receiver), int(2)])),
        };
        call_direct_eval_with_argument_array(&vm, PC, &eval_instruction, &mut values);
        assert!(values.dst == Value::from_object(receiver));
        values.callee = collect;
        values.this_value = Value::from_object(receiver);
        values.arguments = Value::from_object(test_realm.array(&[int(4), int(2)]));
        call_direct_eval_with_argument_array(&vm, PC, &eval_instruction, &mut values);
        assert_eq!(string_of(values.dst), "r:4,2,undefined,2");
    }

    #[test]
    fn direct_eval_stops_where_it_would_compile_the_code() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        let _frame = Frame::new(&vm, &test_realm, &[], None);
        let code = Value::from_string(PrimitiveString::create_from_utf8(&vm, "1 + 1"));
        let message = thrown_message(|| perform_eval(&vm, code, CallerMode::Strict, EvalMode::Direct));
        assert!(
            message.contains(
                "compiling the code of an eval (RustIntegration::compile_eval in PerformEval, strict caller: true, in \
                 function: false, in method: false, in derived constructor: false, in class field initializer: false)"
            ),
            "{message}"
        );

        // HostGetCodeForEval can give objects code, which is then compiled too.
        vm.set_host_get_code_for_eval(|vm, _| Some(PrimitiveString::create_from_utf8(vm, "2")));
        let object = Value::from_object(test_realm.object());
        let message = thrown_message(|| perform_eval(&vm, object, CallerMode::NonStrict, EvalMode::Indirect));
        assert!(message.contains("compiling the code of an eval"), "{message}");
    }

    #[test]
    fn the_call_slow_paths_report_a_full_interpreter_stack() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(&vm, &test_realm, "this.f = function (a) { return a; };");
        let function = global_value(&vm, &test_realm, "f");
        let _frame = Frame::new(&vm, &test_realm, &[], None);
        let expected = "creating a InternalError with the message \"Call stack size limit exceeded\"";

        assert!(thrown_message(|| stack_overflow(&vm, PC)).contains(expected));

        let stack = vm.interpreter_stack();
        let stack_mark = stack.top.get();
        let free_slots =
            (stack.limit.get() as usize - stack_mark as usize - size_of::<ExecutionContext>()) / size_of::<Value>();
        stack
            .allocate(
                u32::try_from(free_slots - 2).expect("the stack fits in u32 slots"),
                0,
                0,
            )
            .expect("the interpreter stack has room for the filler frame");
        let message = thrown_message(|| {
            call(
                &vm,
                PC,
                &call_instruction(1, None),
                &mut call_values(function, Value::UNDEFINED),
                &[int(1)],
            )
        });
        assert!(message.contains(expected), "{message}");
        stack.deallocate(stack_mark);

        // With room again, the same call goes through.
        let mut values = call_values(function, Value::UNDEFINED);
        call(&vm, PC, &call_instruction(1, None), &mut values, &[int(1)]);
        assert_eq!(values.dst, int(1));
        assert!(stack.top.get() == stack_mark);
    }

    #[test]
    fn super_calls_construct_the_parent_and_initialize_the_derived_fields() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(
            &vm,
            &test_realm,
            "class Base { constructor(a, b) { this.s = a + b; } } class Derived extends Base { d = this.s * 10; } this.Base = Base; this.Derived = Derived;",
        );
        let base = global_value(&vm, &test_realm, "Base");
        let derived = value_as_ecmascript_function_object(global_value(&vm, &test_realm, "Derived"))
            .expect("Derived is a class constructor");

        let super_call = |is_synthetic: bool, super_constructor: Value, arguments: &[Value]| {
            let environment = new_function_environment(&vm, derived, Some(derived.upcast()));
            let _frame = Frame::new(&vm, &test_realm, &[], Some(environment.upcast()));
            let instruction = op::SuperCallWithArgumentArray {
                header: header(true),
                dst: Operand(0),
                super_constructor: Operand(0),
                arguments: Operand(0),
                is_synthetic,
            };
            let mut values = op::SuperCallWithArgumentArrayValues {
                dst: Value::EMPTY,
                super_constructor,
                arguments: Value::from_object(test_realm.array(arguments)),
            };
            assert_eq!(
                super_call_with_argument_array(&vm, PC, &instruction, &mut values),
                SlowPathControl::continue_at(PC + op::SuperCallWithArgumentArray::LENGTH)
            );
            assert!(environment.get_this_binding(&vm).must() == values.dst);
            // A second super() finds this already bound.
            let message = thrown_message(|| super_call_with_argument_array(&vm, PC, &instruction, &mut values));
            assert!(
                message.contains("creating a ReferenceError with the message \"|this| is already initialized\""),
                "{message}"
            );
            values.dst.as_object()
        };

        for is_synthetic in [false, true] {
            let instance = super_call(is_synthetic, base, &[int(2), int(3)]);
            assert_eq!(instance.get(&vm, &key("s")).must(), int(5));
            assert_eq!(instance.get(&vm, &key("d")).must(), int(50));
            assert!(instance.prototype() == Some(derived.get(&vm, &key("prototype")).must().as_object()));
        }

        let environment = new_function_environment(&vm, derived, Some(derived.upcast()));
        let frame = Frame::new(&vm, &test_realm, &[], Some(environment.upcast()));
        let mut values = op::SuperCallWithArgumentArrayValues {
            dst: Value::EMPTY,
            super_constructor: Value::NULL,
            arguments: Value::from_object(test_realm.array(&[])),
        };
        let instruction = op::SuperCallWithArgumentArray {
            header: header(true),
            dst: Operand(0),
            super_constructor: Operand(0),
            arguments: Operand(0),
            is_synthetic: false,
        };
        let message = thrown_message(|| super_call_with_argument_array(&vm, PC, &instruction, &mut values));
        assert!(
            message.contains(&type_error("Super constructor is not a constructor")),
            "{message}"
        );
        assert_eq!(running_execution_context(&vm).program_counter.get(), PC);

        let mut values = op::GetSuperConstructorValues { dst: Value::EMPTY };
        get_super_constructor(&vm, PC, &mut values);
        assert!(values.dst == base);
        drop(frame);
    }

    #[test]
    fn set_function_name_names_only_anonymous_functions() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(
            &vm,
            &test_realm,
            "var o = { f: null, g: null }; o.f = function () {}; o.g = function () {}; this.anonymous = o.f; this.anonymous2 = o.g; this.named = function named() {};",
        );
        let _frame = Frame::new(&vm, &test_realm, &[], None);
        let name_of = |value: Value| string_of(value.as_object().get(&vm, &key("name")).must());
        let set_name = |function: Value, name: Value, prefix: FunctionNamePrefix| {
            let instruction = op::SetFunctionName {
                header: header(false),
                function: Operand(0),
                name: Operand(0),
                prefix: prefix as u32,
            };
            let mut values = op::SetFunctionNameValues { function, name };
            assert_eq!(
                set_function_name(&vm, PC, &instruction, &mut values),
                SlowPathControl::continue_at(PC + op::SetFunctionName::LENGTH)
            );
        };

        let anonymous = global_value(&vm, &test_realm, "anonymous");
        assert_eq!(name_of(anonymous), "");
        set_name(anonymous, int(7), FunctionNamePrefix::Get);
        assert_eq!(name_of(anonymous), "get 7");
        set_name(anonymous, int(8), FunctionNamePrefix::None);
        assert_eq!(name_of(anonymous), "get 7");

        let anonymous2 = global_value(&vm, &test_realm, "anonymous2");
        let symbol = Symbol::create(&vm, Some(ak::Utf16String::from_utf8("desc")), Kind::Unique);
        set_name(anonymous2, Value::from_symbol(symbol), FunctionNamePrefix::Set);
        assert_eq!(name_of(anonymous2), "set [desc]");

        let named = global_value(&vm, &test_realm, "named");
        set_name(named, int(1), FunctionNamePrefix::None);
        assert_eq!(name_of(named), "named");
        set_name(int(3), int(1), FunctionNamePrefix::None);
    }

    #[test]
    fn functions_get_the_prototype_of_their_kind() {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        run_in(
            &vm,
            &test_realm,
            "function* g() {} async function af() {} async function* ag() {} this.g = g; this.af = af; this.ag = ag; this.e = function () {}; this.o = { m() {} };",
        );
        let realm = test_realm.realm;
        let prototype_of = |name: &str| global_value(&vm, &test_realm, name).as_object().prototype();
        assert!(prototype_of("g") == Some(realm.generator_function_prototype()));
        assert!(prototype_of("af") == Some(realm.async_function_prototype()));
        assert!(prototype_of("ag") == Some(realm.async_generator_function_prototype()));
        assert!(prototype_of("e") == Some(realm.function_prototype()));

        let holder = global_value(&vm, &test_realm, "o").as_object();
        let method = value_as_ecmascript_function_object(holder.get(&vm, &key("m")).must())
            .expect("the method is an ECMAScript function");
        assert!(method.home_object() == Some(holder));
        assert!(!Value::from_object(method).is_constructor());
    }

    /// Runs a script that recurses until it exceeds the call stack size limit, and returns what it threw as
    /// "name: message".
    fn recurse_until_the_call_stack_size_limit(source: &str) -> String {
        let vm = Vm::create();
        let test_realm = realm_with_global_object(&vm);
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse(&vm, &code_units, test_realm.realm).expect("the script parses");
        let thrown = vm.run_script(script, None).err().expect("the script throws").value();
        let error = thrown.as_object();
        let name = error.get(&vm, &key("name")).must();
        let message = error.get(&vm, &key("message")).must();
        format!("{}: {}", string_of(name), string_of(message))
    }

    #[test]
    fn recursion_without_end_in_frames_the_interpreter_enters_itself() {
        assert_eq!(
            recurse_until_the_call_stack_size_limit("function f() { return f(); } f()"),
            "InternalError: Call stack size limit exceeded"
        );
    }

    #[test]
    fn recursion_without_end_through_the_construct_slow_path() {
        assert_eq!(
            recurse_until_the_call_stack_size_limit("class N { x = 1; constructor() { new N(); } } new N()"),
            "InternalError: Call stack size limit exceeded"
        );
    }
}
