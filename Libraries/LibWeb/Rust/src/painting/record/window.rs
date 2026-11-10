/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The part of a document a recording holds.
//!
//! A recording holds what shows in the viewport and for a margin around it, with every scroller where it is, instead
//! of the whole document. The window is placed where the scrollers are and stays there while each of them is within
//! half the margin of where it was, so scrolling a little changes nothing a recording reads. Once a scroller is
//! further than that, the next recording places the window again: what came into it is recorded, and what left it is
//! dropped.
//!
//! Whether a part of the document lies outside the window is decided from a bound of what it paints, a rect in the
//! space of a node of the visual context tree. Only scrolling places a bound: where nothing but scrollers lies
//! between the node and the root, the bound is where layout put it, moved by how far the scrollers above it have
//! scrolled. Under any other node nothing is left out. A transform, a sticky offset and an anchor's shift change
//! without a recording, some of them on the compositor, and whatever one of them moved into view would be missing.

use crate::css::css_pixels::CssPixelPoint;
use crate::css::style::fast_hash::FastMap;
use crate::painting::display_list::commands::{ClipMode, ContextRef, SpatialNodeIndex};
use crate::painting::published_frame::PublishedPaintState;
use crate::painting::record::order_tree::InkBound;
use crate::painting::visual_context::{
    ClipNodeData, SpatialData, VISUAL_VIEWPORT_NODE_INDEX, VisualContextTree, WorldTransforms,
};
use libgfx_rust::{FloatPoint, FloatRect, IntSize};
use std::sync::Arc;

/// How far the window reaches past the viewport on each side, in viewports.
const MARGIN_IN_VIEWPORTS: f32 = 2.0;
/// The margin of a viewport smaller than this is the margin of one this size, in CSS pixels.
const SMALLEST_VIEWPORT_THE_MARGIN_FOLLOWS: f32 = 500.0;

#[derive(Debug)]
pub(crate) struct RecordingWindow {
    /// The viewport with the margin around it, in device pixels in the root's space.
    rect: FloatRect,
    device_viewport_size: IntSize,
    device_pixels_per_css_pixel: f64,
    /// How far each scroller had moved what it holds when the window was placed, by its scroll state slot. A
    /// scroller with no entry, which came after the window, counts as not scrolled.
    scroll_own_offsets: Arc<Vec<CssPixelPoint>>,
    /// How far a scroller may be from where it was, in CSS pixels on each axis, before the window is placed again.
    slack: (f32, f32),
}

impl RecordingWindow {
    fn placed_at(
        device_viewport_size: IntSize,
        device_pixels_per_css_pixel: f64,
        scroll_own_offsets: &Arc<Vec<CssPixelPoint>>,
    ) -> Self {
        let scale = device_pixels_per_css_pixel as f32;
        let margin = |device_extent: i32| {
            let css_extent = (device_extent as f32 / scale).max(SMALLEST_VIEWPORT_THE_MARGIN_FOLLOWS);
            css_extent * MARGIN_IN_VIEWPORTS
        };
        let (margin_x, margin_y) = (margin(device_viewport_size.width), margin(device_viewport_size.height));
        Self {
            rect: FloatRect::new(
                0.0,
                0.0,
                device_viewport_size.width as f32,
                device_viewport_size.height as f32,
            )
            .inflated(2.0 * margin_x * scale, 2.0 * margin_y * scale),
            device_viewport_size,
            device_pixels_per_css_pixel,
            scroll_own_offsets: Arc::clone(scroll_own_offsets),
            slack: (margin_x / 2.0, margin_y / 2.0),
        }
    }

    fn placed_offset_of(&self, scroll_state_slot: usize) -> CssPixelPoint {
        self.scroll_own_offsets
            .get(scroll_state_slot)
            .copied()
            .unwrap_or_default()
    }

    /// Whether the window still reaches far enough around what a viewport of this size shows with the scrollers at
    /// `scroll_own_offsets`.
    pub(crate) fn still_holds(
        &self,
        device_viewport_size: IntSize,
        device_pixels_per_css_pixel: f64,
        scroll_own_offsets: &[CssPixelPoint],
    ) -> bool {
        self.device_viewport_size == device_viewport_size
            && self.device_pixels_per_css_pixel == device_pixels_per_css_pixel
            && scroll_own_offsets.iter().enumerate().all(|(slot, now)| {
                let placed = self.placed_offset_of(slot);
                (now.x - placed.x).to_float().abs() <= self.slack.0
                    && (now.y - placed.y).to_float().abs() <= self.slack.1
            })
    }

