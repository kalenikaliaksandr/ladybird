/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub use crate::layout::property_lookup_cache::EnvironmentCoordinate;

impl EnvironmentCoordinate {
    /// The coordinate that refers to nothing, which an empty environment coordinate cache holds.
    pub const fn invalid() -> Self {
        Self {
            hops: Self::INVALID_MARKER,
            index: Self::INVALID_MARKER,
        }
    }

    pub const fn is_valid(&self) -> bool {
        self.hops != Self::INVALID_MARKER && self.index != Self::INVALID_MARKER
    }
}

impl Default for EnvironmentCoordinate {
    fn default() -> Self {
        Self::invalid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_is_valid_unless_either_half_is_the_marker() {
        assert!(!EnvironmentCoordinate::default().is_valid());
        assert!(EnvironmentCoordinate { hops: 0, index: 0 }.is_valid());
        assert!(
            !EnvironmentCoordinate {
                hops: 1,
                index: EnvironmentCoordinate::INVALID_MARKER
            }
            .is_valid()
        );
        assert!(
            !EnvironmentCoordinate {
                hops: EnvironmentCoordinate::INVALID_MARKER,
                index: 1
            }
            .is_valid()
        );
    }
}
