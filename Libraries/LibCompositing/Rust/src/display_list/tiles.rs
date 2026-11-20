/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use libgfx_rust::IntRect;
use std::cmp::Ordering;

pub const TILE_SIZE_IN_DEVICE_PIXELS: i32 = 512;
const MAXIMUM_CELLS_CONTENT_IS_FOLLOWED_INTO_ONE_BY_ONE: u64 = 4096;

/// A cell of the tile grid of a layer's raster space, which starts at the space's origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct TileCoord {
    pub column: i32,
    pub row: i32,
}

impl TileCoord {
    pub const fn new(column: i32, row: i32) -> Self {
        Self { column, row }
    }

    pub const fn cell_rect(self) -> IntRect {
        IntRect::new(
            self.column * TILE_SIZE_IN_DEVICE_PIXELS,
            self.row * TILE_SIZE_IN_DEVICE_PIXELS,
            TILE_SIZE_IN_DEVICE_PIXELS,
            TILE_SIZE_IN_DEVICE_PIXELS,
        )
    }
}

impl Ord for TileCoord {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.row, self.column).cmp(&(other.row, other.column))
    }
}

impl PartialOrd for TileCoord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for TileCoord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({},{})", self.column, self.row)
    }
}

/// The cells that `rect` reaches into, row by row. Callers bound `rect` first, since a rect far larger than the
/// viewport reaches into a great many cells.
pub fn tile_coords_touching(rect: IntRect) -> impl Iterator<Item = TileCoord> {
    let coords = (!rect.is_empty()).then(|| {
        let columns =
            rect.x.div_euclid(TILE_SIZE_IN_DEVICE_PIXELS)..=(rect.right() - 1).div_euclid(TILE_SIZE_IN_DEVICE_PIXELS);
        let rows =
            rect.y.div_euclid(TILE_SIZE_IN_DEVICE_PIXELS)..=(rect.bottom() - 1).div_euclid(TILE_SIZE_IN_DEVICE_PIXELS);
        rows.flat_map(move |row| columns.clone().map(move |column| TileCoord::new(column, row)))
    });
    coords.into_iter().flatten()
}

/// Whether content within `rect` reaches too many cells to be followed into each of them, and is taken to reach every
/// cell it can instead.
pub fn reaches_too_many_cells_to_follow_one_by_one(rect: IntRect) -> bool {
    tile_count_touching(rect) > MAXIMUM_CELLS_CONTENT_IS_FOLLOWED_INTO_ONE_BY_ONE
}

/// How many cells `rect` reaches into.
pub fn tile_count_touching(rect: IntRect) -> u64 {
    if rect.is_empty() {
        return 0;
    }
    let cell_count_along_axis = |start: i32, end: i32| {
        (i64::from((end - 1).div_euclid(TILE_SIZE_IN_DEVICE_PIXELS))
            - i64::from(start.div_euclid(TILE_SIZE_IN_DEVICE_PIXELS))
            + 1) as u64
    };
    cell_count_along_axis(rect.x, rect.right()) * cell_count_along_axis(rect.y, rect.bottom())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_touches_the_cells_it_reaches_into_row_by_row() {
        let coords: Vec<TileCoord> = tile_coords_touching(IntRect::new(
            TILE_SIZE_IN_DEVICE_PIXELS - 6,
            10,
            TILE_SIZE_IN_DEVICE_PIXELS + 44,
            TILE_SIZE_IN_DEVICE_PIXELS - 6,
        ))
        .collect();
        assert_eq!(
            coords,
            [
                TileCoord::new(0, 0),
                TileCoord::new(1, 0),
                TileCoord::new(2, 0),
                TileCoord::new(0, 1),
                TileCoord::new(1, 1),
                TileCoord::new(2, 1),
            ]
        );
    }

    #[test]
    fn a_rect_ending_on_a_cell_edge_does_not_touch_the_next_cell() {
        let coords: Vec<TileCoord> = tile_coords_touching(IntRect::new(
            0,
            0,
            TILE_SIZE_IN_DEVICE_PIXELS,
            2 * TILE_SIZE_IN_DEVICE_PIXELS,
        ))
        .collect();
        assert_eq!(coords, [TileCoord::new(0, 0), TileCoord::new(0, 1)]);
    }

    #[test]
    fn cells_left_of_and_above_the_origin_have_negative_coords() {
        let coords: Vec<TileCoord> =
            tile_coords_touching(IntRect::new(-1, -TILE_SIZE_IN_DEVICE_PIXELS - 1, 2, 1)).collect();
        assert_eq!(coords, [TileCoord::new(-1, -2), TileCoord::new(0, -2)]);
    }

    #[test]
    fn an_empty_rect_touches_no_cell() {
        assert_eq!(tile_coords_touching(IntRect::new(10, 10, 0, 20)).count(), 0);
    }
}
