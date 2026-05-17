/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Compositor/ScrollStateSerialization.h>

#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollNodeStableID const& stable_node_id)
{
    TRY(encoder.encode(stable_node_id.node_id));
    TRY(encoder.encode(static_cast<u8>(stable_node_id.kind)));
    TRY(encoder.encode(stable_node_id.pseudo_element_type));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollNodeStableID> decode(Decoder& decoder)
{
    auto node_id = TRY(decoder.decode<Web::UniqueNodeID>());
    auto raw_kind = TRY(decoder.decode<u8>());
    auto pseudo_element_type = TRY(decoder.decode<u8>());

    Web::Compositor::AsyncScrollNodeKind kind;
    switch (raw_kind) {
    case static_cast<u8>(Web::Compositor::AsyncScrollNodeKind::Viewport):
        kind = Web::Compositor::AsyncScrollNodeKind::Viewport;
        break;
    case static_cast<u8>(Web::Compositor::AsyncScrollNodeKind::Element):
        kind = Web::Compositor::AsyncScrollNodeKind::Element;
        break;
    case static_cast<u8>(Web::Compositor::AsyncScrollNodeKind::PseudoElement):
        kind = Web::Compositor::AsyncScrollNodeKind::PseudoElement;
        break;
    default:
        return Error::from_string_literal("Invalid async scroll node kind");
    }

    return Web::Compositor::AsyncScrollNodeStableID {
        .node_id = node_id,
        .kind = kind,
        .pseudo_element_type = pseudo_element_type,
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::AsyncScrollOffset const& scroll_offset)
{
    TRY(encoder.encode(scroll_offset.stable_node_id));
    TRY(encoder.encode(scroll_offset.compositor_scroll_offset));
    TRY(encoder.encode(scroll_offset.unadopted_scroll_delta));
    return {};
}

template<>
ErrorOr<Web::Compositor::AsyncScrollOffset> decode(Decoder& decoder)
{
    auto stable_node_id = TRY(decoder.decode<Web::Compositor::AsyncScrollNodeStableID>());
    auto compositor_scroll_offset = TRY(decoder.decode<Gfx::FloatPoint>());
    auto unadopted_scroll_delta = TRY(decoder.decode<Gfx::FloatPoint>());
    return Web::Compositor::AsyncScrollOffset {
        .stable_node_id = stable_node_id,
        .compositor_scroll_offset = compositor_scroll_offset,
        .unadopted_scroll_delta = unadopted_scroll_delta,
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollStateSnapshot const& snapshot)
{
    TRY(encoder.encode(snapshot.device_offsets()));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollStateSnapshot> decode(Decoder& decoder)
{
    auto device_offsets = TRY(decoder.decode<Vector<Gfx::FloatPoint>>());
    return Web::Painting::ScrollStateSnapshot::from_device_offsets(move(device_offsets));
}

}
