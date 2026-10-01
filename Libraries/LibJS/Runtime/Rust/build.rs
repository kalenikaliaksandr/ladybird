/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Builds everything the crate shares with the interpreter: the layout file flapc compiles interpreter.flap against,
//! computed from the crate's own layout module, the interpreter itself, assembled into the crate's static library,
//! and the static assertions that keep the crate's types in line with that layout.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// The build script only reads the layout, so most of what the crate uses goes unused here.
#[allow(dead_code)]
#[path = "src/layout/mod.rs"]
mod layout;

#[path = "generate/layout_forward.rs"]
mod layout_forward;

#[path = "generate/fixture.rs"]
mod fixture;

#[path = "generate/layout_table.rs"]
mod layout_table;

const KIB: u64 = 1024;
const GIB: u64 = 1024 * 1024 * KIB;
const TIB: u64 = 1024 * GIB;

struct Target {
    architecture: flapc::Architecture,
    object_format: flapc::ObjectFormat,
    is_apple: bool,
}

fn target() -> Target {
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    let pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").expect("CARGO_CFG_TARGET_POINTER_WIDTH");
    let endian = env::var("CARGO_CFG_TARGET_ENDIAN").expect("CARGO_CFG_TARGET_ENDIAN");
    assert!(
        pointer_width == "64" && endian == "little",
        "the interpreter supports 64-bit little-endian targets only"
    );
    // The layout is computed on the build machine, so it has to lay out types the way the target does.
    const {
        assert!(
            cfg!(target_pointer_width = "64") && cfg!(target_endian = "little"),
            "the build machine must be 64-bit little-endian as well"
        );
    }

    let architecture = match architecture.as_str() {
        "x86_64" => flapc::Architecture::X86_64,
        "aarch64" => flapc::Architecture::Aarch64,
        other => panic!("the interpreter does not support {other}"),
    };
    let is_apple = os == "macos" || os == "ios";
    let object_format = match os.as_str() {
        _ if is_apple => flapc::ObjectFormat::MachO,
        "windows" => flapc::ObjectFormat::Coff,
        _ => flapc::ObjectFormat::Elf,
    };
    Target {
        architecture,
        object_format,
        is_apple,
    }
}

/// Mirrors GC::HEAP_REGION_SIZE in Libraries/LibGC/HeapRegion.h, whose choice depends on the target and on whether
/// LibGC is built with ThreadSanitizer, which Rust's own cfg cannot see.
fn heap_region_size(target: &Target, thread_sanitizer: bool) -> u64 {
    let os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    let address_space_is_small = matches!(target.architecture, flapc::Architecture::Aarch64) && os != "macos";
    if address_space_is_small || thread_sanitizer {
        128 * GIB
    } else {
        4 * TIB
    }
}

fn main() {
    let manifest_directory = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let output_directory = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let libjs_directory = manifest_directory.join("../..");
    let interpreter_source = libjs_directory.join("Interpreter/interpreter.flap");
    let layout_fixture = libjs_directory.join("Flap/tests/interpreter-layout.conf");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=generate");
    println!("cargo:rerun-if-changed=src/layout");
    println!("cargo:rerun-if-changed={}", interpreter_source.display());
    println!("cargo:rerun-if-changed={}", layout_fixture.display());

    let target = target();
    let address_sanitizer = env::var_os("CARGO_FEATURE_ADDRESS_SANITIZER").is_some();
    let thread_sanitizer = env::var_os("CARGO_FEATURE_THREAD_SANITIZER").is_some();
    let configuration = layout_table::LayoutConfiguration {
        heap_region_offset_mask: heap_region_size(&target, thread_sanitizer) - 1,
        // Mirrors GC::PrimitiveStorage::default_cage_size.
        primitive_storage_cage_offset_mask: 4 * TIB - 1,
        // Mirrors the C++ generator: instrumented frames need more room before the stack is considered exhausted.
        vm_stack_space_limit: if address_sanitizer { 96 * KIB } else { 32 * KIB },
    };
    println!(
        "cargo:rustc-env=LIBJS_RUNTIME_HEAP_REGION_OFFSET_MASK={}",
        configuration.heap_region_offset_mask
    );
    println!(
        "cargo:rustc-env=LIBJS_RUNTIME_PRIMITIVE_STORAGE_CAGE_OFFSET_MASK={}",
        configuration.primitive_storage_cage_offset_mask
    );
    println!(
        "cargo:rustc-env=LIBJS_RUNTIME_VM_STACK_SPACE_LIMIT={}",
        configuration.vm_stack_space_limit
    );

    let layout = layout_table::generate(&configuration);
    let generated = fixture::parse(&layout.text);
    let fixture_text = fs::read_to_string(&layout_fixture).expect("read the interpreter layout fixture");
    if let Err(problems) = fixture::check_against_fixture(&generated, &fixture::parse(&fixture_text)) {
        panic!("the Rust interpreter layout does not match the one the interpreter expects:\n{problems}");
    }
    write_if_changed(&output_directory.join("layout.conf"), &layout.text);
    write_if_changed(
        &output_directory.join("layout_static_assertions.rs"),
        &layout.static_assertions,
    );

    let interpreter_source_text = fs::read_to_string(&interpreter_source).expect("read interpreter.flap");
    let assembly = compile_interpreter(&target, &interpreter_source, &interpreter_source_text, &layout.text);
    let assembly_path = output_directory.join("interpreter.S");
    write_if_changed(&assembly_path, &assembly);
    assemble_interpreter(&target, &assembly_path);
}

fn compile_interpreter(target: &Target, source_path: &Path, source: &str, layout: &str) -> String {
    let compiler = flapc::Compiler::new(flapc::CompileOptions {
        target: flapc::Target {
            architecture: target.architecture,
            object_format: target.object_format,
        },
        has_jscvt: target.is_apple && matches!(target.architecture, flapc::Architecture::Aarch64),
        enable_assertions: true,
    });
    let unit = flapc::CompilationUnit {
        source: flapc::SourceInput {
            name: &source_path.display().to_string(),
            contents: source,
        },
        constants: Some(flapc::SourceInput {
            name: "layout.conf",
            contents: layout,
        }),
    };
    match compiler.compile(unit) {
        Ok(assembly) => assembly.as_str().to_string(),
        Err(error) => panic!("flapc failed to compile the interpreter:\n{error}"),
    }
}

fn assemble_interpreter(target: &Target, assembly_path: &Path) {
    let mut build = cc::Build::new();
    build.file(assembly_path);
    if matches!(target.architecture, flapc::Architecture::X86_64) {
        build.flag_if_supported("-malign-branch-boundary=32");
        build.flag_if_supported("-malign-branch=fused,jcc");
    }
    build.compile("libjs_runtime_interpreter");
}

fn write_if_changed(path: &Path, contents: &str) {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return;
    }
    fs::write(path, contents).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}
