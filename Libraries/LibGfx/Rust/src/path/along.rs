/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Points along a path, and polylines that follow it.

use super::geometry::{max_of, min_of, point, vector_length};
use super::measure::{ContourMeasure, ContourMeasureIterator, TOLERANCE};
use super::{Path, PathVerb, Segment};
use crate::geometry::FloatPoint;

// The most lines with which to follow a single curve, which bounds the number of vertices of a huge curve.
const MAXIMUM_LINE_COUNT_ALONG_CURVE: u32 = 4096;

/// A point along a path, and the unit tangent of the path there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointAlongPath {
    pub point: FloatPoint,
    pub tangent: FloatPoint,
}

/// A vertex of polylines that follow a path, with its distance along the path and the direction of
/// the path there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PathVertex {
    pub point: FloatPoint,
    pub distance: f32,
    pub direction: FloatPoint,
}

/// The number of lines with which to start following a curve, by Wang's formula: the square root of
/// the largest second difference of its control points, scaled by its degree, over the tolerance.
/// That keeps lines between points at equal parameter steps within the tolerance of the curve, and
/// depends on how much the curve bends rather than on how long it is.
fn line_count_to_follow_curve(segment: &Segment) -> u32 {
    let points = &segment.points;
    let second_difference = |index: usize| {
        vector_length(
            points[index].x - 2.0 * points[index + 1].x + points[index + 2].x,
            points[index].y - 2.0 * points[index + 1].y + points[index + 2].y,
        )
    };

    let scaled_second_difference = match segment.verb {
        PathVerb::Quad => second_difference(0) / 4.0,
        // A conic with a weight of at most 1, like the arcs of SVG paths, bends no more than the
        // quadratic with the same control points.
        PathVerb::Conic => max_of(1.0, segment.conic_weight) * second_difference(0) / 4.0,
        PathVerb::Cubic => 3.0 * max_of(second_difference(0), second_difference(1)) / 4.0,
        _ => return 1,
    };
    let line_count = (f64::from(scaled_second_difference) / f64::from(TOLERANCE))
        .sqrt()
        .ceil();
    line_count.clamp(1.0, f64::from(MAXIMUM_LINE_COUNT_ALONG_CURVE)) as u32
}

fn distance_between(a: FloatPoint, b: FloatPoint) -> f32 {
    if a == b {
        return 0.0;
    }
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    (dx * dx + dy * dy).sqrt()
}

struct CurveToFollow<'a> {
    measure: &'a ContourMeasure,
    distance_to_curve: f32,
    vertices: &'a mut Vec<PathVertex>,
    remaining_line_count: u32,
}

/// Appends the vertices between two points along a curve, splitting the line between them in half
/// for as long as the curve passes further than the tolerance from it halfway, or until the curve
/// has as many lines as it may have.
fn follow_curve_between(
    curve: &mut CurveToFollow<'_>,
    start_distance: f32,
    start_point: FloatPoint,
    end_distance: f32,
    end_point: FloatPoint,
) {
    if curve.remaining_line_count == 0 {
        return;
    }
    let middle_distance = (start_distance + end_distance) / 2.0;
    let Some((middle_point, tangent)) = curve.measure.position_and_tangent(middle_distance) else {
        return;
    };
    let middle_of_line = point((start_point.x + end_point.x) / 2.0, (start_point.y + end_point.y) / 2.0);
    if distance_between(middle_point, middle_of_line) <= TOLERANCE {
        return;
    }

    curve.remaining_line_count -= 1;
    follow_curve_between(curve, start_distance, start_point, middle_distance, middle_point);
    curve.vertices.push(PathVertex {
        point: middle_point,
        distance: curve.distance_to_curve + middle_distance,
        direction: tangent,
    });
    follow_curve_between(curve, middle_distance, middle_point, end_distance, end_point);
}

impl Path {
    /// The point at `distance` along the path, clamped to its ends, and the unit tangent of the
    /// path there. The gaps between the contours do not add to the distance. A path without length
    /// is only its start point, where it has no tangent.
    pub fn point_at_length(&self, distance: f32) -> Option<PointAlongPath> {
        let view = self.snapshot();
        let mut end_of_path = None;
        let mut remaining_distance = max_of(distance, 0.0);
        for contour in ContourMeasureIterator::new(view) {
            let contour_length = contour.length();
            let distance_in_contour = min_of(remaining_distance, contour_length);
            let Some((point, tangent)) = contour.position_and_tangent(distance_in_contour) else {
                continue;
            };
            end_of_path = Some(PointAlongPath { point, tangent });
            if remaining_distance <= contour_length {
                return end_of_path;
            }
            remaining_distance -= contour_length;
        }

        end_of_path.or_else(|| {
            view.points.first().map(|&point| PointAlongPath {
                point,
                tangent: FloatPoint::default(),
            })
        })
    }

