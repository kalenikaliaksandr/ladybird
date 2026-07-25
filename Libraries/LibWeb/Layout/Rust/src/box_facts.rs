/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::display::FfiDisplay;
use crate::node_data::{FfiTableDisplay, NodeData, NodeDisplayFlag, NodeFlag, NodeKind};
use std::ffi::c_void;
use std::ops::Deref;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct FfiAdditionalBoxFacts {
    pub is_inline_flow_interrupting_block: bool,
    pub display_before_box_type_transformation_is_block_outside: bool,
    pub inline_axis_is_reverse: bool,
    pub has_dom_node: bool,
    pub can_have_children: bool,
    pub creates_block_formatting_context: bool,
    pub is_editing_host: bool,
    pub uses_button_layout: bool,
    pub is_html_input_element: bool,

    pub rendered_legend: *mut c_void,
    pub list_item_marker: *mut c_void,
    pub has_css_marker_content: bool,
    pub marker_content_inline_size: CssPixels,
    pub marker_content_block_size: CssPixels,
    pub marker_distance: CssPixels,
    pub marker_list_style_position: u8,

    pub has_definite_natural_width: bool,
    pub has_definite_natural_height: bool,

    pub has_auto_content_width: bool,
    pub auto_content_width: CssPixels,
    pub has_auto_content_height: bool,
    pub auto_content_height: CssPixels,
    pub has_auto_content_aspect_ratio: bool,
    pub auto_content_aspect_ratio_numerator: CssPixels,
    pub auto_content_aspect_ratio_denominator: CssPixels,
    pub has_auto_content_box_size: bool,
    pub has_preferred_aspect_ratio: bool,
    pub preferred_aspect_ratio_numerator: CssPixels,
    pub preferred_aspect_ratio_denominator: CssPixels,
    pub has_default_preferred_width: bool,
    pub default_preferred_width: CssPixels,
    pub has_default_preferred_height: bool,
    pub default_preferred_height: CssPixels,
    pub initial_containing_block_inline_size: CssPixels,
    pub is_scroll_container: bool,

    pub document_in_quirks_mode: bool,
    pub is_in_user_agent_shadow_tree: bool,
    pub is_html_html_element: bool,
}

pub struct FfiLayoutBoxFacts {
    pub is_text_node: bool,
    pub is_break_node: bool,
    pub is_box: bool,
    pub is_block_container: bool,
    pub is_replaced_box: bool,
    pub is_replaced_box_with_children: bool,
    pub is_floating: bool,
    pub is_absolutely_positioned: bool,
    pub is_inline: bool,
    pub is_atomic_inline: bool,
    pub has_box_model_metrics: bool,
    pub is_fragmented_inline: bool,
    pub is_inline_flow_interrupting_block: bool,
    pub is_list_item_marker_box: bool,
    pub is_list_item_box: bool,
    pub is_svg_mask_box: bool,
    pub is_svg_clip_box: bool,
    pub display_before_box_type_transformation_is_block_outside: bool,
    pub inline_axis_is_reverse: bool,
    pub has_dom_node: bool,
    pub children_are_inline: bool,
    pub is_anonymous: bool,
    pub can_have_children: bool,
    pub has_replaced_element_table_display_adjustment: bool,
    pub creates_block_formatting_context: bool,
    pub is_flex_item: bool,
    pub is_grid_item: bool,
    pub is_editing_host: bool,
    pub uses_button_layout: bool,
    pub vertical_align_applies: bool,
    pub is_html_input_element: bool,

    pub is_fieldset_box: bool,
    pub rendered_legend: *mut c_void,
    pub list_item_marker: *mut c_void,
    pub has_css_marker_content: bool,
    pub marker_content_inline_size: CssPixels,
    pub marker_content_block_size: CssPixels,
    pub marker_distance: CssPixels,
    pub marker_list_style_position: u8,

    pub has_definite_natural_width: bool,
    pub has_definite_natural_height: bool,

    pub has_auto_content_width: bool,
    pub auto_content_width: CssPixels,
    pub has_auto_content_height: bool,
    pub auto_content_height: CssPixels,
    pub has_auto_content_aspect_ratio: bool,
    pub auto_content_aspect_ratio_numerator: CssPixels,
    pub auto_content_aspect_ratio_denominator: CssPixels,
    pub has_auto_content_box_size: bool,
    pub has_preferred_aspect_ratio: bool,
    pub preferred_aspect_ratio_numerator: CssPixels,
    pub preferred_aspect_ratio_denominator: CssPixels,
    pub has_default_preferred_width: bool,
    pub default_preferred_width: CssPixels,
    pub has_default_preferred_height: bool,
    pub default_preferred_height: CssPixels,
    pub initial_containing_block_inline_size: CssPixels,
    pub is_scroll_container: bool,

