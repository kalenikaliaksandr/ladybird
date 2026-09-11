/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Math.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Compositor/ScrollSnapState.h>
#include <LibWeb/Compositor/Types.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SnapAreaID const& value)
{
    TRY(encoder.encode(value.element_id));
    TRY(encoder.encode(value.pseudo_element));
    return {};
}

template<>
ErrorOr<Web::Compositor::SnapAreaID> decode(Decoder& decoder)
{
    Web::Compositor::SnapAreaID value;
    value.element_id = TRY(decoder.decode<Web::UniqueNodeID>());
    value.pseudo_element = TRY(decoder.decode<Optional<Web::CSS::PseudoElement>>());
    if (value.pseudo_element.has_value() && *value.pseudo_element >= Web::CSS::PseudoElement::KnownPseudoElementCount)
        return Error::from_string_literal("Invalid scroll snap pseudo-element");
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::CoveringRange const& value)
{
    TRY(encoder.encode(value.start.raw_value()));
    TRY(encoder.encode(value.end.raw_value()));
    return {};
}

template<>
ErrorOr<Web::Compositor::CoveringRange> decode(Decoder& decoder)
{
    Web::Compositor::CoveringRange value;
    value.start = Web::CSSPixels::from_raw(TRY(decoder.decode<i32>()));
    value.end = Web::CSSPixels::from_raw(TRY(decoder.decode<i32>()));
    if (value.start > value.end)
        return Error::from_string_literal("Invalid scroll snap covering range");
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SnapPositionCandidate const& value)
{
    TRY(encoder.encode(value.offset.raw_value()));
    TRY(encoder.encode(value.area));
    TRY(encoder.encode(value.covering_ranges));
    TRY(encoder.encode(value.cross_axis_visible_range_start.raw_value()));
    TRY(encoder.encode(value.cross_axis_visible_range_end.raw_value()));
    TRY(encoder.encode(value.always_stop));
    return {};
}

template<>
ErrorOr<Web::Compositor::SnapPositionCandidate> decode(Decoder& decoder)
{
    Web::Compositor::SnapPositionCandidate value;
    value.offset = Web::CSSPixels::from_raw(TRY(decoder.decode<i32>()));
    value.area = TRY(decoder.decode<Web::Compositor::SnapAreaID>());
    value.covering_ranges = TRY(decoder.decode<Vector<Web::Compositor::CoveringRange, 1>>());
    value.cross_axis_visible_range_start = Web::CSSPixels::from_raw(TRY(decoder.decode<i32>()));
    value.cross_axis_visible_range_end = Web::CSSPixels::from_raw(TRY(decoder.decode<i32>()));
    value.always_stop = TRY(decoder.decode<bool>());
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SnapContainerData const& value)
{
    TRY(encoder.encode(value.axes.x));
    TRY(encoder.encode(value.axes.y));
    TRY(encoder.encode(value.strictness));
    TRY(encoder.encode(value.snapport_size.width().raw_value()));
    TRY(encoder.encode(value.snapport_size.height().raw_value()));
    TRY(encoder.encode(value.min_scroll_offset.x().raw_value()));
    TRY(encoder.encode(value.min_scroll_offset.y().raw_value()));
    TRY(encoder.encode(value.max_scroll_offset.x().raw_value()));
    TRY(encoder.encode(value.max_scroll_offset.y().raw_value()));
    TRY(encoder.encode(value.candidates.x_candidates));
    TRY(encoder.encode(value.candidates.y_candidates));
    return {};
}

