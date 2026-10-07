/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The functions that `Gfx::Path` and the font code in C++ call.

use super::along::{PathVertex, PointAlongPath};
use super::geometry::point;
use super::glyphs;
use super::{Path, PathFillType, PathVerb};
use crate::geometry::{FloatPoint, FloatRect};
use crate::text_layout::DrawGlyph;
use std::ffi::c_void;

/// The path that a `Gfx::Path` owns. C++ only holds pointers to it.
pub struct RustPath {
    _private: [u8; 0],
}

/// The geometry of a path as the compositor uses it. A path with a point that is not finite has
/// none.
#[repr(C)]
pub struct FfiPathData {
    pub verbs: *const PathVerb,
    pub verb_count: usize,
    pub points: *const FloatPoint,
    pub point_count: usize,
    pub conic_weights: *const f32,
    pub conic_weight_count: usize,
    pub fill_type: PathFillType,
}

/// # Safety
///
/// `path` must be a live path that a function of this module gave, and nothing may change it
/// for the lifetime the caller picks.
unsafe fn path_ref<'a>(path: *const RustPath) -> &'a Path {
    assert!(!path.is_null(), "Path pointer must not be null");
    // SAFETY: Guaranteed by the caller.
    unsafe { &*path.cast::<Path>() }
}

/// # Safety
///
/// As for path_ref(), and nothing else may use the path for the lifetime the caller picks.
unsafe fn path_mut<'a>(path: *mut RustPath) -> &'a mut Path {
    assert!(!path.is_null(), "Path pointer must not be null");
    // SAFETY: Guaranteed by the caller.
    unsafe { &mut *path.cast::<Path>() }
}

pub(crate) fn into_raw(path: Path) -> *mut RustPath {
    Box::into_raw(Box::new(path)).cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn ladybird_gfx_path_create() -> *mut RustPath {
    into_raw(Path::new())
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_clone(path: *const RustPath) -> *mut RustPath {
    // SAFETY: Guaranteed by the caller.
    into_raw(unsafe { path_ref(path) }.clone())
}

/// # Safety
///
/// `path` must be null, or a live path that nothing uses after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_destroy(path: *mut RustPath) {
    if !path.is_null() {
        // SAFETY: Guaranteed by the caller. Every path that this module gives is a Box.
        drop(unsafe { Box::from_raw(path.cast::<Path>()) });
    }
}

/// # Safety
///
/// `path` must be a live path that nothing else uses during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_clear(path: *mut RustPath) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.clear();
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_move_to(path: *mut RustPath, x: f32, y: f32) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.move_to(point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_line_to(path: *mut RustPath, x: f32, y: f32) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.line_to(point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_quadratic_bezier_curve_to(
    path: *mut RustPath,
    through_x: f32,
    through_y: f32,
    x: f32,
    y: f32,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.quadratic_bezier_curve_to(point(through_x, through_y), point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ladybird_gfx_path_cubic_bezier_curve_to(
    path: *mut RustPath,
    control1_x: f32,
    control1_y: f32,
    control2_x: f32,
    control2_y: f32,
    x: f32,
    y: f32,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.cubic_bezier_curve_to(
        point(control1_x, control1_y),
        point(control2_x, control2_y),
        point(x, y),
    );
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ladybird_gfx_path_elliptical_arc_to(
    path: *mut RustPath,
    x: f32,
    y: f32,
    radius_x: f32,
    radius_y: f32,
    x_axis_rotation: f32,
    large_arc: bool,
    sweep: bool,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.elliptical_arc_to(
        point(x, y),
        point(radius_x, radius_y),
        x_axis_rotation,
        large_arc,
        sweep,
    );
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_arc_to(
    path: *mut RustPath,
    x: f32,
    y: f32,
    radius: f32,
    large_arc: bool,
    sweep: bool,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.arc_to(point(x, y), radius, large_arc, sweep);
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_close(path: *mut RustPath) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.close();
}

/// # Safety
///
/// As for ladybird_gfx_path_clear(), and `other` must be a live path. It can be `path`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_append_path(path: *mut RustPath, other: *const RustPath) {
    if std::ptr::eq(path.cast_const(), other) {
        // SAFETY: Guaranteed by the caller.
        let copy = unsafe { path_ref(other) }.clone();
        // SAFETY: Guaranteed by the caller.
        unsafe { path_mut(path) }.append_path(&copy);
        return;
    }
    // SAFETY: Guaranteed by the caller, and the paths are not the same.
    unsafe { path_mut(path) }.append_path(unsafe { path_ref(other) });
}

/// Sets the fill type for a `Gfx::WindingRule`.
///
/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_set_fill_type(path: *mut RustPath, winding_rule: i32) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.set_fill_type(PathFillType::from_winding_rule(winding_rule));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear(), and `affine_values` must point to six floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_transform(path: *mut RustPath, affine_values: *const f32) {
    // SAFETY: Guaranteed by the caller.
    let values = unsafe { *affine_values.cast::<[f32; 6]>() };
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.transform(values);
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_is_empty(path: *const RustPath) -> bool {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(path) }.is_empty()
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_last_point(path: *const RustPath) -> FloatPoint {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(path) }.last_point()
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_bounding_box(path: *const RustPath) -> FloatRect {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(path) }.bounding_box()
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_length(path: *const RustPath) -> f32 {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(path) }.length()
}

/// # Safety
///
/// `path` must be a live path, and `out` must point to a PointAlongPath.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_point_at_length(
    path: *const RustPath,
    distance: f32,
    out: *mut PointAlongPath,
) -> bool {
    // SAFETY: Guaranteed by the caller.
    let Some(point_along_path) = unsafe { path_ref(path) }.point_at_length(distance) else {
        return false;
    };
    // SAFETY: Guaranteed by the caller.
    unsafe { out.write(point_along_path) };
    true
}

/// Gives the vertices to `append`, in one call.
///
/// # Safety
///
/// `path` must be a live path, and `append` must accept `context`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_vertices(
    path: *const RustPath,
    append: unsafe extern "C" fn(*mut c_void, *const PathVertex, usize),
    context: *mut c_void,
) {
    // SAFETY: Guaranteed by the caller.
    let vertices = unsafe { path_ref(path) }.vertices();
    // SAFETY: Guaranteed by the caller. The vertices are live for the call.
    unsafe { append(context, vertices.as_ptr(), vertices.len()) };
}

/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_contains(path: *const RustPath, x: f32, y: f32, winding_rule: i32) -> bool {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(path) }.contains(point(x, y), PathFillType::from_winding_rule(winding_rule))
}

/// # Safety
///
/// `a` and `b` must be live paths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_equals(a: *const RustPath, b: *const RustPath) -> bool {
    if std::ptr::eq(a, b) {
        return true;
    }
    // SAFETY: Guaranteed by the caller.
    unsafe { path_ref(a) }.has_same_geometry(unsafe { path_ref(b) })
}

/// Gives the bytes to `append`, in one call.
///
/// # Safety
///
/// `path` must be a live path, and `append` must accept `context`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_serialize(
    path: *const RustPath,
    append: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    context: *mut c_void,
) {
    // SAFETY: Guaranteed by the caller.
    let bytes = unsafe { path_ref(path) }.serialize_to_bytes();
    // SAFETY: Guaranteed by the caller. The bytes are live for the call.
    unsafe { append(context, bytes.as_ptr(), bytes.len()) };
}

/// Bytes that are not a valid path give an empty path.
///
/// # Safety
///
/// `bytes` must point to `count` bytes, or be null if `count` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_create_from_serialized_bytes(
    bytes: *const u8,
    count: usize,
) -> *mut RustPath {
    // SAFETY: Guaranteed by the caller.
    let bytes = unsafe { ak::slice_from_raw_parts(bytes, count) };
    into_raw(Path::from_serialized_bytes(bytes))
}

