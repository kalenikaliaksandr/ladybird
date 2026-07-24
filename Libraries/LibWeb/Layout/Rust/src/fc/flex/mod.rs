/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::sizing::SizingContext;
use super::{
    FfiChildLayoutResult, FfiFlexAxis, FfiFlexLayoutData, FfiFlexLayoutItem, FfiFlexLayoutItemRect, FfiFlexLayoutLine,
    FfiFlexSizeProperty, FfiFormattingContextType, FfiLayoutFcCallbacks, FormattingContextInstance,
};
use crate::box_facts::FfiLayoutBoxFacts;
use crate::css_enums::{
    align_content, align_items, align_self, flex_direction, flex_wrap, justify_content, writing_mode,
};
use crate::css_pixels::CssPixels;
use crate::geometry::{AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput};
use crate::layout_state::{FfiStaticPositionAlignment, FfiStaticPositionRect, state_mut};
use crate::style_facts::{FfiSizeKind, FfiSizeValue, FfiStyleFacts};
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use std::collections::HashMap;
use std::ffi::c_void;

pub(crate) type Node = *mut c_void;

const LAYOUT_MODE_NORMAL: u8 = 0;
const LAYOUT_MODE_INTRINSIC_SIZING: u8 = 1;
const FLEX_GROWING: u8 = 0;
const FLEX_SHRINKING: u8 = 1;
const FLEX_UNCLAMPED: u8 = 0;
const FLEX_CLAMPED_TO_MIN: u8 = 1;
const FLEX_CLAMPED_TO_MAX: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlexFactor {
    Grow,
    Shrink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum AxisDirection {
    HorizontalLr,
    HorizontalRl,
    VerticalTb,
    VerticalBt,
}

#[derive(Clone, Copy, Debug)]
struct PixelFraction {
    numerator: CssPixels,
    denominator: CssPixels,
}

impl PixelFraction {
    fn multiply(self, value: CssPixels) -> CssPixels {
        let wide = value.raw_value() as i64 * self.numerator.raw_value() as i64;
        CssPixels::from_raw((wide / self.denominator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }

    fn divide(self, value: CssPixels) -> CssPixels {
        let wide = value.raw_value() as i64 * self.denominator.raw_value() as i64;
        CssPixels::from_raw((wide / self.numerator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DirectionAgnosticMargins {
    main_before: CssPixels,
    main_after: CssPixels,
    cross_before: CssPixels,
    cross_after: CssPixels,
    main_before_is_auto: bool,
    main_after_is_auto: bool,
    cross_before_is_auto: bool,
    cross_after_is_auto: bool,
}

#[derive(Clone, Copy, Debug)]
enum UsedFlexBasis {
    Content,
    Size {
        value: FfiSizeValue,
        property: FfiFlexSizeProperty,
    },
}

struct FlexItem {
    box_: Node,
    used_values: *mut UsedValuesCore,
    used_flex_basis: UsedFlexBasis,
    used_flex_basis_is_definite: bool,
    main_size_was_resolved_from_aspect_ratio: bool,
    cross_size_was_resolved_from_aspect_ratio: bool,
    flex_base_size: CssPixels,
    hypothetical_main_size: CssPixels,
    hypothetical_cross_size: CssPixels,
    target_main_size: CssPixels,
    frozen: bool,
    flex_factor: f64,
    scaled_flex_shrink_factor: f64,
    desired_flex_fraction: f64,
    main_size: Option<CssPixels>,
    cross_size: Option<CssPixels>,
    main_offset: CssPixels,
    cross_offset: CssPixels,
    margins: DirectionAgnosticMargins,
    borders: DirectionAgnosticMargins,
    padding: DirectionAgnosticMargins,
    is_min_violation: bool,
    is_max_violation: bool,
}

impl FlexItem {
    fn new(box_: Node, used_values: *mut UsedValuesCore) -> Self {
        Self {
            box_,
            used_values,
            used_flex_basis: UsedFlexBasis::Content,
            used_flex_basis_is_definite: false,
            main_size_was_resolved_from_aspect_ratio: false,
            cross_size_was_resolved_from_aspect_ratio: false,
            flex_base_size: CssPixels::default(),
            hypothetical_main_size: CssPixels::default(),
            hypothetical_cross_size: CssPixels::default(),
            target_main_size: CssPixels::default(),
            frozen: false,
            flex_factor: 0.0,
            scaled_flex_shrink_factor: 0.0,
            desired_flex_fraction: 0.0,
            main_size: None,
            cross_size: None,
            main_offset: CssPixels::default(),
            cross_offset: CssPixels::default(),
            margins: DirectionAgnosticMargins::default(),
            borders: DirectionAgnosticMargins::default(),
            padding: DirectionAgnosticMargins::default(),
            is_min_violation: false,
            is_max_violation: false,
        }
    }

    fn hypothetical_cross_size_with_margins(&self) -> CssPixels {
        self.hypothetical_cross_size
            + self.margins.cross_before
            + self.margins.cross_after
            + self.borders.cross_after
            + self.borders.cross_before
            + self.padding.cross_after
            + self.padding.cross_before
    }

    fn outer_hypothetical_main_size(&self) -> CssPixels {
        self.add_main_margin_box_sizes(self.hypothetical_main_size)
    }

    fn outer_target_main_size(&self) -> CssPixels {
        self.add_main_margin_box_sizes(self.target_main_size)
    }

    fn outer_flex_base_size(&self) -> CssPixels {
        self.add_main_margin_box_sizes(self.flex_base_size)
    }

    fn add_main_margin_box_sizes(&self, content_size: CssPixels) -> CssPixels {
        content_size
            + self.margins.main_before
            + self.margins.main_after
            + self.borders.main_before
            + self.borders.main_after
            + self.padding.main_before
            + self.padding.main_after
    }

    fn add_cross_margin_box_sizes(&self, content_size: CssPixels) -> CssPixels {
        content_size
            + self.margins.cross_before
            + self.margins.cross_after
            + self.borders.cross_before
            + self.borders.cross_after
            + self.padding.cross_before
            + self.padding.cross_after
    }
}

#[derive(Default)]
struct FlexLine {
    items: Vec<usize>,
    cross_size: CssPixels,
    has_baseline_aligned_items: bool,
    remaining_free_space: Option<CssPixels>,
    chosen_flex_fraction: f64,
    growth_state: u8,
}

#[derive(Clone, Copy)]
struct AxisAgnosticAvailableSpace {
    main: AvailableSize,
    cross: AvailableSize,
    space: AvailableSpace,
}

struct FlexFormattingContext {
    state: *mut c_void,
    flex_container: Node,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    should_collect_devtools_layout_data: bool,
    flex_container_state: *mut UsedValuesCore,
    flex_lines: Vec<FlexLine>,
    flex_items: Vec<FlexItem>,
    flex_direction: u8,
    available_space_for_items: Option<AxisAgnosticAvailableSpace>,
    available_space: Option<AvailableSpace>,
    layout_input: Option<FfiLayoutInput>,
    item_percentage_bases: FfiContainingBlockConstraints,
}

impl FlexFormattingContext {
    fn new(instance: &FormattingContextInstance) -> Self {
        assert_eq!(instance.fc_type, FfiFormattingContextType::Flex as u8);
        let flex_container_state = Self::get_used_values_from_callbacks(&instance.callbacks, instance.box_);
        let flex_direction = state_mut(instance.state)
            .style_facts(&instance.callbacks, instance.box_)
            .flex_direction;
        Self {
            state: instance.state,
            flex_container: instance.box_,
            layout_mode: instance.layout_mode,
            callbacks: instance.callbacks,
            should_collect_devtools_layout_data: instance.should_collect_devtools_layout_data,
            flex_container_state,
            flex_lines: Vec::new(),
            flex_items: Vec::new(),
            flex_direction,
            available_space_for_items: None,
            available_space: None,
            layout_input: None,
            item_percentage_bases: FfiContainingBlockConstraints::default(),
        }
    }

    fn get_used_values_from_callbacks(callbacks: &FfiLayoutFcCallbacks, node: Node) -> *mut UsedValuesCore {
        // SAFETY: The callback returns state-owned storage.
        let result = unsafe { (callbacks.get_used_values)(callbacks.context, node) };
        assert!(!result.is_null());
        result
    }

    fn item_used(&self, index: usize) -> &UsedValuesCore {
        // SAFETY: Each item stores stable state-owned used-values storage.
        unsafe { &*self.flex_items[index].used_values }
    }

    fn item_used_mut(&mut self, index: usize) -> &mut UsedValuesCore {
        // SAFETY: Layout is single-threaded and item mutations are serialized.
        unsafe { &mut *self.flex_items[index].used_values }
    }

    fn container_used(&self) -> &UsedValuesCore {
        // SAFETY: The container entry outlives this formatting context.
        unsafe { &*self.flex_container_state }
    }

    fn container_used_mut(&mut self) -> &mut UsedValuesCore {
        // SAFETY: Layout is single-threaded and mutations are serialized.
        unsafe { &mut *self.flex_container_state }
    }

    fn style(&self, node: Node) -> FfiStyleFacts {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn facts(&self, node: Node) -> FfiLayoutBoxFacts {
        state_mut(self.state).box_facts(&self.callbacks, node)
    }

    fn sizing(&self) -> SizingContext {
        SizingContext::new(self.state, self.callbacks)
    }

    fn navigate(&self, callback: crate::box_facts::FfiLayoutNavCallback, node: Node) -> Node {
        // SAFETY: Navigation is synchronous and the host owns every node.
        unsafe { callback(self.callbacks.navigation.context, node) }
    }

    fn create_used_values(&self, node: Node) -> *mut UsedValuesCore {
        let constraints = self.item_percentage_bases;
        // SAFETY: The host creates one state entry for this flex item.
        let result = unsafe {
            (self.callbacks.create_used_values)(
                self.callbacks.context,
                node,
                constraints.has_percentage_basis_inline_size,
                constraints.percentage_basis_inline_size,
                constraints.has_percentage_basis_block_size,
                constraints.percentage_basis_block_size,
            )
        };
        assert!(!result.is_null());
        result
    }

    fn constraints_for_child_context(
        &self,
        node: Node,
        constraints: FfiContainingBlockConstraints,
    ) -> FfiContainingBlockConstraints {
        self.sizing().constraints_for_child_context(node, constraints)
    }

    fn item_containing_block_constraints(&self) -> FfiContainingBlockConstraints {
        let mut constraints = self.constraints_for_child_context(
            self.flex_container,
            self.layout_input.unwrap().containing_block_constraints,
        );
        constraints.has_percentage_basis_inline_size = self.item_percentage_bases.has_percentage_basis_inline_size;
        constraints.percentage_basis_inline_size = self.item_percentage_bases.percentage_basis_inline_size;
        constraints.has_percentage_basis_block_size = self.item_percentage_bases.has_percentage_basis_block_size;
        constraints.percentage_basis_block_size = self.item_percentage_bases.percentage_basis_block_size;
        constraints
    }

    fn inline_axis_is_horizontal(&self, node: Node) -> bool {
        self.style(node).writing_mode == writing_mode::HORIZONTAL_TB
    }

    fn is_row_layout(&self) -> bool {
        matches!(self.flex_direction, flex_direction::ROW | flex_direction::ROW_REVERSE)
    }

    fn is_single_line(&self) -> bool {
        self.style(self.flex_container).flex_wrap == flex_wrap::NOWRAP
    }

    fn main_axis_is_horizontal(&self) -> bool {
        if self.is_row_layout() {
            self.inline_axis_is_horizontal(self.flex_container)
        } else {
            !self.inline_axis_is_horizontal(self.flex_container)
        }
    }

    fn cross_axis_is_horizontal(&self) -> bool {
        !self.main_axis_is_horizontal()
    }

    fn main_axis_is_parallel_to_inline_axis(&self, node: Node) -> bool {
        self.main_axis_is_horizontal() == self.inline_axis_is_horizontal(node)
    }

    fn inline_axis_is_reverse(&self, node: Node) -> bool {
        let style = self.style(node);
        match style.writing_mode {
            writing_mode::HORIZONTAL_TB
            | writing_mode::VERTICAL_RL
            | writing_mode::VERTICAL_LR
            | writing_mode::SIDEWAYS_RL => style.direction == 1,
            writing_mode::SIDEWAYS_LR => style.direction == 0,
            _ => unreachable!("invalid writing mode"),
        }
    }

    fn block_axis_is_reverse(&self, node: Node) -> bool {
        matches!(
            self.style(node).writing_mode,
            writing_mode::VERTICAL_RL | writing_mode::SIDEWAYS_RL
        )
    }

    fn cross_axis_is_reverse(&self) -> bool {
        let mut reverse = if self.main_axis_is_parallel_to_inline_axis(self.flex_container) {
            self.block_axis_is_reverse(self.flex_container)
        } else {
            self.inline_axis_is_reverse(self.flex_container)
        };
        if self.style(self.flex_container).flex_wrap == flex_wrap::WRAP_REVERSE {
            reverse = !reverse;
        }
        reverse
    }

    fn is_direction_reverse(&self) -> bool {
        let mut reverse = if self.is_row_layout() {
            self.inline_axis_is_reverse(self.flex_container)
        } else {
            self.block_axis_is_reverse(self.flex_container)
        };
        if matches!(
            self.flex_direction,
            flex_direction::COLUMN_REVERSE | flex_direction::ROW_REVERSE
        ) {
            reverse = !reverse;
        }
        reverse
    }

    fn preferred_aspect_ratio(&self, node: Node) -> Option<PixelFraction> {
        let facts = self.facts(node);
        facts.has_preferred_aspect_ratio.then_some(PixelFraction {
            numerator: facts.preferred_aspect_ratio_numerator,
            denominator: facts.preferred_aspect_ratio_denominator,
        })
    }

    fn main_size_from_cross_size_and_aspect_ratio(
        &self,
        cross_size: CssPixels,
        aspect_ratio: PixelFraction,
    ) -> CssPixels {
        if self.main_axis_is_horizontal() {
            aspect_ratio.multiply(cross_size)
        } else {
            aspect_ratio.divide(cross_size)
        }
    }

    fn cross_size_from_main_size_and_aspect_ratio(
        &self,
        main_size: CssPixels,
        aspect_ratio: PixelFraction,
    ) -> CssPixels {
        if self.main_axis_is_horizontal() {
            aspect_ratio.divide(main_size)
        } else {
            aspect_ratio.multiply(main_size)
        }
    }

    fn has_definite_main_size_used(&self, used: &UsedValuesCore) -> bool {
        if self.main_axis_is_horizontal() {
            used.has_definite_inline_size()
        } else {
            used.has_definite_block_size()
        }
    }

    fn has_definite_cross_size_used(&self, used: &UsedValuesCore) -> bool {
        if self.cross_axis_is_horizontal() {
            used.has_definite_inline_size()
        } else {
            used.has_definite_block_size()
        }
    }

    fn has_definite_main_size(&self, index: usize) -> bool {
        self.has_definite_main_size_used(self.item_used(index))
    }

    fn has_definite_cross_size(&self, index: usize) -> bool {
        self.has_definite_cross_size_used(self.item_used(index))
    }

    fn inner_main_size_used(&self, used: &UsedValuesCore) -> CssPixels {
        if self.main_axis_is_horizontal() {
            used.content_inline_size
        } else {
            used.content_block_size
        }
    }

    fn inner_cross_size_used(&self, used: &UsedValuesCore) -> CssPixels {
        if self.cross_axis_is_horizontal() {
            used.content_inline_size
        } else {
            used.content_block_size
        }
    }

    fn inner_main_size(&self, index: usize) -> CssPixels {
        self.inner_main_size_used(self.item_used(index))
    }

    fn inner_cross_size(&self, index: usize) -> CssPixels {
        self.inner_cross_size_used(self.item_used(index))
    }

    fn set_has_definite_main_size(&mut self, index: usize) {
        if self.main_axis_is_horizontal() {
            self.item_used_mut(index).has_definite_inline_size = true;
        } else {
            self.item_used_mut(index).has_definite_block_size = true;
        }
    }

    fn set_has_definite_cross_size(&mut self, index: usize) {
        if self.cross_axis_is_horizontal() {
            self.item_used_mut(index).has_definite_inline_size = true;
        } else {
            self.item_used_mut(index).has_definite_block_size = true;
        }
    }

    fn set_main_size(&mut self, index: usize, size: CssPixels) {
        if self.main_axis_is_horizontal() {
            self.item_used_mut(index).set_content_inline_size(size);
        } else {
            self.item_used_mut(index).set_content_block_size(size);
        }
    }

    fn set_cross_size(&mut self, index: usize, size: CssPixels) {
        if self.cross_axis_is_horizontal() {
            self.item_used_mut(index).set_content_inline_size(size);
        } else {
            self.item_used_mut(index).set_content_block_size(size);
        }
    }

    fn set_container_main_size(&mut self, size: CssPixels) {
        if self.main_axis_is_horizontal() {
            self.container_used_mut().set_content_inline_size(size);
        } else {
            self.container_used_mut().set_content_block_size(size);
        }
    }

    fn set_container_cross_size(&mut self, size: CssPixels) {
        if self.cross_axis_is_horizontal() {
            self.container_used_mut().set_content_inline_size(size);
        } else {
            self.container_used_mut().set_content_block_size(size);
        }
    }

    fn computed_main_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.main_axis_is_horizontal() {
            (style.width, FfiFlexSizeProperty::Width)
        } else {
            (style.height, FfiFlexSizeProperty::Height)
        }
    }

    fn computed_main_min_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.main_axis_is_horizontal() {
            (style.min_width, FfiFlexSizeProperty::MinWidth)
        } else {
            (style.min_height, FfiFlexSizeProperty::MinHeight)
        }
    }

    fn computed_main_max_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.main_axis_is_horizontal() {
            (style.max_width, FfiFlexSizeProperty::MaxWidth)
        } else {
            (style.max_height, FfiFlexSizeProperty::MaxHeight)
        }
    }

    fn computed_cross_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.cross_axis_is_horizontal() {
            (style.width, FfiFlexSizeProperty::Width)
        } else {
            (style.height, FfiFlexSizeProperty::Height)
        }
    }

    fn computed_cross_min_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.cross_axis_is_horizontal() {
            (style.min_width, FfiFlexSizeProperty::MinWidth)
        } else {
            (style.min_height, FfiFlexSizeProperty::MinHeight)
        }
    }

    fn computed_cross_max_size(&self, node: Node) -> (FfiSizeValue, FfiFlexSizeProperty) {
        let style = self.style(node);
        if self.cross_axis_is_horizontal() {
            (style.max_width, FfiFlexSizeProperty::MaxWidth)
        } else {
            (style.max_height, FfiFlexSizeProperty::MaxHeight)
        }
    }

    fn calculate_inner_size(
        &self,
        node: Node,
        axis: FfiFlexAxis,
        property: FfiFlexSizeProperty,
        available_space: AvailableSpace,
    ) -> CssPixels {
        self.calculate_inner_size_with_constraints(
            node,
            axis,
            property,
            available_space,
            self.item_containing_block_constraints(),
        )
    }

    fn calculate_inner_size_with_constraints(
        &self,
        node: Node,
        axis: FfiFlexAxis,
        property: FfiFlexSizeProperty,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
    ) -> CssPixels {
        self.sizing()
            .calculate_inner_size_for_property(node, axis, property, available_space, constraints)
    }

    fn resolve_inner_inline_size(&self, index: usize, property: FfiFlexSizeProperty) -> CssPixels {
        self.calculate_inner_size(
            self.flex_items[index].box_,
            FfiFlexAxis::Inline,
            property,
            AvailableSpace {
                inline_size: self.available_space.unwrap().inline_size,
                block_size: AvailableSize::indefinite(),
            },
        )
    }

    fn resolve_inner_block_size(&self, index: usize, value: FfiSizeValue, property: FfiFlexSizeProperty) -> CssPixels {
        let available_space = if self.main_axis_is_horizontal() && value.is_intrinsic_sizing_constraint() {
            AvailableSpace {
                inline_size: self.flex_items[index]
                    .main_size
                    .map(|size| AvailableSize::definite(clamp_to_max_dimension_value(size)))
                    .unwrap_or_else(AvailableSize::indefinite),
                block_size: AvailableSize::indefinite(),
            }
        } else {
            self.available_space.unwrap()
        };
        self.calculate_inner_size(
            self.flex_items[index].box_,
            FfiFlexAxis::Block,
            property,
            available_space,
        )
    }

    fn resolve_size_for_axis(
        &self,
        index: usize,
        axis_is_horizontal: bool,
        value: FfiSizeValue,
        property: FfiFlexSizeProperty,
    ) -> CssPixels {
        if axis_is_horizontal {
            self.resolve_inner_inline_size(index, property)
        } else {
            self.resolve_inner_block_size(index, value, property)
        }
    }

    fn calculate_fit_content_size(&self, index: usize, axis: FfiFlexAxis, space: AvailableSpace) -> CssPixels {
        self.sizing().calculate_fit_content_size(
            self.flex_items[index].box_,
            axis,
            space,
            self.item_containing_block_constraints(),
        )
    }

    fn should_treat_size_as_auto(&self, node: Node, axis: FfiFlexAxis) -> bool {
        self.sizing().should_treat_size_as_auto(
            node,
            axis,
            self.available_space_for_items.unwrap().space,
            self.item_containing_block_constraints(),
        )
    }

    fn should_treat_main_size_as_auto(&self, node: Node) -> bool {
        self.should_treat_size_as_auto(
            node,
            if self.main_axis_is_horizontal() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
        )
    }

    fn should_treat_cross_size_as_auto(&self, node: Node) -> bool {
        self.should_treat_size_as_auto(
            node,
            if self.cross_axis_is_horizontal() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
        )
    }

    fn should_treat_max_size_as_none(&self, node: Node, cross: bool) -> bool {
        let axis_is_horizontal = if cross {
            self.cross_axis_is_horizontal()
        } else {
            self.main_axis_is_horizontal()
        };
        let available = if cross {
            self.available_space_for_items.unwrap().cross
        } else {
            self.available_space_for_items.unwrap().main
        };
        if axis_is_horizontal {
            self.sizing().should_treat_max_inline_size_as_none(
                node,
                available,
                self.item_containing_block_constraints(),
            )
        } else {
            self.sizing()
                .should_treat_max_block_size_as_none(node, available, self.item_containing_block_constraints())
        }
    }

    fn has_main_min_size(&self, node: Node) -> bool {
        !self.computed_main_min_size(node).0.is_auto()
    }

    fn has_cross_min_size(&self, node: Node) -> bool {
        !self.computed_cross_min_size(node).0.is_auto()
    }

    fn has_main_max_size(&self, node: Node) -> bool {
        !self.should_treat_max_size_as_none(node, false)
    }

    fn has_cross_max_size(&self, node: Node) -> bool {
        !self.should_treat_max_size_as_none(node, true)
    }

    fn specified_main_min_size(&self, index: usize) -> CssPixels {
        let (value, property) = self.computed_main_min_size(self.flex_items[index].box_);
        self.resolve_size_for_axis(index, self.main_axis_is_horizontal(), value, property)
    }

    fn specified_cross_min_size(&self, index: usize) -> CssPixels {
        let (value, property) = self.computed_cross_min_size(self.flex_items[index].box_);
        self.resolve_size_for_axis(index, self.cross_axis_is_horizontal(), value, property)
    }

    fn specified_main_max_size(&self, index: usize) -> CssPixels {
        let (value, property) = self.computed_main_max_size(self.flex_items[index].box_);
        self.resolve_size_for_axis(index, self.main_axis_is_horizontal(), value, property)
    }

    fn specified_cross_max_size(&self, index: usize) -> CssPixels {
        let (value, property) = self.computed_cross_max_size(self.flex_items[index].box_);
        self.resolve_size_for_axis(index, self.cross_axis_is_horizontal(), value, property)
    }

    fn determine_available_space_for_items(&mut self, available_space: AvailableSpace) {
        self.available_space_for_items = Some(if self.main_axis_is_horizontal() {
            AxisAgnosticAvailableSpace {
                main: available_space.inline_size,
                cross: available_space.block_size,
                space: available_space,
            }
        } else {
            AxisAgnosticAvailableSpace {
                main: available_space.block_size,
                cross: available_space.inline_size,
                space: available_space,
            }
        });
    }

    fn populate_specified_margins(&mut self, index: usize) {
        let node = self.flex_items[index].box_;
        let style = self.style(node);
        let basis = if self.item_percentage_bases.has_percentage_basis_inline_size {
            self.item_percentage_bases.percentage_basis_inline_size
        } else {
            CssPixels::default()
        };
        let padding_left = style.padding_left.to_px(basis);
        let padding_right = style.padding_right.to_px(basis);
        let padding_top = style.padding_top.to_px(basis);
        let padding_bottom = style.padding_bottom.to_px(basis);
        {
            let used = self.item_used_mut(index);
            used.padding_left = padding_left;
            used.padding_right = padding_right;
            used.padding_top = padding_top;
            used.padding_bottom = padding_bottom;
        }

        let main_axis_is_horizontal = self.main_axis_is_horizontal();
        let item = &mut self.flex_items[index];
        if main_axis_is_horizontal {
            item.borders.main_before = style.border_left_width;
            item.borders.main_after = style.border_right_width;
            item.borders.cross_before = style.border_top_width;
            item.borders.cross_after = style.border_bottom_width;
            item.padding.main_before = padding_left;
            item.padding.main_after = padding_right;
            item.padding.cross_before = padding_top;
            item.padding.cross_after = padding_bottom;
            item.margins.main_before = style.margin_left.to_px(basis);
            item.margins.main_after = style.margin_right.to_px(basis);
            item.margins.cross_before = style.margin_top.to_px(basis);
            item.margins.cross_after = style.margin_bottom.to_px(basis);
            item.margins.main_before_is_auto = style.margin_left.is_auto();
            item.margins.main_after_is_auto = style.margin_right.is_auto();
            item.margins.cross_before_is_auto = style.margin_top.is_auto();
            item.margins.cross_after_is_auto = style.margin_bottom.is_auto();
        } else {
            item.borders.main_before = style.border_top_width;
            item.borders.main_after = style.border_bottom_width;
            item.borders.cross_before = style.border_left_width;
            item.borders.cross_after = style.border_right_width;
            item.padding.main_before = padding_top;
            item.padding.main_after = padding_bottom;
            item.padding.cross_before = padding_left;
            item.padding.cross_after = padding_right;
            item.margins.main_before = style.margin_top.to_px(basis);
            item.margins.main_after = style.margin_bottom.to_px(basis);
            item.margins.cross_before = style.margin_left.to_px(basis);
            item.margins.cross_after = style.margin_right.to_px(basis);
            item.margins.main_before_is_auto = style.margin_top.is_auto();
            item.margins.main_after_is_auto = style.margin_bottom.is_auto();
            item.margins.cross_before_is_auto = style.margin_left.is_auto();
            item.margins.cross_after_is_auto = style.margin_right.is_auto();
        }
    }

    fn generate_anonymous_flex_items(&mut self) {
        let mut buckets: HashMap<i32, Vec<FlexItem>> = HashMap::new();
        let mut child = self.navigate(self.callbacks.navigation.first_child, self.flex_container);
        while !child.is_null() {
            let next = self.navigate(self.callbacks.navigation.next_sibling, child);
            let facts = self.facts(child);
            if facts.is_box {
                // SAFETY: The callback only inspects the live layout subtree.
                let skip = unsafe { (self.callbacks.can_skip_is_anonymous_text_run)(self.callbacks.context, child) };
                if !skip && !facts.is_absolutely_positioned {
                    // Flex inhibits floating, so only absolute positioning is out of flow here.
                    unsafe { (self.callbacks.set_flex_item)(self.callbacks.context, child, true) };
                    let used = self.create_used_values(child);
                    let item = FlexItem::new(child, used);
                    buckets.entry(self.style(child).order).or_default().push(item);
                }
            }
            child = next;
        }

        let keys = order_modified_keys(&buckets, self.is_direction_reverse());
        for key in keys {
            self.flex_items.extend(buckets.remove(&key).unwrap());
        }
        for index in 0..self.flex_items.len() {
            self.populate_specified_margins(index);
        }
    }

    fn used_flex_basis_for_item(&self, index: usize) -> UsedFlexBasis {
        let node = self.flex_items[index].box_;
        let style = self.style(node);
        if style.flex_basis_is_content {
            return UsedFlexBasis::Content;
        }
        let mut value = style.flex_basis;
        let mut property = FfiFlexSizeProperty::FlexBasis;
        if value.is_auto() {
            (value, property) = self.computed_main_size(node);
            if value.is_auto() {
                return UsedFlexBasis::Content;
            }
        }
        if value.is_percentage() && !self.has_definite_main_size_used(self.container_used()) {
            return UsedFlexBasis::Content;
        }
        UsedFlexBasis::Size { value, property }
    }

    fn adjust_main_size_through_aspect_ratio(
        &self,
        node: Node,
        mut main_size: CssPixels,
        min_cross_size: FfiSizeValue,
        max_cross_size: FfiSizeValue,
    ) -> CssPixels {
        let ratio = self.preferred_aspect_ratio(node).unwrap();
        let reference = self.inner_cross_size_used(self.container_used());
        if !self.should_treat_max_size_as_none(node, true) {
            main_size =
                main_size.min(self.main_size_from_cross_size_and_aspect_ratio(max_cross_size.to_px(reference), ratio));
        }
        if !min_cross_size.is_auto() {
            main_size =
                main_size.max(self.main_size_from_cross_size_and_aspect_ratio(min_cross_size.to_px(reference), ratio));
        }
        main_size
    }

    fn adjust_cross_size_through_aspect_ratio(
        &self,
        node: Node,
        mut cross_size: CssPixels,
        min_main_size: FfiSizeValue,
        max_main_size: FfiSizeValue,
    ) -> CssPixels {
        let ratio = self.preferred_aspect_ratio(node).unwrap();
        let reference = self.inner_main_size_used(self.container_used());
        if !self.should_treat_max_size_as_none(node, false) {
            cross_size =
                cross_size.min(self.cross_size_from_main_size_and_aspect_ratio(max_main_size.to_px(reference), ratio));
        }
        if !min_main_size.is_auto() {
            cross_size =
                cross_size.max(self.cross_size_from_main_size_and_aspect_ratio(min_main_size.to_px(reference), ratio));
        }
        cross_size
    }

    fn calculate_min_content_inline_size(&self, index: usize) -> CssPixels {
        self.sizing()
            .calculate_min_content_inline_size(self.flex_items[index].box_, self.item_containing_block_constraints())
    }

    fn calculate_max_content_inline_size(&self, index: usize) -> CssPixels {
        self.sizing()
            .calculate_max_content_inline_size(self.flex_items[index].box_, self.item_containing_block_constraints())
    }

    fn calculate_min_content_block_size(&self, index: usize, inline_size: CssPixels) -> CssPixels {
        self.sizing().calculate_min_content_block_size(
            self.flex_items[index].box_,
            inline_size,
            self.item_containing_block_constraints(),
        )
    }

    fn calculate_max_content_block_size(&self, index: usize, inline_size: CssPixels) -> CssPixels {
        self.sizing().calculate_max_content_block_size(
            self.flex_items[index].box_,
            inline_size,
            self.item_containing_block_constraints(),
        )
    }

    fn calculate_inline_size_for_intrinsic_block_size(&self, index: usize) -> CssPixels {
        let node = self.flex_items[index].box_;
        let style = self.style(node);
        let can_resolve_percentages = self.available_space_for_items.unwrap().space.inline_size.is_definite();
        let min_inline_size =
            if !style.min_width.is_auto() && (!style.min_width.contains_percentage || can_resolve_percentages) {
                self.resolve_inner_inline_size(index, FfiFlexSizeProperty::MinWidth)
            } else {
                CssPixels::default()
            };
        let max_inline_size = if !self.should_treat_max_size_as_none_for_axis(
            node,
            FfiFlexAxis::Inline,
            self.available_space_for_items.unwrap().space.inline_size,
        ) && (!style.max_width.contains_percentage || can_resolve_percentages)
        {
            self.resolve_inner_inline_size(index, FfiFlexSizeProperty::MaxWidth)
        } else {
            CssPixels::from_raw(i32::MAX)
        };

        let inline_size = if self.should_treat_size_as_auto(node, FfiFlexAxis::Inline) || style.width.is_fit_content() {
            self.calculate_fit_content_size(
                index,
                FfiFlexAxis::Inline,
                self.available_space_for_items.unwrap().space,
            )
        } else if style.width.is_min_content() {
            self.calculate_min_content_inline_size(index)
        } else if style.width.is_max_content() {
            self.calculate_max_content_inline_size(index)
        } else {
            CssPixels::default()
        };
        css_clamp(inline_size, min_inline_size, max_inline_size)
    }

    fn should_treat_max_size_as_none_for_axis(&self, node: Node, axis: FfiFlexAxis, available: AvailableSize) -> bool {
        match axis {
            FfiFlexAxis::Inline => self.sizing().should_treat_max_inline_size_as_none(
                node,
                available,
                self.item_containing_block_constraints(),
            ),
            FfiFlexAxis::Block => self.sizing().should_treat_max_block_size_as_none(
                node,
                available,
                self.item_containing_block_constraints(),
            ),
        }
    }

    fn intrinsic_block_inline_size(&self, index: usize) -> CssPixels {
        let mut available = self
            .item_used(index)
            .available_inner_space_or_constraints_from(self.available_space_for_items.unwrap().space);
        if available.inline_size.is_indefinite() {
            available.inline_size = AvailableSize::definite(self.calculate_inline_size_for_intrinsic_block_size(index));
        }
        available.inline_size.to_px_or_zero()
    }

    fn calculate_min_content_main_size(&self, index: usize) -> CssPixels {
        if self.main_axis_is_horizontal() {
            self.calculate_min_content_inline_size(index)
        } else {
            self.calculate_min_content_block_size(index, self.intrinsic_block_inline_size(index))
        }
    }

    fn calculate_max_content_main_size(&self, index: usize) -> CssPixels {
        if self.main_axis_is_horizontal() {
            self.calculate_max_content_inline_size(index)
        } else {
            self.calculate_max_content_block_size(index, self.intrinsic_block_inline_size(index))
        }
    }

    fn calculate_min_content_cross_size(&self, index: usize) -> CssPixels {
        if self.cross_axis_is_horizontal() {
            self.calculate_min_content_inline_size(index)
        } else {
            self.calculate_min_content_block_size(index, self.intrinsic_block_inline_size(index))
        }
    }

    fn calculate_max_content_cross_size(&self, index: usize) -> CssPixels {
        if self.cross_axis_is_horizontal() {
            self.calculate_max_content_inline_size(index)
        } else {
            self.calculate_max_content_block_size(index, self.intrinsic_block_inline_size(index))
        }
    }

    fn calculate_fit_content_main_size(&self, index: usize) -> CssPixels {
        self.calculate_fit_content_size(
            index,
            if self.main_axis_is_horizontal() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
            self.available_space_for_items.unwrap().space,
        )
    }

    fn calculate_fit_content_cross_size(&self, index: usize) -> CssPixels {
        self.calculate_fit_content_size(
            index,
            if self.cross_axis_is_horizontal() {
                FfiFlexAxis::Inline
            } else {
                FfiFlexAxis::Block
            },
            self.available_space_for_items.unwrap().space,
        )
    }

    fn determine_flex_base_size(&mut self, index: usize) {
        let node = self.flex_items[index].box_;
        let basis = self.used_flex_basis_for_item(index);
        self.flex_items[index].used_flex_basis = basis;
        let basis_is_definite = match basis {
            UsedFlexBasis::Content => false,
            UsedFlexBasis::Size { value, .. } => {
                if value.is_auto() || value.is_none() || value.is_intrinsic_sizing_constraint() {
                    false
                } else if value.is_length() {
                    true
                } else if value.kind() == FfiSizeKind::Calc {
                    !value.contains_percentage || self.has_definite_main_size_used(self.container_used())
                } else {
                    debug_assert!(value.is_percentage());
                    self.has_definite_main_size_used(self.container_used())
                }
            }
        };
        self.flex_items[index].used_flex_basis_is_definite = basis_is_definite;

        let mut flex_base_size = match basis {
            UsedFlexBasis::Size { value, property } if basis_is_definite => {
                self.resolve_size_for_axis(index, self.main_axis_is_horizontal(), value, property)
            }
            _ if self.facts(node).is_replaced_box
                && self.available_space_for_items.unwrap().main.is_min_content()
                && self.computed_main_size(node).0.contains_percentage =>
            {
                CssPixels::default()
            }
            UsedFlexBasis::Content
                if self.preferred_aspect_ratio(node).is_some() && self.has_definite_cross_size(index) =>
            {
                self.flex_items[index].main_size_was_resolved_from_aspect_ratio = true;
                let (min_cross, _) = self.computed_cross_min_size(node);
                let (max_cross, _) = self.computed_cross_max_size(node);
                self.adjust_main_size_through_aspect_ratio(
                    node,
                    self.main_size_from_cross_size_and_aspect_ratio(
                        self.inner_cross_size(index),
                        self.preferred_aspect_ratio(node).unwrap(),
                    ),
                    min_cross,
                    max_cross,
                )
            }
            UsedFlexBasis::Content if self.available_space_for_items.unwrap().main.is_min_content() => {
                self.calculate_min_content_main_size(index)
            }
            UsedFlexBasis::Content if self.available_space_for_items.unwrap().main.is_max_content() => {
                self.calculate_max_content_main_size(index)
            }
            UsedFlexBasis::Size { value, .. } if value.is_fit_content() => self.calculate_fit_content_main_size(index),
            UsedFlexBasis::Size { value, .. } if value.is_max_content() => self.calculate_max_content_main_size(index),
            UsedFlexBasis::Size { value, .. } if value.is_min_content() => self.calculate_min_content_main_size(index),
            _ if self.has_definite_main_size(index) => self.inner_main_size(index),
            UsedFlexBasis::Content => self.calculate_max_content_main_size(index),
            UsedFlexBasis::Size { .. } => self.calculate_fit_content_main_size(index),
        };

        let facts = self.facts(node);
        if facts.has_auto_content_aspect_ratio {
            if !basis_is_definite
                && !facts.has_auto_content_width
                && !facts.has_auto_content_height
                && !self.has_definite_cross_size(index)
                && self.has_definite_main_size_used(self.container_used())
            {
                flex_base_size = self.inner_main_size_used(self.container_used());
            }
            let (min_cross, _) = self.computed_cross_min_size(node);
            let (max_cross, _) = self.computed_cross_max_size(node);
            flex_base_size = self.adjust_main_size_through_aspect_ratio(node, flex_base_size, min_cross, max_cross);
        }
        self.flex_items[index].flex_base_size = flex_base_size;
    }

    fn automatic_minimum_size(&self, index: usize) -> CssPixels {
        if !self.facts(self.flex_items[index].box_).is_scroll_container {
            self.content_based_minimum_size(index)
        } else {
            CssPixels::default()
        }
    }

    fn specified_size_suggestion(&self, index: usize) -> Option<CssPixels> {
        let node = self.flex_items[index].box_;
        if self.has_definite_main_size(index) && !self.should_treat_main_size_as_auto(node) {
            let (value, property) = self.computed_main_size(node);
            Some(self.resolve_size_for_axis(index, self.main_axis_is_horizontal(), value, property))
        } else {
            None
        }
    }

    fn content_size_suggestion(&self, index: usize) -> CssPixels {
        let node = self.flex_items[index].box_;
        let mut suggestion = self.calculate_min_content_main_size(index);
        if self.preferred_aspect_ratio(node).is_some() {
            suggestion = self.adjust_main_size_through_aspect_ratio(
                node,
                suggestion,
                self.computed_cross_min_size(node).0,
                self.computed_cross_max_size(node).0,
            );
        }
        suggestion
    }

    fn transferred_size_suggestion(&self, index: usize) -> Option<CssPixels> {
        let node = self.flex_items[index].box_;
        let ratio = self.preferred_aspect_ratio(node)?;
        if !self.has_definite_cross_size(index) {
            return None;
        }
        Some(self.adjust_main_size_through_aspect_ratio(
            node,
            self.main_size_from_cross_size_and_aspect_ratio(self.inner_cross_size(index), ratio),
            self.computed_cross_min_size(node).0,
            self.computed_cross_max_size(node).0,
        ))
    }

    fn content_based_minimum_size(&self, index: usize) -> CssPixels {
        let node = self.flex_items[index].box_;
        let content = self.content_size_suggestion(index);
        let transferred = self.transferred_size_suggestion(index);
        let specified = self.specified_size_suggestion(index);
        let mut size = if self.facts(node).is_replaced_box {
            transferred.map_or(content, |value| content.min(value))
        } else {
            transferred.map_or(content, |value| content.max(value))
        };
        if let Some(specified) = specified {
            size = size.min(specified);
        }
        if self.has_main_max_size(node) {
            size.min(self.specified_main_max_size(index))
        } else {
            size
        }
    }

    fn main_gap(&self) -> CssPixels {
        let style = self.style(self.flex_container);
        let gap = if self.is_row_layout() {
            style.column_gap
        } else {
            style.row_gap
        };
        gap.to_px(self.inner_main_size_used(self.container_used()))
    }

    fn cross_gap(&self) -> CssPixels {
        let style = self.style(self.flex_container);
        let gap = if self.is_row_layout() {
            style.row_gap
        } else {
            style.column_gap
        };
        gap.to_px(self.inner_cross_size_used(self.container_used()))
    }

    fn collect_flex_items_into_flex_lines(&mut self) {
        if self.is_single_line() {
            let mut items: Vec<_> = (0..self.flex_items.len()).collect();
            if self.is_direction_reverse() {
                items.reverse();
            }
            self.flex_lines.push(FlexLine {
                items,
                ..Default::default()
            });
            return;
        }

        let mut line = FlexLine::default();
        let mut line_main_size = CssPixels::default();
        for index in 0..self.flex_items.len() {
            let outer = self.flex_items[index].outer_hypothetical_main_size();
            if !line.items.is_empty()
                && self
                    .available_space_for_items
                    .unwrap()
                    .main
                    .pixels_greater_than(line_main_size + outer)
            {
                self.flex_lines.push(line);
                line = FlexLine::default();
                line_main_size = CssPixels::default();
            }
            if self.is_direction_reverse() {
                line.items.insert(0, index);
            } else {
                line.items.push(index);
            }
            line_main_size += outer;
            line_main_size += self.main_gap();
        }
        self.flex_lines.push(line);
    }

    fn remaining_free_space_for_line(
        &self,
        line_index: usize,
        available_main_size: AvailableSize,
    ) -> Option<CssPixels> {
        if available_main_size.is_intrinsic_sizing_constraint() {
            return None;
        }
        let line = &self.flex_lines[line_index];
        let mut sum = CssPixels::default();
        for index in line.items.iter().copied() {
            sum += if self.flex_items[index].frozen {
                self.flex_items[index].outer_target_main_size()
            } else {
                self.flex_items[index].outer_flex_base_size()
            };
        }
        sum += self.main_gap() * line.items.len().wrapping_sub(1);
        Some(available_main_size.to_px_or_zero() - sum)
    }

    fn sum_of_unfrozen_flex_factors(&self, line_index: usize) -> f64 {
        self.flex_lines[line_index]
            .items
            .iter()
            .copied()
            .filter(|index| !self.flex_items[*index].frozen)
            .map(|index| self.flex_items[index].flex_factor)
            .sum()
    }

    fn sum_of_unfrozen_scaled_shrink_factors(&self, line_index: usize) -> f64 {
        self.flex_lines[line_index]
            .items
            .iter()
            .copied()
            .filter(|index| !self.flex_items[*index].frozen)
            .map(|index| self.flex_items[index].scaled_flex_shrink_factor)
            .sum()
    }

    fn resolve_flexible_lengths_for_line(&mut self, line_index: usize) {
        let available_main_size = if self
            .available_space_for_items
            .unwrap()
            .main
            .is_intrinsic_sizing_constraint()
        {
            self.available_space_for_items.unwrap().main
        } else {
            AvailableSize::definite(self.inner_main_size_used(self.container_used()))
        };
        let items = self.flex_lines[line_index].items.clone();
        let mut hypothetical_sum = CssPixels::default();
        for index in items.iter().copied() {
            hypothetical_sum += self.flex_items[index].outer_hypothetical_main_size();
        }
        hypothetical_sum += self.main_gap() * items.len().wrapping_sub(1);
        let factor = if available_main_size.pixels_less_than(hypothetical_sum) {
            FlexFactor::Grow
        } else {
            FlexFactor::Shrink
        };
        self.flex_lines[line_index].growth_state = if factor == FlexFactor::Grow {
            FLEX_GROWING
        } else {
            FLEX_SHRINKING
        };

        for index in items.iter().copied() {
            self.flex_items[index].target_main_size = self.flex_items[index].flex_base_size;
            self.flex_items[index].frozen = false;
        }
        for index in items.iter().copied() {
            self.flex_items[index].flex_factor = if factor == FlexFactor::Grow {
                self.style(self.flex_items[index].box_).flex_grow
            } else {
                self.style(self.flex_items[index].box_).flex_shrink
            };
            let item = &mut self.flex_items[index];
            if item.flex_factor == 0.0
                || (factor == FlexFactor::Grow && item.flex_base_size > item.hypothetical_main_size)
                || (factor == FlexFactor::Shrink && item.flex_base_size < item.hypothetical_main_size)
            {
                item.frozen = true;
                item.target_main_size = item.hypothetical_main_size;
            }
        }

        let initial_free_space = self.remaining_free_space_for_line(line_index, available_main_size);
        while items.iter().copied().any(|index| !self.flex_items[index].frozen) {
            let mut remaining = self.remaining_free_space_for_line(line_index, available_main_size);
            let factor_sum = self.sum_of_unfrozen_flex_factors(line_index);
            if factor_sum < 1.0
                && let Some(initial) = initial_free_space
            {
                let value = CssPixels::nearest_value_for(initial.to_double() * factor_sum);
                if remaining.is_none_or(|current| abs(value) < abs(current)) {
                    remaining = Some(value);
                }
            }
            self.flex_lines[line_index].remaining_free_space = remaining;
            let arithmetic_free_space = remaining.unwrap_or_default();

            if remaining != Some(CssPixels::default()) {
                if factor == FlexFactor::Grow {
                    let sum = self.sum_of_unfrozen_flex_factors(line_index);
                    for index in items.iter().copied() {
                        if self.flex_items[index].frozen {
                            continue;
                        }
                        let ratio = self.flex_items[index].flex_factor / sum;
                        self.flex_items[index].target_main_size =
                            self.flex_items[index].flex_base_size + arithmetic_free_space.scaled(ratio);
                    }
                } else {
                    for index in items.iter().copied() {
                        if self.flex_items[index].frozen {
                            continue;
                        }
                        self.flex_items[index].scaled_flex_shrink_factor =
                            self.flex_items[index].flex_factor * self.flex_items[index].flex_base_size.to_double();
                    }
                    let sum = self.sum_of_unfrozen_scaled_shrink_factors(line_index);
                    for index in items.iter().copied() {
                        if self.flex_items[index].frozen {
                            continue;
                        }
                        let ratio = if sum == 0.0 {
                            1.0
                        } else {
                            self.flex_items[index].scaled_flex_shrink_factor / sum
                        };
                        self.flex_items[index].target_main_size =
                            self.flex_items[index].flex_base_size - abs(arithmetic_free_space).scaled(ratio);
                    }
                }
            }

            let mut total_violation = CssPixels::default();
            for index in items.iter().copied() {
                if self.flex_items[index].frozen {
                    continue;
                }
                let node = self.flex_items[index].box_;
                let min_size = if self.has_main_min_size(node) {
                    self.specified_main_min_size(index)
                } else {
                    self.automatic_minimum_size(index)
                };
                let max_size = if self.has_main_max_size(node) {
                    self.specified_main_max_size(index)
                } else {
                    CssPixels::from_raw(i32::MAX)
                };
                let original = self.flex_items[index].target_main_size;
                let target = css_clamp(original, min_size, max_size).max(CssPixels::default());
                self.flex_items[index].target_main_size = target;
                if target < original {
                    self.flex_items[index].is_max_violation = true;
                }
                if target > original {
                    self.flex_items[index].is_min_violation = true;
                }
                total_violation += target - original;
            }

            for index in items.iter().copied() {
                if self.flex_items[index].frozen {
                    continue;
                }
                if total_violation == CssPixels::default()
                    || (total_violation > CssPixels::default() && self.flex_items[index].is_min_violation)
                    || (total_violation < CssPixels::default() && self.flex_items[index].is_max_violation)
                {
                    self.flex_items[index].frozen = true;
                }
            }
        }

        self.flex_lines[line_index].remaining_free_space =
            self.remaining_free_space_for_line(line_index, available_main_size);
        for index in items {
            let target = self.flex_items[index].target_main_size;
            self.flex_items[index].main_size = Some(target);
            self.set_main_size(index, target);
            let item_is_orthogonal = self.inline_axis_is_horizontal(self.flex_items[index].box_)
                != self.inline_axis_is_horizontal(self.flex_container);
            let container_has_vertical_inline_main_axis =
                self.is_row_layout() && !self.inline_axis_is_horizontal(self.flex_container);
            if self.has_definite_main_size_used(self.container_used())
                || self.flex_items[index].used_flex_basis_is_definite
                || self.flex_items[index].main_size_was_resolved_from_aspect_ratio
                || container_has_vertical_inline_main_axis
                || (item_is_orthogonal && self.main_axis_is_parallel_to_inline_axis(self.flex_items[index].box_))
            {
                self.set_has_definite_main_size(index);
            }
        }
    }

    fn resolve_flexible_lengths(&mut self) {
        for line_index in 0..self.flex_lines.len() {
            self.resolve_flexible_lengths_for_line(line_index);
        }
    }

    fn flex_item_is_stretched(&self, index: usize) -> bool {
        let alignment = self.alignment_for_item(self.flex_items[index].box_);
        if !matches!(alignment, align_items::STRETCH | align_items::NORMAL) {
            return false;
        }
        self.computed_cross_size(self.flex_items[index].box_).0.is_auto()
            && !self.flex_items[index].margins.cross_before_is_auto
            && !self.flex_items[index].margins.cross_after_is_auto
    }

    fn alignment_for_item(&self, node: Node) -> u8 {
        match self.style(node).align_self {
            align_self::AUTO => self.style(self.flex_container).align_items,
            align_self::END => align_items::END,
            align_self::NORMAL => align_items::NORMAL,
            align_self::SELF_START => align_items::SELF_START,
            align_self::SELF_END => align_items::SELF_END,
            align_self::FLEX_START => align_items::FLEX_START,
            align_self::FLEX_END => align_items::FLEX_END,
            align_self::CENTER => align_items::CENTER,
            align_self::BASELINE => align_items::BASELINE,
            align_self::START => align_items::START,
            align_self::STRETCH => align_items::STRETCH,
            align_self::SAFE => align_items::SAFE,
            align_self::UNSAFE => align_items::UNSAFE,
            _ => unreachable!("invalid align-self"),
        }
    }

    fn determine_hypothetical_cross_size_of_item(&mut self, index: usize) {
        let node = self.flex_items[index].box_;
        let min = self.computed_cross_min_size(node).0;
        let clamp_min = if min.is_auto() {
            CssPixels::default()
        } else {
            self.specified_cross_min_size(index)
        };
        let clamp_max = if self.should_treat_max_size_as_none(node, true) {
            CssPixels::from_raw(i32::MAX)
        } else {
            self.specified_cross_max_size(index)
        };

        if self.has_definite_cross_size(index) {
            self.flex_items[index].hypothetical_cross_size =
                css_clamp(self.inner_cross_size(index), clamp_min, clamp_max);
            return;
        }

        if let Some(ratio) = self.preferred_aspect_ratio(node) {
            let facts = self.facts(node);
            let replaced_with_only_natural_ratio =
                facts.is_replaced_box && !(facts.has_auto_content_width && facts.has_auto_content_height);
            if replaced_with_only_natural_ratio && !self.flex_items[index].used_flex_basis_is_definite {
                self.flex_items[index].hypothetical_cross_size =
                    css_clamp(self.inner_cross_size_used(self.container_used()), clamp_min, clamp_max);
                return;
            }
            self.flex_items[index].hypothetical_cross_size = css_clamp(
                self.cross_size_from_main_size_and_aspect_ratio(self.flex_items[index].main_size.unwrap(), ratio),
                clamp_min,
                clamp_max,
            );
            self.flex_items[index].cross_size_was_resolved_from_aspect_ratio = self.has_definite_main_size(index);
            return;
        }

        let computed = self.computed_cross_size(node).0;
        if computed.is_min_content() {
            self.flex_items[index].hypothetical_cross_size =
                css_clamp(self.calculate_min_content_cross_size(index), clamp_min, clamp_max);
            return;
        }
        if computed.is_max_content() {
            self.flex_items[index].hypothetical_cross_size =
                css_clamp(self.calculate_max_content_cross_size(index), clamp_min, clamp_max);
            return;
        }

        let fit_content = if !self.cross_axis_is_horizontal() {
            self.calculate_fit_content_size(
                index,
                FfiFlexAxis::Block,
                AvailableSpace {
                    inline_size: self.flex_items[index]
                        .main_size
                        .map(|size| AvailableSize::definite(clamp_to_max_dimension_value(size)))
                        .unwrap_or_else(AvailableSize::indefinite),
                    block_size: AvailableSize::indefinite(),
                },
            )
        } else {
            self.calculate_fit_content_size(
                index,
                FfiFlexAxis::Inline,
                self.available_space_for_items.unwrap().space,
            )
        };
        self.flex_items[index].hypothetical_cross_size = css_clamp(fit_content, clamp_min, clamp_max);
    }

    fn calculate_inner_container_cross_size(&self, property: FfiFlexSizeProperty) -> CssPixels {
        let axis = if self.cross_axis_is_horizontal() {
            FfiFlexAxis::Inline
        } else {
            FfiFlexAxis::Block
        };
        self.calculate_inner_size_with_constraints(
            self.flex_container,
            axis,
            property,
            self.available_space.unwrap(),
            self.layout_input.unwrap().containing_block_constraints,
        )
    }

    fn calculate_cross_size_of_each_flex_line(&mut self) {
        if self.is_single_line() && self.has_definite_cross_size_used(self.container_used()) {
            self.flex_lines[0].cross_size = self.inner_cross_size_used(self.container_used());
            return;
        }
        for line_index in 0..self.flex_lines.len() {
            let largest = self.flex_lines[line_index]
                .items
                .iter()
                .copied()
                .map(|index| self.flex_items[index].hypothetical_cross_size_with_margins())
                .max()
                .unwrap_or_default();
            self.flex_lines[line_index].cross_size = largest.max(CssPixels::default());
        }
        if self.is_single_line()
            && !self
                .available_space_for_items
                .unwrap()
                .cross
                .is_intrinsic_sizing_constraint()
        {
            let min = self.computed_cross_min_size(self.flex_container).0;
            let cross_min = if min.is_auto() {
                CssPixels::default()
            } else {
                self.calculate_inner_container_cross_size(self.computed_cross_min_size(self.flex_container).1)
            };
            let cross_max = if self.should_treat_max_size_as_none(self.flex_container, true) {
                CssPixels::from_raw(i32::MAX)
            } else {
                self.calculate_inner_container_cross_size(self.computed_cross_max_size(self.flex_container).1)
            };
            self.flex_lines[0].cross_size = css_clamp(self.flex_lines[0].cross_size, cross_min, cross_max);
        }
    }

    fn handle_align_content_stretch(&mut self) {
        if !self.has_definite_cross_size_used(self.container_used())
            || !matches!(
                self.style(self.flex_container).align_content,
                align_content::STRETCH | align_content::NORMAL
            )
        {
            return;
        }
        let mut sum = self
            .flex_lines
            .iter()
            .fold(CssPixels::default(), |sum, line| sum + line.cross_size);
        sum += self.cross_gap() * self.flex_lines.len().wrapping_sub(1);
        let container_size = self.inner_cross_size_used(self.container_used());
        if sum >= container_size {
            return;
        }
        let extra = (container_size - sum) / self.flex_lines.len();
        for line in &mut self.flex_lines {
            line.cross_size += extra;
        }
    }

    fn determine_used_cross_size_of_each_flex_item(&mut self) {
        for line_index in 0..self.flex_lines.len() {
            let items = self.flex_lines[line_index].items.clone();
            for index in items {
                if self.flex_item_is_stretched(index) {
                    let item = &self.flex_items[index];
                    let unclamped = self.flex_lines[line_index].cross_size
                        - item.margins.cross_before
                        - item.margins.cross_after
                        - item.padding.cross_before
                        - item.padding.cross_after
                        - item.borders.cross_before
                        - item.borders.cross_after;
                    let node = item.box_;
                    let min = self.computed_cross_min_size(node).0;
                    let cross_min = if min.is_auto() {
                        CssPixels::default()
                    } else {
                        self.specified_cross_min_size(index)
                    };
                    let cross_max = if self.should_treat_max_size_as_none(node, true) {
                        CssPixels::from_raw(i32::MAX)
                    } else {
                        self.specified_cross_max_size(index)
                    };
                    let cross_size = css_clamp(unclamped, cross_min, cross_max);
                    self.flex_items[index].cross_size = Some(cross_size);
                    self.set_cross_size(index, cross_size);
                    self.set_has_definite_cross_size(index);
                } else {
                    let size = self.flex_items[index].hypothetical_cross_size;
                    self.flex_items[index].cross_size = Some(size);
                    if self.flex_items[index].cross_size_was_resolved_from_aspect_ratio {
                        self.set_cross_size(index, size);
                        self.set_has_definite_cross_size(index);
                    }
                }
            }
        }
    }

    fn set_main_axis_first_margin(&mut self, index: usize, margin: CssPixels) {
        self.flex_items[index].margins.main_before = margin;
        if self.main_axis_is_horizontal() {
            self.item_used_mut(index).margin_left = margin;
        } else {
            self.item_used_mut(index).margin_top = margin;
        }
    }

    fn set_main_axis_second_margin(&mut self, index: usize, margin: CssPixels) {
        self.flex_items[index].margins.main_after = margin;
        if self.main_axis_is_horizontal() {
            self.item_used_mut(index).margin_right = margin;
        } else {
            self.item_used_mut(index).margin_bottom = margin;
        }
    }

    fn distribute_any_remaining_free_space(&mut self) {
        for line_index in 0..self.flex_lines.len() {
            let items = self.flex_lines[line_index].items.clone();
            let mut used_main_space = CssPixels::default();
            let mut auto_margins = 0usize;
            for index in items.iter().copied() {
                let item = &self.flex_items[index];
                used_main_space += item.main_size.unwrap();
                auto_margins += item.margins.main_before_is_auto as usize;
                auto_margins += item.margins.main_after_is_auto as usize;
                used_main_space += item.margins.main_before
                    + item.margins.main_after
                    + item.borders.main_before
                    + item.borders.main_after
                    + item.padding.main_before
                    + item.padding.main_after;
            }
            used_main_space += self.main_gap() * items.len().wrapping_sub(1);

            let remaining = self.flex_lines[line_index].remaining_free_space;
            if remaining.is_some_and(|value| value > CssPixels::default()) && auto_margins > 0 {
                let size = remaining.unwrap() / auto_margins;
                for index in items.iter().copied() {
                    if self.flex_items[index].margins.main_before_is_auto {
                        self.set_main_axis_first_margin(index, size);
                    }
                    if self.flex_items[index].margins.main_after_is_auto {
                        self.set_main_axis_second_margin(index, size);
                    }
                }
            } else {
                for index in items.iter().copied() {
                    if self.flex_items[index].margins.main_before_is_auto {
                        self.set_main_axis_first_margin(index, CssPixels::default());
                    }
                    if self.flex_items[index].margins.main_after_is_auto {
                        self.set_main_axis_second_margin(index, CssPixels::default());
                    }
                }
            }

            let mut space_between_items = CssPixels::default();
            let mut initial_offset = CssPixels::default();
            let number_of_items = items.len();
            let justify = self.style(self.flex_container).justify_content;
            if auto_margins == 0 && number_of_items > 0 {
                match justify {
                    justify_content::START | justify_content::LEFT => {}
                    justify_content::STRETCH | justify_content::NORMAL | justify_content::FLEX_START => {
                        if self.is_direction_reverse() {
                            initial_offset = self.inner_main_size_used(self.container_used());
                        }
                    }
                    justify_content::END => {
                        initial_offset = self.inner_main_size_used(self.container_used());
                    }
                    justify_content::RIGHT => {
                        if self.is_row_layout() {
                            initial_offset = self.inner_main_size_used(self.container_used());
                        }
                    }
                    justify_content::FLEX_END => {
                        if !self.is_direction_reverse() {
                            initial_offset = self.inner_main_size_used(self.container_used());
                        }
                    }
                    justify_content::CENTER => {
                        initial_offset = (self.inner_main_size_used(self.container_used()) - used_main_space) / 2;
                        if self.is_direction_reverse() {
                            initial_offset = self.inner_main_size_used(self.container_used()) - initial_offset;
                        }
                    }
                    justify_content::SPACE_BETWEEN => {
                        if self.is_direction_reverse() {
                            initial_offset = self.inner_main_size_used(self.container_used());
                        }
                        if let Some(free_space) = remaining
                            && number_of_items > 1
                        {
                            space_between_items = (free_space / (number_of_items - 1)).max(CssPixels::default());
                        }
                    }
                    justify_content::SPACE_AROUND => {
                        if let Some(free_space) = remaining {
                            space_between_items = (free_space / number_of_items).max(CssPixels::default());
                        }
                        initial_offset = if self.is_direction_reverse() {
                            self.inner_main_size_used(self.container_used()) - space_between_items / 2
                        } else {
                            space_between_items / 2
                        };
                    }
                    justify_content::SPACE_EVENLY => {
                        if let Some(free_space) = remaining {
                            space_between_items = (free_space / (number_of_items + 1)).max(CssPixels::default());
                        }
                        initial_offset = if self.is_direction_reverse() {
                            self.inner_main_size_used(self.container_used()) - space_between_items
                        } else {
                            space_between_items
                        };
                    }
                    _ => unreachable!("invalid justify-content"),
                }
            }

            let cursor_right = if auto_margins == 0 {
                match justify {
                    justify_content::START | justify_content::LEFT => false,
                    justify_content::NORMAL
                    | justify_content::FLEX_START
                    | justify_content::CENTER
                    | justify_content::SPACE_AROUND
                    | justify_content::SPACE_BETWEEN
                    | justify_content::SPACE_EVENLY
                    | justify_content::STRETCH => self.is_direction_reverse(),
                    justify_content::END => true,
                    justify_content::RIGHT => self.is_row_layout(),
                    justify_content::FLEX_END => !self.is_direction_reverse(),
                    _ => false,
                }
            } else {
                false
            };

            let direction_reverse = self.is_direction_reverse();
            let main_gap = self.main_gap();
            let mut cursor = initial_offset;
            let iterator: Box<dyn Iterator<Item = (usize, usize)>> = if cursor_right {
                Box::new(items.iter().copied().enumerate().rev())
            } else {
                Box::new(items.iter().copied().enumerate())
            };
            for (position, index) in iterator {
                let item = &self.flex_items[index];
                let mut amount = item.main_size.unwrap()
                    + item.margins.main_before
                    + item.borders.main_before
                    + item.padding.main_before
                    + item.margins.main_after
                    + item.borders.main_after
                    + item.padding.main_after
                    + space_between_items;
                if !direction_reverse && cursor_right {
                    if position < items.len() - 1 {
                        amount += main_gap;
                    }
                } else {
                    amount += main_gap;
                }
                if direction_reverse && cursor_right {
                    self.flex_items[index].main_offset = cursor
                        - self.flex_items[index].main_size.unwrap()
                        - self.flex_items[index].margins.main_after
                        - self.flex_items[index].borders.main_after
                        - self.flex_items[index].padding.main_after;
                    cursor -= amount;
                } else if cursor_right {
                    cursor -= amount;
                    self.flex_items[index].main_offset = cursor
                        + self.flex_items[index].margins.main_before
                        + self.flex_items[index].borders.main_before
                        + self.flex_items[index].padding.main_before;
                } else {
                    self.flex_items[index].main_offset = cursor
                        + self.flex_items[index].margins.main_before
                        + self.flex_items[index].borders.main_before
                        + self.flex_items[index].padding.main_before;
                    cursor += amount;
                }
            }
        }
    }

    fn resolve_cross_axis_auto_margins(&mut self) {
        for line_index in 0..self.flex_lines.len() {
            let line_size = self.flex_lines[line_index].cross_size;
            let items = self.flex_lines[line_index].items.clone();
            for index in items {
                let item = &self.flex_items[index];
                if !item.margins.cross_before_is_auto && !item.margins.cross_after_is_auto {
                    continue;
                }
                let outer = item.cross_size.unwrap()
                    + item.padding.cross_before
                    + item.padding.cross_after
                    + item.borders.cross_before
                    + item.borders.cross_after
                    + item.margins.cross_before
                    + item.margins.cross_after;
                if outer < line_size {
                    let remainder = line_size - outer;
                    if item.margins.cross_before_is_auto && item.margins.cross_after_is_auto {
                        self.flex_items[index].margins.cross_before = remainder / 2;
                        self.flex_items[index].margins.cross_after = remainder / 2;
                    } else if item.margins.cross_before_is_auto {
                        self.flex_items[index].margins.cross_before = remainder;
                    } else {
                        self.flex_items[index].margins.cross_after = remainder;
                    }
                }
            }
        }
    }

    fn align_all_flex_items_along_the_cross_axis(&mut self) {
        let wrap_reverse = self.style(self.flex_container).flex_wrap == flex_wrap::WRAP_REVERSE;
        for line_index in 0..self.flex_lines.len() {
            let half_line = self.flex_lines[line_index].cross_size / 2;
            let items = self.flex_lines[line_index].items.clone();
            for index in items {
                let alignment = self.alignment_for_item(self.flex_items[index].box_);
                let item = &self.flex_items[index];
                let offset = match alignment {
                    align_items::NORMAL if wrap_reverse => {
                        half_line
                            - item.cross_size.unwrap()
                            - item.margins.cross_after
                            - item.borders.cross_after
                            - item.padding.cross_after
                    }
                    align_items::NORMAL => {
                        -half_line + item.margins.cross_before + item.borders.cross_before + item.padding.cross_before
                    }
                    align_items::BASELINE => {
                        self.flex_lines[line_index].has_baseline_aligned_items = true;
                        -half_line + item.margins.cross_before + item.borders.cross_before + item.padding.cross_before
                    }
                    align_items::START | align_items::FLEX_START | align_items::SELF_START | align_items::STRETCH => {
                        -half_line + item.margins.cross_before + item.borders.cross_before + item.padding.cross_before
                    }
                    align_items::END | align_items::FLEX_END | align_items::SELF_END => {
                        half_line
                            - item.cross_size.unwrap()
                            - item.margins.cross_after
                            - item.borders.cross_after
                            - item.padding.cross_after
                    }
                    align_items::CENTER => {
                        (-item.cross_size.unwrap()
                            + item.margins.cross_before
                            + item.borders.cross_before
                            + item.padding.cross_before
                            - item.margins.cross_after
                            - item.borders.cross_after
                            - item.padding.cross_after)
                            / 2
                    }
                    _ => item.cross_offset,
                };
                self.flex_items[index].cross_offset = offset;
            }
        }
    }

    fn align_all_flex_lines(&mut self) {
        if self.flex_lines.is_empty() {
            return;
        }
        let reverse_cross_axis = self.cross_axis_is_reverse();
        let container_cross_size = self.inner_cross_size_used(self.container_used());
        if self.is_single_line() {
            let center = self.flex_lines[0].cross_size / 2;
            for index in self.flex_lines[0].items.clone() {
                self.flex_items[index].cross_offset += center;
            }
            return;
        }

        let mut sum = self
            .flex_lines
            .iter()
            .fold(CssPixels::default(), |sum, line| sum + line.cross_size);
        sum += self.cross_gap() * self.flex_lines.len().wrapping_sub(1);
        let mut start = CssPixels::default();
        let mut gap_size = CssPixels::default();
        let mut place_backwards = false;
        let mut iterate_backwards = false;
        match self.style(self.flex_container).align_content {
            align_content::START => {
                iterate_backwards = reverse_cross_axis;
            }
            align_content::END => {
                start = container_cross_size;
                place_backwards = true;
                iterate_backwards = !reverse_cross_axis;
            }
            align_content::FLEX_START => {
                if reverse_cross_axis {
                    start = container_cross_size;
                    place_backwards = true;
                }
            }
            align_content::FLEX_END => {
                iterate_backwards = true;
                if !reverse_cross_axis {
                    start = container_cross_size;
                    place_backwards = true;
                }
            }
            align_content::CENTER => {
                iterate_backwards = reverse_cross_axis;
                start = container_cross_size / 2 - sum / 2;
            }
            align_content::SPACE_BETWEEN => {
                if reverse_cross_axis {
                    start = container_cross_size;
                    place_backwards = true;
                }
                let leftover = container_cross_size - sum;
                if leftover >= CssPixels::default() && self.flex_lines.len() > 1 {
                    gap_size = leftover / (self.flex_lines.len() - 1);
                }
            }
            align_content::SPACE_AROUND => {
                iterate_backwards = reverse_cross_axis;
                let leftover = container_cross_size - sum;
                if leftover < CssPixels::default() {
                    start = container_cross_size / 2 - sum / 2;
                } else {
                    gap_size = leftover / self.flex_lines.len();
                    start = gap_size / 2;
                }
            }
            align_content::SPACE_EVENLY => {
                iterate_backwards = reverse_cross_axis;
                let leftover = container_cross_size - sum;
                if leftover < CssPixels::default() {
                    start = container_cross_size / 2 - sum / 2;
                } else {
                    gap_size = leftover / (self.flex_lines.len() + 1);
                    start = gap_size;
                }
            }
            align_content::NORMAL | align_content::STRETCH => {
                if reverse_cross_axis {
                    start = container_cross_size;
                    place_backwards = true;
                }
            }
            _ => unreachable!("invalid align-content"),
        }

        let cross_gap = self.cross_gap();
        let line_indices: Vec<usize> = if iterate_backwards {
            (0..self.flex_lines.len()).rev().collect()
        } else {
            (0..self.flex_lines.len()).collect()
        };
        for line_index in line_indices {
            let center = if place_backwards {
                start - self.flex_lines[line_index].cross_size / 2
            } else {
                start + self.flex_lines[line_index].cross_size / 2
            };
            for index in self.flex_lines[line_index].items.clone() {
                self.flex_items[index].cross_offset += center;
            }
            let amount = self.flex_lines[line_index].cross_size + gap_size + cross_gap;
            if place_backwards {
                start -= amount;
            } else {
                start += amount;
            }
        }
    }

    fn box_baseline(&self, node: Node) -> CssPixels {
        // SAFETY: Baseline calculation is synchronous for a live box.
        unsafe { (self.callbacks.box_baseline)(self.callbacks.context, node, 0) }
    }

    fn resolve_baseline_aligned_items(&mut self) {
        for line_index in 0..self.flex_lines.len() {
            if !self.flex_lines[line_index].has_baseline_aligned_items {
                continue;
            }
            let participates = |context: &Self, index: usize| {
                context.alignment_for_item(context.flex_items[index].box_) == align_items::BASELINE
                    && context.style(context.flex_items[index].box_).writing_mode == writing_mode::HORIZONTAL_TB
            };
            let mut max_baseline = CssPixels::default();
            for index in self.flex_lines[line_index].items.iter().copied() {
                if participates(self, index) {
                    max_baseline = max_baseline.max(self.box_baseline(self.flex_items[index].box_));
                }
            }
            for index in self.flex_lines[line_index].items.clone() {
                if participates(self, index) {
                    let baseline = self.box_baseline(self.flex_items[index].box_);
                    self.flex_items[index].cross_offset += max_baseline - baseline;
                }
            }
        }
    }

    fn copy_dimensions_from_flex_items_to_boxes(&mut self) {
        let reference = self.container_used().content_inline_size;
        for index in 0..self.flex_items.len() {
            let style = self.style(self.flex_items[index].box_);
            {
                let used = self.item_used_mut(index);
                used.margin_left = style.margin_left.to_px(reference);
                used.margin_right = style.margin_right.to_px(reference);
                used.margin_top = style.margin_top.to_px(reference);
                used.margin_bottom = style.margin_bottom.to_px(reference);
                used.border_left = style.border_left_width;
                used.border_right = style.border_right_width;
                used.border_top = style.border_top_width;
                used.border_bottom = style.border_bottom_width;
            }
            self.set_main_size(index, self.flex_items[index].main_size.unwrap());
            self.set_cross_size(index, self.flex_items[index].cross_size.unwrap());
        }
    }

    fn layout_inside_item(&mut self, index: usize) {
        let node = self.flex_items[index].box_;
        let mut input = FfiLayoutInput {
            available_space: self
                .item_used(index)
                .available_inner_space_or_constraints_from(self.available_space_for_items.unwrap().space),
            containing_block_constraints: self.item_containing_block_constraints(),
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        };
        if self.facts(node).is_table_wrapper && !self.cross_axis_is_horizontal() && self.flex_item_is_stretched(index) {
            let mut intrinsic_space = input.available_space;
            intrinsic_space.block_size = AvailableSize::indefinite();
            // SAFETY: The wrapper and state remain live during this call.
            let intrinsic_size = unsafe {
                (self.callbacks.compute_table_box_block_size_inside_wrapper)(
                    self.callbacks.context,
                    node,
                    intrinsic_space,
                    input.containing_block_constraints,
                )
            };
            let extra = (self.flex_items[index].cross_size.unwrap() - self.flex_items[index].hypothetical_cross_size)
                .max(CssPixels::default());
            input.has_table_grid_min_border_box_block_size = true;
            input.table_grid_min_border_box_block_size = intrinsic_size + extra;
        }

        let mut result = FfiChildLayoutResult::default();
        // SAFETY: Child layout is synchronous and the bridge retains any
        // returned context until the parent-dimension callback.
        let did_layout = unsafe {
            (self.callbacks.layout_inside_child)(
                self.callbacks.context,
                node,
                LAYOUT_MODE_NORMAL,
                input,
                &raw mut result,
            )
        };
        if did_layout {
            // SAFETY: A successful layout call stored this child context.
            unsafe { (self.callbacks.parent_did_dimension_child_root_box)(self.callbacks.context, node) };
        }

        let container_inline_size = self.container_used().content_inline_size;
        let container_block_size = self.container_used().content_block_size;
        super::abspos::compute_inset_native(
            self.state,
            self.callbacks,
            self.layout_mode,
            self.flex_container,
            node,
            container_inline_size,
            container_block_size,
        );
    }

    fn calculate_static_position_rect(&self, node: Node) -> FfiStaticPositionRect {
        let cross_alignment = match self.alignment_for_item(node) {
            align_items::BASELINE | align_items::FLEX_START | align_items::STRETCH | align_items::NORMAL => {
                if self.cross_axis_is_reverse() {
                    FfiStaticPositionAlignment::End
                } else {
                    FfiStaticPositionAlignment::Start
                }
            }
            align_items::FLEX_END => {
                if self.cross_axis_is_reverse() {
                    FfiStaticPositionAlignment::Start
                } else {
                    FfiStaticPositionAlignment::End
                }
            }
            align_items::START | align_items::SELF_START => FfiStaticPositionAlignment::Start,
            align_items::END | align_items::SELF_END => FfiStaticPositionAlignment::End,
            align_items::CENTER => FfiStaticPositionAlignment::Center,
            _ => FfiStaticPositionAlignment::Start,
        };
        let main_alignment = match self.style(self.flex_container).justify_content {
            justify_content::START | justify_content::LEFT => FfiStaticPositionAlignment::Start,
            justify_content::STRETCH
            | justify_content::NORMAL
            | justify_content::FLEX_START
            | justify_content::SPACE_BETWEEN => {
                if self.is_direction_reverse() {
                    FfiStaticPositionAlignment::End
                } else {
                    FfiStaticPositionAlignment::Start
                }
            }
            justify_content::END | justify_content::RIGHT => FfiStaticPositionAlignment::End,
            justify_content::FLEX_END => {
                if self.is_direction_reverse() {
                    FfiStaticPositionAlignment::Start
                } else {
                    FfiStaticPositionAlignment::End
                }
            }
            justify_content::CENTER | justify_content::SPACE_AROUND | justify_content::SPACE_EVENLY => {
                FfiStaticPositionAlignment::Center
            }
            _ => unreachable!("invalid justify-content"),
        };
        let inline_size = if self.main_axis_is_horizontal() {
            self.inner_main_size_used(self.container_used())
        } else {
            self.inner_cross_size_used(self.container_used())
        };
        let block_size = if self.main_axis_is_horizontal() {
            self.inner_cross_size_used(self.container_used())
        } else {
            self.inner_main_size_used(self.container_used())
        };
        FfiStaticPositionRect {
            rect: crate::geometry::LogicalRect {
                offset: Default::default(),
                size: crate::geometry::LogicalSize {
                    inline_size,
                    block_size,
                },
            },
            inline_alignment: if self.main_axis_is_horizontal() {
                main_alignment
            } else {
                cross_alignment
            },
            block_alignment: if self.main_axis_is_horizontal() {
                cross_alignment
            } else {
                main_alignment
            },
            alignment_derives_from_own_computed_values: true,
        }
    }

    fn axis_direction(is_horizontal: bool, is_reverse: bool) -> AxisDirection {
        match (is_horizontal, is_reverse) {
            (true, false) => AxisDirection::HorizontalLr,
            (true, true) => AxisDirection::HorizontalRl,
            (false, false) => AxisDirection::VerticalTb,
            (false, true) => AxisDirection::VerticalBt,
        }
    }

    fn save_flex_layout_data(&self) {
        let mut item_storage: Vec<Vec<FfiFlexLayoutItem>> = Vec::with_capacity(self.flex_lines.len());
        for line in &self.flex_lines {
            let mut items = Vec::with_capacity(line.items.len());
            for index in line.items.iter().copied() {
                let item = &self.flex_items[index];
                let main_size = item.main_size.unwrap_or(item.target_main_size);
                let cross_size = item.cross_size.unwrap_or(item.hypothetical_cross_size);
                let rect = if self.main_axis_is_horizontal() {
                    FfiFlexLayoutItemRect {
                        x: item.main_offset,
                        y: item.cross_offset,
                        width: main_size,
                        height: cross_size,
                    }
                } else {
                    FfiFlexLayoutItemRect {
                        x: item.cross_offset,
                        y: item.main_offset,
                        width: cross_size,
                        height: main_size,
                    }
                };
                let node = item.box_;
                items.push(FfiFlexLayoutItem {
                    node,
                    rect,
                    main_base_size: item.flex_base_size,
                    main_delta_size: item.target_main_size - item.flex_base_size,
                    main_min_size: if self.has_main_min_size(node) {
                        self.specified_main_min_size(index)
                    } else {
                        self.automatic_minimum_size(index)
                    },
                    main_max_size: if self.has_main_max_size(node) {
                        self.specified_main_max_size(index)
                    } else {
                        item.target_main_size
                    },
                    cross_min_size: if self.has_cross_min_size(node) {
                        self.specified_cross_min_size(index)
                    } else {
                        CssPixels::default()
                    },
                    cross_max_size: if self.has_cross_max_size(node) {
                        self.specified_cross_max_size(index)
                    } else {
                        item.cross_size.unwrap_or(item.hypothetical_cross_size)
                    },
                    clamp_state: if item.is_min_violation {
                        FLEX_CLAMPED_TO_MIN
                    } else if item.is_max_violation {
                        FLEX_CLAMPED_TO_MAX
                    } else {
                        FLEX_UNCLAMPED
                    },
                    flex_grow: self.style(node).flex_grow,
                    flex_shrink: self.style(node).flex_shrink,
                });
            }
            item_storage.push(items);
        }
        let lines: Vec<_> = self
            .flex_lines
            .iter()
            .zip(&item_storage)
            .map(|(line, items)| {
                let cross_start = line
                    .items
                    .iter()
                    .copied()
                    .map(|index| self.flex_items[index].cross_offset)
                    .min()
                    .unwrap_or_default();
                FfiFlexLayoutLine {
                    growth_state: line.growth_state,
                    cross_start,
                    cross_size: line.cross_size,
                    items: items.as_ptr(),
                    item_count: items.len(),
                }
            })
            .collect();
        let style = self.style(self.flex_container);
        let data = FfiFlexLayoutData {
            align_content: style.align_content,
            align_items: style.align_items,
            flex_direction: style.flex_direction,
            flex_wrap: style.flex_wrap,
            justify_content: style.justify_content,
            main_axis_direction: Self::axis_direction(self.main_axis_is_horizontal(), self.is_direction_reverse())
                as u8,
            cross_axis_direction: Self::axis_direction(self.cross_axis_is_horizontal(), self.cross_axis_is_reverse())
                as u8,
            lines: lines.as_ptr(),
            line_count: lines.len(),
        };
        // SAFETY: All pointed-to vectors stay alive for this synchronous copy.
        unsafe {
            (self.callbacks.set_flex_layout_data)(self.callbacks.context, self.flex_container, &raw const data);
        }
    }

    fn cross_size_transferred_from_definite_main_size(&self, index: usize) -> Option<CssPixels> {
        let node = self.flex_items[index].box_;
        if !self.cross_axis_is_horizontal() || !self.has_definite_main_size(index) {
            return None;
        }
        let ratio = self.preferred_aspect_ratio(node)?;
        let main_size = if self.has_definite_main_size_used(self.container_used()) {
            self.flex_items[index]
                .main_size
                .unwrap_or(self.flex_items[index].flex_base_size)
        } else {
            self.flex_items[index].flex_base_size
        };
        Some(self.cross_size_from_main_size_and_aspect_ratio(main_size, ratio))
    }

    fn calculate_cross_content_contribution(
        &self,
        index: usize,
        resolve_percentage_min_max_sizes: bool,
        max_content: bool,
    ) -> CssPixels {
        let node = self.flex_items[index].box_;
        let cross_size_auto = self.should_treat_cross_size_as_auto(node);
        let mut size = if cross_size_auto {
            self.cross_size_transferred_from_definite_main_size(index)
                .unwrap_or_else(|| {
                    if max_content {
                        self.calculate_max_content_cross_size(index)
                    } else {
                        self.calculate_min_content_cross_size(index)
                    }
                })
        } else {
            let (value, property) = self.computed_cross_size(node);
            self.resolve_size_for_axis(index, self.cross_axis_is_horizontal(), value, property)
        };
        if cross_size_auto && self.preferred_aspect_ratio(node).is_some() {
            size = self.adjust_cross_size_through_aspect_ratio(
                node,
                size,
                self.computed_main_min_size(node).0,
                self.computed_main_max_size(node).0,
            );
        }
        let min = self.computed_cross_min_size(node).0;
        let max = self.computed_cross_max_size(node).0;
        let clamp_min = if !min.is_auto() && (resolve_percentage_min_max_sizes || !min.contains_percentage) {
            self.specified_cross_min_size(index)
        } else {
            CssPixels::default()
        };
        let clamp_max = if !self.should_treat_max_size_as_none(node, true)
            && (resolve_percentage_min_max_sizes || !max.contains_percentage)
        {
            self.specified_cross_max_size(index)
        } else {
            CssPixels::from_raw(i32::MAX)
        };
        self.flex_items[index].add_cross_margin_box_sizes(css_clamp(size, clamp_min, clamp_max))
    }

    fn specified_main_max_size_for_intrinsic_contribution(
        &self,
        index: usize,
        available_size: AvailableSize,
    ) -> CssPixels {
        let node = self.flex_items[index].box_;
        let max = self.computed_main_max_size(node).0;
        if self.should_treat_max_size_as_none(node, false) {
            return CssPixels::from_raw(i32::MAX);
        }
        if !max.contains_percentage {
            return self.specified_main_max_size(index);
        }
        if available_size.is_min_content() {
            if self.facts(node).is_replaced_box {
                return CssPixels::default();
            }
            return CssPixels::from_raw(i32::MAX);
        }
        if available_size.is_max_content() {
            return CssPixels::from_raw(i32::MAX);
        }
        self.specified_main_max_size(index)
    }

    fn calculate_main_content_contribution(&self, index: usize, max_content: bool) -> CssPixels {
        let node = self.flex_items[index].box_;
        let intrinsic = if max_content {
            self.calculate_max_content_main_size(index)
        } else {
            self.calculate_min_content_main_size(index)
        };
        let preferred = self.computed_main_size(node);
        let larger = if preferred.0.is_auto() {
            intrinsic
        } else {
            intrinsic.max(self.resolve_size_for_axis(index, self.main_axis_is_horizontal(), preferred.0, preferred.1))
        };
        let clamp_min = if self.has_main_min_size(node) {
            self.specified_main_min_size(index)
        } else {
            self.automatic_minimum_size(index)
        };
        let clamp_max = self.specified_main_max_size_for_intrinsic_contribution(
            index,
            if max_content {
                AvailableSize::max_content()
            } else {
                AvailableSize::min_content()
            },
        );
        self.flex_items[index].add_main_margin_box_sizes(css_clamp(larger, clamp_min, clamp_max))
    }

    fn determine_intrinsic_size_of_flex_container(&mut self) {
        if self
            .available_space_for_items
            .unwrap()
            .main
            .is_intrinsic_sizing_constraint()
        {
            let size = self.calculate_intrinsic_main_size_of_flex_container();
            self.set_container_main_size(size);
        }
        if self
            .available_space_for_items
            .unwrap()
            .cross
            .is_intrinsic_sizing_constraint()
        {
            let size = self.calculate_intrinsic_cross_size_of_flex_container();
            self.set_container_cross_size(size);
        }
    }

    fn calculate_intrinsic_main_size_of_flex_container(&mut self) -> CssPixels {
        if !self.is_single_line() && self.available_space_for_items.unwrap().main.is_min_content() {
            return (0..self.flex_items.len())
                .map(|index| self.calculate_main_content_contribution(index, false))
                .max()
                .unwrap_or_default();
        }

        let min_content = self.available_space_for_items.unwrap().main.is_min_content();
        for index in 0..self.flex_items.len() {
            let contribution = self.calculate_main_content_contribution(index, !min_content);
            let result = contribution - self.flex_items[index].outer_flex_base_size();
            let mut adjusted = result;
            let style = self.style(self.flex_items[index].box_);
            if result > CssPixels::default() {
                adjusted = if style.flex_grow >= 1.0 {
                    result.scaled(1.0 / style.flex_grow)
                } else {
                    result.scaled(style.flex_grow)
                };
            } else if result < CssPixels::default() {
                adjusted = if self.flex_items[index].scaled_flex_shrink_factor == 0.0 {
                    CssPixels::from_raw(i32::MIN)
                } else {
                    result.scaled(1.0 / self.flex_items[index].scaled_flex_shrink_factor)
                };
            }
            self.flex_items[index].desired_flex_fraction = adjusted.to_double();
        }

        self.flex_lines.clear();
        if !self.flex_items.is_empty() {
            self.flex_lines.push(FlexLine {
                items: (0..self.flex_items.len()).collect(),
                ..Default::default()
            });
        }
        for line_index in 0..self.flex_lines.len() {
            let mut greatest = 0.0f32;
            let mut grow_sum = 0.0f32;
            let mut shrink_sum = 0.0f32;
            for index in self.flex_lines[line_index].items.iter().copied() {
                greatest = greatest.max(self.flex_items[index].desired_flex_fraction as f32);
                let style = self.style(self.flex_items[index].box_);
                grow_sum += style.flex_grow as f32;
                shrink_sum += style.flex_shrink as f32;
            }
            let mut chosen = greatest;
            if chosen > 0.0 && grow_sum < 1.0 {
                chosen /= grow_sum;
            }
            if chosen < 0.0 && shrink_sum < 1.0 {
                chosen *= shrink_sum;
            }
            self.flex_lines[line_index].chosen_flex_fraction = chosen as f64;
        }

        let mut largest_sum = CssPixels::default();
        for line_index in 0..self.flex_lines.len() {
            let mut sum = CssPixels::default();
            for index in self.flex_lines[line_index].items.iter().copied() {
                let desired = self.flex_items[index].desired_flex_fraction;
                let style = self.style(self.flex_items[index].box_);
                let product = if desired > 0.0 {
                    self.flex_lines[line_index].chosen_flex_fraction * style.flex_grow
                } else if desired < 0.0 {
                    self.flex_lines[line_index].chosen_flex_fraction * self.flex_items[index].scaled_flex_shrink_factor
                } else {
                    0.0
                };
                let result = self.flex_items[index].flex_base_size + CssPixels::nearest_value_for(product);
                let min = self.computed_main_min_size(self.flex_items[index].box_).0;
                let clamp_min = if !min.is_auto() && !min.contains_percentage {
                    self.specified_main_min_size(index)
                } else {
                    self.automatic_minimum_size(index)
                };
                let clamp_max = self.specified_main_max_size_for_intrinsic_contribution(
                    index,
                    self.available_space_for_items.unwrap().main,
                );
                sum += self.flex_items[index].add_main_margin_box_sizes(css_clamp(result, clamp_min, clamp_max));
            }
            sum += self.main_gap() * self.flex_lines[line_index].items.len().wrapping_sub(1);
            largest_sum = largest_sum.max(sum);
        }
        self.set_container_main_size(largest_sum);
        largest_sum
    }

    fn calculate_intrinsic_cross_size_of_flex_container(&mut self) -> CssPixels {
        let min_content = self.available_space_for_items.unwrap().cross.is_min_content();
        if self.is_single_line() {
            let calculate_largest = |context: &Self, resolve_percentages: bool| {
                (0..context.flex_items.len())
                    .map(|index| context.calculate_cross_content_contribution(index, resolve_percentages, !min_content))
                    .max()
                    .unwrap_or_default()
            };
            let first = calculate_largest(self, false);
            self.set_container_cross_size(first);
            return calculate_largest(self, true);
        }
        if !self.is_row_layout() && min_content {
            let calculate_largest = |context: &Self, resolve_percentages: bool| {
                (0..context.flex_items.len())
                    .map(|index| context.calculate_cross_content_contribution(index, resolve_percentages, false))
                    .max()
                    .unwrap_or_default()
            };
            let first = calculate_largest(self, false);
            self.set_container_cross_size(first);
            return calculate_largest(self, true);
        }
        let mut sum = self
            .flex_lines
            .iter()
            .fold(CssPixels::default(), |sum, line| sum + line.cross_size);
        sum += self.cross_gap() * self.flex_lines.len().wrapping_sub(1);
        sum
    }

    fn run(&mut self, layout_input: FfiLayoutInput) {
        let available_space = layout_input.available_space;
        if self.layout_mode == LAYOUT_MODE_INTRINSIC_SIZING
            && !available_space.inline_size.is_intrinsic_sizing_constraint()
            && !available_space.block_size.is_intrinsic_sizing_constraint()
            && !self.facts(self.flex_container).display.is_inline_outside()
        {
            return;
        }

        self.available_space = Some(available_space);
        self.layout_input = Some(layout_input);
        self.item_percentage_bases =
            self.constraints_for_child_context(self.flex_container, layout_input.containing_block_constraints);
        self.generate_anonymous_flex_items();
        self.determine_available_space_for_items(available_space);

        if self.is_single_line() && self.has_definite_cross_size_used(self.container_used()) {
            let container_cross_size = self.inner_cross_size_used(self.container_used());
            for index in 0..self.flex_items.len() {
                if !self.flex_item_is_stretched(index) {
                    continue;
                }
                let node = self.flex_items[index].box_;
                let min_size = if self.has_cross_min_size(node) {
                    self.specified_cross_min_size(index)
                } else {
                    CssPixels::default()
                };
                let max_size = if self.has_cross_max_size(node) {
                    self.specified_cross_max_size(index)
                } else {
                    CssPixels::from_raw(i32::MAX)
                };
                let outer = css_clamp(container_cross_size, min_size, max_size);
                let item = &self.flex_items[index];
                let inner = outer
                    - item.margins.cross_before
                    - item.margins.cross_after
                    - item.padding.cross_before
                    - item.padding.cross_after
                    - item.borders.cross_before
                    - item.borders.cross_after;
                self.set_cross_size(index, inner);
                self.set_has_definite_cross_size(index);
            }
        }

        for index in 0..self.flex_items.len() {
            self.determine_flex_base_size(index);
        }

        let should_skip_automatic_minimum_size_clamp = self.layout_mode != LAYOUT_MODE_INTRINSIC_SIZING
            && self.is_single_line()
            && self.has_definite_main_size_used(self.container_used())
            && self
                .flex_items
                .iter()
                .enumerate()
                .all(|(index, _)| self.has_definite_main_size(index))
            && self
                .flex_items
                .iter()
                .fold(CssPixels::default(), |sum, item| sum + item.flex_base_size)
                <= self.available_space_for_items.unwrap().main.to_px_or_zero();

        for index in 0..self.flex_items.len() {
            let node = self.flex_items[index].box_;
            let clamp_max = if self.has_main_max_size(node) {
                self.specified_main_max_size(index)
            } else {
                CssPixels::from_raw(i32::MAX)
            };
            let facts = self.facts(node);
            let can_skip_for_item = !facts.is_scroll_container
                && !facts.is_replaced_box
                && !facts.has_preferred_aspect_ratio
                && self.flex_items[index].used_flex_basis_is_definite
                && self.specified_size_suggestion(index) == Some(self.flex_items[index].flex_base_size);
            let clamp_min = if self.has_main_min_size(node) {
                self.specified_main_min_size(index)
            } else if should_skip_automatic_minimum_size_clamp || can_skip_for_item {
                CssPixels::default()
            } else {
                self.automatic_minimum_size(index)
            };
            self.flex_items[index].hypothetical_main_size =
                css_clamp(self.flex_items[index].flex_base_size, clamp_min, clamp_max).max(CssPixels::default());
        }

        self.collect_flex_items_into_flex_lines();
        self.resolve_flexible_lengths();
        for index in 0..self.flex_items.len() {
            self.determine_hypothetical_cross_size_of_item(index);
        }
        self.calculate_cross_size_of_each_flex_line();
        self.handle_align_content_stretch();
        self.determine_used_cross_size_of_each_flex_item();
        self.distribute_any_remaining_free_space();
        self.resolve_cross_axis_auto_margins();
        self.align_all_flex_items_along_the_cross_axis();

        if self.should_treat_cross_size_as_auto(self.flex_container) {
            for index in 0..self.flex_items.len() {
                let size = self.flex_items[index].cross_size.unwrap();
                self.set_cross_size(index, size);
                self.set_has_definite_cross_size(index);
            }
        }
        self.align_all_flex_lines();

        if available_space.inline_size.is_intrinsic_sizing_constraint()
            || available_space.block_size.is_intrinsic_sizing_constraint()
        {
            self.determine_intrinsic_size_of_flex_container();
        } else {
            self.copy_dimensions_from_flex_items_to_boxes();
            for index in 0..self.flex_items.len() {
                self.layout_inside_item(index);
            }
            self.resolve_baseline_aligned_items();
            for index in 0..self.flex_items.len() {
                let item = &self.flex_items[index];
                let offset = if self.main_axis_is_horizontal() {
                    FfiCssPixelPoint {
                        x: item.main_offset,
                        y: item.cross_offset,
                    }
                } else {
                    FfiCssPixelPoint {
                        x: item.cross_offset,
                        y: item.main_offset,
                    }
                };
                // SAFETY: The item has live used-values storage.
                unsafe { (self.callbacks.place_child)(self.callbacks.context, item.box_, offset) };
            }
            // SAFETY: The container has live used-values storage.
            unsafe {
                (self.callbacks.compute_and_store_baselines)(self.callbacks.context, self.flex_container);
            }
        }

        if self.should_collect_devtools_layout_data {
            self.save_flex_layout_data();
        }
    }
}

pub(crate) fn run(instance: &mut FormattingContextInstance, input: FfiLayoutInput) {
    let mut context = FlexFormattingContext::new(instance);
    context.run(input);
    instance.automatic_content_inline_size = context.container_used().content_inline_size;
    instance.automatic_content_block_size = context.container_used().content_block_size;
}

pub(crate) fn parent_did_dimension(instance: &mut FormattingContextInstance) {
    if instance.layout_mode != LAYOUT_MODE_NORMAL {
        return;
    }
    let context = FlexFormattingContext::new(instance);
    let mut child = context.navigate(context.callbacks.navigation.first_child, context.flex_container);
    while !child.is_null() {
        let next = context.navigate(context.callbacks.navigation.next_sibling, child);
        let facts = context.facts(child);
        if facts.is_box && facts.is_absolutely_positioned {
            // SAFETY: Registration is synchronous and the host owns the child.
            unsafe {
                (context.callbacks.register_contained_abspos_child)(
                    context.callbacks.context,
                    child,
                    context.calculate_static_position_rect(child),
                );
            }
        }
        child = next;
    }
}

fn css_clamp(value: CssPixels, min: CssPixels, max: CssPixels) -> CssPixels {
    min.max(value.min(max))
}

fn clamp_to_max_dimension_value(value: CssPixels) -> CssPixels {
    if matches!(value.raw_value(), i32::MIN | i32::MAX) {
        CssPixels::from_integer(17_895_700)
    } else {
        value
    }
}

fn abs(value: CssPixels) -> CssPixels {
    if value < CssPixels::default() { -value } else { value }
}

fn order_modified_keys<T>(buckets: &HashMap<i32, Vec<T>>, reverse: bool) -> Vec<i32> {
    let mut keys: Vec<_> = buckets.keys().copied().collect();
    if reverse {
        keys.sort_unstable_by(|left, right| right.cmp(left));
    } else {
        keys.sort_unstable();
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[derive(Clone, Copy)]
    struct FixtureItem {
        base: CssPixels,
        hypothetical: CssPixels,
        min: CssPixels,
        max: CssPixels,
        factor: f64,
        target: CssPixels,
        frozen: bool,
    }

    fn resolve_growing_fixture(container: CssPixels, items: &mut [FixtureItem]) {
        for item in items.iter_mut() {
            item.target = item.base;
            item.frozen = item.factor == 0.0 || item.base > item.hypothetical;
            if item.frozen {
                item.target = item.hypothetical;
            }
        }
        while items.iter().any(|item| !item.frozen) {
            let occupied = items.iter().fold(CssPixels::default(), |sum, item| {
                sum + if item.frozen { item.target } else { item.base }
            });
            let free = container - occupied;
            let factor_sum: f64 = items.iter().filter(|item| !item.frozen).map(|item| item.factor).sum();
            for item in items.iter_mut().filter(|item| !item.frozen) {
                item.target = item.base + free.scaled(item.factor / factor_sum);
            }
            let mut violation = CssPixels::default();
            let mut min_violations = vec![false; items.len()];
            let mut max_violations = vec![false; items.len()];
            for (index, item) in items.iter_mut().enumerate() {
                if item.frozen {
                    continue;
                }
                let original = item.target;
                item.target = css_clamp(original, item.min, item.max).max(CssPixels::default());
                min_violations[index] = item.target > original;
                max_violations[index] = item.target < original;
                violation += item.target - original;
            }
            for (index, item) in items.iter_mut().enumerate() {
                if item.frozen {
                    continue;
                }
                item.frozen = violation == CssPixels::default()
                    || (violation > CssPixels::default() && min_violations[index])
                    || (violation < CssPixels::default() && max_violations[index]);
            }
        }
    }

    fn main_distribution(
        justify: u8,
        reverse: bool,
        container: CssPixels,
        used: CssPixels,
        remaining: Option<CssPixels>,
        count: usize,
    ) -> (CssPixels, CssPixels) {
        let mut initial = CssPixels::default();
        let mut between = CssPixels::default();
        match justify {
            justify_content::CENTER => {
                initial = (container - used) / 2;
                if reverse {
                    initial = container - initial;
                }
            }
            justify_content::SPACE_BETWEEN => {
                if reverse {
                    initial = container;
                }
                if let Some(free) = remaining
                    && count > 1
                {
                    between = (free / (count - 1)).max(CssPixels::default());
                }
            }
            justify_content::SPACE_AROUND => {
                if let Some(free) = remaining {
                    between = (free / count).max(CssPixels::default());
                }
                initial = if reverse { container - between / 2 } else { between / 2 };
            }
            _ => {}
        }
        (initial, between)
    }

    #[test]
    fn flexible_length_loop_refreezes_after_a_max_violation() {
        let mut items = [
            FixtureItem {
                base: px(50),
                hypothetical: px(50),
                min: px(0),
                max: px(80),
                factor: 1.0,
                target: px(0),
                frozen: false,
            },
            FixtureItem {
                base: px(50),
                hypothetical: px(50),
                min: px(0),
                max: px(500),
                factor: 1.0,
                target: px(0),
                frozen: false,
            },
        ];
        resolve_growing_fixture(px(300), &mut items);
        assert_eq!(items[0].target, px(80));
        assert_eq!(items[1].target, px(220));
        assert!(items.iter().all(|item| item.frozen));
    }

    #[test]
    fn main_alignment_distribution_handles_reverse_and_negative_space() {
        assert_eq!(
            main_distribution(justify_content::CENTER, true, px(100), px(40), Some(px(60)), 2),
            (px(70), px(0))
        );
        assert_eq!(
            main_distribution(
                justify_content::SPACE_BETWEEN,
                false,
                px(100),
                px(120),
                Some(px(-20)),
                3
            ),
            (px(0), px(0))
        );
        assert_eq!(
            main_distribution(justify_content::SPACE_AROUND, false, px(100), px(40), Some(px(60)), 3),
            (px(10), px(20))
        );
    }

    #[test]
    fn order_modified_buckets_keep_document_order_within_equal_orders() {
        let mut buckets = HashMap::new();
        buckets.insert(2, vec!['a', 'c']);
        buckets.insert(1, vec!['b']);
        let collect = |reverse| {
            order_modified_keys(&buckets, reverse)
                .into_iter()
                .flat_map(|key| buckets[&key].iter().copied())
                .collect::<Vec<_>>()
        };
        assert_eq!(collect(false), ['b', 'a', 'c']);
        assert_eq!(collect(true), ['a', 'c', 'b']);
    }
}
