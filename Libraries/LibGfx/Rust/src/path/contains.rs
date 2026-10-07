/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkPathPriv.cpp (SkPathPriv::Contains), skia/src/core/SkPathPriv.h
// (SkPathEdgeIter) and skia/src/core/SkCubicClipper.cpp (SkCubicClipper::ChopMonoAtY).

use super::geometry::{
    Conic, NEARLY_ZERO, chop_cubic_at_y_extrema, chop_quad_at_y_extrema, cross, eval_cubic_tangent_at,
    eval_quad_tangent_at, find_unit_quad_roots, interpolate, length_squared, max_of, min_of, nearly_equal, sign_as_int,
};
use super::{Path, PathFillType, PathVerb, PathView, bounds};
use crate::geometry::FloatPoint;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    Line,
    Quad,
    Conic,
    Cubic,
}

#[derive(Clone, Copy)]
struct Edge {
    kind: EdgeKind,
    points: [FloatPoint; 4],
    conic_weight: f32,
}

/// The segments of each contour, without moves and closes. Each contour that has a segment ends
/// with a line back to its start, also when it is not closed and also when the line has no
/// length.
fn for_each_edge(view: &PathView<'_>, mut callback: impl FnMut(Edge)) {
    let points = view.points;
    let mut point_index = 0;
    let mut conic_weight_index = 0;
    let mut move_to_index = 0;
    let mut needs_close_line = false;
    let close_line = |last_index: usize, move_to_index: usize| Edge {
        kind: EdgeKind::Line,
        points: [
            points[last_index],
            points[move_to_index],
            FloatPoint::default(),
            FloatPoint::default(),
        ],
        conic_weight: 0.0,
    };
    for &verb in view.verbs {
        match verb {
            PathVerb::Move => {
                if needs_close_line {
                    callback(close_line(point_index - 1, move_to_index));
                    needs_close_line = false;
                }
                move_to_index = point_index;
                point_index += 1;
            }
            PathVerb::Close => {
                if needs_close_line {
                    callback(close_line(point_index - 1, move_to_index));
                    needs_close_line = false;
                }
            }
            _ => {
                let count = verb.point_count();
                let mut edge = Edge {
                    kind: match verb {
                        PathVerb::Line => EdgeKind::Line,
                        PathVerb::Quad => EdgeKind::Quad,
                        PathVerb::Conic => EdgeKind::Conic,
                        _ => EdgeKind::Cubic,
                    },
                    points: [FloatPoint::default(); 4],
                    conic_weight: 0.0,
                };
                edge.points[..=count].copy_from_slice(&points[point_index - 1..point_index + count]);
                if verb == PathVerb::Conic {
                    edge.conic_weight = view.conic_weights[conic_weight_index];
                    conic_weight_index += 1;
                }
                point_index += count;
                needs_close_line = true;
                callback(edge);
            }
        }
    }
    if needs_close_line {
        callback(close_line(point_index - 1, move_to_index));
    }
}

fn poly_eval_3(a: f32, b: f32, c: f32, t: f32) -> f32 {
    (a * t + b) * t + c
}

fn poly_eval_4(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    ((a * t + b) * t + c) * t + d
}

fn between(a: f32, b: f32, c: f32) -> bool {
    (a - b) * (c - b) <= 0.0
}

fn eval_cubic_points(c0: f32, c1: f32, c2: f32, c3: f32, t: f32) -> f32 {
    let a = c3 + 3.0 * (c1 - c2) - c0;
    let b = 3.0 * (c2 - c1 - c1 + c0);
    let c = 3.0 * (c1 - c0);
    poly_eval_4(a, b, c, c0, t)
}

fn check_on_curve(x: f32, y: f32, start: FloatPoint, end: FloatPoint) -> bool {
    if start.y == end.y {
        between(start.x, x, end.x) && x != end.x
    } else {
        x == start.x && y == start.y
    }
}

/// The parameter where a cubic that only goes one way in y reaches `y`, found by bisection.
fn chop_mono_cubic_at_y(points: &[FloatPoint], y: f32) -> Option<f32> {
    let curve = [points[0].y - y, points[1].y - y, points[2].y - y, points[3].y - y];
    let (mut t_negative, mut t_positive);
    if curve[0] < 0.0 {
        if curve[3] < 0.0 {
            return None;
        }
        t_negative = 0.0f32;
        t_positive = 1.0f32;
    } else if curve[0] > 0.0 {
        if curve[3] > 0.0 {
            return None;
        }
        t_negative = 1.0;
        t_positive = 0.0;
    } else {
        return Some(0.0);
    }
    const TOLERANCE: f32 = 1.0 / 65536.0;
    loop {
        let t_middle = (t_positive + t_negative) / 2.0;
        let y01 = interpolate(curve[0], curve[1], t_middle);
        let y12 = interpolate(curve[1], curve[2], t_middle);
        let y23 = interpolate(curve[2], curve[3], t_middle);
        let y012 = interpolate(y01, y12, t_middle);
        let y123 = interpolate(y12, y23, t_middle);
        let y0123 = interpolate(y012, y123, t_middle);
        if y0123 == 0.0 {
            return Some(t_middle);
        }
        if y0123 < 0.0 {
            t_negative = t_middle;
        } else {
            t_positive = t_middle;
        }
        if (t_positive - t_negative).abs() <= TOLERANCE {
            break;
        }
    }
    Some((t_negative + t_positive) / 2.0)
}

