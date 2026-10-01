/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The runtime functions generated interpreter code calls, and how it calls
//! them.
//!
//! A runtime that links the generated assembly defines every function that
//! [`Compiler::runtime_functions`](crate::Compiler::runtime_functions) reports
//! with the C signature documented on its [`RuntimeFunctionKind`]. In those
//! signatures `VM*` is the pointer the interpreter was entered with, `pc` is
//! the bytecode offset of the current instruction, and `Value` is a NaN-boxed
//! `u64`. Slow paths return the control word described in `SlowPaths.cpp`.

use crate::intrinsic::CallOperation;
use crate::metadata::{SlowPathAbi, SlowPathLayout};
use crate::ssa::{Constant, Intrinsic, Operation, ValueDefinition};
use crate::{Architecture, CompileError, CompileStage, ObjectFormat, PreparedProgram, Target};
use std::collections::BTreeMap;

pub(crate) const FALLBACK_HANDLER: &str = "asm_fallback_handler";
pub(crate) const BREAKPOINT_CHECK: &str = "asm_debugger_check_breakpoint";
pub(crate) const STACK_OVERFLOW_SLOW_PATH: &str = "asm_slow_path_stack_overflow";

/// An external function the generated assembly calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub symbol: String,
    pub kind: RuntimeFunctionKind,
}

/// How the generated assembly calls a runtime function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeFunctionKind {
    /// A `call_slow_path` target of the handler for `op`, with the signature
    /// `abi` documents. `layout` describes `Op::{op}::Values`.
    SlowPath {
        op: String,
        abi: SlowPathAbi,
        layout: SlowPathLayout,
    },
    /// A `call_interp` target of the handler for `op`:
    /// `i64 f(VM*, u32 pc, Op::{op} const*, Op::{op}::Values& values)`.
    /// It returns zero after handling the instruction and storing its outputs
    /// in `values`, and nonzero to leave the instruction to the handler.
    Try { op: String, layout: SlowPathLayout },
    /// A `call_binary_slow_path` target:
    /// `i64 f(VM*, u32 pc, Value& dst, Value lhs, Value rhs)`.
    BinarySlowPath,
    /// A `call_jump_slow_path` target:
    /// `i64 f(VM*, u32 pc, Value lhs, Value rhs, u32 true_target, u32 false_target)`.
    JumpSlowPath,
    /// A `call_helper` target: `u64 f(u64)`. The handler decides what the
    /// argument and result words hold, such as an encoded `Value` or a `VM*`.
    Helper,
    /// A `call_helper_with_two_arguments` target: `u64 f(u64, u64)`.
    HelperWithTwoArguments,
    /// `i64 asm_fallback_handler(VM*, u32 pc, u8 const* instruction)`, which
    /// runs opcodes that have no handler.
    FallbackHandler,
    /// `void asm_debugger_check_breakpoint(VM*, u32 pc)`, which runs before
    /// each instruction while a debugger is attached.
    BreakpointCheck,
    /// `i64 asm_slow_path_stack_overflow(VM*, u32 pc)`, which throws when the
    /// `Op::Values` record of a variable-length operation does not fit on the
    /// stack.
    StackOverflowSlowPath,
}

impl RuntimeFunctionKind {
    fn description(&self) -> String {
        match self {
            Self::SlowPath { op, .. } => format!("a slow path of {op}"),
            Self::Try { op, .. } => format!("a try call of {op}"),
            Self::BinarySlowPath => "a binary slow path".to_string(),
            Self::JumpSlowPath => "a jump slow path".to_string(),
            Self::Helper => "a one-argument helper".to_string(),
            Self::HelperWithTwoArguments => "a two-argument helper".to_string(),
            Self::FallbackHandler => "the fallback handler".to_string(),
            Self::BreakpointCheck => "the breakpoint check".to_string(),
            Self::StackOverflowSlowPath => "the stack-overflow slow path".to_string(),
        }
    }
}

/// Where a `call_raw_native` call finds the two words of the
/// `ThrowCompletionOr<Value>` its native function returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawNativeReturnConvention {
    /// The function takes the `VM&` and returns both words in the first two
    /// integer return registers.
    Registers,
    /// The function takes a pointer to a 16-byte result, then the `VM&`.
    OutPointer,
}

/// How `call_raw_native` receives the result of a native function on `target`.
pub fn raw_native_return_convention(target: Target) -> RawNativeReturnConvention {
    match (target.architecture, target.object_format) {
        (_, ObjectFormat::Coff) | (Architecture::X86_64, ObjectFormat::MachO) => RawNativeReturnConvention::OutPointer,
        (Architecture::X86_64, ObjectFormat::Elf)
        | (Architecture::Aarch64, ObjectFormat::Elf | ObjectFormat::MachO) => RawNativeReturnConvention::Registers,
    }
}

