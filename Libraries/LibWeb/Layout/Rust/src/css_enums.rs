/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// The CSS crate generates these constants from CSS/Enums.json. The layout
// crate cannot run that generator without adding a forbidden Cargo build
// dependency, so keep the subset used by the shared display source here.
// LayoutRustBridge.cpp pins every value against the generated C++ enums.

pub mod display_box {
    pub const CONTENTS: u8 = 0;
    pub const NONE: u8 = 1;
}

pub mod display_inside {
    pub const FLOW: u8 = 0;
    pub const FLOW_ROOT: u8 = 1;
    pub const TABLE: u8 = 2;
    pub const FLEX: u8 = 3;
    pub const GRID: u8 = 4;
    pub const RUBY: u8 = 5;
    pub const MATH: u8 = 6;
}

pub mod display_internal {
    pub const TABLE_ROW_GROUP: u8 = 0;
    pub const TABLE_HEADER_GROUP: u8 = 1;
    pub const TABLE_FOOTER_GROUP: u8 = 2;
    pub const TABLE_ROW: u8 = 3;
    pub const TABLE_CELL: u8 = 4;
    pub const TABLE_COLUMN_GROUP: u8 = 5;
    pub const TABLE_COLUMN: u8 = 6;
    pub const TABLE_CAPTION: u8 = 7;
}

pub mod display_outside {
    pub const BLOCK: u8 = 0;
    pub const INLINE: u8 = 1;
}
