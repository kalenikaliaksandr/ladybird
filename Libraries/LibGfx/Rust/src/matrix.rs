/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::geometry::FloatPoint;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct AffineTransform {
    pub values: [f32; 6],
}

impl Default for AffineTransform {
    fn default() -> Self {
        Self::identity()
    }
}

impl AffineTransform {
    pub const fn identity() -> Self {
        Self {
            values: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct FloatMatrix4x4 {
    pub elements: [[f32; 4]; 4],
}

impl Default for FloatMatrix4x4 {
    fn default() -> Self {
        Self::identity()
    }
}

impl FloatMatrix4x4 {
    pub const fn identity() -> Self {
        Self {
            elements: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn multiplied(self, other: Self) -> Self {
        let mut product = Self::identity();
        for i in 0..4 {
            for j in 0..4 {
                product.elements[i][j] = self.elements[i][0] * other.elements[0][j]
                    + self.elements[i][1] * other.elements[1][j]
                    + self.elements[i][2] * other.elements[2][j]
                    + self.elements[i][3] * other.elements[3][j];
            }
        }
        product
    }

    pub fn determinant(self) -> f32 {
        let mut result = 0.0f32;
        let mut sign = 1.0f32;
        for j in 0..4 {
            result += sign * self.elements[0][j] * self.first_minor(0, j);
            sign = -sign;
        }
        result
    }

    pub fn first_minor(self, skip_row: usize, skip_column: usize) -> f32 {
        determinant_of_three_by_three(skip_row_and_column_of_four_by_four(
            self.elements,
            skip_row,
            skip_column,
        ))
    }

    pub fn is_invertible(self) -> bool {
        self.determinant() != 0.0
    }
}

fn skip_row_and_column_of_four_by_four(elements: [[f32; 4]; 4], skip_row: usize, skip_column: usize) -> [[f32; 3]; 3] {
    let mut minor = [[0.0f32; 3]; 3];
    let mut written = 0;
    for (row, row_values) in elements.iter().enumerate() {
        for (column, value) in row_values.iter().enumerate() {
            if row == skip_row || column == skip_column {
                continue;
            }
            minor[written / 3][written % 3] = *value;
            written += 1;
        }
    }
    minor
}

fn skip_row_and_column_of_three_by_three(
    elements: [[f32; 3]; 3],
    skip_row: usize,
    skip_column: usize,
) -> [[f32; 2]; 2] {
    let mut minor = [[0.0f32; 2]; 2];
    let mut written = 0;
    for (row, row_values) in elements.iter().enumerate() {
        for (column, value) in row_values.iter().enumerate() {
            if row == skip_row || column == skip_column {
                continue;
            }
            minor[written / 2][written % 2] = *value;
            written += 1;
        }
    }
    minor
}

fn determinant_of_three_by_three(elements: [[f32; 3]; 3]) -> f32 {
    let mut result = 0.0f32;
    let mut sign = 1.0f32;
    for j in 0..3 {
        let minor = skip_row_and_column_of_three_by_three(elements, 0, j);
        result += sign * elements[0][j] * determinant_of_two_by_two(minor);
        sign = -sign;
    }
    result
}

fn determinant_of_two_by_two(elements: [[f32; 2]; 2]) -> f32 {
    let mut result = 0.0f32;
    let mut sign = 1.0f32;
    for j in 0..2 {
        result += sign * elements[0][j] * elements[1][1 - j];
        sign = -sign;
    }
    result
}

pub fn translation_matrix(x: f32, y: f32, z: f32) -> FloatMatrix4x4 {
    FloatMatrix4x4 {
        elements: [
            [1.0, 0.0, 0.0, x],
            [0.0, 1.0, 0.0, y],
            [0.0, 0.0, 1.0, z],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

pub fn scale_matrix(x: f32, y: f32, z: f32) -> FloatMatrix4x4 {
    FloatMatrix4x4 {
        elements: [
            [x, 0.0, 0.0, 0.0],
            [0.0, y, 0.0, 0.0],
            [0.0, 0.0, z, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

pub fn perspective_matrix(distance: f32) -> FloatMatrix4x4 {
    FloatMatrix4x4 {
        elements: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, -1.0 / distance, 1.0],
        ],
    }
}

// Converts a CSS-pixel-space 4x4 matrix to device-pixel-space.
// - Translation column (column 3, rows 0-2) is scaled up by DPR
// - Perspective row (row 3, columns 0-2) is scaled down by DPR
// - All other elements are unaffected (the scale factors cancel out)
pub fn scale_matrix_for_device_pixels(mut matrix: FloatMatrix4x4, scale: f32) -> FloatMatrix4x4 {
    matrix.elements[0][3] *= scale;
    matrix.elements[1][3] *= scale;
    matrix.elements[2][3] *= scale;
    matrix.elements[3][0] /= scale;
    matrix.elements[3][1] /= scale;
    matrix.elements[3][2] /= scale;
    matrix
}

pub fn affine_to_matrix(transform: AffineTransform) -> FloatMatrix4x4 {
    let [a, b, c, d, e, f] = transform.values;
    FloatMatrix4x4 {
        elements: [
            [a, c, 0.0, e],
            [b, d, 0.0, f],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

pub fn translated_then_multiplied(translation: FloatPoint, other: AffineTransform) -> AffineTransform {
    let translated = AffineTransform {
        values: [1.0, 0.0, 0.0, 1.0, translation.x, translation.y],
    };
    multiply_affine(translated, other)
}

pub fn multiply_affine(lhs: AffineTransform, rhs: AffineTransform) -> AffineTransform {
    let [a, b, c, d, e, f] = lhs.values;
    let [oa, ob, oc, od, oe, of] = rhs.values;
    AffineTransform {
        values: [
            a * oa + c * ob,
            b * oa + d * ob,
            a * oc + c * od,
            b * oc + d * od,
            a * oe + c * of + e,
            b * oe + d * of + f,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix_from_rows(rows: [[f32; 4]; 4]) -> FloatMatrix4x4 {
        FloatMatrix4x4 { elements: rows }
    }

    #[test]
    fn multiplies_translation_and_scale_in_order() {
        let translated_then_scaled = scale_matrix(2.0, 3.0, 1.0).multiplied(translation_matrix(5.0, 7.0, 0.0));
        assert_eq!(
            translated_then_scaled.elements,
            [
                [2.0, 0.0, 0.0, 10.0],
                [0.0, 3.0, 0.0, 21.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        );
        let scaled_then_translated = translation_matrix(5.0, 7.0, 0.0).multiplied(scale_matrix(2.0, 3.0, 1.0));
        assert_eq!(
            scaled_then_translated.elements,
            [
                [2.0, 0.0, 0.0, 5.0],
                [0.0, 3.0, 0.0, 7.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        );
    }

    #[test]
    fn multiplying_by_identity_preserves_every_element() {
        let arbitrary = matrix_from_rows([
            [0.5, -1.25, 3.0, 4.75],
            [2.5, 0.125, -6.0, 7.5],
            [-8.25, 9.0, 10.5, -11.0],
            [12.75, -13.5, 14.0, 15.25],
        ]);
        assert_eq!(
            arbitrary.multiplied(FloatMatrix4x4::identity()).elements,
            arbitrary.elements
        );
        assert_eq!(
            FloatMatrix4x4::identity().multiplied(arbitrary).elements,
            arbitrary.elements
        );
    }

    #[test]
    fn determinant_of_diagonal_matrix_is_the_diagonal_product() {
        assert_eq!(scale_matrix(2.0, 3.0, 4.0).determinant(), 24.0);
        assert_eq!(FloatMatrix4x4::identity().determinant(), 1.0);
        assert_eq!(translation_matrix(5.0, -7.0, 9.0).determinant(), 1.0);
    }

    #[test]
    fn flat_scale_is_not_invertible() {
        assert!(!scale_matrix(1.0, 1.0, 0.0).is_invertible());
        assert!(scale_matrix(1.0, 1.0, 0.5).is_invertible());
        let rows_are_linearly_dependent = matrix_from_rows([
            [1.0, 2.0, 3.0, 4.0],
            [2.0, 4.0, 6.0, 8.0],
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 0.0, 1.0, 0.0],
        ]);
        assert!(!rows_are_linearly_dependent.is_invertible());
    }

    #[test]
    fn first_minor_of_identity_matches_cofactor_definition() {
        assert_eq!(FloatMatrix4x4::identity().first_minor(0, 0), 1.0);
        assert_eq!(FloatMatrix4x4::identity().first_minor(0, 1), 0.0);
    }

    #[test]
    fn perspective_matrix_places_the_depth_term() {
        let perspective = perspective_matrix(4.0);
        assert_eq!(perspective.elements[3][2], -0.25);
        assert_eq!(perspective.elements[3][3], 1.0);
        assert_eq!(perspective.elements[0][0], 1.0);
    }

    #[test]
    fn device_pixel_scaling_touches_translation_and_bottom_row_only() {
        let scaled = scale_matrix_for_device_pixels(translation_matrix(10.0, 20.0, 30.0), 2.0);
        assert_eq!(scaled.elements[0][3], 20.0);
        assert_eq!(scaled.elements[1][3], 40.0);
        assert_eq!(scaled.elements[2][3], 60.0);
        assert_eq!(scaled.elements[0][0], 1.0);
        let bottom_row_scaled = scale_matrix_for_device_pixels(perspective_matrix(2.0), 2.0);
        assert_eq!(bottom_row_scaled.elements[3][2], -0.25);
    }
}
