/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css::css_pixels::CssPixels;
use crate::layout::node_data::NodeSlotId;
use crate::layout::{FfiCssPixelPoint, FfiCssPixelRect, FfiCssPixelSize};
use crate::painting::paintable_arena::PaintableArena;
use crate::painting::paintable_data::*;
use std::fmt::Write;

pub fn format_f64(value: f64) -> String {
    format!("{value}")
}

pub fn format_f32(value: f32) -> String {
    format!("{value}")
}

pub fn format_css_pixels(value: CssPixels) -> String {
    format_f64(value.to_double())
}

pub fn format_css_point(point: FfiCssPixelPoint) -> String {
    format!("[{},{}]", format_css_pixels(point.x), format_css_pixels(point.y))
}

pub fn format_css_size(size: FfiCssPixelSize) -> String {
    format!("[{}x{}]", format_css_pixels(size.width), format_css_pixels(size.height))
}

pub fn format_css_rect(rect: FfiCssPixelRect) -> String {
    format!(
        "[{},{} {}x{}]",
        format_css_pixels(rect.x),
        format_css_pixels(rect.y),
        format_css_pixels(rect.width),
        format_css_pixels(rect.height)
    )
}

fn format_optional_css_pixels(value: CssPixels, present: bool) -> String {
    if present {
        format_css_pixels(value)
    } else {
        "none".to_string()
    }
}

pub trait LayoutNodeDescriber {
    fn describe(&self, node: NodeSlotId) -> String;
    fn is_live(&self, node: NodeSlotId) -> bool;
    fn describe_dom_node(&self, node: NodeSlotId) -> Option<String>;
}

fn paintable_description(arena: &PaintableArena, describer: &dyn LayoutNodeDescriber, slot: PaintableSlotId) -> String {
    let data = arena.data_ref(slot);
    if data.layout_node.is_invalid() || !describer.is_live(data.layout_node) {
        return format!("{}(detached)", data.kind.class_name());
    }
    format!("{}({})", data.kind.class_name(), describer.describe(data.layout_node))
}

use crate::painting::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
use crate::painting::visual_context::{VisualContextData, VisualContextState};
use libgfx_rust::FloatMatrix4x4;

fn format_matrix(matrix: &FloatMatrix4x4) -> String {
    let mut out = String::from("[");
    for (row_index, row) in matrix.elements.iter().enumerate() {
        for (column_index, value) in row.iter().enumerate() {
            if row_index != 0 || column_index != 0 {
                out.push(' ');
            }
            out.push_str(&format_f32(*value));
        }
    }
    out.push(']');
    out
}

fn format_int_rect(rect: libgfx_rust::IntRect) -> String {
    format!("[{},{} {}x{}]", rect.x, rect.y, rect.width, rect.height)
}

fn format_corner_radii(radii: libgfx_rust::CornerRadii) -> String {
    format!(
        "[{}x{} {}x{} {}x{} {}x{}]",
        radii.top_left.horizontal_radius,
        radii.top_left.vertical_radius,
        radii.top_right.horizontal_radius,
        radii.top_right.vertical_radius,
        radii.bottom_right.horizontal_radius,
        radii.bottom_right.vertical_radius,
        radii.bottom_left.horizontal_radius,
        radii.bottom_left.vertical_radius
    )
}

fn format_float_point(point: libgfx_rust::FloatPoint) -> String {
    format!("[{},{}]", format_f32(point.x), format_f32(point.y))
}

fn dump_visual_context_node(data: &VisualContextData, out: &mut String) {
    match data {
        VisualContextData::Perspective(perspective) => {
            write!(
                out,
                "perspective matrix={} flattens={}",
                format_matrix(&perspective.matrix),
                perspective.flattens_inherited_transform
            )
            .unwrap();
        }
        VisualContextData::BackfaceVisibility(backface) => {
            write!(
                out,
                "backface-hidden plane_root={} flattens={}",
                backface.plane_root_index, backface.flattens_inherited_transform
            )
            .unwrap();
        }
        VisualContextData::Scroll(scroll) => {
            write!(
                out,
                "scroll sticky={} slot={}",
                scroll.is_sticky,
                if scroll.state_slot == NO_SCROLL_STATE_SLOT {
                    "none".to_string()
                } else {
                    scroll.state_slot.to_string()
                }
            )
            .unwrap();
        }
        VisualContextData::Transform(transform) => {
            let name = match transform.role {
                crate::painting::visual_context::TransformDataRole::SvgViewportTransform => "svg-viewport-transform",
                crate::painting::visual_context::TransformDataRole::CssTransform => "transform",
            };
            write!(
                out,
                "{} matrix={} origin={} flattens={}",
                name,
                format_matrix(&transform.matrix),
                format_float_point(transform.origin),
                transform.flattens_inherited_transform
            )
            .unwrap();
        }
        VisualContextData::Clip(clip) => {
            write!(
                out,
                "clip rect={} radii={}",
                format_int_rect(clip.rect),
                format_corner_radii(clip.corner_radii)
            )
            .unwrap();
        }
        VisualContextData::ClipPath(clip_path) => {
            write!(
                out,
                "clip-path bounds={} fill_rule={} path={}",
                format_int_rect(clip_path.bounding_rect),
                clip_path.fill_rule as i32,
                clip_path.path.to_svg_string()
            )
            .unwrap();
        }
        VisualContextData::Effects(effects) => {
            write!(
                out,
                "effects opacity={} blend_mode={}",
                format_f32(effects.opacity),
                effects.blend_mode as i32
            )
            .unwrap();
            match &effects.filter {
                Some(filter) => {
                    let (serialized_size, serialized_hash) = filter.serialized_summary();
                    write!(out, " filter=(size={serialized_size} hash={serialized_hash:#x})").unwrap();
                }
                None => out.push_str(" filter=none"),
            }
        }
        VisualContextData::Mask(mask) => {
            write!(
                out,
                "mask rect={} kind={} origin={}",
                format_int_rect(mask.rect),
                mask.kind as i32,
                mask.origin as u8
            )
            .unwrap();
        }
        VisualContextData::ScrollCompensation(compensation) => {
            write!(
                out,
                "scroll-compensation scroll_node={}",
                compensation.scroll_node_index
            )
            .unwrap();
        }
        VisualContextData::AnchorScrollShift(shift) => {
            write!(
                out,
                "anchor-scroll-shift scroll_node={} negate={} compensate_x={} compensate_y={}",
                shift.scroll_node_index,
                shift.negate,
                shift.compensate_horizontal_scroll,
                shift.compensate_vertical_scroll
            )
            .unwrap();
        }
    }
}

