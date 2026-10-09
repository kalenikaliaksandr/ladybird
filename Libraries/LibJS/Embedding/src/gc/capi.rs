/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Declarations of the parts of Libraries/LibGC/CAPI.h that the crate uses.

#[derive(Default)]
#[repr(C)]
pub struct GCLayout {
    pub cell_mark_offset: u32,
    pub cell_state_offset: u32,
    pub cell_kind_offset: u32,
    pub min_cell_size: u32,
    pub max_cell_size: u32,
    pub max_cell_alignment: u32,
    pub cell_type_info_size: u32,
    pub weak_impl_pointer_offset: u32,
    pub free_cell_next_offset: u32,
    pub free_cell_link_mask: u32,
    pub heap_allocated_bytes_since_last_gc_offset: u32,
    pub heap_gc_bytes_threshold_offset: u32,
    pub heap_total_allocated_bytes_offset: u32,
    pub heap_region_offset_mask: u64,
    pub primitive_storage_cage_offset_mask: u64,
}

unsafe extern "C" {
    pub fn gc_get_layout(layout: *mut GCLayout);
}

// On Windows, a variable that a DLL exports is only reachable through the DLL's import table, which the reference has
// to go through.
#[cfg_attr(windows, link(name = "lagom-gc", kind = "dylib"))]
unsafe extern "C" {
    static js_heap_region_base: usize;
}

/// The base NaN-boxed cell values are relative to.
#[inline(always)]
pub fn heap_region_base() -> usize {
    // SAFETY: LibGC fixes the base before any cell exists, and never changes it after.
    unsafe { js_heap_region_base }
}

/// Definitions of the functions as ones that must never run, for test binaries that do not link LibGC.
#[cfg(any(test, feature = "test-stubs"))]
mod stubs {
    #![allow(clippy::missing_safety_doc, reason = "the stubs never run")]

    use super::GCLayout;

    #[unsafe(no_mangle)]
    pub static js_heap_region_base: usize = 0;

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_get_layout(_: *mut GCLayout) {
        unreachable!("gc_get_layout is not available without LibGC");
    }
}