pub(crate) fn runtime_functions(
    prepared: &PreparedProgram,
    target: Target,
) -> Result<Vec<RuntimeFunction>, CompileError> {
    let record_form_only = target.object_format == ObjectFormat::Coff;
    let mut functions = BTreeMap::<String, RuntimeFunctionKind>::new();
    let mut add = |symbol: &str, kind: RuntimeFunctionKind, handler: Option<&str>| {
        if let Some(previous) = functions.get(symbol) {
            if *previous == kind {
                return Ok(());
            }
            return Err(CompileError::new(
                CompileStage::Semantic,
                handler,
                format!(
                    "runtime function '{symbol}' is called as both {} and {}",
                    previous.description(),
                    kind.description()
                ),
            ));
        }
        functions.insert(symbol.to_string(), kind);
        Ok(())
    };

    add(FALLBACK_HANDLER, RuntimeFunctionKind::FallbackHandler, None)?;
    add(BREAKPOINT_CHECK, RuntimeFunctionKind::BreakpointCheck, None)?;
    for (handler, handler_layout) in prepared.handlers.iter().zip(&prepared.bytecode.handler_layouts) {
        let function = &handler.function;
        let op_layout = || {
            handler_layout.slow_path.clone().ok_or_else(|| {
                CompileError::new(
                    CompileStage::Semantic,
                    Some(handler.name()),
                    "handler without a bytecode layout calls an operation's runtime function",
                )
            })
        };
        for instruction in function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .map(|instruction| &function.instructions[instruction.0])
        {
            let Operation::Intrinsic(Intrinsic::Call(call)) = instruction.operation else {
                continue;
            };
            let kind = match call {
                CallOperation::SlowPath => {
                    let layout = op_layout()?;
                    if layout.array.is_some() {
                        add(
                            STACK_OVERFLOW_SLOW_PATH,
                            RuntimeFunctionKind::StackOverflowSlowPath,
                            Some(handler.name()),
                        )?;
                    }
                    RuntimeFunctionKind::SlowPath {
                        op: handler.name().to_string(),
                        abi: layout.abi(record_form_only),
                        layout,
                    }
                }
                CallOperation::Interpreter => RuntimeFunctionKind::Try {
                    op: handler.name().to_string(),
                    layout: op_layout()?,
                },
                CallOperation::BinarySlowPath => RuntimeFunctionKind::BinarySlowPath,
                CallOperation::JumpSlowPath => RuntimeFunctionKind::JumpSlowPath,
                CallOperation::Helper => RuntimeFunctionKind::Helper,
                CallOperation::HelperWithTwoArguments => RuntimeFunctionKind::HelperWithTwoArguments,
                CallOperation::RawNative => continue,
            };
            let ValueDefinition::Constant(Constant::SlowPath(symbol) | Constant::FunctionSymbol(symbol)) =
                &function.values[instruction.inputs[0].0].definition
            else {
                return Err(CompileError::new(
                    CompileStage::Semantic,
                    Some(handler.name()),
                    format!("'{}' target is not a link-time symbol", call.name()),
                ));
            };
            add(symbol.as_str(), kind, Some(handler.name()))?;
        }
    }

    Ok(functions
        .into_iter()
        .map(|(symbol, kind)| RuntimeFunction { symbol, kind })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompilationUnit, CompileOptions, Compiler, SourceInput};

    fn runtime_functions_for(object_format: ObjectFormat, source: &str) -> Result<Vec<RuntimeFunction>, CompileError> {
        let compiler = Compiler::new(CompileOptions {
            target: Target {
                architecture: Architecture::X86_64,
                object_format,
            },
            has_jscvt: false,
            enable_assertions: false,
        });
        let prepared = compiler.prepare(CompilationUnit {
            source: SourceInput {
                name: "test.flap",
                contents: source,
            },
            constants: None,
        })?;
        compiler.runtime_functions(&prepared)
    }

    fn summary(function: &RuntimeFunction) -> String {
        match &function.kind {
            RuntimeFunctionKind::SlowPath { op, abi, .. } => {
                format!("{} slow path of {op} as {abi:?}", function.symbol)
            }
            RuntimeFunctionKind::Try { op, .. } => format!("{} try call of {op}", function.symbol),
            kind => format!("{} {kind:?}", function.symbol),
        }
    }

    const EVERY_KIND_OF_CALL: &str = r#"
handler Get(dst: out Operand, base: in Operand) = call_slow_path(get);
handler Call(length: u32, dst: out Operand, callee: in Operand, argument_count: u32, arguments: Operand[]) = call_slow_path(call);
handler Put(base: in Operand, value: in Operand) {
    guard call_interp(asm_try_put) == 0 else slow;
    dispatch_next;

    let slow = || @cold {
        call_slow_path(put);
    };
}
handler Add(dst: out Operand, lhs: in Operand, rhs: in Operand) {
    call_binary_slow_path(add_values, dst, load(lhs), load(rhs));
}
handler AddLhsInt32(dst: out Operand, lhs: in Operand, rhs: in Operand) {
    call_binary_slow_path(add_values, dst, load(lhs), load(rhs));
}
handler JumpLessThan(lhs: in Operand, rhs: in Operand, true_target: BytecodeOffset, false_target: BytecodeOffset) {
    call_jump_slow_path(jump_less_than_values, load(lhs), load(rhs), true_target, false_target);
}
handler Not(dst: out Operand, src: in Operand) {
    let truthy: u64 = call_helper(asm_helper_to_boolean, load(src));
    let result: Value = call_helper_with_two_arguments(asm_helper_box_boolean, load(src), truthy);
    store(dst, result);
    dispatch_next;
}
"#;

    #[test]
    fn reports_every_kind_of_runtime_call_once_per_symbol() {
        let functions = runtime_functions_for(ObjectFormat::Elf, EVERY_KIND_OF_CALL).unwrap();

        assert_eq!(
            functions.iter().map(summary).collect::<Vec<_>>(),
            [
                "asm_debugger_check_breakpoint BreakpointCheck",
                "asm_fallback_handler FallbackHandler",
                "asm_helper_box_boolean HelperWithTwoArguments",
                "asm_helper_to_boolean Helper",
                "asm_slow_path_add_values BinarySlowPath",
                "asm_slow_path_call slow path of Call as Record",
                "asm_slow_path_get slow path of Get as Scalar",
                "asm_slow_path_jump_less_than_values JumpSlowPath",
                "asm_slow_path_put slow path of Put as Scalar",
                "asm_slow_path_stack_overflow StackOverflowSlowPath",
                "asm_try_put try call of Put",
            ]
        );
        let RuntimeFunctionKind::SlowPath { layout, .. } = &functions[5].kind else {
            unreachable!();
        };
        assert_eq!(
            layout
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["m_dst", "m_callee"]
        );
        assert_eq!(layout.array.as_ref().unwrap().name, "m_arguments");
    }

    #[test]
    fn passes_every_slow_path_a_record_on_coff() {
        let functions = runtime_functions_for(ObjectFormat::Coff, EVERY_KIND_OF_CALL).unwrap();

        assert_eq!(
            functions
                .iter()
                .filter_map(|function| match &function.kind {
                    RuntimeFunctionKind::SlowPath { abi, .. } => Some(*abi),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [SlowPathAbi::Record; 3]
        );
    }

    #[test]
    fn rejects_one_symbol_reached_as_two_runtime_functions() {
        let error = runtime_functions_for(
            ObjectFormat::Elf,
            r#"
handler First(src: in Operand) = call_slow_path(shared);
handler Second(src: in Operand) = call_slow_path(shared);
"#,
        )
        .unwrap_err();

        assert_eq!(error.stage, CompileStage::Semantic);
        assert_eq!(error.handler.as_deref(), Some("Second"));
        assert_eq!(
            error.message,
            "runtime function 'asm_slow_path_shared' is called as both a slow path of First and a slow path of Second"
        );
    }

    #[test]
    fn returns_raw_native_results_through_memory_on_coff_and_x86_64_mach_o() {
        for (architecture, object_format, convention) in [
            (
                Architecture::X86_64,
                ObjectFormat::Elf,
                RawNativeReturnConvention::Registers,
            ),
            (
                Architecture::X86_64,
                ObjectFormat::MachO,
                RawNativeReturnConvention::OutPointer,
            ),
            (
                Architecture::X86_64,
                ObjectFormat::Coff,
                RawNativeReturnConvention::OutPointer,
            ),
            (
                Architecture::Aarch64,
                ObjectFormat::Elf,
                RawNativeReturnConvention::Registers,
            ),
            (
                Architecture::Aarch64,
                ObjectFormat::MachO,
                RawNativeReturnConvention::Registers,
            ),
            (
                Architecture::Aarch64,
                ObjectFormat::Coff,
                RawNativeReturnConvention::OutPointer,
            ),
        ] {
            assert_eq!(
                raw_native_return_convention(Target {
                    architecture,
                    object_format
                }),
                convention
            );
        }
    }
}