pub fn dump_visual_context_tree(
    state: &VisualContextState,
    arena: &PaintableArena,
    describer: &dyn LayoutNodeDescriber,
    viewport: PaintableSlotId,
) -> String {
    let mut out = String::new();
    let Some(tree) = &state.tree else {
        return "(no visual context tree)\n".to_string();
    };
    writeln!(
        out,
        "nodes={} root_is_visual_viewport={} reused_previous_version={}",
        tree.nodes.len(),
        tree.root_is_visual_viewport,
        tree.reused_previous_version
    )
    .unwrap();
    for (index, node) in tree.nodes.iter().enumerate() {
        write!(
            out,
            "[{}] parent={} depth={} empty_clip={} ",
            index, node.parent_index, node.depth, node.has_empty_effective_clip
        )
        .unwrap();
        dump_visual_context_node(&node.data, &mut out);
        out.push('\n');
    }

    let scroll_state = &state.scroll_state;
    writeln!(out, "scroll_state slots={}", scroll_state.slot_count()).unwrap();
    for slot in 0..scroll_state.slot_count() {
        let node_state = scroll_state.state_at_slot(slot);
        write!(
            out,
            "  slot {} {} sticky={} node={} parent_slot={} own_offset={}",
            slot,
            if arena.is_live(node_state.paintable) {
                paintable_description(arena, describer, node_state.paintable)
            } else {
                "(dead)".to_string()
            },
            node_state.is_sticky,
            node_state.node_index,
            if node_state.parent_slot == NO_SCROLL_STATE_SLOT {
                "none".to_string()
            } else {
                node_state.parent_slot.to_string()
            },
            format_css_point(node_state.own_offset.into())
        )
        .unwrap();
        if let Some(constraints) = &node_state.sticky_constraints {
            let insets = constraints.insets;
            write!(
                out,
                " sticky_constraints=[position={} border_box={} scrollport={} containing_block={} parent_adjust={} insets=[{} {} {} {}]]",
                format_css_point(constraints.position_relative_to_scroll_ancestor.into()),
                format_css_size(constraints.border_box_size.into()),
                format_css_size(constraints.scrollport_size.into()),
                format_css_rect(constraints.containing_block_region.into()),
                constraints.needs_parent_offset_adjustment,
                format_optional_css_pixels(insets.top, insets.has_top),
                format_optional_css_pixels(insets.right, insets.has_right),
                format_optional_css_pixels(insets.bottom, insets.has_bottom),
                format_optional_css_pixels(insets.left, insets.has_left)
            )
            .unwrap();
        }
        out.push('\n');
    }
    out.push_str("snapshot=[");
    for offset in &state.scroll_state_snapshot {
        write!(out, " {}", format_float_point(*offset)).unwrap();
    }
    out.push_str(" ]\n");

    out.push_str("paintables:\n");
    dump_visual_context_paintable(arena, describer, viewport, &mut out);
    out
}

fn dump_visual_context_paintable(
    arena: &PaintableArena,
    describer: &dyn LayoutNodeDescriber,
    slot: PaintableSlotId,
    out: &mut String,
) {
    let data = arena.data_ref(slot);
    writeln!(
        out,
        "  {} own={} descendants={} enclosing_scroll={} own_scroll={} fixed_background={} range=[{},{}) non_invertible={}",
        paintable_description(arena, describer, slot),
        data.accumulated_visual_context_index,
        data.accumulated_visual_context_for_descendants_index,
        data.enclosing_scroll_node_index,
        data.own_scroll_node_index,
        if data.has_fixed_background_visual_context { data.fixed_background_visual_context.to_string() } else { "none".to_string() },
        data.visual_context_nodes_begin,
        data.visual_context_nodes_end,
        data.has_flag(PaintableFlag::HasNonInvertibleCssTransform)
    )
    .unwrap();
    let mut child = arena.first_child(slot);
    while let Some(current) = child {
        dump_visual_context_paintable(arena, describer, current, out);
        child = arena.next_sibling(current);
    }
}
