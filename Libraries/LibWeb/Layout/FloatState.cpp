/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/FloatState.h>
#include <LibWeb/Layout/FormattingContext.h>

namespace Web::Layout {

void FloatState::add_float(Float float_item)
{
    m_floats.append(move(float_item));

    auto float_index = m_floats.size() - 1;
    auto const& added = m_floats[float_index];
    auto& line = (added.side == FloatSide::Left) ? m_left_line : m_right_line;

    line.current_line_float_indices.append(float_index);

    CSSPixels current_width;
    if (added.side == FloatSide::Left)
        current_width = added.offset_from_side_edge + added.used_values.content_width() + added.used_values.margin_box_right();
    else
        current_width = added.offset_from_side_edge + added.used_values.margin_box_left();

    auto& max_width = (added.side == FloatSide::Left) ? m_left_max_width : m_right_max_width;
    max_width = max(current_width, max_width);
}

void FloatState::refresh_float_rects_in_root(FormattingContext const& context, Box const& root)
{
    for (auto& float_item : m_floats) {
        float_item.margin_box_in_root_coords = context.margin_box_rect_in_ancestor_coordinate_space(float_item.used_values, root);
    }
}

FormattingContext::SpaceUsedAndContainingMarginForFloats FloatState::space_used_and_containing_margin_for_floats(CSSPixels y_in_root, Box const& root) const
{
    FormattingContext::SpaceUsedAndContainingMarginForFloats space_and_containing_margin;

    // Search left floats in reverse (most recently added first).
    for (ssize_t float_index = m_floats.size() - 1; float_index >= 0; --float_index) {
        auto const& float_item = m_floats[float_index];
        if (float_item.side != FloatSide::Left)
            continue;
        auto margin_rect = float_item.margin_box_in_root_coords;
        if (margin_rect.contains_vertically(y_in_root)) {
            CSSPixels offset_from_containing_block_chain_margins_between_here_and_root = 0;
            for (auto const* containing_block = float_item.used_values.containing_block_used_values();
                containing_block && &containing_block->node() != &root;
                containing_block = containing_block->containing_block_used_values()) {
                offset_from_containing_block_chain_margins_between_here_and_root += containing_block->margin_box_left();
            }
            space_and_containing_margin.left_used_space = float_item.offset_from_side_edge
                + float_item.used_values.content_width()
                + float_item.used_values.margin_box_right();
            space_and_containing_margin.left_total_containing_margin = offset_from_containing_block_chain_margins_between_here_and_root;
            space_and_containing_margin.matching_left_float_box = float_item.box;
            break;
        }
    }

    for (ssize_t float_index = m_floats.size() - 1; float_index >= 0; --float_index) {
        auto const& float_item = m_floats[float_index];
        if (float_item.side != FloatSide::Right)
            continue;
        auto margin_rect = float_item.margin_box_in_root_coords;
        if (margin_rect.contains_vertically(y_in_root)) {
            CSSPixels offset_from_containing_block_chain_margins_between_here_and_root = 0;
            for (auto const* containing_block = float_item.used_values.containing_block_used_values();
                containing_block && &containing_block->node() != &root;
                containing_block = containing_block->containing_block_used_values()) {
                offset_from_containing_block_chain_margins_between_here_and_root += containing_block->margin_box_right();
            }
            space_and_containing_margin.right_used_space = float_item.offset_from_side_edge
                + float_item.used_values.margin_box_left();
            space_and_containing_margin.right_total_containing_margin = offset_from_containing_block_chain_margins_between_here_and_root;
            space_and_containing_margin.matching_right_float_box = float_item.box;
            break;
        }
    }

    return space_and_containing_margin;
}

FormattingContext::SpaceUsedByFloats FloatState::intrusion_by_floats_into_box(LayoutState::UsedValues const& box_used_values, CSSPixels y_in_box, FormattingContext const& context, Box const& root) const
{
    auto box_in_root_rect = context.content_box_rect_in_ancestor_coordinate_space(box_used_values, root);
    CSSPixels y_in_root = box_in_root_rect.y() + y_in_box;
    auto space_and_containing_margin = space_used_and_containing_margin_for_floats(y_in_root, root);

    CSSPixels left_intrusion = 0;
    if (space_and_containing_margin.matching_left_float_box) {
        auto left_side_floats_limit_to_right = space_and_containing_margin.left_total_containing_margin + space_and_containing_margin.left_used_space;
        left_intrusion = max(CSSPixels(0), left_side_floats_limit_to_right - box_in_root_rect.x());
    }

    CSSPixels right_intrusion = 0;
    if (space_and_containing_margin.matching_right_float_box) {
        auto right_side_floats_limit_to_right = space_and_containing_margin.right_used_space + space_and_containing_margin.right_total_containing_margin;
        CSSPixels offset_from_containing_block_chain_margins_between_here_and_root = 0;
        for (auto const* containing_block = &box_used_values; containing_block && &containing_block->node() != &root; containing_block = containing_block->containing_block_used_values()) {
            offset_from_containing_block_chain_margins_between_here_and_root += containing_block->margin_box_right();
        }
        right_intrusion = max(CSSPixels(0), right_side_floats_limit_to_right - offset_from_containing_block_chain_margins_between_here_and_root);
    }

    return { left_intrusion, right_intrusion };
}

bool FloatState::has_float_at_root_y(CSSPixels y_in_root) const
{
    for (auto const& float_item : m_floats) {
        if (float_item.margin_box_in_root_coords.contains_vertically(y_in_root))
            return true;
    }
    return false;
}

bool FloatState::can_fit_line_at_box_y(LayoutState::UsedValues const& box_used_values, CSSPixels block_offset, CSSPixels line_height, CSSPixels available_width, FormattingContext const& context, Box const& root) const
{
    auto top_intrusions = intrusion_by_floats_into_box(box_used_values, block_offset, context, root);
    auto bottom_intrusions = intrusion_by_floats_into_box(box_used_values, block_offset + line_height - 1, context, root);

    auto top_left = top_intrusions.left;
    auto top_right = available_width - top_intrusions.right;
    auto bottom_left = bottom_intrusions.left;
    auto bottom_right = available_width - bottom_intrusions.right;

    if (top_left > bottom_right)
        return false;
    if (bottom_left > top_right)
        return false;
    return true;
}

CSSPixels FloatState::find_opportunity_box_y(LayoutState::UsedValues const& box_used_values, CSSPixels start_y, CSSPixels line_height, CSSPixels available_width, FormattingContext const& context, Box const& root) const
{
    auto box_in_root_rect = context.content_box_rect_in_ancestor_coordinate_space(box_used_values, root);
    CSSPixels start_y_root = box_in_root_rect.y() + start_y;

    // Collect candidate Y positions where line availability can change.
    // Two kinds of transition points exist:
    //   1. Float bottom edges — where a float exits, freeing space for the
    //      top-of-line sample (y).
    //   2. float_bottom - line_height + 1 — where a float exits for the
    //      bottom-of-line sample (y + line_height - 1), which can create an
    //      opportunity earlier than the float bottom edge itself.
    // This is O(N^2) overall: O(N log N) sort + O(N) candidates each calling
    // intrusion_by_floats_into_box() which is O(N).
    Vector<CSSPixels, 16> candidates;
    candidates.append(start_y_root);
    for (auto const& float_item : m_floats) {
        auto float_bottom = float_item.margin_box_in_root_coords.bottom();
        if (float_bottom > start_y_root)
            candidates.append(float_bottom);
        auto early_exit_y = float_bottom - line_height + 1;
        if (early_exit_y > start_y_root)
            candidates.append(early_exit_y);
    }

    quick_sort(candidates);

    for (auto candidate_root_y : candidates) {
        auto local_y = candidate_root_y - box_in_root_rect.y();
        if (can_fit_line_at_box_y(box_used_values, local_y, line_height, available_width, context, root))
            return local_y;
    }

    // Below all floats, a line always fits.
    CSSPixels max_bottom = start_y_root;
    for (auto const& float_item : m_floats)
        max_bottom = max(max_bottom, float_item.margin_box_in_root_coords.bottom());
    return max_bottom - box_in_root_rect.y();
}

FloatState::FloatPlacement FloatState::compute_float_placement(FloatSide side, LayoutState::UsedValues const& box_state, AvailableSpace const& available_space, CSSPixels y_in_root, FloatPlacementOptions const& options)
{
    auto& line = (side == FloatSide::Left) ? m_left_line : m_right_line;

    CSSPixels offset_from_side_edge = 0;
    auto float_to_edge = [&] {
        if (side == FloatSide::Left)
            offset_from_side_edge = box_state.margin_box_left();
        else
            offset_from_side_edge = box_state.content_width() + box_state.margin_box_right();
    };

    if (line.is_empty()) {
        float_to_edge();
        line.current_line_block_offset_adjustment = 0;
    } else {
        // NOTE: If we're in inline layout, the LineBuilder has already provided the right Y offset.
        //       In block layout, we adjust by the side's current Y offset here.
        CSSPixels adjusted_y_in_root = y_in_root;
        if (!options.is_inline_layout)
            adjusted_y_in_root += line.current_line_block_offset_adjustment;

        bool did_touch_preceding_float = false;
        bool did_place_next_to_preceding_float = false;

        // Walk current-line exclusions in reverse, looking for the innermost preceding
        // float that intersects vertically with the new float.
        for (int index_in_line = line.current_line_float_indices.size() - 1; index_in_line >= 0; --index_in_line) {
            auto const& preceding = m_floats[line.current_line_float_indices[index_in_line]];
            if (!preceding.margin_box_in_root_coords.contains_vertically(adjusted_y_in_root))
                continue;

            CSSPixels tentative_offset_from_side_edge = 0;
            bool fits_next_to_preceding_float = false;
            if (side == FloatSide::Left) {
                tentative_offset_from_side_edge = max(preceding.offset_from_side_edge + preceding.used_values.content_width() + preceding.used_values.margin_box_right(), CSSPixels(0)) + box_state.margin_box_left();
                if (available_space.width.is_definite())
                    fits_next_to_preceding_float = (tentative_offset_from_side_edge + box_state.content_width() + box_state.margin_box_right()) <= available_space.width.to_px_or_zero();
                else if (available_space.width.is_max_content() || available_space.width.is_indefinite())
                    fits_next_to_preceding_float = true;
            } else {
                tentative_offset_from_side_edge = preceding.offset_from_side_edge + preceding.used_values.margin_box_left() + box_state.margin_box_right() + box_state.content_width();
                if (available_space.width.is_definite())
                    fits_next_to_preceding_float = (tentative_offset_from_side_edge + box_state.margin_box_left()) <= available_space.width.to_px_or_zero();
                else if (available_space.width.is_max_content() || available_space.width.is_indefinite())
                    fits_next_to_preceding_float = true;
            }

            did_touch_preceding_float = true;
            if (!fits_next_to_preceding_float)
                break;
            offset_from_side_edge = tentative_offset_from_side_edge;
            did_place_next_to_preceding_float = true;
            break;
        }

        if (!did_touch_preceding_float || !did_place_next_to_preceding_float || options.has_clearance) {
            // No vertical overlap, no horizontal space, or clearance — break the float line.
            float_to_edge();
            CSSPixels lowest_margin_edge = 0;
            for (auto float_index : line.current_line_float_indices)
                lowest_margin_edge = max(lowest_margin_edge, m_floats[float_index].margin_box_in_root_coords.bottom());

            line.current_line_block_offset_adjustment += max(CSSPixels(0), lowest_margin_edge - adjusted_y_in_root + box_state.margin_box_top());
            line.reset();
        }
    }

    return { offset_from_side_edge, line.current_line_block_offset_adjustment };
}

void FloatState::clear_float_line(FloatSide side)
{
    auto& line = (side == FloatSide::Left) ? m_left_line : m_right_line;
    line.reset();
}

bool FloatState::has_active_float_line() const
{
    return !m_left_line.is_empty() || !m_right_line.is_empty();
}

bool FloatState::has_active_float_line_on(FloatSide side) const
{
    auto const& line = (side == FloatSide::Left) ? m_left_line : m_right_line;
    return !line.is_empty();
}

CSSPixels FloatState::current_line_bottom_in_root(FloatSide side) const
{
    auto const& line = (side == FloatSide::Left) ? m_left_line : m_right_line;
    CSSPixels lowest_bottom = 0;
    for (auto float_index : line.current_line_float_indices)
        lowest_bottom = max(lowest_bottom, m_floats[float_index].margin_box_in_root_coords.bottom());
    return lowest_bottom;
}

}
