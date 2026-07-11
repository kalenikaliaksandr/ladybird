/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Position.h>
#include <LibWeb/HTML/LocalNavigable.h>
#include <LibWeb/Layout/Node.h>
#include <LibWeb/Painting/DisplayListRecorder.h>
#include <LibWeb/Painting/DisplayListRecordingContext.h>
#include <LibWeb/Painting/HitTestDisplayList.h>
#include <LibWeb/Painting/InlinePaintable.h>
#include <LibWeb/Painting/StackingContext.h>

namespace Web::Painting {

NonnullRefPtr<InlinePaintable> InlinePaintable::create(Layout::NodeWithStyleAndBoxModelMetrics const& layout_node)
{
    return adopt_ref(*new InlinePaintable(layout_node));
}

InlinePaintable::InlinePaintable(Layout::NodeWithStyleAndBoxModelMetrics const& layout_node)
    : Paintable(layout_node)
{
}

InlinePaintable::~InlinePaintable() = default;

void InlinePaintable::reset_for_relayout()
{
    Paintable::reset_for_relayout();
    m_piece_indices.clear();
    m_local_padding_box_union = {};
    m_local_border_box_union = {};
}

PaintableWithLines const* InlinePaintable::inline_root() const
{
    return as_if<PaintableWithLines>(containing_block().ptr());
}

CSSPixelRect InlinePaintable::absolute_piece_border_box_rect(InlineBoxPiece const& piece) const
{
    auto rect = piece.border_box_rect;
    if (auto const* root = inline_root())
        rect.translate_by(root->absolute_position());
    return rect;
}

CSSPixelRect InlinePaintable::piece_padding_box_rect(InlineBoxPiece const& piece, CSSPixelRect const& border_box_rect) const
{
    return piece.shrunken_by_included_edges(border_box_rect, box_model().border);
}

CSSPixelRect InlinePaintable::piece_content_box_rect(InlineBoxPiece const& piece, CSSPixelRect const& border_box_rect) const
{
    return piece.shrunken_by_included_edges(piece_padding_box_rect(piece, border_box_rect), box_model().padding);
}

BorderRadiiData InlinePaintable::piece_border_radii_data(InlineBoxPiece const& piece) const
{
    using Edge = InlineBoxPiece::Edge;
    auto const& computed_values = this->computed_values();
    if (!computed_values.has_noninitial_border_radii())
        return {};

    // Corners adjacent to a cut edge (box-decoration-break: slice) have no radii.
    auto top_cut = !piece.has_edge(Edge::Top);
    auto bottom_cut = !piece.has_edge(Edge::Bottom);
    auto left_cut = !piece.has_edge(Edge::Left);
    auto right_cut = !piece.has_edge(Edge::Right);

    CSSPixelRect const border_rect { 0, 0, piece.border_box_rect.width(), piece.border_box_rect.height() };
    auto top_left = top_cut || left_cut ? CSS::BorderRadiusData {} : computed_values.border_top_left_radius();
    auto top_right = top_cut || right_cut ? CSS::BorderRadiusData {} : computed_values.border_top_right_radius();
    auto bottom_right = bottom_cut || right_cut ? CSS::BorderRadiusData {} : computed_values.border_bottom_right_radius();
    auto bottom_left = bottom_cut || left_cut ? CSS::BorderRadiusData {} : computed_values.border_bottom_left_radius();
    return normalize_border_radii_data(border_rect, border_rect, top_left, top_right, bottom_right, bottom_left);
}

bool InlinePaintable::has_content_pieces() const
{
    auto const* root = inline_root();
    if (!root)
        return false;
    auto const& pieces = root->inline_box_pieces();
    for (auto piece_index : m_piece_indices) {
        if (!pieces[piece_index].is_placeholder)
            return true;
    }
    return false;
}

CSSPixelPoint InlinePaintable::box_type_agnostic_position() const
{
    // Mouse event offsets and similar box-type-agnostic positions are measured from the
    // first piece's content origin, matching the geometry the box had when it was a single
    // line piece.
    auto const* root = inline_root();
    if (!root || m_piece_indices.is_empty())
        return absolute_position();
    auto const& piece = root->inline_box_pieces()[m_piece_indices.first()];
    auto rect = piece.is_placeholder ? piece.border_box_rect : piece_content_box_rect(piece, piece.border_box_rect);
    return rect.location().translated(root->absolute_position());
}

CSSPixelRect InlinePaintable::compute_absolute_padding_box_rect() const
{
    // An inline box's padding box is the union of its per-line pieces' padding boxes.
    return m_local_padding_box_union.translated(absolute_rect().location());
}

CSSPixelRect InlinePaintable::compute_absolute_border_box_rect() const
{
    // An inline box's border box is the union of its per-line pieces' border boxes.
    return m_local_border_box_union.translated(absolute_rect().location());
}

void InlinePaintable::paint(DisplayListRecordingContext& context, PaintPhase phase) const
{
    if (!is_visible())
        return;

    auto const* root = inline_root();
    if (!root)
        return;

    auto root_position = root->absolute_position();

    if (phase == PaintPhase::Background) {
        auto has_borders = has_css_borders();
        for_each_piece([&](auto const& piece) {
            if (piece.is_placeholder)
                return;
            auto border_box_rect = piece.border_box_rect.translated(root_position);
            auto padding_box_rect = piece_padding_box_rect(piece, border_box_rect);
            auto border_radii = piece_border_radii_data(piece);
            paint_background_within(context, has_borders ? border_box_rect : padding_box_rect, border_radii);
            paint_box_shadow(context, border_box_rect, padding_box_rect, border_radii);
        });
    }

    if (phase == PaintPhase::Border) {
        using Edge = InlineBoxPiece::Edge;
        auto const& computed_values = this->computed_values();
        auto const& border = box_model().border;
        for_each_piece([&](auto const& piece) {
            if (piece.is_placeholder)
                return;
            auto borders_data = BordersData {
                .top = border.top == 0 || !piece.has_edge(Edge::Top) ? CSS::BorderData() : computed_values.border_top(),
                .right = border.right == 0 || !piece.has_edge(Edge::Right) ? CSS::BorderData() : computed_values.border_right(),
                .bottom = border.bottom == 0 || !piece.has_edge(Edge::Bottom) ? CSS::BorderData() : computed_values.border_bottom(),
                .left = border.left == 0 || !piece.has_edge(Edge::Left) ? CSS::BorderData() : computed_values.border_left(),
            };
            paint_border(context, piece.border_box_rect.translated(root_position), borders_data, piece_border_radii_data(piece));
        });
    }

    if (phase == PaintPhase::Outline) {
        for_each_piece([&](auto const& piece) {
            if (piece.is_placeholder)
                return;
            paint_outline(context, piece.border_box_rect.translated(root_position), piece_border_radii_data(piece));
        });
    }

    if (phase == PaintPhase::Foreground) {
        // A self-painting inline box records its fragments' foreground itself so glyphs land
        // inside its stacking context / positioned paint order; otherwise the containing
        // block has already painted them.
        if (is_self_painting()) {
            root->paint_fragments_foreground(context, root->fragment_ownership_filter(this));
            if (document().cursor_position())
                root->paint_cursor(context, this);
        }
        if (document().cursor_position())
            paint_empty_editable_cursor(context);
    }
}

void InlinePaintable::paint_empty_editable_cursor(DisplayListRecordingContext& context) const
{
    // Editable inline elements without any content still draw a caret.
    if (has_content_pieces())
        return;

    if (!document().cursor_blink_state() || !document().navigable()->is_focused())
        return;

    auto cursor_position = document().cursor_position();
    VERIFY(cursor_position);

    auto const* dom_node = layout_node().dom_node();
    if (!dom_node || cursor_position->node() != dom_node)
        return;
    if (!dom_node->is_editable_or_editing_host())
        return;

    auto caret_color = computed_values().caret_color();
    if (caret_color.alpha() == 0)
        return;

    auto position = box_type_agnostic_position();
    CSSPixelRect cursor_rect { position.x(), position.y(), 1, computed_values().line_height() };
    context.display_list_recorder().fill_rect(context.rounded_device_rect(cursor_rect).to_type<int>(), caret_color);
}

void InlinePaintable::record_hit_test_items(DisplayListRecordingContext& context, PaintPhase phase) const
{
    auto* hit_test_display_list = context.hit_test_display_list();
    if (!hit_test_display_list)
        return;

    if (!is_visible() || !visible_for_hit_testing())
        return;

    if (layout_node().is_anonymous() && !layout_node().is_generated_for_pseudo_element())
        return;

    auto append_piece_boxes = [&] {
        for_each_piece([&](auto const& piece) {
            if (piece.is_placeholder)
                return;
            hit_test_display_list->append_box(*this, const_cast<InlinePaintable&>(*this), absolute_piece_border_box_rect(piece), accumulated_visual_context_index(), piece_border_radii_data(piece));
        });
    };

    if (phase == PaintPhase::Background) {
        append_piece_boxes();

        if (layout_node().dom_node()
            && layout_node().dom_node()->is_editable_or_editing_host()
            && !has_content_pieces()) {
            hit_test_display_list->append_empty_editable(*this, absolute_border_box_rect(), accumulated_visual_context_index());
        }
        return;
    }

    if (phase != PaintPhase::Foreground)
        return;

    if (is_self_painting()) {
        auto const* root = inline_root();
        if (root) {
            auto filter = root->fragment_ownership_filter(this);
            auto const& fragments = root->fragments();
            for (auto const& range : filter.included) {
                for (u32 index = range.begin; index < range.end; ++index) {
                    if (filter.is_excluded(index))
                        continue;
                    auto const& fragment = fragments[index];
                    if (fragment.is_block_level_box())
                        continue;
                    hit_test_display_list->append_text_fragment(fragment, accumulated_visual_context_for_descendants_index());
                }
            }
        }

        // Re-record the piece boxes so their z-order matches this box's paint order.
        if (stacking_context())
            append_piece_boxes();
    }
}

void InlinePaintable::set_needs_repaint(InvalidateDisplayList should_invalidate_display_list)
{
    Paintable::set_needs_repaint(should_invalidate_display_list);

    if (should_invalidate_display_list == InvalidateDisplayList::Yes) {
        // This box's glyphs are recorded in an ancestor's foreground commands: the containing
        // block's, or a self-painting inline ancestor's.
        for (auto ancestor = parent(); ancestor; ancestor = ancestor->parent()) {
            ancestor->invalidate_paint_cache();
            if (is<PaintableWithLines>(*ancestor))
                break;
        }
    }
}

}
