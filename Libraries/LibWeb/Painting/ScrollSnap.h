/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/Vector.h>
#include <LibGC/Ptr.h>
#include <LibGC/WeakInlines.h>
#include <LibWeb/CSS/PseudoElement.h>
#include <LibWeb/Compositor/ScrollSnap.h>
#include <LibWeb/Export.h>
#include <LibWeb/PixelUnits.h>

namespace Web::DOM {

class Document;
class Element;
class Node;

}

namespace Web::Layout {

class Node;

}

namespace Web::Painting {

// The element a snap area belongs to, which identifies the area across relayouts. The reference is weak: an area
// removed from the document is no longer a snap area the container can be returned to.
struct SnapAreaReference {
    GC::Weak<DOM::Element const> element;
    Optional<CSS::PseudoElement> pseudo_element;

    // Two references to areas that have both gone away compare equal, which no remembered area is ever tested against.
    bool operator==(SnapAreaReference const& other) const
    {
        return element.ptr() == other.element.ptr() && pseudo_element == other.pseudo_element;
    }
};

using Compositor::MomentumFlingEstimator;
using Compositor::SnapAxes;
using Compositor::SnapSelectionStrategy;

WEB_API SnapAxes snap_axes_of_scroll_container(Layout::Node const& snap_container);

WEB_API bool is_scroll_snap_container(Layout::Node const&);

// The snap areas a scroll container is snapped to in each axis, so that it can be re-snapped to those same snap areas
// after a content change.
struct SnappedAreas {
    Vector<SnapAreaReference> x;
    Vector<SnapAreaReference> y;

    bool is_empty() const { return x.is_empty() && y.is_empty(); }
};

struct SnapDestination {
    CSSPixelPoint position;
    // Whether a snap position was selected in each axis; an axis whose snap positions are all ineligible for the
    // scroll keeps the destination it was given.
    bool snapped_x { false };
    bool snapped_y { false };
    // Whether snap position selection ran in each axis. An axis the scroll did not travel in is not evaluated, and
    // whatever snap area the container is snapped to there remains snapped.
    bool evaluated_x { false };
    bool evaluated_y { false };
    SnappedAreas snapped_areas {};
};

WEB_API Optional<Compositor::SnapContainerData> collect_scroll_snap_data(Layout::Node const&);
WEB_API SnappedAreas resolve_snapped_areas(Compositor::SnappedAreaIDs const&, DOM::Document const&);

WEB_API SnapDestination adjust_scroll_destination_for_snapping(Layout::Node const& snap_container, CSSPixelPoint destination, SnapSelectionStrategy const& strategy = {});

struct ResnapSelection {
    SnappedAreas const& snapped_areas;
    GC::Ptr<DOM::Node const> focused_node;
    GC::Ptr<DOM::Element const> targeted_element;
};

WEB_API SnapDestination select_resnap_destination(Layout::Node const& snap_container, CSSPixelPoint current_offset, ResnapSelection const&);

}
