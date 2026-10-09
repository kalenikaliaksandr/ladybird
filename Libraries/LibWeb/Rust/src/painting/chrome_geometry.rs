/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css::css_enums;
use crate::css::css_pixels::{CssPixelFraction, CssPixelPoint, CssPixelRect, CssPixels};
use crate::layout::node_data::{NodeKind, NodeSlotId};
use crate::painting::display_list::commands::VISUAL_VIEWPORT_NODE_INDEX;
use crate::painting::ffi::{FfiChromeMetrics, ScrollDirection, ScrollbarStyle};
use crate::painting::host::{FfiHitTestQueryCallbacks, RootBackgroundSource};
use crate::painting::paint_read::PaintRead;
use crate::painting::paintable_data::PaintableFlag;
use crate::painting::paintable_geometry;
use crate::painting::record::RecordingInputs;
use crate::painting::style_queries;
use libgfx_rust::Color;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollbarData {
    pub(crate) gutter_rect: CssPixelRect,
    pub(crate) thumb_rect: CssPixelRect,
    pub(crate) track_rect: CssPixelRect,
    pub(crate) thumb_travel_to_scroll_ratio: CssPixelFraction,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollbarScrollState {
    pub(crate) device_scroll_offset: f32,
    pub(crate) device_pixels_per_css_pixel: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PhysicalAxes {
    pub(crate) horizontal: bool,
    pub(crate) vertical: bool,
}

impl PhysicalAxes {
    fn along(self, direction: ScrollDirection) -> bool {
        match direction {
            ScrollDirection::Horizontal => self.horizontal,
            ScrollDirection::Vertical => self.vertical,
        }
    }
}

fn primary_size(rect: CssPixelRect, direction: ScrollDirection) -> CssPixels {
    match direction {
        ScrollDirection::Horizontal => rect.width,
        ScrollDirection::Vertical => rect.height,
    }
}

fn primary_offset(point: CssPixelPoint, direction: ScrollDirection) -> CssPixels {
    match direction {
        ScrollDirection::Horizontal => point.x,
        ScrollDirection::Vertical => point.y,
    }
}

struct AxisView<'r> {
    primary_offset: &'r mut CssPixels,
    primary_size: &'r mut CssPixels,
    secondary_offset: &'r mut CssPixels,
    secondary_size: &'r mut CssPixels,
}

fn axis_view(rect: &mut CssPixelRect, direction: ScrollDirection) -> AxisView<'_> {
    match direction {
        ScrollDirection::Horizontal => AxisView {
            primary_offset: &mut rect.x,
            primary_size: &mut rect.width,
            secondary_offset: &mut rect.y,
            secondary_size: &mut rect.height,
        },
        ScrollDirection::Vertical => AxisView {
            primary_offset: &mut rect.y,
            primary_size: &mut rect.height,
            secondary_offset: &mut rect.x,
            secondary_size: &mut rect.width,
        },
    }
}

pub(crate) fn is_chrome_mirrored(arena: &impl PaintRead, slot: NodeSlotId) -> bool {
    arena.node_style_if_live(slot).is_some_and(|style| {
        let writing_mode = style.writing_mode();
        (writing_mode == css_enums::writing_mode::HORIZONTAL_TB && style.direction() == css_enums::direction::RTL)
            || writing_mode == css_enums::writing_mode::VERTICAL_RL
            || writing_mode == css_enums::writing_mode::SIDEWAYS_RL
    })
}