    /// The window of a recording of a frame: the published recording's where that still holds, or one placed where
    /// the frame's scrollers are.
    pub(crate) fn of_frame(
        published: Option<&Arc<RecordingWindow>>,
        device_viewport_size: IntSize,
        device_pixels_per_css_pixel: f64,
        paint_state: &PublishedPaintState,
    ) -> Arc<Self> {
        let scroll_own_offsets = paint_state.scroll_own_offsets();
        if let Some(published) = published
            && published.still_holds(device_viewport_size, device_pixels_per_css_pixel, scroll_own_offsets)
        {
            return Arc::clone(published);
        }
        Arc::new(Self::placed_at(
            device_viewport_size,
            device_pixels_per_css_pixel,
            scroll_own_offsets,
        ))
    }
}

/// The spaces of a frame's visual context tree against the window of its recording.
pub(crate) struct WindowSpaces<'a> {
    window: Arc<RecordingWindow>,
    tree: &'a VisualContextTree,
    /// For each space asked about, how far the scrollers above it had moved what it holds when the window was
    /// placed, in device pixels, or none where something other than a scroller lies above it.
    scroll_displacements: FastMap<u32, Option<(f32, f32)>>,
}

impl<'a> WindowSpaces<'a> {
    pub(crate) fn new(window: Arc<RecordingWindow>, tree: &'a VisualContextTree) -> Self {
        Self {
            window,
            tree,
            scroll_displacements: FastMap::default(),
        }
    }

    fn scroll_displacement(&mut self, space: SpatialNodeIndex) -> Option<(f32, f32)> {
        if let Some(displacement) = self.scroll_displacements.get(&space.0) {
            return *displacement;
        }
        let displacement = self.sum_of_scroll_offsets_above(space);
        self.scroll_displacements.insert(space.0, displacement);
        displacement
    }

    fn sum_of_scroll_offsets_above(&self, space: SpatialNodeIndex) -> Option<(f32, f32)> {
        let scale = self.window.device_pixels_per_css_pixel as f32;
        let mut displacement = (0.0, 0.0);
        let mut node = space;
        while node != VISUAL_VIEWPORT_NODE_INDEX {
            let spatial_node = &self.tree.spatial_nodes[node.0 as usize];
            match &spatial_node.data {
                SpatialData::Scroll(scroll) => {
                    let offset = self.window.placed_offset_of(scroll.state_slot);
                    displacement.0 += offset.x.to_float() * scale;
                    displacement.1 += offset.y.to_float() * scale;
                }
                SpatialData::BackfaceVisibility(_) | SpatialData::Dead => {}
                SpatialData::Transform(_)
                | SpatialData::Sticky(_)
                | SpatialData::Perspective(_)
                | SpatialData::AnchorScrollShift(_) => return None,
            }
            node = spatial_node.parent;
        }
        Some(displacement)
    }

    // Where the bound is in the root's space with the scrollers where the window was placed.
    fn rect_against_window(&mut self, bound: InkBound) -> Option<FloatRect> {
        let (dx, dy) = self.scroll_displacement(bound.space)?;
        Some(bound.rect.translated(dx, dy))
    }

    /// Whether nothing inside the bound shows in the window. A bound that cannot be placed is not outside it.
    pub(crate) fn is_outside(&mut self, bound: InkBound) -> bool {
        // One device pixel of slack keeps rounding from leaving out what touches the window's edge.
        self.rect_against_window(bound)
            .is_some_and(|rect| rect.inflated(2.0, 2.0).intersected(self.window.rect).is_empty())
    }

    /// Whether what a command draws in `rect` under `context` shows in the window as the tree stands, with the
    /// scrollers where the window was placed and the clips of the context applied, or none where a node on the way
    /// to the root moves what it holds on its own or takes it out of the plane. A recording of a window that lacks
    /// the command was right to leave it out where this is false.
    pub(crate) fn shows_in_window(&self, rect: FloatRect, context: ContextRef) -> Option<bool> {
        let scale = self.window.device_pixels_per_css_pixel as f32;
        let mut scroll_offsets = vec![FloatPoint::default(); self.tree.spatial_nodes.len()];
        for (index, node) in self.tree.spatial_nodes.iter().enumerate() {
            if let SpatialData::Scroll(scroll) = &node.data {
                let offset = self.window.placed_offset_of(scroll.state_slot);
                scroll_offsets[index] = FloatPoint {
                    x: offset.x.to_float() * scale,
                    y: offset.y.to_float() * scale,
                };
            }
        }
        let spaces = WorldTransforms::new(self.tree, &scroll_offsets, None);
        let to_root = |space: SpatialNodeIndex| {
            let mut node = space;
            while node != VISUAL_VIEWPORT_NODE_INDEX {
                let spatial_node = &self.tree.spatial_nodes[node.0 as usize];
                if matches!(
                    spatial_node.data,
                    SpatialData::Sticky(_) | SpatialData::AnchorScrollShift(_) | SpatialData::Perspective(_)
                ) {
                    return None;
                }
                node = spatial_node.parent;
            }
            let matrix = spaces.draw_matrix(space);
            matrix.is_2d_affine().then(|| matrix.extract_2d_affine())
        };
        let mut shown = to_root(context.spatial)?.map_rect(rect);
        let mut clip = context.clip;
        while !clip.is_none() {
            let clip_node = &self.tree.clip_nodes[clip.0 as usize];
            let clip_rect = match &clip_node.data {
                ClipNodeData::Rect(data) if data.mode == ClipMode::Intersect => Some(data.rect),
                ClipNodeData::Path(path) => Some(path.bounding_rect.to_float()),
                _ => None,
            };
            if let Some(clip_rect) = clip_rect
                && let Some(clip_to_root) = to_root(clip_node.spatial)
            {
                shown = shown.intersected(clip_to_root.map_rect(clip_rect));
            }
            clip = clip_node.parent;
        }
        Some(!shown.intersected(self.window.rect).is_empty())
    }

