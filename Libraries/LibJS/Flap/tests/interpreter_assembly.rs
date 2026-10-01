/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use flapc::{Architecture, CompilationUnit, CompileOptions, Compiler, ObjectFormat, SourceInput, Target};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const INTERPRETER: &str = include_str!("../../Interpreter/interpreter.flap");
const LAYOUT: &str = include_str!("interpreter-layout.conf");

fn interpreter_compiler(architecture: Architecture) -> Compiler {
    Compiler::new(CompileOptions {
        target: Target {
            architecture,
            object_format: ObjectFormat::Elf,
        },
        has_jscvt: false,
        enable_assertions: true,
    })
}

fn interpreter_unit() -> CompilationUnit<'static> {
    CompilationUnit {
        source: SourceInput {
            name: "interpreter.flap",
            contents: INTERPRETER,
        },
        constants: Some(SourceInput {
            name: "interpreter-layout.conf",
            contents: LAYOUT,
        }),
    }
}

fn compile_interpreter(architecture: Architecture) -> String {
    interpreter_compiler(architecture)
        .compile(interpreter_unit())
        .expect("interpreter compilation should succeed")
        .as_str()
        .to_owned()
}

fn external_symbols(assembly: &str) -> BTreeSet<&str> {
    let mut referenced = BTreeSet::new();
    let mut defined = BTreeSet::new();
    for line in assembly.lines().filter(|line| !line.starts_with('#')) {
        let mut rest = line;
        while let Some(start) = rest.find("CSYM(") {
            rest = &rest[start + "CSYM(".len()..];
            let end = rest.find(')').expect("CSYM reference should be closed");
            let symbol = &rest[..end];
            rest = &rest[end + 1..];
            if rest.starts_with(':') {
                defined.insert(symbol);
            } else {
                referenced.insert(symbol);
            }
        }
    }
    referenced.difference(&defined).copied().collect()
}

fn snapshot_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("interpreter-assembly")
        .join(file_name)
}

fn compare_or_rebaseline(file_name: &str, assembly: &str) {
    let path = snapshot_path(file_name);
    if std::env::var_os("FLAP_REBASELINE").is_some() {
        fs::write(&path, assembly).unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
        return;
    }

    let expected =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    assert!(
        assembly == expected,
        "{} is out of date; rebaseline with FLAP_REBASELINE=1 cargo test --test interpreter_assembly",
        path.display()
    );
}

#[test]
fn interpreter_assembly_matches_snapshots() {
    compare_or_rebaseline("x86_64.S", &compile_interpreter(Architecture::X86_64));
    compare_or_rebaseline("aarch64.S", &compile_interpreter(Architecture::Aarch64));
}

#[test]
fn keeps_helper_setup_out_of_the_hot_handler_region() {
    for architecture in [Architecture::X86_64, Architecture::Aarch64] {
        let assembly = compile_interpreter(architecture);
        let cold_start = assembly.find("asm_cold_handler_paths:").unwrap();
        for handler in ["Exp", "ExpRhsInt32"] {
            assert!(assembly.find(&format!("asm_handler_{handler}:")).unwrap() > cold_start);
        }
    }
}

#[test]
fn keeps_assertion_traps_after_all_hot_and_cold_handlers() {
    for (architecture, trap) in [(Architecture::X86_64, "    ud2"), (Architecture::Aarch64, "    brk")] {
        let assembly = compile_interpreter(architecture);
        let (hot, cold) = assembly.split_once("asm_cold_handler_paths:").unwrap();
        assert!(!hot.contains(trap));
        let (cold, traps) = cold.split_once("asm_assertion_failure_traps:").unwrap();
        assert!(!cold.contains(trap));
        assert!(traps.contains(trap));
        assert!(!traps.contains("asm_handler_"));
        assert!(hot.contains("assert_failure"));
        assert!(!assembly.contains("assert_ok"));
    }
}

#[test]
fn reports_every_runtime_function_the_assembly_calls() {
    for architecture in [Architecture::X86_64, Architecture::Aarch64] {
        let compiler = interpreter_compiler(architecture);
        let prepared = compiler
            .prepare(interpreter_unit())
            .expect("interpreter preparation should succeed");
        let assembly = compiler
            .compile_prepared(&prepared)
            .expect("interpreter compilation should succeed");
        let runtime_functions = compiler
            .runtime_functions(&prepared)
            .expect("interpreter runtime functions should be consistent");
        let reported = runtime_functions
            .iter()
            .map(|function| function.symbol.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(reported.len(), runtime_functions.len());
        assert_eq!(external_symbols(assembly.as_str()), reported);
        assert_eq!(reported.len(), 182);
    }
}
