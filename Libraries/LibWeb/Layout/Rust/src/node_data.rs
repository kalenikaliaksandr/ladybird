/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::ffi::c_void;

pub const INVALID_NODE_SLOT_INDEX: u32 = u32::MAX;
pub const GENERATED_FOR_MARKER: u8 = 6;

const NODE_SLOT_INDEX_BITS: u32 = 24;
const NODE_SLOT_INDEX_MASK: u32 = (1 << NODE_SLOT_INDEX_BITS) - 1;
pub(crate) const MAX_NODE_SLOT_COUNT: u32 = NODE_SLOT_INDEX_MASK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct NodeSlotId {
    pub index: u32,
}

impl NodeSlotId {
    pub const INVALID: Self = Self {
        index: INVALID_NODE_SLOT_INDEX,
    };

    pub(crate) fn new(index: u32, generation: u8) -> Self {
        assert!(
            index < MAX_NODE_SLOT_COUNT,
            "layout node arena exhausted its 24-bit slot index space"
        );
        assert_ne!(generation, 0, "layout node arena slot generation must be nonzero");
        Self {
            index: index | (u32::from(generation) << NODE_SLOT_INDEX_BITS),
        }
    }

    pub(crate) fn slot_index(self) -> u32 {
        self.index & NODE_SLOT_INDEX_MASK
    }

    pub(crate) fn generation(self) -> u8 {
        (self.index >> NODE_SLOT_INDEX_BITS) as u8
    }

    pub fn is_invalid(self) -> bool {
        self == Self::INVALID
    }
}

impl Default for NodeSlotId {
    fn default() -> Self {
        Self::INVALID
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodeKind {
    Unset = 0,
    AudioBox = 1,
    BlockContainer = 2,
    Box = 3,
    BreakNode = 4,
    CanvasBox = 5,
    CheckBox = 6,
    FieldSetBox = 7,
    GeneratedTextNode = 8,
    ImageBox = 9,
    InlineNode = 10,
    LegendBox = 11,
    ListItemBox = 12,
    ListItemMarkerBox = 13,
    NavigableContainerViewport = 14,
    Node = 15,
    NodeWithStyle = 16,
    NodeWithStyleAndBoxModelMetrics = 17,
    RadioButton = 18,
    RangeInputBox = 19,
    ReplacedBox = 20,
    SVGBox = 21,
    SVGClipBox = 22,
    SVGForeignObjectBox = 23,
    SVGGeometryBox = 24,
    SVGGraphicsBox = 25,
    SVGImageBox = 26,
    SVGMaskBox = 27,
    SVGPatternBox = 28,
    SVGSVGBox = 29,
    SVGTextBox = 30,
    SVGTextPathBox = 31,
    TableWrapper = 32,
    TextAreaBox = 33,
    TextInputBox = 34,
    TextNode = 35,
    TextSliceNode = 36,
    VideoBox = 37,
    Viewport = 38,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NodeFlag {
    Anonymous = 1 << 0,
    HasStyle = 1 << 1,
    ChildrenAreInline = 1 << 2,
    IsFlexItem = 1 << 3,
    IsGridItem = 1 << 4,
    HasBeenWrappedInTableWrapper = 1 << 5,
    IsBody = 1 << 6,
    NeedsLayoutUpdate = 1 << 7,
    NeedsOwnGeometryUpdate = 1 << 8,
    AbsposDescendantEscapes = 1 << 9,
    CompensatesForHorizontalScroll = 1 << 10,
    CompensatesForVerticalScroll = 1 << 11,
    IsReplacedElement = 1 << 12,
    HasSavedAbsposLayoutInputs = 1 << 13,
    SavedAbsposCbDerivesFromOwnComputedValues = 1 << 14,
    SavedAbsposAlignmentDerivesFromOwnComputedValues = 1 << 15,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
// NB: Some variants are only constructed by C++ through the FFI.
#[allow(dead_code)]
pub enum FfiTableDisplay {
    Other,
    TableRoot,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableColumnGroup,
    TableColumn,
    TableRow,
    TableCell,
    TableCaption,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodeDisplayFlag {
    InlineOutside = 1 << 0,
    FlowInside = 1 << 1,
    FlexInside = 1 << 2,
    GridInside = 1 << 3,
    Floating = 1 << 4,
    AbsolutelyPositioned = 1 << 5,
    MathInside = 1 << 6,
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
    pub intrinsic_cache_epoch: u16,
    pub flags: u32,
    pub initial_quote_nesting_level: u32,
    pub layout_index: u32,
    pub table_display: FfiTableDisplay,
    pub table_display_before: FfiTableDisplay,
    pub display_bits: u8,
    pub slot_generation: u8,
    pub style: *const c_void,
    pub shell: *mut c_void,
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
            intrinsic_cache_epoch: 0,
            flags: 0,
            initial_quote_nesting_level: 0,
            layout_index: 0,
            table_display: FfiTableDisplay::Other,
            table_display_before: FfiTableDisplay::Other,
            display_bits: 0,
            slot_generation: 0,
            style: std::ptr::null(),
            shell: std::ptr::null_mut(),
        }
    }
}
