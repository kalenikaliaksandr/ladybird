/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Classic scrollbars take room in layout. A scroll container keeps a gutter for each scrollbar it
//! shows, between its border and its padding. Overlay scrollbars take none: they are laid out as
//! scrollbars with no thickness, which keep no gutter and never show for overflow, and painting
//! decides where they show.
//! https://drafts.csswg.org/css-overflow-3/#scrollbar-layout

use super::node_data::NodeKind;
use super::*;
use crate::painting::paint_read::{GeometryRead, PaintRead};

/// How thick a scrollbar is, and a thin one, in CSS pixels at the page's zoom. Overlay scrollbars
/// have no thickness here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollbarThicknesses {
    pub(crate) auto: CssPixels,
    pub(crate) thin: CssPixels,
}

/// The scrollbars a scroll container shows on its axes with `overflow: auto`. The last layout of
/// the box decides them, from whether its content overflowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutoScrollbars {
    pub(crate) horizontal: bool,
    pub(crate) vertical: bool,
}

/// The room a box keeps for scrollbars between its border and its padding, and the scrollbars it
/// shows in that room. A stable gutter can be there without a scrollbar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollbarGutters {
    pub(crate) left: CssPixels,
    pub(crate) right: CssPixels,
    pub(crate) top: CssPixels,
    pub(crate) bottom: CssPixels,
    pub(crate) has_vertical_scrollbar: bool,
    pub(crate) has_horizontal_scrollbar: bool,
}

impl ScrollbarGutters {
    pub(crate) fn horizontal_sum(&self) -> CssPixels {
        self.left + self.right
    }

    pub(crate) fn vertical_sum(&self) -> CssPixels {
        self.top + self.bottom
    }
}

/// Whether the vertical scrollbar of a box with `style` goes on its left edge. That is the
/// inline-start edge for right-to-left horizontal text, and the block-start edge of a box whose
/// blocks flow from right to left.
pub(crate) fn vertical_scrollbar_is_on_left(style: ComputedValuesView<'_>) -> bool {
    let writing_mode = style.writing_mode();
    (writing_mode == writing_mode::HORIZONTAL_TB && style.direction() == direction::RTL)
        || writing_mode == writing_mode::VERTICAL_RL
        || writing_mode == writing_mode::SIDEWAYS_RL
}

/// Whether a box of `kind` with `style` keeps gutters for its scrollbars when it is a scroll
/// container. Tables, the boxes inside them other than cells, replaced boxes and inline boxes
/// never do.
fn kind_keeps_scrollbar_gutters(kind: NodeKind, style: ComputedValuesView<'_>) -> bool {
    if kind == NodeKind::Viewport {
        return true;
    }
    if !matches!(
        kind,
        NodeKind::BlockContainer
            | NodeKind::Box
            | NodeKind::FieldSetBox
            | NodeKind::LegendBox
            | NodeKind::ListItemBox
            | NodeKind::TextAreaBox
    ) {
        return false;
    }
    let display = style.display();
    !display.is_table_inside() && (!display.is_internal_table() || display.is_table_cell())
}

