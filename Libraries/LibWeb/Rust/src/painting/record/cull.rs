/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Where each paint producer can draw. A recording around the scroll positions may leave out a producer that cannot
//! draw inside the interest rect of the scroll node it draws in, so every producer is placed in the content of the
//! scroll node its commands record under, within an estimate of what they cover there or anywhere in it, unless hit
//! testing or the compositor needs its output for the whole document.
//!
//! With debug assertions, or with LIBWEB_VERIFY_PAINT_PRODUCER_BOUNDS set, every producer that records is checked
//! against its placement, and a command recorded where the placement does not reach panics.

use super::order_tree::ProducerKind;
use super::producers::draw_phase;
use super::trace::Observer;
use super::{PaintPhase, PaintRecorder};
use crate::css::computed_value_types::ComputedShadow;
use crate::css::css_pixels::{CssPixelRect, CssPixels};
use crate::layout::node_data::{NodeKind, NodeSlotId};
use crate::painting::chrome_geometry::ChromeGeometry;
use crate::painting::display_list::builder::{for_each_command, read_command};
use crate::painting::display_list::commands::{
    CompositorBlockingWheelEventRegion, CompositorMainThreadWheelEventRegion, CompositorNestedContextWheelRegion,
    CompositorScrollChainingBarrier, CompositorWheelHitTestTarget, CompositorWheelHitTestTargetWithCornerRadii,
    ContextRef, DisplayListCommandHeader, DisplayListCommandRun, DisplayListCommandType, SpatialNodeIndex,
};
use crate::painting::display_list::device_pixels::DevicePixelConverter;
use crate::painting::ffi::ScrollDirection;
use crate::painting::paint_read::{GeometryRead, PaintRead};
use crate::painting::record::inputs::CaretTarget;
use crate::painting::record::paint::{
    border_image, column_rule, table_backgrounds, table_borders, text, text_decoration,
};
use crate::painting::visual_context::interest::{CullSpace, cull_space_of, effects_under_spreading_filters};
use crate::painting::visual_context::{SpatialData, VisualContextTree};
use crate::painting::{border_shape, node_painting, paintable_geometry, style_queries, text_fragment};
use libgfx_rust::{IntRect, enclosing_int_rect};
use std::ops::Range;
use std::sync::OnceLock;

/// What a recording around the scroll positions may leave out of a producer's output, and where that output lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProducerPlacement {
    /// Hit-test items, with which the main thread hit tests the whole document.
    HitTest,
    /// Output needed whatever is visible: what the compositor scrolls and snaps by, its scrollbars, and declarations
    /// that later output refers to.
    DocumentWide,
    /// Output outside every scroll node the compositor scrolls, such as fixed-position content.
    NeverCulled,
    /// Output anywhere in the content of a scroll node, or of the scroll nodes inside it: transformed or filtered
    /// content, and content whose extent the estimates cannot tell.
    AnywhereInScrollNode(SpatialNodeIndex),
    /// Output within `bounds` of a scroll node's content, in device pixels at scroll offset zero.
    WithinScrollNode {
        scroll_node: SpatialNodeIndex,
        bounds: IntRect,
    },
}

// The estimates snap to device pixels a little differently from the paint code they follow.
const ESTIMATE_ROUNDING_SLACK: i32 = 2;

fn place(
    cull_space: CullSpace,
    draws_beyond_bounds_estimates: bool,
    estimate_bounds: impl FnOnce() -> IntRect,
) -> ProducerPlacement {
    match cull_space {
        CullSpace::NeverCulled => ProducerPlacement::NeverCulled,
        CullSpace::ScrollNode {
            scroll_node,
            placed_exactly,
        } if !placed_exactly || draws_beyond_bounds_estimates => ProducerPlacement::AnywhereInScrollNode(scroll_node),
        CullSpace::ScrollNode { scroll_node, .. } => ProducerPlacement::WithinScrollNode {
            scroll_node,
            bounds: estimate_bounds(),
        },
    }
}

/// What placing producers reads of the visual context tree, worked out once per recording.
pub(crate) struct CullSpaces {
    under_spreading_filter: Vec<bool>,
    // One flag per spatial node: whether it is a scroll node the recording records a compositor scroll node for.
    scrolled_by_compositor: Vec<bool>,
}

/// The scroll nodes that content is culled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CullingScrollNodes {
    ScrolledByCompositor,
    /// Every scroll node, which places content that the compositor scrolls nothing of, as most test pages are, in the
    /// nearest one so that the estimates of its producers can be checked as well.
    Every,
}

impl CullSpaces {
    fn cull_space_of(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        culling_scroll_nodes: CullingScrollNodes,
    ) -> CullSpace {
        cull_space_of(
            tree,
            context,
            |index| {
                culling_scroll_nodes == CullingScrollNodes::Every
                    || self
                        .scrolled_by_compositor
                        .get(index.0 as usize)
                        .copied()
                        .unwrap_or(false)
            },
            &self.under_spreading_filter,
        )
    }
}

/// Whether every producer that records is checked against its placement: always under debug assertions, and with
/// LIBWEB_VERIFY_PAINT_PRODUCER_BOUNDS set.
pub(crate) fn verifies_paint_producer_bounds() -> bool {
    static REQUESTED: OnceLock<bool> = OnceLock::new();
    cfg!(debug_assertions)
        || *REQUESTED.get_or_init(|| std::env::var_os("LIBWEB_VERIFY_PAINT_PRODUCER_BOUNDS").is_some())
}

