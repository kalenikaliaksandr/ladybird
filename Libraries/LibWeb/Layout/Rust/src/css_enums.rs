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

pub mod align_content {
    pub const NORMAL: u8 = 0;
    pub const START: u8 = 1;
    pub const FLEX_START: u8 = 2;
    pub const END: u8 = 3;
    pub const FLEX_END: u8 = 4;
    pub const CENTER: u8 = 5;
    pub const SPACE_BETWEEN: u8 = 6;
    pub const SPACE_AROUND: u8 = 7;
    pub const SPACE_EVENLY: u8 = 8;
    pub const STRETCH: u8 = 9;
}

pub mod align_items {
    pub const BASELINE: u8 = 0;
    pub const CENTER: u8 = 1;
    pub const END: u8 = 2;
    pub const FLEX_END: u8 = 3;
    pub const FLEX_START: u8 = 4;
    pub const NORMAL: u8 = 5;
    pub const SAFE: u8 = 6;
    pub const SELF_END: u8 = 7;
    pub const SELF_START: u8 = 8;
    pub const START: u8 = 9;
    pub const STRETCH: u8 = 10;
    pub const UNSAFE: u8 = 11;
}

pub mod align_self {
    pub const AUTO: u8 = 0;
    pub const BASELINE: u8 = 1;
    pub const CENTER: u8 = 2;
    pub const END: u8 = 3;
    pub const FLEX_END: u8 = 4;
    pub const FLEX_START: u8 = 5;
    pub const NORMAL: u8 = 6;
    pub const SAFE: u8 = 7;
    pub const SELF_END: u8 = 8;
    pub const SELF_START: u8 = 9;
    pub const START: u8 = 10;
    pub const STRETCH: u8 = 11;
    pub const UNSAFE: u8 = 12;
}

pub mod flex_direction {
    pub const ROW: u8 = 0;
    pub const ROW_REVERSE: u8 = 1;
    pub const COLUMN: u8 = 2;
    pub const COLUMN_REVERSE: u8 = 3;
}

pub mod flex_wrap {
    pub const NOWRAP: u8 = 0;
    pub const WRAP: u8 = 1;
    pub const WRAP_REVERSE: u8 = 2;
}

pub mod justify_content {
    pub const NORMAL: u8 = 0;
    pub const START: u8 = 1;
    pub const END: u8 = 2;
    pub const FLEX_START: u8 = 3;
    pub const FLEX_END: u8 = 4;
    pub const CENTER: u8 = 5;
    pub const SPACE_BETWEEN: u8 = 6;
    pub const SPACE_AROUND: u8 = 7;
    pub const SPACE_EVENLY: u8 = 8;
    pub const STRETCH: u8 = 9;
    pub const LEFT: u8 = 10;
    pub const RIGHT: u8 = 11;
}

pub mod overflow {
    pub const AUTO: u8 = 0;
    pub const CLIP: u8 = 1;
    pub const HIDDEN: u8 = 2;
    pub const SCROLL: u8 = 3;
    pub const VISIBLE: u8 = 4;
}

pub mod writing_mode {
    pub const HORIZONTAL_TB: u8 = 0;
    pub const VERTICAL_RL: u8 = 1;
    pub const VERTICAL_LR: u8 = 2;
    pub const SIDEWAYS_RL: u8 = 3;
    pub const SIDEWAYS_LR: u8 = 4;
}
