/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::ffi::c_void;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct NodeSlotId {
    pub index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodeKind {
    Unset,
}

#[repr(C)]
pub struct NodeData {
    pub parent: NodeSlotId,
    pub first_child: NodeSlotId,
    pub last_child: NodeSlotId,
    pub previous_sibling: NodeSlotId,
    pub next_sibling: NodeSlotId,
    pub containing_block: NodeSlotId,
    pub inline_containing_block: NodeSlotId,
    pub kind: NodeKind,
    pub generated_for: u8,
    pub flags: u32,
    pub initial_quote_nesting_level: u32,
    pub layout_index: u32,
    pub style: *const c_void,
}