impl<O: Observer> PaintRecorder<'_, O> {
    pub(crate) fn cull_spaces_of_recording(&mut self) -> Option<CullSpaces> {
        let paint_state = self.paint_state;
        let tree = paint_state.visual_context_tree.as_deref()?;
        let scrolled_by_compositor = tree
            .spatial_nodes
            .iter()
            .map(|node| match &node.data {
                SpatialData::Scroll(scroll) => {
                    let owner = scroll.owner_paintable;
                    !owner.is_invalid()
                        && self.source.paintable_row_is_populated(owner)
                        && self.kind_of_compositor_scroll_node_recorded_for(owner).is_some()
                }
                _ => false,
            })
            .collect();
        Some(CullSpaces {
            under_spreading_filter: effects_under_spreading_filters(tree),
            scrolled_by_compositor,
        })
    }

    fn producer_placement(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        culling_scroll_nodes: CullingScrollNodes,
    ) -> ProducerPlacement {
        match kind {
            ProducerKind::HitBackground | ProducerKind::HitForeground | ProducerKind::HitOverlay => {
                return ProducerPlacement::HitTest;
            }
            ProducerKind::ScopePreamble => return ProducerPlacement::DocumentWide,
            ProducerKind::ScrollMetadata | ProducerKind::DrawOverlay
                if self.records_scroll_node_or_viewport_chrome(owner) =>
            {
                return ProducerPlacement::DocumentWide;
            }
            // The root background covers the whole canvas.
            ProducerKind::DrawBackground if style_queries::node_is_root_element(self.source, owner) => {
                return ProducerPlacement::DocumentWide;
            }
            _ => {}
        }
        let paint_state = self.paint_state;
        let Some(tree) = paint_state.visual_context_tree.as_deref() else {
            return ProducerPlacement::NeverCulled;
        };
        let context = self.producer_context(owner, kind);
        let cull_space = self.cull_space(tree, context, culling_scroll_nodes);
        let draws_beyond_bounds_estimates =
            self.draws_beyond_bounds_estimates(tree, owner, kind, cull_space, culling_scroll_nodes);
        place(cull_space, draws_beyond_bounds_estimates, || {
            self.producer_bounds(owner, kind)
        })
    }

    fn cull_space(
        &self,
        tree: &VisualContextTree,
        context: ContextRef,
        culling_scroll_nodes: CullingScrollNodes,
    ) -> CullSpace {
        self.cull_spaces
            .as_ref()
            .expect("a recording that places its producers knows their cull spaces")
            .cull_space_of(tree, context, culling_scroll_nodes)
    }

    // The scroll metadata of a box the compositor scrolls holds its scroll node and snap geometry, and its overlay
    // holds the scrollbars the compositor drags. The viewport's hold the viewport scrollbars and middle-button
    // scrolling indicator.
    fn records_scroll_node_or_viewport_chrome(&mut self, owner: NodeSlotId) -> bool {
        self.source.node_kind_if_live(owner) == Some(NodeKind::Viewport)
            || self.kind_of_compositor_scroll_node_recorded_for(owner).is_some()
    }

    /// The context a producer records under, which decides the scroll node it is culled in.
    fn producer_context(&self, owner: NodeSlotId, kind: ProducerKind) -> ContextRef {
        match kind {
            ProducerKind::InlinePiece(index) => self.own_context(self.inline_piece_node(owner, index)),
            ProducerKind::TextFragment(_) => self.context_for_phase(owner, PaintPhase::Foreground),
            _ => match draw_phase(kind) {
                Some(phase) => self.context_for_phase(owner, phase),
                None => self.own_context(owner),
            },
        }
    }

    fn inline_piece_node(&self, owner: NodeSlotId, index: u32) -> NodeSlotId {
        let root = self.line_root_of_inline_producer(owner);
        self.source.committed_side_data(root).inline_box_pieces()[index as usize].node
    }

    /// Whether the producer can draw where its bounds estimate does not reach, in its own scroll node or in another
    /// one inside it, which places it anywhere in its scroll node.
    fn draws_beyond_bounds_estimates(
        &mut self,
        tree: &VisualContextTree,
        owner: NodeSlotId,
        kind: ProducerKind,
        cull_space: CullSpace,
        culling_scroll_nodes: CullingScrollNodes,
    ) -> bool {
        match kind {
            // SVG content records under the transforms of its viewport and elements.
            ProducerKind::Svg => true,
            ProducerKind::DrawBackground => self.box_paint_reaches_beyond_estimates(owner),
            ProducerKind::InlinePiece(index) => {
                let node = self.inline_piece_node(owner, index);
                self.box_paint_reaches_beyond_estimates(node)
            }
            // Column rules record in the box's content, which a box the compositor scrolls offsets.
            ProducerKind::DrawBorder => {
                self.box_paint_reaches_beyond_estimates(owner)
                    || (self.base_paint_facts(owner).paints_column_rules
                        && self.cull_space(tree, self.for_descendants_context(owner), culling_scroll_nodes)
                            != cull_space)
            }
            _ => false,
        }
    }

    // A `background-attachment: local` image is placed by its scroller's offset when it records, and a border shape,
    // which the border, the background and the shadows follow, can reach anywhere.
    fn box_paint_reaches_beyond_estimates(&mut self, node: NodeSlotId) -> bool {
        self.base_paint_facts(node).has_scroll_offset_dependent_background
            || self
                .source
                .node_style_if_live(node)
                .is_some_and(border_shape::has_border_shape)
    }

    /// A conservative estimate of what the commands of a producer placed exactly in its scroll node cover there.
    fn producer_bounds(&mut self, owner: NodeSlotId, kind: ProducerKind) -> IntRect {
        let bounds = match kind {
            ProducerKind::DrawBackground => self.background_bounds(owner),
            ProducerKind::DrawBorder => self.border_bounds(owner),
            ProducerKind::DrawTableCollapsedBorder => self.collapsed_table_borders_bounds(owner),
            ProducerKind::DrawForeground => self.foreground_bounds(owner),
            ProducerKind::DrawOutline => self.outline_bounds(owner),
            ProducerKind::DrawOverlay => self.overlay_bounds(owner),
            ProducerKind::InlinePiece(index) => self.inline_piece_bounds(owner, index),
            ProducerKind::TextFragment(index) => {
                let root = self.line_root_of_inline_producer(owner);
                self.text_fragments_bounds(root, &[index])
            }
            // The wheel targets and regions of a box lie within its border box.
            _ => self.border_box_bounds(owner),
        };
        if bounds.is_empty() {
            return bounds;
        }
        bounds.inflated_edges(
            ESTIMATE_ROUNDING_SLACK,
            ESTIMATE_ROUNDING_SLACK,
            ESTIMATE_ROUNDING_SLACK,
            ESTIMATE_ROUNDING_SLACK,
        )
    }

    fn border_box_bounds(&self, node: NodeSlotId) -> IntRect {
        self.converter
            .enclosing_device_rect(paintable_geometry::absolute_border_box_rect(self.source, node))
    }

    fn background_bounds(&mut self, owner: NodeSlotId) -> IntRect {
        let border_box = paintable_geometry::absolute_border_box_rect(self.source, owner);
        let mut bounds = self.converter.enclosing_device_rect(border_box);
        // A table part paints its background in the cells that originate in it, which can span outside it.
        if table_backgrounds::paints_background_in_cells(self.display(owner)) {
            for cell in table_backgrounds::originating_cells(self.source, owner) {
                bounds = bounds.united(self.border_box_bounds(cell));
            }
        }
        if !self.base_paint_facts(owner).has_box_shadow {
            return bounds;
        }
        // A box split into fragments casts its shadows around each of them, also around one without area.
        for fragment_border_box in paintable_geometry::absolute_border_box_rects_of_fragments(self.source, owner) {
            bounds = bounds.united(self.outer_box_shadows_bounds(owner, fragment_border_box));
        }
        bounds
    }

    fn outer_box_shadows_bounds(&self, node: NodeSlotId, border_box: CssPixelRect) -> IntRect {
        let Some(style) = self.source.node_style_if_live(node) else {
            return IntRect::default();
        };
        outer_box_shadows_bounds(
            &self.converter,
            self.converter.rounded_device_rect(border_box),
            style.effects().box_shadows.as_slice(),
        )
    }

    // A box split into fragments draws its borders around each of them.
    fn border_bounds(&mut self, owner: NodeSlotId) -> IntRect {
        let facts = self.base_paint_facts(owner);
        let Some(style) = self.source.node_style_if_live(owner) else {
            return self.border_box_bounds(owner);
        };
        let mut bounds = paintable_geometry::absolute_border_box_rects_of_fragments(self.source, owner)
            .into_iter()
            .fold(IntRect::default(), |bounds, border_box| {
                bounds.united(self.box_border_bounds(style, border_box, facts.paints_border_image))
            });
        if facts.paints_column_rules {
            let rule_reach = self.stroked_line_reach(style.column_rule_width());
            for rule in column_rule::column_rule_rects(self.source, owner, style) {
                bounds = bounds.united(
                    self.converter
                        .enclosing_device_rect(rule)
                        .inflated_edges(rule_reach, rule_reach, rule_reach, rule_reach),
                );
            }
        }
        bounds
    }

    fn box_border_bounds(
        &self,
        style: crate::css::computed_value_views::ComputedValuesView<'_>,
        border_box: CssPixelRect,
        paints_border_image: bool,
    ) -> IntRect {
        let widest_border = style
            .border_top_width()
            .max(style.border_right_width())
            .max(style.border_bottom_width())
            .max(style.border_left_width());
        let reach = self.stroked_line_reach(widest_border);
        let bounds = self
            .converter
            .enclosing_device_rect(border_box)
            .inflated_edges(reach, reach, reach, reach);
        if !paints_border_image {
            return bounds;
        }
        let [top, right, bottom, left] = border_image::border_image_outsets(style);
        bounds.united(
            self.converter
                .enclosing_device_rect(border_box.inflated(top, right, bottom, left)),
        )
    }

    // A dotted or dashed border is stroked along its center line with mitered joins, and the bounds of a stroke reach
    // out to the miter limit of four times half its width past the line.
    fn stroked_line_reach(&self, width: CssPixels) -> i32 {
        self.converter.enclosing_device_pixels(width) * 2
    }

    fn collapsed_table_borders_bounds(&self, owner: NodeSlotId) -> IntRect {
        let bounds = self.border_box_bounds(owner);
        match table_borders::collapsed_table_borders_bounds(self.source, owner) {
            Some(borders) => bounds.united(self.converter.enclosing_device_rect(borders)),
            None => bounds,
        }
    }

    // A box with lines paints its text, math and caret in its foreground, an inline box only the caret, and a replaced
    // box its content within its border box.
    fn foreground_bounds(&mut self, owner: NodeSlotId) -> IntRect {
        let source = self.source;
        let mut bounds = IntRect::default();
        if node_painting::has_lines(source, owner) {
            let math_paint_data = paintable_geometry::committed_math_paint_data(source, owner);
            if let Some(data) = &math_paint_data {
                let content_box_origin = paintable_geometry::absolute_rect(source, owner).location();
                for rule in &data.rules {
                    bounds = bounds.united(
                        self.converter
                            .enclosing_device_rect(rule.translated_by(content_box_origin)),
                    );
                }
                for run in &data.glyph_runs {
                    bounds = bounds.united(
                        self.converter
                            .enclosing_device_rect(run.ink_bounds.translated_by(content_box_origin)),
                    );
                }
            }
            // Text that inline content items list is painted by producers of its own.
            let paints_text = !math_paint_data.as_ref().is_some_and(|data| data.replaces_text)
                && source
                    .committed_side_data(owner)
                    .inline_content
                    .as_ref()
                    .is_none_or(|content| content.items.is_empty());
            if paints_text {
                let indices = text::owned_fragment_indices(source, owner, None);
                bounds = bounds.united(self.text_fragments_bounds(owner, &indices));
            }
        } else if !node_painting::is_fragmented_inline(source, owner) {
            bounds = self.border_box_bounds(owner);
        }
        if let Some(caret) = self.inputs.caret
            && caret_is_painted_by(caret.target, owner)
        {
            bounds = bounds.united(self.converter.rounded_device_rect(caret.rect));
        }
        bounds
    }

    /// What the glyphs, shadows, highlights and decorations of the given fragments of `root` cover.
    fn text_fragments_bounds(&mut self, root: NodeSlotId, indices: &[u32]) -> IntRect {
        let source = self.source;
        let converter = self.converter;
        let scale = self.inputs.device_pixels_per_css_pixel;
        let spans = text::compute_render_spans(self, root, indices);
        let side = source.committed_side_data(root);
        let mut bounds = IntRect::default();
        for span in &spans {
            let fragment = &side.fragments()[span.fragment_index as usize];
            let fragment_rect = text_fragment::absolute_rect(source, fragment);
            let fragment_device_rect = converter.enclosing_device_rect(fragment_rect);
            bounds = bounds.united(fragment_device_rect);
            if let Some(run) = &fragment.glyph_run {
                let emission = text::glyph_run_emission(fragment, run, fragment_rect, fragment_device_rect, scale);
                bounds = bounds.united(emission.glyph_bounding_rect);
            }
            let shadow_layers = span
                .shadow_layers
                .iter()
                .chain(span.highlights.iter().flat_map(|overlay| &overlay.shadow_layers));
            for layer in shadow_layers {
                let (_, layer_bounding_rect) = text::text_shadow_layer(&converter, scale, fragment_device_rect, layer);
                bounds = bounds.united(layer_bounding_rect);
            }
            if !span.highlights.is_empty() || span.text_fragment_indication_background_color.is_some() {
                bounds = bounds.united(converter.rounded_device_rect(text::highlight_rect(self, root, span)));
            }
            for set in text_decoration::decoration_sets_for_span(self, root, span) {
                bounds = bounds.united(text_decoration::decoration_lines_bounds(
                    &converter,
                    fragment_rect,
                    &set,
                ));
            }
        }
        bounds
    }

    // An inline box outlines each of its pieces, and a box split into fragments each of them, also one without area.
    fn outline_bounds(&mut self, owner: NodeSlotId) -> IntRect {
        let source = self.source;
        let border_boxes: Vec<CssPixelRect> = if node_painting::is_fragmented_inline(source, owner) {
            let piece_indices = source.committed_side_data(owner).piece_indices().to_vec();
            piece_indices
                .iter()
                .filter_map(|index| self.inline_piece_border_box(owner, *index))
                .collect()
        } else {
            paintable_geometry::absolute_border_box_rects_of_fragments(source, owner).to_vec()
        };
        let Some(style) = source.node_style_if_live(owner) else {
            return border_boxes.iter().fold(IntRect::default(), |bounds, border_box| {
                bounds.united(self.converter.enclosing_device_rect(*border_box))
            });
        };
        // The assistive-technology focus target draws an auto outline whatever its style says.
        let width = style_queries::outline_geometry(style)
            .map_or(CssPixels::default(), |geometry| geometry.width)
            .max(style_queries::auto_outline_width());
        let reach = (width + style_queries::outline_offset(source, owner)).max(CssPixels::default());
        let stroke_reach = self.stroked_line_reach(width);
        border_boxes.iter().fold(IntRect::default(), |bounds, border_box| {
            bounds.united(
                self.converter
                    .enclosing_device_rect(border_box.inflated(reach, reach, reach, reach))
                    .inflated_edges(stroke_reach, stroke_reach, stroke_reach, stroke_reach),
            )
        })
    }

    // The resizer and the scrollbars of a small box can reach past its border box.
    fn overlay_bounds(&self, owner: NodeSlotId) -> IntRect {
        let chrome_geometry = ChromeGeometry::for_recording(self.source, self.inputs);
        let mut bounds = self.border_box_bounds(owner);
        if let Some(resizer) = chrome_geometry.absolute_resizer_rect(owner) {
            bounds = bounds.united(self.converter.enclosing_device_rect(resizer));
        }
        for direction in [ScrollDirection::Vertical, ScrollDirection::Horizontal] {
            for enlarged in [false, true] {
                if let Some(scrollbar) = chrome_geometry.compute_scrollbar_data(owner, direction, enlarged) {
                    for rect in [scrollbar.gutter_rect, scrollbar.thumb_rect, scrollbar.track_rect] {
                        bounds = bounds.united(self.converter.enclosing_device_rect(rect));
                    }
                }
            }
        }
        bounds
    }

    /// The border box of the inline box piece of `node` at `index` among the pieces of the box whose lines hold it.
    fn inline_piece_border_box(&mut self, node: NodeSlotId, index: u32) -> Option<CssPixelRect> {
        let source = self.source;
        let block = self.data(node).containing_block;
        if block.is_invalid() || !source.paintable_row_is_populated(block) || !node_painting::has_lines(source, block) {
            return None;
        }
        let root_positions = self.line_root_positions(block);
        let side = source.committed_side_data(block);
        let piece = side.inline_box_pieces().get(index as usize)?;
        Some(
            CssPixelRect::from(piece.border_box_rect)
                .translated_by(root_positions.of_line(side.lines(), piece.line_index as usize)),
        )
    }

    fn inline_piece_bounds(&mut self, owner: NodeSlotId, index: u32) -> IntRect {
        let node = self.inline_piece_node(owner, index);
        let Some(border_box) = self.inline_piece_border_box(node, index) else {
            return IntRect::default();
        };
        let mut bounds = self.outer_box_shadows_bounds(node, border_box);
        let facts = self.base_paint_facts(node);
        // Every piece of an inline with a backdrop filter records the filter's region over the whole inline.
        if facts.has_backdrop_filter {
            bounds = bounds.united(self.border_box_bounds(node));
        }
        match self.source.node_style_if_live(node) {
            Some(style) => bounds.united(self.box_border_bounds(style, border_box, facts.paints_border_image)),
            None => bounds.united(self.converter.enclosing_device_rect(border_box)),
        }
    }

    /// Panics when a producer recorded `recorded_bytes` where its placement does not reach. Producers that the
    /// compositor scrolls nothing of are checked against where they would be placed in the nearest scroll node.
    pub(crate) fn verify_recorded_producer_placement(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        recorded_bytes: Range<usize>,
    ) {
        if recorded_bytes.is_empty() {
            return;
        }
        for culling_scroll_nodes in [CullingScrollNodes::ScrolledByCompositor, CullingScrollNodes::Every] {
            let placement = self.producer_placement(owner, kind, culling_scroll_nodes);
            match placement {
                ProducerPlacement::HitTest | ProducerPlacement::DocumentWide => return,
                ProducerPlacement::NeverCulled => continue,
                ProducerPlacement::AnywhereInScrollNode(_) | ProducerPlacement::WithinScrollNode { .. } => {}
            }
            self.verify_recorded_commands(owner, kind, recorded_bytes.clone(), placement, culling_scroll_nodes);
            if matches!(placement, ProducerPlacement::WithinScrollNode { .. }) {
                return;
            }
        }
    }

    fn verify_recorded_commands(
        &mut self,
        owner: NodeSlotId,
        kind: ProducerKind,
        recorded_bytes: Range<usize>,
        placement: ProducerPlacement,
        culling_scroll_nodes: CullingScrollNodes,
    ) {
        let draws_fixed_background = self.draws_fixed_background(owner, kind);
        let paint_state = self.paint_state;
        let Some(tree) = paint_state.visual_context_tree.as_deref() else {
            return;
        };
        let misplaced = first_misplaced_command(
            self.recorder.bytes(),
            self.recorder.command_runs(),
            recorded_bytes,
            placement,
            |context| self.cull_space(tree, context, culling_scroll_nodes),
            |outer, inner| tree.spatial_is_ancestor_or_self(outer, inner),
            draws_fixed_background,
        );
        if let Some(misplaced) = misplaced {
            panic!(
                "{kind:?} of {owner:?} recorded {misplaced}, which its placement {placement:?} does not reach, culled in \
                 {culling_scroll_nodes:?} scroll nodes"
            );
        }
    }

    // A fixed background records above every scroll node, clipped by its box.
    fn draws_fixed_background(&mut self, owner: NodeSlotId, kind: ProducerKind) -> bool {
        let node = match kind {
            ProducerKind::DrawBackground => owner,
            ProducerKind::InlinePiece(index) => self.inline_piece_node(owner, index),
            _ => return false,
        };
        self.base_paint_facts(node).has_fixed_background
    }
}

