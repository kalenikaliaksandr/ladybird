/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Unconditional in every build flavor: the HTML tokenizer transfers allocation
// ownership across the FFI boundary, so the crate-global allocator must stay
// the Ladybird allocator for C++-side frees to stay balanced.
#[path = "../../../RustAllocator.rs"]
mod rust_allocator;

mod encoding_detection;

pub mod css;
pub mod layout;
pub mod painting;

pub use libweb_html_tokenizer as html_tokenizer;

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

unsafe fn bytes_from_raw<'a>(bytes: *const u8, len: usize) -> Option<&'a [u8]> {
    unsafe {
        if len == 0 {
            return Some(&[]);
        }
        if bytes.is_null() {
            eprintln!("bytes_from_raw: null pointer with non-zero length {len}");
            return None;
        }
        Some(std::slice::from_raw_parts(bytes, len))
    }
}

#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn ladybird_gfx_font_unref(_font: *mut std::ffi::c_void) {}

#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn ladybird_gfx_filter_destroy(_filter: *mut std::ffi::c_void) {}

#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn ladybird_gfx_filter_serialized_summary(
    _filter: *const std::ffi::c_void,
    _out_size: *mut usize,
    _out_hash: *mut u64,
) {
}

#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn ladybird_web_svg_path_from_path_data_ascii(
    _bytes: *const u8,
    _length: usize,
) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}

#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn ladybird_web_svg_path_from_path_data_utf16(
    _units: *const u16,
    _length: usize,
) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}