fn winding_mono_cubic(points: &[FloatPoint], x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    let mut y0 = points[0].y;
    let mut y3 = points[3].y;
    let mut direction = 1;
    if y0 > y3 {
        std::mem::swap(&mut y0, &mut y3);
        direction = -1;
    }
    if y < y0 || y > y3 {
        return 0;
    }
    if check_on_curve(x, y, points[0], points[3]) {
        *on_curve_count += 1;
        return 0;
    }
    if y == y3 {
        return 0;
    }
    // Reject or accept quickly on the bounds.
    let (mut min, mut max) = (points[0].x, points[0].x);
    for point in &points[1..4] {
        min = min_of(min, point.x);
        max = max_of(max, point.x);
    }
    if x < min {
        return 0;
    }
    if x > max {
        return direction;
    }
    let Some(t) = chop_mono_cubic_at_y(points, y) else {
        return 0;
    };
    let xt = eval_cubic_points(points[0].x, points[1].x, points[2].x, points[3].x, t);
    // An end point is the start point of the next segment, so it does not count here.
    if nearly_equal(xt, x) && (x != points[3].x || y != points[3].y) {
        *on_curve_count += 1;
        return 0;
    }
    if xt < x { direction } else { 0 }
}

fn winding_cubic(points: &[FloatPoint], x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    let (chopped, count) = chop_cubic_at_y_extrema(points);
    (0..=count)
        .map(|index| winding_mono_cubic(&chopped[index * 3..], x, y, on_curve_count))
        .sum()
}

fn conic_eval_numerator(c0: f32, c1: f32, c2: f32, weight: f32, t: f32) -> f64 {
    let c1_weighted = c1 * weight;
    let c = c0;
    let a = c2 - 2.0 * c1_weighted + c;
    let b = 2.0 * (c1_weighted - c);
    f64::from(poly_eval_3(a, b, c, t))
}

fn conic_eval_denominator(weight: f32, t: f32) -> f64 {
    let b = 2.0 * (weight - 1.0);
    let c = 1.0;
    let a = -b;
    f64::from(poly_eval_3(a, b, c, t))
}

fn conic_x_at(conic: &Conic, t: f32) -> f32 {
    let [p0, p1, p2] = conic.points;
    (conic_eval_numerator(p0.x, p1.x, p2.x, conic.weight, t) / conic_eval_denominator(conic.weight, t)) as f32
}

fn conic_y_roots(points: &[FloatPoint; 3], weight: f32, y: f32) -> ([f32; 2], usize) {
    let mut a = points[2].y;
    let mut b = points[1].y * weight - y * weight + y;
    let mut c = points[0].y;
    a += c - 2.0 * b;
    b -= c;
    c -= y;
    find_unit_quad_roots(a, 2.0 * b, c)
}

fn winding_mono_conic(conic: &Conic, x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    let points = &conic.points;
    let mut y0 = points[0].y;
    let mut y2 = points[2].y;
    let mut direction = 1;
    if y0 > y2 {
        std::mem::swap(&mut y0, &mut y2);
        direction = -1;
    }
    if y < y0 || y > y2 {
        return 0;
    }
    if check_on_curve(x, y, points[0], points[2]) {
        *on_curve_count += 1;
        return 0;
    }
    if y == y2 {
        return 0;
    }
    let (roots, count) = conic_y_roots(points, conic.weight, y);
    let xt = if count == 0 {
        // There are no roots only when y0 is y. Take the start of the conic in its direction.
        points[(1 - direction) as usize].x
    } else {
        conic_x_at(conic, roots[0])
    };
    if nearly_equal(xt, x) && (x != points[2].x || y != points[2].y) {
        *on_curve_count += 1;
        return 0;
    }
    if xt < x { direction } else { 0 }
}

fn is_mono_quad(y0: f32, y1: f32, y2: f32) -> bool {
    if y0 == y1 {
        return true;
    }
    if y0 < y1 { y1 <= y2 } else { y1 >= y2 }
}