fn caret_is_painted_by(target: CaretTarget, owner: NodeSlotId) -> bool {
    match target {
        CaretTarget::InBlock {
            block,
            owner: caret_owner,
        } => caret_owner.map_or(block == owner, |caret_owner| caret_owner == owner),
        CaretTarget::EmptyInline(inline) => inline == owner,
    }
}

/// Encloses the outer shadows of a box with the given device border box: each moved by its offset and grown by twice
/// its blur and by its spread, which a shadow cast from a path grows by up to four times at its miters.
fn outer_box_shadows_bounds(
    converter: &DevicePixelConverter,
    device_border_box: IntRect,
    layers: &[ComputedShadow],
) -> IntRect {
    layers
        .iter()
        .filter(|layer| !layer.is_inner())
        .fold(IntRect::default(), |bounds, layer| {
            let device_pixels = |raw: i32| converter.rounded_device_pixels(CssPixels::from_raw(raw));
            let reach = device_pixels(layer.spread_distance).max(0) * 4 + device_pixels(layer.blur_radius) * 2 + 1;
            let shadow_rect = IntRect::new(
                device_border_box.x + device_pixels(layer.offset_x),
                device_border_box.y + device_pixels(layer.offset_y),
                device_border_box.width,
                device_border_box.height,
            );
            bounds.united(shadow_rect.inflated_edges(reach, reach, reach, reach))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Misplacement {
    /// A scroll node, scrollbar or snap geometry, which the compositor needs for the whole document.
    DocumentWideMetadata,
    OutsideEveryScrollNode,
    /// In a scroll node other than the producer's, or placed inexactly in it.
    InAnotherScrollNode,
    OutsideBoundsEstimate,
}

struct MisplacedCommand {
    misplacement: Misplacement,
    command_type: DisplayListCommandType,
    context: ContextRef,
    cull_space: CullSpace,
    rect: Option<IntRect>,
}

impl std::fmt::Display for MisplacedCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?} with rect {:?} under {:?} in {:?} ({:?})",
            self.command_type, self.rect, self.context, self.cull_space, self.misplacement
        )
    }
}