/// The gutters of a box of `kind` with `style`, when it shows the `overflow: auto` scrollbars in
/// `auto_scrollbars`. The viewport's style holds the overflow and scrollbar properties the root
/// element propagated to it.
pub(crate) fn scrollbar_gutters(
    kind: NodeKind,
    style: ComputedValuesView<'_>,
    auto_scrollbars: AutoScrollbars,
    thicknesses: ScrollbarThicknesses,
) -> ScrollbarGutters {
    let mut gutters = ScrollbarGutters::default();
    if !kind_keeps_scrollbar_gutters(kind, style) {
        return gutters;
    }
    let misc = style.misc_reset();
    let thickness = match misc.scrollbar_width {
        scrollbar_width::NONE => return gutters,
        scrollbar_width::THIN => thicknesses.thin,
        _ => thicknesses.auto,
    };
    if thickness <= CssPixels::default() {
        return gutters;
    }

    let shows_scrollbar = |overflow_value: u8, shown_when_auto: bool| match overflow_value {
        overflow::SCROLL => true,
        overflow::AUTO => shown_when_auto,
        _ => false,
    };
    gutters.has_vertical_scrollbar = shows_scrollbar(style.overflow_y(), auto_scrollbars.vertical);
    gutters.has_horizontal_scrollbar = shows_scrollbar(style.overflow_x(), auto_scrollbars.horizontal);

    // https://drafts.csswg.org/css-overflow-3/#scrollbar-gutter-property
    // A stable gutter is kept for the scrollbar at the inline-start or inline-end edge when the
    // box is a scroll container in that axis, whether the scrollbar is shown or not.
    let keeps_stable_gutter = |overflow_value: u8| {
        misc.scrollbar_gutter != scrollbar_gutter::AUTO
            && matches!(overflow_value, overflow::AUTO | overflow::HIDDEN | overflow::SCROLL)
    };
    let both_edges = misc.scrollbar_gutter == scrollbar_gutter::BOTH_EDGES;
    let horizontal_writing_mode = style.writing_mode() == writing_mode::HORIZONTAL_TB;

    let stable_vertical_gutter = horizontal_writing_mode && keeps_stable_gutter(style.overflow_y());
    if gutters.has_vertical_scrollbar || stable_vertical_gutter {
        // NB: The viewport keeps its vertical scrollbar on the right in every direction, so that
        //     no document coordinate moves when the scrollbar comes and goes.
        if kind != NodeKind::Viewport && vertical_scrollbar_is_on_left(style) {
            gutters.left = thickness;
        } else {
            gutters.right = thickness;
        }
        if stable_vertical_gutter && both_edges {
            gutters.left = thickness;
            gutters.right = thickness;
        }
    }

    let stable_horizontal_gutter = !horizontal_writing_mode && keeps_stable_gutter(style.overflow_x());
    if gutters.has_horizontal_scrollbar || stable_horizontal_gutter {
        gutters.bottom = thickness;
        if stable_horizontal_gutter && both_edges {
            gutters.top = thickness;
        }
    }
    gutters
}

/// The content size of a query container with `style` and `content_size`, as container queries and
/// container-relative lengths read it: with the room of its `overflow: auto` scrollbars given back.
/// Those scrollbars come and go with the container's content, so a query that read them could undo
/// itself. A scrollbar that `overflow: scroll` or a stable gutter keeps is left out of the size.
pub(crate) fn content_size_for_container_queries(
    style: ComputedValuesView<'_>,
    gutters: ScrollbarGutters,
    content_size: FfiCssPixelSize,
) -> FfiCssPixelSize {
    let horizontal_writing_mode = style.writing_mode() == writing_mode::HORIZONTAL_TB;
    let stable = style.misc_reset().scrollbar_gutter != scrollbar_gutter::AUTO;
    let mut size = content_size;
    if style.overflow_y() == overflow::AUTO && gutters.has_vertical_scrollbar && !(stable && horizontal_writing_mode) {
        size.width += gutters.horizontal_sum();
    }
    if style.overflow_x() == overflow::AUTO && gutters.has_horizontal_scrollbar && !(stable && !horizontal_writing_mode)
    {
        size.height += gutters.vertical_sum();
    }
    size
}

impl LayoutPass<'_> {
    /// The scrollbar gutters of `node` in this pass. They follow from its style and from the
    /// `overflow: auto` scrollbars it shows, so a box that has no record yet has them too.
    pub(crate) fn scrollbar_gutters(&self, node: Node) -> ScrollbarGutters {
        let kind = self.node_data(node).kind.get();
        let Some(style) = self.computed_values_view_if_styled(node) else {
            return ScrollbarGutters::default();
        };
        if !node_facts::kind_and_style_make_scroll_container(kind, Some(style)) {
            return ScrollbarGutters::default();
        }
        scrollbar_gutters(
            kind,
            style,
            self.arena().row_auto_scrollbars(node),
            self.scrollbar_thicknesses,
        )
    }
}

impl LayoutNodeArena {
    /// How thick scrollbars were in the last layout.
    pub(crate) fn noted_scrollbar_thicknesses(&self) -> ScrollbarThicknesses {
        self.scrollbar_thicknesses.get().unwrap_or_default()
    }

    /// Takes in how thick scrollbars are for the next layout. Where that changed, as it does with the
    /// page's zoom and scrollbar style, every scroll container is laid out again, since what was laid
    /// out and measured around it kept the old room. That includes the ones that kept no room, as
    /// none does with overlay scrollbars.
    pub(crate) fn note_scrollbar_thicknesses(&self, thicknesses: ScrollbarThicknesses) {
        let Some(previous) = self.scrollbar_thicknesses.replace(Some(thicknesses)) else {
            return;
        };
        if previous == thicknesses {
            return;
        }
        // Scrollbars with no thickness never show for overflow, so the ones that showed are forgotten
        // rather than kept for when scrollbars take room again.
        if thicknesses == ScrollbarThicknesses::default() {
            self.forget_auto_scrollbars();
        }
        let layout_root = self.layout_root();
        if layout_root.is_invalid() {
            return;
        }
        let mut scroll_containers = Vec::new();
        self.for_each_node_in_layout_subtree_in_pre_order(layout_root, |slot| {
            if let Some(kind) = self.node_kind_if_live(slot)
                && node_facts::kind_and_style_make_scroll_container(kind, self.node_style_if_live(slot))
            {
                scroll_containers.push(slot);
            }
        });
        for slot in scroll_containers {
            self.set_needs_layout_update(slot, true);
        }
    }

