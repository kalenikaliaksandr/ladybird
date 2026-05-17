/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Vector.h>
#include <LibGfx/Font/FontVariationSettings.h>
#include <LibGfx/ShapeFeature.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/SerializedPayload.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Compositor {

struct SerializedFontDataBuffer {
    SerializedPayload payload;
};

struct SerializedFontResource {
    Painting::FontResourceId font_id;
    size_t font_data_buffer_index { 0 };
    u32 ttc_index { 0 };
    float point_size { 0 };
    Gfx::FontVariationSettings font_variation_settings;
    Gfx::ShapeFeatures shape_features;
};

struct SerializedImageFrameResource {
    Painting::ImageFrameResourceId image_frame_id;
};

struct SerializedVideoFrameSourceResource {
    Painting::VideoFrameResourceId video_frame_source_id;
};

WEB_API SerializedImageFrameResource serialize_image_frame_resource(Painting::ImageFrameResourceAddition const&);
WEB_API SerializedVideoFrameSourceResource serialize_video_frame_source_resource(Painting::VideoFrameSourceResourceAddition const&);
WEB_API ErrorOr<Painting::FontResourceAddition> deserialize_font_resource(SerializedFontResource const&, Vector<SerializedFontDataBuffer> const&);

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::DisplayListResourceTransaction const&);

template<>
WEB_API ErrorOr<Web::Painting::DisplayListResourceTransaction> decode(Decoder&);

}
