/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkGeometry.{h,cpp}, skia/src/core/SkPoint.cpp and
// skia/include/private/base/SkPoint_impl.h.

//! Curve arithmetic. Each function keeps the order of operations and the float and double steps
//! of the original, because path lengths, hit tests and the text form of a path depend on the
//! exact bits.

use crate::geometry::FloatPoint;

pub(crate) const NEARLY_ZERO: f32 = 1.0 / 4096.0;

pub(crate) fn point(x: f32, y: f32) -> FloatPoint {
    FloatPoint { x, y }
}

pub(crate) fn add(a: FloatPoint, b: FloatPoint) -> FloatPoint {
    point(a.x + b.x, a.y + b.y)
}

pub(crate) fn subtract(a: FloatPoint, b: FloatPoint) -> FloatPoint {
    point(a.x - b.x, a.y - b.y)
}

pub(crate) fn scale(a: FloatPoint, factor: f32) -> FloatPoint {
    point(a.x * factor, a.y * factor)
}

fn multiply(a: FloatPoint, b: FloatPoint) -> FloatPoint {
    point(a.x * b.x, a.y * b.y)
}

fn times_2(a: FloatPoint) -> FloatPoint {
    add(a, a)
}

pub(crate) fn is_finite(point: FloatPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

pub(crate) fn are_finite(points: &[FloatPoint]) -> bool {
    points.iter().all(|point| is_finite(*point))
}

pub(crate) fn cross(a: FloatPoint, b: FloatPoint) -> f32 {
    a.x * b.y - a.y * b.x
}

pub(crate) fn length_squared(a: FloatPoint) -> f32 {
    a.x * a.x + a.y * a.y
}

/// The comparison that the original's std::max() makes, which keeps a NaN in the first argument.
pub(crate) fn max_of(a: f32, b: f32) -> f32 {
    if a < b { b } else { a }
}

/// As above, for std::min().
pub(crate) fn min_of(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}

pub(crate) fn sign_as_int(value: f32) -> i32 {
    if value < 0.0 {
        -1
    } else if value > 0.0 {
        1
    } else {
        0
    }
}

pub(crate) fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= NEARLY_ZERO
}

