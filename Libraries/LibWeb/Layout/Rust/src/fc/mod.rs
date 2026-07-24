/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::box_facts::{FfiLayoutBoxFacts, FfiLayoutNavCallbacks};
use crate::css_pixels::CssPixels;
#[cfg(test)]
use crate::display::FfiDisplay;
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::{AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput, LogicalOffset};
use crate::layout_state::{FfiStaticPositionAlignment, FfiStaticPositionRect, state_mut};
use crate::style_facts::FfiStyleFacts;
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use std::ffi::c_void;

mod flex;
mod sizing;
mod table;

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
#[repr(C)]
pub struct FfiMeasuredCellContent {
    pub content_block_size: CssPixels,
    pub first_baseline: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCaptionLayoutResult {
    pub margin_box_block_size: CssPixels,
    pub pending_table_block_offset: CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiMeasurementState {
    pub cpp_state: *mut c_void,
    pub rust_state: *mut c_void,
    pub root_used_values: *mut UsedValuesCore,
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

pub type FfiBuildStyleFactsCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> FfiStyleFacts;
pub type FfiBuildBoxFactsCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> FfiLayoutBoxFacts;
pub type FfiBuildTableBoxFactsCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> FfiTableBoxFacts;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiLayoutFcCallbacks {
    pub context: *mut c_void,
    pub navigation: FfiLayoutNavCallbacks,
    pub build_style_facts: FfiBuildStyleFactsCallback,
    pub build_box_facts: FfiBuildBoxFactsCallback,
    pub build_table_box_facts: FfiBuildTableBoxFactsCallback,
    pub create_used_values:
        unsafe extern "C" fn(*mut c_void, *mut c_void, bool, CssPixels, bool, CssPixels) -> *mut UsedValuesCore,
    pub get_used_values: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut UsedValuesCore,
    pub layout_inside_child:
        unsafe extern "C" fn(*mut c_void, *mut c_void, u8, FfiLayoutInput, *mut FfiChildLayoutResult) -> bool,
    pub parent_did_dimension_child_root_box: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub discard_child_context: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub calculate_min_content_inline_size:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiContainingBlockConstraints) -> CssPixels,
    pub calculate_max_content_inline_size:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiContainingBlockConstraints) -> CssPixels,
    pub calculate_min_content_block_size:
        unsafe extern "C" fn(*mut c_void, *mut c_void, CssPixels, FfiContainingBlockConstraints) -> CssPixels,
    pub calculate_max_content_block_size:
        unsafe extern "C" fn(*mut c_void, *mut c_void, CssPixels, FfiContainingBlockConstraints) -> CssPixels,
    pub set_table_cell_coordinates: unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize, usize, usize),
    pub set_override_borders_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *const FfiBordersData),
    pub place_child: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiCssPixelPoint),
    pub box_baseline: unsafe extern "C" fn(*mut c_void, *mut c_void, u8) -> CssPixels,
    pub compute_and_store_baselines: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub layout_absolutely_positioned_children: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub layout_table_caption: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        u8,
        AvailableSpace,
        FfiContainingBlockConstraints,
        *mut FfiCaptionLayoutResult,
    ) -> bool,
    pub measure_table_cell_content: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        u8,
        *const UsedValuesCore,
        AvailableSpace,
        *mut FfiMeasuredCellContent,
    ) -> bool,
    pub should_treat_max_inline_size_as_none:
        unsafe extern "C" fn(*mut c_void, *mut c_void, AvailableSize, FfiContainingBlockConstraints) -> bool,
    pub calculate_inner_inline_size:
        unsafe extern "C" fn(*mut c_void, *mut c_void, AvailableSize, FfiContainingBlockConstraints) -> CssPixels,
    pub constraints_for_child_context:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiContainingBlockConstraints) -> FfiContainingBlockConstraints,
    pub can_skip_is_anonymous_text_run: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub set_flex_item: unsafe extern "C" fn(*mut c_void, *mut c_void, bool),
    pub calculate_fit_content_size: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        FfiFlexAxis,
        AvailableSpace,
        FfiContainingBlockConstraints,
    ) -> CssPixels,
    pub calculate_inner_size_for_property: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        FfiFlexAxis,
        FfiFlexSizeProperty,
        AvailableSpace,
        FfiContainingBlockConstraints,
    ) -> CssPixels,
    pub should_treat_size_as_auto: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        FfiFlexAxis,
        AvailableSpace,
        FfiContainingBlockConstraints,
    ) -> bool,
    pub should_treat_max_block_size_as_none:
        unsafe extern "C" fn(*mut c_void, *mut c_void, AvailableSize, FfiContainingBlockConstraints) -> bool,
    pub create_measurement_state:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiContainingBlockConstraints) -> FfiMeasurementState,
    pub destroy_measurement_state: unsafe extern "C" fn(*mut c_void, *mut c_void),
    pub run_measurement_context:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, FfiLayoutInput) -> FfiChildLayoutResult,
    pub intrinsic_size_cache_get: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        FfiIntrinsicSizeCacheKind,
        FfiIntrinsicSizeCacheKey,
        *mut CssPixels,
    ) -> bool,
    pub intrinsic_size_cache_put:
        unsafe extern "C" fn(*mut c_void, *mut c_void, FfiIntrinsicSizeCacheKind, FfiIntrinsicSizeCacheKey, CssPixels),
    pub compute_table_box_block_size_inside_wrapper:
        unsafe extern "C" fn(*mut c_void, *mut c_void, AvailableSpace, FfiContainingBlockConstraints) -> CssPixels,
    pub register_contained_abspos_child: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiStaticPositionRect),
    pub compute_inset: unsafe extern "C" fn(*mut c_void, *mut c_void, CssPixels, CssPixels),
    pub set_flex_layout_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *const FfiFlexLayoutData),
}