/// The data stays valid until the path changes or is destroyed.
///
/// # Safety
///
/// `path` must be a live path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_data(path: *const RustPath) -> FfiPathData {
    // SAFETY: Guaranteed by the caller.
    let view = unsafe { path_ref(path) }.snapshot();
    FfiPathData {
        verbs: view.verbs.as_ptr(),
        verb_count: view.verbs.len(),
        points: view.points.as_ptr(),
        point_count: view.points.len(),
        conic_weights: view.conic_weights.as_ptr(),
        conic_weight_count: view.conic_weights.len(),
        fill_type: view.fill_type,
    }
}

/// # Safety
///
/// As for ladybird_gfx_path_clear(), `font` must be a live `Gfx::Font`, and `glyphs` must point
/// to `glyph_count` glyphs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_append_glyph_run(
    path: *mut RustPath,
    font: *const c_void,
    glyphs: *const DrawGlyph,
    glyph_count: usize,
) {
    // SAFETY: Guaranteed by the caller.
    let glyphs = unsafe { ak::slice_from_raw_parts(glyphs, glyph_count) };
    // SAFETY: Guaranteed by the caller.
    glyphs::append_glyph_run(unsafe { path_mut(path) }, font, glyphs);
}

// The functions below build a glyph outline. Unlike the operations above, a close does not start
// a new contour.

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_move_to(path: *mut RustPath, x: f32, y: f32) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_move_to(point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_line_to(path: *mut RustPath, x: f32, y: f32) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_line_to(point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_quad_to(
    path: *mut RustPath,
    control_x: f32,
    control_y: f32,
    x: f32,
    y: f32,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_quad_to(point(control_x, control_y), point(x, y));
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_conic_to(
    path: *mut RustPath,
    control_x: f32,
    control_y: f32,
    x: f32,
    y: f32,
    weight: f32,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_conic_to(point(control_x, control_y), point(x, y), weight);
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_cubic_to(
    path: *mut RustPath,
    control1_x: f32,
    control1_y: f32,
    control2_x: f32,
    control2_y: f32,
    x: f32,
    y: f32,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_cubic_to(
        point(control1_x, control1_y),
        point(control2_x, control2_y),
        point(x, y),
    );
}

/// # Safety
///
/// As for ladybird_gfx_path_clear().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladybird_gfx_path_outline_close(path: *mut RustPath) {
    // SAFETY: Guaranteed by the caller.
    unsafe { path_mut(path) }.sk_close();
}
