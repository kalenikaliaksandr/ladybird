/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::grid::GridFormattingContext;
use super::sizing::{Node, SizingContext};
use super::{FfiChildLayoutResult, FfiFlexAxis, FfiFormattingContextType, FfiLayoutFcCallbacks};
use crate::abort_on_panic;
use crate::css_pixels::CssPixels;
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::{
    AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput, LogicalOffset, LogicalRect,
    LogicalSize,
};
use crate::layout_state::{
    FfiAbsposAlignment, FfiAbsposAxisMode, FfiAbsposContainingBlockInfo, FfiAbsposLayoutInputs,
    FfiStaticPositionAlignment, FfiStaticPositionRect, state_mut,
};
use crate::style_facts::{FfiSizeValue, FfiStyleFacts};
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use std::ffi::c_void;

const LAYOUT_MODE_NORMAL: u8 = 0;
const POSITION_RELATIVE: u8 = 2;
const DIRECTION_LTR: u8 = 0;
const WRITING_MODE_HORIZONTAL_TB: u8 = 0;
const LENGTH_UNIT_PX: u8 = 29;
const CALC_NUMERIC_KIND_LENGTH: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum FfiAnchorSideKind {
    Invalid,
    Top,
    Right,
    Bottom,
    Left,
    Center,
    Start,
    End,
    SelfStart,
    SelfEnd,
    Inside,
    Outside,
    Percentage,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiAnchorFunctionFacts {
    pub has_anchor_name: bool,
    pub anchor_name: usize,
    pub side_kind: FfiAnchorSideKind,
    pub side_percentage: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiAnchorFallbackKind {
    None,
    Px,
    Percentage,
    Calculated,
    Anchor,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiAnchorFallbackFacts {
    pub kind: FfiAnchorFallbackKind,
    pub px: CssPixels,
    pub fraction: f64,
    pub value: *const c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiResolvedAnchorInsets {
    pub resolves_top: bool,
    pub top_is_auto: bool,
    pub top: CssPixels,
    pub resolves_right: bool,
    pub right_is_auto: bool,
    pub right: CssPixels,
    pub resolves_bottom: bool,
    pub bottom_is_auto: bool,
    pub bottom: CssPixels,
    pub resolves_left: bool,
    pub left_is_auto: bool,
    pub left: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LineFragmentFacts {
    layout_node: *mut c_void,
    is_atomic_inline: bool,
    writing_mode: u8,
    style_block_axis_is_reverse: bool,
    inline_offset: CssPixels,
    block_offset: CssPixels,
    offset: FfiCssPixelPoint,
    size: FfiCssPixelPoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PhysicalRect {
    x: CssPixels,
    y: CssPixels,
    width: CssPixels,
    height: CssPixels,
}

impl PhysicalRect {
    fn left(self) -> CssPixels {
        self.x
    }

    fn top(self) -> CssPixels {
        self.y
    }

    fn right(self) -> CssPixels {
        self.x + self.width
    }

    fn bottom(self) -> CssPixels {
        self.y + self.height
    }

    fn is_empty(self) -> bool {
        self.width <= CssPixels::default() || self.height <= CssPixels::default()
    }

    fn translated(self, offset: FfiCssPixelPoint) -> Self {
        Self {
            x: self.x + offset.x,
            y: self.y + offset.y,
            ..self
        }
    }

    fn union(self, other: Self) -> Self {
        let left = self.left().min(other.left());
        let top = self.top().min(other.top());
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }
}

fn point_add(left: FfiCssPixelPoint, right: FfiCssPixelPoint) -> FfiCssPixelPoint {
    FfiCssPixelPoint {
        x: left.x + right.x,
        y: left.y + right.y,
    }
}

fn point_sub(left: FfiCssPixelPoint, right: FfiCssPixelPoint) -> FfiCssPixelPoint {
    FfiCssPixelPoint {
        x: left.x - right.x,
        y: left.y - right.y,
    }
}

fn translate_static_position_between_chains(
    mut rect: FfiStaticPositionRect,
    static_chain_offset: FfiCssPixelPoint,
    containing_chain_offset: FfiCssPixelPoint,
) -> FfiStaticPositionRect {
    let physical_offset = point_sub(static_chain_offset, containing_chain_offset);
    rect.rect.offset.inline_offset += physical_offset.x;
    rect.rect.offset.block_offset += physical_offset.y;
    rect
}

fn anchor_rect_from_geometry(
    anchor_state: &UsedValuesCore,
    containing_block_state: &UsedValuesCore,
    anchor_offset: FfiCssPixelPoint,
) -> PhysicalRect {
    let collapsed = anchor_state.uses_collapsing_borders_model;
    PhysicalRect {
        x: anchor_offset.x - anchor_state.border_box_left(collapsed) + containing_block_state.padding_left,
        y: anchor_offset.y - anchor_state.border_box_top(collapsed) + containing_block_state.padding_top,
        width: anchor_state.border_box_inline_size(collapsed),
        height: anchor_state.border_box_block_size(collapsed),
    }
}

fn clamp_to_max_dimension_value(value: CssPixels) -> CssPixels {
    if matches!(value.raw_value(), i32::MIN | i32::MAX) {
        CssPixels::from_integer(17_895_700)
    } else {
        value
    }
}

fn axis_modes(style: FfiStyleFacts) -> (FfiAbsposAxisMode, FfiAbsposAxisMode) {
    (
        if style.inset_left.is_auto() && style.inset_right.is_auto() {
            FfiAbsposAxisMode::StaticPosition
        } else {
            FfiAbsposAxisMode::InsetFromRect
        },
        if style.inset_top.is_auto() && style.inset_bottom.is_auto() {
            FfiAbsposAxisMode::StaticPosition
        } else {
            FfiAbsposAxisMode::InsetFromRect
        },
    )
}

fn aligned_static_offset(
    static_position_rect: FfiStaticPositionRect,
    margin_box_inline_size: CssPixels,
    margin_box_block_size: CssPixels,
) -> LogicalOffset {
    let mut offset = static_position_rect.rect.offset;
    match static_position_rect.inline_alignment {
        FfiStaticPositionAlignment::Start => {}
        FfiStaticPositionAlignment::Center => {
            offset.inline_offset += (static_position_rect.rect.size.inline_size - margin_box_inline_size) / 2;
        }
        FfiStaticPositionAlignment::End => {
            offset.inline_offset += static_position_rect.rect.size.inline_size - margin_box_inline_size;
        }
    }
    match static_position_rect.block_alignment {
        FfiStaticPositionAlignment::Start => {}
        FfiStaticPositionAlignment::Center => {
            offset.block_offset += (static_position_rect.rect.size.block_size - margin_box_block_size) / 2;
        }
        FfiStaticPositionAlignment::End => {
            offset.block_offset += static_position_rect.rect.size.block_size - margin_box_block_size;
        }
    }
    offset
}

pub(crate) struct AbsposEngine {
    state: *mut c_void,
    callbacks: FfiLayoutFcCallbacks,
    layout_mode: u8,
    context_box: Node,
    grid_context: *const GridFormattingContext,
}

impl AbsposEngine {
    fn new(
        state: *mut c_void,
        callbacks: FfiLayoutFcCallbacks,
        layout_mode: u8,
        context_box: Node,
        grid_context: *const GridFormattingContext,
    ) -> Self {
        assert!(!state.is_null());
        assert!(!context_box.is_null());
        Self {
            state,
            callbacks,
            layout_mode,
            context_box,
            grid_context,
        }
    }

    fn sizing(&self) -> SizingContext {
        SizingContext::new(self.state, self.callbacks)
    }

    fn style(&self, node: Node) -> FfiStyleFacts {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn facts(&self, node: Node) -> crate::box_facts::FfiLayoutBoxFacts {
        state_mut(self.state).box_facts(&self.callbacks, node)
    }

    fn used_pointer(&self, node: Node) -> *mut UsedValuesCore {
        state_mut(self.state).used_values(&self.callbacks, node)
    }

    fn try_used_pointer(&self, node: Node) -> *mut UsedValuesCore {
        state_mut(self.state).try_used_values(&self.callbacks, node)
    }

    fn used(&self, node: Node) -> &UsedValuesCore {
        let used = self.used_pointer(node);
        // SAFETY: Used-values entries are stable for the lifetime of the
        // Rust-owned layout state.
        unsafe { &*used }
    }

    // The used-values store owns stable, disjoint entries and layout is
    // single-threaded. Mutability is mediated by the opaque state pointer,
    // so it cannot be expressed through the borrow of `self`.
    #[allow(clippy::mut_from_ref)]
    fn used_mut(&self, node: Node) -> &mut UsedValuesCore {
        let used = self.used_pointer(node);
        // SAFETY: Layout is single-threaded and this engine serializes all
        // mutations of the selected entry.
        unsafe { &mut *used }
    }

    fn navigate(&self, callback: crate::box_facts::FfiLayoutNavCallback, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: Navigation is synchronous and all layout nodes remain live
        // throughout the layout pass.
        unsafe { callback(self.callbacks.navigation.context, node) }
    }

    fn parent(&self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.parent, node)
    }

    fn first_child(&self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.first_child, node)
    }

    fn next_sibling(&self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.next_sibling, node)
    }

    fn containing_block(&self, node: Node) -> Node {
        self.navigate(self.callbacks.navigation.containing_block, node)
    }

    fn static_position_containing_block(&self, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: See navigate().
        unsafe { (self.callbacks.static_position_containing_block)(self.callbacks.context, node) }
    }

    fn inline_containing_block(&self, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: See navigate().
        unsafe { (self.callbacks.inline_containing_block)(self.callbacks.context, node) }
    }

    fn non_anonymous_containing_block(&self, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: See navigate().
        unsafe { (self.callbacks.non_anonymous_containing_block)(self.callbacks.context, node) }
    }

    fn node_is_ancestor(&self, ancestor: Node, node: Node) -> bool {
        bump(FfiOp::NavigationCallback);
        // SAFETY: See navigate().
        unsafe { (self.callbacks.node_is_ancestor)(self.callbacks.context, ancestor, node) }
    }

    fn dom_node_is_inclusive_ancestor(&self, ancestor: Node, node: Node) -> bool {
        bump(FfiOp::NavigationCallback);
        // SAFETY: See navigate().
        unsafe { (self.callbacks.dom_node_is_inclusive_ancestor)(self.callbacks.context, ancestor, node) }
    }

    fn resolve_static_position_relative_to_containing_block(
        &self,
        node: Node,
        static_position_rect: FfiStaticPositionRect,
    ) -> FfiStaticPositionRect {
        let static_position_cb = self.static_position_containing_block(node);
        let actual_containing_block = self.containing_block(node);
        if static_position_cb.is_null() || static_position_cb == actual_containing_block {
            return static_position_rect;
        }

        let mut merge_point = static_position_cb;
        while merge_point != actual_containing_block && !self.node_is_ancestor(merge_point, actual_containing_block) {
            merge_point = self.containing_block(merge_point);
            assert!(!merge_point.is_null());
        }

        let offset_relative_to_merge_point = |descendant: Node| {
            let mut offset = FfiCssPixelPoint::default();
            let mut current = descendant;
            while current != merge_point {
                let used = self.used(current);
                offset = point_add(offset, used.content_offset);
                current = self.containing_block(current);
                assert!(!current.is_null());
            }
            offset
        };
        translate_static_position_between_chains(
            static_position_rect,
            offset_relative_to_merge_point(static_position_cb),
            offset_relative_to_merge_point(actual_containing_block),
        )
    }

    fn line_fragments(&self, node: Node) -> Vec<LineFragmentFacts> {
        let facts = self.facts(node);
        if !facts.has_layout_index {
            return Vec::new();
        }
        let mut fragments = Vec::new();
        let Some(lines) = state_mut(self.state).line_data(facts.layout_index) else {
            return fragments;
        };
        for line in &lines.line_boxes {
            for fragment in &line.fragments {
                let (x, y) = fragment.offset();
                let (width, height) = fragment.size();
                fragments.push(LineFragmentFacts {
                    layout_node: fragment.layout_node,
                    is_atomic_inline: fragment.is_atomic_inline,
                    writing_mode: fragment.writing_mode,
                    style_block_axis_is_reverse: fragment.style_block_axis_is_reverse,
                    inline_offset: fragment.inline_offset,
                    block_offset: fragment.block_offset,
                    offset: FfiCssPixelPoint { x, y },
                    size: FfiCssPixelPoint { x: width, y: height },
                });
            }
        }
        fragments
    }

    fn add_atomic_inline_fragment_rect(
        &self,
        inline_node: Node,
        fragment: LineFragmentFacts,
        offset: FfiCssPixelPoint,
        bounding_rect: &mut Option<PhysicalRect>,
        empty_bounding_rect: &mut Option<PhysicalRect>,
    ) {
        let child_used = self.try_used_pointer(fragment.layout_node);
        if child_used.is_null() {
            return;
        }
        bump(FfiOp::AbsposInlineCbAtomicFragment);
        // SAFETY: A non-null pointer is a stable state entry.
        let child_used = unsafe { &*child_used };
        let collapsed = child_used.uses_collapsing_borders_model;
        let is_horizontal = fragment.writing_mode == WRITING_MODE_HORIZONTAL_TB;
        let inline_axis_border_box_start = fragment.inline_offset
            - if is_horizontal {
                child_used.border_box_left(collapsed)
            } else {
                child_used.border_box_top(collapsed)
            };
        let inline_axis_border_box_extent = if is_horizontal {
            child_used.border_box_inline_size(collapsed)
        } else {
            child_used.border_box_block_size(collapsed)
        };
        let block_axis_line_height = self.style(inline_node).line_height;
        let block_axis_start = if fragment.style_block_axis_is_reverse {
            fragment.block_offset + child_used.border_box_right(collapsed) - block_axis_line_height
        } else {
            fragment.block_offset
                - if is_horizontal {
                    child_used.border_box_top(collapsed)
                } else {
                    child_used.border_box_left(collapsed)
                }
        };
        let rect = if is_horizontal {
            PhysicalRect {
                x: inline_axis_border_box_start,
                y: block_axis_start,
                width: inline_axis_border_box_extent,
                height: block_axis_line_height,
            }
        } else {
            PhysicalRect {
                x: block_axis_start,
                y: inline_axis_border_box_start,
                width: block_axis_line_height,
                height: inline_axis_border_box_extent,
            }
        }
        .translated(offset);
        add_fragment_rect(rect, bounding_rect, empty_bounding_rect);
    }

    fn walk_inline_containing_block(
        &self,
        inline_node: Node,
        node: Node,
        offset: FfiCssPixelPoint,
        bounding_rect: &mut Option<PhysicalRect>,
        empty_bounding_rect: &mut Option<PhysicalRect>,
    ) {
        for fragment in self.line_fragments(node) {
            if !self.dom_node_is_inclusive_ancestor(inline_node, fragment.layout_node) {
                continue;
            }
            if fragment.is_atomic_inline {
                self.add_atomic_inline_fragment_rect(inline_node, fragment, offset, bounding_rect, empty_bounding_rect);
                continue;
            }
            bump(FfiOp::AbsposInlineCbNormalFragment);
            add_fragment_rect(
                PhysicalRect {
                    x: fragment.offset.x + offset.x,
                    y: fragment.offset.y + offset.y,
                    width: fragment.size.x,
                    height: fragment.size.y,
                },
                bounding_rect,
                empty_bounding_rect,
            );
        }

        let mut child = self.first_child(node);
        while !child.is_null() {
            let next = self.next_sibling(child);
            let facts = self.facts(child);
            if facts.is_absolutely_positioned || facts.is_floating {
                child = next;
                continue;
            }
            let child_used_pointer = if facts.has_layout_index {
                self.try_used_pointer(child)
            } else {
                std::ptr::null_mut()
            };
            let child_offset = if child_used_pointer.is_null() {
                offset
            } else {
                // SAFETY: A non-null pointer is a stable state entry.
                let child_used = unsafe { &*child_used_pointer };
                point_add(offset, child_used.content_offset)
            };
            if facts.is_box && !facts.is_anonymous {
                if !self.dom_node_is_inclusive_ancestor(inline_node, child) {
                    child = next;
                    continue;
                }
                if facts.is_atomic_inline {
                    child = next;
                    continue;
                }
                if !child_used_pointer.is_null() {
                    // SAFETY: See above.
                    let child_used = unsafe { &*child_used_pointer };
                    let collapsed = child_used.uses_collapsing_borders_model;
                    let border_box_origin = FfiCssPixelPoint {
                        x: child_offset.x - child_used.border_left_collapsed(collapsed) - child_used.padding_left,
                        y: child_offset.y - child_used.border_top_collapsed(collapsed) - child_used.padding_top,
                    };
                    add_fragment_rect(
                        PhysicalRect {
                            x: border_box_origin.x,
                            y: border_box_origin.y,
                            width: child_used.border_box_inline_size(collapsed),
                            height: child_used.border_box_block_size(collapsed),
                        },
                        bounding_rect,
                        empty_bounding_rect,
                    );
                }
            }
            self.walk_inline_containing_block(inline_node, child, child_offset, bounding_rect, empty_bounding_rect);
            child = next;
        }
    }

    fn compute_inline_containing_block_rect(
        &self,
        inline_node: Node,
        abspos_containing_block: Node,
    ) -> Option<PhysicalRect> {
        bump(FfiOp::AbsposInlineCbAttempt);
        if !self.dom_node_is_inclusive_ancestor(inline_node, inline_node) {
            return None;
        }
        let outer_block = self.non_anonymous_containing_block(inline_node);
        if outer_block.is_null() {
            return None;
        }

        let mut outer_offset = FfiCssPixelPoint::default();
        let mut ancestor = outer_block;
        while !ancestor.is_null() && ancestor != abspos_containing_block {
            let used = self.try_used_pointer(ancestor);
            if !used.is_null() {
                // SAFETY: A non-null pointer is a stable state entry.
                outer_offset = point_add(outer_offset, unsafe { (*used).content_offset });
            }
            ancestor = self.parent(ancestor);
        }

        let mut bounding_rect = None;
        let mut empty_bounding_rect = None;
        self.walk_inline_containing_block(
            inline_node,
            outer_block,
            outer_offset,
            &mut bounding_rect,
            &mut empty_bounding_rect,
        );
        let mut rect = bounding_rect.or(empty_bounding_rect)?;
        bump(FfiOp::AbsposInlineCbSuccess);
        let inline_used = self.try_used_pointer(inline_node);
        if !inline_used.is_null() {
            // SAFETY: A non-null pointer is a stable state entry.
            let inline_used = unsafe { &*inline_used };
            rect.x -= inline_used.padding_left;
            rect.y -= inline_used.padding_top;
            rect.width += inline_used.padding_left + inline_used.padding_right;
            rect.height += inline_used.padding_top + inline_used.padding_bottom;
        }
        Some(rect)
    }

    fn base_containing_block_info(&self, node: Node) -> FfiAbsposContainingBlockInfo {
        let style = self.style(node);
        let (inline_axis_mode, block_axis_mode) = axis_modes(style);
        let containing_block = self.containing_block(node);
        assert!(!containing_block.is_null());
        let inline_containing_block = self.inline_containing_block(node);
        if !inline_containing_block.is_null()
            && let Some(rect) = self.compute_inline_containing_block_rect(inline_containing_block, containing_block)
        {
            return FfiAbsposContainingBlockInfo {
                rect: LogicalRect {
                    offset: LogicalOffset {
                        inline_offset: rect.x,
                        block_offset: rect.y,
                    },
                    size: LogicalSize {
                        inline_size: rect.width,
                        block_size: rect.height,
                    },
                },
                inline_axis_mode,
                block_axis_mode,
                has_inline_alignment: false,
                inline_alignment: FfiAbsposAlignment::Normal,
                has_block_alignment: false,
                block_alignment: FfiAbsposAlignment::Normal,
                derives_from_own_computed_values: false,
            };
        }

        let containing_block_used = self.used(containing_block);
        FfiAbsposContainingBlockInfo {
            rect: LogicalRect {
                offset: LogicalOffset {
                    inline_offset: -containing_block_used.padding_left,
                    block_offset: -containing_block_used.padding_top,
                },
                size: LogicalSize {
                    inline_size: containing_block_used.content_inline_size
                        + containing_block_used.padding_left
                        + containing_block_used.padding_right,
                    block_size: containing_block_used.content_block_size
                        + containing_block_used.padding_top
                        + containing_block_used.padding_bottom,
                },
            },
            inline_axis_mode,
            block_axis_mode,
            has_inline_alignment: false,
            inline_alignment: FfiAbsposAlignment::Normal,
            has_block_alignment: false,
            block_alignment: FfiAbsposAlignment::Normal,
            derives_from_own_computed_values: false,
        }
    }

    fn containing_block_info(&self, node: Node) -> FfiAbsposContainingBlockInfo {
        let base = self.base_containing_block_info(node);
        if self.grid_context.is_null() {
            return base;
        }
        // SAFETY: The pointer refers to the grid context owned by the live
        // FormattingContextInstance for the duration of this call.
        let grid = unsafe { &*self.grid_context };
        let mut info = grid.abspos_containing_block_info(node);
        let uses_grid_area_as_static_position = self.static_position_containing_block(node) == self.context_box;
        if !uses_grid_area_as_static_position {
            info.inline_axis_mode = base.inline_axis_mode;
            info.block_axis_mode = base.block_axis_mode;
        }
        info
    }
}

fn add_fragment_rect(
    rect: PhysicalRect,
    bounding_rect: &mut Option<PhysicalRect>,
    empty_bounding_rect: &mut Option<PhysicalRect>,
) {
    let destination = if rect.is_empty() {
        empty_bounding_rect
    } else {
        bounding_rect
    };
    *destination = Some(destination.map_or(rect, |existing| existing.union(rect)));
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiNumericType {
    has_exponent: [bool; 7],
    exponents: [i32; 7],
    has_percent_hint: bool,
    percent_hint: u8,
    valid: bool,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiResolvedCalc {
    resolved: bool,
    value: f64,
    numeric_type: CssFfiNumericType,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiCalcResolutionContext {
    basis_kind: u8,
    basis_value: f64,
    basis_unit: u8,
    length_resolution_context: *const c_void,
    callback_context: *mut c_void,
    resolve_non_math_function: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_channel_keyword: unsafe extern "C" fn(*mut c_void, u8, *mut f64) -> bool,
    random_base_value: unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> bool,
    absolutize_random_sharing: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_length: unsafe extern "C" fn(*mut c_void, f64, u8, *mut f64) -> bool,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn rust_calc_resolve(
        calculated: *const c_void,
        context: *const CssFfiCalcResolutionContext,
        apply_censoring_and_clamping: bool,
    ) -> CssFfiResolvedCalc;
    fn rust_calc_node_create_numeric_dimension(kind: u8, value: f64, unit: u8) -> *const c_void;
    fn ladybird_layout_release_anchor_name_handle(raw: usize);
}

#[cfg(test)]
unsafe fn rust_calc_resolve(
    _calculated: *const c_void,
    _context: *const CssFfiCalcResolutionContext,
    _apply_censoring_and_clamping: bool,
) -> CssFfiResolvedCalc {
    CssFfiResolvedCalc {
        resolved: false,
        value: 0.0,
        numeric_type: CssFfiNumericType {
            has_exponent: [false; 7],
            exponents: [0; 7],
            has_percent_hint: false,
            percent_hint: 0,
            valid: false,
        },
    }
}

#[cfg(test)]
unsafe fn rust_calc_node_create_numeric_dimension(_kind: u8, _value: f64, _unit: u8) -> *const c_void {
    std::ptr::null()
}

#[cfg(test)]
unsafe fn ladybird_layout_release_anchor_name_handle(_raw: usize) {}

unsafe extern "C" fn no_channel_keyword(_context: *mut c_void, _channel: u8, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_random_base_value(_context: *mut c_void, _sharing: *const c_void, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_absolutized_random_sharing(_context: *mut c_void, _sharing: *const c_void) -> *const c_void {
    std::ptr::null()
}

unsafe extern "C" fn no_fallback_length(_context: *mut c_void, _value: f64, _unit: u8, _out: *mut f64) -> bool {
    false
}

struct AnchorResolutionState {
    default_anchor_box: Node,
    compensates_for_horizontal_scroll: bool,
    compensates_for_vertical_scroll: bool,
}

#[derive(Clone, Copy)]
struct AnchorValueAxis {
    is_from_end: bool,
    is_horizontal: bool,
    containing_block_extent: CssPixels,
}

#[derive(Clone, Copy)]
struct AnchorCalcCallbackContext {
    engine: *const AbsposEngine,
    positioned_box: Node,
    containing_block: Node,
    is_from_end: bool,
    is_horizontal_axis: bool,
    containing_block_extent: CssPixels,
    resolution_state: *mut AnchorResolutionState,
}

impl AbsposEngine {
    fn anchor_lookup(&self, positioned_box: Node, anchor_name: usize) -> Option<Node> {
        let mut anchor_box = std::ptr::null_mut();
        bump(FfiOp::AbsposAnchorLookupCallback);
        // SAFETY: The name handle is retained by either the style snapshot or
        // the live anchor() shell, and the out pointer is stack-local.
        let found = unsafe {
            (self.callbacks.anchor_lookup)(self.callbacks.context, positioned_box, anchor_name, &raw mut anchor_box)
        };
        if found {
            assert!(!anchor_box.is_null());
            Some(anchor_box)
        } else {
            None
        }
    }

    fn nearest_scroll_container_ancestor(&self, node: Node) -> Node {
        let mut ancestor = self.containing_block(node);
        while !ancestor.is_null() {
            if self.facts(ancestor).is_scroll_container {
                return ancestor;
            }
            ancestor = self.containing_block(ancestor);
        }
        std::ptr::null_mut()
    }

    fn anchor_rect(&self, anchor_box: Node, containing_block: Node) -> PhysicalRect {
        let anchor_state = self.used(anchor_box);
        let mut anchor_offset = FfiCssPixelPoint::default();
        let mut node = anchor_box;
        while node != containing_block {
            assert!(!node.is_null());
            anchor_offset = point_add(anchor_offset, self.used(node).content_offset);
            node = self.containing_block(node);
        }
        anchor_rect_from_geometry(anchor_state, self.used(containing_block), anchor_offset)
    }

    fn anchor_side(
        &self,
        facts: FfiAnchorFunctionFacts,
        rect: PhysicalRect,
        positioned_box: Node,
        containing_block: Node,
        is_from_end: bool,
        is_horizontal_axis: bool,
    ) -> Option<CssPixels> {
        let containing_block_direction = self.style(containing_block).direction;
        let box_direction = self.style(positioned_box).direction;
        match facts.side_kind {
            FfiAnchorSideKind::Invalid => None,
            FfiAnchorSideKind::Top => (!is_horizontal_axis).then_some(rect.top()),
            FfiAnchorSideKind::Bottom => (!is_horizontal_axis).then_some(rect.bottom()),
            FfiAnchorSideKind::Left => is_horizontal_axis.then_some(rect.left()),
            FfiAnchorSideKind::Right => is_horizontal_axis.then_some(rect.right()),
            FfiAnchorSideKind::Center => Some(if is_horizontal_axis {
                rect.left() + rect.width / 2
            } else {
                rect.top() + rect.height / 2
            }),
            FfiAnchorSideKind::Start | FfiAnchorSideKind::End => {
                let is_start = facts.side_kind == FfiAnchorSideKind::Start;
                if is_horizontal_axis {
                    let use_left = (containing_block_direction == DIRECTION_LTR) == is_start;
                    Some(if use_left { rect.left() } else { rect.right() })
                } else {
                    Some(if is_start { rect.top() } else { rect.bottom() })
                }
            }
            FfiAnchorSideKind::SelfStart | FfiAnchorSideKind::SelfEnd => {
                let is_start = facts.side_kind == FfiAnchorSideKind::SelfStart;
                if is_horizontal_axis {
                    let use_left = (box_direction == DIRECTION_LTR) == is_start;
                    Some(if use_left { rect.left() } else { rect.right() })
                } else {
                    Some(if is_start { rect.top() } else { rect.bottom() })
                }
            }
            FfiAnchorSideKind::Inside | FfiAnchorSideKind::Outside => {
                let same_side = facts.side_kind == FfiAnchorSideKind::Inside;
                if is_horizontal_axis {
                    Some(if is_from_end == same_side {
                        rect.right()
                    } else {
                        rect.left()
                    })
                } else {
                    Some(if is_from_end == same_side {
                        rect.bottom()
                    } else {
                        rect.top()
                    })
                }
            }
            FfiAnchorSideKind::Percentage => {
                if is_horizontal_axis {
                    let (start, end) = if containing_block_direction == DIRECTION_LTR {
                        (rect.left(), rect.right())
                    } else {
                        (rect.right(), rect.left())
                    };
                    Some(start + CssPixels::nearest_value_for((end - start).to_double() * facts.side_percentage))
                } else {
                    Some(rect.top() + CssPixels::nearest_value_for(rect.height.to_double() * facts.side_percentage))
                }
            }
        }
    }

    fn note_resolved_anchor_function(
        &self,
        anchor_box: Node,
        is_horizontal_axis: bool,
        state: &mut AnchorResolutionState,
    ) {
        if state.default_anchor_box.is_null() {
            return;
        }
        if anchor_box != state.default_anchor_box
            && self.nearest_scroll_container_ancestor(anchor_box)
                != self.nearest_scroll_container_ancestor(state.default_anchor_box)
        {
            return;
        }
        if is_horizontal_axis {
            state.compensates_for_horizontal_scroll = true;
        } else {
            state.compensates_for_vertical_scroll = true;
        }
    }

    fn resolve_anchor_value(
        &self,
        value: FfiSizeValue,
        positioned_box: Node,
        containing_block: Node,
        axis: AnchorValueAxis,
        resolution_state: &mut AnchorResolutionState,
    ) -> Option<CssPixels> {
        assert!(value.contains_anchor_function);
        assert!(!value.calc.is_null());
        let mut callback_context = AnchorCalcCallbackContext {
            engine: self,
            positioned_box,
            containing_block,
            is_from_end: axis.is_from_end,
            is_horizontal_axis: axis.is_horizontal,
            containing_block_extent: axis.containing_block_extent,
            resolution_state,
        };
        let context = CssFfiCalcResolutionContext {
            basis_kind: 3,
            basis_value: axis.containing_block_extent.to_double(),
            basis_unit: LENGTH_UNIT_PX,
            length_resolution_context: std::ptr::null(),
            callback_context: (&raw mut callback_context).cast(),
            resolve_non_math_function: resolve_anchor_non_math_function,
            resolve_channel_keyword: no_channel_keyword,
            random_base_value: no_random_base_value,
            absolutize_random_sharing: no_absolutized_random_sharing,
            resolve_length: no_fallback_length,
        };
        // SAFETY: The calculated handle is retained by the style cache and
        // all callback state remains live for this synchronous resolution.
        let result = unsafe { rust_calc_resolve(value.calc, &raw const context, true) };
        result.resolved.then(|| CssPixels::nearest_value_for(result.value))
    }

    fn resolve_anchor_insets(&self, node: Node) {
        bump(FfiOp::AbsposAnchorResolve);
        // Clear a stale default scroll shift before any early return.
        bump(FfiOp::AbsposSetScrollShiftCallback);
        // SAFETY: The node is live and a null anchor clears the weak target.
        unsafe {
            (self.callbacks.set_default_scroll_shift)(self.callbacks.context, node, std::ptr::null_mut(), false, false);
        }

        let style = self.style(node);
        let top_contains_anchor = style.inset_top.contains_anchor_function;
        let right_contains_anchor = style.inset_right.contains_anchor_function;
        let bottom_contains_anchor = style.inset_bottom.contains_anchor_function;
        let left_contains_anchor = style.inset_left.contains_anchor_function;
        if !top_contains_anchor && !right_contains_anchor && !bottom_contains_anchor && !left_contains_anchor {
            return;
        }

        let containing_block = self.containing_block(node);
        if containing_block.is_null() {
            return;
        }
        let containing_block_state = self.used(containing_block);
        let default_anchor_box = if style.has_position_anchor {
            self.anchor_lookup(node, style.position_anchor_name)
                .unwrap_or(std::ptr::null_mut())
        } else {
            std::ptr::null_mut()
        };
        let mut resolution_state = AnchorResolutionState {
            default_anchor_box,
            compensates_for_horizontal_scroll: false,
            compensates_for_vertical_scroll: false,
        };
        let mut resolved = FfiResolvedAnchorInsets::default();

        if top_contains_anchor {
            let value = self.resolve_anchor_value(
                style.inset_top,
                node,
                containing_block,
                AnchorValueAxis {
                    is_from_end: false,
                    is_horizontal: false,
                    containing_block_extent: containing_block_state.content_block_size
                        + containing_block_state.padding_top
                        + containing_block_state.padding_bottom,
                },
                &mut resolution_state,
            );
            resolved.resolves_top = true;
            resolved.top_is_auto = value.is_none();
            resolved.top = value.unwrap_or_default();
        }
        if right_contains_anchor {
            let value = self.resolve_anchor_value(
                style.inset_right,
                node,
                containing_block,
                AnchorValueAxis {
                    is_from_end: true,
                    is_horizontal: true,
                    containing_block_extent: containing_block_state.content_inline_size
                        + containing_block_state.padding_left
                        + containing_block_state.padding_right,
                },
                &mut resolution_state,
            );
            resolved.resolves_right = true;
            resolved.right_is_auto = value.is_none();
            resolved.right = value.unwrap_or_default();
        }
        if bottom_contains_anchor {
            let value = self.resolve_anchor_value(
                style.inset_bottom,
                node,
                containing_block,
                AnchorValueAxis {
                    is_from_end: true,
                    is_horizontal: false,
                    containing_block_extent: containing_block_state.content_block_size
                        + containing_block_state.padding_top
                        + containing_block_state.padding_bottom,
                },
                &mut resolution_state,
            );
            resolved.resolves_bottom = true;
            resolved.bottom_is_auto = value.is_none();
            resolved.bottom = value.unwrap_or_default();
        }
        if left_contains_anchor {
            let value = self.resolve_anchor_value(
                style.inset_left,
                node,
                containing_block,
                AnchorValueAxis {
                    is_from_end: false,
                    is_horizontal: true,
                    containing_block_extent: containing_block_state.content_inline_size
                        + containing_block_state.padding_left
                        + containing_block_state.padding_right,
                },
                &mut resolution_state,
            );
            resolved.resolves_left = true;
            resolved.left_is_auto = value.is_none();
            resolved.left = value.unwrap_or_default();
        }

        bump(FfiOp::AbsposSetResolvedInsetsCallback);
        // SAFETY: The callback synchronously updates this live box's computed
        // values with plain auto/px insets.
        unsafe {
            (self.callbacks.set_resolved_anchor_insets)(self.callbacks.context, node, resolved);
        }
        state_mut(self.state).replace_resolved_anchor_insets(&self.callbacks, node, resolved);

        if resolution_state.compensates_for_horizontal_scroll || resolution_state.compensates_for_vertical_scroll {
            bump(FfiOp::AbsposSetScrollShiftCallback);
            // SAFETY: The anchor and positioned box remain live through the
            // pass; C++ stores the anchor as a weak pointer.
            unsafe {
                (self.callbacks.set_default_scroll_shift)(
                    self.callbacks.context,
                    node,
                    resolution_state.default_anchor_box,
                    resolution_state.compensates_for_horizontal_scroll,
                    resolution_state.compensates_for_vertical_scroll,
                );
            }
        }
    }
}

unsafe extern "C" fn resolve_anchor_non_math_function(context: *mut c_void, shell: *const c_void) -> *const c_void {
    // SAFETY: The CSS calc engine calls this only during resolve_anchor_value,
    // whose stack owns this callback context.
    let context = unsafe { &mut *context.cast::<AnchorCalcCallbackContext>() };
    // SAFETY: The engine pointer is live for the enclosing resolution.
    let engine = unsafe { &*context.engine };
    bump(FfiOp::AbsposAnchorFactsCallback);
    // SAFETY: `shell` is the live C++ style-value shell supplied by the CSS
    // calc core.
    let facts = unsafe { (engine.callbacks.build_anchor_function_facts)(engine.callbacks.context, shell) };
    let style = engine.style(context.positioned_box);
    let anchor_name = if facts.has_anchor_name {
        Some(facts.anchor_name)
    } else if style.has_position_anchor {
        Some(style.position_anchor_name)
    } else {
        None
    };
    let mut resolved_node = std::ptr::null();
    if engine.facts(context.positioned_box).is_absolutely_positioned
        && let Some(anchor_name) = anchor_name
        && let Some(anchor_box) = engine.anchor_lookup(context.positioned_box, anchor_name)
    {
        let rect = engine.anchor_rect(anchor_box, context.containing_block);
        if let Some(side) = engine.anchor_side(
            facts,
            rect,
            context.positioned_box,
            context.containing_block,
            context.is_from_end,
            context.is_horizontal_axis,
        ) {
            // SAFETY: The state pointer is live and uniquely used by this
            // synchronous resolver.
            let resolution_state = unsafe { &mut *context.resolution_state };
            engine.note_resolved_anchor_function(anchor_box, context.is_horizontal_axis, resolution_state);
            let inset = if context.is_from_end {
                context.containing_block_extent - side
            } else {
                side
            };
            // SAFETY: This CSS crate export transfers one Arc reference to
            // the calc resolver, which consumes it on return.
            resolved_node = unsafe {
                rust_calc_node_create_numeric_dimension(CALC_NUMERIC_KIND_LENGTH, inset.to_double(), LENGTH_UNIT_PX)
            };
        }
    }
    if facts.has_anchor_name {
        // SAFETY: The C++ facts callback transferred one raw fly-string
        // reference for this explicit anchor name.
        unsafe {
            ladybird_layout_release_anchor_name_handle(facts.anchor_name);
        }
    }
    if !resolved_node.is_null() {
        return resolved_node;
    }

    bump(FfiOp::AbsposAnchorFallbackCallback);
    // SAFETY: The callback borrows fallback data from the live anchor style
    // value for this synchronous resolution.
    let fallback = unsafe { (engine.callbacks.anchor_function_fallback)(engine.callbacks.context, shell) };
    match fallback.kind {
        FfiAnchorFallbackKind::None => std::ptr::null(),
        FfiAnchorFallbackKind::Px => unsafe {
            rust_calc_node_create_numeric_dimension(CALC_NUMERIC_KIND_LENGTH, fallback.px.to_double(), LENGTH_UNIT_PX)
        },
        FfiAnchorFallbackKind::Percentage => unsafe {
            rust_calc_node_create_numeric_dimension(
                CALC_NUMERIC_KIND_LENGTH,
                context.containing_block_extent.to_double() * fallback.fraction,
                LENGTH_UNIT_PX,
            )
        },
        FfiAnchorFallbackKind::Calculated => {
            assert!(!fallback.value.is_null());
            let mut nested_context = *context;
            let ffi_context = CssFfiCalcResolutionContext {
                basis_kind: 3,
                basis_value: context.containing_block_extent.to_double(),
                basis_unit: LENGTH_UNIT_PX,
                length_resolution_context: std::ptr::null(),
                callback_context: (&raw mut nested_context).cast(),
                resolve_non_math_function: resolve_anchor_non_math_function,
                resolve_channel_keyword: no_channel_keyword,
                random_base_value: no_random_base_value,
                absolutize_random_sharing: no_absolutized_random_sharing,
                resolve_length: no_fallback_length,
            };
            let resolved = unsafe { rust_calc_resolve(fallback.value, &raw const ffi_context, true) };
            if !resolved.resolved {
                return std::ptr::null();
            }
            unsafe { rust_calc_node_create_numeric_dimension(CALC_NUMERIC_KIND_LENGTH, resolved.value, LENGTH_UNIT_PX) }
        }
        FfiAnchorFallbackKind::Anchor => {
            assert!(!fallback.value.is_null());
            let mut nested_context = *context;
            unsafe { resolve_anchor_non_math_function((&raw mut nested_context).cast(), fallback.value) }
        }
    }
}

type AutoPx = Option<CssPixels>;

fn resolve_or_auto(value: FfiSizeValue, basis: CssPixels) -> AutoPx {
    (!value.is_auto()).then(|| value.to_px(basis))
}

fn auto_px_value(value: AutoPx) -> CssPixels {
    value.unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn solve_abspos_axis_for(
    available: CssPixels,
    target: AutoPx,
    clamp_to_zero: bool,
    start: AutoPx,
    margin_start: AutoPx,
    border_start: CssPixels,
    padding_start: CssPixels,
    size: AutoPx,
    padding_end: CssPixels,
    border_end: CssPixels,
    margin_end: AutoPx,
    end: AutoPx,
) -> CssPixels {
    let value = available
        - auto_px_value(start)
        - auto_px_value(margin_start)
        - border_start
        - padding_start
        - auto_px_value(size)
        - padding_end
        - border_end
        - auto_px_value(margin_end)
        - auto_px_value(end)
        + auto_px_value(target);
    if clamp_to_zero {
        value.max(CssPixels::default())
    } else {
        value
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReplacedAxisSolution {
    start: CssPixels,
    end: CssPixels,
    margin_start: CssPixels,
    margin_end: CssPixels,
}

#[derive(Clone, Copy)]
struct ReplacedAxisBehavior {
    clear_auto_margins_if_start_is_auto: bool,
    clear_negative_auto_margins: bool,
}

fn solve_replaced_axis(
    available: CssPixels,
    mut start: AutoPx,
    mut end: AutoPx,
    mut margin_start: AutoPx,
    mut margin_end: AutoPx,
    static_offset: CssPixels,
    behavior: ReplacedAxisBehavior,
) -> ReplacedAxisSolution {
    if start.is_none() && end.is_none() {
        start = Some(static_offset);
    }
    if end.is_none() || (behavior.clear_auto_margins_if_start_is_auto && start.is_none()) {
        if margin_start.is_none() {
            margin_start = Some(CssPixels::default());
        }
        if margin_end.is_none() {
            margin_end = Some(CssPixels::default());
        }
    }
    if margin_start.is_none() && margin_end.is_none() {
        let remainder = available - auto_px_value(start) - auto_px_value(end);
        if behavior.clear_negative_auto_margins && remainder < CssPixels::default() {
            // This deliberately matches the C++ inline-axis implementation,
            // which zeroes both margins instead of solving the end margin.
            margin_start = Some(CssPixels::default());
            margin_end = Some(CssPixels::default());
        } else {
            margin_start = Some(remainder / 2);
            margin_end = Some(remainder / 2);
        }
    }
    if start.is_none() {
        start = Some(available - auto_px_value(end) - auto_px_value(margin_start) - auto_px_value(margin_end));
    } else if end.is_none() {
        end = Some(available - auto_px_value(start) - auto_px_value(margin_start) - auto_px_value(margin_end));
    } else if margin_start.is_none() {
        margin_start = Some(available - auto_px_value(start) - auto_px_value(end) - auto_px_value(margin_end));
    } else if margin_end.is_none() {
        margin_end = Some(available - auto_px_value(start) - auto_px_value(margin_start) - auto_px_value(end));
    }
    if CssPixels::default()
        != available
            - auto_px_value(start)
            - auto_px_value(end)
            - auto_px_value(margin_start)
            - auto_px_value(margin_end)
    {
        end = Some(available - auto_px_value(start) - auto_px_value(margin_start) - auto_px_value(margin_end));
    }
    ReplacedAxisSolution {
        start: auto_px_value(start),
        end: auto_px_value(end),
        margin_start: auto_px_value(margin_start),
        margin_end: auto_px_value(margin_end),
    }
}

impl AbsposEngine {
    fn static_offset(&self, node: Node, rect: FfiStaticPositionRect) -> LogicalOffset {
        let used = self.used(node);
        let collapsed = used.uses_collapsing_borders_model;
        aligned_static_offset(
            rect,
            used.margin_box_inline_size(collapsed),
            used.margin_box_block_size(collapsed),
        )
    }

    fn solve_non_replaced_inline_once(
        &self,
        node: Node,
        containing_block_inline_size: CssPixels,
        _available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
        input_inline_size: AutoPx,
    ) -> (AutoPx, CssPixels, CssPixels, AutoPx, AutoPx) {
        let style = self.style(node);
        let used = self.used(node);
        let border_left = style.border_left_width;
        let border_right = style.border_right_width;
        let padding_left = used.padding_left;
        let padding_right = used.padding_right;
        let computed_left = style.inset_left;
        let computed_right = style.inset_right;
        let mut left = style.inset_left.to_px(containing_block_inline_size);
        let mut right = style.inset_right.to_px(containing_block_inline_size);
        let mut margin_left = resolve_or_auto(style.margin_left, containing_block_inline_size);
        let mut margin_right = resolve_or_auto(style.margin_right, containing_block_inline_size);
        let mut inline_size = input_inline_size;

        let solve_for_left = |inline_size: AutoPx, margin_left: AutoPx, margin_right: AutoPx, right: CssPixels| {
            containing_block_inline_size
                - auto_px_value(margin_left)
                - border_left
                - padding_left
                - auto_px_value(inline_size)
                - padding_right
                - border_right
                - auto_px_value(margin_right)
                - right
        };
        let solve_for_inline_size = |left: CssPixels, margin_left: AutoPx, margin_right: AutoPx, right: CssPixels| {
            (containing_block_inline_size
                - left
                - auto_px_value(margin_left)
                - border_left
                - padding_left
                - padding_right
                - border_right
                - auto_px_value(margin_right)
                - right)
                .max(CssPixels::default())
        };
        let solve_for_right = |left: CssPixels, inline_size: AutoPx, margin_left: AutoPx, margin_right: AutoPx| {
            containing_block_inline_size
                - left
                - auto_px_value(margin_left)
                - border_left
                - padding_left
                - auto_px_value(inline_size)
                - padding_right
                - border_right
                - auto_px_value(margin_right)
        };
        let shrink_to_fit = |left: CssPixels, margin_left: AutoPx, margin_right: AutoPx, right: CssPixels| {
            let available = solve_for_inline_size(left, margin_left, margin_right, right);
            let sizing = self.sizing();
            let preferred = sizing.calculate_max_content_inline_size(node, constraints);
            if preferred <= available {
                preferred
            } else {
                let preferred_minimum = sizing.calculate_min_content_inline_size(node, constraints);
                preferred_minimum.max(available).min(preferred)
            }
        };

        if computed_left.is_auto() && inline_size.is_none() && computed_right.is_auto() {
            if margin_left.is_none() {
                margin_left = Some(CssPixels::default());
            }
            if margin_right.is_none() {
                margin_right = Some(CssPixels::default());
            }
            let content_inline_size = shrink_to_fit(left, margin_left, margin_right, right);
            inline_size = Some(content_inline_size);
            self.used_mut(node).set_content_inline_size(content_inline_size);
            left = self.static_offset(node, static_position_rect).inline_offset;
            right = solve_for_right(left, inline_size, margin_left, margin_right);
        }

        if !computed_left.is_auto() && inline_size.is_some() && !computed_right.is_auto() {
            let available_for_margins = containing_block_inline_size
                - border_left
                - padding_left
                - auto_px_value(inline_size)
                - padding_right
                - border_right
                - left
                - right;
            if margin_left.is_none() && margin_right.is_none() {
                margin_left = Some(available_for_margins / 2);
                margin_right = Some(available_for_margins / 2);
                return (inline_size, left, right, margin_left, margin_right);
            }
            if margin_left.is_none() {
                margin_left = Some(available_for_margins);
                return (inline_size, left, right, margin_left, margin_right);
            }
            if margin_right.is_none() {
                margin_right = Some(available_for_margins);
                return (inline_size, left, right, margin_left, margin_right);
            }
            right = solve_for_right(left, inline_size, margin_left, margin_right);
            return (inline_size, left, right, margin_left, margin_right);
        }

        if margin_left.is_none() {
            margin_left = Some(CssPixels::default());
        }
        if margin_right.is_none() {
            margin_right = Some(CssPixels::default());
        }

        if computed_left.is_auto() && inline_size.is_none() && !computed_right.is_auto() {
            inline_size = Some(shrink_to_fit(left, margin_left, margin_right, right));
            left = solve_for_left(inline_size, margin_left, margin_right, right);
        } else if computed_left.is_auto() && computed_right.is_auto() && inline_size.is_some() {
            left = self.static_offset(node, static_position_rect).inline_offset;
            right = solve_for_right(left, inline_size, margin_left, margin_right);
        } else if inline_size.is_none() && computed_right.is_auto() && !computed_left.is_auto() {
            inline_size = Some(shrink_to_fit(left, margin_left, margin_right, right));
            right = solve_for_right(left, inline_size, margin_left, margin_right);
        } else if computed_left.is_auto() && inline_size.is_some() && !computed_right.is_auto() {
            left = solve_for_left(inline_size, margin_left, margin_right, right);
        } else if inline_size.is_none() && !computed_left.is_auto() && !computed_right.is_auto() {
            inline_size = Some(solve_for_inline_size(left, margin_left, margin_right, right));
        } else if computed_right.is_auto() && !computed_left.is_auto() && inline_size.is_some() {
            right = solve_for_right(left, inline_size, margin_left, margin_right);
        }

        (inline_size, left, right, margin_left, margin_right)
    }

    fn compute_inline_size_for_non_replaced(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
    ) {
        let containing_block_inline_size = available_space.inline_size.to_px_or_zero();
        let style = self.style(node);
        let sizing = self.sizing();
        let initial = if self.facts(node).is_table_wrapper {
            // SAFETY: The callback is synchronous and the node is a live
            // table wrapper.
            Some(unsafe {
                (self.callbacks.compute_table_box_inline_size_inside_wrapper)(
                    self.callbacks.context,
                    node,
                    available_space,
                    constraints,
                    false,
                    CssPixels::default(),
                    0,
                )
            })
        } else if style.width.is_auto() {
            None
        } else {
            Some(sizing.calculate_inner_inline_size(node, available_space.inline_size, style.width, constraints))
        };
        let (mut used_inline_size, mut left, mut right, mut margin_left, mut margin_right) = self
            .solve_non_replaced_inline_once(
                node,
                containing_block_inline_size,
                available_space,
                constraints,
                static_position_rect,
                initial,
            );

        if !sizing.should_treat_max_inline_size_as_none(node, available_space.inline_size, constraints) {
            let max_inline_size =
                sizing.calculate_inner_inline_size(node, available_space.inline_size, style.max_width, constraints);
            if auto_px_value(used_inline_size) > max_inline_size {
                (used_inline_size, left, right, margin_left, margin_right) = self.solve_non_replaced_inline_once(
                    node,
                    containing_block_inline_size,
                    available_space,
                    constraints,
                    static_position_rect,
                    Some(max_inline_size),
                );
            }
        }
        if !style.min_width.is_auto() {
            let min_inline_size =
                sizing.calculate_inner_inline_size(node, available_space.inline_size, style.min_width, constraints);
            if auto_px_value(used_inline_size) < min_inline_size {
                (used_inline_size, left, right, margin_left, margin_right) = self.solve_non_replaced_inline_once(
                    node,
                    containing_block_inline_size,
                    available_space,
                    constraints,
                    static_position_rect,
                    Some(min_inline_size),
                );
            }
        }

        let used = self.used_mut(node);
        used.set_content_inline_size(auto_px_value(used_inline_size));
        used.inset_left = left;
        used.inset_right = right;
        used.margin_left = auto_px_value(margin_left);
        used.margin_right = auto_px_value(margin_right);
    }

    fn compute_inline_size_for_replaced(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
    ) {
        let sizing = self.sizing();
        let inline_size = sizing.compute_inline_size_for_replaced_element(node, available_space, constraints);
        let containing_block_inline_size = available_space.inline_size.to_px_or_zero();
        let style = self.style(node);
        let used = self.used(node);
        let available = containing_block_inline_size
            - inline_size
            - style.border_left_width
            - used.padding_left
            - used.padding_right
            - style.border_right_width;
        let solution = solve_replaced_axis(
            available,
            resolve_or_auto(style.inset_left, containing_block_inline_size),
            resolve_or_auto(style.inset_right, containing_block_inline_size),
            resolve_or_auto(style.margin_left, containing_block_inline_size),
            resolve_or_auto(style.margin_right, containing_block_inline_size),
            self.static_offset(node, static_position_rect).inline_offset,
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: true,
                clear_negative_auto_margins: true,
            },
        );

        let used = self.used_mut(node);
        used.inset_left = solution.start;
        used.inset_right = solution.end;
        used.margin_left = solution.margin_start;
        used.margin_right = solution.margin_end;
        used.set_content_inline_size(inline_size);
    }

    fn compute_inline_size(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
    ) {
        if self
            .sizing()
            .box_is_sized_as_replaced_element(node, available_space, constraints)
        {
            self.compute_inline_size_for_replaced(node, available_space, constraints, static_position_rect);
        } else {
            self.compute_inline_size_for_non_replaced(node, available_space, constraints, static_position_rect);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockSizePass {
    BeforeInsideLayout,
    AfterInsideLayout,
}

impl AbsposEngine {
    fn apply_min_max_block_size_constraints(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        unconstrained: AutoPx,
    ) -> AutoPx {
        let style = self.style(node);
        let sizing = self.sizing();
        let mut constrained = unconstrained;
        if !style.max_height.is_none() {
            let maximum = sizing.calculate_inner_block_size(node, available_space, style.max_height, constraints);
            if maximum < auto_px_value(constrained) {
                constrained = Some(maximum);
            }
        }
        if !style.min_height.is_auto() {
            let minimum = sizing.calculate_inner_block_size(node, available_space, style.min_height, constraints);
            if minimum > auto_px_value(constrained) {
                constrained = Some(minimum);
            }
        }
        constrained
    }

    fn automatic_block_size(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        pass: BlockSizePass,
    ) -> AutoPx {
        if self.facts(node).creates_block_formatting_context {
            if pass == BlockSizePass::BeforeInsideLayout {
                return None;
            }
            bump(FfiOp::AbsposAutomaticBlockSizeCallback);
            // SAFETY: The callback reads the live C++ line/rare data that has
            // no Rust representation yet.
            return Some(unsafe {
                (self.callbacks.automatic_block_size_for_abspos_bfc_root)(self.callbacks.context, node)
            });
        }
        let inner = self
            .used(node)
            .available_inner_space_or_constraints_from(available_space);
        Some(
            self.sizing()
                .calculate_fit_content_size(node, FfiFlexAxis::Block, inner, constraints),
        )
    }

    fn solve_non_replaced_block_once(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
        pass: BlockSizePass,
        mut block_size: AutoPx,
    ) -> (AutoPx, AutoPx, AutoPx, AutoPx, AutoPx) {
        let style = self.style(node);
        let containing_block_inline_size = available_space.inline_size.to_px_or_zero();
        let containing_block_block_size = available_space.block_size.to_px_or_zero();
        let mut margin_top = resolve_or_auto(style.margin_top, containing_block_inline_size);
        let mut margin_bottom = resolve_or_auto(style.margin_bottom, containing_block_inline_size);
        let mut top = resolve_or_auto(style.inset_top, containing_block_block_size);
        let mut bottom = resolve_or_auto(style.inset_bottom, containing_block_block_size);
        let used = self.used(node);
        let padding_top = used.padding_top;
        let padding_bottom = used.padding_bottom;

        let solve_for = |length: AutoPx,
                         clamp_to_zero: bool,
                         top: AutoPx,
                         margin_top: AutoPx,
                         block_size: AutoPx,
                         margin_bottom: AutoPx,
                         bottom: AutoPx| {
            solve_abspos_axis_for(
                containing_block_block_size,
                length,
                clamp_to_zero,
                top,
                margin_top,
                style.border_top_width,
                padding_top,
                block_size,
                padding_bottom,
                style.border_bottom_width,
                margin_bottom,
                bottom,
            )
        };

        if top.is_none() && block_size.is_none() && bottom.is_none() {
            if margin_top.is_none() {
                margin_top = Some(CssPixels::default());
            }
            if margin_bottom.is_none() {
                margin_bottom = Some(CssPixels::default());
            }
            let Some(automatic) = self.automatic_block_size(node, available_space, constraints, pass) else {
                return (block_size, top, bottom, margin_top, margin_bottom);
            };
            block_size = Some(automatic);
            let constrained = self.apply_min_max_block_size_constraints(node, available_space, constraints, block_size);
            self.used_mut(node).set_content_block_size(auto_px_value(constrained));
            top = Some(self.static_offset(node, static_position_rect).block_offset);
            bottom = Some(solve_for(
                bottom,
                false,
                top,
                margin_top,
                block_size,
                margin_bottom,
                bottom,
            ));
        } else if top.is_some() && block_size.is_some() && bottom.is_some() {
            if margin_top.is_none() && margin_bottom.is_none() {
                let remainder = solve_for(
                    Some(auto_px_value(margin_top) + auto_px_value(margin_bottom)),
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                );
                margin_top = Some(remainder / 2);
                margin_bottom = Some(remainder / 2);
            } else if margin_top.is_none() || margin_bottom.is_none() {
                if margin_top.is_none() {
                    margin_top = Some(solve_for(
                        margin_top,
                        false,
                        top,
                        margin_top,
                        block_size,
                        margin_bottom,
                        bottom,
                    ));
                } else {
                    margin_bottom = Some(solve_for(
                        margin_bottom,
                        false,
                        top,
                        margin_top,
                        block_size,
                        margin_bottom,
                        bottom,
                    ));
                }
            } else {
                bottom = Some(solve_for(
                    bottom,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            }
        } else {
            if margin_top.is_none() {
                margin_top = Some(CssPixels::default());
            }
            if margin_bottom.is_none() {
                margin_bottom = Some(CssPixels::default());
            }

            if top.is_none() && block_size.is_none() && bottom.is_some() {
                let Some(automatic) = self.automatic_block_size(node, available_space, constraints, pass) else {
                    return (block_size, top, bottom, margin_top, margin_bottom);
                };
                block_size = Some(automatic);
                top = Some(solve_for(
                    top,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            } else if top.is_none() && bottom.is_none() && block_size.is_some() {
                top = Some(self.static_offset(node, static_position_rect).block_offset);
                bottom = Some(solve_for(
                    bottom,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            } else if block_size.is_none() && bottom.is_none() && top.is_some() {
                let Some(automatic) = self.automatic_block_size(node, available_space, constraints, pass) else {
                    return (block_size, top, bottom, margin_top, margin_bottom);
                };
                block_size = Some(automatic);
                bottom = Some(solve_for(
                    bottom,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            } else if top.is_none() && block_size.is_some() && bottom.is_some() {
                top = Some(solve_for(
                    top,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            } else if block_size.is_none() && top.is_some() && bottom.is_some() {
                block_size = Some(solve_for(
                    block_size,
                    true,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            } else if bottom.is_none() && top.is_some() && block_size.is_some() {
                bottom = Some(solve_for(
                    bottom,
                    false,
                    top,
                    margin_top,
                    block_size,
                    margin_bottom,
                    bottom,
                ));
            }
        }
        (block_size, top, bottom, margin_top, margin_bottom)
    }

    fn compute_block_size_for_non_replaced(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
        pass: BlockSizePass,
    ) {
        let style = self.style(node);
        let mut intrinsic_available_space = available_space;
        intrinsic_available_space.inline_size = AvailableSize::definite(self.used(node).content_inline_size);
        let initial = if self.facts(node).is_table_wrapper {
            // SAFETY: The callback synchronously measures this live wrapper.
            Some(unsafe {
                (self.callbacks.compute_table_box_block_size_inside_wrapper)(
                    self.callbacks.context,
                    node,
                    available_space,
                    constraints,
                )
            })
        } else if self
            .sizing()
            .should_treat_block_size_as_auto(node, available_space, constraints)
        {
            None
        } else {
            Some(
                self.sizing()
                    .calculate_inner_block_size(node, intrinsic_available_space, style.height, constraints),
            )
        };
        let (mut used_block_size, mut top, mut bottom, mut margin_top, mut margin_bottom) =
            self.solve_non_replaced_block_once(node, available_space, constraints, static_position_rect, pass, initial);

        if used_block_size.is_some() && !style.max_height.is_none() {
            let max_block_size = self.sizing().calculate_inner_block_size(
                node,
                intrinsic_available_space,
                style.max_height,
                constraints,
            );
            if auto_px_value(used_block_size) > max_block_size {
                (used_block_size, top, bottom, margin_top, margin_bottom) = self.solve_non_replaced_block_once(
                    node,
                    available_space,
                    constraints,
                    static_position_rect,
                    pass,
                    Some(max_block_size),
                );
            }
        }
        if used_block_size.is_some() && !style.min_height.is_auto() {
            let min_block_size = self.sizing().calculate_inner_block_size(
                node,
                intrinsic_available_space,
                style.min_height,
                constraints,
            );
            if auto_px_value(used_block_size) < min_block_size {
                (used_block_size, top, bottom, margin_top, margin_bottom) = self.solve_non_replaced_block_once(
                    node,
                    available_space,
                    constraints,
                    static_position_rect,
                    pass,
                    Some(min_block_size),
                );
            }
        }
        if used_block_size.is_none() {
            used_block_size =
                self.apply_min_max_block_size_constraints(node, available_space, constraints, used_block_size);
        }

        let containing_block_inline_size = available_space.inline_size.to_px_or_zero();
        let containing_block_block_size = available_space.block_size.to_px_or_zero();
        let used = self.used_mut(node);
        used.set_content_block_size(auto_px_value(used_block_size));
        if style.height.is_auto() && pass == BlockSizePass::BeforeInsideLayout {
            return;
        }
        if !style.height.is_intrinsic_sizing_constraint() {
            used.has_definite_block_size = true;
        }
        used.inset_top = auto_px_value(top);
        used.inset_bottom = auto_px_value(bottom);
        // The local values are already resolved against these bases. Keep the
        // variables to document and pin the C++ basis distinction.
        let _ = (containing_block_inline_size, containing_block_block_size);
        used.margin_top = auto_px_value(margin_top);
        used.margin_bottom = auto_px_value(margin_bottom);
    }

    fn compute_block_size_for_replaced(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
        pass: BlockSizePass,
    ) {
        let block_size = self
            .sizing()
            .compute_block_size_for_replaced_element(node, available_space, constraints);
        let containing_block_block_size = available_space.block_size.to_px_or_zero();
        let style = self.style(node);
        let used = self.used(node);
        let available = containing_block_block_size
            - block_size
            - style.border_top_width
            - used.padding_top
            - used.padding_bottom
            - style.border_bottom_width;
        // Deliberately pass false for `clear_auto_margins_if_start_is_auto`:
        // this matches the C++ condition, which tests only the end inset.
        let solution = solve_replaced_axis(
            available,
            resolve_or_auto(style.inset_top, containing_block_block_size),
            resolve_or_auto(style.inset_bottom, containing_block_block_size),
            resolve_or_auto(style.margin_top, containing_block_block_size),
            resolve_or_auto(style.margin_bottom, containing_block_block_size),
            self.static_offset(node, static_position_rect).block_offset,
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: false,
                clear_negative_auto_margins: false,
            },
        );

        let used = self.used_mut(node);
        used.set_content_block_size(block_size);
        if style.height.is_auto() && pass == BlockSizePass::BeforeInsideLayout {
            return;
        }
        if !style.height.is_intrinsic_sizing_constraint() {
            used.has_definite_block_size = true;
        }
        used.inset_top = solution.start;
        used.inset_bottom = solution.end;
        used.margin_top = solution.margin_start;
        used.margin_bottom = solution.margin_end;
    }

    fn compute_block_size(
        &self,
        node: Node,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
        static_position_rect: FfiStaticPositionRect,
        pass: BlockSizePass,
    ) {
        if self
            .sizing()
            .box_is_sized_as_replaced_element(node, available_space, constraints)
        {
            self.compute_block_size_for_replaced(node, available_space, constraints, static_position_rect, pass);
        } else {
            self.compute_block_size_for_non_replaced(node, available_space, constraints, static_position_rect, pass);
        }
    }
}

impl AbsposEngine {
    fn layout_element(&self, node: Node, inputs: FfiAbsposLayoutInputs) {
        assert!(!self.facts(node).is_svg_box);
        let containing_block_size = LogicalSize {
            inline_size: clamp_to_max_dimension_value(inputs.containing_block_info.rect.size.inline_size),
            block_size: clamp_to_max_dimension_value(inputs.containing_block_info.rect.size.block_size),
        };
        let available_space = AvailableSpace {
            inline_size: AvailableSize::definite(containing_block_size.inline_size),
            block_size: AvailableSize::definite(containing_block_size.block_size),
        };
        let constraints = FfiContainingBlockConstraints {
            has_percentage_basis_inline_size: true,
            percentage_basis_inline_size: containing_block_size.inline_size,
            has_percentage_basis_block_size: true,
            percentage_basis_block_size: containing_block_size.block_size,
            has_quirks_mode_percentage_basis_block_size: false,
            quirks_mode_percentage_basis_block_size: CssPixels::default(),
        };
        let style = self.style(node);
        {
            let used = self.used_mut(node);
            used.border_left = style.border_left_width;
            used.border_right = style.border_right_width;
            used.border_top = style.border_top_width;
            used.border_bottom = style.border_bottom_width;
            used.padding_left = style.padding_left.to_px(containing_block_size.inline_size);
            used.padding_right = style.padding_right.to_px(containing_block_size.inline_size);
            used.padding_top = style.padding_top.to_px(containing_block_size.inline_size);
            used.padding_bottom = style.padding_bottom.to_px(containing_block_size.inline_size);
        }

        self.compute_inline_size(node, available_space, constraints, inputs.static_position_rect);
        self.compute_block_size(
            node,
            available_space,
            constraints,
            inputs.static_position_rect,
            BlockSizePass::BeforeInsideLayout,
        );

        {
            let used = self.used_mut(node);
            if !style.inset_left.is_auto() && !style.inset_right.is_auto() {
                used.has_definite_inline_size = true;
            }
            if !style.inset_top.is_auto()
                && !style.inset_bottom.is_auto()
                && (style.height.is_auto() || !style.height.is_intrinsic_sizing_constraint())
            {
                used.has_definite_block_size = true;
            }
        }
        if !self.facts(node).creates_block_formatting_context {
            let block_size_resolved_from_aspect_ratio = style.height.is_auto()
                && self.facts(node).has_preferred_aspect_ratio
                && self.used(node).has_definite_inline_size();
            let used = self.used_mut(node);
            used.has_definite_inline_size = true;
            if (!style.height.is_auto() && !style.height.is_intrinsic_sizing_constraint())
                || block_size_resolved_from_aspect_ratio
            {
                used.has_definite_block_size = true;
            }
        }

        bump(FfiOp::AbsposButtonDefiniteCallback);
        // SAFETY: This callback retains the still-C++ button-specific
        // measurement helper and mutates only this live used-values entry.
        unsafe {
            (self.callbacks.make_button_content_box_definite)(
                self.callbacks.context,
                node,
                available_space,
                constraints,
            );
        }

        let inner_available_space = self
            .used(node)
            .available_inner_space_or_constraints_from(available_space);
        let mut child_result = FfiChildLayoutResult {
            automatic_content_inline_size: CssPixels::default(),
            automatic_content_block_size: CssPixels::default(),
        };
        bump(FfiOp::LayoutInsideCallback);
        // SAFETY: The callback synchronously creates and runs a child
        // formatting context and retains it in the bridge until the matching
        // parent-dimension callback below.
        let has_independent_context = unsafe {
            (self.callbacks.layout_inside_child)(
                self.callbacks.context,
                node,
                LAYOUT_MODE_NORMAL,
                FfiLayoutInput {
                    available_space: inner_available_space,
                    containing_block_constraints: constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
                &raw mut child_result,
            )
        };

        if style.height.is_auto() {
            self.compute_block_size(
                node,
                available_space,
                constraints,
                inputs.static_position_rect,
                BlockSizePass::AfterInsideLayout,
            );
        }

        {
            let used = self.used_mut(node);
            let collapsed = used.uses_collapsing_borders_model;
            if inputs.containing_block_info.has_inline_alignment
                && style.inset_left.is_auto()
                && style.inset_right.is_auto()
            {
                let available = containing_block_size.inline_size - used.margin_box_inline_size(collapsed);
                match inputs.containing_block_info.inline_alignment {
                    FfiAbsposAlignment::Center => {
                        used.inset_left = available / 2;
                        used.inset_right = available / 2;
                    }
                    FfiAbsposAlignment::Start => {
                        used.inset_right = available;
                    }
                    FfiAbsposAlignment::End => {
                        used.inset_left = available;
                    }
                    _ => {}
                }
            }
            if inputs.containing_block_info.has_block_alignment
                && style.inset_top.is_auto()
                && style.inset_bottom.is_auto()
            {
                let available = containing_block_size.block_size - used.margin_box_block_size(collapsed);
                match inputs.containing_block_info.block_alignment {
                    FfiAbsposAlignment::Center => {
                        used.inset_top = available / 2;
                        used.inset_bottom = available / 2;
                    }
                    FfiAbsposAlignment::Start | FfiAbsposAlignment::SelfStart => {
                        used.inset_bottom = available;
                    }
                    FfiAbsposAlignment::End | FfiAbsposAlignment::SelfEnd => {
                        used.inset_top = available;
                    }
                    _ => {}
                }
            }
        }

        let static_offset = self.static_offset(node, inputs.static_position_rect);
        let used = self.used(node);
        let collapsed = used.uses_collapsing_borders_model;
        let mut used_offset = LogicalOffset {
            inline_offset: if inputs.containing_block_info.inline_axis_mode == FfiAbsposAxisMode::StaticPosition {
                static_offset.inline_offset
            } else {
                inputs.containing_block_info.rect.offset.inline_offset + used.inset_left
            },
            block_offset: if inputs.containing_block_info.block_axis_mode == FfiAbsposAxisMode::StaticPosition {
                static_offset.block_offset
            } else {
                inputs.containing_block_info.rect.offset.block_offset + used.inset_top
            },
        };
        used_offset.inline_offset += used.margin_left + used.border_box_left(collapsed);
        used_offset.block_offset += used.margin_top + used.border_box_top(collapsed);
        bump(FfiOp::PlaceChildCallback);
        // SAFETY: Placement is synchronous and the bridge owns the live
        // LayoutState wrapper.
        unsafe {
            (self.callbacks.place_child)(
                self.callbacks.context,
                node,
                FfiCssPixelPoint {
                    x: used_offset.inline_offset,
                    y: used_offset.block_offset,
                },
            );
        }

        // SAFETY: The callback only reads the LayoutState purpose.
        let is_measurement = unsafe { (self.callbacks.is_measurement_state)(self.callbacks.context) };
        if self.layout_mode == LAYOUT_MODE_NORMAL && !is_measurement {
            bump(FfiOp::AbsposSavedInputsSetCallback);
            // SAFETY: The callback copies the POD inputs into the C++
            // extension owned by this state entry.
            unsafe {
                (self.callbacks.set_abspos_layout_inputs)(self.callbacks.context, node, inputs);
            }
        }

        if has_independent_context {
            bump(FfiOp::ParentDidDimensionCallback);
            // SAFETY: This consumes the child context retained by the
            // matching layout_inside_child call.
            unsafe {
                (self.callbacks.parent_did_dimension_child_root_box)(self.callbacks.context, node);
            }
        }
    }

    pub(crate) fn layout_children(&self, box_: Node) {
        bump(FfiOp::AbsposEngine);
        if self.layout_mode != LAYOUT_MODE_NORMAL {
            return;
        }
        // SAFETY: The callback only reads the LayoutState purpose.
        if unsafe { (self.callbacks.is_measurement_state)(self.callbacks.context) } {
            return;
        }
        loop {
            bump(FfiOp::AbsposTake);
            let Some(child) = state_mut(self.state).take_next_contained_abspos_child(box_) else {
                break;
            };
            let child_box = child.child_box;
            if self.try_used_pointer(child_box).is_null() {
                bump(FfiOp::UsedValuesCreateCallback);
                // SAFETY: The callback creates one stable entry for this live
                // box in the shared state.
                let created = unsafe {
                    (self.callbacks.create_used_values)(
                        self.callbacks.context,
                        child_box,
                        false,
                        CssPixels::default(),
                        false,
                        CssPixels::default(),
                    )
                };
                assert!(!created.is_null());
            }
            self.resolve_anchor_insets(child_box);
            let inputs = FfiAbsposLayoutInputs {
                static_position_rect: self
                    .resolve_static_position_relative_to_containing_block(child_box, child.static_position_rect),
                containing_block_info: self.containing_block_info(child_box),
            };
            self.layout_element(child_box, inputs);
        }
    }

    fn replay(&self, node: Node) {
        bump(FfiOp::AbsposReplay);
        let mut inputs = FfiAbsposLayoutInputs {
            static_position_rect: FfiStaticPositionRect {
                rect: LogicalRect::default(),
                inline_alignment: FfiStaticPositionAlignment::Start,
                block_alignment: FfiStaticPositionAlignment::Start,
                alignment_derives_from_own_computed_values: false,
            },
            containing_block_info: FfiAbsposContainingBlockInfo {
                rect: LogicalRect::default(),
                inline_axis_mode: FfiAbsposAxisMode::StaticPosition,
                block_axis_mode: FfiAbsposAxisMode::StaticPosition,
                has_inline_alignment: false,
                inline_alignment: FfiAbsposAlignment::Normal,
                has_block_alignment: false,
                block_alignment: FfiAbsposAlignment::Normal,
                derives_from_own_computed_values: false,
            },
        };
        bump(FfiOp::AbsposSavedInputsGetCallback);
        // SAFETY: The callback copies from the box-owned saved input slot.
        let found =
            unsafe { (self.callbacks.get_saved_abspos_layout_inputs)(self.callbacks.context, node, &raw mut inputs) };
        assert!(found);
        if !inputs.containing_block_info.derives_from_own_computed_values {
            let (inline, block) = axis_modes(self.style(node));
            inputs.containing_block_info.inline_axis_mode = inline;
            inputs.containing_block_info.block_axis_mode = block;
        }
        bump(FfiOp::UsedValuesCreateCallback);
        // SAFETY: Partial relayout uses a fresh state and creates the replay
        // root exactly once.
        let created = unsafe {
            (self.callbacks.create_used_values)(
                self.callbacks.context,
                node,
                false,
                CssPixels::default(),
                false,
                CssPixels::default(),
            )
        };
        assert!(!created.is_null());
        self.layout_element(node, inputs);
    }

    fn compute_inset(&self, node: Node, containing_block_size: LogicalSize) {
        // Most boxes are neither relatively positioned nor carry anchor()
        // insets. Preserve the old C++ fast path without populating the
        // comprehensive Rust facts caches for those boxes.
        // SAFETY: The callback only reads the live node's computed values.
        if !unsafe { (self.callbacks.needs_inset_resolution)(self.callbacks.context, node) } {
            return;
        }
        let initial_style = self.style(node);
        if initial_style.inset_top.contains_anchor_function
            || initial_style.inset_right.contains_anchor_function
            || initial_style.inset_bottom.contains_anchor_function
            || initial_style.inset_left.contains_anchor_function
        {
            self.resolve_anchor_insets(node);
        }
        let style = self.style(node);
        if style.position != POSITION_RELATIVE {
            return;
        }

        let resolve_opposing = |first: FfiSizeValue, second: FfiSizeValue, basis: CssPixels| {
            let resolved_first = first.to_px(basis);
            let resolved_second = second.to_px(basis);
            if first.is_auto() && second.is_auto() {
                (CssPixels::default(), CssPixels::default())
            } else if first.is_auto() {
                (-resolved_second, resolved_second)
            } else {
                (resolved_first, -resolved_first)
            }
        };
        let (left, right) = resolve_opposing(style.inset_left, style.inset_right, containing_block_size.inline_size);

        let treat_percentage_as_auto = |value: FfiSizeValue| {
            if !value.contains_percentage {
                return value;
            }
            let mut containing_block = self.containing_block(node);
            while !containing_block.is_null() {
                let facts = self.facts(containing_block);
                if !facts.is_anonymous || facts.is_table_cell {
                    break;
                }
                containing_block = self.containing_block(containing_block);
            }
            if !containing_block.is_null() && !self.used(containing_block).has_definite_block_size() {
                FfiSizeValue::auto_value()
            } else {
                value
            }
        };
        let (top, bottom) = resolve_opposing(
            treat_percentage_as_auto(style.inset_top),
            treat_percentage_as_auto(style.inset_bottom),
            containing_block_size.block_size,
        );
        let used = self.used_mut(node);
        used.inset_left = left;
        used.inset_right = right;
        used.inset_top = top;
        used.inset_bottom = bottom;
    }
}

pub(super) fn layout_children_for_instance(instance: &mut super::FormattingContextInstance, box_: Node) {
    if instance.layout_mode != LAYOUT_MODE_NORMAL {
        return;
    }
    let grid_context = instance
        .grid_context
        .as_deref()
        .map_or(std::ptr::null(), |grid| grid as *const GridFormattingContext);
    AbsposEngine::new(
        instance.state,
        instance.callbacks,
        instance.layout_mode,
        instance.box_,
        grid_context,
    )
    .layout_children(box_);
}

pub(crate) fn layout_children_native(
    state: *mut c_void,
    callbacks: FfiLayoutFcCallbacks,
    layout_mode: u8,
    context_box: Node,
    box_: Node,
) {
    AbsposEngine::new(state, callbacks, layout_mode, context_box, std::ptr::null()).layout_children(box_);
}

pub(crate) fn compute_inset_native(
    state: *mut c_void,
    callbacks: FfiLayoutFcCallbacks,
    layout_mode: u8,
    context_box: Node,
    node: Node,
    inline_size: CssPixels,
    block_size: CssPixels,
) {
    AbsposEngine::new(state, callbacks, layout_mode, context_box, std::ptr::null()).compute_inset(
        node,
        LogicalSize {
            inline_size,
            block_size,
        },
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_abspos_layout_children(
    state: *mut c_void,
    context_box: Node,
    layout_mode: u8,
    callbacks: *const FfiLayoutFcCallbacks,
) {
    abort_on_panic(|| {
        assert!(!callbacks.is_null());
        // SAFETY: C++ passes a live table and Rust copies it immediately.
        let callbacks = unsafe { *callbacks };
        AbsposEngine::new(state, callbacks, layout_mode, context_box, std::ptr::null()).layout_children(context_box);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_abspos_compute_inset(
    state: *mut c_void,
    context_box: Node,
    node: Node,
    containing_block_inline_size: CssPixels,
    containing_block_block_size: CssPixels,
    callbacks: *const FfiLayoutFcCallbacks,
) {
    abort_on_panic(|| {
        assert!(!callbacks.is_null());
        // SAFETY: C++ passes a live table and Rust copies it immediately.
        let callbacks = unsafe { *callbacks };
        AbsposEngine::new(state, callbacks, LAYOUT_MODE_NORMAL, context_box, std::ptr::null()).compute_inset(
            node,
            LogicalSize {
                inline_size: containing_block_inline_size,
                block_size: containing_block_block_size,
            },
        );
    });
}

pub(super) fn replay_for_instance(instance: &mut super::FormattingContextInstance, node: Node) {
    assert_eq!(instance.fc_type, FfiFormattingContextType::AbsposReplay as u8);
    AbsposEngine::new(
        instance.state,
        instance.callbacks,
        instance.layout_mode,
        instance.box_,
        std::ptr::null(),
    )
    .replay(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    fn static_rect(
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        inline_alignment: FfiStaticPositionAlignment,
        block_alignment: FfiStaticPositionAlignment,
    ) -> FfiStaticPositionRect {
        FfiStaticPositionRect {
            rect: LogicalRect {
                offset: LogicalOffset {
                    inline_offset: px(x),
                    block_offset: px(y),
                },
                size: LogicalSize {
                    inline_size: px(width),
                    block_size: px(height),
                },
            },
            inline_alignment,
            block_alignment,
            alignment_derives_from_own_computed_values: false,
        }
    }

    #[test]
    fn aligned_static_offsets_cover_center_and_end() {
        let offset = aligned_static_offset(
            static_rect(
                10,
                20,
                100,
                80,
                FfiStaticPositionAlignment::Center,
                FfiStaticPositionAlignment::End,
            ),
            px(20),
            px(10),
        );
        assert_eq!(offset.inline_offset, px(50));
        assert_eq!(offset.block_offset, px(90));
    }

    #[test]
    fn static_position_merge_translation_subtracts_the_actual_chain() {
        let translated = translate_static_position_between_chains(
            static_rect(
                10,
                20,
                1,
                1,
                FfiStaticPositionAlignment::Start,
                FfiStaticPositionAlignment::Start,
            ),
            FfiCssPixelPoint { x: px(40), y: px(50) },
            FfiCssPixelPoint { x: px(15), y: px(12) },
        );
        assert_eq!(translated.rect.offset.inline_offset, px(35));
        assert_eq!(translated.rect.offset.block_offset, px(58));
    }

    #[test]
    fn anchor_rect_uses_border_box_and_containing_block_padding() {
        let anchor = UsedValuesCore {
            content_inline_size: px(30),
            content_block_size: px(20),
            border_left: px(2),
            border_right: px(3),
            border_top: px(4),
            border_bottom: px(5),
            padding_left: px(6),
            padding_right: px(7),
            padding_top: px(8),
            padding_bottom: px(9),
            ..Default::default()
        };
        let containing_block = UsedValuesCore {
            padding_left: px(11),
            padding_top: px(13),
            ..Default::default()
        };

        let rect = anchor_rect_from_geometry(&anchor, &containing_block, FfiCssPixelPoint { x: px(100), y: px(200) });
        assert_eq!(
            rect,
            PhysicalRect {
                x: px(103),
                y: px(201),
                width: px(48),
                height: px(46),
            }
        );
    }

    #[test]
    fn non_replaced_axis_equation_solves_size_and_clamps_negative_size() {
        let size = solve_abspos_axis_for(
            px(200),
            None,
            true,
            Some(px(10)),
            Some(px(5)),
            px(2),
            px(4),
            None,
            px(6),
            px(3),
            Some(px(7)),
            Some(px(11)),
        );
        assert_eq!(size, px(152));

        let clamped = solve_abspos_axis_for(
            px(20),
            None,
            true,
            Some(px(10)),
            Some(px(5)),
            px(2),
            px(4),
            None,
            px(6),
            px(3),
            Some(px(7)),
            Some(px(11)),
        );
        assert_eq!(clamped, px(0));
    }

    #[test]
    fn non_replaced_over_constrained_axis_recomputes_the_end_inset() {
        let end = solve_abspos_axis_for(
            px(200),
            Some(px(30)),
            false,
            Some(px(10)),
            Some(px(5)),
            px(0),
            px(0),
            Some(px(100)),
            px(0),
            px(0),
            Some(px(7)),
            Some(px(30)),
        );
        assert_eq!(end, px(78));
    }

    #[test]
    fn replaced_axis_covers_auto_margins_static_position_and_over_constraint() {
        let inline_behavior = ReplacedAxisBehavior {
            clear_auto_margins_if_start_is_auto: true,
            clear_negative_auto_margins: true,
        };
        let centered = solve_replaced_axis(px(200), Some(px(10)), Some(px(20)), None, None, px(0), inline_behavior);
        assert_eq!(centered.margin_start, px(85));
        assert_eq!(centered.margin_end, px(85));

        let static_position = solve_replaced_axis(px(200), None, None, None, None, px(30), inline_behavior);
        assert_eq!(static_position.start, px(30));
        assert_eq!(static_position.end, px(170));
        assert_eq!(static_position.margin_start, px(0));
        assert_eq!(static_position.margin_end, px(0));

        let over_constrained = solve_replaced_axis(
            px(200),
            Some(px(10)),
            Some(px(20)),
            Some(px(5)),
            Some(px(6)),
            px(0),
            inline_behavior,
        );
        assert_eq!(over_constrained.end, px(179));
    }

    #[test]
    fn replaced_variants_preserve_the_cpp_negative_margin_difference() {
        let inline = solve_replaced_axis(
            px(50),
            Some(px(40)),
            Some(px(20)),
            None,
            None,
            px(0),
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: true,
                clear_negative_auto_margins: true,
            },
        );
        assert_eq!((inline.margin_start, inline.margin_end), (px(0), px(0)));
        assert_eq!(inline.end, px(10));

        let block = solve_replaced_axis(
            px(50),
            Some(px(40)),
            Some(px(20)),
            None,
            None,
            px(0),
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: false,
                clear_negative_auto_margins: false,
            },
        );
        assert_eq!((block.margin_start, block.margin_end), (px(-5), px(-5)));
        assert_eq!(block.end, px(20));
    }
}
