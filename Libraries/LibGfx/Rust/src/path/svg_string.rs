/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/utils/SkParsePath.cpp (SkParsePath::ToSVGString), skia/src/core/SkPath.cpp
// (SkPath::Iter) and skia/src/core/SkString.cpp (SkStrAppendScalar).

use super::geometry::{Conic, point};
use super::{Path, PathVerb};
use crate::geometry::FloatPoint;
use std::fmt::Write;

// How close the quads that replace a conic must be to it.
const CONIC_TOLERANCE: f32 = 1.0 / 1024.0;

fn remove_trailing_zeros(number: &mut String) {
    if number.contains('.') {
        let length = number.trim_end_matches('0').trim_end_matches('.').len();
        number.truncate(length);
    }
}

/// Appends the value as printf() does with "%.8g".
fn append_number(output: &mut String, value: f32) {
    if value.is_nan() {
        output.push_str("nan");
        return;
    }
    if value.is_infinite() {
        output.push_str(if value > 0.0 { "inf" } else { "-inf" });
        return;
    }
    const PRECISION: i32 = 8;
    let value = f64::from(value);
    let scientific = format!("{:.*e}", (PRECISION - 1) as usize, value);
    let (mantissa, exponent) = scientific.split_once('e').expect("The scientific form has an exponent");
    let exponent: i32 = exponent.parse().expect("The exponent is a number");
    if !(-4..PRECISION).contains(&exponent) {
        let mut mantissa = mantissa.to_string();
        remove_trailing_zeros(&mut mantissa);
        let sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(output, "{mantissa}e{sign}{:02}", exponent.abs());
    } else {
        let mut fixed = format!("{:.*}", (PRECISION - 1 - exponent) as usize, value);
        remove_trailing_zeros(&mut fixed);
        output.push_str(&fixed);
    }
}

struct Writer {
    output: String,
    // The encoder subtracts this from each point. It is 0 in the absolute form, but it is -0 for a
    // coordinate after a command that ended at a negative coordinate.
    current_point: FloatPoint,
}

impl Writer {
    fn append_command(&mut self, command: char, points: &[FloatPoint]) {
        self.output.push(command);
        for (index, point) in points.iter().enumerate() {
            if index > 0 {
                self.output.push(' ');
            }
            append_number(&mut self.output, point.x - self.current_point.x);
            self.output.push(' ');
            append_number(&mut self.output, point.y - self.current_point.y);
        }
        let last = points[points.len() - 1];
        self.current_point = point(last.x * 0.0, last.y * 0.0);
    }
}

fn has_nan(point: FloatPoint) -> bool {
    point.x.is_nan() || point.y.is_nan()
}

impl Path {
    /// The path as SVG path data with absolute commands. Conics become quads.
    pub fn to_svg_string(&self) -> String {
        let view = self.snapshot();
        let (verbs, points, conic_weights) = (view.verbs, view.points, view.conic_weights);
        let mut writer = Writer {
            output: String::new(),
            current_point: FloatPoint::default(),
        };
        let mut verb_index = 0;
        let mut point_index = 0;
        let mut conic_weight_index = 0;
        let mut last_point = FloatPoint::default();
        let mut move_to = FloatPoint::default();
        while let Some(&verb) = verbs.get(verb_index) {
            verb_index += 1;
            match verb {
                PathVerb::Move => {
                    // A move at the end of the path is not written.
                    if verb_index == verbs.len() {
                        break;
                    }
                    move_to = points[point_index];
                    last_point = move_to;
                    point_index += 1;
                    writer.append_command('M', &[move_to]);
                }
                PathVerb::Line => {
                    last_point = points[point_index];
                    point_index += 1;
                    writer.append_command('L', &[last_point]);
                }
                PathVerb::Quad => {
                    writer.append_command('Q', &points[point_index..point_index + 2]);
                    last_point = points[point_index + 1];
                    point_index += 2;
                }
                PathVerb::Conic => {
                    let weight = conic_weights[conic_weight_index];
                    conic_weight_index += 1;
                    let conic = Conic::new([last_point, points[point_index], points[point_index + 1]], weight);
                    let quad_points = conic.chop_into_quads_pow2(conic.compute_quad_pow2(CONIC_TOLERANCE));
                    for quad in 0..(quad_points.len() - 1) / 2 {
                        writer.append_command('Q', &quad_points[quad * 2 + 1..quad * 2 + 3]);
                    }
                    last_point = points[point_index + 1];
                    point_index += 2;
                }
                PathVerb::Cubic => {
                    writer.append_command('C', &points[point_index..point_index + 3]);
                    last_point = points[point_index + 2];
                    point_index += 3;
                }
                PathVerb::Close => {
                    // A contour that does not end at its start gets a line back to it before the
                    // close.
                    if last_point != move_to && !has_nan(last_point) && !has_nan(move_to) {
                        writer.append_command('L', &[move_to]);
                    }
                    writer.output.push('Z');
                    last_point = move_to;
                }
            }
        }
        writer.output
    }
}
