/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Web::Painting {

// Links a placeholder HTMLCanvasElement to the canvas surface its transferred
// OffscreenCanvas renders into. The canvas id is reserved in the Compositor's
// registry up front; the nonce proves a claiming or watching connection was
// actually handed the link, since ids themselves are small enumerable values.
struct OffscreenCanvasPlaceholderLink {
    CanvasId canvas_id;
    u64 nonce { 0 };
};

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::OffscreenCanvasPlaceholderLink const&);
template<>
WEB_API ErrorOr<Web::Painting::OffscreenCanvasPlaceholderLink> decode(Decoder&);

}
