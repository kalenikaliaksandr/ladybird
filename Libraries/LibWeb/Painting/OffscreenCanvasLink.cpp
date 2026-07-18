/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/OffscreenCanvasLink.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::OffscreenCanvasPlaceholderLink const& link)
{
    TRY(encoder.encode(link.canvas_id));
    TRY(encoder.encode(link.nonce));
    return {};
}

template<>
ErrorOr<Web::Painting::OffscreenCanvasPlaceholderLink> decode(Decoder& decoder)
{
    return Web::Painting::OffscreenCanvasPlaceholderLink {
        .canvas_id = TRY(decoder.decode<Web::Painting::CanvasId>()),
        .nonce = TRY(decoder.decode<u64>()),
    };
}

}
