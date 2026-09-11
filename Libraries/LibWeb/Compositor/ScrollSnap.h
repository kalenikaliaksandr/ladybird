/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/Vector.h>
#include <LibWeb/CSS/Enums.h>
#include <LibWeb/CSS/PseudoElement.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Compositor {

// A snap area's identity survives layout updates without keeping its DOM node alive.
struct SnapAreaID {
    UniqueNodeID element_id { 0 };
    Optional<CSS::PseudoElement> pseudo_element;

    bool is_valid() const { return element_id.value() != 0; }
    bool operator==(SnapAreaID const&) const = default;
};

// https://drafts.csswg.org/css-scroll-snap-1/#scroll-types
struct SnapSelectionStrategy {
    enum class Type : u8 {
        // An absolute scroll, or any other operation with only an intended end position.
        EndPosition,
        // A relative scroll with only an intended direction, such as a mouse wheel step or an arrow key press.
        Direction,
        // A relative scroll with both an intended direction and end position, such as scrollBy().
        EndPositionAndDirection,
    };

    Type type { Type::EndPosition };
    // The scroll offset the operation travels from; a snap position with `scroll-snap-stop: always` must not be
    // passed over on the way from there to the selected snap position.
    Optional<CSSPixelPoint> start_offset {};
    // The net offset change the operation's input produced; an axis the operation did not travel in selects no snap
    // position.
    CSSPixelPoint displacement {};
    // Snap positions short of this offset in the direction of travel are not selected; it defaults to the start
    // offset.
    Optional<CSSPixelPoint> starting_positions_boundary {};
};

// The displacement the momentum of a flick has left to travel, estimated from the deltas that momentum has produced
// so far.
class WEB_API MomentumFlingEstimator {
public:
    void reset();

    // The displacement left to travel, including the delta given; momentum that has not yet decayed far enough to
    // tell where it is headed reports no estimate.
    Optional<CSSPixelPoint> estimate_remaining_displacement(CSSPixelPoint momentum_delta);

private:
    Optional<CSSPixelPoint> m_previous_momentum_delta;
    u32 m_consecutively_decaying_momentum_deltas { 0 };
};

struct SnapAxes {
    bool x { false };
    bool y { false };

    bool is_empty() const { return !x && !y; }
    bool operator==(SnapAxes const&) const = default;
};

struct CoveringRange {
    CSSPixels start;
    CSSPixels end;
    bool operator==(CoveringRange const&) const = default;
};

struct SnapPositionCandidate {
    CSSPixels offset;
    SnapAreaID area {};
    Vector<CoveringRange, 1> covering_ranges;
    // Open interval in the other axis where the area overlaps the snapport.
    CSSPixels cross_axis_visible_range_start { 0 };
    CSSPixels cross_axis_visible_range_end { 0 };
    bool always_stop { false };
    bool operator==(SnapPositionCandidate const&) const = default;
};

struct SnapAxisCandidates {
    Vector<SnapPositionCandidate> x_candidates;
    Vector<SnapPositionCandidate> y_candidates;
    bool operator==(SnapAxisCandidates const&) const = default;
};

struct SnapContainerData {
    SnapAxes axes;
    CSS::ScrollSnapStrictness strictness { CSS::ScrollSnapStrictness::None };
    CSSPixelSize snapport_size;
    CSSPixelPoint min_scroll_offset;
    CSSPixelPoint max_scroll_offset;
    SnapAxisCandidates candidates;
    bool operator==(SnapContainerData const&) const = default;
};

struct SnappedAreaIDs {
    Vector<SnapAreaID> x;
    Vector<SnapAreaID> y;
    bool operator==(SnappedAreaIDs const&) const = default;
};

struct SnapDestination {
    CSSPixelPoint position;
    bool snapped_x { false };
    bool snapped_y { false };
    bool evaluated_x { false };
    bool evaluated_y { false };
    SnappedAreaIDs snapped_areas {};
};

WEB_API SnapDestination select_snap_destination(SnapContainerData const&, CSSPixelPoint destination, SnapSelectionStrategy const& = {});

// Numeric helpers also used by WebContent's DOM-dependent re-snap selection.
struct SnapAxisSelection {
    CSSPixels destination;
    CSSPixels start;
    CSSPixels direction;
    Optional<CSSPixels> starting_positions_boundary;
};

struct SnapAxisChoice {
    CSSPixels offset;
    SnapAreaID area;
};

WEB_API bool snap_area_is_visible_at_cross_axis_offset(SnapPositionCandidate const&, CSSPixels);
WEB_API bool candidate_has_snap_position_at(SnapPositionCandidate const&, CSSPixels);
WEB_API bool chosen_offsets_are_mutually_visible(SnapAxisCandidates const&, CSSPixels x, CSSPixels y);
WEB_API Optional<SnapAxisChoice> choose_snap_offset_for_axis(Vector<SnapPositionCandidate> const&, SnapAxisSelection const&, CSSPixels snapport_size, CSS::ScrollSnapStrictness, Optional<CSSPixels> cross_axis_offset, Optional<SnapAreaID> only_area = {});
WEB_API Vector<SnapAreaID> snap_areas_at_offset(Vector<SnapPositionCandidate> const&, CSSPixels offset, CSSPixels cross_axis_offset);

}