    pub has_layout_index: bool,
    pub layout_index: u32,
    pub display: FfiDisplay,
    pub is_svg_box: bool,
    pub is_svg_svg_box: bool,
    pub is_table_box: bool,
    pub is_table_wrapper: bool,
    pub is_table_row_group: bool,
    pub is_table_header_group: bool,
    pub is_table_footer_group: bool,
    pub is_table_row: bool,
    pub is_table_cell: bool,
    pub is_table_column_group: bool,
    pub is_table_column: bool,
    pub is_table_caption: bool,
    pub is_viewport: bool,

    pub document_in_quirks_mode: bool,
    pub is_in_user_agent_shadow_tree: bool,
    pub is_html_html_element: bool,
    pub is_html_body_element: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct BoxItemFacts {
    pub is_flex_item: bool,
    pub is_grid_item: bool,
    pub vertical_align_applies: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct BoxFactsView<'a> {
    facts: &'a FfiLayoutBoxFacts,
    pub is_grid_item: bool,
    pub vertical_align_applies: bool,
}

impl<'a> BoxFactsView<'a> {
    #[inline]
    pub(crate) fn new(facts: &'a FfiLayoutBoxFacts, item_facts: BoxItemFacts) -> Self {
        Self {
            facts,
            is_grid_item: item_facts.is_grid_item,
            vertical_align_applies: item_facts.vertical_align_applies,
        }
    }
}

impl Deref for BoxFactsView<'_> {
    type Target = FfiLayoutBoxFacts;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.facts
    }
}

pub(crate) fn has_flag(data: &NodeData, flag: NodeFlag) -> bool {
    data.flags & flag as u32 != 0
}

pub(crate) fn has_display_flag(data: &NodeData, flag: NodeDisplayFlag) -> bool {
    data.display_bits & flag as u8 != 0
}

pub(crate) fn kind_is_text(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::GeneratedTextNode | NodeKind::TextNode | NodeKind::TextSliceNode
    )
}

pub(crate) fn kind_is_box(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::Unset
            | NodeKind::BreakNode
            | NodeKind::InlineNode
            | NodeKind::Node
            | NodeKind::NodeWithStyle
            | NodeKind::NodeWithStyleAndBoxModelMetrics
            | NodeKind::GeneratedTextNode
            | NodeKind::TextNode
            | NodeKind::TextSliceNode
    )
}

pub(crate) fn kind_is_block_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::BlockContainer
            | NodeKind::FieldSetBox
            | NodeKind::LegendBox
            | NodeKind::ListItemBox
            | NodeKind::RangeInputBox
            | NodeKind::SVGForeignObjectBox
            | NodeKind::TableWrapper
            | NodeKind::TextAreaBox
            | NodeKind::TextInputBox
            | NodeKind::Viewport
    )
}

pub(crate) fn kind_is_replaced_box(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::AudioBox
            | NodeKind::CanvasBox
            | NodeKind::CheckBox
            | NodeKind::ImageBox
            | NodeKind::NavigableContainerViewport
            | NodeKind::RadioButton
            | NodeKind::ReplacedBox
            | NodeKind::SVGSVGBox
            | NodeKind::VideoBox
    )
}

fn kind_is_svg_box(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::SVGBox
            | NodeKind::SVGClipBox
            | NodeKind::SVGGeometryBox
            | NodeKind::SVGGraphicsBox
            | NodeKind::SVGImageBox
            | NodeKind::SVGMaskBox
            | NodeKind::SVGPatternBox
            | NodeKind::SVGTextBox
            | NodeKind::SVGTextPathBox
    )
}