fn winding_conic(points: &[FloatPoint], x: f32, y: f32, weight: f32, on_curve_count: &mut i32) -> i32 {
    let conic = Conic::new([points[0], points[1], points[2]], weight);
    // A conic with very large points can fail to split even if it is not monotonic. Then it stays
    // in one part.
    if !is_mono_quad(points[0].y, points[1].y, points[2].y)
        && let Some(chopped) = conic.chop_at_y_extrema()
    {
        return winding_mono_conic(&chopped[0], x, y, on_curve_count)
            + winding_mono_conic(&chopped[1], x, y, on_curve_count);
    }
    winding_mono_conic(&conic, x, y, on_curve_count)
}

fn quad_x_at(points: &[FloatPoint], t: f32) -> f32 {
    let c = points[0].x;
    let a = points[2].x - 2.0 * points[1].x + c;
    let b = 2.0 * (points[1].x - c);
    poly_eval_3(a, b, c, t)
}

fn quad_y_roots(points: &[FloatPoint], y: f32) -> ([f32; 2], usize) {
    find_unit_quad_roots(
        points[0].y - 2.0 * points[1].y + points[2].y,
        2.0 * (points[1].y - points[0].y),
        points[0].y - y,
    )
}

fn winding_mono_quad(points: &[FloatPoint], x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    let mut y0 = points[0].y;
    let mut y2 = points[2].y;
    let mut direction = 1;
    if y0 > y2 {
        std::mem::swap(&mut y0, &mut y2);
        direction = -1;
    }
    if y < y0 || y > y2 {
        return 0;
    }
    if check_on_curve(x, y, points[0], points[2]) {
        *on_curve_count += 1;
        return 0;
    }
    if y == y2 {
        return 0;
    }
    let (roots, count) = quad_y_roots(points, y);
    let xt = if count == 0 {
        points[(1 - direction) as usize].x
    } else {
        quad_x_at(points, roots[0])
    };
    if nearly_equal(xt, x) && (x != points[2].x || y != points[2].y) {
        *on_curve_count += 1;
        return 0;
    }
    if xt < x { direction } else { 0 }
}

fn winding_quad(points: &[FloatPoint], x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    if is_mono_quad(points[0].y, points[1].y, points[2].y) {
        return winding_mono_quad(points, x, y, on_curve_count);
    }
    let (chopped, count) = chop_quad_at_y_extrema(points);
    let mut winding = winding_mono_quad(&chopped, x, y, on_curve_count);
    if count > 0 {
        winding += winding_mono_quad(&chopped[2..], x, y, on_curve_count);
    }
    winding
}

fn winding_line(points: &[FloatPoint], x: f32, y: f32, on_curve_count: &mut i32) -> i32 {
    let (x0, mut y0) = (points[0].x, points[0].y);
    let (x1, mut y1) = (points[1].x, points[1].y);
    let dy = y1 - y0;
    let mut direction = 1;
    if y0 > y1 {
        std::mem::swap(&mut y0, &mut y1);
        direction = -1;
    }
    if y < y0 || y > y1 {
        return 0;
    }
    if check_on_curve(x, y, points[0], points[1]) {
        *on_curve_count += 1;
        return 0;
    }
    if y == y1 {
        return 0;
    }
    let cross = (x1 - x0) * (y - points[0].y) - dy * (x - x0);
    if cross == 0.0 {
        // The point is on the line. The end point is handled above.
        if x != x1 || y != points[1].y {
            *on_curve_count += 1;
        }
        direction = 0;
    } else if sign_as_int(cross) == direction {
        direction = 0;
    }
    direction
}

fn tangent_cubic(points: &[FloatPoint], x: f32, y: f32, tangents: &mut Vec<FloatPoint>) {
    if !between(points[0].y, y, points[1].y)
        && !between(points[1].y, y, points[2].y)
        && !between(points[2].y, y, points[3].y)
    {
        return;
    }
    if !between(points[0].x, x, points[1].x)
        && !between(points[1].x, x, points[2].x)
        && !between(points[2].x, x, points[3].x)
    {
        return;
    }
    let (chopped, count) = chop_cubic_at_y_extrema(points);
    for index in 0..=count {
        let cubic = &chopped[index * 3..index * 3 + 4];
        let Some(t) = chop_mono_cubic_at_y(cubic, y) else {
            continue;
        };
        let xt = eval_cubic_points(cubic[0].x, cubic[1].x, cubic[2].x, cubic[3].x, t);
        if !nearly_equal(x, xt) {
            continue;
        }
        tangents.push(eval_cubic_tangent_at(cubic, t));
    }
}

fn tangent_conic(points: &[FloatPoint], x: f32, y: f32, weight: f32, tangents: &mut Vec<FloatPoint>) {
    if !between(points[0].y, y, points[1].y) && !between(points[1].y, y, points[2].y) {
        return;
    }
    if !between(points[0].x, x, points[1].x) && !between(points[1].x, x, points[2].x) {
        return;
    }
    let raw_points = [points[0], points[1], points[2]];
    let (roots, count) = conic_y_roots(&raw_points, weight, y);
    for &t in &roots[..count] {
        // The weight is used as it is here, also if it is not positive.
        let xt = (conic_eval_numerator(points[0].x, points[1].x, points[2].x, weight, t)
            / conic_eval_denominator(weight, t)) as f32;
        if !nearly_equal(x, xt) {
            continue;
        }
        tangents.push(Conic::new(raw_points, weight).eval_tangent_at(t));
    }
}

