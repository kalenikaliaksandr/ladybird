/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Generates the Rust declarations of LibJS's embedding ABI and the constants of LibJS/Embedding/Layout.h from the
//! sources of libjs_rust, with the same generators that libjs_rust's build script writes the C headers with.

use std::env;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

// The build script only reads the layout, so most of what the runtime uses goes unused here.
#[allow(dead_code)]
#[path = "../Rust/src/layout/mod.rs"]
mod layout;

#[path = "../Rust/generate/layout_forward.rs"]
mod layout_forward;

#[path = "../Rust/src/gc/class_id.rs"]
mod class_id;

#[path = "../Rust/generate/abi_rust.rs"]
mod abi_rust;

#[path = "../Rust/generate/c_type_spelling.rs"]
mod c_type_spelling;

#[allow(dead_code)]
#[path = "../Rust/generate/embedding_items.rs"]
mod embedding_items;

#[path = "../Rust/generate/heap_region.rs"]
mod heap_region;

#[path = "../Rust/generate/layout_header.rs"]
mod layout_header;

fn main() {
    let manifest_directory = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let output_directory = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let runtime_directory = manifest_directory.join("../Rust");

    println!("cargo:rerun-if-changed=build.rs");
    for input in [
        "generate",
        "src/layout",
        "src/gc/class_id.rs",
        "src/gc/capi.rs",
        "src/embedding",
    ] {
        println!("cargo:rerun-if-changed={}", runtime_directory.join(input).display());
    }

    let thread_sanitizer = env::var_os("CARGO_FEATURE_THREAD_SANITIZER").is_some();

    // The sources that the build script reads as data rather than compiles, which cargo's dep-info would otherwise
    // only name by their directory.
    for source in embedding_items::sources(&runtime_directory) {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let items = embedding_items::read(&runtime_directory);
    let abi = abi_rust::generate(&items);
    write_if_changed(&output_directory.join("abi.rs"), &abi.declarations);
    write_if_changed(&output_directory.join("abi_stubs.rs"), &abi.stubs);

    let mut layout = layout_header::generate().rust_constants;
    let _ = writeln!(
        layout,
        "pub const HEAP_REGION_OFFSET_MASK: u64 = {};",
        heap_region::heap_region_size(thread_sanitizer) - 1
    );
    write_if_changed(&output_directory.join("layout.rs"), &layout);
}

fn write_if_changed(path: &Path, contents: &str) {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return;
    }
    fs::write(path, contents).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}