pub(crate) fn physical_resize_axes(arena: &impl PaintRead, slot: NodeSlotId) -> PhysicalAxes {
    let Some(style) = arena.node_style_if_live(slot) else {
        return PhysicalAxes::default();
    };
    let box_values = style.box_values();
    if box_values.resize == css_enums::resize::NONE {
        return PhysicalAxes::default();
    }
    if style.display().is_inline_outside() && style.display().is_flow_inside() {
        return PhysicalAxes::default();
    }

    let horizontal_writing_mode = style.writing_mode() == css_enums::writing_mode::HORIZONTAL_TB;
    let overflow_allows_resize =
        |overflow| overflow != css_enums::overflow::VISIBLE && overflow != css_enums::overflow::CLIP;
    PhysicalAxes {
        horizontal: overflow_allows_resize(box_values.overflow_x)
            && (box_values.resize == css_enums::resize::BOTH
                || box_values.resize == css_enums::resize::HORIZONTAL
                || (box_values.resize == css_enums::resize::INLINE && horizontal_writing_mode)
                || (box_values.resize == css_enums::resize::BLOCK && !horizontal_writing_mode)),
        vertical: overflow_allows_resize(box_values.overflow_y)
            && (box_values.resize == css_enums::resize::BOTH
                || box_values.resize == css_enums::resize::VERTICAL
                || (box_values.resize == css_enums::resize::INLINE && !horizontal_writing_mode)
                || (box_values.resize == css_enums::resize::BLOCK && horizontal_writing_mode)),
    }
}

pub(crate) fn has_resizer(arena: &impl PaintRead, slot: NodeSlotId) -> bool {
    if !arena.paintable_row_is_populated(slot)
        || arena.node_kind_if_live(slot) == Some(NodeKind::Viewport)
        || arena.node_is_generated_for_pseudo_element(slot)
    {
        return false;
    }
    let axes = physical_resize_axes(arena, slot);
    axes.horizontal || axes.vertical
}

// NB: The viewport's own overflow is the one the layout pass propagated to it, as applied to the viewport.
pub(crate) fn wheel_scrollable_axes(arena: &impl PaintRead, slot: NodeSlotId) -> PhysicalAxes {
    let Some(style) = arena.node_style_if_live(slot) else {
        return PhysicalAxes::default();
    };
    let box_values = style.box_values();
    let allows_wheel_scrolling =
        |overflow| overflow == css_enums::overflow::AUTO || overflow == css_enums::overflow::SCROLL;
    let mut axes = PhysicalAxes {
        horizontal: allows_wheel_scrolling(box_values.overflow_x),
        vertical: allows_wheel_scrolling(box_values.overflow_y),
    };
    if !axes.horizontal && !axes.vertical {
        return axes;
    }

    let Some(scrollable_overflow) = paintable_geometry::scrollable_overflow_rect(arena, slot) else {
        return PhysicalAxes::default();
    };
    let scrollport = paintable_geometry::absolute_padding_box_rect(arena, slot);
    axes.horizontal &= scrollable_overflow.width > scrollport.width;
    axes.vertical &= scrollable_overflow.height > scrollport.height;
    axes
}

pub(crate) fn scroll_offset_bounds(arena: &impl PaintRead, slot: NodeSlotId) -> Option<(CssPixelPoint, CssPixelPoint)> {
    let overflow = paintable_geometry::scrollable_overflow_rect(arena, slot)?;
    let scrollport = paintable_geometry::absolute_padding_box_rect(arena, slot);
    let zero = CssPixels::from_raw(0);
    let minimum = CssPixelPoint::new(
        (overflow.left() - scrollport.left()).min(zero),
        (overflow.top() - scrollport.top()).min(zero),
    );
    let maximum = CssPixelPoint::new(
        (overflow.right() - scrollport.right()).max(zero),
        (overflow.bottom() - scrollport.bottom()).max(zero),
    );
    Some((minimum, maximum))
}

pub(crate) fn minimum_scroll_offset(arena: &impl PaintRead, slot: NodeSlotId) -> CssPixelPoint {
    scroll_offset_bounds(arena, slot).map_or(CssPixelPoint::default(), |(minimum, _)| minimum)
}

pub(crate) fn maximum_scroll_offset(arena: &impl PaintRead, slot: NodeSlotId) -> CssPixelPoint {
    scroll_offset_bounds(arena, slot).map_or(CssPixelPoint::default(), |(_, maximum)| maximum)
}

pub(crate) fn scrollbar_is_enlarged(arena: &impl PaintRead, slot: NodeSlotId, direction: ScrollDirection) -> bool {
    let flag = match direction {
        ScrollDirection::Horizontal => PaintableFlag::HorizontalScrollbarEnlarged,
        ScrollDirection::Vertical => PaintableFlag::VerticalScrollbarEnlarged,
    };
    arena.paintable_data(slot).has_flag(flag)
}