impl FfiLayoutBoxFacts {
    pub(crate) fn from_arena(data: &NodeData, additional: FfiAdditionalBoxFacts, display: FfiDisplay) -> Self {
        let is_text_node = kind_is_text(data.kind);
        let is_box = kind_is_box(data.kind);
        let is_block_container = kind_is_block_container(data.kind);
        let is_replaced_box = kind_is_replaced_box(data.kind);
        let is_inline = is_text_node || has_display_flag(data, NodeDisplayFlag::InlineOutside);
        let is_replaced_element = has_flag(data, NodeFlag::IsReplacedElement);
        let is_list_item_marker_box = data.kind == NodeKind::ListItemMarkerBox;
        Self {
            is_text_node,
            is_break_node: data.kind == NodeKind::BreakNode,
            is_box,
            is_block_container,
            is_replaced_box,
            is_replaced_box_with_children: is_replaced_box && additional.can_have_children,
            is_floating: has_display_flag(data, NodeDisplayFlag::Floating),
            is_absolutely_positioned: has_display_flag(data, NodeDisplayFlag::AbsolutelyPositioned),
            is_inline,
            is_atomic_inline: is_replaced_element
                || is_list_item_marker_box
                || (display.is_inline_outside() && !display.is_flow_inside()),
            has_box_model_metrics: !matches!(
                data.kind,
                NodeKind::Unset
                    | NodeKind::Node
                    | NodeKind::NodeWithStyle
                    | NodeKind::GeneratedTextNode
                    | NodeKind::TextNode
                    | NodeKind::TextSliceNode
            ),
            is_fragmented_inline: data.kind == NodeKind::InlineNode
                || (data.kind == NodeKind::ListItemBox && display.is_inline_outside() && display.is_flow_inside()),
            is_inline_flow_interrupting_block: additional.is_inline_flow_interrupting_block,
            is_list_item_marker_box,
            is_list_item_box: data.kind == NodeKind::ListItemBox,
            is_svg_mask_box: data.kind == NodeKind::SVGMaskBox,
            is_svg_clip_box: data.kind == NodeKind::SVGClipBox,
            display_before_box_type_transformation_is_block_outside: additional
                .display_before_box_type_transformation_is_block_outside,
            inline_axis_is_reverse: additional.inline_axis_is_reverse,
            has_dom_node: additional.has_dom_node,
            children_are_inline: has_flag(data, NodeFlag::ChildrenAreInline),
            is_anonymous: has_flag(data, NodeFlag::Anonymous),
            can_have_children: additional.can_have_children,
            has_replaced_element_table_display_adjustment: is_replaced_element
                && data.table_display_before != FfiTableDisplay::Other,
            creates_block_formatting_context: additional.creates_block_formatting_context,
            is_flex_item: has_flag(data, NodeFlag::IsFlexItem),
            is_grid_item: has_flag(data, NodeFlag::IsGridItem),
            is_editing_host: additional.is_editing_host,
            uses_button_layout: additional.uses_button_layout,
            vertical_align_applies: is_box
                && !has_flag(data, NodeFlag::IsFlexItem)
                && !has_flag(data, NodeFlag::IsGridItem),
            is_html_input_element: additional.is_html_input_element,
            is_fieldset_box: data.kind == NodeKind::FieldSetBox,
            rendered_legend: additional.rendered_legend,
            list_item_marker: additional.list_item_marker,
            has_css_marker_content: additional.has_css_marker_content,
            marker_content_inline_size: additional.marker_content_inline_size,
            marker_content_block_size: additional.marker_content_block_size,
            marker_distance: additional.marker_distance,
            marker_list_style_position: additional.marker_list_style_position,
            has_definite_natural_width: additional.has_definite_natural_width,
            has_definite_natural_height: additional.has_definite_natural_height,
            has_auto_content_width: additional.has_auto_content_width,
            auto_content_width: additional.auto_content_width,
            has_auto_content_height: additional.has_auto_content_height,
            auto_content_height: additional.auto_content_height,
            has_auto_content_aspect_ratio: additional.has_auto_content_aspect_ratio,
            auto_content_aspect_ratio_numerator: additional.auto_content_aspect_ratio_numerator,
            auto_content_aspect_ratio_denominator: additional.auto_content_aspect_ratio_denominator,
            has_auto_content_box_size: additional.has_auto_content_box_size,
            has_preferred_aspect_ratio: additional.has_preferred_aspect_ratio,
            preferred_aspect_ratio_numerator: additional.preferred_aspect_ratio_numerator,
            preferred_aspect_ratio_denominator: additional.preferred_aspect_ratio_denominator,
            has_default_preferred_width: additional.has_default_preferred_width,
            default_preferred_width: additional.default_preferred_width,
            has_default_preferred_height: additional.has_default_preferred_height,
            default_preferred_height: additional.default_preferred_height,
            initial_containing_block_inline_size: additional.initial_containing_block_inline_size,
            is_scroll_container: additional.is_scroll_container,
            has_layout_index: has_flag(data, NodeFlag::HasStyle),
            layout_index: data.layout_index,
            display,
            is_svg_box: kind_is_svg_box(data.kind),
            is_svg_svg_box: data.kind == NodeKind::SVGSVGBox,
            is_table_box: data.table_display == FfiTableDisplay::TableRoot,
            is_table_wrapper: data.kind == NodeKind::TableWrapper,
            is_table_row_group: data.table_display == FfiTableDisplay::TableRowGroup,
            is_table_header_group: data.table_display == FfiTableDisplay::TableHeaderGroup,
            is_table_footer_group: data.table_display == FfiTableDisplay::TableFooterGroup,
            is_table_row: data.table_display == FfiTableDisplay::TableRow,
            is_table_cell: data.table_display == FfiTableDisplay::TableCell,
            is_table_column_group: data.table_display == FfiTableDisplay::TableColumnGroup,
            is_table_column: data.table_display == FfiTableDisplay::TableColumn,
            is_table_caption: data.table_display == FfiTableDisplay::TableCaption,
            is_viewport: data.kind == NodeKind::Viewport,
            document_in_quirks_mode: additional.document_in_quirks_mode,
            is_in_user_agent_shadow_tree: additional.is_in_user_agent_shadow_tree,
            is_html_html_element: additional.is_html_html_element,
            is_html_body_element: has_flag(data, NodeFlag::IsBody),
        }
    }
}

