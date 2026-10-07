/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkMatrix.cpp and skia/include/core/SkMatrix.h.

//! The part of a 3x3 matrix that path geometry needs. The arithmetic that maps points changes
//! with the kind of matrix, and the compositor transforms paths with the same rules, so a point
//! must go through the same branch here.

use crate::geometry::FloatPoint;

const TRANSLATE_MASK: u8 = 0x01;
const SCALE_MASK: u8 = 0x02;
const AFFINE_MASK: u8 = 0x04;
const PERSPECTIVE_MASK: u8 = 0x08;

pub(crate) const PI: f32 = std::f32::consts::PI;
const SIN_COS_NEARLY_ZERO: f32 = 1.0 / 65536.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SkMatrix {
    scale_x: f32,
    skew_x: f32,
    translate_x: f32,
    skew_y: f32,
    scale_y: f32,
    translate_y: f32,
    // Only a scale by a value that is not finite makes these other than zero. The third
    // perspective value is always 1.
    perspective_0: f32,
    perspective_1: f32,
}

pub(crate) fn sin_snap_to_zero(radians: f32) -> f32 {
    let value = radians.sin();
    if value.abs() <= SIN_COS_NEARLY_ZERO { 0.0 } else { value }
}

pub(crate) fn cos_snap_to_zero(radians: f32) -> f32 {
    let value = radians.cos();
    if value.abs() <= SIN_COS_NEARLY_ZERO { 0.0 } else { value }
}

// Both zeros give 0.
fn bits_as_twos_complement(value: f32) -> i32 {
    let bits = value.to_bits() as i32;
    if bits < 0 { -(bits & 0x7FFF_FFFF) } else { bits }
}

fn mul_add_mul(a: f32, b: f32, c: f32, d: f32) -> f32 {
    (f64::from(a) * f64::from(b) + f64::from(c) * f64::from(d)) as f32
}

impl SkMatrix {
    pub(crate) const IDENTITY: SkMatrix = SkMatrix {
        scale_x: 1.0,
        skew_x: 0.0,
        translate_x: 0.0,
        skew_y: 0.0,
        scale_y: 1.0,
        translate_y: 0.0,
        perspective_0: 0.0,
        perspective_1: 0.0,
    };

    /// The matrix that maps (x, y) to (a * x + c * y + e, b * x + d * y + f).
    pub(crate) fn from_affine(values: [f32; 6]) -> SkMatrix {
        let [a, b, c, d, e, f] = values;
        SkMatrix {
            scale_x: a,
            skew_x: c,
            translate_x: e,
            skew_y: b,
            scale_y: d,
            translate_y: f,
            ..Self::IDENTITY
        }
    }

    pub(crate) fn translate(dx: f32, dy: f32) -> SkMatrix {
        SkMatrix {
            translate_x: dx,
            translate_y: dy,
            ..Self::IDENTITY
        }
    }

    pub(crate) fn scale(sx: f32, sy: f32) -> SkMatrix {
        SkMatrix {
            scale_x: sx,
            scale_y: sy,
            ..Self::IDENTITY
        }
    }

    pub(crate) fn rotate(degrees: f32) -> SkMatrix {
        let radians = degrees * (PI / 180.0);
        let sin = sin_snap_to_zero(radians);
        let cos = cos_snap_to_zero(radians);
        SkMatrix {
            scale_x: cos,
            skew_x: -sin,
            skew_y: sin,
            scale_y: cos,
            ..Self::IDENTITY
        }
    }

    fn type_mask(&self) -> u8 {
        if self.perspective_0 != 0.0 || self.perspective_1 != 0.0 {
            return TRANSLATE_MASK | SCALE_MASK | AFFINE_MASK | PERSPECTIVE_MASK;
        }
        let mut mask = 0;
        if self.translate_x != 0.0 || self.translate_y != 0.0 {
            mask |= TRANSLATE_MASK;
        }
        let m00 = bits_as_twos_complement(self.scale_x);
        let m01 = bits_as_twos_complement(self.skew_x);
        let m10 = bits_as_twos_complement(self.skew_y);
        let m11 = bits_as_twos_complement(self.scale_y);
        let one = 1.0f32.to_bits() as i32;
        if m01 | m10 != 0 {
            mask |= AFFINE_MASK | SCALE_MASK;
        } else if (m00 ^ one) | (m11 ^ one) != 0 {
            mask |= SCALE_MASK;
        }
        mask
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.type_mask() == 0
    }

