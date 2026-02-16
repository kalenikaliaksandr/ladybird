/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Vector.h>
#include <LibGC/Ptr.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/LayoutState.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Layout {

class FloatState {
public:
    enum class FloatSide {
        Left,
        Right,
    };

    struct Float {
        GC::Ref<Box const> box;
        LayoutState::UsedValues& used_values;
        CSSPixelRect margin_box_in_root_coords;
        FloatSide side;
        CSSPixels offset_from_side_edge { 0 };
        CSSPixels margin_top_in_root { 0 };
        CSSPixels margin_bottom_in_root { 0 };
    };

    void add_float(Float);
    void refresh_float_rects_in_root(FormattingContext const&, Box const& root);

    FormattingContext::SpaceUsedAndContainingMarginForFloats space_used_and_containing_margin_for_floats(CSSPixels y_in_root, Box const& root) const;
    bool has_float_at_root_y(CSSPixels y_in_root) const;

    FormattingContext::SpaceUsedByFloats intrusion_by_floats_into_box(LayoutState::UsedValues const& box_used_values, CSSPixels y_in_box, FormattingContext const& context, Box const& root) const;
    bool can_fit_line_at_box_y(LayoutState::UsedValues const& box_used_values, CSSPixels block_offset, CSSPixels line_height, CSSPixels available_width, FormattingContext const& context, Box const& root) const;
    CSSPixels find_opportunity_box_y(LayoutState::UsedValues const& box_used_values, CSSPixels start_y, CSSPixels line_height, CSSPixels available_width, FormattingContext const& context, Box const& root) const;

    bool is_empty() const { return m_floats.is_empty(); }

    CSSPixels left_max_width() const { return m_left_max_width; }
    CSSPixels right_max_width() const { return m_right_max_width; }

    template<typename Callback>
    void for_each_float(Callback callback) const
    {
        for (auto const& float_item : m_floats) {
            if (callback(float_item) == IterationDecision::Break)
                return;
        }
    }

    template<typename Callback>
    void for_each_left_float(Callback callback) const
    {
        for (auto const& float_item : m_floats) {
            if (float_item.side == FloatSide::Left) {
                if (callback(float_item) == IterationDecision::Break)
                    return;
            }
        }
    }

    template<typename Callback>
    void for_each_right_float(Callback callback) const
    {
        for (auto const& float_item : m_floats) {
            if (float_item.side == FloatSide::Right) {
                if (callback(float_item) == IterationDecision::Break)
                    return;
            }
        }
    }

    Optional<Float const&> last_float() const
    {
        if (m_floats.is_empty())
            return {};
        return m_floats.last();
    }

    struct FloatPlacement {
        CSSPixels offset_from_side_edge;
        CSSPixels block_offset_adjustment;
    };

    struct FloatPlacementOptions {
        bool is_inline_layout { false };
        bool has_clearance { false };
    };

    FloatPlacement compute_float_placement(FloatSide side, LayoutState::UsedValues const& box_state, AvailableSpace const& available_space, CSSPixels y_in_root, FloatPlacementOptions const&);

    void clear_float_line(FloatSide side);
    bool has_active_float_line() const;
    bool has_active_float_line_on(FloatSide side) const;

    CSSPixels current_line_bottom_in_root(FloatSide side) const;

private:
    struct FloatLineState {
        Vector<size_t> current_line_float_indices;
        CSSPixels current_line_block_offset_adjustment { 0 };

        void reset() { current_line_float_indices.clear(); }
        bool is_empty() const { return current_line_float_indices.is_empty(); }
    };

    Vector<Float> m_floats;
    CSSPixels m_left_max_width { 0 };
    CSSPixels m_right_max_width { 0 };

    FloatLineState m_left_line;
    FloatLineState m_right_line;
};

}
