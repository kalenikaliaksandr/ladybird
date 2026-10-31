/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! How much of each scroll node's content a display list records around what is visible, and
//! whether a recording still covers what is visible. WebContent decides what it records with these,
//! and the compositor decides when to ask for a new recording with the same rules.
//!
//! Rects are in device pixels of a scroll node's content at scroll offset zero. A scroll node only
//! offsets its content, so a nested scroll node placed exactly in its parent shares its parent's space.

use super::{ContextRef, SpatialData, SpatialNodeIndex, VISUAL_VIEWPORT_NODE_INDEX, VisualContextTree};
use crate::display_list::effect_geometry::nearest_spreading_effects;
use crate::host::FfiVisualAnimationTargetKind;
use libgfx_rust::{FloatPoint, IntRect, IntSize};

/// Interest rects snap outward to multiples of this, so that they cover whole raster tiles.
pub const RASTER_TILE_SIZE: i32 = 512;

/// A side of an interest rect this far from the origin is unbounded: nothing beyond it is left out.
pub const UNBOUNDED_INTEREST_EDGE: i32 = 1 << 24;

const _: () = assert!(UNBOUNDED_INTEREST_EDGE % RASTER_TILE_SIZE == 0);

/// The axes along which a scroll node's scroll range is not empty: its minimum scroll offset differs from its maximum.
/// Whether wheels scroll the node does not matter, since script and smooth scrolls move `overflow: hidden` content too.
/// On an axis it cannot scroll, the node always sits at scroll offset zero, so there its content space is its parent's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollableAxes {
    pub horizontal: bool,
    pub vertical: bool,
}

/// How far scrolling is expected to carry what is visible toward each side before a new recording
/// can arrive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeadPerSide {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CullSpace {
    /// Content outside every scroll node the compositor scrolls, such as fixed-position content.
    NeverCulled,
    /// Content of `scroll_node`. It is placed exactly when its own bounds say where it lands in
    /// the scroll node's content: no transform, sticky offset or anchor shift moves it, and no
    /// filter spreads it.
    ScrollNode {
        scroll_node: SpatialNodeIndex,
        placed_exactly: bool,
    },
}

/// `under_spreading_filter` is `effects_under_spreading_filters(tree)`, kept across contexts.
pub fn cull_space_of(
    tree: &VisualContextTree,
    context: ContextRef,
    has_compositor_scroll_node: impl Fn(SpatialNodeIndex) -> bool,
    under_spreading_filter: &[bool],
) -> CullSpace {
    let mut placed_exactly = context.effect.is_none() || !under_spreading_filter[context.effect.0 as usize];
    let mut index = context.spatial;
    loop {
        if index == VISUAL_VIEWPORT_NODE_INDEX {
            return CullSpace::NeverCulled;
        }
        let node = &tree.spatial_nodes[index.0 as usize];
        match node.data {
            SpatialData::Scroll(_) if has_compositor_scroll_node(index) => {
                return CullSpace::ScrollNode {
                    scroll_node: index,
                    placed_exactly,
                };
            }
            SpatialData::Scroll(_) => {}
            SpatialData::Sticky(_)
            | SpatialData::Transform(_)
            | SpatialData::Perspective(_)
            | SpatialData::BackfaceVisibility(_)
            | SpatialData::AnchorScrollShift(_) => placed_exactly = false,
            SpatialData::Dead => return CullSpace::NeverCulled,
        }
        index = node.parent;
    }
}

/// One flag per effect node: whether a filter on the node or an ancestor may spread content beyond its bounds, as
/// published or once a compositor filter animation changes it.
pub fn effects_under_spreading_filters(tree: &VisualContextTree) -> Vec<bool> {
    let mut under_spreading_filter: Vec<bool> = nearest_spreading_effects(tree)
        .iter()
        .map(|nearest| !nearest.is_none())
        .collect();
    let animated_filter_nodes = tree
        .visual_animations()
        .iter()
        .filter(|animation| animation.target_kind == FfiVisualAnimationTargetKind::Filter)
        .flat_map(|animation| animation.node_indices.iter());
    for &node_index in animated_filter_nodes {
        if let Some(flag) = under_spreading_filter.get_mut(node_index as usize) {
            *flag = true;
        }
    }
    tree.visit_effect_nodes_parents_first(|index| {
        let parent = tree.effect_nodes[index].parent;
        if !parent.is_none() && under_spreading_filter[parent.0 as usize] {
            under_spreading_filter[index] = true;
        }
    });
    under_spreading_filter
}