pub(crate) struct ChromeGeometry<'a, Arena: PaintRead> {
    pub(crate) arena: &'a Arena,
    pub(crate) metrics: FfiChromeMetrics,
}

impl<'a, Arena: PaintRead> ChromeGeometry<'a, Arena> {
    pub(crate) fn for_recording(arena: &'a Arena, inputs: &RecordingInputs) -> Self {
        Self {
            arena,
            metrics: inputs.uncaptured.chrome_metrics,
        }
    }

    pub(crate) fn for_hit_test_query(arena: &'a Arena, callbacks: &FfiHitTestQueryCallbacks) -> Self {
        Self {
            arena,
            metrics: callbacks.chrome_metrics,
        }
    }

    fn wheel_scrollable_axes(&self, slot: NodeSlotId) -> PhysicalAxes {
        wheel_scrollable_axes(self.arena, slot)
    }

    /// The strip of the box's vertical scrollbar gutter beside its padding box, as x and width,
    /// where the box shows a vertical scrollbar.
    fn vertical_scrollbar_strip(&self, slot: NodeSlotId, padding_rect: CssPixelRect) -> Option<(CssPixels, CssPixels)> {
        let gutters = paintable_geometry::committed_scrollbar_gutters(self.arena, slot);
        if !gutters.has_vertical_scrollbar {
            return None;
        }
        // NB: Layout puts the scrollbar on the left for a mirrored box, but the viewport keeps it on the right.
        let on_left = gutters.left > CssPixels::from_raw(0)
            && (gutters.right == CssPixels::from_raw(0)
                || (is_chrome_mirrored(self.arena, slot)
                    && self.arena.node_kind_if_live(slot) != Some(NodeKind::Viewport)));
        Some(if on_left {
            (padding_rect.x - gutters.left, gutters.left)
        } else {
            (padding_rect.right(), gutters.right)
        })
    }

    /// The strip of the box's horizontal scrollbar gutter below its padding box, as y and height,
    /// where the box shows a horizontal scrollbar.
    fn horizontal_scrollbar_strip(
        &self,
        slot: NodeSlotId,
        padding_rect: CssPixelRect,
    ) -> Option<(CssPixels, CssPixels)> {
        let gutters = paintable_geometry::committed_scrollbar_gutters(self.arena, slot);
        gutters
            .has_horizontal_scrollbar
            .then_some((padding_rect.bottom(), gutters.bottom))
    }

    /// Where the resizer goes: in the corner the scrollbar gutters leave, or in the corner of the
    /// padding box on the side the box has no scrollbar.
    pub(crate) fn absolute_resizer_rect(&self, slot: NodeSlotId) -> Option<CssPixelRect> {
        if !has_resizer(self.arena, slot) {
            return None;
        }
        let padding_rect = paintable_geometry::absolute_padding_box_rect(self.arena, slot);
        let gripper_size = self.metrics.resize_gripper_size;
        let (x, width) = self.vertical_scrollbar_strip(slot, padding_rect).unwrap_or_else(|| {
            if is_chrome_mirrored(self.arena, slot) {
                (padding_rect.x, gripper_size)
            } else {
                (padding_rect.right() - gripper_size, gripper_size)
            }
        });
        let (y, height) = self
            .horizontal_scrollbar_strip(slot, padding_rect)
            .unwrap_or((padding_rect.bottom() - gripper_size, gripper_size));
        Some(CssPixelRect::new(x, y, width, height))
    }

    pub(crate) fn resizer_contains(&self, slot: NodeSlotId, point: CssPixelPoint) -> bool {
        if !self.arena.paintable_row_is_populated(slot) {
            return false;
        }
        let Some(mut rect) = self.absolute_resizer_rect(slot) else {
            return false;
        };
        let border = paintable_geometry::committed_border(self.arena, slot);
        if is_chrome_mirrored(self.arena, slot) {
            rect.x -= border.left;
            rect.width += border.left;
        } else {
            rect.width += border.right;
        }
        rect.height += border.bottom;
        rect.contains_point(point)
    }