template<>
ErrorOr<Web::Compositor::SnapContainerData> decode(Decoder& decoder)
{
    Web::Compositor::SnapContainerData value;
    value.axes.x = TRY(decoder.decode<bool>());
    value.axes.y = TRY(decoder.decode<bool>());
    value.strictness = TRY(decoder.decode<Web::CSS::ScrollSnapStrictness>());
    value.snapport_size = { Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())), Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())) };
    value.min_scroll_offset = { Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())), Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())) };
    value.max_scroll_offset = { Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())), Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())) };
    value.candidates.x_candidates = TRY(decoder.decode<Vector<Web::Compositor::SnapPositionCandidate>>());
    value.candidates.y_candidates = TRY(decoder.decode<Vector<Web::Compositor::SnapPositionCandidate>>());
    if (value.strictness != Web::CSS::ScrollSnapStrictness::None
        && value.strictness != Web::CSS::ScrollSnapStrictness::Mandatory
        && value.strictness != Web::CSS::ScrollSnapStrictness::Proximity)
        return Error::from_string_literal("Invalid scroll snap strictness");
    if (value.snapport_size.width() < 0 || value.snapport_size.height() < 0
        || value.min_scroll_offset.x() > value.max_scroll_offset.x()
        || value.min_scroll_offset.y() > value.max_scroll_offset.y())
        return Error::from_string_literal("Invalid scroll snap geometry");
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SnappedAreaIDs const& value)
{
    TRY(encoder.encode(value.x));
    TRY(encoder.encode(value.y));
    return {};
}

template<>
ErrorOr<Web::Compositor::SnappedAreaIDs> decode(Decoder& decoder)
{
    Web::Compositor::SnappedAreaIDs value;
    value.x = TRY(decoder.decode<Vector<Web::Compositor::SnapAreaID>>());
    value.y = TRY(decoder.decode<Vector<Web::Compositor::SnapAreaID>>());
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SnapDestination const& value)
{
    TRY(encoder.encode(value.position.x().raw_value()));
    TRY(encoder.encode(value.position.y().raw_value()));
    TRY(encoder.encode(value.snapped_x));
    TRY(encoder.encode(value.snapped_y));
    TRY(encoder.encode(value.evaluated_x));
    TRY(encoder.encode(value.evaluated_y));
    TRY(encoder.encode(value.snapped_areas));
    return {};
}

template<>
ErrorOr<Web::Compositor::SnapDestination> decode(Decoder& decoder)
{
    Web::Compositor::SnapDestination value;
    value.position = { Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())), Web::CSSPixels::from_raw(TRY(decoder.decode<i32>())) };
    value.snapped_x = TRY(decoder.decode<bool>());
    value.snapped_y = TRY(decoder.decode<bool>());
    value.evaluated_x = TRY(decoder.decode<bool>());
    value.evaluated_y = TRY(decoder.decode<bool>());
    value.snapped_areas = TRY(decoder.decode<Web::Compositor::SnappedAreaIDs>());
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::ScrollSnapContainer const& value)
{
    TRY(encoder.encode(value.stable_node_id));
    TRY(encoder.encode(value.data));
    return {};
}

template<>
ErrorOr<Web::Compositor::ScrollSnapContainer> decode(Decoder& decoder)
{
    Web::Compositor::ScrollSnapContainer value;
    value.stable_node_id = TRY(decoder.decode<Web::Compositor::AsyncScrollNodeStableID>());
    value.data = TRY(decoder.decode<Web::Compositor::SnapContainerData>());
    return value;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::ScrollSnapStateSnapshot const& value)
{
    TRY(encoder.encode(value.document_id));
    TRY(encoder.encode(value.revision));
    TRY(encoder.encode(value.device_pixels_per_css_pixel));
    TRY(encoder.encode(value.containers));
    return {};
}

template<>
ErrorOr<Web::Compositor::ScrollSnapStateSnapshot> decode(Decoder& decoder)
{
    Web::Compositor::ScrollSnapStateSnapshot value;
    value.document_id = TRY(decoder.decode<Web::UniqueNodeID>());
    value.revision = TRY(decoder.decode<u64>());
    value.device_pixels_per_css_pixel = TRY(decoder.decode<double>());
    value.containers = TRY(decoder.decode<Vector<Web::Compositor::ScrollSnapContainer>>());
    if (!isfinite(value.device_pixels_per_css_pixel) || value.device_pixels_per_css_pixel <= 0)
        return Error::from_string_literal("Invalid scroll snap scale");
    return value;
}

}