struct FormattingContextInstance {
    state: *mut c_void,
    box_: *mut c_void,
    fc_type: u8,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    should_collect_devtools_layout_data: bool,
    automatic_content_inline_size: CssPixels,
    automatic_content_block_size: CssPixels,
    pending_table_box_content_offset_in_wrapper: LogicalOffset,
}

fn instance_mut(fc: *mut c_void) -> &'static mut FormattingContextInstance {
    assert!(!fc.is_null());
    // SAFETY: FC pointers are created below, owned by one C++ shim, and
    // returned for destruction exactly once.
    unsafe { &mut *fc.cast::<FormattingContextInstance>() }
}

fn formatting_context_type_created_by_box(facts: FfiLayoutBoxFacts) -> Option<FfiFormattingContextType> {
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
pub extern "C" fn rust_layout_owns_fc_type(fc_type: u8) -> bool {
    abort_on_panic(|| {
        matches!(
            fc_type,
            type_ if type_ == FfiFormattingContextType::Flex as u8
                || type_ == FfiFormattingContextType::Table as u8
        )
    })
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

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_create(
    state: *mut c_void,
    box_: *mut c_void,
    fc_type: u8,
    layout_mode: u8,
    should_collect_devtools_layout_data: bool,
    callbacks: *const FfiLayoutFcCallbacks,
) -> *mut c_void {
    abort_on_panic(|| {
        bump(FfiOp::FcCreate);
        assert!(!state.is_null());
        assert!(!box_.is_null());
        assert!(!callbacks.is_null());
        // SAFETY: C++ passes a live callback table and Rust copies it before
        // returning across the boundary.
        let callbacks = unsafe { *callbacks };
        let _ = state_mut(state).box_facts(&callbacks, box_);
        Box::into_raw(Box::new(FormattingContextInstance {
            state,
            box_,
            fc_type,
            layout_mode,
            callbacks,
            should_collect_devtools_layout_data,
            automatic_content_inline_size: CssPixels::default(),
            automatic_content_block_size: CssPixels::default(),
            pending_table_box_content_offset_in_wrapper: LogicalOffset::default(),
        }))
        .cast()
    })
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
                // SAFETY: Registration is synchronous and the host owns the child.
                unsafe {
                    (instance.callbacks.register_contained_abspos_child)(
                        instance.callbacks.context,
                        child,
                        FfiStaticPositionRect {
                            rect: Default::default(),
                            inline_alignment: FfiStaticPositionAlignment::Start,
                            block_alignment: FfiStaticPositionAlignment::Start,
                            alignment_derives_from_own_computed_values: false,
                        },
                    );
                }
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

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_parent_did_dimension(fc: *mut c_void) {
    abort_on_panic(|| {
        let instance = instance_mut(fc);
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
            _ => panic!("no Rust parent-dimension implementation for this formatting context"),
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_destroy(fc: *mut c_void) {
    abort_on_panic(|| {
        bump(FfiOp::FcDestroy);
        assert!(!fc.is_null());
        // SAFETY: Ownership is transferred back from the C++ shim exactly
        // once for a pointer returned by rust_layout_fc_create.
        unsafe {
            drop(Box::from_raw(fc.cast::<FormattingContextInstance>()));
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_run(fc: *mut c_void, _input: FfiLayoutInput) {
    abort_on_panic(|| {
        bump(FfiOp::FcRun);
        let instance = instance_mut(fc);
        match instance.fc_type {
            type_ if type_ == FfiFormattingContextType::Table as u8 => {
                table::run(instance, _input);
            }
            type_ if type_ == FfiFormattingContextType::Flex as u8 => {
                flex::run(instance, _input);
            }
            _ => panic!("no Rust implementation for this formatting context"),
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_automatic_content_inline_size(fc: *mut c_void) -> i32 {
    abort_on_panic(|| {
        bump(FfiOp::FcAutomaticInlineSize);
        instance_mut(fc).automatic_content_inline_size.raw_value()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_automatic_content_block_size(fc: *mut c_void) -> i32 {
    abort_on_panic(|| {
        bump(FfiOp::FcAutomaticBlockSize);
        instance_mut(fc).automatic_content_block_size.raw_value()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_set_table_box_content_offset_in_wrapper(fc: *mut c_void, offset: LogicalOffset) {
    abort_on_panic(|| {
        instance_mut(fc).pending_table_box_content_offset_in_wrapper = offset;
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_table_box_content_offset_in_wrapper(fc: *mut c_void) -> LogicalOffset {
    abort_on_panic(|| instance_mut(fc).pending_table_box_content_offset_in_wrapper)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_fc_run_until_table_inline_size_calculation(
    fc: *mut c_void,
    _input: FfiLayoutInput,
    _skip_row_measurement: bool,
) {
    abort_on_panic(|| {
        bump(FfiOp::FcRunUntilTableInlineSize);
        let instance = instance_mut(fc);
        assert_eq!(instance.fc_type, FfiFormattingContextType::Table as u8);
        table::run_until_inline_size_calculation(instance, _input, _skip_row_measurement);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_enums::{display_inside, display_internal, display_outside};

    fn base() -> FfiLayoutBoxFacts {
        FfiLayoutBoxFacts {
            is_box: true,
            is_block_container: true,
            can_have_children: true,
            display: FfiDisplay::block(),
            ..Default::default()
        }
    }

    #[test]
    fn table_flex_grid_and_math_branches() {
        for (inside, expected) in [
            (display_inside::TABLE, FfiFormattingContextType::Table),
            (display_inside::FLEX, FfiFormattingContextType::Flex),
            (display_inside::GRID, FfiFormattingContextType::Grid),
            (display_inside::MATH, FfiFormattingContextType::Block),
        ] {
            let facts = FfiLayoutBoxFacts {
                display: FfiDisplay::outside_and_inside(display_outside::BLOCK, inside, false),
                ..base()
            };
            assert_eq!(formatting_context_type_created_by_box(facts), Some(expected));
        }
    }

    #[test]
    fn svg_and_replaced_branches_precede_display() {
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_svg_svg_box: true,
                ..base()
            }),
            Some(FfiFormattingContextType::Svg)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_replaced_box_with_children: true,
                ..base()
            }),
            Some(FfiFormattingContextType::ReplacedWithChildren)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_replaced_box: true,
                ..base()
            }),
            Some(FfiFormattingContextType::InternalReplaced)
        );
    }

    #[test]
    fn block_and_none_branches() {
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                creates_block_formatting_context: true,
                ..base()
            }),
            Some(FfiFormattingContextType::Block)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                children_are_inline: true,
                ..base()
            }),
            None
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                can_have_children: false,
                ..base()
            }),
            None
        );
    }

    #[test]
    fn table_parts_and_unknown_non_flow_inside_match_cpp() {
        let row = FfiLayoutBoxFacts {
            display: FfiDisplay::internal(display_internal::TABLE_ROW),
            ..base()
        };
        assert_eq!(formatting_context_type_created_by_box(row), None);

        let ruby = FfiLayoutBoxFacts {
            display: FfiDisplay::outside_and_inside(display_outside::BLOCK, display_inside::RUBY, false),
            ..base()
        };
        assert_eq!(
            formatting_context_type_created_by_box(ruby),
            Some(FfiFormattingContextType::InternalDummy)
        );
    }

    #[test]
    fn rust_owns_flex_and_table_formatting_contexts() {
        for type_ in 0..=u8::MAX {
            assert_eq!(
                rust_layout_owns_fc_type(type_),
                type_ == FfiFormattingContextType::Flex as u8 || type_ == FfiFormattingContextType::Table as u8
            );
        }
    }
}