    /// Whether everything inside the bound shows in the window.
    pub(crate) fn is_wholly_inside(&mut self, bound: InkBound) -> bool {
        self.rect_against_window(bound)
            .is_some_and(|rect| self.window.rect.contains_rect(rect.inflated(2.0, 2.0)))
    }

    /// One bound that holds both, or none where scrolling does not move both alike.
    pub(crate) fn united(&mut self, first: InkBound, second: InkBound) -> Option<InkBound> {
        if first.space != second.space {
            let first_displacement = self.scroll_displacement(first.space)?;
            let second_displacement = self.scroll_displacement(second.space)?;
            if self.nearest_scroller_above(first.space) != self.nearest_scroller_above(second.space) {
                return None;
            }
            debug_assert_eq!(first_displacement, second_displacement);
        }
        Some(InkBound {
            rect: first.rect.united(second.rect),
            space: first.space,
        })
    }

    fn nearest_scroller_above(&self, space: SpatialNodeIndex) -> SpatialNodeIndex {
        let mut node = space;
        while node != VISUAL_VIEWPORT_NODE_INDEX {
            let spatial_node = &self.tree.spatial_nodes[node.0 as usize];
            if matches!(spatial_node.data, SpatialData::Scroll(_)) {
                return node;
            }
            node = spatial_node.parent;
        }
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::css_pixels::CssPixels;
    use crate::layout::node_data::NodeSlotId;
    use crate::painting::visual_context::{
        BackfaceVisibilityData, ScrollData, StickyData, TransformData, TransformDataRole,
    };
    use libgfx_rust::FloatMatrix4x4;

    fn transform(matrix: FloatMatrix4x4) -> SpatialData {
        SpatialData::Transform(TransformData {
            matrix,
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
            bounds: FloatRect::default(),
        })
    }

    fn scroller(state_slot: usize) -> SpatialData {
        SpatialData::Scroll(ScrollData {
            state_slot,
            owner_paintable: NodeSlotId::INVALID,
        })
    }

    fn empty_tree() -> VisualContextTree {
        let SpatialData::Transform(identity) = transform(FloatMatrix4x4::identity()) else {
            unreachable!()
        };
        VisualContextTree::create(identity)
    }

    fn offsets(offsets: &[(i64, i64)]) -> Vec<CssPixelPoint> {
        offsets
            .iter()
            .map(|(x, y)| CssPixelPoint::new(CssPixels::from_integer(*x), CssPixels::from_integer(*y)))
            .collect()
    }

    const VIEWPORT: IntSize = IntSize {
        width: 800,
        height: 600,
    };

    // A window of an 800 by 600 viewport, which reaches 1200 pixels above and below it, placed with the scrollers
    // having moved what they hold by `placed_offsets`.
    fn window_at(placed_offsets: &[(i64, i64)]) -> Arc<RecordingWindow> {
        Arc::new(RecordingWindow::placed_at(
            VIEWPORT,
            1.0,
            &Arc::new(offsets(placed_offsets)),
        ))
    }

    fn bound(space: SpatialNodeIndex, y: f32) -> InkBound {
        InkBound {
            rect: FloatRect::new(0.0, y, 100.0, 100.0),
            space,
        }
    }

    #[test]
    fn a_window_holds_while_every_scroller_stays_within_half_the_margin() {
        let window = window_at(&[(0, -1000)]);
        assert!(window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -1000)])));
        assert!(window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -1600)])));
        assert!(window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -400)])));
        assert!(!window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -1601)])));
        assert!(!window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -399)])));
        // Another viewport or another scale is another window.
        let taller = IntSize {
            width: 800,
            height: 601,
        };
        assert!(!window.still_holds(taller, 1.0, &offsets(&[(0, -1000)])));
        assert!(!window.still_holds(VIEWPORT, 2.0, &offsets(&[(0, -1000)])));
        // A scroller that came after the window counts as placed where it does not scroll, and one that went
        // is not asked about.
        assert!(window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -1000), (0, -600)])));
        assert!(!window.still_holds(VIEWPORT, 1.0, &offsets(&[(0, -1000), (0, -601)])));
        assert!(window.still_holds(VIEWPORT, 1.0, &[]));
    }

    #[test]
    fn what_a_scroller_holds_is_placed_where_the_scroller_was_when_the_window_was_placed() {
        let mut tree = empty_tree();
        let page = tree.append_spatial(scroller(0), VISUAL_VIEWPORT_NODE_INDEX);
        // The page is scrolled down by 5000.
        let mut spaces = WindowSpaces::new(window_at(&[(0, -5000)]), &tree);

        // The window reaches from 3800 to 6800 down the page.
        assert!(spaces.is_outside(bound(page, 0.0)));
        assert!(spaces.is_outside(bound(page, 3600.0)));
        assert!(!spaces.is_outside(bound(page, 3750.0)));
        assert!(!spaces.is_outside(bound(page, 5000.0)));
        assert!(spaces.is_wholly_inside(bound(page, 5000.0)));
        assert!(!spaces.is_wholly_inside(bound(page, 3750.0)));
        assert!(!spaces.is_outside(bound(page, 6750.0)));
        assert!(spaces.is_outside(bound(page, 6900.0)));
        // What the page does not scroll stays where the viewport is.
        assert!(!spaces.is_outside(bound(VISUAL_VIEWPORT_NODE_INDEX, 0.0)));
        assert!(spaces.is_outside(bound(VISUAL_VIEWPORT_NODE_INDEX, 5000.0)));
    }

    #[test]
    fn what_a_scroller_in_a_scroller_holds_is_moved_by_both() {
        let mut tree = empty_tree();
        let page = tree.append_spatial(scroller(0), VISUAL_VIEWPORT_NODE_INDEX);
        let inner = tree.append_spatial(scroller(1), page);
        let behind_a_backface_check = tree.append_spatial(
            SpatialData::BackfaceVisibility(BackfaceVisibilityData {
                plane_root_index: inner,
                flattens_inherited_transform: false,
            }),
            inner,
        );
        // The page is scrolled down by 1000 and the scroller in it by 2000.
        let mut spaces = WindowSpaces::new(window_at(&[(0, -1000), (0, -2000)]), &tree);

        // The window reaches from 1800 to 4800 down what the inner scroller holds.
        assert!(spaces.is_outside(bound(inner, 1600.0)));
        assert!(!spaces.is_outside(bound(inner, 1750.0)));
        assert!(!spaces.is_outside(bound(inner, 4750.0)));
        assert!(spaces.is_outside(bound(inner, 4900.0)));
        assert!(!spaces.is_outside(bound(behind_a_backface_check, 3000.0)));

        // What one scroller moves is bounded together, and what two move is not.
        assert_eq!(
            spaces.united(bound(inner, 1000.0), bound(behind_a_backface_check, 1200.0)),
            Some(InkBound {
                rect: FloatRect::new(0.0, 1000.0, 100.0, 300.0),
                space: inner,
            })
        );
        assert_eq!(spaces.united(bound(inner, 0.0), bound(page, 0.0)), None);
    }

    #[test]
    fn nothing_below_a_node_that_moves_what_it_holds_without_a_recording_is_outside() {
        let mut tree = empty_tree();
        let page = tree.append_spatial(scroller(0), VISUAL_VIEWPORT_NODE_INDEX);
        let sticky = tree.append_spatial(
            SpatialData::Sticky(StickyData::unconstrained(page, None, 1, NodeSlotId::INVALID)),
            page,
        );
        let moved = tree.append_spatial(transform(libgfx_rust::translation_matrix(0.0, 50.0, 0.0)), page);
        let scrolled_below_a_transform = tree.append_spatial(scroller(2), moved);
        let mut spaces = WindowSpaces::new(window_at(&[(0, 0), (0, 0), (0, 0)]), &tree);

        for space in [sticky, moved, scrolled_below_a_transform] {
            assert!(!spaces.is_outside(bound(space, 100_000.0)));
            assert!(!spaces.is_wholly_inside(bound(space, 0.0)));
            assert_eq!(spaces.united(bound(space, 0.0), bound(page, 0.0)), None);
        }
        assert!(spaces.is_outside(bound(page, 100_000.0)));
    }
}
