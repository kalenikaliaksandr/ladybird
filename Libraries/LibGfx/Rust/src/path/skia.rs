/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The path query that Skia still answers. LibGfx makes a Skia path from the geometry for it.

use super::Path;
use std::ffi::c_void;

unsafe extern "C" {
    fn ladybird_gfx_skia_path_append_svg_string(
        path: *const c_void,
        append: unsafe extern "C" fn(*mut c_void, *const u8, usize),
        context: *mut c_void,
    );
}

fn raw(path: &Path) -> *const c_void {
    std::ptr::from_ref(path).cast()
}

unsafe extern "C" fn append_to_string(context: *mut c_void, bytes: *const u8, length: usize) {
    if length == 0 {
        return;
    }
    // SAFETY: `context` is the String that to_svg_string() passes, and the bytes are live UTF-8 for
    // this synchronous call.
    unsafe {
        let output = &mut *context.cast::<String>();
        output.push_str(&String::from_utf8_lossy(ak::slice_from_raw_parts(bytes, length)));
    }
}

impl Path {
    /// The path as SVG path data with absolute commands. Conics become quads.
    pub fn to_svg_string(&self) -> String {
        let mut output = String::new();
        // SAFETY: The path is live for the call, and the callback only writes into the String whose
        // pointer it receives.
        unsafe { ladybird_gfx_skia_path_append_svg_string(raw(self), append_to_string, (&raw mut output).cast()) };
        output
    }
}
