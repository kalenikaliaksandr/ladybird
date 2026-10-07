/*
 * Copyright 2018 Google LLC
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkContourMeasure.cpp and skia/src/core/SkPathMeasure.cpp.

//! Lengths of contours, and positions and directions on them. Curves are measured as chains of
//! lines whose middles are close to the curve.

use super::geometry::{
    Conic, add, chop_cubic_at_half, chop_quad_at_half, distance, eval_cubic_at, eval_cubic_tangent_at, eval_quad_at,
    eval_quad_tangent_at, interpolate, interpolate_points, is_finite, max_of, normalize, point, scale, subtract,
};
use super::{Path, PathVerb, PathView};
use crate::geometry::FloatPoint;

const MAX_T_VALUE: i32 = 0x3FFF_FFFF;
// Curves are measured sixteen times as precisely as the original measures them by default. At that
// default, the length of a circle with a radius of 25 units is about 1 unit short.
const RESOLUTION_SCALE: f32 = 16.0;
// How far a chain of lines can be from the curve.
pub(crate) const TOLERANCE: f32 = 0.5 * (1.0 / RESOLUTION_SCALE);
const MAX_RECURSION_DEPTH: i32 = 8;

fn t_value_to_scalar(t: i32) -> f32 {
    const RECIPROCAL: f32 = 1.0 / MAX_T_VALUE as f32;
    t as f32 * RECIPROCAL
}

fn t_span_is_big_enough(t_span: i32) -> bool {
    t_span >> 10 != 0
}

fn half(value: f32) -> f32 {
    value * 0.5
}

fn quad_is_too_curvy(points: &[FloatPoint]) -> bool {
    let dx = half(points[1].x) - half(half(points[0].x + points[2].x));
    let dy = half(points[1].y) - half(half(points[0].y + points[2].y));
    max_of(dx.abs(), dy.abs()) > TOLERANCE
}

fn conic_is_too_curvy(first: FloatPoint, middle: FloatPoint, last: FloatPoint) -> bool {
    let middle_of_ends = scale(add(first, last), 0.5);
    let difference = subtract(middle, middle_of_ends);
    max_of(difference.x.abs(), difference.y.abs()) > TOLERANCE
}

fn cheap_distance_exceeds_limit(point: FloatPoint, x: f32, y: f32) -> bool {
    max_of((x - point.x).abs(), (y - point.y).abs()) > TOLERANCE
}

fn cubic_is_too_curvy(points: &[FloatPoint]) -> bool {
    let one_third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;
    cheap_distance_exceeds_limit(
        points[1],
        interpolate(points[0].x, points[3].x, one_third),
        interpolate(points[0].y, points[3].y, one_third),
    ) || cheap_distance_exceeds_limit(
        points[2],
        interpolate(points[0].x, points[3].x, two_thirds),
        interpolate(points[0].y, points[3].y, two_thirds),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SegmentKind {
    Line,
    Quad,
    Conic,
    Cubic,
}

#[derive(Clone, Copy)]
struct Segment {
    // The length of the contour up to the end of this segment.
    distance: f32,
    // Where the points of the curve that this segment is a part of start.
    point_index: usize,
    t_value: i32,
    kind: SegmentKind,
}

/// The measured segments of one contour.
pub(crate) struct ContourMeasure {
    segments: Vec<Segment>,
    // The points of the curves. A conic keeps its weight in the x of the point after its start.
    points: Vec<FloatPoint>,
    length: f32,
}

#[derive(Default)]
struct Builder {
    segments: Vec<Segment>,
    points: Vec<FloatPoint>,
}

impl Builder {
    fn push_segment(&mut self, distance: f32, point_index: usize, kind: SegmentKind, t_value: i32) {
        self.segments.push(Segment {
            distance,
            point_index,
            t_value,
            kind,
        });
    }

    // A segment counts only if it makes the distance larger. A very short one can be longer than
    // zero but still not change the sum.
    fn add_leaf(&mut self, distance: f32, length: f32, point_index: usize, kind: SegmentKind, t_value: i32) -> f32 {
        let previous = distance;
        let distance = distance + length;
        if distance > previous {
            self.push_segment(distance, point_index, kind, t_value);
        }
        distance
    }

    fn line(&mut self, start: FloatPoint, end: FloatPoint, distance: f32, point_index: usize) -> f32 {
        self.add_leaf(
            distance,
            self::distance(start, end),
            point_index,
            SegmentKind::Line,
            MAX_T_VALUE,
        )
    }

    fn quad(
        &mut self,
        points: &[FloatPoint],
        distance: f32,
        min_t: i32,
        max_t: i32,
        point_index: usize,
        depth: i32,
    ) -> f32 {
        if depth < MAX_RECURSION_DEPTH && t_span_is_big_enough(max_t - min_t) && quad_is_too_curvy(points) {
            let half_t = (min_t + max_t) >> 1;
            let chopped = chop_quad_at_half(points);
            let distance = self.quad(&chopped[..3], distance, min_t, half_t, point_index, depth + 1);
            return self.quad(&chopped[2..], distance, half_t, max_t, point_index, depth + 1);
        }
        self.add_leaf(
            distance,
            self::distance(points[0], points[2]),
            point_index,
            SegmentKind::Quad,
            max_t,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn conic(
        &mut self,
        conic: &Conic,
        distance: f32,
        min_t: i32,
        min_point: FloatPoint,
        max_t: i32,
        max_point: FloatPoint,
        point_index: usize,
        depth: i32,
    ) -> f32 {
        let half_t = (min_t + max_t) >> 1;
        let half_point = conic.eval_at(t_value_to_scalar(half_t));
        if !is_finite(half_point) {
            return distance;
        }
        if depth < MAX_RECURSION_DEPTH
            && t_span_is_big_enough(max_t - min_t)
            && conic_is_too_curvy(min_point, half_point, max_point)
        {
            let distance = self.conic(
                conic,
                distance,
                min_t,
                min_point,
                half_t,
                half_point,
                point_index,
                depth + 1,
            );
            return self.conic(
                conic,
                distance,
                half_t,
                half_point,
                max_t,
                max_point,
                point_index,
                depth + 1,
            );
        }
        self.add_leaf(
            distance,
            self::distance(min_point, max_point),
            point_index,
            SegmentKind::Conic,
            max_t,
        )
    }

    fn cubic(
        &mut self,
        points: &[FloatPoint],
        distance: f32,
        min_t: i32,
        max_t: i32,
        point_index: usize,
        depth: i32,
    ) -> f32 {
        if depth < MAX_RECURSION_DEPTH && t_span_is_big_enough(max_t - min_t) && cubic_is_too_curvy(points) {
            let half_t = (min_t + max_t) >> 1;
            let chopped = chop_cubic_at_half(points);
            let distance = self.cubic(&chopped[..4], distance, min_t, half_t, point_index, depth + 1);
            return self.cubic(&chopped[3..], distance, half_t, max_t, point_index, depth + 1);
        }
        self.add_leaf(
            distance,
            self::distance(points[0], points[3]),
            point_index,
            SegmentKind::Cubic,
            max_t,
        )
    }
}

/// Measures the contours of a path in order.
pub(crate) struct ContourMeasureIterator<'a> {
    view: PathView<'a>,
    verb_index: usize,
    point_index: usize,
    conic_weight_index: usize,
}

impl<'a> ContourMeasureIterator<'a> {
    pub(crate) fn new(view: PathView<'a>) -> Self {
        Self {
            view,
            verb_index: 0,
            point_index: 0,
            conic_weight_index: 0,
        }
    }

    fn build_segments(&mut self) -> Option<ContourMeasure> {
        let points = self.view.points;
        // The index of the last point in the builder's points, which can start a segment.
        let mut point_index: isize = -1;
        let mut distance = 0.0f32;
        let mut has_seen_close = false;
        let mut has_seen_move = false;
        let mut builder = Builder::default();

        while let Some(&verb) = self.view.verbs.get(self.verb_index) {
            if has_seen_move && verb == PathVerb::Move {
                break;
            }
            let start = self.point_index;
            match verb {
                PathVerb::Move => {
                    point_index += 1;
                    builder.points.push(points[start]);
                    has_seen_move = true;
                }
                PathVerb::Line => {
                    let previous = distance;
                    distance = builder.line(points[start - 1], points[start], distance, point_index as usize);
                    if distance > previous {
                        builder.points.push(points[start]);
                        point_index += 1;
                    }
                }
                PathVerb::Quad => {
                    let previous = distance;
                    distance = builder.quad(
                        &points[start - 1..start + 2],
                        distance,
                        0,
                        MAX_T_VALUE,
                        point_index as usize,
                        0,
                    );
                    if distance > previous {
                        builder.points.extend_from_slice(&points[start..start + 2]);
                        point_index += 2;
                    }
                }
                PathVerb::Conic => {
                    let weight = self.view.conic_weights[self.conic_weight_index];
                    let conic = Conic::new([points[start - 1], points[start], points[start + 1]], weight);
                    let previous = distance;
                    distance = builder.conic(
                        &conic,
                        distance,
                        0,
                        conic.points[0],
                        MAX_T_VALUE,
                        conic.points[2],
                        point_index as usize,
                        0,
                    );
                    if distance > previous {
                        builder.points.push(point(conic.weight, 0.0));
                        builder.points.extend_from_slice(&points[start..start + 2]);
                        point_index += 3;
                    }
                }
                PathVerb::Cubic => {
                    let previous = distance;
                    distance = builder.cubic(
                        &points[start - 1..start + 3],
                        distance,
                        0,
                        MAX_T_VALUE,
                        point_index as usize,
                        0,
                    );
                    if distance > previous {
                        builder.points.extend_from_slice(&points[start..start + 3]);
                        point_index += 3;
                    }
                }
                PathVerb::Close => has_seen_close = true,
            }
            self.point_index += verb.point_count();
            if verb == PathVerb::Conic {
                self.conic_weight_index += 1;
            }
            self.verb_index += 1;
        }

        if !distance.is_finite() || builder.segments.is_empty() {
            return None;
        }
        if has_seen_close {
            let previous = distance;
            let first = builder.points[0];
            let last_index = point_index as usize;
            distance = builder.line(builder.points[last_index], first, distance, last_index);
            if distance > previous {
                builder.points.push(first);
            }
        }
        Some(ContourMeasure {
            segments: builder.segments,
            points: builder.points,
            length: distance,
        })
    }
}

impl Iterator for ContourMeasureIterator<'_> {
    type Item = ContourMeasure;

    /// The next contour that has a length.
    fn next(&mut self) -> Option<ContourMeasure> {
        while self.verb_index < self.view.verbs.len() {
            if let Some(contour) = self.build_segments() {
                return Some(contour);
            }
        }
        None
    }
}

impl ContourMeasure {
    /// The first contour of the path that has a length.
    pub(crate) fn first_of(view: PathView<'_>) -> Option<ContourMeasure> {
        ContourMeasureIterator::new(view).next()
    }

    pub(crate) fn length(&self) -> f32 {
        self.length
    }

    fn distance_to_segment(&self, distance: f32) -> (usize, f32) {
        // Find the first segment that ends at or after the distance.
        let segments = &self.segments;
        let (mut low, mut high) = (0usize, segments.len() - 1);
        while low < high {
            let middle = (high + low) >> 1;
            if segments[middle].distance < distance {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if segments[high].distance < distance {
            high += 1;
        }
        let index = high;
        let segment = &segments[index];

        // Interpolate the parameter from the previous segment if it is a part of the same curve.
        let (mut start_t, mut start_distance) = (0.0f32, 0.0f32);
        if index > 0 {
            let previous = &segments[index - 1];
            start_distance = previous.distance;
            if previous.point_index == segment.point_index {
                start_t = t_value_to_scalar(previous.t_value);
            }
        }
        let t = start_t
            + (t_value_to_scalar(segment.t_value) - start_t) * (distance - start_distance)
                / (segment.distance - start_distance);
        (index, t)
    }

    /// The point at `distance` along the contour, and the unit tangent there.
    pub(crate) fn position_and_tangent(&self, distance: f32) -> Option<(FloatPoint, FloatPoint)> {
        if distance.is_nan() {
            return None;
        }
        let distance = if distance < 0.0 {
            0.0
        } else if distance > self.length {
            self.length
        } else {
            distance
        };
        let (index, t) = self.distance_to_segment(distance);
        if t.is_nan() {
            return None;
        }
        let segment = &self.segments[index];
        let points = &self.points[segment.point_index..];
        let (position, tangent) = match segment.kind {
            SegmentKind::Line => (
                interpolate_points(points[0], points[1], t),
                subtract(points[1], points[0]),
            ),
            SegmentKind::Quad => (eval_quad_at(points, t), eval_quad_tangent_at(points, t)),
            SegmentKind::Conic => {
                let conic = Conic::new([points[0], points[2], points[3]], points[1].x);
                (conic.eval_at(t), conic.eval_tangent_at(t))
            }
            SegmentKind::Cubic => (eval_cubic_at(points, t), eval_cubic_tangent_at(points, t)),
        };
        Some((position, normalize(tangent)))
    }
}

impl Path {
    /// The sum of the lengths of all contours.
    pub fn length(&self) -> f32 {
        ContourMeasureIterator::new(self.snapshot()).fold(0.0, |length, contour| length + contour.length())
    }
}