/// What a scroll node's interest rect grows around, in its content space at scroll offset zero: what its scrollport
/// shows at `scroll_offset` through the part of it that `parent_recorded` reaches, the rect a recording of the parent
/// records: snapped to the grid and saturated at the ends of the parent's scroll range. Both rects are in the parent's
/// content space, which a node placed exactly in its parent shares. It is empty when the parent's rect misses the node.
/// The root scroll node, and one placed inexactly in its parent, have no parent rect: nothing cuts the scrollport, and
/// every axis they cannot scroll is unbounded.
pub fn view_to_record_around(
    scrollport: IntRect,
    scroll_offset: FloatPoint,
    parent_recorded: Option<IntRect>,
    axes: ScrollableAxes,
) -> IntRect {
    let unbounded = i64::from(UNBOUNDED_INTEREST_EDGE);
    rect_from_axis_spans(|axis| {
        let scrollport = axis.span_of(scrollport);
        let reached = match parent_recorded {
            Some(parent_recorded) => scrollport.intersected(axis.span_of(parent_recorded)),
            None if axis.is_scrollable(axes) => scrollport,
            None => {
                return AxisSpan {
                    start: -unbounded,
                    end: unbounded,
                };
            }
        };
        reached.moved_enclosing(axis.coordinate_of(scroll_offset))
    })
}

/// `view`, from `view_to_record_around`, grown by one scrollport on each side of each axis the node can scroll, or
/// empty when `view` is.
pub fn default_interest_rect(view: IntRect, scrollport_size: IntSize, axes: ScrollableAxes) -> IntRect {
    if view.is_empty() {
        return view;
    }
    rect_from_axis_spans(|axis| {
        let span = axis.span_of(view);
        if axis.is_scrollable(axes) {
            span.inflated(axis.extent_of(scrollport_size))
        } else {
            span
        }
    })
}

pub fn snap_to_interest_grid(rect: IntRect) -> IntRect {
    if rect.is_empty() {
        return rect;
    }
    let tile = i64::from(RASTER_TILE_SIZE);
    rect_from_axis_spans(|axis| {
        let span = axis.span_of(rect);
        AxisSpan {
            start: span.start.div_euclid(tile) * tile,
            end: (span.end + tile - 1).div_euclid(tile) * tile,
        }
    })
}

/// On each axis the node can scroll, a side at or past the end of `content_area`, the content the
/// node's scrollport can show over its whole scroll range, becomes unbounded.
pub fn saturate_at_scroll_range(rect: IntRect, content_area: IntRect, axes: ScrollableAxes) -> IntRect {
    if rect.is_empty() {
        return rect;
    }
    let unbounded = i64::from(UNBOUNDED_INTEREST_EDGE);
    rect_from_axis_spans(|axis| {
        let span = axis.span_of(rect);
        if !axis.is_scrollable(axes) {
            return span;
        }
        let content = axis.span_of(content_area);
        let reaches_content_start = span.start <= content.start;
        let reaches_content_end = span.end >= content.end;
        AxisSpan {
            start: if reaches_content_start { -unbounded } else { span.start },
            end: if reaches_content_end { unbounded } else { span.end },
        }
    })
}

