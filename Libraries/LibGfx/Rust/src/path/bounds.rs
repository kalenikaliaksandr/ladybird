/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkRect.cpp (SkRect::Bounds) and skia/src/core/SkPathPriv.h
// (SkPathPriv::TrimmedBounds).

use super::PathVerb;
use crate::geometry::FloatPoint;

/// The left, top, right and bottom of the points, or None if a point is not finite.
pub(crate) fn point_bounds(points: &[FloatPoint]) -> Option<[f32; 4]> {
    let Some(first) = points.first() else {
        return Some([0.0; 4]);
    };
    let (mut left, mut top, mut right, mut bottom) = (first.x, first.y, first.x, first.y);
    // A product of zeros stays zero, unless a value is infinite or NaN.
    let (mut x_product, mut y_product) = (0.0f32, 0.0f32);
    for point in points {
        // On a tie, keep the value that is there. That keeps the sign of the first zero.
        if point.x < left {
            left = point.x;
        }
        if point.y < top {
            top = point.y;
        }
        if point.x > right {
            right = point.x;
        }
        if point.y > bottom {
            bottom = point.y;
        }
        x_product *= point.x;
        y_product *= point.y;
    }
    if x_product == 0.0 && y_product == 0.0 {
        Some([left, top, right, bottom])
    } else {
        None
    }
}

/// The bounds of a path's points. A move at the end of the path does not count, unless it is the
/// only verb, but it must still be finite.
pub(crate) fn trimmed_bounds(points: &[FloatPoint], verbs: &[PathVerb]) -> Option<[f32; 4]> {
    let mut points = points;
    if verbs.len() > 1 && verbs.last() == Some(&PathVerb::Move) {
        let last = points.last()?;
        if !last.x.is_finite() || !last.y.is_finite() {
            return None;
        }
        points = &points[..points.len() - 1];
    }
    point_bounds(points)
}
