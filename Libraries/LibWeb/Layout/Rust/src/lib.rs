/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[cfg(feature = "allocator")]
#[path = "../../../../RustAllocator.rs"]
mod rust_allocator;

#[allow(dead_code)]
mod box_facts;
#[allow(dead_code)]
mod css_enums;
#[allow(dead_code)]
mod css_pixels;
#[path = "../../../CSS/Rust/src/display.rs"]
#[allow(dead_code)]
mod display;

#[allow(dead_code)]
mod ffi_stats;
#[allow(dead_code)]
mod geometry;
mod layout_node_arena;
pub mod node_data;
#[allow(dead_code)]
mod style_facts;
#[cfg(test)]
mod tests;
mod tree_builder;
#[allow(dead_code)]
mod used_values;

use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;

fn abort_on_panic<F: FnOnce() -> R, R>(f: F) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_string()
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else {
                "unknown panic".to_string()
            };
            eprintln!("Rust panic at FFI boundary: {message}");
            std::process::abort();
        }
    }
}
