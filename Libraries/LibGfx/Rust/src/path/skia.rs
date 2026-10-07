/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The path queries that Skia still answers. LibGfx makes a Skia path from the geometry for each
//! one.

use super::Path;
use crate::geometry::FloatPoint;
use std::ffi::c_void;

unsafe extern "C" {
    fn ladybird_gfx_skia_path_length(path: *const c_void) -> f32;
    fn ladybird_gfx_skia_path_first_contour_length(path: *const c_void) -> f32;
    fn ladybird_gfx_skia_path_position_and_tangent(
        path: *const c_void,
        distance: f32,
        out_position_and_tangent: *mut f32,
    ) -> bool;
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
    /// The sum of the lengths of all contours.
    pub fn length(&self) -> f32 {
        // SAFETY: The path is live for the call.
        unsafe { ladybird_gfx_skia_path_length(raw(self)) }
    }

    /// The path as SVG path data with absolute commands. Conics become quads.
    pub fn to_svg_string(&self) -> String {
        let mut output = String::new();
        // SAFETY: The path is live for the call, and the callback only writes into the String whose
        // pointer it receives.
        unsafe { ladybird_gfx_skia_path_append_svg_string(raw(self), append_to_string, (&raw mut output).cast()) };
        output
    }
}

/// The first contour of a path that has a length.
pub(crate) struct ContourMeasure<'a> {
    path: &'a Path,
    length: f32,
}

impl<'a> ContourMeasure<'a> {
    pub(crate) fn first_of(path: &'a Path) -> Option<ContourMeasure<'a>> {
        // SAFETY: The path is live for the call.
        let length = unsafe { ladybird_gfx_skia_path_first_contour_length(raw(path)) };
        (length > 0.0).then_some(ContourMeasure { path, length })
    }

    pub(crate) fn length(&self) -> f32 {
        self.length
    }

    /// The point at `distance` along the contour, and the unit tangent there.
    pub(crate) fn position_and_tangent(&self, distance: f32) -> Option<(FloatPoint, FloatPoint)> {
        let mut out = [0.0f32; 4];
        // SAFETY: The path is live for the call, and the out-array holds four floats.
        let found = unsafe { ladybird_gfx_skia_path_position_and_tangent(raw(self.path), distance, out.as_mut_ptr()) };
        found.then_some((FloatPoint { x: out[0], y: out[1] }, FloatPoint { x: out[2], y: out[3] }))
    }
}