    /// The vertices of polylines through the ends of the segments of the path, with their distance
    /// along it. Along a curved segment, the polylines pass through points along the curve that
    /// keep them within the tolerance of the measure. The gaps between the contours do not add to
    /// the distance, so a contour starts at the distance its predecessor ends at. A path without
    /// length is only its start point.
    pub fn vertices(&self) -> Vec<PathVertex> {
        let view = self.snapshot();

        // Each vertex has the unit tangent of the path there as its direction. Where the path
        // changes direction at once, the vertex appears twice: with the direction in which the
        // path arrives, and with the one in which it leaves. A contour start that no segment with
        // length leaves from has no direction.
        let mut vertices: Vec<PathVertex> = Vec::new();
        let mut distance = 0.0f32;
        let mut segment_path = Path::new();
        for segment in view.segments() {
            let points = &segment.points;
            segment_path.clear();
            segment_path.sk_move_to(points[0]);
            let end_point = match segment.verb {
                PathVerb::Move => {
                    vertices.push(PathVertex {
                        point: points[0],
                        distance,
                        direction: FloatPoint::default(),
                    });
                    continue;
                }
                PathVerb::Line | PathVerb::Close => {
                    segment_path.sk_line_to(points[1]);
                    points[1]
                }
                PathVerb::Quad => {
                    segment_path.sk_quad_to(points[1], points[2]);
                    points[2]
                }
                PathVerb::Conic => {
                    segment_path.sk_conic_to(points[1], points[2], segment.conic_weight);
                    points[2]
                }
                PathVerb::Cubic => {
                    segment_path.sk_cubic_to(points[1], points[2], points[3]);
                    points[3]
                }
            };

            // A segment without length has no direction, and leaves the path where it was.
            let Some(measure) = ContourMeasure::first_of(segment_path.snapshot()) else {
                continue;
            };
            let segment_length = measure.length();
            let direction_at = |distance_in_segment: f32| {
                measure
                    .position_and_tangent(distance_in_segment)
                    .map_or(FloatPoint::default(), |(_, tangent)| tangent)
            };

            let start_direction = direction_at(0.0);
            match vertices.last_mut() {
                Some(last) if last.direction == FloatPoint::default() => last.direction = start_direction,
                Some(last) if last.direction == start_direction => {}
                _ => vertices.push(PathVertex {
                    point: points[0],
                    distance,
                    direction: start_direction,
                }),
            }

            let line_count = line_count_to_follow_curve(&segment);
            let mut curve = CurveToFollow {
                measure: &measure,
                distance_to_curve: distance,
                vertices: &mut vertices,
                remaining_line_count: MAXIMUM_LINE_COUNT_ALONG_CURVE - line_count,
            };
            let mut previous_distance = 0.0f32;
            let mut previous_point = points[0];
            for line in 1..=line_count {
                let distance_in_segment = segment_length * line as f32 / line_count as f32;
                let (position, tangent) = if line == line_count {
                    (end_point, FloatPoint::default())
                } else {
                    let Some(position_and_tangent) = measure.position_and_tangent(distance_in_segment) else {
                        continue;
                    };
                    position_and_tangent
                };
                follow_curve_between(
                    &mut curve,
                    previous_distance,
                    previous_point,
                    distance_in_segment,
                    position,
                );
                if line < line_count {
                    curve.vertices.push(PathVertex {
                        point: position,
                        distance: distance + distance_in_segment,
                        direction: tangent,
                    });
                }
                previous_distance = distance_in_segment;
                previous_point = position;
            }
            distance += segment_length;
            vertices.push(PathVertex {
                point: end_point,
                distance,
                direction: direction_at(segment_length),
            });
        }

        // The segments leave out a move that ends the path.
        if vertices.is_empty()
            && let Some(&point) = view.points.first()
        {
            vertices.push(PathVertex {
                point,
                distance: 0.0,
                direction: FloatPoint::default(),
            });
        }
        vertices
    }
}
