/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::ffi::c_void;

use crate::fc::FfiLayoutFcCallbacks;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiTextChunk {
    pub start: usize,
    pub length: usize,
    pub font: *const c_void,
    pub has_breaking_newline: bool,
    pub has_breaking_tab: bool,
    pub is_all_whitespace: bool,
    pub can_break_after: bool,
    pub text_type: u8,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiTextNodeFacts {
    pub text_utf16: *const u16,
    pub text_length_in_code_units: usize,
    pub chunks: *const FfiTextChunk,
    pub chunk_count: usize,
    pub should_collapse_whitespace: bool,
    pub is_generated_for_pseudo_element: bool,
    pub is_empty_editable: bool,
    pub has_dom_node: bool,
    pub retained: *mut c_void,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiShapeRequest {
    pub baseline_start_x: f32,
    pub letter_spacing: f32,
    pub text_utf16: *const u16,
    pub length_in_code_units: usize,
    pub font: *const c_void,
    pub text_type: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiDrawGlyph {
    pub x: f32,
    pub y: f32,
    pub length_in_code_units: usize,
    pub glyph_width: f32,
    pub glyph_id: u32,
    pub should_paint: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiShapedRunView {
    pub glyphs: *const FfiDrawGlyph,
    pub glyph_count: usize,
    pub width: f32,
    pub retained: *mut c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiFontPixelMetrics {
    pub x_height: f32,
    pub advance_of_ascii_zero: f32,
    pub ascent: f32,
    pub descent: f32,
    pub pixel_size: f32,
}

pub(crate) struct TextNodeFacts {
    ffi: FfiTextNodeFacts,
    release: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

impl TextNodeFacts {
    pub(crate) fn build(
        callbacks: &FfiLayoutFcCallbacks,
        text_node: *mut c_void,
        should_wrap_lines: bool,
        should_respect_linebreaks: bool,
        unidirectional_ltr: bool,
    ) -> Self {
        let mut ffi = std::mem::MaybeUninit::<FfiTextNodeFacts>::uninit();
        // SAFETY: The host synchronously initializes `ffi` on the true path.
        let built = unsafe {
            (callbacks.build_text_facts)(
                callbacks.context,
                text_node,
                should_wrap_lines,
                should_respect_linebreaks,
                unidirectional_ltr,
                ffi.as_mut_ptr(),
            )
        };
        assert!(built);
        // SAFETY: `built` is true, so the callback initialized every field.
        let ffi = unsafe { ffi.assume_init() };
        assert!(!ffi.retained.is_null());
        assert!(ffi.text_length_in_code_units == 0 || !ffi.text_utf16.is_null());
        assert!(ffi.chunk_count == 0 || !ffi.chunks.is_null());
        Self {
            ffi,
            release: callbacks.release_text_facts,
        }
    }

    pub(crate) fn ffi(&self) -> FfiTextNodeFacts {
        self.ffi
    }
}

impl Drop for TextNodeFacts {
    fn drop(&mut self) {
        // The release operation is context-independent by contract: text
        // snapshots can outlive the bridge instance that populated the state.
        // SAFETY: `retained` is owned by this snapshot and released once.
        unsafe {
            (self.release)(std::ptr::null_mut(), self.ffi.retained);
        }
    }
}
