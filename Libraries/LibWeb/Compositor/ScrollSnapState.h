/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibIPC/Forward.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Compositor/ScrollSnap.h>

namespace Web::Compositor {

struct ScrollSnapContainer {
    AsyncScrollNodeStableID stable_node_id;
    SnapContainerData data;
    bool operator==(ScrollSnapContainer const&) const = default;
};

// Replaced independently of paint command reuse, but installed with the scene or scroll state that produced it.
struct ScrollSnapStateSnapshot {
    UniqueNodeID document_id { 0 };
    u64 revision { 0 };
    double device_pixels_per_css_pixel { 1 };
    Vector<ScrollSnapContainer> containers;
    bool operator==(ScrollSnapStateSnapshot const&) const = default;
};

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SnapAreaID const&);
template<>
WEB_API ErrorOr<Web::Compositor::SnapAreaID> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::CoveringRange const&);
template<>
WEB_API ErrorOr<Web::Compositor::CoveringRange> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SnapPositionCandidate const&);
template<>
WEB_API ErrorOr<Web::Compositor::SnapPositionCandidate> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SnapContainerData const&);
template<>
WEB_API ErrorOr<Web::Compositor::SnapContainerData> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SnappedAreaIDs const&);
template<>
WEB_API ErrorOr<Web::Compositor::SnappedAreaIDs> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::SnapDestination const&);
template<>
WEB_API ErrorOr<Web::Compositor::SnapDestination> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::ScrollSnapContainer const&);
template<>
WEB_API ErrorOr<Web::Compositor::ScrollSnapContainer> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::ScrollSnapStateSnapshot const&);
template<>
WEB_API ErrorOr<Web::Compositor::ScrollSnapStateSnapshot> decode(Decoder&);

}
