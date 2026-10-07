/*
 * Copyright 2015 Google Inc.
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkPathBuilder.cpp (SkPathBuilder::arcTo).

use super::Path;
use super::sk_matrix::{PI, SkMatrix, cos_snap_to_zero, sin_snap_to_zero};
use crate::geometry::FloatPoint;

const NEARLY_ZERO: f32 = 1.0 / 4096.0;

fn subtract(a: FloatPoint, b: FloatPoint) -> FloatPoint {
    FloatPoint {
        x: a.x - b.x,
        y: a.y - b.y,
    }
}

fn add(a: FloatPoint, b: FloatPoint) -> FloatPoint {
    FloatPoint {
        x: a.x + b.x,
        y: a.y + b.y,
    }
}

fn scaled(point: FloatPoint, scale: f32) -> FloatPoint {
    FloatPoint {
        x: point.x * scale,
        y: point.y * scale,
    }
}

fn round_to_integer(value: f32) -> f32 {
    (f64::from(value) + 0.5).floor() as f32
}

fn saturating_ceil_to_int(value: f32) -> i32 {
    const MAX: f32 = 2_147_483_520.0;
    let mut value = value.ceil();
    // NaN gives the maximum, as in the original.
    value = if value < MAX { value } else { MAX };
    value = if value > -MAX { value } else { -MAX };
    value as i32
}

/// Appends an SVG elliptical arc from the last point to `end` as conics. The rotation of the
/// x-axis is in degrees.
pub(super) fn arc_to(
    path: &mut Path,
    radii: FloatPoint,
    angle: f32,
    large_arc: bool,
    counter_clockwise: bool,
    end: FloatPoint,
) {
    path.sk_ensure_move();

    let start = *path.points().last().expect("A path has a point after a move");

    // An arc with a zero radius is a line, and so is an arc that ends where it starts.
    if radii.x == 0.0 || radii.y == 0.0 || start == end {
        path.sk_line_to(end);
        return;
    }
    let mut rx = radii.x.abs();
    let mut ry = radii.y.abs();
    let mid_point_distance = scaled(subtract(start, end), 0.5);

    let point_transform = SkMatrix::rotate(-angle);
    let transformed_mid_point = point_transform.map_point_affine(mid_point_distance);
    let square_rx = rx * rx;
    let square_ry = ry * ry;
    let square_x = transformed_mid_point.x * transformed_mid_point.x;
    let square_y = transformed_mid_point.y * transformed_mid_point.y;

    // Make the radii larger if they cannot reach from the start to the end.
    let mut radii_scale = square_x / square_rx + square_y / square_ry;
    if radii_scale > 1.0 {
        radii_scale = radii_scale.sqrt();
        rx *= radii_scale;
        ry *= radii_scale;
    }

    let point_transform = SkMatrix::scale(1.0 / rx, 1.0 / ry).pre_rotate(-angle);

    let mut unit_points = [FloatPoint::default(); 2];
    point_transform.map_points(&mut unit_points, &[start, end]);
    let mut delta = subtract(unit_points[1], unit_points[0]);

    let d = delta.x * delta.x + delta.y * delta.y;
    // Keep NaN, as std::max() does.
    let difference = 1.0 / d - 0.25f32;
    let scale_factor_squared = if difference < 0.0 { 0.0 } else { difference };

    let mut scale_factor = scale_factor_squared.sqrt();
    if counter_clockwise != large_arc {
        scale_factor = -scale_factor;
    }
    delta = scaled(delta, scale_factor);
    let mut center_point = scaled(add(unit_points[0], unit_points[1]), 0.5);
    center_point.x += -delta.y;
    center_point.y += delta.x;
    unit_points[0] = subtract(unit_points[0], center_point);
    unit_points[1] = subtract(unit_points[1], center_point);
    let theta1 = unit_points[0].y.atan2(unit_points[0].x);
    let theta2 = unit_points[1].y.atan2(unit_points[1].x);
    let mut theta_arc = theta2 - theta1;
    if theta_arc < 0.0 && !counter_clockwise {
        theta_arc += PI * 2.0;
    } else if theta_arc > 0.0 && counter_clockwise {
        theta_arc -= PI * 2.0;
    }

    // A very small angle makes the arithmetic below unstable.
    if theta_arc.abs() < (PI / (1000.0 * 1000.0)) {
        path.sk_line_to(end);
        return;
    }

    let point_transform = SkMatrix::rotate(angle).pre_scale(rx, ry);

    // The arc can be a little larger than a quarter circle, so allow up to a third of a circle for
    // each segment.
    let segments = saturating_ceil_to_int((theta_arc / (2.0 * PI / 3.0)).abs());
    let theta_width = theta_arc / segments as f32;
    let t = (0.5 * theta_width).tan();
    if !t.is_finite() {
        return;
    }
    let mut start_theta = theta1;
    let weight = (0.5f32 + theta_width.cos() * 0.5).sqrt();
    let is_integer = |value: f32| value == value.floor();
    let expect_integers = (PI / 2.0 - theta_width.abs()).abs() <= NEARLY_ZERO
        && is_integer(rx)
        && is_integer(ry)
        && is_integer(end.x)
        && is_integer(end.y);

    for _ in 0..segments {
        let end_theta = start_theta + theta_width;
        let sin_end_theta = sin_snap_to_zero(end_theta);
        let cos_end_theta = cos_snap_to_zero(end_theta);

        unit_points[1] = add(
            FloatPoint {
                x: cos_end_theta,
                y: sin_end_theta,
            },
            center_point,
        );
        unit_points[0] = unit_points[1];
        unit_points[0].x += t * sin_end_theta;
        unit_points[0].y += -t * cos_end_theta;
        let mut mapped = [FloatPoint::default(); 2];
        point_transform.map_points(&mut mapped, &unit_points);
        // Rounding errors in the arc width can make the arc start a little outside of its marks.
        // If the input is on integers, put the conic on integers too.
        if expect_integers {
            for point in &mut mapped {
                point.x = round_to_integer(point.x);
                point.y = round_to_integer(point.y);
            }
        }
        path.sk_conic_to(mapped[0], mapped[1], weight);
        start_theta = end_theta;
    }

    // The arc ends at `end` by definition. Rounding errors must not move it.
    path.sk_set_last_point(end);
}