    /// How thick an overlay scrollbar's track is while it is enlarged, and its thumb then.
    fn enlarged_overlay_scrollbar_thicknesses(&self, slot: NodeSlotId) -> (CssPixels, CssPixels) {
        let metrics = self.metrics;
        let thin = self
            .arena
            .node_style_if_live(slot)
            .is_some_and(|style| style.misc_reset().scrollbar_width == css_enums::scrollbar_width::THIN);
        if thin {
            let track_thickness = metrics.scroll_gutter_thickness_thin;
            (
                track_thickness,
                metrics.scroll_thumb_thickness_thin.min(track_thickness),
            )
        } else {
            let track_thickness = metrics.scroll_gutter_thickness;
            (track_thickness, metrics.scroll_thumb_thickness.min(track_thickness))
        }
    }

    fn available_overlay_scrollbar_length(&self, slot: NodeSlotId, direction: ScrollDirection) -> CssPixels {
        if !self.arena.paintable_row_is_populated(slot) {
            return CssPixels::from_raw(0);
        }
        let mut length = primary_size(
            paintable_geometry::absolute_padding_box_rect(self.arena, slot),
            direction,
        );
        let axes = self.wheel_scrollable_axes(slot);
        let other_axis_scrolls = match direction {
            ScrollDirection::Horizontal => axes.vertical,
            ScrollDirection::Vertical => axes.horizontal,
        };
        if has_resizer(self.arena, slot) {
            length -= self.metrics.resize_gripper_size;
        } else if other_axis_scrolls {
            length -= self.enlarged_overlay_scrollbar_thicknesses(slot).0;
        }
        length
    }

    /// The overlay scrollbar's track, inside the padding box along its end edge. It shows where the
    /// box can be scrolled along `direction`.
    fn absolute_overlay_scrollbar_rect(
        &self,
        slot: NodeSlotId,
        direction: ScrollDirection,
        enlarged: bool,
    ) -> Option<CssPixelRect> {
        if !self.arena.paintable_row_is_populated(slot) {
            return None;
        }
        let axes = self.wheel_scrollable_axes(slot);
        if !axes.along(direction) {
            return None;
        }
        let style = self.arena.node_style_if_live(slot)?;
        if style.misc_reset().scrollbar_width == css_enums::scrollbar_width::NONE {
            return None;
        }

        let metrics = self.metrics;
        let adjusting_for_resizer = has_resizer(self.arena, slot);
        let mirrored = is_chrome_mirrored(self.arena, slot);
        let (enlarged_track_thickness, _) = self.enlarged_overlay_scrollbar_thicknesses(slot);
        let rect_thickness = if enlarged {
            enlarged_track_thickness
        } else {
            metrics.scroll_thumb_thickness_thin + metrics.scroll_thumb_padding_thin
        };
        let zero = CssPixels::from_raw(0);
        let mut rect = paintable_geometry::absolute_padding_box_rect(self.arena, slot);
        match direction {
            ScrollDirection::Horizontal => {
                if !adjusting_for_resizer && axes.vertical {
                    rect.width = (rect.width - enlarged_track_thickness).max(zero);
                    if mirrored {
                        rect.x += enlarged_track_thickness;
                    }
                } else if adjusting_for_resizer {
                    rect.width = self.available_overlay_scrollbar_length(slot, direction);
                    if mirrored {
                        rect.x += metrics.resize_gripper_size;
                    }
                }
                rect.y = (rect.bottom() - rect_thickness).max(zero);
                rect.height = rect_thickness;
            }
            ScrollDirection::Vertical => {
                if adjusting_for_resizer {
                    rect.height = self.available_overlay_scrollbar_length(slot, direction);
                }
                if !mirrored {
                    rect.x = (rect.right() - rect_thickness).max(zero);
                }
                rect.width = rect_thickness;
            }
        }
        Some(rect)
    }