pub(crate) fn interpolate(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn valid_unit_divide(mut numerator: f32, mut denominator: f32) -> Option<f32> {
    if numerator < 0.0 {
        numerator = -numerator;
        denominator = -denominator;
    }
    if denominator == 0.0 || numerator == 0.0 || numerator >= denominator {
        return None;
    }
    let ratio = numerator / denominator;
    if ratio.is_nan() || ratio == 0.0 {
        return None;
    }
    Some(ratio)
}

/// The roots of A t^2 + B t + C between 0 and 1, both exclusive, in increasing order.
pub(crate) fn find_unit_quad_roots(a: f32, b: f32, c: f32) -> ([f32; 2], usize) {
    let mut roots = [0.0f32; 2];
    if a == 0.0 {
        return match valid_unit_divide(-c, b) {
            Some(root) => ([root, 0.0], 1),
            None => (roots, 0),
        };
    }
    let mut discriminant = f64::from(b) * f64::from(b) - 4.0 * f64::from(a) * f64::from(c);
    if discriminant < 0.0 {
        return (roots, 0);
    }
    discriminant = discriminant.sqrt();
    let r = discriminant as f32;
    if !r.is_finite() {
        return (roots, 0);
    }
    let q = if b < 0.0 { -(b - r) / 2.0 } else { -(b + r) / 2.0 };
    let mut count = 0;
    if let Some(root) = valid_unit_divide(q, a) {
        roots[count] = root;
        count += 1;
    }
    if let Some(root) = valid_unit_divide(c, q) {
        roots[count] = root;
        count += 1;
    }
    if count == 2 {
        if roots[0] > roots[1] {
            roots.swap(0, 1);
        } else if roots[0] == roots[1] {
            count -= 1;
        }
    }
    (roots, count)
}

fn is_not_monotonic(a: f32, b: f32, c: f32) -> bool {
    let ab = a - b;
    let mut bc = b - c;
    if ab < 0.0 {
        bc = -bc;
    }
    ab == 0.0 || bc < 0.0
}

/// A t^2 + B t + C, for each coordinate.
#[derive(Clone, Copy)]
pub(crate) struct QuadCoefficients {
    pub a: FloatPoint,
    pub b: FloatPoint,
    pub c: FloatPoint,
}

impl QuadCoefficients {
    pub(crate) fn eval(&self, t: FloatPoint) -> FloatPoint {
        add(multiply(add(multiply(self.a, t), self.b), t), self.c)
    }
}

pub(crate) fn eval_quad_tangent_at(points: &[FloatPoint], t: f32) -> FloatPoint {
    // The derivative is zero at an end where the control point is on the end point. Use the end
    // points then.
    if (t == 0.0 && points[0] == points[1]) || (t == 1.0 && points[1] == points[2]) {
        return subtract(points[2], points[0]);
    }
    let b = subtract(points[1], points[0]);
    let a = subtract(subtract(points[2], points[1]), b);
    let tangent = add(scale(a, t), b);
    add(tangent, tangent)
}

fn interpolate_points(a: FloatPoint, b: FloatPoint, t: f32) -> FloatPoint {
    add(a, scale(subtract(b, a), t))
}

pub(crate) fn chop_quad_at(points: &[FloatPoint], t: f32) -> [FloatPoint; 5] {
    let (p0, p1, p2) = (points[0], points[1], points[2]);
    let p01 = interpolate_points(p0, p1, t);
    let p12 = interpolate_points(p1, p2, t);
    [p0, p01, interpolate_points(p01, p12, t), p12, p2]
}

/// Splits a quad where its y coordinate turns, so that each part only goes up or down. Gives the
/// number of splits.
pub(crate) fn chop_quad_at_y_extrema(points: &[FloatPoint]) -> ([FloatPoint; 5], usize) {
    let a = points[0].y;
    let mut b = points[1].y;
    let c = points[2].y;
    if is_not_monotonic(a, b, c) {
        if let Some(t) = valid_unit_divide(a - b, a - b - b + c) {
            let mut chopped = chop_quad_at(points, t);
            chopped[1].y = chopped[2].y;
            chopped[3].y = chopped[2].y;
            return (chopped, 1);
        }
        // The division can fail, for example with an underflow. Make the quad monotonic anyway.
        b = if (a - b).abs() < (b - c).abs() { a } else { c };
    }
    let mut result = [FloatPoint::default(); 5];
    result[0] = point(points[0].x, a);
    result[1] = point(points[1].x, b);
    result[2] = point(points[2].x, c);
    (result, 0)
}

fn eval_cubic_derivative(points: &[FloatPoint], t: f32) -> FloatPoint {
    let (p0, p1, p2, p3) = (points[0], points[1], points[2], points[3]);
    let coefficients = QuadCoefficients {
        a: subtract(add(p3, scale(subtract(p1, p2), 3.0)), p0),
        b: times_2(add(subtract(p2, times_2(p1)), p0)),
        c: subtract(p1, p0),
    };
    coefficients.eval(point(t, t))
}

pub(crate) fn eval_cubic_tangent_at(points: &[FloatPoint], t: f32) -> FloatPoint {
    // The derivative is zero at an end where the next control point is on the end point. Use the
    // other control point then, or the end points.
    if (t == 0.0 && points[0] == points[1]) || (t == 1.0 && points[2] == points[3]) {
        let mut tangent = if t == 0.0 {
            subtract(points[2], points[0])
        } else {
            subtract(points[3], points[1])
        };
        if tangent.x == 0.0 && tangent.y == 0.0 {
            tangent = subtract(points[3], points[0]);
        }
        return tangent;
    }
    eval_cubic_derivative(points, t)
}

/// Gives the parameter values between 0 and 1 where one coordinate of a cubic turns.
fn find_cubic_extrema(a: f32, b: f32, c: f32, d: f32) -> ([f32; 2], usize) {
    let coefficient_a = d - a + 3.0 * (b - c);
    let coefficient_b = 2.0 * (a - b - b + c);
    let coefficient_c = b - a;
    find_unit_quad_roots(coefficient_a, coefficient_b, coefficient_c)
}

// This does not give b when t is 1, but it is more precise at cusps than a * (1 - t) + b * t.
fn unchecked_mix(a: FloatPoint, b: FloatPoint, t: FloatPoint) -> FloatPoint {
    add(multiply(subtract(b, a), t), a)
}

pub(crate) fn chop_cubic_at(points: &[FloatPoint], t: f32) -> [FloatPoint; 7] {
    if t == 1.0 {
        return [
            points[0], points[1], points[2], points[3], points[3], points[3], points[3],
        ];
    }
    let (p0, p1, p2, p3) = (points[0], points[1], points[2], points[3]);
    let tt = point(t, t);
    let ab = unchecked_mix(p0, p1, tt);
    let bc = unchecked_mix(p1, p2, tt);
    let cd = unchecked_mix(p2, p3, tt);
    let abc = unchecked_mix(ab, bc, tt);
    let bcd = unchecked_mix(bc, cd, tt);
    let abcd = unchecked_mix(abc, bcd, tt);
    [p0, ab, abc, abcd, bcd, cd, p3]
}

/// Splits a cubic at two parameter values in one pass, as the original does with four lanes.
fn chop_cubic_at_two(points: &[FloatPoint], t0: f32, t1: f32) -> [FloatPoint; 10] {
    let mut result = [FloatPoint::default(); 10];
    if t1 == 1.0 {
        result[..7].copy_from_slice(&chop_cubic_at(points, t0));
        result[7] = points[3];
        result[8] = points[3];
        result[9] = points[3];
        return result;
    }
    let (p0, p1, p2, p3) = (points[0], points[1], points[2], points[3]);
    let first = point(t0, t0);
    let second = point(t1, t1);
    let mix_both =
        |a: [FloatPoint; 2], b: [FloatPoint; 2]| [unchecked_mix(a[0], b[0], first), unchecked_mix(a[1], b[1], second)];
    let ab = mix_both([p0, p0], [p1, p1]);
    let bc = mix_both([p1, p1], [p2, p2]);
    let cd = mix_both([p2, p2], [p3, p3]);
    let abc = mix_both(ab, bc);
    let bcd = mix_both(bc, cd);
    let abcd = mix_both(abc, bcd);
    let middle = [
        unchecked_mix(abc[0], bcd[0], second),
        unchecked_mix(abc[1], bcd[1], first),
    ];
    result[0] = p0;
    result[1] = ab[0];
    result[2] = abc[0];
    result[3] = abcd[0];
    result[4] = middle[0];
    result[5] = middle[1];
    result[6] = abcd[1];
    result[7] = bcd[1];
    result[8] = cd[1];
    result[9] = p3;
    result
}

/// Splits a cubic where its y coordinate turns, so that each part only goes up or down. Gives
/// the number of splits, so the parts are at 0..4, 3..7 and 6..10.
pub(crate) fn chop_cubic_at_y_extrema(points: &[FloatPoint]) -> ([FloatPoint; 10], usize) {
    let (t_values, count) = find_cubic_extrema(points[0].y, points[1].y, points[2].y, points[3].y);
    let mut result = [FloatPoint::default(); 10];
    match count {
        0 => result[..4].copy_from_slice(&points[..4]),
        1 => result[..7].copy_from_slice(&chop_cubic_at(points, t_values[0])),
        _ => result = chop_cubic_at_two(points, t_values[0], t_values[1]),
    }
    // Make the turns flat.
    if count > 0 {
        result[2].y = result[3].y;
        result[4].y = result[3].y;
        if count == 2 {
            result[5].y = result[6].y;
            result[7].y = result[6].y;
        }
    }
    (result, count)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Conic {
    pub points: [FloatPoint; 3],
    pub weight: f32,
}

impl Conic {
    /// A weight that is not positive and finite becomes 1.
    pub(crate) fn new(points: [FloatPoint; 3], weight: f32) -> Conic {
        Conic {
            points,
            weight: if weight > 0.0 && weight.is_finite() {
                weight
            } else {
                1.0
            },
        }
    }

    pub(crate) fn eval_tangent_at(&self, t: f32) -> FloatPoint {
        // The derivative is zero at an end where the control point is on the end point. Use the
        // end points then.
        if (t == 0.0 && self.points[0] == self.points[1]) || (t == 1.0 && self.points[1] == self.points[2]) {
            return subtract(self.points[2], self.points[0]);
        }
        let [p0, p1, p2] = self.points;
        let ww = point(self.weight, self.weight);
        let p20 = subtract(p2, p0);
        let p10 = subtract(p1, p0);
        let c = multiply(ww, p10);
        let a = subtract(multiply(ww, p20), p20);
        let b = subtract(subtract(p20, c), c);
        QuadCoefficients { a, b, c }.eval(point(t, t))
    }

    /// Splits the conic at `t`, or gives None if that makes a value that is not finite.
    pub(crate) fn chop_at(&self, t: f32) -> Option<[Conic; 2]> {
        let [p0, p1, p2] = self.points;
        let w = self.weight;
        // The points in homogeneous coordinates.
        let source = [
            [p0.x * 1.0, p0.y * 1.0, 1.0],
            [p1.x * w, p1.y * w, w],
            [p2.x * 1.0, p2.y * 1.0, 1.0],
        ];
        let mut chopped = [[0.0f32; 3]; 3];
        for axis in 0..3 {
            let ab = interpolate(source[0][axis], source[1][axis], t);
            let bc = interpolate(source[1][axis], source[2][axis], t);
            chopped[0][axis] = ab;
            chopped[1][axis] = interpolate(ab, bc, t);
            chopped[2][axis] = bc;
        }
        let project = |value: [f32; 3]| point(value[0] / value[2], value[1] / value[2]);
        let middle = project(chopped[1]);
        let root = chopped[1][2].sqrt();
        let result = [
            Conic {
                points: [p0, project(chopped[0]), middle],
                weight: chopped[0][2] / root,
            },
            Conic {
                points: [middle, project(chopped[2]), p2],
                weight: chopped[2][2] / root,
            },
        ];
        let all_finite = result
            .iter()
            .all(|conic| are_finite(&conic.points) && conic.weight.is_finite());
        all_finite.then_some(result)
    }

    fn find_y_extrema(&self) -> Option<f32> {
        let [p0, p1, p2] = self.points;
        let p20 = p2.y - p0.y;
        let p10 = p1.y - p0.y;
        let wp10 = self.weight * p10;
        let (roots, count) = find_unit_quad_roots(self.weight * p20 - p20, p20 - 2.0 * wp10, wp10);
        (count == 1).then_some(roots[0])
    }

    /// Splits the conic where its y coordinate turns, if it does and the split stays finite.
    pub(crate) fn chop_at_y_extrema(&self) -> Option<[Conic; 2]> {
        let t = self.find_y_extrema()?;
        let mut result = self.chop_at(t)?;
        // The split is at a turn, so make it flat.
        let value = result[0].points[2].y;
        result[0].points[1].y = value;
        result[1].points[0].y = value;
        result[1].points[1].y = value;
        Some(result)
    }
}