    /// The matrix that applies `other` first and then this matrix.
    pub(crate) fn pre_concat(self, other: SkMatrix) -> SkMatrix {
        if other.is_identity() {
            return self;
        }
        let (a, b) = (self, other);
        let a_mask = a.type_mask();
        let b_mask = b.type_mask();
        if a_mask == 0 {
            return b;
        }
        if (a_mask | b_mask) & (AFFINE_MASK | PERSPECTIVE_MASK) == 0 {
            return SkMatrix {
                scale_x: a.scale_x * b.scale_x,
                translate_x: a.scale_x * b.translate_x + a.translate_x,
                scale_y: a.scale_y * b.scale_y,
                translate_y: a.scale_y * b.translate_y + a.translate_y,
                ..Self::IDENTITY
            };
        }
        if (a_mask | b_mask) & PERSPECTIVE_MASK != 0 {
            let a_rows = a.rows();
            let b_rows = b.rows();
            let at = |row: usize, column: usize| {
                a_rows[row][0] * b_rows[0][column]
                    + a_rows[row][1] * b_rows[1][column]
                    + a_rows[row][2] * b_rows[2][column]
            };
            // Paths never come here with a third perspective value other than 1.
            return SkMatrix {
                scale_x: at(0, 0),
                skew_x: at(0, 1),
                translate_x: at(0, 2),
                skew_y: at(1, 0),
                scale_y: at(1, 1),
                translate_y: at(1, 2),
                perspective_0: at(2, 0),
                perspective_1: at(2, 1),
            };
        }
        SkMatrix {
            scale_x: mul_add_mul(a.scale_x, b.scale_x, a.skew_x, b.skew_y),
            skew_x: mul_add_mul(a.scale_x, b.skew_x, a.skew_x, b.scale_y),
            translate_x: mul_add_mul(a.scale_x, b.translate_x, a.skew_x, b.translate_y) + a.translate_x,
            skew_y: mul_add_mul(a.skew_y, b.scale_x, a.scale_y, b.skew_y),
            scale_y: mul_add_mul(a.skew_y, b.skew_x, a.scale_y, b.scale_y),
            translate_y: mul_add_mul(a.skew_y, b.translate_x, a.scale_y, b.translate_y) + a.translate_y,
            ..Self::IDENTITY
        }
    }

    fn rows(&self) -> [[f32; 3]; 3] {
        [
            [self.scale_x, self.skew_x, self.translate_x],
            [self.skew_y, self.scale_y, self.translate_y],
            [self.perspective_0, self.perspective_1, 1.0],
        ]
    }

    pub(crate) fn pre_rotate(self, degrees: f32) -> SkMatrix {
        self.pre_concat(SkMatrix::rotate(degrees))
    }

    pub(crate) fn pre_scale(mut self, sx: f32, sy: f32) -> SkMatrix {
        if sx == 1.0 && sy == 1.0 {
            return self;
        }
        self.scale_x *= sx;
        self.skew_y *= sx;
        self.perspective_0 *= sx;
        self.skew_x *= sy;
        self.scale_y *= sy;
        self.perspective_1 *= sy;
        self
    }

    /// Maps one point with the full affine formula, whatever kind of matrix this is.
    pub(crate) fn map_point_affine(&self, point: FloatPoint) -> FloatPoint {
        FloatPoint {
            x: (point.x * self.scale_x + point.y * self.skew_x) + self.translate_x,
            y: (point.x * self.skew_y + point.y * self.scale_y) + self.translate_y,
        }
    }

    fn map_point_with_mask(&self, mask: u8, point: FloatPoint) -> FloatPoint {
        if mask == 0 {
            point
        } else if mask == TRANSLATE_MASK {
            FloatPoint {
                x: point.x + self.translate_x,
                y: point.y + self.translate_y,
            }
        } else if mask & (AFFINE_MASK | PERSPECTIVE_MASK) == 0 {
            FloatPoint {
                x: point.x * self.scale_x + self.translate_x,
                y: point.y * self.scale_y + self.translate_y,
            }
        } else if mask & PERSPECTIVE_MASK == 0 {
            self.map_point_affine(point)
        } else {
            let x = (point.x * self.scale_x + point.y * self.skew_x) + self.translate_x;
            let y = (point.x * self.skew_y + point.y * self.scale_y) + self.translate_y;
            let mut z = (point.x * self.perspective_0 + point.y * self.perspective_1) + 1.0;
            if z != 0.0 {
                z = 1.0 / z;
            }
            FloatPoint { x: x * z, y: y * z }
        }
    }

    /// Maps each point with the arithmetic that the kind of matrix selects.
    pub(crate) fn map_points(&self, destination: &mut [FloatPoint], source: &[FloatPoint]) {
        debug_assert_eq!(destination.len(), source.len());
        let mask = self.type_mask();
        for (out, point) in destination.iter_mut().zip(source) {
            *out = self.map_point_with_mask(mask, *point);
        }
    }

    pub(crate) fn map_points_in_place(&self, points: &mut [FloatPoint]) {
        let mask = self.type_mask();
        for point in points.iter_mut() {
            *point = self.map_point_with_mask(mask, *point);
        }
    }
}