    /// An idle overlay scrollbar shows a thin thumb over the content. An enlarged one, hovered or
    /// dragged, shows a wider track behind a thicker thumb.
    fn compute_overlay_scrollbar_data(
        &self,
        slot: NodeSlotId,
        direction: ScrollDirection,
        enlarged: bool,
        scroll_state: Option<ScrollbarScrollState>,
    ) -> Option<ScrollbarData> {
        let arena = self.arena;
        let metrics = self.metrics;
        if !arena.paintable_row_is_populated(slot) {
            return None;
        }
        let style = arena.node_style_if_live(slot)?;
        let overflow = match direction {
            ScrollDirection::Horizontal => style.box_values().overflow_x,
            ScrollDirection::Vertical => style.box_values().overflow_y,
        };
        if overflow != css_enums::overflow::SCROLL && !self.wheel_scrollable_axes(slot).along(direction) {
            return None;
        }
        if arena.paintable_data(slot).own_scroll_node_index == VISUAL_VIEWPORT_NODE_INDEX {
            return None;
        }
        let overflow_length = primary_size(paintable_geometry::scrollable_overflow_rect(arena, slot)?, direction);
        if overflow_length == CssPixels::from_raw(0) {
            return None;
        }
        let scrollbar_rect = self.absolute_overlay_scrollbar_rect(slot, direction, enlarged)?;
        let (thumb_thickness, thumb_margin) = if enlarged {
            let (track_thickness, thumb_thickness) = self.enlarged_overlay_scrollbar_thicknesses(slot);
            (thumb_thickness, (track_thickness - thumb_thickness) / 2)
        } else {
            (metrics.scroll_thumb_thickness_thin, metrics.scroll_thumb_padding_thin)
        };
        let usable_length = (primary_size(scrollbar_rect, direction) - thumb_margin * 2).max(CssPixels::from_raw(0));
        let scrollport_size = primary_size(paintable_geometry::absolute_padding_box_rect(arena, slot), direction);
        let min_thumb_length = usable_length.min(metrics.scroll_thumb_min_length);
        let thumb_length = usable_length
            .mul_by_fraction(CssPixelFraction::ratio_of(scrollport_size, overflow_length))
            .max(min_thumb_length);
        let ratio = if overflow_length > scrollport_size {
            CssPixelFraction::ratio_of(usable_length - thumb_length, overflow_length - scrollport_size)
        } else {
            CssPixelFraction::zero()
        };
        let mut thumb_rect = scrollbar_rect;
        let thumb = axis_view(&mut thumb_rect, direction);
        *thumb.primary_size = thumb_length;
        *thumb.secondary_size = thumb_thickness;
        let minimum_offset = primary_offset(minimum_scroll_offset(arena, slot), direction);
        *thumb.primary_offset += thumb_margin - minimum_offset.mul_by_fraction(ratio);
        if enlarged || (direction == ScrollDirection::Vertical && is_chrome_mirrored(arena, slot)) {
            *thumb.secondary_offset += thumb_margin;
        }
        if let Some(scroll_state) = scroll_state {
            let scroll_offset = CssPixels::nearest_value_for_f32(
                scroll_state.device_scroll_offset / scroll_state.device_pixels_per_css_pixel as f32,
            );
            *thumb.primary_offset += scroll_offset.mul_by_fraction(ratio);
        }
        Some(ScrollbarData {
            gutter_rect: if enlarged {
                scrollbar_rect
            } else {
                CssPixelRect::default()
            },
            thumb_rect,
            track_rect: scrollbar_rect,
            thumb_travel_to_scroll_ratio: ratio,
        })
    }

    /// The corner between the box's two scrollbars, where it shows both.
    pub(crate) fn absolute_scroll_corner_rect(&self, slot: NodeSlotId) -> Option<CssPixelRect> {
        if !self.arena.paintable_row_is_populated(slot) {
            return None;
        }
        let padding_rect = paintable_geometry::absolute_padding_box_rect(self.arena, slot);
        let (x, width) = self.vertical_scrollbar_strip(slot, padding_rect)?;
        let (y, height) = self.horizontal_scrollbar_strip(slot, padding_rect)?;
        Some(CssPixelRect::new(x, y, width, height))
    }