/// The first top-level command in `recorded_bytes` that `placement` does not reach. A command outside every scroll
/// node the compositor scrolls can be exempt, which a fixed background is.
fn first_misplaced_command(
    bytes: &[u8],
    runs: &[DisplayListCommandRun],
    recorded_bytes: Range<usize>,
    placement: ProducerPlacement,
    cull_space_of: impl Fn(ContextRef) -> CullSpace,
    scroll_node_contains: impl Fn(SpatialNodeIndex, SpatialNodeIndex) -> bool,
    exempts_commands_outside_every_scroll_node: bool,
) -> Option<MisplacedCommand> {
    let (scroll_node, bounds) = match placement {
        ProducerPlacement::WithinScrollNode { scroll_node, bounds } => (scroll_node, Some(bounds)),
        ProducerPlacement::AnywhereInScrollNode(scroll_node) => (scroll_node, None),
        ProducerPlacement::HitTest | ProducerPlacement::DocumentWide | ProducerPlacement::NeverCulled => {
            return None;
        }
    };
    let first_run = runs.partition_point(|run| (run.offset + run.size) as usize <= recorded_bytes.start);
    for run in &runs[first_run..] {
        let run_bytes = run.offset as usize..(run.offset + run.size) as usize;
        if run_bytes.start >= recorded_bytes.end {
            break;
        }
        let cull_space = cull_space_of(run.context);
        // Ok holds whether the run's commands must lie within the bounds estimate.
        let run_placement = match cull_space {
            CullSpace::NeverCulled if exempts_commands_outside_every_scroll_node => Ok(false),
            CullSpace::NeverCulled => Err(Misplacement::OutsideEveryScrollNode),
            CullSpace::ScrollNode {
                scroll_node: space,
                placed_exactly,
            } => match bounds {
                Some(_) if space == scroll_node && placed_exactly => Ok(true),
                None if scroll_node_contains(scroll_node, space) => Ok(false),
                _ => Err(Misplacement::InAnotherScrollNode),
            },
        };
        let mut misplaced = None;
        let checked_bytes = run_bytes.start.max(recorded_bytes.start)..run_bytes.end.min(recorded_bytes.end);
        for_each_command(&bytes[checked_bytes], |header, _, payload| {
            if misplaced.is_some() {
                return;
            }
            let rect = recorded_rect_of_command(header, payload);
            let misplacement = if is_document_wide_metadata(header.command_type) {
                Some(Misplacement::DocumentWideMetadata)
            } else {
                match run_placement {
                    Err(misplacement) => Some(misplacement),
                    Ok(true) => bounds
                        .zip(rect)
                        .filter(|(bounds, rect)| !rect.is_empty() && !bounds.contains_rect(*rect))
                        .map(|_| Misplacement::OutsideBoundsEstimate),
                    Ok(false) => None,
                }
            };
            misplaced = misplacement.map(|misplacement| MisplacedCommand {
                misplacement,
                command_type: header.command_type,
                context: run.context,
                cull_space,
                rect,
            });
        });
        if misplaced.is_some() {
            return misplaced;
        }
    }
    None
}

