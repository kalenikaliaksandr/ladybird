/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The size of LibGC's heap region, which decides the mask that turns a NaN-boxed cell into the cell's offset in the
//! region.

use std::env;

const GIB: u64 = 1024 * 1024 * 1024;
const TIB: u64 = 1024 * GIB;

/// Mirrors GC::HEAP_REGION_SIZE in Libraries/LibGC/HeapRegion.h, whose choice depends on the target and on whether
/// LibGC is built with ThreadSanitizer, which Rust's own cfg cannot see.
pub fn heap_region_size(thread_sanitizer: bool) -> u64 {
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    let address_space_is_small = architecture == "aarch64" && os != "macos";
    if address_space_is_small || thread_sanitizer {
        128 * GIB
    } else {
        4 * TIB
    }
}
