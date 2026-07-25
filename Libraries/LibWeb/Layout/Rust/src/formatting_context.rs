/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::box_facts::{FfiLayoutBoxFacts, FfiLayoutNavCallbacks};
use crate::css_pixels::CssPixels;
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::{AvailableSize, AvailableSpace, FfiLayoutInput};
use crate::layout_state::{
    FfiCommitSink, FfiStaticPositionAlignment, FfiStaticPositionRect, LayoutState, LayoutStatePurpose, state_mut,
};
use crate::used_values::{FfiCssPixelPoint, FfiSizeConstraint};
use std::collections::HashMap;
use std::ffi::c_void;

pub(crate) mod abspos {
    /*
     * Copyright (c) 2026-present, the Ladybird developers.
     *
     * SPDX-License-Identifier: BSD-2-Clause
     */

    use super::block;
    use super::grid::GridFormattingContext;
    use super::sizing::{Node, SizingContext};
    use super::{FfiFlexAxis, FfiFormattingContextType, FfiLayoutFcCallbacks};
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
    use crate::style_facts::{FfiSizeValue, StyleValues};
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
    pub(crate) struct PhysicalRect {
        pub(crate) x: CssPixels,
        pub(crate) y: CssPixels,
        pub(crate) width: CssPixels,
        pub(crate) height: CssPixels,
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

    pub(crate) fn translate_static_position_between_chains(
        mut rect: FfiStaticPositionRect,
        static_chain_offset: FfiCssPixelPoint,
        containing_chain_offset: FfiCssPixelPoint,
    ) -> FfiStaticPositionRect {
        let physical_offset = point_sub(static_chain_offset, containing_chain_offset);
        rect.rect.offset.inline_offset += physical_offset.x;
        rect.rect.offset.block_offset += physical_offset.y;
        rect
    }

