/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Declarations of the parts of Libraries/LibGC/CAPI.h that the crate uses.

use core::ffi::{c_char, c_void};

use crate::abi::{GCHeap, GCVisitor};

#[repr(C)]
pub struct GCAllocator {
    _opaque: [u8; 0],
}

pub const GC_CELL_KIND_OTHER: u8 = 0;
pub const GC_CELL_STATE_LIVE: u8 = 0;

#[repr(C)]
pub struct GCCellTypeInfo {
    pub cell_size: u32,
    pub alignment: u32,
    pub kind: u8,
    pub visit_edges: Option<unsafe extern "C" fn(cell: *mut c_void, visitor: *mut GCVisitor)>,
    pub finalize: Option<unsafe extern "C" fn(cell: *mut c_void)>,
    pub destroy: Option<unsafe extern "C" fn(cell: *mut c_void)>,
    pub external_memory_size: Option<unsafe extern "C" fn(cell: *const c_void) -> usize>,
    pub class_name: Option<unsafe extern "C" fn(cell: *const c_void, length: *mut usize) -> *const c_char>,
}

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
    pub fn gc_allocator_create(
        type_info: *const GCCellTypeInfo,
        name: *const c_char,
        name_length: usize,
    ) -> *mut GCAllocator;
    pub fn gc_heap_allocate_cell(heap: *mut GCHeap, allocator: *mut GCAllocator, must_mark: *mut bool) -> *mut c_void;
    pub fn gc_visitor_visit_cell(visitor: *mut GCVisitor, cell: *mut c_void);
    pub fn gc_visitor_visit_values(visitor: *mut GCVisitor, values: *const u64, count: usize);
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

    use core::ffi::{c_char, c_void};

    use super::{GCAllocator, GCCellTypeInfo, GCLayout};
    use crate::abi::{GCHeap, GCVisitor};

    #[unsafe(no_mangle)]
    pub static js_heap_region_base: usize = 0;

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_get_layout(_: *mut GCLayout) {
        unreachable!("gc_get_layout is not available without LibGC");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_allocator_create(
        _: *const GCCellTypeInfo,
        _: *const c_char,
        _: usize,
    ) -> *mut GCAllocator {
        unreachable!("gc_allocator_create is not available without LibGC");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_heap_allocate_cell(_: *mut GCHeap, _: *mut GCAllocator, _: *mut bool) -> *mut c_void {
        unreachable!("gc_heap_allocate_cell is not available without LibGC");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_visitor_visit_cell(_: *mut GCVisitor, _: *mut c_void) {
        unreachable!("gc_visitor_visit_cell is not available without LibGC");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gc_visitor_visit_values(_: *mut GCVisitor, _: *const u64, _: usize) {
        unreachable!("gc_visitor_visit_values is not available without LibGC");
    }
}