fn is_document_wide_metadata(command_type: DisplayListCommandType) -> bool {
    matches!(
        command_type,
        DisplayListCommandType::CompositorScrollNode
            | DisplayListCommandType::CompositorScrollbar
            | DisplayListCommandType::CompositorSnapContainer
            | DisplayListCommandType::CompositorSnapArea
    )
}

/// The rect a command draws in, or that compositor metadata applies to.
fn recorded_rect_of_command(header: &DisplayListCommandHeader, payload: &[u8]) -> Option<IntRect> {
    if header.has_bounding_rect {
        return Some(header.bounding_rect);
    }
    let rect = match header.command_type {
        DisplayListCommandType::CompositorWheelHitTestTarget => {
            read_command::<CompositorWheelHitTestTarget>(payload).rect
        }
        DisplayListCommandType::CompositorWheelHitTestTargetWithCornerRadii => {
            read_command::<CompositorWheelHitTestTargetWithCornerRadii>(payload).rect
        }
        DisplayListCommandType::CompositorMainThreadWheelEventRegion => {
            read_command::<CompositorMainThreadWheelEventRegion>(payload).rect
        }
        DisplayListCommandType::CompositorBlockingWheelEventRegion => {
            read_command::<CompositorBlockingWheelEventRegion>(payload).rect
        }
        DisplayListCommandType::CompositorScrollChainingBarrier => {
            read_command::<CompositorScrollChainingBarrier>(payload).rect
        }
        DisplayListCommandType::CompositorNestedContextWheelRegion => {
            read_command::<CompositorNestedContextWheelRegion>(payload).rect
        }
        _ => return None,
    };
    Some(enclosing_int_rect(rect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painting::display_list::builder::DisplayListBuilder;
    use crate::painting::display_list::commands::{
        ClipNodeIndex, CompositorScrollNode, CompositorScrollNodeKind, DisplayListCommand, EffectNodeIndex, FillRect,
        PaintOuterBoxShadow, PaintPathBoxShadow, UniqueNodeId, VISUAL_VIEWPORT_NODE_INDEX,
    };
    use crate::painting::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
    use crate::painting::visual_context::{ScrollData, TransformData, TransformDataRole};
    use libgfx_rust::{Color, CompositingAndBlendingOperator, CornerRadii, FloatMatrix4x4, FloatPoint, FloatRect};

    struct Spaces {
        tree: VisualContextTree,
        cull_spaces: CullSpaces,
        root: SpatialNodeIndex,
        nested: SpatialNodeIndex,
        transformed: SpatialNodeIndex,
        fixed: SpatialNodeIndex,
    }

    fn identity_transform() -> TransformData {
        TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
        }
    }

    fn scroll() -> SpatialData {
        SpatialData::Scroll(ScrollData {
            state_slot: NO_SCROLL_STATE_SLOT,
            owner_paintable: NodeSlotId::INVALID,
            registry_parent_node: VISUAL_VIEWPORT_NODE_INDEX,
        })
    }

    // The root scroll node with a nested one and a transformed box in it, and a fixed box outside both.
    fn spaces() -> Spaces {
        let mut tree = VisualContextTree::create(identity_transform());
        let root = tree.append_spatial(scroll(), VISUAL_VIEWPORT_NODE_INDEX);
        let nested = tree.append_spatial(scroll(), root);
        let transformed = tree.append_spatial(SpatialData::Transform(identity_transform()), root);
        let fixed = tree.append_spatial(SpatialData::Transform(identity_transform()), VISUAL_VIEWPORT_NODE_INDEX);
        let mut scrolled_by_compositor = vec![false; tree.spatial_nodes.len()];
        scrolled_by_compositor[root.0 as usize] = true;
        scrolled_by_compositor[nested.0 as usize] = true;
        let cull_spaces = CullSpaces {
            under_spreading_filter: effects_under_spreading_filters(&tree),
            scrolled_by_compositor,
        };
        Spaces {
            tree,
            cull_spaces,
            root,
            nested,
            transformed,
            fixed,
        }
    }

    fn context(spatial: SpatialNodeIndex) -> ContextRef {
        ContextRef {
            spatial,
            clip: ClipNodeIndex::NONE,
            effect: EffectNodeIndex::NONE,
        }
    }

    fn fill(builder: &mut DisplayListBuilder, rect: IntRect, spatial: SpatialNodeIndex) {
        builder.append(
            &FillRect {
                rect,
                color: Color::from_rgb(0, 0, 0),
                compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
                background_color_animation_effect: EffectNodeIndex::NONE,
            },
            &[],
            context(spatial),
        );
    }

    fn misplacement_from(
        spaces: &Spaces,
        builder: &DisplayListBuilder,
        recorded_bytes: Range<usize>,
        placement: ProducerPlacement,
        exempts_commands_outside_every_scroll_node: bool,
    ) -> Option<Misplacement> {
        first_misplaced_command(
            builder.bytes(),
            builder.command_runs(),
            recorded_bytes,
            placement,
            |context| {
                spaces
                    .cull_spaces
                    .cull_space_of(&spaces.tree, context, CullingScrollNodes::ScrolledByCompositor)
            },
            |outer, inner| spaces.tree.spatial_is_ancestor_or_self(outer, inner),
            exempts_commands_outside_every_scroll_node,
        )
        .map(|misplaced| misplaced.misplacement)
    }

    fn misplacement(
        spaces: &Spaces,
        builder: &DisplayListBuilder,
        placement: ProducerPlacement,
        exempts_commands_outside_every_scroll_node: bool,
    ) -> Option<Misplacement> {
        misplacement_from(
            spaces,
            builder,
            0..builder.byte_size(),
            placement,
            exempts_commands_outside_every_scroll_node,
        )
    }

    fn within(scroll_node: SpatialNodeIndex, bounds: IntRect) -> ProducerPlacement {
        ProducerPlacement::WithinScrollNode { scroll_node, bounds }
    }

    #[test]
    fn producers_are_placed_by_their_cull_space_and_estimated_only_when_placed_exactly() {
        let root = SpatialNodeIndex(1);
        let bounds = IntRect::new(0, 0, 10, 10);
        let exactly = CullSpace::ScrollNode {
            scroll_node: root,
            placed_exactly: true,
        };
        let roughly = CullSpace::ScrollNode {
            scroll_node: root,
            placed_exactly: false,
        };
        let never_estimated = || -> IntRect { panic!("only producers placed exactly are estimated") };
        assert_eq!(
            place(CullSpace::NeverCulled, false, never_estimated),
            ProducerPlacement::NeverCulled
        );
        assert_eq!(
            place(roughly, false, never_estimated),
            ProducerPlacement::AnywhereInScrollNode(root)
        );
        assert_eq!(
            place(exactly, true, never_estimated),
            ProducerPlacement::AnywhereInScrollNode(root)
        );
        assert_eq!(place(exactly, false, || bounds), within(root, bounds));
    }

    #[test]
    fn a_producer_placed_within_bounds_may_draw_only_inside_them() {
        let spaces = spaces();
        let bounds = IntRect::new(0, 0, 100, 100);
        let mut inside = DisplayListBuilder::new();
        fill(&mut inside, IntRect::new(10, 10, 90, 90), spaces.root);
        assert_eq!(misplacement(&spaces, &inside, within(spaces.root, bounds), false), None);

        let mut past_the_bounds = DisplayListBuilder::new();
        fill(&mut past_the_bounds, IntRect::new(10, 10, 20, 20), spaces.root);
        fill(&mut past_the_bounds, IntRect::new(90, 90, 20, 20), spaces.root);
        assert_eq!(
            misplacement(&spaces, &past_the_bounds, within(spaces.root, bounds), false),
            Some(Misplacement::OutsideBoundsEstimate)
        );
        // An empty command draws nothing, wherever it says it is.
        let mut empty = DisplayListBuilder::new();
        fill(&mut empty, IntRect::new(500, 500, 0, 10), spaces.root);
        assert_eq!(misplacement(&spaces, &empty, within(spaces.root, bounds), false), None);
    }

    #[test]
    fn only_what_the_producer_recorded_is_checked() {
        let spaces = spaces();
        let bounds = IntRect::new(0, 0, 100, 100);
        let mut builder = DisplayListBuilder::new();
        fill(&mut builder, IntRect::new(500, 500, 10, 10), spaces.root);
        let producer_start = builder.byte_size();
        fill(&mut builder, IntRect::new(10, 10, 10, 10), spaces.root);
        let producer_end = builder.byte_size();
        fill(&mut builder, IntRect::new(500, 500, 10, 10), spaces.root);
        assert_eq!(
            misplacement_from(
                &spaces,
                &builder,
                producer_start..producer_end,
                within(spaces.root, bounds),
                false
            ),
            None
        );
        assert_eq!(
            misplacement(&spaces, &builder, within(spaces.root, bounds), false),
            Some(Misplacement::OutsideBoundsEstimate)
        );
    }

    #[test]
    fn content_of_another_scroll_node_fits_only_a_placement_anywhere_in_one_around_it() {
        let spaces = spaces();
        let bounds = IntRect::new(0, 0, 100, 100);
        for spatial in [spaces.nested, spaces.transformed] {
            let mut builder = DisplayListBuilder::new();
            fill(&mut builder, IntRect::new(10, 10, 10, 10), spatial);
            assert_eq!(
                misplacement(&spaces, &builder, within(spaces.root, bounds), false),
                Some(Misplacement::InAnotherScrollNode)
            );
            assert_eq!(
                misplacement(
                    &spaces,
                    &builder,
                    ProducerPlacement::AnywhereInScrollNode(spaces.root),
                    false
                ),
                None
            );
        }
        let mut in_the_root = DisplayListBuilder::new();
        fill(&mut in_the_root, IntRect::new(10, 10, 10, 10), spaces.root);
        assert_eq!(
            misplacement(
                &spaces,
                &in_the_root,
                ProducerPlacement::AnywhereInScrollNode(spaces.nested),
                false
            ),
            Some(Misplacement::InAnotherScrollNode)
        );
    }

    #[test]
    fn only_a_fixed_background_may_record_outside_every_scroll_node() {
        let spaces = spaces();
        let bounds = IntRect::new(0, 0, 100, 100);
        let mut builder = DisplayListBuilder::new();
        fill(&mut builder, IntRect::new(10, 10, 10, 10), spaces.root);
        fill(&mut builder, IntRect::new(500, 500, 10, 10), spaces.fixed);
        assert_eq!(
            misplacement(&spaces, &builder, within(spaces.root, bounds), false),
            Some(Misplacement::OutsideEveryScrollNode)
        );
        assert_eq!(misplacement(&spaces, &builder, within(spaces.root, bounds), true), None);
    }

    #[test]
    fn wheel_routing_is_checked_by_its_rect_and_scroll_nodes_are_never_left_to_culling() {
        let spaces = spaces();
        let bounds = IntRect::new(0, 0, 100, 100);
        let wheel_target = |rect: FloatRect| CompositorWheelHitTestTarget {
            document_id: UniqueNodeId(1),
            target_scroll_node_index: spaces.root,
            rect,
        };
        let mut inside = DisplayListBuilder::new();
        inside.append(
            &wheel_target(FloatRect::new(10.0, 10.0, 50.0, 50.0)),
            &[],
            context(spaces.root),
        );
        assert_eq!(misplacement(&spaces, &inside, within(spaces.root, bounds), false), None);

        let mut outside = DisplayListBuilder::new();
        outside.append(
            &wheel_target(FloatRect::new(10.0, 10.0, 50.0, 150.5)),
            &[],
            context(spaces.root),
        );
        assert_eq!(
            misplacement(&spaces, &outside, within(spaces.root, bounds), false),
            Some(Misplacement::OutsideBoundsEstimate)
        );

        let mut scroll_node = DisplayListBuilder::new();
        scroll_node.append(
            &CompositorScrollNode {
                document_id: UniqueNodeId(1),
                scrollable_node_id: UniqueNodeId(2),
                scroll_node_index: spaces.nested,
                parent_scroll_node_index: spaces.root,
                scrollport_rect: IntRect::new(10, 10, 10, 10),
                min_scroll_offset: FloatPoint::default(),
                max_scroll_offset: FloatPoint { x: 0.0, y: 100.0 },
                scroll_node_kind: CompositorScrollNodeKind::Element,
                pseudo_element_type: 0,
                is_viewport: false,
                can_be_wheel_scrolled_horizontally: false,
                can_be_wheel_scrolled_vertically: true,
                chains_scroll_to_ancestors_horizontally: true,
                chains_scroll_to_ancestors_vertically: true,
                shows_overscroll_effect_horizontally: true,
                shows_overscroll_effect_vertically: true,
            },
            &[],
            context(spaces.root),
        );
        assert_eq!(
            misplacement(
                &spaces,
                &scroll_node,
                ProducerPlacement::AnywhereInScrollNode(spaces.root),
                false
            ),
            Some(Misplacement::DocumentWideMetadata)
        );
    }

    #[test]
    fn outer_box_shadow_estimates_cover_the_shadows_recorded_for_a_rect_or_a_shaped_box() {
        let converter = DevicePixelConverter::new(2.0);
        let css = |pixels: i64| CssPixels::from_integer(pixels).raw_value();
        let shadow = |offset_x: i64, offset_y: i64, blur_radius: i64, spread_distance: i64| ComputedShadow {
            offset_x: css(offset_x),
            offset_y: css(offset_y),
            blur_radius: css(blur_radius),
            spread_distance: css(spread_distance),
            color: 0xff000000,
            color_syntax: 0,
            placement: 0,
        };
        let border_box = IntRect::new(100, 200, 300, 40);
        for layer in [shadow(0, 0, 0, 0), shadow(7, -3, 5, 10), shadow(-20, 15, 2, -4)] {
            let bounds = outer_box_shadows_bounds(&converter, border_box, &[layer]);
            let device_pixels = |raw: i32| converter.rounded_device_pixels(CssPixels::from_raw(raw));
            let (offset_x, offset_y) = (device_pixels(layer.offset_x), device_pixels(layer.offset_y));
            let (blur_radius, spread_distance) =
                (device_pixels(layer.blur_radius), device_pixels(layer.spread_distance));
            let mut shadow_rect =
                border_box.inflated_edges(spread_distance, spread_distance, spread_distance, spread_distance);
            shadow_rect.x += offset_x;
            shadow_rect.y += offset_y;
            let rect_shadow = PaintOuterBoxShadow {
                color: Color::from_rgb(0, 0, 0),
                blur_radius,
                device_content_rect: border_box,
                content_corner_radii: CornerRadii::default(),
                shadow_rect,
                shadow_corner_radii: CornerRadii::default(),
            };
            let shaped_shadow = PaintPathBoxShadow {
                color: Color::from_rgb(0, 0, 0),
                blur_radius,
                spread_distance: spread_distance as f32,
                offset: FloatPoint {
                    x: offset_x as f32,
                    y: offset_y as f32,
                },
                shape_path_bounding_rect: border_box.to_float(),
                shape_path_data: Default::default(),
                inner: false,
            };
            assert!(bounds.contains_rect(rect_shadow.bounding_rect().unwrap()));
            assert!(bounds.contains_rect(shaped_shadow.bounding_rect().unwrap()));
        }
        let inset = ComputedShadow {
            placement: 1,
            ..shadow(50, 50, 50, 50)
        };
        assert!(outer_box_shadows_bounds(&converter, border_box, &[inset]).is_empty());
    }
}