    /// The scrollbar's track. A classic scrollbar keeps it in its gutter, and an overlay one over the
    /// padding box, wider while it is `enlarged`.
    pub(crate) fn absolute_scrollbar_rect(
        &self,
        slot: NodeSlotId,
        direction: ScrollDirection,
        enlarged: bool,
    ) -> Option<CssPixelRect> {
        match self.metrics.scrollbar_style {
            ScrollbarStyle::Classic => self.absolute_gutter_scrollbar_rect(slot, direction),
            ScrollbarStyle::Overlay => self.absolute_overlay_scrollbar_rect(slot, direction, enlarged),
        }
    }

    pub(crate) fn compute_scrollbar_data(
        &self,
        slot: NodeSlotId,
        direction: ScrollDirection,
        enlarged: bool,
        scroll_state: Option<ScrollbarScrollState>,
    ) -> Option<ScrollbarData> {
        match self.metrics.scrollbar_style {
            ScrollbarStyle::Classic => self.compute_gutter_scrollbar_data(slot, direction, enlarged, scroll_state),
            ScrollbarStyle::Overlay => self.compute_overlay_scrollbar_data(slot, direction, enlarged, scroll_state),
        }
    }

    /// The classic scrollbar's track: the strip of its gutter along the padding box, less the resizer
    /// where the resizer shares the strip.
    fn absolute_gutter_scrollbar_rect(&self, slot: NodeSlotId, direction: ScrollDirection) -> Option<CssPixelRect> {
        if !self.arena.paintable_row_is_populated(slot) {
            return None;
        }
        let padding_rect = paintable_geometry::absolute_padding_box_rect(self.arena, slot);
        let vertical_strip = self.vertical_scrollbar_strip(slot, padding_rect);
        let horizontal_strip = self.horizontal_scrollbar_strip(slot, padding_rect);
        let resizer = self.absolute_resizer_rect(slot);
        let zero = CssPixels::from_raw(0);
        match direction {
            ScrollDirection::Vertical => {
                let (x, width) = vertical_strip?;
                let mut rect = CssPixelRect::new(x, padding_rect.y, width, padding_rect.height);
                if let Some(resizer) = resizer
                    && horizontal_strip.is_none()
                {
                    rect.height = (rect.height - resizer.height).max(zero);
                }
                Some(rect)
            }
            ScrollDirection::Horizontal => {
                let (y, height) = horizontal_strip?;
                let mut rect = CssPixelRect::new(padding_rect.x, y, padding_rect.width, height);
                if let Some(resizer) = resizer
                    && vertical_strip.is_none()
                {
                    rect.width = (rect.width - resizer.width).max(zero);
                    if is_chrome_mirrored(self.arena, slot) {
                        rect.x += resizer.width;
                    }
                }
                Some(rect)
            }
        }
    }

