/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Stand-ins for the LibGfx entry points, which the unit tests link without the C++ runtime.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

#[unsafe(no_mangle)]
extern "C" fn ladybird_gfx_skia_path_append_svg_string(
    _path: *const c_void,
    _append: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    _context: *mut c_void,
) {
}

// One counter stands in for each of LibGfx's process-wide counters: their values only need to be unique.
static NEXT: AtomicU64 = AtomicU64::new(1);

#[unsafe(no_mangle)]
extern "C" fn ladybird_gfx_process_next_path_identity() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_gfx_process_next_structural_epoch() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}
