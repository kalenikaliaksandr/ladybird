/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Vector.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Compositor {

struct SerializedFontResource {
    Painting::FontResourceId font_id;
};

struct SerializedImageFrameResource {
    Painting::ImageFrameResourceId image_frame_id;
};

struct SerializedVideoFrameSourceResource {
    Painting::VideoFrameResourceId video_frame_source_id;
};

WEB_API SerializedFontResource serialize_font_resource(Painting::FontResourceAddition const&);
WEB_API SerializedImageFrameResource serialize_image_frame_resource(Painting::ImageFrameResourceAddition const&);
WEB_API SerializedVideoFrameSourceResource serialize_video_frame_source_resource(Painting::VideoFrameSourceResourceAddition const&);

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::DisplayListResourceTransaction const&);

template<>
WEB_API ErrorOr<Web::Painting::DisplayListResourceTransaction> decode(Decoder&);

}