    /// An idle classic scrollbar shows a thin thumb in its gutter. An enlarged one, hovered or
    /// dragged, fills its gutter with its track and shows a thicker thumb.
    fn compute_gutter_scrollbar_data(
        &self,
        slot: NodeSlotId,
        direction: ScrollDirection,
        enlarged: bool,
        scroll_state: Option<ScrollbarScrollState>,
    ) -> Option<ScrollbarData> {
        let arena = self.arena;
        let metrics = self.metrics;
        if !arena.paintable_row_is_populated(slot) {
            return None;
        }
        let style = arena.node_style_if_live(slot)?;
        if arena.paintable_data(slot).own_scroll_node_index == VISUAL_VIEWPORT_NODE_INDEX {
            return None;
        }
        let track_rect = self.absolute_gutter_scrollbar_rect(slot, direction)?;
        let zero = CssPixels::from_raw(0);
        let gutter_thickness = match direction {
            ScrollDirection::Horizontal => track_rect.height,
            ScrollDirection::Vertical => track_rect.width,
        };
        let enlarged_thumb_thickness = if style.misc_reset().scrollbar_width == css_enums::scrollbar_width::THIN {
            metrics.scroll_thumb_thickness_thin
        } else {
            metrics.scroll_thumb_thickness
        }
        .min(gutter_thickness);
        let thumb_thickness = if enlarged {
            enlarged_thumb_thickness
        } else {
            metrics.scroll_thumb_thickness_thin.min(enlarged_thumb_thickness)
        };
        // NB: The thumb keeps its length and travel when it enlarges, so that only its thickness changes.
        let thumb_margin_along_track = (gutter_thickness - enlarged_thumb_thickness) / 2;
        let gutter_rect = if enlarged { track_rect } else { CssPixelRect::default() };

        // A scrollbar with nothing to scroll shows no thumb.
        let overflow_length = paintable_geometry::scrollable_overflow_rect(arena, slot)
            .map_or(zero, |overflow| primary_size(overflow, direction));
        let scrollport_size = primary_size(paintable_geometry::absolute_padding_box_rect(arena, slot), direction);
        if overflow_length <= scrollport_size {
            return Some(ScrollbarData {
                gutter_rect,
                thumb_rect: CssPixelRect::default(),
                track_rect,
                thumb_travel_to_scroll_ratio: CssPixelFraction::zero(),
            });
        }

        let usable_length = (primary_size(track_rect, direction) - thumb_margin_along_track * 2).max(zero);
        let min_thumb_length = usable_length.min(metrics.scroll_thumb_min_length);
        let thumb_length = usable_length
            .mul_by_fraction(CssPixelFraction::ratio_of(scrollport_size, overflow_length))
            .max(min_thumb_length);
        let ratio = CssPixelFraction::ratio_of(usable_length - thumb_length, overflow_length - scrollport_size);
        let mut thumb_rect = track_rect;
        let thumb = axis_view(&mut thumb_rect, direction);
        *thumb.primary_size = thumb_length;
        *thumb.secondary_size = thumb_thickness;
        let minimum_offset = primary_offset(minimum_scroll_offset(arena, slot), direction);
        *thumb.primary_offset += thumb_margin_along_track - minimum_offset.mul_by_fraction(ratio);
        *thumb.secondary_offset += (gutter_thickness - thumb_thickness) / 2;
        if let Some(scroll_state) = scroll_state {
            let scroll_offset = CssPixels::nearest_value_for_f32(
                scroll_state.device_scroll_offset / scroll_state.device_pixels_per_css_pixel as f32,
            );
            *thumb.primary_offset += scroll_offset.mul_by_fraction(ratio);
        }
        Some(ScrollbarData {
            gutter_rect,
            thumb_rect,
            track_rect,
            thumb_travel_to_scroll_ratio: ratio,
        })
    }
}

fn is_canvas_background_source(
    arena: &impl PaintRead,
    slot: NodeSlotId,
    root_background_source: RootBackgroundSource,
) -> bool {
    style_queries::node_is_root_element(arena, slot)
        || (root_background_source.use_body_background_properties && root_background_source.body_layout_node == slot)
}

pub(crate) fn scrollbar_colors_for_paint(
    arena: &impl PaintRead,
    slot: NodeSlotId,
    root_background_source: RootBackgroundSource,
    canvas_background_color: Color,
) -> (Color, Color) {
    let Some(style) = arena.node_style_if_live(slot) else {
        return (Color::TRANSPARENT, Color::TRANSPARENT);
    };
    let colors = style.inherited_ui().scrollbar_color;
    if !colors.is_auto {
        return (Color(colors.thumb_color), Color(colors.track_color));
    }

    let mut ancestors = Vec::new();
    let mut current = Some(slot);
    while let Some(ancestor) = current {
        ancestors.push(ancestor);
        current = arena.node_parent_if_live(ancestor);
    }
    let mut background_color = canvas_background_color;
    for ancestor in ancestors.into_iter().rev() {
        if is_canvas_background_source(arena, ancestor, root_background_source) {
            continue;
        }
        let Some(style) = arena.node_style_if_live(ancestor) else {
            continue;
        };
        let color = Color(style.background().background_color);
        if color.alpha() != 0 {
            background_color = background_color.blend(color);
        }
    }
    let black_thumb = Color::from_rgb(0, 0, 0).with_alpha(128);
    let white_thumb = Color::from_rgb(255, 255, 255).with_alpha(128);
    let black_contrast = background_color.contrast_ratio(background_color.blend(black_thumb));
    let white_contrast = background_color.contrast_ratio(background_color.blend(white_thumb));
    let thumb = if black_contrast >= white_contrast {
        black_thumb
    } else {
        white_thumb
    };
    (thumb, thumb.with_alpha(25))
}
