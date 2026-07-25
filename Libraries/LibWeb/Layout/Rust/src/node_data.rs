/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::ffi::c_void;

pub const INVALID_NODE_SLOT_INDEX: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct NodeSlotId {
    pub index: u32,
}

impl NodeSlotId {
    pub const INVALID: Self = Self {
        index: INVALID_NODE_SLOT_INDEX,
    };
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

impl Default for NodeData {
    fn default() -> Self {
        Self {
            parent: NodeSlotId::INVALID,
            first_child: NodeSlotId::INVALID,
            last_child: NodeSlotId::INVALID,
            previous_sibling: NodeSlotId::INVALID,
            next_sibling: NodeSlotId::INVALID,
            containing_block: NodeSlotId::INVALID,
            inline_containing_block: NodeSlotId::INVALID,
            kind: NodeKind::Unset,
            generated_for: 0,
            flags: 0,
            initial_quote_nesting_level: 0,
            layout_index: 0,
            style: std::ptr::null(),
        }
    }
}
