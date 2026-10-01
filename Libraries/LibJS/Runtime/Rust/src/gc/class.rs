/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::ffi::{c_char, c_void};

/// Mirrors GCCellTypeInfo from Libraries/LibGC/CAPI.h, which LibGC dispatches every per-cell operation through.
#[repr(C)]
pub struct CellTypeInfo {
    pub cell_size: u32,
    pub alignment: u32,
    pub kind: u8,
    pub visit_edges: unsafe extern "C" fn(cell: *mut c_void, visitor: *mut c_void),
    pub finalize: Option<unsafe extern "C" fn(cell: *mut c_void)>,
    pub destroy: Option<unsafe extern "C" fn(cell: *mut c_void)>,
    pub external_memory_size: Option<unsafe extern "C" fn(cell: *const c_void) -> usize>,
    pub class_name: Option<unsafe extern "C" fn(cell: *const c_void, length: *mut usize) -> *const c_char>,
}

/// The class of a cell, stored in the first word of every cell. It starts with the type info LibGC dispatches
/// through, so the class of a cell and the type info of the block it lives in are the same address.
#[repr(C)]
pub struct Class {
    pub type_info: CellTypeInfo,
    pub name: &'static str,
}