    pub(crate) fn anchor_rect_from_geometry(
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

    fn axis_modes(style: StyleValues) -> (FfiAbsposAxisMode, FfiAbsposAxisMode) {
        (
            if style.inset_left().is_auto() && style.inset_right().is_auto() {
                FfiAbsposAxisMode::StaticPosition
            } else {
                FfiAbsposAxisMode::InsetFromRect
            },
            if style.inset_top().is_auto() && style.inset_bottom().is_auto() {
                FfiAbsposAxisMode::StaticPosition
            } else {
                FfiAbsposAxisMode::InsetFromRect
            },
        )
    }

    pub(crate) fn aligned_static_offset(
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
        rust_context_handle: *mut c_void,
    }

    impl AbsposEngine {
        fn new(
            state: *mut c_void,
            callbacks: FfiLayoutFcCallbacks,
            layout_mode: u8,
            context_box: Node,
            grid_context: *const GridFormattingContext,
            rust_context_handle: *mut c_void,
        ) -> Self {
            assert!(!state.is_null());
            assert!(!context_box.is_null());
            Self {
                state,
                callbacks,
                layout_mode,
                context_box,
                grid_context,
                rust_context_handle,
            }
        }

        fn sizing(&self) -> SizingContext {
            SizingContext::new(self.state, self.callbacks)
        }

        fn style(&self, node: Node) -> StyleValues {
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
            while merge_point != actual_containing_block && !self.node_is_ancestor(merge_point, actual_containing_block)
            {
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
                    self.add_atomic_inline_fragment_rect(
                        inline_node,
                        fragment,
                        offset,
                        bounding_rect,
                        empty_bounding_rect,
                    );
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

    unsafe extern "C" fn no_absolutized_random_sharing(
        _context: *mut c_void,
        _sharing: *const c_void,
    ) -> *const c_void {
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
            let eligible_anchor_boxes = state_mut(self.state).used_value_nodes();
            bump(FfiOp::AbsposAnchorLookupCallback);
            // SAFETY: The name handle is retained by either the style snapshot or
            // the live anchor() shell. The eligible-node slice and out pointer
            // are borrowed only for this synchronous lookup.
            let found = unsafe {
                (self.callbacks.anchor_lookup)(
                    self.callbacks.context,
                    positioned_box,
                    anchor_name,
                    eligible_anchor_boxes.as_ptr(),
                    eligible_anchor_boxes.len(),
                    &raw mut anchor_box,
                )
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
                (self.callbacks.set_default_scroll_shift)(
                    self.callbacks.context,
                    node,
                    std::ptr::null_mut(),
                    false,
                    false,
                );
            }

            let style = self.style(node);
            let top_contains_anchor = style.inset_top().contains_anchor_function;
            let right_contains_anchor = style.inset_right().contains_anchor_function;
            let bottom_contains_anchor = style.inset_bottom().contains_anchor_function;
            let left_contains_anchor = style.inset_left().contains_anchor_function;
            if !top_contains_anchor && !right_contains_anchor && !bottom_contains_anchor && !left_contains_anchor {
                return;
            }

            let containing_block = self.containing_block(node);
            if containing_block.is_null() {
                return;
            }
            let containing_block_state = self.used(containing_block);
            let default_anchor_box = if style.has_position_anchor() {
                self.anchor_lookup(node, style.position_anchor_name())
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
                    style.inset_top(),
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
                    style.inset_right(),
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
                    style.inset_bottom(),
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
                    style.inset_left(),
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
        } else if style.has_position_anchor() {
            Some(style.position_anchor_name())
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
                rust_calc_node_create_numeric_dimension(
                    CALC_NUMERIC_KIND_LENGTH,
                    fallback.px.to_double(),
                    LENGTH_UNIT_PX,
                )
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
                unsafe {
                    rust_calc_node_create_numeric_dimension(CALC_NUMERIC_KIND_LENGTH, resolved.value, LENGTH_UNIT_PX)
                }
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
    pub(crate) fn solve_abspos_axis_for(
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
    pub(crate) struct ReplacedAxisSolution {
        pub(crate) start: CssPixels,
        pub(crate) end: CssPixels,
        pub(crate) margin_start: CssPixels,
        pub(crate) margin_end: CssPixels,
    }

    #[derive(Clone, Copy)]
    pub(crate) struct ReplacedAxisBehavior {
        pub(crate) clear_auto_margins_if_start_is_auto: bool,
        pub(crate) clear_negative_auto_margins: bool,
    }

    pub(crate) fn solve_replaced_axis(
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
            let computed_left = style.inset_left();
            let computed_right = style.inset_right();
            let mut left = style.inset_left().to_px(containing_block_inline_size);
            let mut right = style.inset_right().to_px(containing_block_inline_size);
            let mut margin_left = resolve_or_auto(style.margin_left(), containing_block_inline_size);
            let mut margin_right = resolve_or_auto(style.margin_right(), containing_block_inline_size);
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
            let solve_for_inline_size =
                |left: CssPixels, margin_left: AutoPx, margin_right: AutoPx, right: CssPixels| {
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
                Some(sizing.compute_table_box_inline_size_inside_wrapper(
                    node,
                    available_space,
                    constraints,
                    None,
                    super::sizing::TableWrapperInlineSizeMode::ClampToAvailableInlineSize,
                ))
            } else if style.width().is_auto() {
                None
            } else {
                Some(sizing.calculate_inner_inline_size(node, available_space.inline_size, style.width(), constraints))
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
                let max_inline_size = sizing.calculate_inner_inline_size(
                    node,
                    available_space.inline_size,
                    style.max_width(),
                    constraints,
                );
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
            if !style.min_width().is_auto() {
                let min_inline_size = sizing.calculate_inner_inline_size(
                    node,
                    available_space.inline_size,
                    style.min_width(),
                    constraints,
                );
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
                resolve_or_auto(style.inset_left(), containing_block_inline_size),
                resolve_or_auto(style.inset_right(), containing_block_inline_size),
                resolve_or_auto(style.margin_left(), containing_block_inline_size),
                resolve_or_auto(style.margin_right(), containing_block_inline_size),
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
            if !style.max_height().is_none() {
                let maximum = sizing.calculate_inner_block_size(node, available_space, style.max_height(), constraints);
                if maximum < auto_px_value(constrained) {
                    constrained = Some(maximum);
                }
            }
            if !style.min_height().is_auto() {
                let minimum = sizing.calculate_inner_block_size(node, available_space, style.min_height(), constraints);
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
                return Some(block::automatic_block_size_for_bfc_root(
                    self.state,
                    self.callbacks,
                    node,
                ));
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
            let mut margin_top = resolve_or_auto(style.margin_top(), containing_block_inline_size);
            let mut margin_bottom = resolve_or_auto(style.margin_bottom(), containing_block_inline_size);
            let mut top = resolve_or_auto(style.inset_top(), containing_block_block_size);
            let mut bottom = resolve_or_auto(style.inset_bottom(), containing_block_block_size);
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
                let constrained =
                    self.apply_min_max_block_size_constraints(node, available_space, constraints, block_size);
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
                Some(
                    self.sizing()
                        .compute_table_box_block_size_inside_wrapper(node, available_space, constraints),
                )
            } else if self
                .sizing()
                .should_treat_block_size_as_auto(node, available_space, constraints)
            {
                None
            } else {
                Some(self.sizing().calculate_inner_block_size(
                    node,
                    intrinsic_available_space,
                    style.height(),
                    constraints,
                ))
            };
            let (mut used_block_size, mut top, mut bottom, mut margin_top, mut margin_bottom) = self
                .solve_non_replaced_block_once(node, available_space, constraints, static_position_rect, pass, initial);

            if used_block_size.is_some() && !style.max_height().is_none() {
                let max_block_size = self.sizing().calculate_inner_block_size(
                    node,
                    intrinsic_available_space,
                    style.max_height(),
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
            if used_block_size.is_some() && !style.min_height().is_auto() {
                let min_block_size = self.sizing().calculate_inner_block_size(
                    node,
                    intrinsic_available_space,
                    style.min_height(),
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
            if style.height().is_auto() && pass == BlockSizePass::BeforeInsideLayout {
                return;
            }
            if !style.height().is_intrinsic_sizing_constraint() {
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
                resolve_or_auto(style.inset_top(), containing_block_block_size),
                resolve_or_auto(style.inset_bottom(), containing_block_block_size),
                resolve_or_auto(style.margin_top(), containing_block_block_size),
                resolve_or_auto(style.margin_bottom(), containing_block_block_size),
                self.static_offset(node, static_position_rect).block_offset,
                ReplacedAxisBehavior {
                    clear_auto_margins_if_start_is_auto: false,
                    clear_negative_auto_margins: false,
                },
            );

            let used = self.used_mut(node);
            used.set_content_block_size(block_size);
            if style.height().is_auto() && pass == BlockSizePass::BeforeInsideLayout {
                return;
            }
            if !style.height().is_intrinsic_sizing_constraint() {
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
                self.compute_block_size_for_non_replaced(
                    node,
                    available_space,
                    constraints,
                    static_position_rect,
                    pass,
                );
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
                used.padding_left = style.padding_left().to_px(containing_block_size.inline_size);
                used.padding_right = style.padding_right().to_px(containing_block_size.inline_size);
                used.padding_top = style.padding_top().to_px(containing_block_size.inline_size);
                used.padding_bottom = style.padding_bottom().to_px(containing_block_size.inline_size);
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
                if !style.inset_left().is_auto() && !style.inset_right().is_auto() {
                    used.has_definite_inline_size = true;
                }
                if !style.inset_top().is_auto()
                    && !style.inset_bottom().is_auto()
                    && (style.height().is_auto() || !style.height().is_intrinsic_sizing_constraint())
                {
                    used.has_definite_block_size = true;
                }
            }
            if !self.facts(node).creates_block_formatting_context {
                let block_size_resolved_from_aspect_ratio = style.height().is_auto()
                    && self.facts(node).has_preferred_aspect_ratio
                    && self.used(node).has_definite_inline_size();
                let used = self.used_mut(node);
                used.has_definite_inline_size = true;
                if (!style.height().is_auto() && !style.height().is_intrinsic_sizing_constraint())
                    || block_size_resolved_from_aspect_ratio
                {
                    used.has_definite_block_size = true;
                }
            }

            self.sizing()
                .make_button_content_box_definite(node, self.layout_mode, available_space, constraints, None);

            let inner_available_space = self
                .used(node)
                .available_inner_space_or_constraints_from(available_space);
            let has_independent_context = super::layout_inside_child(
                self.rust_context_handle,
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
                false,
            )
            .is_some();

            if style.height().is_auto() {
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
                    && style.inset_left().is_auto()
                    && style.inset_right().is_auto()
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
                    && style.inset_top().is_auto()
                    && style.inset_bottom().is_auto()
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
            super::place_child(
                self.state,
                &self.callbacks,
                node,
                FfiCssPixelPoint {
                    x: used_offset.inline_offset,
                    y: used_offset.block_offset,
                },
            );

            let is_measurement = state_mut(self.state).is_measurement();
            if self.layout_mode == LAYOUT_MODE_NORMAL && !is_measurement {
                bump(FfiOp::AbsposSavedInputsSetCallback);
                state_mut(self.state)
                    .used_values_rare_data_for_node_mut(&self.callbacks, node)
                    .abspos_layout_inputs = Some(inputs);
            }

            if has_independent_context {
                super::finish_child_layout(self.rust_context_handle, node);
            }
        }

        pub(crate) fn layout_children(&self, box_: Node) {
            bump(FfiOp::AbsposEngine);
            if self.layout_mode != LAYOUT_MODE_NORMAL {
                return;
            }
            if state_mut(self.state).is_measurement() {
                return;
            }
            loop {
                bump(FfiOp::AbsposTake);
                let Some(child) = state_mut(self.state).take_next_contained_abspos_child(box_) else {
                    break;
                };
                let child_box = child.child_box;
                if self.try_used_pointer(child_box).is_null() {
                    let created = state_mut(self.state).create_used_values(
                        &self.callbacks,
                        child_box,
                        FfiContainingBlockConstraints::default(),
                    );
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
            let found = unsafe {
                (self.callbacks.get_saved_abspos_layout_inputs)(self.callbacks.context, node, &raw mut inputs)
            };
            assert!(found);
            if !inputs.containing_block_info.derives_from_own_computed_values {
                let (inline, block) = axis_modes(self.style(node));
                inputs.containing_block_info.inline_axis_mode = inline;
                inputs.containing_block_info.block_axis_mode = block;
            }
            // Partial relayout uses a fresh state and creates the replay root
            // exactly once.
            let created = state_mut(self.state).create_used_values(
                &self.callbacks,
                node,
                FfiContainingBlockConstraints::default(),
            );
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
            if initial_style.inset_top().contains_anchor_function
                || initial_style.inset_right().contains_anchor_function
                || initial_style.inset_bottom().contains_anchor_function
                || initial_style.inset_left().contains_anchor_function
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
            let (left, right) = resolve_opposing(
                style.inset_left(),
                style.inset_right(),
                containing_block_size.inline_size,
            );

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
                treat_percentage_as_auto(style.inset_top()),
                treat_percentage_as_auto(style.inset_bottom()),
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
            instance as *mut super::FormattingContextInstance as *mut c_void,
        )
        .layout_children(box_);
    }

    pub(crate) fn layout_children_native(
        state: *mut c_void,
        callbacks: FfiLayoutFcCallbacks,
        layout_mode: u8,
        context_box: Node,
        rust_context_handle: *mut c_void,
        box_: Node,
    ) {
        AbsposEngine::new(
            state,
            callbacks,
            layout_mode,
            context_box,
            std::ptr::null(),
            rust_context_handle,
        )
        .layout_children(box_);
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
        AbsposEngine::new(
            state,
            callbacks,
            layout_mode,
            context_box,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
        .compute_inset(
            node,
            LogicalSize {
                inline_size,
                block_size,
            },
        );
    }

    pub(super) fn replay_for_instance(instance: &mut super::FormattingContextInstance, node: Node) {
        assert_eq!(instance.fc_type, FfiFormattingContextType::AbsposReplay as u8);
        AbsposEngine::new(
            instance.state,
            instance.callbacks,
            instance.layout_mode,
            instance.box_,
            std::ptr::null(),
            instance as *mut super::FormattingContextInstance as *mut c_void,
        )
        .replay(node);
    }
}

#[path = "block_formatting_context.rs"]
pub(crate) mod block;
#[path = "flex_formatting_context.rs"]
pub(crate) mod flex;
#[path = "grid_formatting_context.rs"]
pub(crate) mod grid;
#[path = "inline_formatting_context.rs"]
pub(crate) mod inline;

pub(crate) mod sizing {
    /*
     * Copyright (c) 2026-present, the Ladybird developers.
     *
     * SPDX-License-Identifier: BSD-2-Clause
     */

    use super::{
        FfiFlexAxis, FfiFlexSizeProperty, FfiIntrinsicSizeCacheKey, FfiIntrinsicSizeCacheKind, FfiLayoutFcCallbacks,
    };
    use crate::box_facts::FfiLayoutBoxFacts;
    use crate::css_pixels::CssPixels;
    use crate::ffi_stats::{FfiOp, bump};
    use crate::geometry::{AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput};
    use crate::layout_state::{LayoutState, LayoutStatePurpose, state_mut};
    use crate::style_facts::{FfiSizeValue, StyleValues};
    use crate::used_values::{FfiSizeConstraint, UsedValuesCore};
    use std::ffi::c_void;

    pub(crate) type Node = *mut c_void;

    const BOX_SIZING_BORDER_BOX: u8 = 0;
    const LAYOUT_MODE_INTRINSIC_SIZING: u8 = 1;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CyclicPercentageIntrinsicContribution {
        NotCyclic,
        ResolveAsZero,
        TreatAsInitialValue,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CyclicPercentageSizeProperty {
        PreferredOrMaxSize,
        MinSize,
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct PixelFraction {
        pub(crate) numerator: CssPixels,
        pub(crate) denominator: CssPixels,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct ReplacedIntrinsicSize {
        width: Option<CssPixels>,
        height: Option<CssPixels>,
        aspect_ratio: Option<PixelFraction>,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct ReplacedMaxContentSizeConstraints {
        definite_size_in_ratio_determining_axis: Option<CssPixels>,
        minimum_inline_size: Option<CssPixels>,
        minimum_block_size: Option<CssPixels>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SizeDimension {
        Inline,
        Block,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum TableWrapperInlineSizeMode {
        ClampToAvailableInlineSize,
        UseTableUsedInlineSizeIfNotAuto,
    }

    pub(crate) struct MeasurementState {
        state: Box<LayoutState>,
        callbacks: FfiLayoutFcCallbacks,
        root: Node,
    }

    impl MeasurementState {
        pub(crate) fn create(
            callbacks: FfiLayoutFcCallbacks,
            node: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> Self {
            let mut state = Box::new(LayoutState::new(LayoutStatePurpose::Measurement));
            let root = state.create_used_values(&callbacks, node, constraints);
            assert!(!root.is_null());
            Self {
                state,
                callbacks,
                root: node,
            }
        }

        pub(crate) fn root_used_mut(&mut self) -> &mut UsedValuesCore {
            let root = self.state.used_values(&self.callbacks, self.root);
            // SAFETY: The measurement state uniquely owns the root entry.
            unsafe { &mut *root }
        }

        fn run(&self, node: Node, input: FfiLayoutInput) -> super::FfiChildLayoutResult {
            self.run_with_layout_mode(node, LAYOUT_MODE_INTRINSIC_SIZING, input)
        }

        pub(crate) fn run_with_layout_mode(
            &self,
            node: Node,
            layout_mode: u8,
            input: FfiLayoutInput,
        ) -> super::FfiChildLayoutResult {
            let rust_state = self.rust_state();
            let fc_type = super::independent_formatting_context_type(rust_state, node, &self.callbacks);
            let mut context = super::create_formatting_context(
                rust_state,
                node,
                std::ptr::null_mut(),
                fc_type as u8,
                layout_mode,
                false,
                self.callbacks,
            );
            super::run_formatting_context(&mut context, input);
            super::FfiChildLayoutResult {
                automatic_content_inline_size: context.automatic_content_inline_size,
                automatic_content_block_size: context.automatic_content_block_size,
            }
        }

        pub(crate) fn rust_state(&self) -> *mut c_void {
            std::ptr::from_ref(&*self.state).cast_mut().cast()
        }

        pub(crate) fn callbacks(&self) -> &FfiLayoutFcCallbacks {
            &self.callbacks
        }

        pub(crate) fn state_mut(&mut self) -> &mut LayoutState {
            &mut self.state
        }
    }

    fn cache_key(
        measured_at_inline_size: Option<CssPixels>,
        constraints: FfiContainingBlockConstraints,
    ) -> FfiIntrinsicSizeCacheKey {
        FfiIntrinsicSizeCacheKey {
            has_measured_at_inline_size: measured_at_inline_size.is_some(),
            measured_at_inline_size: measured_at_inline_size.unwrap_or_default(),
            has_percentage_basis_inline_size: constraints.has_percentage_basis_inline_size,
            percentage_basis_inline_size: constraints.percentage_basis_inline_size,
            has_percentage_basis_block_size: constraints.has_percentage_basis_block_size,
            percentage_basis_block_size: constraints.percentage_basis_block_size,
            has_quirks_mode_percentage_basis_block_size: constraints.has_quirks_mode_percentage_basis_block_size,
            quirks_mode_percentage_basis_block_size: constraints.quirks_mode_percentage_basis_block_size,
        }
    }

    fn clamp_to_max_dimension_value(value: CssPixels) -> CssPixels {
        if matches!(value.raw_value(), i32::MIN | i32::MAX) {
            CssPixels::from_integer(17_895_700)
        } else {
            value
        }
    }

    impl PixelFraction {
        fn multiply(self, value: CssPixels) -> CssPixels {
            if self.denominator == CssPixels::default() {
                return CssPixels::default();
            }
            let wide = value.raw_value() as i64 * self.numerator.raw_value() as i64;
            CssPixels::from_raw(
                (wide / self.denominator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            )
        }

        fn divide(self, value: CssPixels) -> CssPixels {
            if self.numerator == CssPixels::default() {
                return CssPixels::default();
            }
            let wide = value.raw_value() as i64 * self.denominator.raw_value() as i64;
            CssPixels::from_raw(
                (wide / self.numerator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            )
        }
    }

    pub(crate) fn cyclic_percentage_intrinsic_contribution(
        is_replaced_box: bool,
        size_contains_percentage: bool,
        available_size: AvailableSize,
        size_property: CyclicPercentageSizeProperty,
    ) -> CyclicPercentageIntrinsicContribution {
        if !size_contains_percentage {
            return CyclicPercentageIntrinsicContribution::NotCyclic;
        }
        // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
        // For the min size properties, as well as for margins and paddings (and gutters), a cyclic percentage is resolved
        // against zero for determining intrinsic size contributions.
        if size_property == CyclicPercentageSizeProperty::MinSize && available_size.is_intrinsic_sizing_constraint() {
            return CyclicPercentageIntrinsicContribution::ResolveAsZero;
        }
        // If the box is non-replaced, then the entire value of any max size property or preferred size property
        // ('width'/'max-width'/'height'/'max-height') specified as an expression containing a percentage (such as '10%' or
        // 'calc(10px + 0%)') that is cyclic is treated for the purpose of calculating the box's intrinsic size contributions
        // only as that property's initial value.
        if available_size.is_min_content() {
            if is_replaced_box {
                // If the box is replaced, a cyclic percentage in the value of any max size property or preferred size property
                // ('width'/'max-width'/'height'/'max-height'), is resolved against zero when calculating the min-content
                // contribution in the corresponding axis.
                return CyclicPercentageIntrinsicContribution::ResolveAsZero;
            }
            return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
        }
        if available_size.is_max_content() {
            // Likewise, if the box is replaced, then the entire value of any max size property or preferred size property
            // specified as an expression containing a percentage that is cyclic is treated for the purpose of calculating
            // the box's max-content contributions only as that property's initial value.
            return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
        }
        CyclicPercentageIntrinsicContribution::NotCyclic
    }

    pub(crate) fn subtract_border_box_adjustment(
        value: CssPixels,
        before_border: CssPixels,
        before_padding: CssPixels,
        after_border: CssPixels,
        after_padding: CssPixels,
    ) -> CssPixels {
        (value - before_border - before_padding - after_border - after_padding).max(CssPixels::default())
    }

    pub(crate) fn content_block_size_from_aspect_ratio_values(
        content_inline_size: CssPixels,
        ratio: PixelFraction,
        use_border_box: bool,
        inline_before: CssPixels,
        inline_after: CssPixels,
        block_before: CssPixels,
        block_after: CssPixels,
    ) -> CssPixels {
        // NB: Intrinsic grid sizing can transfer an aspect ratio before block-axis border metrics are copied into the layout
        //     state. Border widths are already definite at computed-value time, while padding remains resolved in the state.
        if ratio.numerator == CssPixels::default() {
            return CssPixels::default();
        }
        if use_border_box {
            return (ratio.divide(content_inline_size + inline_before + inline_after) - block_before - block_after)
                .max(CssPixels::default());
        }
        ratio.divide(content_inline_size)
    }

    pub(crate) fn content_inline_size_from_aspect_ratio_values(
        content_block_size: CssPixels,
        ratio: PixelFraction,
        use_border_box: bool,
        inline_before: CssPixels,
        inline_after: CssPixels,
        block_before: CssPixels,
        block_after: CssPixels,
    ) -> CssPixels {
        if ratio.numerator == CssPixels::default() {
            return CssPixels::default();
        }
        if use_border_box {
            return (ratio.multiply(content_block_size + block_before + block_after) - inline_before - inline_after)
                .max(CssPixels::default());
        }
        ratio.multiply(content_block_size)
    }

    pub(crate) struct SizingContext {
        state: *mut c_void,
        callbacks: FfiLayoutFcCallbacks,
    }

    impl SizingContext {
        pub(crate) fn new(state: *mut c_void, callbacks: FfiLayoutFcCallbacks) -> Self {
            Self { state, callbacks }
        }

        fn facts(&self, node: Node) -> FfiLayoutBoxFacts {
            state_mut(self.state).box_facts(&self.callbacks, node)
        }

        fn style(&self, node: Node) -> StyleValues {
            state_mut(self.state).style_facts(&self.callbacks, node)
        }

        fn used(&self, node: Node) -> &UsedValuesCore {
            let used = state_mut(self.state).used_values(&self.callbacks, node);
            // SAFETY: The Rust layout state owns this stable entry.
            unsafe { &*used }
        }

        // The used-values store owns stable, disjoint entries and layout is
        // single-threaded. Mutability is mediated by the opaque state pointer.
        #[allow(clippy::mut_from_ref)]
        fn used_mut(&self, node: Node) -> &mut UsedValuesCore {
            let used = state_mut(self.state).used_values(&self.callbacks, node);
            // SAFETY: Layout is single-threaded and this helper serializes
            // mutations of the selected entry.
            unsafe { &mut *used }
        }

        fn parent(&self, node: Node) -> Node {
            bump(FfiOp::NavigationCallback);
            // SAFETY: Navigation is synchronous and the host owns the node tree.
            unsafe { (self.callbacks.navigation.parent)(self.callbacks.navigation.context, node) }
        }

        fn first_child(&self, node: Node) -> Node {
            bump(FfiOp::NavigationCallback);
            // SAFETY: Navigation is synchronous and the host owns the node tree.
            unsafe { (self.callbacks.navigation.first_child)(self.callbacks.navigation.context, node) }
        }

        fn next_sibling(&self, node: Node) -> Node {
            bump(FfiOp::NavigationCallback);
            // SAFETY: Navigation is synchronous and the host owns the node tree.
            unsafe { (self.callbacks.navigation.next_sibling)(self.callbacks.navigation.context, node) }
        }

        fn has_children(&self, node: Node) -> bool {
            bump(FfiOp::NavigationCallback);
            // SAFETY: Navigation is synchronous and the host owns the node tree.
            !unsafe { (self.callbacks.navigation.first_child)(self.callbacks.navigation.context, node) }.is_null()
        }

        fn preferred_aspect_ratio(&self, node: Node) -> Option<PixelFraction> {
            let facts = self.facts(node);
            facts.has_preferred_aspect_ratio.then_some(PixelFraction {
                numerator: facts.preferred_aspect_ratio_numerator,
                denominator: facts.preferred_aspect_ratio_denominator,
            })
        }

        fn content_block_size_from_aspect_ratio(&self, node: Node, content_inline_size: CssPixels) -> CssPixels {
            let style = self.style(node);
            let used = self.used(node);
            content_block_size_from_aspect_ratio_values(
                content_inline_size,
                self.preferred_aspect_ratio(node).unwrap(),
                style.box_sizing_for_aspect_ratio() == BOX_SIZING_BORDER_BOX,
                style.border_left_width + used.padding_left,
                style.border_right_width + used.padding_right,
                style.border_top_width + used.padding_top,
                style.border_bottom_width + used.padding_bottom,
            )
        }

        fn content_inline_size_from_aspect_ratio(&self, node: Node, content_block_size: CssPixels) -> CssPixels {
            let style = self.style(node);
            let used = self.used(node);
            content_inline_size_from_aspect_ratio_values(
                content_block_size,
                self.preferred_aspect_ratio(node).unwrap(),
                style.box_sizing_for_aspect_ratio() == BOX_SIZING_BORDER_BOX,
                style.border_left_width + used.padding_left,
                style.border_right_width + used.padding_right,
                style.border_top_width + used.padding_top,
                style.border_bottom_width + used.padding_bottom,
            )
        }

        fn auto_content_size(&self, node: Node) -> ReplacedIntrinsicSize {
            let facts = self.facts(node);
            ReplacedIntrinsicSize {
                width: facts.has_auto_content_width.then_some(facts.auto_content_width),
                height: facts.has_auto_content_height.then_some(facts.auto_content_height),
                aspect_ratio: facts.has_auto_content_aspect_ratio.then_some(PixelFraction {
                    numerator: facts.auto_content_aspect_ratio_numerator,
                    denominator: facts.auto_content_aspect_ratio_denominator,
                }),
            }
        }

        fn intrinsic_size_for_replaced_sizing(&self, node: Node) -> ReplacedIntrinsicSize {
            let auto_size = self.auto_content_size(node);
            if auto_size.width.is_some() || auto_size.height.is_some() || auto_size.aspect_ratio.is_some() {
                return auto_size;
            }
            let facts = self.facts(node);
            // https://drafts.csswg.org/css-ui-4/#appearance-switching
            // The element is rendered following the usual rules of CSS. Replaced elements other than widgets are not affected
            // by this and remain replaced elements. Widgets must not have their native appearance, and instead must have their
            // primitive appearance.
            //
            // https://html.spec.whatwg.org/multipage/rendering.html#the-input-element-as-a-text-entry-widget
            // An input element whose type attribute is in one of the above states is an element with default preferred size,
            // and user agents are expected to apply the 'field-sizing' CSS property to the element.
            ReplacedIntrinsicSize {
                width: facts
                    .has_default_preferred_width
                    .then_some(facts.default_preferred_width),
                height: facts
                    .has_default_preferred_height
                    .then_some(facts.default_preferred_height),
                aspect_ratio: None,
            }
        }

        fn max_content_size_for_replaced_element_without_natural_size(
            &self,
            node: Node,
            natural_size: ReplacedIntrinsicSize,
            dimension: SizeDimension,
            constraints: ReplacedMaxContentSizeConstraints,
        ) -> Option<CssPixels> {
            // https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
            // the intrinsic sizes of replaced elements without natural sizes are defined below:
            let facts = self.facts(node);
            let is_inline_axis = dimension == SizeDimension::Inline;
            if !facts.is_replaced_box
                || if is_inline_axis {
                    natural_size.width.is_some()
                } else {
                    natural_size.height.is_some()
                }
            {
                return None;
            }

            // SVG Integration says that a non-top-level <svg> starts with auto width/height, and that with a viewBox, missing
            // width/height attributes "keep" their auto value. The resulting width, height, and aspect ratio are then
            // "used in CSS sizing as intrinsic element size properties".
            //
            // CSS Sizing defines max-content as the size the box would have "if it was a float" with an auto preferred size.
            // CSS2 replaced sizing then resolves auto width from "(used height) * (intrinsic ratio)", and auto height from
            // "(used width) / (intrinsic ratio)". Keep this SVG specific bridge before falling through to CSS Sizing's fallback
            // for replaced elements without natural sizes.
            //  - https://svgwg.org/specs/integration/#svg-css-sizing
            //  - https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
            //  - https://drafts.csswg.org/css2/#inline-replaced-width
            //  - https://drafts.csswg.org/css2/#inline-replaced-height
            if facts.is_svg_svg_box
                && let Some(ratio) = natural_size.aspect_ratio
            {
                if is_inline_axis {
                    if let Some(height) = natural_size.height {
                        return Some(ratio.multiply(height));
                    }
                } else if let Some(width) = natural_size.width {
                    return Some(ratio.divide(width));
                }
            }

            // For the max-content size:
            // If it has a preferred aspect ratio:
            if facts.has_preferred_aspect_ratio {
                if let Some(size) = constraints.definite_size_in_ratio_determining_axis {
                    // If the available space is definite in the inline axis, use the stretch fit into that size for the inline size
                    // and calculate the block size using the aspect ratio.
                    //
                    // NB: This helper is only for the max-content size, which has no definite available inline size. Callers may
                    //     still know a definite used size in the opposite axis when the box lacks a natural size in that axis.
                    return Some(if is_inline_axis {
                        self.content_inline_size_from_aspect_ratio(node, size)
                    } else {
                        self.content_block_size_from_aspect_ratio(node, size)
                    });
                }

                let style = self.style(node);
                // Otherwise if the box has a <length> as its computed value for min-width or min-height, use that size and
                // calculate the other dimension using the aspect ratio; if both dimensions have a <length> minimum, choose the
                // one that results in the larger overall size.
                //
                // NOTE: This case was previous calculated from a 300x150 default size, rather than the box’s min size. This is
                //       believed to be a better behavior, and likely to be Web-compatible, but please send feedback to the CSSWG
                //       if there are any problems.
                let size_from_min_inline = if let Some(inline_size) = constraints.minimum_inline_size {
                    Some(if is_inline_axis {
                        inline_size
                    } else {
                        self.content_block_size_from_aspect_ratio(node, inline_size)
                    })
                } else if style.min_width().is_length_percentage() && !style.min_width().contains_percentage {
                    let inline_size = style.min_width().to_px(CssPixels::default());
                    Some(if is_inline_axis {
                        inline_size
                    } else {
                        self.content_block_size_from_aspect_ratio(node, inline_size)
                    })
                } else {
                    None
                };
                let size_from_min_block = if let Some(block_size) = constraints.minimum_block_size {
                    Some(if is_inline_axis {
                        self.content_inline_size_from_aspect_ratio(node, block_size)
                    } else {
                        block_size
                    })
                } else if style.min_height().is_length_percentage() && !style.min_height().contains_percentage {
                    let block_size = style.min_height().to_px(CssPixels::default());
                    Some(if is_inline_axis {
                        self.content_inline_size_from_aspect_ratio(node, block_size)
                    } else {
                        block_size
                    })
                } else {
                    None
                };

                return match (size_from_min_inline, size_from_min_block) {
                    (Some(inline), Some(block)) => Some(inline.max(block)),
                    (Some(inline), None) => Some(inline),
                    (None, Some(block)) => Some(block),
                    (None, None) => Some(if is_inline_axis {
                        // Otherwise use an inline size matching the corresponding dimension of the initial containing block and calculate
                        // the other dimension using the aspect ratio.
                        //
                        // NOTE: This author-controllable behavior is made possible by the new auto value for the min size properties.
                        //       This is believed to be a better behavior, but it is not yet clear if it is Web-compatible, so please
                        //       send feedback to the CSSWG if there are any problems.
                        facts.initial_containing_block_inline_size
                    } else {
                        self.content_block_size_from_aspect_ratio(node, facts.initial_containing_block_inline_size)
                    }),
                };
            }

            // If it has no preferred aspect ratio:
            // For both the min-content size and max-content size:
            // If the box has a <length> as its computed minimum size (min-width/min-height) in that dimension, use that size.
            let min_size = if is_inline_axis {
                self.style(node).min_width()
            } else {
                self.style(node).min_height()
            };
            if min_size.is_length_percentage() && !min_size.contains_percentage {
                return Some(min_size.to_px(CssPixels::default()));
            }
            // Otherwise, use 300px for the width and/or 150px for the height as needed.
            Some(CssPixels::from_integer(if is_inline_axis { 300 } else { 150 }))
        }

        fn tentative_inline_size_for_replaced_element(
            &self,
            node: Node,
            computed_inline_size: FfiSizeValue,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // 10.3.2 Inline, replaced elements, https://www.w3.org/TR/CSS22/visudet.html#inline-replaced-width
            // Treat percentages of indefinite containing block widths as 0 (the initial width).
            if computed_inline_size.is_percentage() && !constraints.has_percentage_basis_inline_size {
                return CssPixels::default();
            }
            let style = self.style(node);
            let computed_block_size = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
                FfiSizeValue::auto_value()
            } else {
                style.height()
            };
            let used_inline_size = if computed_inline_size.is_auto() {
                computed_inline_size.to_px(available_space.inline_size.to_px_or_zero())
            } else {
                self.calculate_inner_inline_size(node, available_space.inline_size, computed_inline_size, constraints)
            };
            // If 'height' and 'width' both have computed values of 'auto' and the element also has an intrinsic width,
            // then that intrinsic width is the used value of 'width'.
            let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
            if computed_block_size.is_auto()
                && computed_inline_size.is_auto()
                && let Some(width) = intrinsic.width
            {
                return width;
            }
            // If 'height' and 'width' both have computed values of 'auto' and the element has no intrinsic width,
            // but does have an intrinsic height and intrinsic ratio;
            // or if 'width' has a computed value of 'auto',
            // 'height' has some other computed value, and the element does have an intrinsic ratio; then the used value of 'width' is:
            //
            //     (used height) * (intrinsic ratio)
            let has_ratio = self.facts(node).has_preferred_aspect_ratio;
            if (computed_block_size.is_auto()
                && computed_inline_size.is_auto()
                && intrinsic.width.is_none()
                && intrinsic.height.is_some()
                && has_ratio)
                || (computed_inline_size.is_auto() && !computed_block_size.is_auto() && has_ratio)
            {
                let block_size = self.compute_block_size_for_replaced_element(node, available_space, constraints);
                return self.content_inline_size_from_aspect_ratio(node, block_size);
            }
            // If 'height' and 'width' both have computed values of 'auto' and the element has an intrinsic ratio but no intrinsic height or width,
            // then the used value of 'width' is undefined in CSS 2.2. However, it is suggested that, if the containing block's width does not itself
            // depend on the replaced element's width, then the used value of 'width' is calculated from the constraint equation used for block-level,
            // non-replaced elements in normal flow.
            if computed_block_size.is_auto()
                && computed_inline_size.is_auto()
                && intrinsic.width.is_none()
                && intrinsic.height.is_none()
                && has_ratio
            {
                if !available_space.inline_size.is_intrinsic_sizing_constraint() {
                    return self.calculate_stretch_fit_inline_size(node, available_space.inline_size);
                }
                match cyclic_percentage_intrinsic_contribution(
                    self.facts(node).is_replaced_box,
                    style.width().contains_percentage,
                    available_space.inline_size,
                    CyclicPercentageSizeProperty::PreferredOrMaxSize,
                ) {
                    CyclicPercentageIntrinsicContribution::ResolveAsZero => {
                        return CssPixels::default();
                    }
                    CyclicPercentageIntrinsicContribution::TreatAsInitialValue => {}
                    CyclicPercentageIntrinsicContribution::NotCyclic => {
                        return self.calculate_stretch_fit_inline_size(node, available_space.inline_size);
                    }
                }
            }
            // Otherwise, if 'width' has a computed value of 'auto', and the element has an intrinsic width, then that intrinsic width is the used value of 'width'.
            //
            // Otherwise, if 'width' has a computed value of 'auto', but none of the conditions above are met, then the used value of 'width' becomes 300px.
            // If 300px is too wide to fit the device, UAs should use the width of the largest rectangle that has a 2:1 ratio and fits the device instead.
            if computed_inline_size.is_auto() {
                if let Some(width) = intrinsic.width {
                    return width;
                }
                return CssPixels::from_integer(300);
            }
            used_inline_size
        }

        fn tentative_block_size_for_replaced_element(
            &self,
            node: Node,
            computed_block_size: FfiSizeValue,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // 10.6.2 Inline replaced elements, block-level replaced elements in normal flow, 'inline-block' replaced elements in normal flow and floating replaced elements
            // https://www.w3.org/TR/CSS22/visudet.html#inline-replaced-height
            let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
            // If 'height' and 'width' both have computed values of 'auto' and the element also has
            // an intrinsic height, then that intrinsic height is the used value of 'height'.
            if self.should_treat_inline_size_as_auto(node, available_space)
                && self.should_treat_block_size_as_auto(node, available_space, constraints)
                && let Some(height) = intrinsic.height
            {
                return height;
            }
            // Otherwise, if 'height' has a computed value of 'auto', and the element has an intrinsic ratio then the used value of 'height' is:
            //
            //     (used width) / (intrinsic ratio)
            if computed_block_size.is_auto() && self.facts(node).has_preferred_aspect_ratio {
                return self.content_block_size_from_aspect_ratio(node, self.used(node).content_inline_size);
            }
            // Otherwise, if 'height' has a computed value of 'auto', and the element has an intrinsic height, then that intrinsic height is the used value of 'height'.
            //
            // Otherwise, if 'height' has a computed value of 'auto', but none of the conditions above are met,
            // then the used value of 'height' must be set to the height of the largest rectangle that has a 2:1 ratio, has a height not greater than 150px,
            // and has a width not greater than the device width.
            if computed_block_size.is_auto() {
                return intrinsic.height.unwrap_or_else(|| CssPixels::from_integer(150));
            }
            // FIXME: Handle cases when available_space is not definite.
            self.calculate_inner_block_size(node, available_space, computed_block_size, constraints)
        }

        fn solve_replaced_size_constraint(
            &self,
            node: Node,
            input_inline_size: CssPixels,
            input_block_size: CssPixels,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> (CssPixels, CssPixels) {
            // 10.4 Minimum and maximum widths: 'min-width' and 'max-width'
            // https://www.w3.org/TR/CSS22/visudet.html#min-max-widths
            let style = self.style(node);
            let min_inline = if style.min_width().is_auto() {
                CssPixels::default()
            } else {
                self.calculate_inner_inline_size(node, available_space.inline_size, style.min_width(), constraints)
            };
            let specified_max_inline =
                if self.should_treat_max_inline_size_as_none(node, available_space.inline_size, constraints) {
                    input_inline_size
                } else {
                    self.calculate_inner_inline_size(node, available_space.inline_size, style.max_width(), constraints)
                };
            let max_inline = min_inline.max(specified_max_inline);
            let min_block = if style.min_height().is_auto() {
                CssPixels::default()
            } else {
                self.calculate_inner_block_size(node, available_space, style.min_height(), constraints)
            };
            let specified_max_block =
                if self.should_treat_max_block_size_as_none(node, available_space.block_size, constraints) {
                    input_block_size
                } else {
                    self.calculate_inner_block_size(node, available_space, style.max_height(), constraints)
                };
            let max_block = min_block.max(specified_max_block);

            // These are from the "Constraint Violation" table in spec, but reordered so that each condition is
            // interpreted as mutually exclusive to any other.
            if input_inline_size < min_inline && input_block_size > max_block {
                return (min_inline, max_block);
            }
            if input_inline_size > max_inline && input_block_size < min_block {
                return (max_inline, min_block);
            }
            if input_inline_size > CssPixels::default() && input_block_size > CssPixels::default() {
                let max_inline_fraction_le_max_block = (max_inline.raw_value() as i64)
                    * (input_block_size.raw_value() as i64)
                    <= (max_block.raw_value() as i64) * (input_inline_size.raw_value() as i64);
                if input_inline_size > max_inline && input_block_size > max_block && max_inline_fraction_le_max_block {
                    return (
                        max_inline,
                        min_block.max(self.content_block_size_from_aspect_ratio(node, max_inline)),
                    );
                }
                if input_inline_size > max_inline && input_block_size > max_block && !max_inline_fraction_le_max_block {
                    return (
                        min_inline.max(self.content_inline_size_from_aspect_ratio(node, max_block)),
                        max_block,
                    );
                }
                let min_inline_fraction_le_min_block = (min_inline.raw_value() as i64)
                    * (input_block_size.raw_value() as i64)
                    <= (min_block.raw_value() as i64) * (input_inline_size.raw_value() as i64);
                if input_inline_size < min_inline && input_block_size < min_block && min_inline_fraction_le_min_block {
                    return (
                        max_inline.min(self.content_inline_size_from_aspect_ratio(node, min_block)),
                        min_block,
                    );
                }
                if input_inline_size < min_inline && input_block_size < min_block && !min_inline_fraction_le_min_block {
                    return (
                        min_inline,
                        max_block.min(self.content_block_size_from_aspect_ratio(node, min_inline)),
                    );
                }
            }
            if input_inline_size > max_inline {
                return (
                    max_inline,
                    self.content_block_size_from_aspect_ratio(node, max_inline)
                        .max(min_block),
                );
            }
            if input_inline_size < min_inline {
                return (
                    min_inline,
                    self.content_block_size_from_aspect_ratio(node, min_inline)
                        .min(max_block),
                );
            }
            if input_block_size > max_block {
                return (
                    self.content_inline_size_from_aspect_ratio(node, max_block)
                        .max(min_inline),
                    max_block,
                );
            }
            if input_block_size < min_block {
                return (
                    self.content_inline_size_from_aspect_ratio(node, min_block)
                        .min(max_inline),
                    min_block,
                );
            }
            (input_inline_size, input_block_size)
        }

        pub(crate) fn compute_inline_size_for_replaced_element(
            &self,
            node: Node,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // 10.3.4 Block-level, replaced elements in normal flow...
            // 10.3.2 Inline, replaced elements
            let style = self.style(node);
            let computed_inline = if self.should_treat_inline_size_as_auto(node, available_space) {
                FfiSizeValue::auto_value()
            } else {
                style.width()
            };
            let computed_block = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
                FfiSizeValue::auto_value()
            } else {
                style.height()
            };
            // 1. The tentative used width is calculated (without 'min-width' and 'max-width')
            let mut used =
                self.tentative_inline_size_for_replaced_element(node, computed_inline, available_space, constraints);
            if computed_inline.is_auto() && computed_block.is_auto() && self.facts(node).has_preferred_aspect_ratio {
                let block =
                    self.tentative_block_size_for_replaced_element(node, computed_block, available_space, constraints);
                used = self
                    .solve_replaced_size_constraint(node, used, block, available_space, constraints)
                    .0;
            }
            // 2. If the tentative used width is greater than 'max-width', the rules above are applied again,
            //    but this time using the computed value of 'max-width' as the computed value for 'width'.
            if !self.should_treat_max_inline_size_as_none(node, available_space.inline_size, constraints) {
                let max =
                    self.calculate_inner_inline_size(node, available_space.inline_size, style.max_width(), constraints);
                if used > max {
                    used = self.tentative_inline_size_for_replaced_element(
                        node,
                        style.max_width(),
                        available_space,
                        constraints,
                    );
                }
            }
            // 3. If the resulting width is smaller than 'min-width', the rules above are applied again,
            //    but this time using the value of 'min-width' as the computed value for 'width'.
            if !style.min_width().is_auto() {
                let min =
                    self.calculate_inner_inline_size(node, available_space.inline_size, style.min_width(), constraints);
                if used < min {
                    used = self.tentative_inline_size_for_replaced_element(
                        node,
                        style.min_width(),
                        available_space,
                        constraints,
                    );
                }
            }
            used
        }

        pub(crate) fn compute_block_size_for_replaced_element(
            &self,
            node: Node,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // 10.6.2 Inline replaced elements
            // 10.6.4 Block-level replaced elements in normal flow
            // 10.6.6 Floating replaced elements
            // 10.6.10 'inline-block' replaced elements in normal flow
            let style = self.style(node);
            let computed_inline = if self.should_treat_inline_size_as_auto(node, available_space) {
                FfiSizeValue::auto_value()
            } else {
                style.width()
            };
            let computed_block = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
                FfiSizeValue::auto_value()
            } else {
                style.height()
            };
            // 1. The tentative used height is calculated (without 'min-height' and 'max-height')
            let mut used =
                self.tentative_block_size_for_replaced_element(node, computed_block, available_space, constraints);
            if computed_inline.is_auto() && computed_block.is_auto() && self.facts(node).has_preferred_aspect_ratio {
                // However, for replaced elements with both 'width' and 'height' computed as 'auto',
                // use the algorithm under 'Minimum and maximum widths'
                // https://www.w3.org/TR/CSS22/visudet.html#min-max-widths
                // to find the used width and height.
                let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
                if intrinsic.width.is_some() || intrinsic.height.is_none() {
                    // NOTE: This is a special case where calling tentative_inline_size_for_replaced_element() would call us right back,
                    //       and we'd end up in an infinite loop. So we need to handle this case separately.
                    let inline = self.tentative_inline_size_for_replaced_element(
                        node,
                        computed_inline,
                        available_space,
                        constraints,
                    );
                    used = self
                        .solve_replaced_size_constraint(node, inline, used, available_space, constraints)
                        .1;
                }
            }
            // 2. If this tentative height is greater than 'max-height', the rules above are applied again,
            //    but this time using the value of 'max-height' as the computed value for 'height'.
            if !self.should_treat_max_block_size_as_none(node, available_space.block_size, constraints) {
                let max = self.calculate_inner_block_size(node, available_space, style.max_height(), constraints);
                if used > max {
                    used = self.tentative_block_size_for_replaced_element(
                        node,
                        style.max_height(),
                        available_space,
                        constraints,
                    );
                }
            }
            // 3. If the resulting height is smaller than 'min-height', the rules above are applied again,
            //    but this time using the value of 'min-height' as the computed value for 'height'.
            if !style.min_height().is_auto() {
                let min = self.calculate_inner_block_size(node, available_space, style.min_height(), constraints);
                if used < min {
                    used = self.tentative_block_size_for_replaced_element(
                        node,
                        style.min_height(),
                        available_space,
                        constraints,
                    );
                }
            }
            used
        }

        pub(crate) fn box_is_sized_as_replaced_element(
            &self,
            node: Node,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> bool {
            let facts = self.facts(node);
            // When a box has a preferred aspect ratio, its automatic sizes are calculated the same as for a
            // replaced element with a natural aspect ratio and no natural size in that axis, see e.g. CSS2 §10
            // and CSS Flexible Box Model Level 1 §9.2.
            // https://drafts.csswg.org/css-sizing-4/#aspect-ratio-automatic
            if facts.has_replaced_element_table_display_adjustment
                || (facts.is_replaced_box && facts.has_auto_content_box_size)
            {
                return true;
            }
            if facts.has_preferred_aspect_ratio || facts.has_auto_content_box_size {
                // From CSS2:
                // If height and width both have computed values of auto and the element has an intrinsic ratio but no intrinsic height or width,
                // then the used value of width is undefined in CSS 2.
                // However, it is suggested that, if the containing block’s width does not itself depend on the replaced element’s width,
                // then the used value of width is calculated from the constraint equation used for block-level, non-replaced elements in normal flow.
                //
                // AD-HOC: If box has preferred aspect ratio but width and height are not specified, then we should
                //         size it as a normal box to match other browsers.
                if self.should_treat_inline_size_as_auto(node, available_space)
                    && self.should_treat_block_size_as_auto(node, available_space, constraints)
                    && !facts.has_auto_content_width
                    && !facts.has_auto_content_height
                {
                    return false;
                }
                return true;
            }
            false
        }

        pub(crate) fn constraints_for_child_context(
            &self,
            containing_block: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> FfiContainingBlockConstraints {
            let facts = self.facts(containing_block);
            let style = self.style(containing_block);
            let used = self.used(containing_block);
            // Anonymous boxes are invisible to percentage resolution: their children resolve percentages
            // against the closest non-anonymous ancestor, so an anonymous containing block without a
            // definite size of its own passes the constraints it was given through. Anonymous table
            // cells are the exception: they are proper containing blocks with their own size semantics.
            let should_forward_indefinite_basis = facts.is_box
                && facts.is_anonymous
                && !facts.is_table_cell
                && !facts.has_auto_content_box_size
                && used.inline_size_constraint == FfiSizeConstraint::None
                && used.block_size_constraint == FfiSizeConstraint::None;

            let (has_inline, inline) = if used.has_definite_inline_size() {
                (true, used.content_inline_size)
            } else if should_forward_indefinite_basis {
                (
                    constraints.has_percentage_basis_inline_size,
                    constraints.percentage_basis_inline_size,
                )
            } else {
                (false, CssPixels::default())
            };
            let (has_block, block) = if used.has_definite_block_size() {
                (true, used.content_block_size)
            } else if should_forward_indefinite_basis {
                (
                    constraints.has_percentage_basis_block_size,
                    constraints.percentage_basis_block_size,
                )
            } else {
                (false, CssPixels::default())
            };

            // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
            // 1. Let element be the nearest ancestor containing block of element, if there is one.
            //    Otherwise, return the initial containing block.
            //
            // 2. If element has a computed value of the display property that is table-cell, then return a
            //    UA-defined value.
            // FIXME: Likely UA-defined value should not be 0.
            //
            // 3. If element has a computed value of the height property that is not auto, then return element.
            //
            // 4. If element has a computed value of the position property that is absolute, or if element is a
            //    not a block container or a table wrapper box, then return element.
            //
            // 5. Jump to the first step.
            // NOTE: Evaluated incrementally: in-flow auto-height block containers pass the basis they
            //       inherited from their own containing block through to their children.
            let (has_quirks_block, quirks_block) = if facts.is_viewport
                || facts.is_table_cell
                || !style.height().is_auto()
                || facts.is_absolutely_positioned
                || !facts.is_block_container
                || facts.is_table_wrapper
            {
                (true, used.content_block_size)
            } else {
                (
                    constraints.has_quirks_mode_percentage_basis_block_size,
                    constraints.quirks_mode_percentage_basis_block_size,
                )
            };

            FfiContainingBlockConstraints {
                has_percentage_basis_inline_size: has_inline,
                percentage_basis_inline_size: inline,
                has_percentage_basis_block_size: has_block,
                percentage_basis_block_size: block,
                has_quirks_mode_percentage_basis_block_size: has_quirks_block,
                quirks_mode_percentage_basis_block_size: quirks_block,
            }
        }

        pub(crate) fn should_treat_inline_size_as_auto(&self, node: Node, available_space: AvailableSpace) -> bool {
            let style = self.style(node);
            let size = style.width();
            if size.is_auto() {
                return true;
            }
            // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
            if size.contains_percentage {
                match cyclic_percentage_intrinsic_contribution(
                    self.facts(node).is_replaced_box,
                    true,
                    available_space.inline_size,
                    CyclicPercentageSizeProperty::PreferredOrMaxSize,
                ) {
                    CyclicPercentageIntrinsicContribution::ResolveAsZero => return false,
                    CyclicPercentageIntrinsicContribution::TreatAsInitialValue => return true,
                    CyclicPercentageIntrinsicContribution::NotCyclic => {}
                }
                if available_space.inline_size.is_indefinite() {
                    return true;
                }
            }
            let facts = self.facts(node);
            // AD-HOC: If the box has a preferred aspect ratio and an intrinsic keyword for width...
            if facts.has_preferred_aspect_ratio && size.is_intrinsic_sizing_constraint() {
                // If the box has no natural height to resolve the aspect ratio, we treat the width as auto.
                if !facts.has_auto_content_height {
                    return true;
                }
                // If the box has definite height, we can resolve the width through the aspect ratio.
                if self.used(node).has_definite_block_size() {
                    return true;
                }
            }
            false
        }

        pub(crate) fn should_treat_block_size_as_auto(
            &self,
            node: Node,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> bool {
            let style = self.style(node);
            let size = style.height();
            let facts = self.facts(node);
            if size.is_auto() {
                if self.used(node).has_definite_inline_size() && facts.has_preferred_aspect_ratio {
                    return false;
                }
                return true;
            }
            // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
            if size.contains_percentage {
                match cyclic_percentage_intrinsic_contribution(
                    facts.is_replaced_box,
                    true,
                    available_space.block_size,
                    CyclicPercentageSizeProperty::PreferredOrMaxSize,
                ) {
                    CyclicPercentageIntrinsicContribution::ResolveAsZero => return false,
                    CyclicPercentageIntrinsicContribution::TreatAsInitialValue => return true,
                    CyclicPercentageIntrinsicContribution::NotCyclic => {}
                }
                // https://www.w3.org/TR/CSS22/visudet.html#the-height-property
                // If the height of the containing block is not specified explicitly (i.e., it depends on
                // content height), and this element is not absolutely positioned, the percentage value
                // is treated as 'auto'.
                // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
                // In quirks mode, percentage heights can resolve even without explicit containing block
                // height. The quirk applies to DOM elements only (not anonymous boxes), and excludes
                // table-related display types.
                if !facts.is_absolutely_positioned {
                    let parent = self.parent(node);
                    let parent_is_flex_or_grid = if parent.is_null() {
                        false
                    } else {
                        let display = self.facts(parent).display;
                        display.is_flex_inside() || display.is_grid_inside()
                    };
                    // Flex/grid items resolve percentage heights against their container, not via quirk.
                    // The quirk should not apply inside user agent shadow trees.
                    let quirk_applies = facts.document_in_quirks_mode
                        && !facts.is_anonymous
                        && !facts.is_table_box
                        && !parent_is_flex_or_grid
                        && !facts.is_in_user_agent_shadow_tree;
                    if !quirk_applies && !constraints.has_percentage_basis_block_size {
                        return true;
                    }
                }
            }
            // AD-HOC: If the box has a preferred aspect ratio and an intrinsic keyword for height...
            if facts.has_preferred_aspect_ratio && size.is_intrinsic_sizing_constraint() {
                // If the box has no natural width to resolve the aspect ratio, we treat the height as auto.
                if !facts.has_auto_content_width {
                    return true;
                }
                // If the box has definite width, we can resolve the height through the aspect ratio.
                if self.used(node).has_definite_inline_size() {
                    return true;
                }
            }
            false
        }

        pub(crate) fn should_treat_max_inline_size_as_none(
            &self,
            node: Node,
            available: AvailableSize,
            constraints: FfiContainingBlockConstraints,
        ) -> bool {
            let size = self.style(node).max_width();
            if size.is_none() || (available.is_max_content() && size.is_max_content()) {
                return true;
            }
            // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
            if size.contains_percentage {
                match cyclic_percentage_intrinsic_contribution(
                    self.facts(node).is_replaced_box,
                    true,
                    available,
                    CyclicPercentageSizeProperty::PreferredOrMaxSize,
                ) {
                    CyclicPercentageIntrinsicContribution::ResolveAsZero => return false,
                    CyclicPercentageIntrinsicContribution::TreatAsInitialValue => return true,
                    CyclicPercentageIntrinsicContribution::NotCyclic => {}
                }
                if !constraints.has_percentage_basis_inline_size {
                    return true;
                }
            }
            (size.is_fit_content() && available.is_intrinsic_sizing_constraint())
                || (size.is_max_content() && available.is_max_content())
                || (size.is_min_content() && available.is_min_content())
        }

        pub(crate) fn should_treat_max_block_size_as_none(
            &self,
            node: Node,
            available: AvailableSize,
            constraints: FfiContainingBlockConstraints,
        ) -> bool {
            // https://www.w3.org/TR/CSS22/visudet.html#min-max-heights
            // If the height of the containing block is not specified explicitly (i.e., it depends on content height),
            // and this element is not absolutely positioned, the percentage value is treated as '0' (for 'min-height')
            // or 'none' (for 'max-height').
            let size = self.style(node).max_height();
            if size.is_none() {
                return true;
            }
            if size.contains_percentage {
                if available.is_min_content() {
                    return false;
                }
                if !constraints.has_percentage_basis_block_size {
                    return true;
                }
            }
            (size.is_fit_content() && available.is_intrinsic_sizing_constraint())
                || (size.is_max_content() && available.is_max_content())
                || (size.is_min_content() && available.is_min_content())
        }

        fn calculate_stretch_fit_inline_size(&self, node: Node, available: AvailableSize) -> CssPixels {
            // https://drafts.csswg.org/css-sizing-3/#stretch-fit-size
            // The size a box would take if its outer size filled the available space in the given axis;
            // in other words, the stretch fit into the available space, if that is definite.
            //
            // Undefined if the available space is indefinite.
            if !available.is_definite() {
                return CssPixels::default();
            }
            let used = self.used(node);
            available.to_px_or_zero()
                - used.margin_left
                - used.margin_right
                - used.padding_left
                - used.padding_right
                - used.border_left
                - used.border_right
        }

        fn calculate_stretch_fit_block_size(&self, node: Node, available: AvailableSize) -> CssPixels {
            // https://drafts.csswg.org/css-sizing-3/#stretch-fit-size
            // The size a box would take if its outer size filled the available space in the given axis;
            // in other words, the stretch fit into the available space, if that is definite.
            // Undefined if the available space is indefinite.
            let used = self.used(node);
            available.to_px_or_zero()
                - used.margin_top
                - used.margin_bottom
                - used.padding_top
                - used.padding_bottom
                - used.border_top
                - used.border_bottom
        }

        fn intrinsic_cache_get(
            &self,
            node: Node,
            kind: FfiIntrinsicSizeCacheKind,
            key: FfiIntrinsicSizeCacheKey,
        ) -> Option<CssPixels> {
            let mut value = CssPixels::default();
            bump(FfiOp::IntrinsicCacheGetCallback);
            // SAFETY: The out pointer is valid for this synchronous cache lookup.
            let found = unsafe {
                (self.callbacks.intrinsic_size_cache_get)(self.callbacks.context, node, kind, key, &raw mut value)
            };
            if found {
                bump(FfiOp::IntrinsicCacheHit);
                Some(value)
            } else {
                None
            }
        }

        fn intrinsic_cache_put(
            &self,
            node: Node,
            kind: FfiIntrinsicSizeCacheKind,
            key: FfiIntrinsicSizeCacheKey,
            value: CssPixels,
        ) {
            bump(FfiOp::IntrinsicCachePutCallback);
            // SAFETY: The host owns the box cache and copies the key/value.
            unsafe {
                (self.callbacks.intrinsic_size_cache_put)(self.callbacks.context, node, kind, key, value);
            }
        }

        fn calculate_transferred_inline_size_for_replaced_element(
            &self,
            node: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> Option<CssPixels> {
            // https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
            // "size constraints in the opposite dimension will transfer through and can affect the auto size in the considered one"
            let facts = self.facts(node);
            let style = self.style(node);
            // https://drafts.csswg.org/css2/#inline-replaced-width
            // "'width' has a computed value of 'auto', 'height' has some other computed value, and the element does have an intrinsic ratio"
            if !facts.is_replaced_box
                || !facts.has_preferred_aspect_ratio
                || !style.width().is_auto()
                || style.height().is_auto()
                || style.height().is_intrinsic_sizing_constraint()
            {
                return None;
            }
            let available_space = self
                .used(node)
                .available_inner_space_or_constraints_from(AvailableSpace {
                    inline_size: AvailableSize::max_content(),
                    block_size: AvailableSize::indefinite(),
                });
            if self.should_treat_block_size_as_auto(node, available_space, constraints) {
                return None;
            }
            // https://drafts.csswg.org/css2/#inline-replaced-width
            // "(used height) * (intrinsic ratio)"
            Some(self.compute_inline_size_for_replaced_element(
                node,
                available_space,
                FfiContainingBlockConstraints::default(),
            ))
        }

        pub(crate) fn calculate_min_content_inline_size(
            &self,
            node: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            let facts = self.facts(node);
            let style = self.style(node);
            if facts.is_replaced_box && (style.width().contains_percentage || style.max_width().contains_percentage) {
                // https://www.w3.org/TR/css-sizing-3/#replaced-percentage-min-contribution
                // NOTE: If the box is replaced, a cyclic percentage in the value of any max size property or
                //       preferred size property (width/max-width/height/max-height), is resolved against zero
                //       when calculating the min-content contribution in the corresponding axis.
                // FIXME: If the box also has a preferred aspect ratio, then this min-content contribution is
                //        floored by any <length-percentage> minimum size from the opposite axis—resolving any
                //        such percentage against zero—transferred through the preferred aspect ratio.
                // Note: The min-content contribution is, as always, also floored by the minimum size in its own axis.
                if !style.min_width().is_length_percentage() {
                    return CssPixels::default();
                }
                let mut zero_constraints = constraints;
                zero_constraints.has_percentage_basis_inline_size = true;
                zero_constraints.percentage_basis_inline_size = CssPixels::default();
                return self.calculate_inner_inline_size(
                    node,
                    AvailableSize::min_content(),
                    style.min_width(),
                    zero_constraints,
                );
            }
            if let Some(transferred) = self.calculate_transferred_inline_size_for_replaced_element(node, constraints) {
                return transferred;
            }
            let auto_size = self.auto_content_size(node);
            if let Some(width) = auto_size.width {
                return width;
            }
            if facts.is_replaced_box
                && !facts.has_preferred_aspect_ratio
                && let Some(fallback) = self.max_content_size_for_replaced_element_without_natural_size(
                    node,
                    auto_size,
                    SizeDimension::Inline,
                    ReplacedMaxContentSizeConstraints::default(),
                )
            {
                return fallback;
            }
            // Boxes with no children have zero intrinsic inline size.
            if !self.has_children(node) {
                return CssPixels::default();
            }
            let key = cache_key(None, constraints);
            if let Some(cached) = self.intrinsic_cache_get(node, FfiIntrinsicSizeCacheKind::MinContentInline, key) {
                return cached;
            }

            let mut measurement = MeasurementState::create(self.callbacks, node, constraints);
            let root = measurement.root_used_mut();
            root.inline_size_constraint = FfiSizeConstraint::MinContent;
            root.has_definite_inline_size = false;
            let block_size = if root.has_definite_block_size() {
                AvailableSize::definite(root.content_block_size)
            } else {
                AvailableSize::indefinite()
            };
            let result = measurement.run(
                node,
                FfiLayoutInput {
                    available_space: AvailableSpace {
                        inline_size: AvailableSize::min_content(),
                        block_size,
                    },
                    containing_block_constraints: constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
            );
            let value = clamp_to_max_dimension_value(result.automatic_content_inline_size);
            self.intrinsic_cache_put(node, FfiIntrinsicSizeCacheKind::MinContentInline, key, value);
            value
        }

        pub(crate) fn calculate_max_content_inline_size(
            &self,
            node: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            let facts = self.facts(node);
            let style = self.style(node);
            let mut auto_size = self.auto_content_size(node);
            if let Some(transferred) = self.calculate_transferred_inline_size_for_replaced_element(node, constraints) {
                return transferred;
            }
            if auto_size.width.is_none() && (facts.has_default_preferred_width || facts.has_default_preferred_height) {
                // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
                // "If the box is non-replaced, then the entire value of any max size property or preferred size property
                // ('width'/'max-width'/'height'/'max-height') specified as an expression containing a percentage [...] that is
                // cyclic is treated for the purpose of calculating the box's intrinsic size contributions only as that
                // property's initial value."
                //
                // This means an `appearance: none` text input with a cyclic `width: 100%` still contributes its `width: auto`
                // size to max-content sizing. Do not use this for min-content sizing: CSS Sizing's "Compressible Replaced
                // Elements" section considers non-button-like <input> controls replaced for the percentage-sized replaced
                // element rule, so their cyclic-percentage min-content contribution can still compress toward zero.
                auto_size = ReplacedIntrinsicSize {
                    width: facts
                        .has_default_preferred_width
                        .then_some(facts.default_preferred_width),
                    height: facts
                        .has_default_preferred_height
                        .then_some(facts.default_preferred_height),
                    aspect_ratio: None,
                };
            }
            if let Some(width) = auto_size.width {
                return width;
            }
            let definite_block_size =
                if facts.is_replaced_box && auto_size.height.is_none() && self.used(node).has_definite_block_size() {
                    Some(self.used(node).content_block_size)
                } else {
                    None
                };
            let max_content_available = AvailableSize::max_content();
            let intrinsic_available_space = AvailableSpace {
                inline_size: max_content_available,
                block_size: AvailableSize::indefinite(),
            };
            let resolve_destination_inline_size =
                |size: FfiSizeValue, property: CyclicPercentageSizeProperty| -> Option<CssPixels> {
                    if !size.is_length_percentage() {
                        return None;
                    }
                    match cyclic_percentage_intrinsic_contribution(
                        facts.is_replaced_box,
                        size.contains_percentage,
                        max_content_available,
                        property,
                    ) {
                        CyclicPercentageIntrinsicContribution::TreatAsInitialValue => None,
                        CyclicPercentageIntrinsicContribution::ResolveAsZero => {
                            let mut zero_constraints = constraints;
                            zero_constraints.has_percentage_basis_inline_size = true;
                            zero_constraints.percentage_basis_inline_size = CssPixels::default();
                            Some(self.calculate_inner_inline_size(node, max_content_available, size, zero_constraints))
                        }
                        CyclicPercentageIntrinsicContribution::NotCyclic => {
                            if size.contains_percentage && !constraints.has_percentage_basis_inline_size {
                                None
                            } else {
                                Some(self.calculate_inner_inline_size(node, max_content_available, size, constraints))
                            }
                        }
                    }
                };
            let resolve_block_size = |size: FfiSizeValue,
                                      property: CyclicPercentageSizeProperty|
             -> Option<CssPixels> {
                if !size.is_length_percentage() {
                    return None;
                }
                if !size.contains_percentage || constraints.has_percentage_basis_block_size {
                    return Some(self.calculate_inner_block_size(node, intrinsic_available_space, size, constraints));
                }
                match cyclic_percentage_intrinsic_contribution(
                    facts.is_replaced_box,
                    size.contains_percentage,
                    max_content_available,
                    property,
                ) {
                    CyclicPercentageIntrinsicContribution::TreatAsInitialValue => None,
                    CyclicPercentageIntrinsicContribution::ResolveAsZero => {
                        let mut zero_constraints = constraints;
                        zero_constraints.has_percentage_basis_block_size = true;
                        zero_constraints.percentage_basis_block_size = CssPixels::default();
                        Some(self.calculate_inner_block_size(node, intrinsic_available_space, size, zero_constraints))
                    }
                    CyclicPercentageIntrinsicContribution::NotCyclic => None,
                }
            };

            let definite_minimum_inline_size =
                resolve_destination_inline_size(style.min_width(), CyclicPercentageSizeProperty::MinSize);
            let definite_minimum_block_size =
                resolve_block_size(style.min_height(), CyclicPercentageSizeProperty::MinSize);
            let replaced_constraints = ReplacedMaxContentSizeConstraints {
                definite_size_in_ratio_determining_axis: definite_block_size,
                minimum_inline_size: definite_minimum_inline_size,
                minimum_block_size: definite_minimum_block_size,
            };
            if let Some(max_content_inline_size) = self.max_content_size_for_replaced_element_without_natural_size(
                node,
                auto_size,
                SizeDimension::Inline,
                replaced_constraints,
            ) {
                if definite_block_size.is_none()
                    && facts.has_preferred_aspect_ratio
                    && let Some(definite_maximum_block_size) =
                        resolve_block_size(style.max_height(), CyclicPercentageSizeProperty::PreferredOrMaxSize)
                {
                    // https://drafts.csswg.org/css-sizing-4/#aspect-ratio-size-transfers
                    // First, any definite minimum size is converted and transferred from the origin to destination axis.
                    // This transferred minimum is capped by any definite preferred or maximum size in the destination axis.
                    let mut transferred_minimum = definite_minimum_block_size
                        .map(|value| self.content_inline_size_from_aspect_ratio(node, value));
                    if let Some(value) = transferred_minimum {
                        transferred_minimum = resolve_destination_inline_size(
                            style.width(),
                            CyclicPercentageSizeProperty::PreferredOrMaxSize,
                        )
                        .map_or(Some(value), |resolved| Some(value.min(resolved)));
                        let value = transferred_minimum.unwrap();
                        transferred_minimum = resolve_destination_inline_size(
                            style.max_width(),
                            CyclicPercentageSizeProperty::PreferredOrMaxSize,
                        )
                        .map_or(Some(value), |resolved| Some(value.min(resolved)));
                    }

                    // Then, any definite maximum size is converted and transferred from the origin to destination.
                    // This transferred maximum is floored by any definite preferred or minimum size in the destination axis
                    // as well as by the transferred minimum, if any.
                    let mut transferred_maximum =
                        self.content_inline_size_from_aspect_ratio(node, definite_maximum_block_size);
                    if let Some(resolved) =
                        resolve_destination_inline_size(style.width(), CyclicPercentageSizeProperty::PreferredOrMaxSize)
                    {
                        transferred_maximum = transferred_maximum.max(resolved);
                    }
                    if let Some(resolved) =
                        resolve_destination_inline_size(style.min_width(), CyclicPercentageSizeProperty::MinSize)
                    {
                        transferred_maximum = transferred_maximum.max(resolved);
                    }
                    if let Some(transferred_minimum) = transferred_minimum {
                        transferred_maximum = transferred_maximum.max(transferred_minimum);
                    }
                    return max_content_inline_size.min(transferred_maximum);
                }
                return max_content_inline_size;
            }
            // Boxes with no children have zero intrinsic inline size.
            if !self.has_children(node) {
                return CssPixels::default();
            }
            let key = cache_key(None, constraints);
            if let Some(cached) = self.intrinsic_cache_get(node, FfiIntrinsicSizeCacheKind::MaxContentInline, key) {
                return cached;
            }

            let mut measurement = MeasurementState::create(self.callbacks, node, constraints);
            let root = measurement.root_used_mut();
            root.inline_size_constraint = FfiSizeConstraint::MaxContent;
            root.has_definite_inline_size = false;
            let block_size = if root.has_definite_block_size() {
                AvailableSize::definite(root.content_block_size)
            } else {
                AvailableSize::indefinite()
            };
            let result = measurement.run(
                node,
                FfiLayoutInput {
                    available_space: AvailableSpace {
                        inline_size: AvailableSize::max_content(),
                        block_size,
                    },
                    containing_block_constraints: constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
            );
            let value = clamp_to_max_dimension_value(result.automatic_content_inline_size);
            self.intrinsic_cache_put(node, FfiIntrinsicSizeCacheKind::MaxContentInline, key, value);
            value
        }

        pub(crate) fn calculate_min_content_block_size(
            &self,
            node: Node,
            inline_size: CssPixels,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // https://www.w3.org/TR/css-sizing-3/#min-content-block-size
            let facts = self.facts(node);
            // For block containers, tables, and inline boxes, this is equivalent to the max-content block size.
            if facts.is_block_container || facts.is_table_box {
                return self.calculate_max_content_block_size(node, inline_size, constraints);
            }
            let auto_size = self.auto_content_size(node);
            if let Some(height) = auto_size.height {
                return auto_size.aspect_ratio.map_or(height, |ratio| ratio.divide(inline_size));
            }
            // Boxes with no children have zero intrinsic height.
            if !self.has_children(node) {
                return CssPixels::default();
            }
            let key = cache_key(Some(inline_size), constraints);
            if let Some(cached) = self.intrinsic_cache_get(node, FfiIntrinsicSizeCacheKind::MinContentBlock, key) {
                return cached;
            }

            let mut measurement = MeasurementState::create(self.callbacks, node, constraints);
            let root = measurement.root_used_mut();
            root.block_size_constraint = FfiSizeConstraint::MinContent;
            root.has_definite_block_size = false;
            root.set_content_inline_size(inline_size);
            let result = measurement.run(
                node,
                FfiLayoutInput {
                    available_space: AvailableSpace {
                        inline_size: AvailableSize::definite(inline_size),
                        block_size: AvailableSize::min_content(),
                    },
                    containing_block_constraints: constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
            );
            let value = clamp_to_max_dimension_value(result.automatic_content_block_size);
            self.intrinsic_cache_put(node, FfiIntrinsicSizeCacheKind::MinContentBlock, key, value);
            value
        }

        pub(crate) fn calculate_max_content_block_size(
            &self,
            node: Node,
            inline_size: CssPixels,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            if let Some(ratio) = self.preferred_aspect_ratio(node) {
                return ratio.divide(inline_size);
            }
            let auto_size = self.auto_content_size(node);
            if let Some(height) = auto_size.height {
                return height;
            }
            if let Some(fallback) = self.max_content_size_for_replaced_element_without_natural_size(
                node,
                auto_size,
                SizeDimension::Block,
                ReplacedMaxContentSizeConstraints::default(),
            ) {
                return fallback;
            }
            // Boxes with no children have zero intrinsic height.
            if !self.has_children(node) {
                return CssPixels::default();
            }
            let key = cache_key(Some(inline_size), constraints);
            if let Some(cached) = self.intrinsic_cache_get(node, FfiIntrinsicSizeCacheKind::MaxContentBlock, key) {
                return cached;
            }

            let mut measurement = MeasurementState::create(self.callbacks, node, constraints);
            let root = measurement.root_used_mut();
            root.block_size_constraint = FfiSizeConstraint::MaxContent;
            root.has_definite_block_size = false;
            root.set_content_inline_size(inline_size);
            let result = measurement.run(
                node,
                FfiLayoutInput {
                    available_space: AvailableSpace {
                        inline_size: AvailableSize::definite(inline_size),
                        block_size: AvailableSize::max_content(),
                    },
                    containing_block_constraints: constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
            );
            let value = clamp_to_max_dimension_value(result.automatic_content_block_size);
            self.intrinsic_cache_put(node, FfiIntrinsicSizeCacheKind::MaxContentBlock, key, value);
            value
        }

        pub(crate) fn measure_automatic_content_block_size(
            &self,
            node: Node,
            layout_mode: u8,
            inner_available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            let measurement = MeasurementState::create(self.callbacks, node, constraints);
            measurement
                .run_with_layout_mode(
                    node,
                    layout_mode,
                    FfiLayoutInput {
                        available_space: inner_available_space,
                        containing_block_constraints: constraints,
                        has_content_box_position_in_bfc_root: false,
                        content_box_position_in_bfc_root: Default::default(),
                        has_table_grid_min_border_box_block_size: false,
                        table_grid_min_border_box_block_size: CssPixels::default(),
                    },
                )
                .automatic_content_block_size
        }

        pub(crate) fn make_button_content_box_definite(
            &self,
            node: Node,
            layout_mode: u8,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
            measured_content_block_size: Option<CssPixels>,
        ) {
            let facts = self.facts(node);
            if !facts.uses_button_layout {
                return;
            }
            // Flex/grid-inside buttons are their own flex/grid container and get no anonymous content wrapper,
            // so there is nothing to make definite for centering.
            let style = self.style(node);
            if style.display.is_flex_inside() || style.display.is_grid_inside() {
                return;
            }
            // With auto height and no min-height the content box already exactly wraps the content, so there is
            // no extra space to center within and no need to force a definite content box.
            if style.height().is_auto() && style.min_height().is_auto() {
                return;
            }
            if self.used(node).has_definite_block_size() {
                return;
            }
            let natural = measured_content_block_size.unwrap_or_else(|| {
                self.measure_automatic_content_block_size(
                    node,
                    layout_mode,
                    self.used(node)
                        .available_inner_space_or_constraints_from(available_space),
                    constraints,
                )
            });
            let mut used_block_size = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
                natural
            } else {
                self.calculate_inner_block_size(node, available_space, style.height(), constraints)
            };
            if !self.should_treat_max_block_size_as_none(node, available_space.block_size, constraints)
                && !style.max_height().is_auto()
            {
                used_block_size = used_block_size.min(self.calculate_inner_block_size(
                    node,
                    available_space,
                    style.max_height(),
                    constraints,
                ));
            }
            if !style.min_height().is_auto() {
                used_block_size = used_block_size.max(self.calculate_inner_block_size(
                    node,
                    available_space,
                    style.min_height(),
                    constraints,
                ));
            }
            // Only force a definite content box when the button's used block size exceeds its content block size, so a larger
            // preferred or minimum size has room to center within. A content-sized box stays indefinite, so an intrinsic
            // keyword does not resolve percentage-sized descendants.
            if used_block_size <= natural {
                return;
            }
            let used = self.used_mut(node);
            used.set_content_block_size(used_block_size);
            used.has_definite_block_size = true;
        }

        fn table_box_inside_wrapper(&self, wrapper: Node) -> Node {
            fn find(context: &SizingContext, parent: Node) -> Option<Node> {
                let mut child = context.first_child(parent);
                while !child.is_null() {
                    let facts = context.facts(child);
                    if facts.is_box && facts.display.is_table_inside() {
                        return Some(child);
                    }
                    if let Some(table) = find(context, child) {
                        return Some(table);
                    }
                    child = context.next_sibling(child);
                }
                None
            }

            find(self, wrapper).expect("table wrapper must contain a table box")
        }

        fn create_measurement_used_values(
            measurement: &mut MeasurementState,
            node: Node,
            constraints: FfiContainingBlockConstraints,
        ) -> *mut UsedValuesCore {
            let callbacks = *measurement.callbacks();
            let used = measurement
                .state_mut()
                .create_used_values(&callbacks, node, constraints);
            assert!(!used.is_null());
            used
        }

        // 17.5.2 Table width algorithms: the 'table-layout' property
        // https://www.w3.org/TR/CSS22/tables.html#width-layout
        pub(crate) fn compute_table_box_inline_size_inside_wrapper(
            &self,
            wrapper: Node,
            available_space: AvailableSpace,
            table_wrapper_constraints: FfiContainingBlockConstraints,
            table_wrapper_containing_block_inline_size: Option<CssPixels>,
            table_wrapper_inline_size_mode: TableWrapperInlineSizeMode,
        ) -> CssPixels {
            // CSS 2 says the table wrapper inline size is the border-edge inline size of the table grid box inside it.

            let style = self.style(wrapper);
            let containing_block_inline_size = table_wrapper_containing_block_inline_size
                .unwrap_or_else(|| available_space.inline_size.to_px_or_zero());

            // If 'margin-left', or 'margin-right' are computed as 'auto', their used value is '0'.
            let margin_left = style.margin_left().to_px(containing_block_inline_size);
            let margin_right = style.margin_right().to_px(containing_block_inline_size);

            // table-wrapper can't have borders or paddings but it might have margin taken from table-root.
            let available_inline_size = containing_block_inline_size - margin_left - margin_right;
            let table_box = self.table_box_inside_wrapper(wrapper);

            let mut measurement = MeasurementState::create(self.callbacks, wrapper, table_wrapper_constraints);

            // The table wrapper is invisible to percentage resolution, so the table box gets the
            // wrapper's constraints unchanged. Callers measuring a table wrapper for grid alignment
            // pass the grid-area inline size as the wrapper's percentage basis.
            let table_constraints = table_wrapper_constraints;
            let table_used = Self::create_measurement_used_values(&mut measurement, table_box, table_constraints);
            let table_style = self.style(table_box);
            // SAFETY: This is the newly allocated table entry.
            unsafe {
                (*table_used).border_left = table_style.border_left_width;
                (*table_used).border_right = table_style.border_right_width;
                (*table_used).padding_left = table_style.padding_left().to_px(containing_block_inline_size);
                (*table_used).padding_right = table_style.padding_right().to_px(containing_block_inline_size);
            }

            let mut context = super::create_formatting_context(
                measurement.rust_state(),
                table_box,
                std::ptr::null_mut(),
                super::FfiFormattingContextType::Table as u8,
                LAYOUT_MODE_INTRINSIC_SIZING,
                false,
                *measurement.callbacks(),
            );
            // SAFETY: The table entry remains live for the measurement host's lifetime.
            let table_available = unsafe { (*table_used).available_inner_space_or_constraints_from(available_space) };
            super::table::run_until_inline_size_calculation(
                &mut context,
                FfiLayoutInput {
                    available_space: table_available,
                    containing_block_constraints: table_constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
                true,
            );

            // SAFETY: The table entry remains live for the measurement host's lifetime.
            let table_used_inline_size = unsafe { (*table_used).border_box_inline_size(false) };
            if table_wrapper_inline_size_mode == TableWrapperInlineSizeMode::UseTableUsedInlineSizeIfNotAuto
                && !table_style.width().is_auto()
            {
                return table_used_inline_size;
            }
            if available_space.inline_size.is_definite() {
                table_used_inline_size.min(available_inline_size)
            } else {
                table_used_inline_size
            }
        }

        // 17.5.3 Table height algorithms
        // https://www.w3.org/TR/CSS22/tables.html#height-layout
        pub(crate) fn compute_table_box_block_size_inside_wrapper(
            &self,
            wrapper: Node,
            available_space: AvailableSpace,
            table_wrapper_constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // The table wrapper block size should equal the block size of the table box it contains.

            let style = self.style(wrapper);
            let containing_block_inline_size = available_space.inline_size.to_px_or_zero();
            let containing_block_block_size = available_space.block_size.to_px_or_zero();

            // If 'margin-top', or 'margin-bottom' are computed as 'auto', their used value is '0'.
            let margin_top = style.margin_top().to_px(containing_block_inline_size);
            let margin_bottom = style.margin_bottom().to_px(containing_block_inline_size);

            // table-wrapper can't have borders or paddings but it might have margin taken from table-root.
            let available_block_size = containing_block_block_size - margin_top - margin_bottom;
            let table_box = self.table_box_inside_wrapper(wrapper);

            let measurement = MeasurementState::create(self.callbacks, wrapper, table_wrapper_constraints);
            measurement.run_with_layout_mode(
                wrapper,
                LAYOUT_MODE_INTRINSIC_SIZING,
                FfiLayoutInput {
                    available_space: self
                        .used(wrapper)
                        .available_inner_space_or_constraints_from(available_space),
                    containing_block_constraints: table_wrapper_constraints,
                    has_content_box_position_in_bfc_root: false,
                    content_box_position_in_bfc_root: Default::default(),
                    has_table_grid_min_border_box_block_size: false,
                    table_grid_min_border_box_block_size: CssPixels::default(),
                },
            );

            let table_used = state_mut(measurement.rust_state()).used_values(measurement.callbacks(), table_box);
            // SAFETY: The table entry remains live for the measurement host's lifetime.
            let table_used_block_size =
                unsafe { (*table_used).border_box_block_size((*table_used).uses_collapsing_borders_model) };
            if available_space.block_size.is_definite() {
                table_used_block_size.min(available_block_size)
            } else {
                table_used_block_size
            }
        }

        pub(crate) fn calculate_fit_content_size(
            &self,
            node: Node,
            axis: FfiFlexAxis,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            // https://drafts.csswg.org/css-sizing-3/#fit-content-size
            match axis {
                FfiFlexAxis::Inline => {
                    // If the available space in a given axis is definite, equal to clamp(min-content size, stretch-fit size,
                    // max-content size) (i.e. max(min-content size, min(max-content size, stretch-fit size))).
                    if available_space.inline_size.is_definite() {
                        let stretch = self.calculate_stretch_fit_inline_size(node, available_space.inline_size);
                        let max_content = self.calculate_max_content_inline_size(node, constraints);
                        if max_content <= stretch {
                            return max_content;
                        }
                        return self.calculate_min_content_inline_size(node, constraints).max(stretch);
                    }
                    // When sizing under a min-content constraint, equal to the min-content size.
                    if available_space.inline_size.is_min_content() {
                        return self.calculate_min_content_inline_size(node, constraints);
                    }
                    // Otherwise, equal to the max-content size in that axis.
                    self.calculate_max_content_inline_size(node, constraints)
                }
                FfiFlexAxis::Block => {
                    let inline_size = available_space.inline_size.to_px_or_zero();
                    // https://drafts.csswg.org/css-sizing-3/#fit-content-size
                    // If the available space in a given axis is definite,
                    // equal to clamp(min-content size, stretch-fit size, max-content size)
                    // (i.e. max(min-content size, min(max-content size, stretch-fit size))).
                    if available_space.block_size.is_definite() {
                        let stretch = self.calculate_stretch_fit_block_size(node, available_space.block_size);
                        let max_content = self.calculate_max_content_block_size(node, inline_size, constraints);
                        if max_content <= stretch {
                            return max_content;
                        }
                        return self
                            .calculate_min_content_block_size(node, inline_size, constraints)
                            .max(stretch);
                    }
                    // When sizing under a min-content constraint, equal to the min-content size.
                    if available_space.block_size.is_min_content() {
                        return self.calculate_min_content_block_size(node, inline_size, constraints);
                    }
                    // Otherwise, equal to the max-content size in that axis.
                    self.calculate_max_content_block_size(node, inline_size, constraints)
                }
            }
        }

        pub(crate) fn calculate_inner_inline_size(
            &self,
            node: Node,
            available: AvailableSize,
            preferred_size: FfiSizeValue,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            assert!(!preferred_size.is_auto());
            let basis = if preferred_size.contains_percentage {
                if constraints.has_percentage_basis_inline_size {
                    constraints.percentage_basis_inline_size
                } else {
                    available.to_px_or_zero()
                }
            } else {
                available.to_px_or_zero()
            };
            if preferred_size.is_fit_content() {
                return self.calculate_fit_content_size(
                    node,
                    FfiFlexAxis::Inline,
                    AvailableSpace {
                        inline_size: available,
                        block_size: AvailableSize::indefinite(),
                    },
                    constraints,
                );
            }
            if preferred_size.is_max_content() {
                return self.calculate_max_content_inline_size(node, constraints);
            }
            if preferred_size.is_min_content() {
                return self.calculate_min_content_inline_size(node, constraints);
            }
            let value = preferred_size.to_px(basis);
            let style = self.style(node);
            if style.box_sizing == BOX_SIZING_BORDER_BOX {
                let used = self.used(node);
                return subtract_border_box_adjustment(
                    value,
                    style.border_left_width,
                    used.padding_left,
                    style.border_right_width,
                    used.padding_right,
                );
            }
            value
        }

        pub(crate) fn calculate_inner_block_size(
            &self,
            node: Node,
            available_space: AvailableSpace,
            preferred_size: FfiSizeValue,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            if preferred_size.is_auto() && self.facts(node).has_preferred_aspect_ratio {
                return self.content_block_size_from_aspect_ratio(node, self.used(node).content_inline_size);
            }
            assert!(!preferred_size.is_auto());
            if preferred_size.is_fit_content() {
                return self.calculate_fit_content_size(node, FfiFlexAxis::Block, available_space, constraints);
            }
            if preferred_size.is_max_content() {
                return self.calculate_max_content_block_size(
                    node,
                    available_space.inline_size.to_px_or_zero(),
                    constraints,
                );
            }
            if preferred_size.is_min_content() {
                return self.calculate_min_content_block_size(
                    node,
                    available_space.inline_size.to_px_or_zero(),
                    constraints,
                );
            }

            let mut basis = available_space.block_size.to_px_or_zero();
            // NOTE: Percentage heights are resolved against the containing block's used height,
            //       not the available space height. The containing block's height must be definite
            //       for percentage resolution to work (otherwise should_treat_block_size_as_auto
            //       should have returned true and we wouldn't be here).
            // NOTE: We only do this when available space height is indefinite. If it's definite,
            //       we trust that the caller has set it up correctly (e.g., grid/flex items get
            //       their cell/area size as available space).
            if preferred_size.contains_percentage && available_space.block_size.is_indefinite() {
                // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
                // NOTE: Flex/grid items resolve percentage heights against their container, not via quirk.
                let facts = self.facts(node);
                let parent = self.parent(node);
                let parent_is_flex_or_grid = if parent.is_null() {
                    false
                } else {
                    let display = self.facts(parent).display;
                    display.is_flex_inside() || display.is_grid_inside()
                };
                if facts.document_in_quirks_mode
                    && !facts.is_anonymous
                    && !parent_is_flex_or_grid
                    && !facts.is_in_user_agent_shadow_tree
                {
                    basis = if constraints.has_quirks_mode_percentage_basis_block_size {
                        constraints.quirks_mode_percentage_basis_block_size
                    } else {
                        CssPixels::default()
                    };
                } else if constraints.has_percentage_basis_block_size {
                    basis = constraints.percentage_basis_block_size;
                }
            }
            let value = preferred_size.to_px(basis);
            let style = self.style(node);
            if style.box_sizing == BOX_SIZING_BORDER_BOX {
                let used = self.used(node);
                return subtract_border_box_adjustment(
                    value,
                    style.border_top_width,
                    used.padding_top,
                    style.border_bottom_width,
                    used.padding_bottom,
                );
            }
            value
        }

        pub(crate) fn calculate_inner_size_for_property(
            &self,
            node: Node,
            axis: FfiFlexAxis,
            property: FfiFlexSizeProperty,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            let style = self.style(node);
            let size = match property {
                FfiFlexSizeProperty::Width => style.width(),
                FfiFlexSizeProperty::Height => style.height(),
                FfiFlexSizeProperty::MinWidth => style.min_width(),
                FfiFlexSizeProperty::MinHeight => style.min_height(),
                FfiFlexSizeProperty::MaxWidth => style.max_width(),
                FfiFlexSizeProperty::MaxHeight => style.max_height(),
                FfiFlexSizeProperty::FlexBasis => style.flex_basis(),
            };
            match axis {
                FfiFlexAxis::Inline => {
                    self.calculate_inner_inline_size(node, available_space.inline_size, size, constraints)
                }
                FfiFlexAxis::Block => self.calculate_inner_block_size(node, available_space, size, constraints),
            }
        }

        pub(crate) fn calculate_inner_inline_width(
            &self,
            node: Node,
            available: AvailableSize,
            constraints: FfiContainingBlockConstraints,
        ) -> CssPixels {
            self.calculate_inner_inline_size(node, available, self.style(node).width(), constraints)
        }

        pub(crate) fn should_treat_size_as_auto(
            &self,
            node: Node,
            axis: FfiFlexAxis,
            available_space: AvailableSpace,
            constraints: FfiContainingBlockConstraints,
        ) -> bool {
            match axis {
                FfiFlexAxis::Inline => self.should_treat_inline_size_as_auto(node, available_space),
                FfiFlexAxis::Block => self.should_treat_block_size_as_auto(node, available_space, constraints),
            }
        }
    }
}

#[path = "replaced_with_children_formatting_context.rs"]
mod replaced_with_children;
#[path = "svg_formatting_context.rs"]
pub(crate) mod svg;
#[path = "table_formatting_context.rs"]
pub(crate) mod table;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaselineSet {
    First,
    Last,
}

fn navigate_for_baseline(
    callbacks: &FfiLayoutFcCallbacks,
    callback: crate::box_facts::FfiLayoutNavCallback,
    node: *mut c_void,
) -> *mut c_void {
    bump(FfiOp::NavigationCallback);
    // SAFETY: Navigation is synchronous and the host owns every node.
    unsafe { callback(callbacks.navigation.context, node) }
}

pub(crate) fn place_child(
    state: *mut c_void,
    callbacks: &FfiLayoutFcCallbacks,
    node: *mut c_void,
    offset: FfiCssPixelPoint,
) {
    let used = state_mut(state).used_values(callbacks, node);
    // SAFETY: The Rust layout state owns this stable entry and placement is
    // assigned once per layout run.
    unsafe {
        assert!(!(*used).has_content_offset);
        (*used).has_content_offset = true;
        (*used).content_offset = offset;
    }
}

pub(crate) fn register_contained_abspos_child(
    state: *mut c_void,
    callbacks: &FfiLayoutFcCallbacks,
    child: *mut c_void,
    static_position_rect: FfiStaticPositionRect,
) {
    let mut target = navigate_for_baseline(callbacks, callbacks.navigation.containing_block, child);
    if target.is_null() {
        return;
    }
    loop {
        let containing_block = navigate_for_baseline(callbacks, callbacks.navigation.containing_block, target);
        let facts = state_mut(state).box_facts(callbacks, target);
        if containing_block.is_null() || formatting_context_type_created_by_box(facts).is_some() {
            break;
        }
        target = containing_block;
    }
    let child_facts = state_mut(state).box_facts(callbacks, child);
    bump(FfiOp::AbsposRegister);
    state_mut(state).register_contained_abspos_child(target, child, child_facts.layout_index, static_position_rect);
}

pub(crate) fn box_baseline(
    state: *mut c_void,
    callbacks: &FfiLayoutFcCallbacks,
    box_: *mut c_void,
    mut baseline_set: BaselineSet,
) -> CssPixels {
    let facts = state_mut(state).box_facts(callbacks, box_);
    let style = state_mut(state).style_facts(callbacks, box_);
    let used_pointer = state_mut(state).used_values(callbacks, box_);
    // SAFETY: The Rust layout state owns this stable entry.
    let used = unsafe { &*used_pointer };
    let collapsed = used.uses_collapsing_borders_model;

    // https://drafts.csswg.org/css2/#propdef-vertical-align
    if facts.vertical_align_applies && style.vertical_align_is_keyword() {
        match style.vertical_align_keyword() {
            crate::css_enums::vertical_align::TOP => {
                // Top: Align the top of the aligned subtree with the top of the line box.
                return used.border_box_top(collapsed);
            }
            crate::css_enums::vertical_align::MIDDLE => {
                // Middle: Align the vertical midpoint of the box with the baseline of the parent box plus half the x-height of the parent.
                let containing_block = navigate_for_baseline(callbacks, callbacks.navigation.containing_block, box_);
                assert!(!containing_block.is_null());
                let containing_style = state_mut(state).style_facts(callbacks, containing_block);
                return used.margin_box_block_size(collapsed) / 2
                    + CssPixels::nearest_value_for_f32(containing_style.font_x_height() / 2.0);
            }
            crate::css_enums::vertical_align::BOTTOM => {
                // Bottom: Align the bottom of the aligned subtree with the bottom of the line box.
                return used.content_block_size + used.margin_box_top(collapsed);
            }
            crate::css_enums::vertical_align::TEXT_TOP => {
                // TextTop: Align the top of the box with the top of the parent's content area (see 10.6.1).
                return style.font_size;
            }
            crate::css_enums::vertical_align::TEXT_BOTTOM => {
                // TextBottom: Align the bottom of the box with the bottom of the parent's content area (see 10.6.1).
                let containing_block = navigate_for_baseline(callbacks, callbacks.navigation.containing_block, box_);
                assert!(!containing_block.is_null());
                let containing_style = state_mut(state).style_facts(callbacks, containing_block);
                return used.margin_box_block_size(collapsed)
                    - CssPixels::nearest_value_for_f32(containing_style.font_descent());
            }
            _ => {}
        }
    }

    // https://drafts.csswg.org/css-inline-3/#baseline-source
    // auto: Specifies last-baseline alignment for inline-block, first-baseline alignment for everything else.
    // NB: Callers ask an inline-level box for its last baseline set, since that is what CSS2's inline-block rule below
    //     describes; inline-level flex and grid containers participate with their first baseline set instead.
    let display = facts.display;
    let is_flex_or_grid_container = display.is_flex_inside() || display.is_grid_inside();
    if display.is_inline_outside() && is_flex_or_grid_container {
        baseline_set = BaselineSet::First;
    }

    // https://drafts.csswg.org/css2/#propdef-vertical-align
    // The baseline of an 'inline-block' is the baseline of its last line box in the normal flow, unless it has either
    // no in-flow line boxes or if its 'overflow' property has a computed value other than 'visible', in which case the
    // baseline is the bottom margin edge.
    // https://drafts.csswg.org/css-align-3/#baseline-rules
    // CSS Align restates this overflow exception as only applying to the last baseline set: "for legacy reasons if its
    // baseline-source is auto (the initial value) a block-level or inline-level block container that is a scroll
    // container always has a last baseline set, whose baselines all correspond to its block-end margin edge". First
    // baseline sets always derive from content; so do flex and grid containers, which are not block containers.
    // FIXME: Per CSS Align, a scroll container's content-derived baseline position should be clamped to its border
    //        edge.
    let has_visible_overflow = style.overflow_x == crate::css_enums::overflow::VISIBLE
        && style.overflow_y == crate::css_enums::overflow::VISIBLE;
    let derive_baseline_from_content =
        baseline_set == BaselineSet::First || is_flex_or_grid_container || has_visible_overflow;

    // AD-HOC: We also use the content-derived baseline for <input> elements with block children. Per the HTML spec,
    //         inputs have `overflow: clip !important`, so CSS2 says to use bottom margin edge. However, the internal
    //         shadow tree baseline should determine the control's baseline for proper alignment with adjacent text.
    //         https://html.spec.whatwg.org/multipage/rendering.html#form-controls
    let input_derives_from_children = facts.is_html_input_element && !facts.children_are_inline;

    let content_baseline = match baseline_set {
        BaselineSet::First if used.has_first_baseline => Some(used.first_baseline),
        BaselineSet::Last if used.has_last_baseline => Some(used.last_baseline),
        _ => None,
    };
    if let Some(content_baseline) = content_baseline
        && (derive_baseline_from_content || input_derives_from_children)
    {
        return used.margin_box_top(collapsed) + content_baseline;
    }

    // If the box has no baseline set, the bottom margin edge of the box is used.
    used.margin_box_block_size(collapsed)
}

pub(crate) fn compute_and_store_baselines(
    state: *mut c_void,
    callbacks: &FfiLayoutFcCallbacks,
    box_: *mut c_void,
    inhibits_floating: bool,
) {
    let used_pointer = state_mut(state).used_values(callbacks, box_);
    // NOTE: This may run more than once for the same UsedValues (e.g. table cells are laid out twice),
    //       so reset both baselines before deriving them anew.
    // SAFETY: The Rust layout state owns this stable entry.
    unsafe {
        (*used_pointer).has_first_baseline = false;
        (*used_pointer).has_last_baseline = false;
    }

    let facts = state_mut(state).box_facts(callbacks, box_);
    let line_count = state_mut(state)
        .line_data(facts.layout_index)
        .map_or(0, |data| data.line_boxes.len());
    if line_count > 0 {
        let baseline_for_line_box = |line_index: usize, baseline_set: BaselineSet| -> CssPixels {
            let (has_block_level_box, block_start, baseline, fragment_count, fragment_node) = {
                let line = &state_mut(state).line_data(facts.layout_index).unwrap().line_boxes[line_index];
                (
                    line.has_block_level_box,
                    line.physical_vertical_end() - line.block_length,
                    line.baseline,
                    line.fragments.len(),
                    line.fragments.first().map(|fragment| fragment.layout_node),
                )
            };
            if !has_block_level_box {
                return block_start + baseline;
            }

            assert_eq!(fragment_count, 1);
            let fragment_node = fragment_node.expect("block-level line must have one fragment");
            let block_child_state = state_mut(state).used_values(callbacks, fragment_node);
            // SAFETY: The Rust layout state owns this stable entry.
            let block_child_state = unsafe { &*block_child_state };
            let child_offset_from_margin_edge = block_child_state.content_offset.y
                - block_child_state.margin_box_top(block_child_state.uses_collapsing_borders_model);
            child_offset_from_margin_edge + box_baseline(state, callbacks, fragment_node, baseline_set)
        };

        let mut first_line_index = 0;
        while first_line_index < line_count {
            let is_empty =
                state_mut(state).line_data(facts.layout_index).unwrap().line_boxes[first_line_index].is_empty();
            if !is_empty {
                break;
            }
            first_line_index += 1;
        }
        if first_line_index == line_count {
            first_line_index = 0;
        }
        let first_baseline = baseline_for_line_box(first_line_index, BaselineSet::First);

        let mut last_line_index = line_count - 1;
        while last_line_index > 0 {
            let is_empty =
                state_mut(state).line_data(facts.layout_index).unwrap().line_boxes[last_line_index].is_empty();
            if !is_empty {
                break;
            }
            last_line_index -= 1;
        }
        let last_baseline = baseline_for_line_box(last_line_index, BaselineSet::Last);
        // SAFETY: The stable root entry remains live throughout this pass.
        unsafe {
            (*used_pointer).has_first_baseline = true;
            (*used_pointer).first_baseline = first_baseline;
            (*used_pointer).has_last_baseline = true;
            (*used_pointer).last_baseline = last_baseline;
        }
        return;
    }

    if navigate_for_baseline(callbacks, callbacks.navigation.first_child, box_).is_null() || facts.children_are_inline {
        return;
    }

    // Derive baselines from the first/last in-flow child that has a baseline set of its own.
    // https://drafts.csswg.org/css-flexbox-1/#flex-baselines
    // Otherwise, if the flex container has at least one flex item, the flex container's first/last main-axis baseline
    // set is generated from the alignment baseline of the startmost/endmost flex item.
    // https://drafts.csswg.org/css-grid-1/#grid-baselines
    // Otherwise, the grid container's first (last) baseline set is generated from the alignment baseline of the first
    // (last) grid item in row-major grid order.
    // FIXME: This does not yet select the spec-defined startmost/endmost flex item, or the first/last grid item in
    //        row-major grid order.
    let baseline_from_children = |baseline_set: BaselineSet| -> Option<CssPixels> {
        let mut children = Vec::new();
        let mut child = navigate_for_baseline(callbacks, callbacks.navigation.first_child, box_);
        while !child.is_null() {
            children.push(child);
            child = navigate_for_baseline(callbacks, callbacks.navigation.next_sibling, child);
        }
        if baseline_set == BaselineSet::Last {
            children.reverse();
        }
        for child in children {
            let child_facts = state_mut(state).box_facts(callbacks, child);
            if !child_facts.is_box {
                continue;
            }
            if child_facts.is_absolutely_positioned || (!inhibits_floating && child_facts.is_floating) {
                continue;
            }
            let child_state = state_mut(state).try_used_values(callbacks, child);
            if child_state.is_null() {
                continue;
            }
            // SAFETY: A non-null pointer is a stable state entry.
            let child_state = unsafe { &*child_state };
            match baseline_set {
                BaselineSet::First if child_state.has_first_baseline => {}
                BaselineSet::Last if child_state.has_last_baseline => {}
                _ => continue,
            }
            let child_offset_from_margin_edge =
                child_state.content_offset.y - child_state.margin_box_top(child_state.uses_collapsing_borders_model);
            return Some(child_offset_from_margin_edge + box_baseline(state, callbacks, child, baseline_set));
        }
        None
    };
    let first_baseline = baseline_from_children(BaselineSet::First);
    let last_baseline = baseline_from_children(BaselineSet::Last);
    // SAFETY: The stable root entry remains live throughout this pass.
    unsafe {
        if let Some(value) = first_baseline {
            (*used_pointer).has_first_baseline = true;
            (*used_pointer).first_baseline = value;
        }
        if let Some(value) = last_baseline {
            (*used_pointer).has_last_baseline = true;
            (*used_pointer).last_baseline = value;
        }
    }
}

const NO_FORMATTING_CONTEXT: u8 = u8::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiFormattingContextType {
    Block,
    Inline,
    Flex,
    Grid,
    Table,
    Svg,
    ReplacedWithChildren,
    AbsposReplay,
    InternalReplaced,
    InternalDummy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiBorderData {
    pub color: u32,
    pub line_style: u8,
    pub width: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiBorderDataWithElementKind {
    pub border_data: FfiBorderData,
    pub element_kind: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiBordersData {
    pub top: FfiBorderDataWithElementKind,
    pub right: FfiBorderDataWithElementKind,
    pub bottom: FfiBorderDataWithElementKind,
    pub left: FfiBorderDataWithElementKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiTableBoxFacts {
    pub cell_column_span: usize,
    pub cell_row_span: usize,
    pub column_span: u32,
    pub raw_column_span: u32,
    pub border_top_color: u32,
    pub border_right_color: u32,
    pub border_bottom_color: u32,
    pub border_left_color: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiChildLayoutResult {
    pub automatic_content_inline_size: CssPixels,
    pub automatic_content_block_size: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MeasuredCellContent {
    pub content_block_size: CssPixels,
    pub first_baseline: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiIntrinsicSizeCacheKey {
    pub has_measured_at_inline_size: bool,
    pub measured_at_inline_size: CssPixels,
    pub has_percentage_basis_inline_size: bool,
    pub percentage_basis_inline_size: CssPixels,
    pub has_percentage_basis_block_size: bool,
    pub percentage_basis_block_size: CssPixels,
    pub has_quirks_mode_percentage_basis_block_size: bool,
    pub quirks_mode_percentage_basis_block_size: CssPixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiIntrinsicSizeCacheKind {
    MinContentInline,
    MaxContentInline,
    MinContentBlock,
    MaxContentBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiFlexAxis {
    Inline,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiFlexSizeProperty {
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    FlexBasis,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiFlexLayoutItemRect {
    pub x: CssPixels,
    pub y: CssPixels,
    pub width: CssPixels,
    pub height: CssPixels,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFlexLayoutItem {
    pub node: *mut c_void,
    pub rect: FfiFlexLayoutItemRect,
    pub main_base_size: CssPixels,
    pub main_delta_size: CssPixels,
    pub main_min_size: CssPixels,
    pub main_max_size: CssPixels,
    pub cross_min_size: CssPixels,
    pub cross_max_size: CssPixels,
    pub clamp_state: u8,
    pub flex_grow: f64,
    pub flex_shrink: f64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFlexLayoutLine {
    pub growth_state: u8,
    pub cross_start: CssPixels,
    pub cross_size: CssPixels,
    pub items: *const FfiFlexLayoutItem,
    pub item_count: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiFlexLayoutData {
    pub align_content: u8,
    pub align_items: u8,
    pub flex_direction: u8,
    pub flex_wrap: u8,
    pub justify_content: u8,
    pub main_axis_direction: u8,
    pub cross_axis_direction: u8,
    pub lines: *const FfiFlexLayoutLine,
    pub line_count: usize,
}

pub type FfiBuildBoxFactsCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> FfiLayoutBoxFacts;
pub type FfiBuildTableBoxFactsCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> FfiTableBoxFacts;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiLayoutFcCallbacks {
    pub context: *mut c_void,
    pub navigation: FfiLayoutNavCallbacks,
    pub static_position_containing_block: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    pub inline_containing_block: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    pub non_anonymous_containing_block: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    pub node_is_ancestor: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
    pub dom_node_is_inclusive_ancestor: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
    pub is_table_cell: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub needs_inset_resolution: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub decode_style_field: crate::style_facts::FfiDecodeStyleFieldCallback,
    pub build_box_facts: FfiBuildBoxFactsCallback,
    pub build_text_facts: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        bool,
        bool,
        bool,
        *mut inline::iterator::text::FfiTextNodeFacts,
    ) -> bool,
    pub release_text_facts: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub text_may_require_bidi_processing: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub document_cursor_is_on_node: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub shape_text: unsafe extern "C" fn(
        *mut c_void,
        inline::iterator::text::FfiShapeRequest,
    ) -> inline::iterator::text::FfiShapedRunView,
    pub release_shaped_run: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub font_metrics:
        unsafe extern "C" fn(*mut c_void, *const c_void, *mut inline::iterator::text::FfiFontPixelMetrics),
    pub font_glyph_width: unsafe extern "C" fn(*mut c_void, *const c_void, u32) -> f32,
    pub font_glyph_id: unsafe extern "C" fn(*mut c_void, *const c_void, u32) -> u32,
    pub build_table_box_facts: FfiBuildTableBoxFactsCallback,
    pub build_grid_facts: unsafe extern "C" fn(*mut c_void, *mut c_void) -> grid::facts::FfiGridStyleFacts,
    pub release_grid_facts_snapshot: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub build_svg_facts: unsafe extern "C" fn(*mut c_void, *mut c_void) -> svg::FfiSvgElementFacts,
    pub read_paintable_geometry: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut crate::layout_state::FfiPaintableGeometry,
    ) -> bool,
    pub read_paintable_svg_transforms:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut svg::FfiSvgComputedTransforms) -> bool,
    pub compute_svg_path:
        unsafe extern "C" fn(*mut c_void, *mut c_void, svg::FfiSvgPathRequest) -> svg::FfiSvgPathResult,
    pub release_svg_path: crate::layout_state::ReleaseRetainedLayoutHandle,
    pub svg_image_bounding_box:
        unsafe extern "C" fn(*mut c_void, *mut c_void, CssPixels, CssPixels) -> svg::FfiFloatRect,
    pub get_saved_abspos_layout_inputs:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut crate::layout_state::FfiAbsposLayoutInputs) -> bool,
    pub anchor_lookup:
        unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *const *mut c_void, usize, *mut *mut c_void) -> bool,
    pub build_anchor_function_facts: unsafe extern "C" fn(*mut c_void, *const c_void) -> abspos::FfiAnchorFunctionFacts,
    pub anchor_function_fallback: unsafe extern "C" fn(*mut c_void, *const c_void) -> abspos::FfiAnchorFallbackFacts,
    pub set_resolved_anchor_insets: unsafe extern "C" fn(*mut c_void, *mut c_void, abspos::FfiResolvedAnchorInsets),
    pub set_default_scroll_shift: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, bool, bool),
    pub can_skip_is_anonymous_text_run: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub set_flex_item: unsafe extern "C" fn(*mut c_void, *mut c_void, bool),
    pub set_grid_item: unsafe extern "C" fn(*mut c_void, *mut c_void, bool),
    pub intrinsic_size_cache_get: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        FfiIntrinsicSizeCacheKind,
        FfiIntrinsicSizeCacheKey,
        *mut CssPixels,
    ) -> bool,
    pub intrinsic_size_cache_put:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiIntrinsicSizeCacheKind, FfiIntrinsicSizeCacheKey, CssPixels),
    pub create_flex_layout_data: unsafe extern "C" fn(*mut c_void, *const FfiFlexLayoutData) -> *mut c_void,
    pub release_flex_layout_data: crate::layout_state::ReleaseRetainedLayoutHandle,
    pub create_grid_layout_data:
        unsafe extern "C" fn(*mut c_void, *const grid::facts::FfiGridLayoutData) -> *mut c_void,
    pub release_grid_layout_data: crate::layout_state::ReleaseRetainedLayoutHandle,
    pub create_used_grid_tracks: unsafe extern "C" fn(
        *mut c_void,
        *const grid::facts::FfiUsedGridTrackList,
        *const grid::facts::FfiUsedGridTrackList,
    ) -> *mut c_void,
    pub release_used_grid_tracks: crate::layout_state::ReleaseRetainedLayoutHandle,
}

pub(crate) struct FormattingContextInstance {
    state: *mut c_void,
    box_: *mut c_void,
    parent_rust_fc: *mut c_void,
    fc_type: u8,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    block_context: Option<Box<block::BlockFormattingContext>>,
    grid_context: Option<Box<grid::GridFormattingContext>>,
    svg_context: Option<Box<svg::SvgFormattingContext>>,
    should_collect_devtools_layout_data: bool,
    automatic_content_inline_size: CssPixels,
    automatic_content_block_size: CssPixels,
    child_contexts: HashMap<usize, Box<FormattingContextInstance>>,
}

fn instance_mut(fc: *mut c_void) -> &'static mut FormattingContextInstance {
    assert!(!fc.is_null());
    // SAFETY: Formatting-context pointers refer to stable Box allocations
    // owned by either a top-level pass or their parent context.
    unsafe { &mut *fc.cast::<FormattingContextInstance>() }
}

pub(crate) fn instance_ref(fc: *mut c_void) -> &'static FormattingContextInstance {
    assert!(!fc.is_null());
    // SAFETY: Formatting-context handles remain live while their owning
    // top-level pass or parent context is active.
    unsafe { &*fc.cast::<FormattingContextInstance>() }
}

pub(crate) fn block_context_for_fc(fc: *mut c_void) -> Option<&'static block::BlockFormattingContext> {
    if fc.is_null() {
        return None;
    }
    let instance = instance_ref(fc);
    if instance.fc_type != FfiFormattingContextType::Block as u8 {
        return None;
    }
    instance.block_context.as_deref()
}

pub(crate) fn formatting_context_type_created_by_box(facts: FfiLayoutBoxFacts) -> Option<FfiFormattingContextType> {
    if facts.is_svg_svg_box {
        return Some(FfiFormattingContextType::Svg);
    }
    if facts.is_replaced_box_with_children {
        return Some(FfiFormattingContextType::ReplacedWithChildren);
    }
    if facts.is_replaced_box {
        return Some(FfiFormattingContextType::InternalReplaced);
    }
    if !facts.can_have_children {
        return None;
    }

    let display = facts.display;
    if facts.has_replaced_element_table_display_adjustment {
        return Some(if facts.is_block_container {
            FfiFormattingContextType::Block
        } else {
            FfiFormattingContextType::InternalReplaced
        });
    }
    if display.is_flex_inside() {
        return Some(FfiFormattingContextType::Flex);
    }
    if display.is_table_inside() {
        return Some(FfiFormattingContextType::Table);
    }
    if display.is_grid_inside() {
        return Some(FfiFormattingContextType::Grid);
    }
    if display.is_math_inside() {
        return Some(FfiFormattingContextType::Block);
    }
    if facts.creates_block_formatting_context {
        return Some(FfiFormattingContextType::Block);
    }
    if facts.children_are_inline
        || display.is_table_column()
        || display.is_table_row_group()
        || display.is_table_header_group()
        || display.is_table_footer_group()
        || display.is_table_row()
        || display.is_table_column_group()
    {
        return None;
    }
    if !display.is_flow_inside() {
        return Some(FfiFormattingContextType::InternalDummy);
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_formatting_context_type_for_box(facts: FfiLayoutBoxFacts) -> u8 {
    abort_on_panic(|| {
        bump(FfiOp::FcTypeDecision);
        formatting_context_type_created_by_box(facts)
            .map(|type_| type_ as u8)
            .unwrap_or(NO_FORMATTING_CONTEXT)
    })
}

fn create_formatting_context(
    state: *mut c_void,
    box_: *mut c_void,
    parent_rust_fc: *mut c_void,
    fc_type: u8,
    layout_mode: u8,
    should_collect_devtools_layout_data: bool,
    callbacks: FfiLayoutFcCallbacks,
) -> Box<FormattingContextInstance> {
    bump(FfiOp::FcCreate);
    assert!(!state.is_null());
    assert!(!box_.is_null());
    let root_facts = state_mut(state).box_facts(&callbacks, box_);
    if fc_type == FfiFormattingContextType::Block as u8
        && state_mut(state).mark_bfc_root_fact_builds_excluded(root_facts.layout_index)
    {
        let _ = state_mut(state).style_facts(&callbacks, box_);
        // Root setup is BFC implementation overhead rather than a newly
        // visited box. Preserve the pre-flip counter semantics while
        // retaining both facts in the shared per-pass caches.
        crate::ffi_stats::exclude_bfc_root_fact_builds();
    }

    let mut instance = Box::new(FormattingContextInstance {
        state,
        box_,
        parent_rust_fc,
        fc_type,
        layout_mode,
        callbacks,
        block_context: None,
        grid_context: None,
        svg_context: None,
        should_collect_devtools_layout_data,
        automatic_content_inline_size: CssPixels::default(),
        automatic_content_block_size: CssPixels::default(),
        child_contexts: HashMap::new(),
    });
    let rust_context_handle = (&raw mut *instance).cast();
    if fc_type == FfiFormattingContextType::Block as u8 {
        instance.block_context = Some(Box::new(block::BlockFormattingContext::new(
            state,
            box_,
            parent_rust_fc,
            rust_context_handle,
            layout_mode,
            callbacks,
        )));
    }
    if fc_type == FfiFormattingContextType::Grid as u8 {
        instance.grid_context = Some(Box::new(grid::GridFormattingContext::new(
            state,
            box_,
            parent_rust_fc,
            rust_context_handle,
            layout_mode,
            callbacks,
            should_collect_devtools_layout_data,
        )));
    }
    if fc_type == FfiFormattingContextType::Svg as u8 {
        instance.svg_context = Some(Box::new(svg::SvgFormattingContext::new(
            state,
            box_,
            rust_context_handle,
            layout_mode,
            callbacks,
        )));
    }
    instance
}

fn navigate(
    instance: &FormattingContextInstance,
    callback: crate::box_facts::FfiLayoutNavCallback,
    node: *mut c_void,
) -> *mut c_void {
    bump(FfiOp::NavigationCallback);
    // SAFETY: Navigation is synchronous and the host owns every node.
    unsafe { callback(instance.callbacks.navigation.context, node) }
}

fn register_table_abspos_descendants(instance: &mut FormattingContextInstance, parent: *mut c_void) {
    let mut child = navigate(instance, instance.callbacks.navigation.first_child, parent);
    while !child.is_null() {
        let next = navigate(instance, instance.callbacks.navigation.next_sibling, child);
        let facts = state_mut(instance.state).box_facts(&instance.callbacks, child);
        if facts.is_box {
            if facts.is_absolutely_positioned {
                register_contained_abspos_child(
                    instance.state,
                    &instance.callbacks,
                    child,
                    FfiStaticPositionRect {
                        rect: Default::default(),
                        inline_alignment: FfiStaticPositionAlignment::Start,
                        block_alignment: FfiStaticPositionAlignment::Start,
                        alignment_derives_from_own_computed_values: false,
                    },
                );
            }
            if formatting_context_type_created_by_box(facts).is_none() {
                register_table_abspos_descendants(instance, child);
            }
        } else {
            register_table_abspos_descendants(instance, child);
        }
        child = next;
    }
}

fn parent_did_dimension(instance: &mut FormattingContextInstance) {
    if instance.fc_type == FfiFormattingContextType::Block as u8 {
        let context = instance.block_context.as_deref().unwrap();
        context.parent_context_did_dimension_child_root_box();
        // The table formatting context handles cell abspos layout after vertical alignment.
        // SAFETY: This reads the live context box's display.
        let is_table_cell = unsafe { (instance.callbacks.is_table_cell)(instance.callbacks.context, instance.box_) };
        if !is_table_cell {
            let box_ = instance.box_;
            abspos::layout_children_for_instance(instance, box_);
        }
        return;
    }
    if instance.layout_mode != 0 {
        return;
    }
    match instance.fc_type {
        type_ if type_ == FfiFormattingContextType::Table as u8 => {
            register_table_abspos_descendants(instance, instance.box_);
        }
        type_ if type_ == FfiFormattingContextType::Flex as u8 => {
            flex::parent_did_dimension(instance);
        }
        type_ if type_ == FfiFormattingContextType::Grid as u8 => {
            instance.grid_context.as_ref().unwrap().parent_did_dimension();
        }
        type_ if type_ == FfiFormattingContextType::Svg as u8 => {}
        type_ if type_ == FfiFormattingContextType::ReplacedWithChildren as u8 => {
            replaced_with_children::parent_did_dimension(instance);
            return;
        }
        type_
            if type_ == FfiFormattingContextType::InternalReplaced as u8
                || type_ == FfiFormattingContextType::InternalDummy as u8 =>
        {
            return;
        }
        _ => panic!("no Rust parent-dimension implementation for this formatting context"),
    }
    // SAFETY: This reads the live context box's display without
    // populating the Rust facts cache solely for this guard.
    let is_table_cell = unsafe { (instance.callbacks.is_table_cell)(instance.callbacks.context, instance.box_) };
    if !is_table_cell {
        let box_ = instance.box_;
        abspos::layout_children_for_instance(instance, box_);
    }
}

impl Drop for FormattingContextInstance {
    fn drop(&mut self) {
        if self.fc_type != FfiFormattingContextType::Block as u8
            || self
                .block_context
                .as_deref()
                .unwrap()
                .was_notified_after_parent_dimensioned_root()
        {
            return;
        }
        // HACK: The parent formatting context never notified us after assigning dimensions to our root box.
        //       Pretend that it did anyway, to make sure absolutely positioned children get laid out.
        // FIXME: Get rid of this hack once parent contexts behave properly.
        self.block_context
            .as_deref()
            .unwrap()
            .parent_context_did_dimension_child_root_box();
        // The table formatting context handles cell abspos layout after vertical alignment.
        // SAFETY: This reads the live context box's display.
        let is_table_cell = unsafe { (self.callbacks.is_table_cell)(self.callbacks.context, self.box_) };
        if !is_table_cell {
            let box_ = self.box_;
            abspos::layout_children_for_instance(self, box_);
        }
    }
}

fn run_formatting_context(instance: &mut FormattingContextInstance, input: FfiLayoutInput) {
    bump(FfiOp::FcRun);
    if instance.fc_type == FfiFormattingContextType::Block as u8 {
        let context = instance.block_context.as_deref().unwrap();
        context.run(input);
        instance.automatic_content_inline_size = context.automatic_content_inline_size();
        instance.automatic_content_block_size = context.automatic_content_block_size();
        return;
    }
    match instance.fc_type {
        type_ if type_ == FfiFormattingContextType::Table as u8 => {
            table::run(instance, input);
        }
        type_ if type_ == FfiFormattingContextType::Flex as u8 => {
            flex::run(instance, input);
        }
        type_ if type_ == FfiFormattingContextType::Grid as u8 => {
            let (inline_size, block_size) = {
                let context = instance.grid_context.as_mut().unwrap();
                context.run(input);
                (
                    context.automatic_content_inline_size(),
                    context.automatic_content_block_size(),
                )
            };
            instance.automatic_content_inline_size = inline_size;
            instance.automatic_content_block_size = block_size;
        }
        type_ if type_ == FfiFormattingContextType::Svg as u8 => {
            svg::run(instance, input);
        }
        type_ if type_ == FfiFormattingContextType::ReplacedWithChildren as u8 => {
            replaced_with_children::run(instance, input);
        }
        type_
            if type_ == FfiFormattingContextType::InternalReplaced as u8
                || type_ == FfiFormattingContextType::InternalDummy as u8 =>
        {
            replaced_with_children::run_noop(instance);
        }
        _ => panic!("no Rust implementation for this formatting context"),
    }
}

pub(crate) fn layout_inside_child(
    parent_rust_fc: *mut c_void,
    child: *mut c_void,
    layout_mode: u8,
    input: FfiLayoutInput,
    force_independent_context_run: bool,
) -> Option<FfiChildLayoutResult> {
    assert!(!parent_rust_fc.is_null());
    let parent = instance_mut(parent_rust_fc);
    assert!(!parent.child_contexts.contains_key(&(child as usize)));

    let facts = state_mut(parent.state).box_facts(&parent.callbacks, child);
    let used = state_mut(parent.state).try_used_values(&parent.callbacks, child);
    if !force_independent_context_run
        && layout_mode == 1
        && !facts.is_inline
        && !used.is_null()
        // SAFETY: Non-null used-values entries are stable for the pass.
        && unsafe {
            (*used).inline_size_constraint == FfiSizeConstraint::None
                && (*used).block_size_constraint == FfiSizeConstraint::None
                && (*used).has_definite_inline_size()
                && (*used).has_definite_block_size()
        }
    {
        return None;
    }
    if !facts.can_have_children {
        return None;
    }

    let Some(fc_type) = formatting_context_type_created_by_box(facts) else {
        if !force_independent_context_run {
            // Preserve the former C++ layout_inside() behavior: when the box
            // does not establish an independent context, its contents stay in
            // the current context.
            run_formatting_context(parent, input);
        }
        return None;
    };
    let mut context = create_formatting_context(
        parent.state,
        child,
        parent_rust_fc,
        fc_type as u8,
        layout_mode,
        parent.should_collect_devtools_layout_data,
        parent.callbacks,
    );
    run_formatting_context(&mut context, input);
    let result = FfiChildLayoutResult {
        automatic_content_inline_size: context.automatic_content_inline_size,
        automatic_content_block_size: context.automatic_content_block_size,
    };
    parent.child_contexts.insert(child as usize, context);
    Some(result)
}

pub(crate) fn finish_child_layout(parent_rust_fc: *mut c_void, child: *mut c_void) {
    let parent = instance_mut(parent_rust_fc);
    let mut child_context = parent
        .child_contexts
        .remove(&(child as usize))
        .expect("missing child formatting context");
    parent_did_dimension(&mut child_context);
}

pub(crate) fn discard_child_layout(parent_rust_fc: *mut c_void, child: *mut c_void) {
    let parent = instance_mut(parent_rust_fc);
    parent
        .child_contexts
        .remove(&(child as usize))
        .expect("missing child formatting context");
}

fn independent_formatting_context_type(
    state: *mut c_void,
    box_: *mut c_void,
    callbacks: &FfiLayoutFcCallbacks,
) -> FfiFormattingContextType {
    let facts = state_mut(state).box_facts(callbacks, box_);
    if let Some(fc_type) = formatting_context_type_created_by_box(facts) {
        return fc_type;
    }
    if facts.is_block_container {
        return FfiFormattingContextType::Block;
    }

    // HACK: Instead of crashing in scenarios that assume the formatting context can be created, create a dummy formatting context that does nothing.
    eprintln!(
        "FIXME: An independent formatting context was requested from a Box that does not have a formatting context type. A dummy formatting context will be created instead."
    );
    FfiFormattingContextType::InternalDummy
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_run_root_layout(
    root: *mut c_void,
    viewport_inline_size_raw: i32,
    viewport_block_size_raw: i32,
    node_count: u32,
    should_collect_devtools_layout_data: bool,
    callbacks: *const FfiLayoutFcCallbacks,
    sink: *const FfiCommitSink,
) {
    abort_on_panic(|| {
        assert!(!root.is_null());
        assert!(!callbacks.is_null());
        assert!(!sink.is_null());
        // SAFETY: The C++ pass host keeps both callback tables live for this
        // synchronous entry.
        let callbacks = unsafe { *callbacks };
        let sink = unsafe { &*sink };
        let viewport_inline_size = CssPixels::from_raw(viewport_inline_size_raw);
        let viewport_block_size = CssPixels::from_raw(viewport_block_size_raw);

        bump(FfiOp::StateCreate);
        let mut state = LayoutState::new(LayoutStatePurpose::Commit);
        state.ensure_capacity(node_count);
        let root_constraints = crate::geometry::FfiContainingBlockConstraints {
            has_percentage_basis_inline_size: true,
            percentage_basis_inline_size: viewport_inline_size,
            has_percentage_basis_block_size: true,
            percentage_basis_block_size: viewport_block_size,
            ..crate::geometry::FfiContainingBlockConstraints::default()
        };
        let viewport_used = state.create_used_values(&callbacks, root, root_constraints);
        unsafe {
            (*viewport_used).set_content_inline_size(viewport_inline_size);
            (*viewport_used).set_content_block_size(viewport_block_size);
        }

        let mut root_for_layout = root;
        bump(FfiOp::NavigationCallback);
        let initial_containing_block =
            unsafe { (callbacks.navigation.first_child)(callbacks.navigation.context, root) };
        let has_initial_containing_block = !initial_containing_block.is_null();
        if !initial_containing_block.is_null() {
            let icb_used = state.create_used_values(&callbacks, initial_containing_block, root_constraints);
            unsafe {
                (*icb_used).set_content_inline_size(viewport_inline_size);
            }
            let icb_facts = state.box_facts(&callbacks, initial_containing_block);
            if icb_facts.is_svg_svg_box {
                // Standalone SVG documents use the viewport size for the root
                // SVG container and enter SVG layout directly.
                unsafe {
                    (*icb_used).set_content_block_size((*viewport_used).content_block_size);
                }
                root_for_layout = initial_containing_block;
            }
        }
        state.allow_precreated_used_values_reuse();

        let input = FfiLayoutInput {
            available_space: AvailableSpace {
                inline_size: AvailableSize::definite(viewport_inline_size),
                block_size: AvailableSize::definite(viewport_block_size),
            },
            containing_block_constraints: crate::geometry::FfiContainingBlockConstraints::default(),
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        };
        let state_handle = std::ptr::from_mut(&mut state).cast();
        let fc_type = independent_formatting_context_type(state_handle, root_for_layout, &callbacks);
        let mut context = create_formatting_context(
            state_handle,
            root_for_layout,
            std::ptr::null_mut(),
            fc_type as u8,
            0,
            should_collect_devtools_layout_data,
            callbacks,
        );
        run_formatting_context(&mut context, input);
        drop(context);
        if has_initial_containing_block {
            crate::ffi_stats::exclude_pass_seed_fact_builds();
        }
        state.commit_replacing(root, std::ptr::null_mut(), &callbacks, sink);
        bump(FfiOp::StateDestroy);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_compute_subtree_layout(
    root: *mut c_void,
    viewport: *mut c_void,
    paintable_to_replace: *mut c_void,
    callbacks: *const FfiLayoutFcCallbacks,
    sink: *const FfiCommitSink,
) {
    abort_on_panic(|| {
        assert!(!root.is_null());
        assert!(!paintable_to_replace.is_null());
        assert!(!callbacks.is_null());
        assert!(!sink.is_null());
        // SAFETY: The C++ pass host keeps both callback tables live for this
        // synchronous entry.
        let callbacks = unsafe { *callbacks };
        let sink = unsafe { &*sink };

        bump(FfiOp::StateCreate);
        let mut state = LayoutState::new(LayoutStatePurpose::Commit);
        let root_used = state
            .populate_from_paintable(&callbacks, root, paintable_to_replace)
            .expect("partial relayout root must have committed geometry");
        if !viewport.is_null() && viewport != root {
            let _ = state.populate_from_paintable(&callbacks, viewport, std::ptr::null_mut());
        }
        let input = unsafe {
            FfiLayoutInput {
                available_space: AvailableSpace {
                    inline_size: AvailableSize::definite((*root_used).content_inline_size),
                    block_size: AvailableSize::definite((*root_used).content_block_size),
                },
                // The subtree root has definite sizes in both axes, so boxes
                // below it do not need inherited percentage constraints.
                containing_block_constraints: crate::geometry::FfiContainingBlockConstraints::default(),
                has_content_box_position_in_bfc_root: false,
                content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
                has_table_grid_min_border_box_block_size: false,
                table_grid_min_border_box_block_size: CssPixels::default(),
            }
        };

        let state_handle = std::ptr::from_mut(&mut state).cast();
        let facts = state.box_facts(&callbacks, root);
        let fc_type = formatting_context_type_created_by_box(facts)
            .expect("partial relayout root must establish an independent formatting context");
        let mut context = create_formatting_context(
            state_handle,
            root,
            std::ptr::null_mut(),
            fc_type as u8,
            0,
            false,
            callbacks,
        );
        run_formatting_context(&mut context, input);

        // Lay out the subtree root's own absolutely positioned children, like the parent formatting
        // context would do after dimensioning the root box during a full layout.
        parent_did_dimension(&mut context);
        drop(context);
        state.commit_replacing(root, paintable_to_replace, &callbacks, sink);
        bump(FfiOp::StateDestroy);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_replay_saved_abspos_layout(
    box_: *mut c_void,
    paintable_to_replace: *mut c_void,
    callbacks: *const FfiLayoutFcCallbacks,
    sink: *const FfiCommitSink,
) {
    abort_on_panic(|| {
        assert!(!box_.is_null());
        assert!(!paintable_to_replace.is_null());
        assert!(!callbacks.is_null());
        assert!(!sink.is_null());
        // SAFETY: The C++ pass host keeps both callback tables live for this
        // synchronous entry.
        let callbacks = unsafe { *callbacks };
        let sink = unsafe { &*sink };
        bump(FfiOp::StateCreate);
        let mut state = LayoutState::new(LayoutStatePurpose::Commit);
        let state_handle = std::ptr::from_mut(&mut state).cast();
        bump(FfiOp::NavigationCallback);
        // SAFETY: The target box and its containing block remain live for the
        // partial-relayout pass.
        let containing_block = unsafe { (callbacks.navigation.containing_block)(callbacks.navigation.context, box_) };
        assert!(!containing_block.is_null());
        let mut context = create_formatting_context(
            state_handle,
            containing_block,
            std::ptr::null_mut(),
            FfiFormattingContextType::AbsposReplay as u8,
            0,
            false,
            callbacks,
        );
        abspos::replay_for_instance(&mut context, box_);
        drop(context);
        state.commit_replacing(box_, paintable_to_replace, &callbacks, sink);
        bump(FfiOp::StateDestroy);
    });
}
