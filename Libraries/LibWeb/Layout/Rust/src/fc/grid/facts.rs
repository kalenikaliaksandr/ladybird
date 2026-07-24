/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::fc::FfiLayoutFcCallbacks;
use crate::style_facts::FfiSizeValue;
use std::ffi::c_void;

pub(crate) const NO_GRID_INDEX: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiGridTrackEntryKind {
    LineNames,
    TrackSize,
    MinMax,
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiGridTrackBreadthKind {
    Auto,
    LengthPercentage,
    Flex,
    MinContent,
    MaxContent,
    FitContent,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridTrackBreadth {
    pub kind: u8,
    pub value: FfiSizeValue,
    pub flex_factor: f64,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiGridTrackList {
    pub is_subgrid: bool,
    pub preserves_line_name_sets: bool,
    pub first_entry: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridTrackEntry {
    pub kind: u8,
    pub next_sibling: u32,
    pub name_index_start: usize,
    pub name_index_count: usize,
    pub size: FfiGridTrackBreadth,
    pub min_size: FfiGridTrackBreadth,
    pub max_size: FfiGridTrackBreadth,
    pub repeat_type: u8,
    pub repeat_count: usize,
    pub repeat_list: FfiGridTrackList,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiGridArea {
    pub name_index: u32,
    pub implicit_start_name_index: u32,
    pub implicit_end_name_index: u32,
    pub row_start: usize,
    pub row_end: usize,
    pub column_start: usize,
    pub column_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiGridPlacementKind {
    Auto,
    Line,
    Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiGridPlacement {
    pub kind: u8,
    pub has_line_number: bool,
    pub line_number: i32,
    pub has_name: bool,
    pub name_index: u32,
    pub implicit_start_name_index: u32,
    pub implicit_end_name_index: u32,
}

impl Default for FfiGridPlacement {
    fn default() -> Self {
        Self {
            kind: FfiGridPlacementKind::Auto as u8,
            has_line_number: false,
            line_number: 0,
            has_name: false,
            name_index: NO_GRID_INDEX,
            implicit_start_name_index: NO_GRID_INDEX,
            implicit_end_name_index: NO_GRID_INDEX,
        }
    }
}

/// Borrowed array view returned by the C++ snapshot builder.
///
/// `snapshot_owner` owns only the backing arrays. Each entry in `names` is one
/// leaked `Utf16FlyString` reference and each non-null calc pointer nested in
/// `entries` is one retained calculated-style-value reference. Rust copies the
/// arrays, takes ownership of those references, and then invokes
/// `release_grid_facts_snapshot` to release only the backing storage.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridStyleFacts {
    pub snapshot_owner: *mut c_void,
    pub names: *const usize,
    pub name_count: usize,
    pub name_indices: *const u32,
    pub name_index_count: usize,
    pub entries: *const FfiGridTrackEntry,
    pub entry_count: usize,
    pub template_columns: FfiGridTrackList,
    pub template_rows: FfiGridTrackList,
    pub auto_columns: FfiGridTrackList,
    pub auto_rows: FfiGridTrackList,
    pub areas: *const FfiGridArea,
    pub area_count: usize,
    pub area_row_count: usize,
    pub area_column_count: usize,
    pub column_start: FfiGridPlacement,
    pub column_end: FfiGridPlacement,
    pub row_start: FfiGridPlacement,
    pub row_end: FfiGridPlacement,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutLine {
    pub names: *const usize,
    pub name_count: usize,
    pub start: crate::css_pixels::CssPixels,
    pub breadth: crate::css_pixels::CssPixels,
    pub type_: u8,
    pub number: u32,
    pub negative_number: i32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutTrack {
    pub start: crate::css_pixels::CssPixels,
    pub breadth: crate::css_pixels::CssPixels,
    pub type_: u8,
    pub state: u8,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutDimension {
    pub lines: *const FfiGridLayoutLine,
    pub line_count: usize,
    pub tracks: *const FfiGridLayoutTrack,
    pub track_count: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutArea {
    pub name: usize,
    pub type_: u8,
    pub row_start: u32,
    pub row_end: u32,
    pub column_start: u32,
    pub column_end: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutFragment {
    pub areas: *const FfiGridLayoutArea,
    pub area_count: usize,
    pub columns: FfiGridLayoutDimension,
    pub rows: FfiGridLayoutDimension,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiGridLayoutData {
    pub direction: u8,
    pub writing_mode: u8,
    pub is_subgrid: bool,
    pub fragments: *const FfiGridLayoutFragment,
    pub fragment_count: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiUsedGridLine {
    pub names: *const usize,
    pub name_count: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiUsedGridTrackList {
    pub is_subgrid: bool,
    pub lines: *const FfiUsedGridLine,
    pub line_count: usize,
    pub track_sizes: *const crate::css_pixels::CssPixels,
    pub track_count: usize,
}

struct RetainedGridName {
    raw: usize,
}

impl Drop for RetainedGridName {
    fn drop(&mut self) {
        // SAFETY: The C++ snapshot builder leaked exactly one reference for
        // every raw handle copied into this owner.
        unsafe {
            ladybird_layout_release_grid_name_handle(self.raw);
        }
    }
}

pub(crate) struct GridStyleFacts {
    names: Vec<RetainedGridName>,
    pub(crate) name_indices: Vec<u32>,
    pub(crate) entries: Vec<FfiGridTrackEntry>,
    pub(crate) template_columns: FfiGridTrackList,
    pub(crate) template_rows: FfiGridTrackList,
    pub(crate) auto_columns: FfiGridTrackList,
    pub(crate) auto_rows: FfiGridTrackList,
    pub(crate) areas: Vec<FfiGridArea>,
    pub(crate) area_row_count: usize,
    pub(crate) area_column_count: usize,
    pub(crate) column_start: FfiGridPlacement,
    pub(crate) column_end: FfiGridPlacement,
    pub(crate) row_start: FfiGridPlacement,
    pub(crate) row_end: FfiGridPlacement,
}

impl GridStyleFacts {
    pub(crate) fn build(callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> Self {
        // SAFETY: The callback returns a synchronous borrowed snapshot whose
        // owner stays live until the matching release callback below.
        let ffi = unsafe { (callbacks.build_grid_facts)(callbacks.context, node) };
        assert!(!ffi.snapshot_owner.is_null());

        // SAFETY: Every pointer/count pair belongs to `snapshot_owner` and is
        // valid until `release_grid_facts_snapshot` is invoked.
        let name_raws = unsafe { copy_slice(ffi.names, ffi.name_count) };
        let name_indices = unsafe { copy_slice(ffi.name_indices, ffi.name_index_count) };
        let entries = unsafe { copy_slice(ffi.entries, ffi.entry_count) };
        let areas = unsafe { copy_slice(ffi.areas, ffi.area_count) };

        // This releases only the C++ vectors. Ownership of the leaked name
        // references and retained calc references has moved to this object.
        unsafe {
            (callbacks.release_grid_facts_snapshot)(callbacks.context, ffi.snapshot_owner);
        }

        Self {
            names: name_raws.into_iter().map(|raw| RetainedGridName { raw }).collect(),
            name_indices,
            entries,
            template_columns: ffi.template_columns,
            template_rows: ffi.template_rows,
            auto_columns: ffi.auto_columns,
            auto_rows: ffi.auto_rows,
            areas,
            area_row_count: ffi.area_row_count,
            area_column_count: ffi.area_column_count,
            column_start: ffi.column_start,
            column_end: ffi.column_end,
            row_start: ffi.row_start,
            row_end: ffi.row_end,
        }
    }

    pub(crate) fn name_raw(&self, index: u32) -> usize {
        self.names[index as usize].raw
    }

    pub(crate) fn name_count(&self) -> usize {
        self.names.len()
    }
}

impl Drop for GridStyleFacts {
    fn drop(&mut self) {
        for entry in &self.entries {
            entry.size.value.release_calc_handle();
            entry.min_size.value.release_calc_handle();
            entry.max_size.value.release_calc_handle();
        }
    }
}

unsafe fn copy_slice<T: Copy>(pointer: *const T, length: usize) -> Vec<T> {
    if length == 0 {
        return Vec::new();
    }
    assert!(!pointer.is_null());
    // SAFETY: The caller guarantees a live array containing `length` values.
    unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
}

unsafe extern "C" {
    fn ladybird_layout_release_grid_name_handle(raw: usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_grid_fact_types_are_plain_c_abi_values() {
        assert_eq!(NO_GRID_INDEX, u32::MAX);
        assert!(size_of::<FfiGridTrackEntry>() >= size_of::<FfiGridTrackBreadth>() * 3);
        assert_eq!(FfiGridPlacementKind::Auto as u8, 0);
        assert_eq!(FfiGridPlacementKind::Line as u8, 1);
        assert_eq!(FfiGridPlacementKind::Span as u8, 2);
    }
}
