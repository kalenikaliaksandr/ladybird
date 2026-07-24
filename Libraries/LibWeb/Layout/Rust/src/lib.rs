/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[cfg(feature = "allocator")]
#[path = "../../../../RustAllocator.rs"]
mod rust_allocator;

mod css_pixels;

// The shared source also contains the CSS crate's exported arithmetic parity
// hooks. Including it in the production static library would define those
// symbols twice when LibWeb links both Rust crates, so use it here to pin the
// layout ABI wrapper in tests without copying its implementation.
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
#[path = "../../../CSS/Rust/src/css_pixels.rs"]
mod shared_css_pixels;

mod ffi_stats;
mod geometry;
mod layout_state;
mod tree_builder;
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
