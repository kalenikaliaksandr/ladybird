/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::{
    FfiFlexAxis, FfiFlexSizeProperty, FfiIntrinsicSizeCacheKey, FfiIntrinsicSizeCacheKind, FfiLayoutFcCallbacks,
    FfiMeasurementState,
};
use crate::box_facts::FfiLayoutBoxFacts;
use crate::css_pixels::CssPixels;
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::{AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput};
use crate::layout_state::state_mut;
use crate::style_facts::{FfiSizeValue, FfiStyleFacts};
use crate::used_values::{FfiSizeConstraint, UsedValuesCore};
use std::ffi::c_void;

pub(crate) type Node = *mut c_void;

const BOX_SIZING_BORDER_BOX: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CyclicPercentageIntrinsicContribution {
    NotCyclic,
    ResolveAsZero,
    TreatAsInitialValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CyclicPercentageSizeProperty {
    PreferredOrMaxSize,
    MinSize,
}

#[derive(Clone, Copy, Debug)]
struct PixelFraction {
    numerator: CssPixels,
    denominator: CssPixels,
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

struct MeasurementState {
    callbacks: FfiLayoutFcCallbacks,
    handles: FfiMeasurementState,
}

impl MeasurementState {
    fn create(callbacks: FfiLayoutFcCallbacks, node: Node, constraints: FfiContainingBlockConstraints) -> Self {
        bump(FfiOp::MeasurementStateCreateCallback);
        // SAFETY: The callback synchronously allocates a measurement-only C++
        // wrapper and returns its paired Rust state and root core.
        let handles = unsafe { (callbacks.create_measurement_state)(callbacks.context, node, constraints) };
        assert!(!handles.cpp_state.is_null());
        assert!(!handles.rust_state.is_null());
        assert!(!handles.root_used_values.is_null());
        Self { callbacks, handles }
    }

    fn root_used_mut(&mut self) -> &mut UsedValuesCore {
        // SAFETY: The measurement state owns the root entry until Drop.
        unsafe { &mut *self.handles.root_used_values }
    }

    fn run(&self, node: Node, input: FfiLayoutInput) -> super::FfiChildLayoutResult {
        bump(FfiOp::MeasurementContextRunCallback);
        // SAFETY: The C++ measurement wrapper, node, and input remain live
        // for this synchronous dispatcher call.
        unsafe { (self.callbacks.run_measurement_context)(self.callbacks.context, self.handles.cpp_state, node, input) }
    }
}

impl Drop for MeasurementState {
    fn drop(&mut self) {
        bump(FfiOp::MeasurementStateDestroyCallback);
        // SAFETY: Ownership of the C++ wrapper is returned exactly once.
        unsafe {
            (self.callbacks.destroy_measurement_state)(self.callbacks.context, self.handles.cpp_state);
        }
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
        CssPixels::from_raw((wide / self.denominator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }

    fn divide(self, value: CssPixels) -> CssPixels {
        if self.numerator == CssPixels::default() {
            return CssPixels::default();
        }
        let wide = value.raw_value() as i64 * self.denominator.raw_value() as i64;
        CssPixels::from_raw((wide / self.numerator.raw_value() as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }
}

fn cyclic_percentage_intrinsic_contribution(
    is_replaced_box: bool,
    size_contains_percentage: bool,
    available_size: AvailableSize,
    size_property: CyclicPercentageSizeProperty,
) -> CyclicPercentageIntrinsicContribution {
    if !size_contains_percentage {
        return CyclicPercentageIntrinsicContribution::NotCyclic;
    }
    if size_property == CyclicPercentageSizeProperty::MinSize && available_size.is_intrinsic_sizing_constraint() {
        return CyclicPercentageIntrinsicContribution::ResolveAsZero;
    }
    if available_size.is_min_content() {
        if is_replaced_box {
            return CyclicPercentageIntrinsicContribution::ResolveAsZero;
        }
        return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
    }
    if available_size.is_max_content() {
        return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
    }
    CyclicPercentageIntrinsicContribution::NotCyclic
}

fn subtract_border_box_adjustment(
    value: CssPixels,
    before_border: CssPixels,
    before_padding: CssPixels,
    after_border: CssPixels,
    after_padding: CssPixels,
) -> CssPixels {
    (value - before_border - before_padding - after_border - after_padding).max(CssPixels::default())
}

fn content_block_size_from_aspect_ratio_values(
    content_inline_size: CssPixels,
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
        return (ratio.divide(content_inline_size + inline_before + inline_after) - block_before - block_after)
            .max(CssPixels::default());
    }
    ratio.divide(content_inline_size)
}

fn content_inline_size_from_aspect_ratio_values(
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

    fn style(&self, node: Node) -> FfiStyleFacts {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn used(&self, node: Node) -> &UsedValuesCore {
        let used = state_mut(self.state).used_values(&self.callbacks, node);
        // SAFETY: The Rust layout state owns this stable entry.
        unsafe { &*used }
    }

    fn parent(&self, node: Node) -> Node {
        bump(FfiOp::NavigationCallback);
        // SAFETY: Navigation is synchronous and the host owns the node tree.
        unsafe { (self.callbacks.navigation.parent)(self.callbacks.navigation.context, node) }
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
            style.box_sizing_for_aspect_ratio == BOX_SIZING_BORDER_BOX,
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
            style.box_sizing_for_aspect_ratio == BOX_SIZING_BORDER_BOX,
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

        if facts.has_preferred_aspect_ratio {
            if let Some(size) = constraints.definite_size_in_ratio_determining_axis {
                return Some(if is_inline_axis {
                    self.content_inline_size_from_aspect_ratio(node, size)
                } else {
                    self.content_block_size_from_aspect_ratio(node, size)
                });
            }

            let style = self.style(node);
            let size_from_min_inline = if let Some(inline_size) = constraints.minimum_inline_size {
                Some(if is_inline_axis {
                    inline_size
                } else {
                    self.content_block_size_from_aspect_ratio(node, inline_size)
                })
            } else if style.min_width.is_length_percentage() && !style.min_width.contains_percentage {
                let inline_size = style.min_width.to_px(CssPixels::default());
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
            } else if style.min_height.is_length_percentage() && !style.min_height.contains_percentage {
                let block_size = style.min_height.to_px(CssPixels::default());
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
                    facts.initial_containing_block_inline_size
                } else {
                    self.content_block_size_from_aspect_ratio(node, facts.initial_containing_block_inline_size)
                }),
            };
        }

        let min_size = if is_inline_axis {
            self.style(node).min_width
        } else {
            self.style(node).min_height
        };
        if min_size.is_length_percentage() && !min_size.contains_percentage {
            return Some(min_size.to_px(CssPixels::default()));
        }
        Some(CssPixels::from_integer(if is_inline_axis { 300 } else { 150 }))
    }

    fn tentative_inline_size_for_replaced_element(
        &self,
        node: Node,
        computed_inline_size: FfiSizeValue,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
    ) -> CssPixels {
        if computed_inline_size.is_percentage() && !constraints.has_percentage_basis_inline_size {
            return CssPixels::default();
        }
        let style = self.style(node);
        let computed_block_size = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
            FfiSizeValue::auto_value()
        } else {
            style.height
        };
        let used_inline_size = if computed_inline_size.is_auto() {
            computed_inline_size.to_px(available_space.inline_size.to_px_or_zero())
        } else {
            self.calculate_inner_inline_size(node, available_space.inline_size, computed_inline_size, constraints)
        };
        let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
        if computed_block_size.is_auto()
            && computed_inline_size.is_auto()
            && let Some(width) = intrinsic.width
        {
            return width;
        }
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
                style.width.contains_percentage,
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
        let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
        if self.should_treat_inline_size_as_auto(node, available_space)
            && self.should_treat_block_size_as_auto(node, available_space, constraints)
            && let Some(height) = intrinsic.height
        {
            return height;
        }
        if computed_block_size.is_auto() && self.facts(node).has_preferred_aspect_ratio {
            return self.content_block_size_from_aspect_ratio(node, self.used(node).content_inline_size);
        }
        if computed_block_size.is_auto() {
            return intrinsic.height.unwrap_or_else(|| CssPixels::from_integer(150));
        }
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
        let style = self.style(node);
        let min_inline = if style.min_width.is_auto() {
            CssPixels::default()
        } else {
            self.calculate_inner_inline_size(node, available_space.inline_size, style.min_width, constraints)
        };
        let specified_max_inline =
            if self.should_treat_max_inline_size_as_none(node, available_space.inline_size, constraints) {
                input_inline_size
            } else {
                self.calculate_inner_inline_size(node, available_space.inline_size, style.max_width, constraints)
            };
        let max_inline = min_inline.max(specified_max_inline);
        let min_block = if style.min_height.is_auto() {
            CssPixels::default()
        } else {
            self.calculate_inner_block_size(node, available_space, style.min_height, constraints)
        };
        let specified_max_block =
            if self.should_treat_max_block_size_as_none(node, available_space.block_size, constraints) {
                input_block_size
            } else {
                self.calculate_inner_block_size(node, available_space, style.max_height, constraints)
            };
        let max_block = min_block.max(specified_max_block);

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
        let style = self.style(node);
        let computed_inline = if self.should_treat_inline_size_as_auto(node, available_space) {
            FfiSizeValue::auto_value()
        } else {
            style.width
        };
        let computed_block = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
            FfiSizeValue::auto_value()
        } else {
            style.height
        };
        let mut used =
            self.tentative_inline_size_for_replaced_element(node, computed_inline, available_space, constraints);
        if computed_inline.is_auto() && computed_block.is_auto() && self.facts(node).has_preferred_aspect_ratio {
            let block =
                self.tentative_block_size_for_replaced_element(node, computed_block, available_space, constraints);
            used = self
                .solve_replaced_size_constraint(node, used, block, available_space, constraints)
                .0;
        }
        if !self.should_treat_max_inline_size_as_none(node, available_space.inline_size, constraints) {
            let max = self.calculate_inner_inline_size(node, available_space.inline_size, style.max_width, constraints);
            if used > max {
                used = self.tentative_inline_size_for_replaced_element(
                    node,
                    style.max_width,
                    available_space,
                    constraints,
                );
            }
        }
        if !style.min_width.is_auto() {
            let min = self.calculate_inner_inline_size(node, available_space.inline_size, style.min_width, constraints);
            if used < min {
                used = self.tentative_inline_size_for_replaced_element(
                    node,
                    style.min_width,
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
        let style = self.style(node);
        let computed_inline = if self.should_treat_inline_size_as_auto(node, available_space) {
            FfiSizeValue::auto_value()
        } else {
            style.width
        };
        let computed_block = if self.should_treat_block_size_as_auto(node, available_space, constraints) {
            FfiSizeValue::auto_value()
        } else {
            style.height
        };
        let mut used =
            self.tentative_block_size_for_replaced_element(node, computed_block, available_space, constraints);
        if computed_inline.is_auto() && computed_block.is_auto() && self.facts(node).has_preferred_aspect_ratio {
            let intrinsic = self.intrinsic_size_for_replaced_sizing(node);
            if intrinsic.width.is_some() || intrinsic.height.is_none() {
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
        if !self.should_treat_max_block_size_as_none(node, available_space.block_size, constraints) {
            let max = self.calculate_inner_block_size(node, available_space, style.max_height, constraints);
            if used > max {
                used = self.tentative_block_size_for_replaced_element(
                    node,
                    style.max_height,
                    available_space,
                    constraints,
                );
            }
        }
        if !style.min_height.is_auto() {
            let min = self.calculate_inner_block_size(node, available_space, style.min_height, constraints);
            if used < min {
                used = self.tentative_block_size_for_replaced_element(
                    node,
                    style.min_height,
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
        if facts.has_replaced_element_table_display_adjustment
            || (facts.is_replaced_box && facts.has_auto_content_box_size)
        {
            return true;
        }
        if facts.has_preferred_aspect_ratio || facts.has_auto_content_box_size {
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

        let (has_quirks_block, quirks_block) = if facts.is_viewport
            || facts.is_table_cell
            || !style.height.is_auto()
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
        let size = style.width;
        if size.is_auto() {
            return true;
        }
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
        if facts.has_preferred_aspect_ratio && size.is_intrinsic_sizing_constraint() {
            if !facts.has_auto_content_height {
                return true;
            }
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
        let size = style.height;
        let facts = self.facts(node);
        if size.is_auto() {
            if self.used(node).has_definite_inline_size() && facts.has_preferred_aspect_ratio {
                return false;
            }
            return true;
        }
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
            if !facts.is_absolutely_positioned {
                let parent = self.parent(node);
                let parent_is_flex_or_grid = if parent.is_null() {
                    false
                } else {
                    let display = self.facts(parent).display;
                    display.is_flex_inside() || display.is_grid_inside()
                };
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
        if facts.has_preferred_aspect_ratio && size.is_intrinsic_sizing_constraint() {
            if !facts.has_auto_content_width {
                return true;
            }
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
        let size = self.style(node).max_width;
        if size.is_none() || (available.is_max_content() && size.is_max_content()) {
            return true;
        }
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
        let size = self.style(node).max_height;
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
        let facts = self.facts(node);
        let style = self.style(node);
        if !facts.is_replaced_box
            || !facts.has_preferred_aspect_ratio
            || !style.width.is_auto()
            || style.height.is_auto()
            || style.height.is_intrinsic_sizing_constraint()
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
        if facts.is_replaced_box && (style.width.contains_percentage || style.max_width.contains_percentage) {
            if !style.min_width.is_length_percentage() {
                return CssPixels::default();
            }
            let mut zero_constraints = constraints;
            zero_constraints.has_percentage_basis_inline_size = true;
            zero_constraints.percentage_basis_inline_size = CssPixels::default();
            return self.calculate_inner_inline_size(
                node,
                AvailableSize::min_content(),
                style.min_width,
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
        let resolve_block_size = |size: FfiSizeValue, property: CyclicPercentageSizeProperty| -> Option<CssPixels> {
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
            resolve_destination_inline_size(style.min_width, CyclicPercentageSizeProperty::MinSize);
        let definite_minimum_block_size = resolve_block_size(style.min_height, CyclicPercentageSizeProperty::MinSize);
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
                    resolve_block_size(style.max_height, CyclicPercentageSizeProperty::PreferredOrMaxSize)
            {
                let mut transferred_minimum =
                    definite_minimum_block_size.map(|value| self.content_inline_size_from_aspect_ratio(node, value));
                if let Some(value) = transferred_minimum {
                    transferred_minimum =
                        resolve_destination_inline_size(style.width, CyclicPercentageSizeProperty::PreferredOrMaxSize)
                            .map_or(Some(value), |resolved| Some(value.min(resolved)));
                    let value = transferred_minimum.unwrap();
                    transferred_minimum = resolve_destination_inline_size(
                        style.max_width,
                        CyclicPercentageSizeProperty::PreferredOrMaxSize,
                    )
                    .map_or(Some(value), |resolved| Some(value.min(resolved)));
                }

                let mut transferred_maximum =
                    self.content_inline_size_from_aspect_ratio(node, definite_maximum_block_size);
                if let Some(resolved) =
                    resolve_destination_inline_size(style.width, CyclicPercentageSizeProperty::PreferredOrMaxSize)
                {
                    transferred_maximum = transferred_maximum.max(resolved);
                }
                if let Some(resolved) =
                    resolve_destination_inline_size(style.min_width, CyclicPercentageSizeProperty::MinSize)
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
        let facts = self.facts(node);
        if facts.is_block_container || facts.is_table_box {
            return self.calculate_max_content_block_size(node, inline_size, constraints);
        }
        let auto_size = self.auto_content_size(node);
        if let Some(height) = auto_size.height {
            return auto_size.aspect_ratio.map_or(height, |ratio| ratio.divide(inline_size));
        }
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

    pub(crate) fn calculate_fit_content_size(
        &self,
        node: Node,
        axis: FfiFlexAxis,
        available_space: AvailableSpace,
        constraints: FfiContainingBlockConstraints,
    ) -> CssPixels {
        match axis {
            FfiFlexAxis::Inline => {
                if available_space.inline_size.is_definite() {
                    let stretch = self.calculate_stretch_fit_inline_size(node, available_space.inline_size);
                    let max_content = self.calculate_max_content_inline_size(node, constraints);
                    if max_content <= stretch {
                        return max_content;
                    }
                    return self.calculate_min_content_inline_size(node, constraints).max(stretch);
                }
                if available_space.inline_size.is_min_content() {
                    return self.calculate_min_content_inline_size(node, constraints);
                }
                self.calculate_max_content_inline_size(node, constraints)
            }
            FfiFlexAxis::Block => {
                let inline_size = available_space.inline_size.to_px_or_zero();
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
                if available_space.block_size.is_min_content() {
                    return self.calculate_min_content_block_size(node, inline_size, constraints);
                }
                self.calculate_max_content_block_size(node, inline_size, constraints)
            }
        }
    }

    fn calculate_inner_inline_size(
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

    fn calculate_inner_block_size(
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
        if preferred_size.contains_percentage && available_space.block_size.is_indefinite() {
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
            FfiFlexSizeProperty::Width => style.width,
            FfiFlexSizeProperty::Height => style.height,
            FfiFlexSizeProperty::MinWidth => style.min_width,
            FfiFlexSizeProperty::MinHeight => style.min_height,
            FfiFlexSizeProperty::MaxWidth => style.max_width,
            FfiFlexSizeProperty::MaxHeight => style.max_height,
            FfiFlexSizeProperty::FlexBasis => style.flex_basis,
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
        self.calculate_inner_inline_size(node, available, self.style(node).width, constraints)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn cyclic_percentage_rules_match_property_axis_and_replaced_status() {
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                false,
                true,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::TreatAsInitialValue
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                true,
                true,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::ResolveAsZero
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                false,
                true,
                AvailableSize::max_content(),
                CyclicPercentageSizeProperty::MinSize,
            ),
            CyclicPercentageIntrinsicContribution::ResolveAsZero
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                true,
                false,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::NotCyclic
        );
    }

    #[test]
    fn border_box_adjustment_preserves_cpp_operation_order_and_floor() {
        assert_eq!(
            subtract_border_box_adjustment(px(100), px(3), px(5), px(7), px(11)),
            px(74)
        );
        assert_eq!(
            subtract_border_box_adjustment(px(10), px(3), px(5), px(7), px(11)),
            CssPixels::default()
        );
    }

    #[test]
    fn aspect_ratio_transfer_accounts_for_border_box_edges() {
        let ratio = PixelFraction {
            numerator: px(2),
            denominator: px(1),
        };
        assert_eq!(
            content_block_size_from_aspect_ratio_values(px(80), ratio, true, px(10), px(10), px(5), px(5),),
            px(40)
        );
        assert_eq!(
            content_inline_size_from_aspect_ratio_values(px(40), ratio, true, px(10), px(10), px(5), px(5),),
            px(80)
        );
    }
}