    /// Decides again which `overflow: auto` scrollbars the scroll containers whose overflow the last
    /// commits may have changed show, from what overflows them now. Each box whose scrollbars change
    /// is marked for layout. Answers whether any changed.
    pub(crate) fn update_auto_scrollbars_from_overflow(&self) -> bool {
        let mut changed = false;
        for slot in crate::painting::scrollable_overflow::scroll_containers_awaiting_overflow_settlement(self) {
            changed |= self.update_auto_scrollbars_of(slot);
        }
        changed
    }

    fn update_auto_scrollbars_of(&self, slot: NodeSlotId) -> bool {
        let (Some(kind), Some(style)) = (self.node_kind_if_live(slot), self.node_style_if_live(slot)) else {
            return false;
        };
        // Only an axis whose scrollbar would take room follows the overflow.
        let possible = scrollbar_gutters(
            kind,
            style,
            AutoScrollbars {
                horizontal: true,
                vertical: true,
            },
            self.noted_scrollbar_thicknesses(),
        );
        let horizontal_follows_overflow = style.overflow_x() == overflow::AUTO && possible.has_horizontal_scrollbar;
        let vertical_follows_overflow = style.overflow_y() == overflow::AUTO && possible.has_vertical_scrollbar;
        let shown = self.row_auto_scrollbars(slot);
        if !horizontal_follows_overflow && !vertical_follows_overflow && shown == AutoScrollbars::default() {
            return false;
        }

        self.ensure_scrollable_overflow(slot);
        let rows = self.paintable_rows();
        let Some(overflow_rect) = crate::painting::paintable_geometry::scrollable_overflow_rect(&rows, slot) else {
            return false;
        };
        let scrollport = crate::painting::paintable_geometry::absolute_padding_box_rect(&rows, slot);
        let gutters = crate::painting::paintable_geometry::committed_scrollbar_gutters(&rows, slot);
        let overflows = |content: CssPixels, room: CssPixels| content.round() > room.round();
        let mut wanted = AutoScrollbars {
            horizontal: horizontal_follows_overflow && overflows(overflow_rect.width, scrollport.width),
            vertical: vertical_follows_overflow && overflows(overflow_rect.height, scrollport.height),
        };
        // Content that fits once the `overflow: auto` scrollbars are gone shows none of them. A box
        // asks this only when it shows both, since then each one can be what makes the other
        // necessary, but the viewport asks it always, since its content often fits it exactly.
        if kind == NodeKind::Viewport || (shown.horizontal && shown.vertical) {
            let stays = scrollbar_gutters(
                kind,
                style,
                AutoScrollbars::default(),
                self.noted_scrollbar_thicknesses(),
            );
            let room_width = scrollport.width + gutters.horizontal_sum() - stays.horizontal_sum();
            let room_height = scrollport.height + gutters.vertical_sum() - stays.vertical_sum();
            if !overflows(overflow_rect.width, room_width) && !overflows(overflow_rect.height, room_height) {
                wanted = AutoScrollbars::default();
            }
        }
        // A scrollbar that appeared during the layout update stays until it ends. Otherwise content
        // that overflows only without the scrollbar would make it come and go without end.
        let frozen = self.auto_scrollbars_frozen_for_update(slot);
        wanted.horizontal |= frozen.horizontal && horizontal_follows_overflow;
        wanted.vertical |= frozen.vertical && vertical_follows_overflow;
        if wanted == shown {
            return false;
        }
        self.freeze_auto_scrollbars_for_update(
            slot,
            AutoScrollbars {
                horizontal: wanted.horizontal && !shown.horizontal,
                vertical: wanted.vertical && !shown.vertical,
            },
        );
        self.set_row_auto_scrollbars(slot, wanted);
        // A box that is laid out without its ancestors, or that keeps its border box, lays out only
        // its own content again.
        let lays_out_alone =
            self.node_is_partial_relayout_boundary(slot) || self.node_keeps_border_box_for_scrollbars(slot);
        self.set_needs_layout_update(slot, !lays_out_alone);
        true
    }
}