fn tangent_quad(points: &[FloatPoint], x: f32, y: f32, tangents: &mut Vec<FloatPoint>) {
    if !between(points[0].y, y, points[1].y) && !between(points[1].y, y, points[2].y) {
        return;
    }
    if !between(points[0].x, x, points[1].x) && !between(points[1].x, x, points[2].x) {
        return;
    }
    let (roots, count) = quad_y_roots(points, y);
    for &t in &roots[..count] {
        let xt = quad_x_at(points, t);
        if !nearly_equal(x, xt) {
            continue;
        }
        tangents.push(eval_quad_tangent_at(points, t));
    }
}

fn tangent_line(points: &[FloatPoint], x: f32, y: f32, tangents: &mut Vec<FloatPoint>) {
    let (y0, y1) = (points[0].y, points[1].y);
    if !between(y0, y, y1) {
        return;
    }
    let (x0, x1) = (points[0].x, points[1].x);
    if !between(x0, x, x1) {
        return;
    }
    let dx = x1 - x0;
    let dy = y1 - y0;
    if !nearly_equal((x - x0) * dy, dx * (y - y0)) {
        return;
    }
    tangents.push(FloatPoint { x: dx, y: dy });
}

fn view_contains(view: &PathView<'_>, point: FloatPoint) -> bool {
    let fill_type = view.fill_type;
    let is_inverse = fill_type.is_inverse();
    if view.is_empty() {
        return is_inverse;
    }
    let Some([left, top, right, bottom]) = bounds::trimmed_bounds(view.points, view.verbs) else {
        return is_inverse;
    };
    let (x, y) = (point.x, point.y);
    if !(left <= x && x <= right && top <= y && y <= bottom) {
        return is_inverse;
    }

    let mut winding = 0;
    let mut on_curve_count = 0;
    for_each_edge(view, |edge| {
        let points = &edge.points;
        winding += match edge.kind {
            EdgeKind::Line => winding_line(points, x, y, &mut on_curve_count),
            EdgeKind::Quad => winding_quad(points, x, y, &mut on_curve_count),
            EdgeKind::Conic => winding_conic(points, x, y, edge.conic_weight, &mut on_curve_count),
            EdgeKind::Cubic => winding_cubic(points, x, y, &mut on_curve_count),
        };
    });
    let even_odd = fill_type.is_even_odd();
    if even_odd {
        winding &= 1;
    }
    if winding != 0 {
        return !is_inverse;
    }
    if on_curve_count <= 1 {
        return (on_curve_count != 0) ^ is_inverse;
    }
    if (on_curve_count & 1) != 0 || even_odd {
        return ((on_curve_count & 1) != 0) ^ is_inverse;
    }
    // The point is on an even number of curves, and the fill is winding. Curves that meet there
    // with the same tangent in opposite directions cancel out.
    let mut tangents: Vec<FloatPoint> = Vec::new();
    for_each_edge(view, |edge| {
        let old_count = tangents.len();
        let points = &edge.points;
        match edge.kind {
            EdgeKind::Line => tangent_line(points, x, y, &mut tangents),
            EdgeKind::Quad => tangent_quad(points, x, y, &mut tangents),
            EdgeKind::Conic => tangent_conic(points, x, y, edge.conic_weight, &mut tangents),
            EdgeKind::Cubic => tangent_cubic(points, x, y, &mut tangents),
        }
        if tangents.len() > old_count {
            let last = tangents.len() - 1;
            let tangent = tangents[last];
            if length_squared(tangent).abs() <= NEARLY_ZERO {
                tangents.remove(last);
            } else {
                for index in 0..last {
                    let test = tangents[index];
                    if cross(test, tangent).abs() <= NEARLY_ZERO
                        && sign_as_int(tangent.x * test.x) <= 0
                        && sign_as_int(tangent.y * test.y) <= 0
                    {
                        tangents.remove(last);
                        tangents.swap_remove(index);
                        break;
                    }
                }
            }
        }
    });
    !tangents.is_empty() ^ is_inverse
}

impl Path {
    /// True if `point` is inside the path with the given fill type, as the compositor fills it.
    pub fn contains(&self, point: FloatPoint, fill_type: PathFillType) -> bool {
        // view_contains() treats a path without finite bounds as empty, as the snapshot does.
        let mut view = self.raw_view();
        view.fill_type = fill_type;
        view_contains(&view, point)
    }
}