/// Whether `recorded` still holds `must_be_recorded` with room to scroll: on each axis the node can
/// scroll, every bounded side keeps half a scrollport plus that side's lead beyond it; on each axis
/// it cannot, `recorded` contains it.
pub fn interest_rect_covers(
    recorded: IntRect,
    must_be_recorded: IntRect,
    scrollport_size: IntSize,
    lead: LeadPerSide,
    axes: ScrollableAxes,
) -> bool {
    if must_be_recorded.is_empty() {
        return true;
    }
    let unbounded = i64::from(UNBOUNDED_INTEREST_EDGE);
    Axis::BOTH.into_iter().all(|axis| {
        let recorded = axis.span_of(recorded);
        let needed = axis.span_of(must_be_recorded);
        if !axis.is_scrollable(axes) {
            return recorded.contains(needed);
        }
        let scrollport_extent = axis.extent_of(scrollport_size);
        let keeps_margin = |margin: i64, lead: i64| 2 * margin >= scrollport_extent + 2 * lead.max(0);
        let (lead_toward_start, lead_toward_end) = axis.leads(lead);
        (recorded.start <= -unbounded || keeps_margin(needed.start - recorded.start, lead_toward_start))
            && (recorded.end >= unbounded || keeps_margin(recorded.end - needed.end, lead_toward_end))
    })
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

// Edges are kept in 64 bits so that rects from another process cannot overflow the arithmetic.
#[derive(Clone, Copy)]
struct AxisSpan {
    start: i64,
    end: i64,
}

impl Axis {
    const BOTH: [Self; 2] = [Self::Horizontal, Self::Vertical];

    fn span_of(self, rect: IntRect) -> AxisSpan {
        let (start, extent) = match self {
            Self::Horizontal => (rect.x, rect.width),
            Self::Vertical => (rect.y, rect.height),
        };
        AxisSpan {
            start: i64::from(start),
            end: i64::from(start) + i64::from(extent),
        }
    }

    fn coordinate_of(self, point: FloatPoint) -> f64 {
        f64::from(match self {
            Self::Horizontal => point.x,
            Self::Vertical => point.y,
        })
    }

    fn extent_of(self, size: IntSize) -> i64 {
        i64::from(match self {
            Self::Horizontal => size.width,
            Self::Vertical => size.height,
        })
    }

    fn is_scrollable(self, axes: ScrollableAxes) -> bool {
        match self {
            Self::Horizontal => axes.horizontal,
            Self::Vertical => axes.vertical,
        }
    }

    fn leads(self, lead: LeadPerSide) -> (i64, i64) {
        match self {
            Self::Horizontal => (i64::from(lead.left), i64::from(lead.right)),
            Self::Vertical => (i64::from(lead.top), i64::from(lead.bottom)),
        }
    }
}

impl AxisSpan {
    fn inflated(self, amount: i64) -> Self {
        Self {
            start: self.start - amount,
            end: self.end + amount,
        }
    }

    fn intersected(self, other: Self) -> Self {
        let start = self.start.max(other.start);
        Self {
            start,
            end: self.end.min(other.end).max(start),
        }
    }

    fn contains(self, other: Self) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    // An empty span stays empty.
    fn moved_enclosing(self, distance: f64) -> Self {
        let start = (self.start as f64 + distance).floor() as i64;
        let end = if self.end > self.start {
            (self.end as f64 + distance).ceil() as i64
        } else {
            start
        };
        Self { start, end }
    }
}

// Edges past the unbounded edge are clamped to it, which keeps them on the grid.
fn rect_from_axis_spans(span_on: impl Fn(Axis) -> AxisSpan) -> IntRect {
    let unbounded = i64::from(UNBOUNDED_INTEREST_EDGE);
    let clamped = |span: AxisSpan| {
        let start = span.start.clamp(-unbounded, unbounded);
        (start, span.end.clamp(-unbounded, unbounded) - start)
    };
    let (x, width) = clamped(span_on(Axis::Horizontal));
    let (y, height) = clamped(span_on(Axis::Vertical));
    IntRect::new(x as i32, y as i32, width as i32, height as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::commands::ClipNodeIndex;
    use crate::node_slot_id::NodeSlotId;
    use crate::visual_animation::VisualAnimation;
    use crate::visual_context::scroll_state::NO_SCROLL_STATE_SLOT;
    use crate::visual_context::{
        EffectNodeData, EffectNodeIndex, EffectsData, ScrollData, StickyData, TransformData, TransformDataRole,
    };
    use libgfx_rust::filter::Filter;
    use libgfx_rust::{CompositingAndBlendingOperator, FloatMatrix4x4};
    use std::sync::Arc;

    const UNBOUNDED: i32 = UNBOUNDED_INTEREST_EDGE;
    const VERTICAL: ScrollableAxes = ScrollableAxes {
        horizontal: false,
        vertical: true,
    };
    const HORIZONTAL: ScrollableAxes = ScrollableAxes {
        horizontal: true,
        vertical: false,
    };
    const NO_LEAD: LeadPerSide = LeadPerSide {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    fn size(width: i32, height: i32) -> IntSize {
        IntSize { width, height }
    }

    fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> IntRect {
        IntRect::new(left, top, right - left, bottom - top)
    }

    fn covers(recorded: IntRect, must_be_recorded: IntRect, scrollport_size: IntSize, axes: ScrollableAxes) -> bool {
        interest_rect_covers(recorded, must_be_recorded, scrollport_size, NO_LEAD, axes)
    }

    fn offset(x: f32, y: f32) -> FloatPoint {
        FloatPoint { x, y }
    }

    fn recorded_rect(view: IntRect, scrollport_size: IntSize, content_area: IntRect, axes: ScrollableAxes) -> IntRect {
        snap_to_interest_grid(saturate_at_scroll_range(
            default_interest_rect(view, scrollport_size, axes),
            content_area,
            axes,
        ))
    }

    #[test]
    fn a_scrolled_root_records_a_scrollport_above_and_below_what_is_visible() {
        let scrollport = IntRect::new(0, 0, 1000, 800);
        let scrollport_size = size(1000, 800);
        let content_area = IntRect::new(0, 0, 1000, 20000);
        let visible = IntRect::new(0, 5000, 1000, 800);

        let view = view_to_record_around(scrollport, offset(0.0, 5000.0), None, VERTICAL);
        assert_eq!(view, from_edges(-UNBOUNDED, 5000, UNBOUNDED, 5800));
        assert_eq!(
            default_interest_rect(view, scrollport_size, VERTICAL),
            from_edges(-UNBOUNDED, 4200, UNBOUNDED, 6600)
        );
        let recorded = recorded_rect(view, scrollport_size, content_area, VERTICAL);
        assert_eq!(recorded, from_edges(-UNBOUNDED, 4096, UNBOUNDED, 6656));
        assert!(covers(recorded, visible, scrollport_size, VERTICAL));

        let scrolled_a_little = IntRect::new(0, 5300, 1000, 800);
        assert!(covers(recorded, scrolled_a_little, scrollport_size, VERTICAL));
        let heading_down = LeadPerSide { bottom: 200, ..NO_LEAD };
        assert!(!interest_rect_covers(
            recorded,
            scrolled_a_little,
            scrollport_size,
            heading_down,
            VERTICAL
        ));
        let scrolled_near_the_edge = IntRect::new(0, 5700, 1000, 800);
        assert!(!covers(recorded, scrolled_near_the_edge, scrollport_size, VERTICAL));

        let view_at_the_top = view_to_record_around(scrollport, offset(0.0, 0.0), None, VERTICAL);
        let recorded_at_the_top = recorded_rect(view_at_the_top, scrollport_size, content_area, VERTICAL);
        assert_eq!(recorded_at_the_top, from_edges(-UNBOUNDED, -UNBOUNDED, UNBOUNDED, 2048));
        assert!(covers(recorded_at_the_top, scrollport, scrollport_size, VERTICAL));
    }

    #[test]
    fn a_right_to_left_scroller_with_negative_scroll_offsets_records_on_both_sides() {
        let scrollport_size = size(1000, 800);
        // Scroll offsets run from -3000 to 0, so the content starts 3000 px left of the scrollport.
        let content_area = IntRect::new(-3000, 0, 4000, 800);

        let scrolled_halfway = IntRect::new(-1500, 0, 1000, 800);
        let recorded = recorded_rect(scrolled_halfway, scrollport_size, content_area, HORIZONTAL);
        assert_eq!(recorded, from_edges(-2560, 0, 512, 1024));
        assert!(covers(recorded, scrolled_halfway, scrollport_size, HORIZONTAL));

        let at_the_start = IntRect::new(0, 0, 1000, 800);
        let recorded_at_the_start = recorded_rect(at_the_start, scrollport_size, content_area, HORIZONTAL);
        assert_eq!(recorded_at_the_start, from_edges(-1024, 0, UNBOUNDED, 1024));
        assert!(covers(recorded_at_the_start, at_the_start, scrollport_size, HORIZONTAL));

        let at_the_far_end = IntRect::new(-3000, 0, 1000, 800);
        let recorded_at_the_far_end = recorded_rect(at_the_far_end, scrollport_size, content_area, HORIZONTAL);
        assert_eq!(recorded_at_the_far_end, from_edges(-UNBOUNDED, 0, -512, 1024));
        assert!(covers(
            recorded_at_the_far_end,
            at_the_far_end,
            scrollport_size,
            HORIZONTAL
        ));
        assert!(!covers(
            recorded_at_the_far_end,
            scrolled_halfway,
            scrollport_size,
            HORIZONTAL
        ));
    }

    #[test]
    fn snapping_rounds_negative_edges_away_from_the_rect() {
        assert_eq!(
            snap_to_interest_grid(from_edges(-1, -513, 1, 1)),
            from_edges(-512, -1024, 512, 512)
        );
        assert_eq!(
            snap_to_interest_grid(from_edges(-600, -1100, -1, -513)),
            from_edges(-1024, -1536, 0, -512)
        );
        assert_eq!(
            snap_to_interest_grid(from_edges(-1024, 512, 0, 1536)),
            from_edges(-1024, 512, 0, 1536)
        );
        assert_eq!(
            snap_to_interest_grid(from_edges(-UNBOUNDED, -7, UNBOUNDED, 7)),
            from_edges(-UNBOUNDED, -512, UNBOUNDED, 512)
        );
        assert_eq!(
            snap_to_interest_grid(IntRect::new(-5, -5, 0, 10)),
            IntRect::new(-5, -5, 0, 10)
        );
    }

    #[test]
    fn only_axes_the_node_can_scroll_become_unbounded_at_the_end_of_the_content() {
        let content_area = IntRect::new(0, 0, 2000, 3000);
        let past_every_edge = from_edges(-100, -100, 2100, 3100);
        assert_eq!(
            saturate_at_scroll_range(past_every_edge, content_area, VERTICAL),
            from_edges(-100, -UNBOUNDED, 2100, UNBOUNDED)
        );
        assert_eq!(
            saturate_at_scroll_range(past_every_edge, content_area, HORIZONTAL),
            from_edges(-UNBOUNDED, -100, UNBOUNDED, 3100)
        );
        let inside = from_edges(100, 100, 1900, 2900);
        let both = ScrollableAxes {
            horizontal: true,
            vertical: true,
        };
        assert_eq!(saturate_at_scroll_range(inside, content_area, both), inside);
        let at_the_left_and_bottom_edges = from_edges(0, 100, 1900, 3000);
        assert_eq!(
            saturate_at_scroll_range(at_the_left_and_bottom_edges, content_area, both),
            from_edges(-UNBOUNDED, 100, 1900, UNBOUNDED)
        );
    }

    #[test]
    fn a_tall_scroller_that_only_scrolls_sideways_records_what_its_parent_records_of_it() {
        let root_scrollport = IntRect::new(0, 0, 1000, 800);
        let root_scrollport_size = size(1000, 800);
        let root_content_area = IntRect::new(0, 0, 1000, 40000);
        let root_visible = IntRect::new(0, 10000, 1000, 800);
        let root_aimed = default_interest_rect(
            view_to_record_around(root_scrollport, offset(0.0, 10000.0), None, VERTICAL),
            root_scrollport_size,
            VERTICAL,
        );
        assert_eq!(root_aimed, from_edges(-UNBOUNDED, 9200, UNBOUNDED, 11600));
        let root_recorded = snap_to_interest_grid(saturate_at_scroll_range(root_aimed, root_content_area, VERTICAL));
        assert_eq!(root_recorded, from_edges(-UNBOUNDED, 8704, UNBOUNDED, 11776));

        // An `overflow-x: auto` box 20000 px tall, with 5000 px of content across its 800 px scrollport, scrolled
        // 1500 px to the right.
        let scrollport = IntRect::new(100, 2000, 800, 20000);
        let scrollport_size = size(scrollport.width, scrollport.height);
        let scroll_offset = offset(1500.0, 0.0);
        let content_area = IntRect::new(100, 2000, 5000, 20000);
        let view = view_to_record_around(scrollport, scroll_offset, Some(root_recorded), HORIZONTAL);
        assert_eq!(view, from_edges(1600, 8704, 2400, 11776));
        let aimed = default_interest_rect(view, scrollport_size, HORIZONTAL);
        assert_eq!(aimed, from_edges(800, 8704, 3200, 11776));
        let recorded = snap_to_interest_grid(saturate_at_scroll_range(aimed, content_area, HORIZONTAL));
        assert_eq!(recorded, from_edges(512, 8704, 3584, 11776));
        let visible = from_edges(1600, 10000, 2400, 10800);
        assert!(covers(recorded, visible, scrollport_size, HORIZONTAL));
        assert!(covers(recorded, view, scrollport_size, HORIZONTAL));

        // Once the root scrolls past what it recorded, both the root and the box need new recordings.
        let root_scrolled_down = IntRect::new(0, 11000, 1000, 800);
        assert!(!covers(
            root_recorded,
            root_scrolled_down,
            root_scrollport_size,
            VERTICAL
        ));
        let visible_after_scrolling = from_edges(1600, 11000, 2400, 11800);
        assert!(!covers(recorded, visible_after_scrolling, scrollport_size, HORIZONTAL));
        assert!(covers(root_recorded, root_visible, root_scrollport_size, VERTICAL));

        // A box the root records none of records nothing.
        let root_recorded_far_below = from_edges(-UNBOUNDED, 29696, UNBOUNDED, 32768);
        let view_far_below =
            view_to_record_around(scrollport, scroll_offset, Some(root_recorded_far_below), HORIZONTAL);
        assert!(view_far_below.is_empty());
        assert!(default_interest_rect(view_far_below, scrollport_size, HORIZONTAL).is_empty());
        assert!(recorded_rect(view_far_below, scrollport_size, content_area, HORIZONTAL).is_empty());
    }

    #[test]
    fn a_scroller_below_the_fold_records_what_scrolling_its_parent_brings_into_view() {
        let root_scrollport = IntRect::new(0, 0, 1000, 800);
        let root_scrollport_size = size(1000, 800);
        let root_recorded = snap_to_interest_grid(default_interest_rect(
            view_to_record_around(root_scrollport, offset(0.0, 10000.0), None, VERTICAL),
            root_scrollport_size,
            VERTICAL,
        ));
        assert_eq!(root_recorded, from_edges(-UNBOUNDED, 8704, UNBOUNDED, 11776));

        // An `overflow-x: auto` box 300 px tall that starts 200 px below what the root shows, scrolled 700 px.
        let scrollport = IntRect::new(100, 11000, 800, 300);
        let scrollport_size = size(scrollport.width, scrollport.height);
        let scroll_offset = offset(700.0, 0.0);
        let content_area = IntRect::new(100, 11000, 3000, 300);
        let view = view_to_record_around(scrollport, scroll_offset, Some(root_recorded), HORIZONTAL);
        assert_eq!(view, from_edges(800, 11000, 1600, 11300));
        let recorded = recorded_rect(view, scrollport_size, content_area, HORIZONTAL);
        assert_eq!(recorded, from_edges(-UNBOUNDED, 10752, 2560, 11776));
        assert!(covers(recorded, view, scrollport_size, HORIZONTAL));

        // Scrolling the root 400 px down shows the top 200 px of the box, which is recorded.
        let visible_once_the_root_scrolls = from_edges(800, 11000, 1600, 11200);
        assert!(covers(
            recorded,
            visible_once_the_root_scrolls,
            scrollport_size,
            HORIZONTAL
        ));
    }

    #[test]
    fn a_scroller_without_a_parent_rect_records_every_axis_it_cannot_scroll_whole() {
        // The root scroll node, or a box placed under a transform, which its parent's rect cannot be mapped into.
        let scrollport = IntRect::new(100, 2000, 800, 600);
        let scrollport_size = size(scrollport.width, scrollport.height);
        let view = view_to_record_around(scrollport, offset(300.5, 0.0), None, HORIZONTAL);
        assert_eq!(view, from_edges(400, -UNBOUNDED, 1201, UNBOUNDED));
        assert_eq!(
            default_interest_rect(view, scrollport_size, HORIZONTAL),
            from_edges(-400, -UNBOUNDED, 2001, UNBOUNDED)
        );

        let placed_exactly = view_to_record_around(
            scrollport,
            offset(300.5, 0.0),
            Some(from_edges(-UNBOUNDED, 2300, UNBOUNDED, 4000)),
            HORIZONTAL,
        );
        assert_eq!(placed_exactly, from_edges(400, 2300, 1201, 2600));
    }

    #[test]
    fn what_a_stationary_node_records_by_default_covers_what_it_shows() {
        let scrollport_sizes = [
            size(1, 1),
            size(300, 200),
            size(511, 513),
            size(1000, 800),
            size(1920, 1080),
        ];
        let every_axes = [
            ScrollableAxes {
                horizontal: false,
                vertical: false,
            },
            HORIZONTAL,
            VERTICAL,
            ScrollableAxes {
                horizontal: true,
                vertical: true,
            },
        ];
        let scrollport_origins = [(0, 0), (-700, 37), (1234, -4321)];
        let scroll_ranges = [(0, 0), (0, 5000), (-3000, 0), (-1025, 1023)];
        for scrollport_size in scrollport_sizes {
            for axes in every_axes {
                for (origin_x, origin_y) in scrollport_origins {
                    for (minimum_offset, maximum_offset) in scroll_ranges {
                        let scrollport =
                            IntRect::new(origin_x, origin_y, scrollport_size.width, scrollport_size.height);
                        let range_on = |scrollable: bool| {
                            if scrollable {
                                (minimum_offset, maximum_offset)
                            } else {
                                (0, 0)
                            }
                        };
                        let (minimum_x, maximum_x) = range_on(axes.horizontal);
                        let (minimum_y, maximum_y) = range_on(axes.vertical);
                        let content_area = from_edges(
                            scrollport.x + minimum_x,
                            scrollport.y + minimum_y,
                            scrollport.right() + maximum_x,
                            scrollport.bottom() + maximum_y,
                        );
                        for step in 0..=16 {
                            let offset_x = minimum_x + (maximum_x - minimum_x) * step / 16;
                            let offset_y = minimum_y + (maximum_y - minimum_y) * step / 16;
                            let visible = IntRect::new(
                                scrollport.x + offset_x,
                                scrollport.y + offset_y,
                                scrollport.width,
                                scrollport.height,
                            );
                            let view = view_to_record_around(
                                scrollport,
                                offset(offset_x as f32, offset_y as f32),
                                Some(scrollport),
                                axes,
                            );
                            assert_eq!(view, visible);
                            let aimed = default_interest_rect(view, scrollport_size, axes);
                            for recorded in [
                                snap_to_interest_grid(saturate_at_scroll_range(aimed, content_area, axes)),
                                saturate_at_scroll_range(snap_to_interest_grid(aimed), content_area, axes),
                            ] {
                                assert!(
                                    covers(recorded, visible, scrollport_size, axes),
                                    "{recorded:?} does not cover {visible:?} ({scrollport_size:?}, {axes:?})"
                                );
                            }
                        }
                    }
                }
            }
        }
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

    fn in_space(spatial: SpatialNodeIndex, effect: EffectNodeIndex) -> ContextRef {
        ContextRef {
            spatial,
            clip: ClipNodeIndex::NONE,
            effect,
        }
    }

    #[test]
    fn content_is_culled_in_the_nearest_scroll_node_the_compositor_scrolls() {
        let mut tree = VisualContextTree::create(identity_transform());
        let root = tree.append_spatial(scroll(), VISUAL_VIEWPORT_NODE_INDEX);
        let unscrollable = tree.append_spatial(scroll(), root);
        let nested = tree.append_spatial(scroll(), unscrollable);
        let transformed = tree.append_spatial(SpatialData::Transform(identity_transform()), root);
        let sticky = tree.append_spatial(
            SpatialData::Sticky(StickyData::unconstrained(
                root,
                None,
                NO_SCROLL_STATE_SLOT,
                NodeSlotId::INVALID,
                root,
            )),
            nested,
        );
        let fixed = tree.append_spatial(SpatialData::Transform(identity_transform()), VISUAL_VIEWPORT_NODE_INDEX);
        let effect_with = |tree: &mut VisualContextTree, filter: Option<Filter>, parent: EffectNodeIndex| {
            tree.append_effect(
                EffectNodeData::Effects(EffectsData {
                    isolation: false,
                    opacity: 0.5,
                    blend_mode: CompositingAndBlendingOperator::Normal,
                    filter: filter.map(|filter| Arc::new(filter.serialize())),
                    backdrop_filter: None,
                }),
                parent,
                root,
                ClipNodeIndex::NONE,
            )
        };
        let blur = effect_with(&mut tree, Some(Filter::blur(4.0, 4.0, None)), EffectNodeIndex::NONE);
        let under_blur = effect_with(&mut tree, None, blur);
        let plain = effect_with(&mut tree, None, EffectNodeIndex::NONE);
        let animated_filter = effect_with(&mut tree, None, EffectNodeIndex::NONE);
        let under_animated_filter = effect_with(&mut tree, None, animated_filter);
        tree.set_visual_animations(vec![VisualAnimation {
            target_kind: FfiVisualAnimationTargetKind::Filter,
            node_indices: vec![animated_filter.0],
            ..VisualAnimation::default()
        }]);
        let under_spreading_filter = effects_under_spreading_filters(&tree);
        let has_compositor_scroll_node = |index: SpatialNodeIndex| index != unscrollable;
        let cull_space = |spatial, effect| {
            cull_space_of(
                &tree,
                in_space(spatial, effect),
                has_compositor_scroll_node,
                &under_spreading_filter,
            )
        };
        let exactly_in = |scroll_node| CullSpace::ScrollNode {
            scroll_node,
            placed_exactly: true,
        };
        let roughly_in = |scroll_node| CullSpace::ScrollNode {
            scroll_node,
            placed_exactly: false,
        };

        assert_eq!(cull_space(root, EffectNodeIndex::NONE), exactly_in(root));
        assert_eq!(cull_space(unscrollable, EffectNodeIndex::NONE), exactly_in(root));
        assert_eq!(cull_space(nested, plain), exactly_in(nested));
        assert_eq!(cull_space(transformed, EffectNodeIndex::NONE), roughly_in(root));
        assert_eq!(cull_space(sticky, EffectNodeIndex::NONE), roughly_in(nested));
        assert_eq!(cull_space(root, under_blur), roughly_in(root));
        assert_eq!(cull_space(root, animated_filter), roughly_in(root));
        assert_eq!(cull_space(root, under_animated_filter), roughly_in(root));
        assert_eq!(cull_space(fixed, EffectNodeIndex::NONE), CullSpace::NeverCulled);
        assert_eq!(cull_space(fixed, under_blur), CullSpace::NeverCulled);
        assert_eq!(
            cull_space(VISUAL_VIEWPORT_NODE_INDEX, EffectNodeIndex::NONE),
            CullSpace::NeverCulled
        );
    }
}
