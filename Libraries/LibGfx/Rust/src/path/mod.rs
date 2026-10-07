/*
 * Copyright 2015 Google Inc.
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkPathBuilder.cpp, skia/include/core/SkPathBuilder.h and
// skia/src/core/SkPathData.cpp.

//! Paths, kept as verbs, points and conic weights.
//!
//! The compositor turns a path into a Skia path, so the representation, and every operation that
//! makes or reads one, follows Skia's rules exactly. A path that a document makes here must give
//! the same bytes, bounds and hit tests that the compositor gets from the same data.

mod along;
mod arc;
mod bounds;
mod buffer;
mod contains;
pub mod ffi;
mod geometry;
mod glyphs;
mod measure;
mod serialize;
mod sk_matrix;
mod svg_string;

use crate::geometry::{FloatPoint, FloatRect};
use crate::matrix::AffineTransform;
use buffer::RawBuffer;
use sk_matrix::SkMatrix;

pub use glyphs::GlyphRun;

unsafe extern "C" {
    fn ladybird_gfx_process_next_path_identity() -> u64;
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathVerb {
    Move = 0,
    Line = 1,
    Quad = 2,
    Conic = 3,
    Cubic = 4,
    Close = 5,
}

impl PathVerb {
    pub(crate) fn from_u8(value: u8) -> Option<PathVerb> {
        Some(match value {
            0 => PathVerb::Move,
            1 => PathVerb::Line,
            2 => PathVerb::Quad,
            3 => PathVerb::Conic,
            4 => PathVerb::Cubic,
            5 => PathVerb::Close,
            _ => return None,
        })
    }

    /// The number of points that the verb adds to the path.
    pub(crate) fn point_count(self) -> usize {
        match self {
            PathVerb::Move | PathVerb::Line => 1,
            PathVerb::Quad | PathVerb::Conic => 2,
            PathVerb::Cubic => 3,
            PathVerb::Close => 0,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathFillType {
    #[default]
    Winding = 0,
    EvenOdd = 1,
    InverseWinding = 2,
    InverseEvenOdd = 3,
}

impl PathFillType {
    pub(crate) fn from_bits(value: u32) -> PathFillType {
        match value & 3 {
            0 => PathFillType::Winding,
            1 => PathFillType::EvenOdd,
            2 => PathFillType::InverseWinding,
            _ => PathFillType::InverseEvenOdd,
        }
    }

    /// The fill type for a `Gfx::WindingRule`.
    pub(crate) fn from_winding_rule(winding_rule: i32) -> PathFillType {
        if winding_rule == 1 {
            PathFillType::EvenOdd
        } else {
            PathFillType::Winding
        }
    }

    pub(crate) fn is_even_odd(self) -> bool {
        matches!(self, PathFillType::EvenOdd | PathFillType::InverseEvenOdd)
    }

    pub(crate) fn is_inverse(self) -> bool {
        matches!(self, PathFillType::InverseWinding | PathFillType::InverseEvenOdd)
    }
}

/// The geometry of a path, as the compositor and the queries see it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PathView<'a> {
    pub verbs: &'a [PathVerb],
    pub points: &'a [FloatPoint],
    pub conic_weights: &'a [f32],
    pub fill_type: PathFillType,
}

impl<'a> PathView<'a> {
    pub(crate) fn empty(fill_type: PathFillType) -> PathView<'a> {
        PathView {
            verbs: &[],
            points: &[],
            conic_weights: &[],
            fill_type,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    /// Element-wise float comparison, so 0 and -0 are equal.
    pub(crate) fn has_same_geometry(&self, other: &PathView<'_>) -> bool {
        self.fill_type == other.fill_type
            && self.verbs == other.verbs
            && self.points == other.points
            && self.conic_weights == other.conic_weights
    }

    /// The verbs in order, with the points each one uses. The points of a segment start with the
    /// point it continues from, and a close gets the last point and the start of its contour. As
    /// in older iteration, a move at the end of the path is not part of the sequence.
    pub(crate) fn segments(&self) -> Segments<'a> {
        let verbs = match self.verbs.last() {
            Some(PathVerb::Move) => &self.verbs[..self.verbs.len() - 1],
            _ => self.verbs,
        };
        Segments {
            verbs,
            points: self.points,
            conic_weights: self.conic_weights,
            verb_index: 0,
            point_index: 0,
            conic_weight_index: 0,
            contour_start: FloatPoint::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    pub verb: PathVerb,
    pub points: [FloatPoint; 4],
    pub conic_weight: f32,
}

pub(crate) struct Segments<'a> {
    verbs: &'a [PathVerb],
    points: &'a [FloatPoint],
    conic_weights: &'a [f32],
    verb_index: usize,
    point_index: usize,
    conic_weight_index: usize,
    contour_start: FloatPoint,
}

impl Iterator for Segments<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        let verb = *self.verbs.get(self.verb_index)?;
        self.verb_index += 1;
        let mut segment = Segment {
            verb,
            points: [FloatPoint::default(); 4],
            conic_weight: -1.0,
        };
        match verb {
            PathVerb::Move => {
                self.contour_start = self.points[self.point_index];
                self.point_index += 1;
                segment.points[0] = self.contour_start;
            }
            PathVerb::Close => {
                segment.points[0] = self.points[self.point_index - 1];
                segment.points[1] = self.contour_start;
            }
            _ => {
                if verb == PathVerb::Conic {
                    segment.conic_weight = self.conic_weights[self.conic_weight_index];
                    self.conic_weight_index += 1;
                }
                let count = verb.point_count();
                segment.points[..=count].copy_from_slice(&self.points[self.point_index - 1..self.point_index + count]);
                self.point_index += count;
            }
        }
        Some(segment)
    }
}

/// A path that is being built, with the state that path operations keep beside the geometry.
///
/// The layout is C because each copy of this crate can get a path that another copy made.
#[repr(C)]
#[derive(Clone)]
pub struct Path {
    verbs: RawBuffer<PathVerb>,
    points: RawBuffer<FloatPoint>,
    conic_weights: RawBuffer<f32>,
    // The point that close() goes back to.
    last_move_to: FloatPoint,
    // The index of the point of the last move verb, or -1.
    last_move_index: i32,
    fill_type: PathFillType,
    has_current_point: bool,
}

impl Default for Path {
    fn default() -> Self {
        Self::new()
    }
}

impl Path {
    pub fn new() -> Path {
        Path {
            verbs: RawBuffer::new(),
            points: RawBuffer::new(),
            conic_weights: RawBuffer::new(),
            last_move_to: FloatPoint::default(),
            last_move_index: -1,
            fill_type: PathFillType::Winding,
            has_current_point: false,
        }
    }

    pub(crate) fn verbs(&self) -> &[PathVerb] {
        self.verbs.as_slice()
    }

    pub(crate) fn points(&self) -> &[FloatPoint] {
        self.points.as_slice()
    }

    pub(crate) fn conic_weights(&self) -> &[f32] {
        self.conic_weights.as_slice()
    }

    /// All of the data, also when a point is not finite.
    pub(crate) fn raw_view(&self) -> PathView<'_> {
        PathView {
            verbs: self.verbs(),
            points: self.points(),
            conic_weights: self.conic_weights(),
            fill_type: self.fill_type,
        }
    }

    /// The path as the compositor sees it. A path with a point that is not finite is empty.
    pub(crate) fn snapshot(&self) -> PathView<'_> {
        if self.verbs.is_empty() || bounds::trimmed_bounds(self.points(), self.verbs()).is_none() {
            return PathView::empty(self.fill_type);
        }
        self.raw_view()
    }

    // The operations below follow SkPathBuilder.

    fn sk_reset(&mut self) {
        self.verbs.clear();
        self.points.clear();
        self.conic_weights.clear();
        self.fill_type = PathFillType::Winding;
        self.last_move_index = -1;
    }

    pub(crate) fn sk_move_to(&mut self, point: FloatPoint) {
        if self.verbs.last() == Some(&PathVerb::Move) {
            *self.points.last_mut().expect("A move verb has a point") = point;
        } else {
            self.last_move_index = self.points.len() as i32;
            self.points.push(point);
            self.verbs.push(PathVerb::Move);
        }
    }

    pub(crate) fn sk_ensure_move(&mut self) {
        match self.verbs.last() {
            None => self.sk_move_to(FloatPoint::default()),
            Some(PathVerb::Close) => {
                let point = self.points()[self.last_move_index as usize];
                self.sk_move_to(point);
            }
            _ => {}
        }
    }

    pub(crate) fn sk_line_to(&mut self, point: FloatPoint) {
        self.sk_ensure_move();
        self.points.push(point);
        self.verbs.push(PathVerb::Line);
    }

    pub(crate) fn sk_quad_to(&mut self, point1: FloatPoint, point2: FloatPoint) {
        self.sk_ensure_move();
        self.points.extend_from_slice(&[point1, point2]);
        self.verbs.push(PathVerb::Quad);
    }

    pub(crate) fn sk_conic_to(&mut self, point1: FloatPoint, point2: FloatPoint, weight: f32) {
        self.sk_ensure_move();
        self.points.extend_from_slice(&[point1, point2]);
        if weight == 1.0 {
            self.verbs.push(PathVerb::Quad);
        } else {
            self.verbs.push(PathVerb::Conic);
            self.conic_weights.push(weight);
        }
    }

    pub(crate) fn sk_cubic_to(&mut self, point1: FloatPoint, point2: FloatPoint, point3: FloatPoint) {
        self.sk_ensure_move();
        self.points.extend_from_slice(&[point1, point2, point3]);
        self.verbs.push(PathVerb::Cubic);
    }

    pub(crate) fn sk_close(&mut self) {
        // A second close in sequence does nothing.
        if let Some(&last) = self.verbs.last()
            && last != PathVerb::Close
        {
            self.verbs.push(PathVerb::Close);
        }
    }

    fn sk_last_point(&self) -> Option<FloatPoint> {
        self.points.last().copied()
    }

    pub(crate) fn sk_set_last_point(&mut self, point: FloatPoint) {
        if let Some(last) = self.points.last_mut() {
            *last = point;
        }
    }

    fn sk_transform(&mut self, matrix: &SkMatrix) {
        if matrix.is_identity() || self.verbs.is_empty() {
            return;
        }
        matrix.map_points_in_place(self.points.as_mut_slice());
    }

    /// Replaces this path with `source`, replaying each verb, as assigning a finished path to a
    /// builder does.
    fn sk_assign(&mut self, source: PathView<'_>) {
        self.sk_reset();
        self.fill_type = source.fill_type;
        if source.is_empty() {
            return;
        }
        for segment in source.segments() {
            let points = segment.points;
            match segment.verb {
                PathVerb::Move => self.sk_move_to(points[0]),
                PathVerb::Line => self.sk_line_to(points[1]),
                PathVerb::Quad => self.sk_quad_to(points[1], points[2]),
                PathVerb::Conic => self.sk_conic_to(points[1], points[2], segment.conic_weight),
                PathVerb::Cubic => self.sk_cubic_to(points[1], points[2], points[3]),
                PathVerb::Close => self.sk_close(),
            }
        }
        // The segments leave out a move at the end, so put it back.
        if source.verbs.last() == Some(&PathVerb::Move) && self.verbs.last() != Some(&PathVerb::Move) {
            self.sk_move_to(*source.points.last().expect("A move verb has a point"));
        }
        self.last_move_index = find_last_move_index(self.verbs(), self.points.len());
    }

    /// Appends `source` with `matrix` applied, as the append mode of adding a path to a builder
    /// does.
    pub(crate) fn sk_add_path(&mut self, source: PathView<'_>, matrix: &SkMatrix) {
        if source.is_empty() {
            return;
        }
        if self.verbs.len() <= 1 && matrix.is_identity() {
            let fill_type = self.fill_type;
            self.sk_assign(source);
            self.fill_type = fill_type;
            return;
        }
        // The verbs are copied as they are, so a move at the end of this path and the first move
        // of the source stay two moves.
        self.last_move_index = find_last_move_index(source.verbs, source.points.len()) + self.points.len() as i32;
        self.verbs.extend_from_slice(source.verbs);
        let new_points = self.points.push_n(source.points.len(), FloatPoint::default());
        matrix.map_points(new_points, source.points);
        self.conic_weights.extend_from_slice(source.conic_weights);
    }

    /// Removes a move that ends the path, with its point.
    fn remove_last_move(&mut self) {
        self.verbs.truncate(self.verbs.len() - 1);
        self.points.truncate(self.points.len() - 1);
        self.last_move_index = find_last_move_index(self.verbs(), self.points.len());
    }

    // The operations below are the ones that Gfx::Path offers.

    pub fn clear(&mut self) {
        self.sk_reset();
        self.last_move_to = FloatPoint::default();
        self.has_current_point = false;
    }

    pub fn move_to(&mut self, point: FloatPoint) {
        self.last_move_to = point;
        self.has_current_point = true;
        self.sk_move_to(point);
    }

    pub fn line_to(&mut self, point: FloatPoint) {
        if !self.has_current_point {
            self.move_to(point);
            return;
        }
        self.sk_line_to(point);
    }

    /// Closes the subpath, and starts a new one at the point where the closed one started.
    pub fn close(&mut self) {
        if !self.has_current_point {
            return;
        }
        self.sk_close();
        self.sk_move_to(self.last_move_to);
    }

    /// An SVG arc to `point`. The rotation of the x-axis is in radians.
    pub fn elliptical_arc_to(
        &mut self,
        point: FloatPoint,
        radii: FloatPoint,
        x_axis_rotation: f32,
        large_arc: bool,
        sweep: bool,
    ) {
        if !self.has_current_point {
            self.move_to(point);
            return;
        }
        let degrees = x_axis_rotation * (180.0 / sk_matrix::PI);
        arc::arc_to(self, radii, degrees, large_arc, !sweep, point);
    }

    pub fn arc_to(&mut self, point: FloatPoint, radius: f32, large_arc: bool, sweep: bool) {
        if !self.has_current_point {
            self.move_to(point);
            return;
        }
        arc::arc_to(self, FloatPoint { x: radius, y: radius }, 0.0, large_arc, !sweep, point);
    }

    pub fn quadratic_bezier_curve_to(&mut self, through: FloatPoint, point: FloatPoint) {
        if !self.has_current_point {
            self.move_to(through);
        }
        self.sk_quad_to(through, point);
    }

    pub fn cubic_bezier_curve_to(&mut self, control1: FloatPoint, control2: FloatPoint, point: FloatPoint) {
        if !self.has_current_point {
            self.move_to(control1);
        }
        self.sk_cubic_to(control1, control2, point);
    }

    pub fn append_path(&mut self, other: &Path) {
        let source = other.snapshot();
        // The source starts with a move. If this path ends with a move, the result has two moves in
        // sequence. That is not a valid path, and the compositor gets an empty path. Remove the move
        // first. A subpath with one point is not filled or stroked.
        if !source.is_empty() && self.verbs.len() > 1 && self.verbs.last() == Some(&PathVerb::Move) {
            self.remove_last_move();
        }
        self.sk_add_path(source, &SkMatrix::IDENTITY);
        if other.has_current_point {
            self.has_current_point = true;
            self.last_move_to = other.last_move_to;
        }
    }

    pub fn set_fill_type(&mut self, fill_type: PathFillType) {
        self.fill_type = fill_type;
    }

    /// True if no operation gave the path a current point.
    pub fn is_empty(&self) -> bool {
        !self.has_current_point
    }

    pub fn last_point(&self) -> FloatPoint {
        self.sk_last_point().unwrap_or_default()
    }

    /// The bounds of all points, also the control points of curves. A path with a point that is not
    /// finite has empty bounds.
    pub fn bounding_box(&self) -> FloatRect {
        let [left, top, right, bottom] = bounds::trimmed_bounds(self.points(), self.verbs()).unwrap_or([0.0; 4]);
        FloatRect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// Applies the transform that maps (x, y) to (a * x + c * y + e, b * x + d * y + f).
    pub fn transform(&mut self, affine_values: [f32; 6]) {
        self.sk_transform(&SkMatrix::from_affine(affine_values));
        if self.has_current_point {
            self.last_move_to = AffineTransform { values: affine_values }.map_point(self.last_move_to);
        }
    }

    pub fn copy_transformed(&self, affine_values: [f32; 6]) -> Path {
        let mut copy = self.clone();
        copy.transform(affine_values);
        copy
    }

    /// Replaces this path with a decoded one, as receiving a finished path does.
    fn set_from_view(&mut self, view: PathView<'_>) {
        self.sk_assign(view);
        self.update_state_from_view(view);
    }

    fn update_state_from_view(&mut self, view: PathView<'_>) {
        (self.has_current_point, self.last_move_to) = state_of_view(view);
    }

    /// Takes the current point and the start of the last contour from the path as the compositor
    /// sees it.
    pub(crate) fn update_state_from_snapshot(&mut self) {
        (self.has_current_point, self.last_move_to) = state_of_view(self.snapshot());
    }

    pub fn serialize_to_bytes(&self) -> Vec<u8> {
        serialize::write(self.snapshot())
    }

    pub fn from_serialized_bytes(bytes: &[u8]) -> Path {
        let mut path = Path::new();
        serialize::read(bytes, |view| path.set_from_view(view));
        path
    }

    /// True if both paths have the same geometry and fill type, as the compositor sees them.
    pub fn has_same_geometry(&self, other: &Path) -> bool {
        self.snapshot().has_same_geometry(&other.snapshot())
    }
}

fn state_of_view(view: PathView<'_>) -> (bool, FloatPoint) {
    let mut last_move_to = FloatPoint::default();
    let mut point_index = 0;
    for verb in view.verbs {
        if *verb == PathVerb::Move {
            last_move_to = view.points[point_index];
        }
        point_index += verb.point_count();
    }
    (!view.verbs.is_empty(), last_move_to)
}

fn find_last_move_index(verbs: &[PathVerb], point_count: usize) -> i32 {
    if verbs.is_empty() {
        return -1;
    }
    let mut index = point_count as i32 - 1;
    for verb in verbs.iter().rev() {
        if *verb == PathVerb::Move {
            break;
        }
        index -= verb.point_count() as i32;
    }
    index
}

/// Builds a path one operation at a time.
#[derive(Clone, Default)]
pub struct PathBuilder {
    path: Path,
}

impl PathBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if the path has no verbs.
    pub fn is_empty(&self) -> bool {
        self.path.verbs.is_empty()
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(FloatPoint { x, y });
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(FloatPoint { x, y });
    }

    pub fn quadratic_bezier_curve_to(&mut self, through_x: f32, through_y: f32, x: f32, y: f32) {
        self.path.quadratic_bezier_curve_to(
            FloatPoint {
                x: through_x,
                y: through_y,
            },
            FloatPoint { x, y },
        );
    }

    pub fn cubic_bezier_curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.path.cubic_bezier_curve_to(
            FloatPoint { x: c1x, y: c1y },
            FloatPoint { x: c2x, y: c2y },
            FloatPoint { x, y },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn elliptical_arc_to(
        &mut self,
        x: f32,
        y: f32,
        radius_x: f32,
        radius_y: f32,
        x_axis_rotation: f32,
        large_arc: bool,
        sweep: bool,
    ) {
        self.path.elliptical_arc_to(
            FloatPoint { x, y },
            FloatPoint {
                x: radius_x,
                y: radius_y,
            },
            x_axis_rotation,
            large_arc,
            sweep,
        );
    }

    pub fn arc_to(&mut self, x: f32, y: f32, radius: f32, large_arc: bool, sweep: bool) {
        self.path.arc_to(FloatPoint { x, y }, radius, large_arc, sweep);
    }

    pub fn close(&mut self) {
        self.path.close();
    }

    pub fn build(self) -> OwnedPath {
        OwnedPath::new(self.path)
    }
}

/// A finished path on the heap, with an identity that no other path in the process has.
pub struct OwnedPath {
    path: Box<Path>,
    identity: u64,
}

impl std::fmt::Debug for OwnedPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedPath")
            .field("identity", &self.identity)
            .finish()
    }
}

impl OwnedPath {
    pub fn new(path: Path) -> OwnedPath {
        OwnedPath {
            path: Box::new(path),
            // The counter is LibGfx's: this crate is compiled into more than one library, and a
            // counter here would let each of them hand out the same identities. See
            // `LibGfx/RustProcessState.cpp`.
            // SAFETY: The counter is an atomic on the other side of the boundary.
            identity: unsafe { ladybird_gfx_process_next_path_identity() },
        }
    }

    /// The path, still owned by this value.
    #[inline]
    pub fn as_raw(&self) -> *const Path {
        &raw const *self.path
    }

    /// Gives the path to the caller.
    #[inline]
    pub fn into_raw(self) -> *mut Path {
        Box::into_raw(self.path)
    }

    /// A process-unique, never-reused identity for this path allocation, so
    /// consumers holding a copied snapshot can recognize an unchanged path
    /// without comparing contents.
    #[inline]
    pub fn identity(&self) -> u64 {
        self.identity
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn path_mut(&mut self) -> &mut Path {
        &mut self.path
    }

    pub fn set_fill_type(&mut self, winding_rule: i32) {
        self.path_mut()
            .set_fill_type(PathFillType::from_winding_rule(winding_rule));
    }

    pub fn bounding_box(&self) -> [f32; 4] {
        let rect = self.path().bounding_box();
        [rect.x, rect.y, rect.width, rect.height]
    }

    pub fn length(&self) -> f32 {
        self.path().length()
    }

    pub fn copy_transformed(&self, affine_values: [f32; 6]) -> OwnedPath {
        OwnedPath::new(self.path().copy_transformed(affine_values))
    }

    /// The outlines of the given glyphs, as one path.
    pub fn from_glyph_runs(runs: &[GlyphRun]) -> OwnedPath {
        let mut path = Path::new();
        for run in runs {
            glyphs::append_glyph_run(&mut path, run.font.as_raw(), &run.glyphs);
        }
        OwnedPath::new(path)
    }

    /// The outlines of the given glyphs, laid along this path from `offset`.
    pub fn place_glyph_runs_along(&self, runs: &[GlyphRun], offset: f32) -> OwnedPath {
        OwnedPath::new(glyphs::place_glyph_runs_along(self.path(), runs, offset))
    }

    pub fn from_serialized_bytes(bytes: &[u8]) -> OwnedPath {
        OwnedPath::new(Path::from_serialized_bytes(bytes))
    }

    pub fn serialize_to_bytes(&self) -> Vec<u8> {
        self.path().serialize_to_bytes()
    }

    pub fn contains(&self, x: f32, y: f32, winding_rule: i32) -> bool {
        self.path()
            .contains(FloatPoint { x, y }, PathFillType::from_winding_rule(winding_rule))
    }

    pub fn to_svg_string(&self) -> String {
        self.path().to_svg_string()
    }
}

impl PartialEq for OwnedPath {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity || self.path().has_same_geometry(other.path())
    }
}