impl Default for FfiLayoutBoxFacts {
    fn default() -> Self {
        Self {
            is_text_node: false,
            is_break_node: false,
            is_box: false,
            is_block_container: false,
            is_replaced_box: false,
            is_replaced_box_with_children: false,
            is_floating: false,
            is_absolutely_positioned: false,
            is_inline: false,
            is_atomic_inline: false,
            has_box_model_metrics: false,
            is_fragmented_inline: false,
            is_inline_flow_interrupting_block: false,
            is_list_item_marker_box: false,
            is_list_item_box: false,
            is_svg_mask_box: false,
            is_svg_clip_box: false,
            display_before_box_type_transformation_is_block_outside: false,
            inline_axis_is_reverse: false,
            has_dom_node: false,
            children_are_inline: false,
            is_anonymous: false,
            can_have_children: false,
            has_replaced_element_table_display_adjustment: false,
            creates_block_formatting_context: false,
            is_flex_item: false,
            is_grid_item: false,
            is_editing_host: false,
            uses_button_layout: false,
            vertical_align_applies: false,
            is_html_input_element: false,
            is_fieldset_box: false,
            rendered_legend: std::ptr::null_mut(),
            list_item_marker: std::ptr::null_mut(),
            has_css_marker_content: false,
            marker_content_inline_size: CssPixels::default(),
            marker_content_block_size: CssPixels::default(),
            marker_distance: CssPixels::default(),
            marker_list_style_position: 0,
            has_definite_natural_width: false,
            has_definite_natural_height: false,
            has_auto_content_width: false,
            auto_content_width: CssPixels::default(),
            has_auto_content_height: false,
            auto_content_height: CssPixels::default(),
            has_auto_content_aspect_ratio: false,
            auto_content_aspect_ratio_numerator: CssPixels::default(),
            auto_content_aspect_ratio_denominator: CssPixels::default(),
            has_auto_content_box_size: false,
            has_preferred_aspect_ratio: false,
            preferred_aspect_ratio_numerator: CssPixels::default(),
            preferred_aspect_ratio_denominator: CssPixels::default(),
            has_default_preferred_width: false,
            default_preferred_width: CssPixels::default(),
            has_default_preferred_height: false,
            default_preferred_height: CssPixels::default(),
            initial_containing_block_inline_size: CssPixels::default(),
            is_scroll_container: false,
            has_layout_index: false,
            layout_index: 0,
            display: FfiDisplay::block(),
            is_svg_box: false,
            is_svg_svg_box: false,
            is_table_box: false,
            is_table_wrapper: false,
            is_table_row_group: false,
            is_table_header_group: false,
            is_table_footer_group: false,
            is_table_row: false,
            is_table_cell: false,
            is_table_column_group: false,
            is_table_column: false,
            is_table_caption: false,
            is_viewport: false,
            document_in_quirks_mode: false,
            is_in_user_agent_shadow_tree: false,
            is_html_html_element: false,
            is_html_body_element: false,
        }
    }
}
