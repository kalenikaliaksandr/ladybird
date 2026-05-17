/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Time.h>
#include <AK/Vector.h>
#include <LibGfx/ColorSpace.h>
#include <LibGfx/Font/FontVariationSettings.h>
#include <LibGfx/ShapeFeature.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/Size.h>
#include <LibIPC/Forward.h>
#include <LibMedia/Color/CodingIndependentCodePoints.h>
#include <LibMedia/Forward.h>
#include <LibMedia/Subsampling.h>
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
    Gfx::ShareableBitmap bitmap;
    Gfx::ColorSpace color_space;
};

struct SerializedVideoFrameSourceResource {
    Painting::VideoFrameResourceId video_frame_source_id;
};

struct SerializedVideoFrameUpdate {
    Painting::VideoFrameResourceId video_frame_source_id;
    u64 frame_sequence_id { 0 };
    Gfx::Size<u32> size;
    u8 bit_depth { 0 };
    Media::Subsampling subsampling;
    Media::CodingIndependentCodePoints cicp;
    Gfx::ColorSpace color_space;
    AK::Duration timestamp;
    AK::Duration duration;
    SerializedPayload payload;
    size_t y_plane_offset { 0 };
    size_t y_plane_size { 0 };
    size_t u_plane_offset { 0 };
    size_t u_plane_size { 0 };
    size_t v_plane_offset { 0 };
    size_t v_plane_size { 0 };
};

WEB_API ErrorOr<SerializedImageFrameResource> serialize_image_frame_resource(Painting::ImageFrameResourceAddition const&);
WEB_API SerializedVideoFrameSourceResource serialize_video_frame_source_resource(Painting::VideoFrameSourceResourceAddition const&);
WEB_API ErrorOr<Painting::FontResourceAddition> deserialize_font_resource(SerializedFontResource const&, Vector<SerializedFontDataBuffer> const&);
WEB_API ErrorOr<Painting::ImageFrameResourceAddition> deserialize_image_frame_resource(SerializedImageFrameResource const&);
WEB_API ErrorOr<Painting::VideoFrameSourceResourceAddition> deserialize_video_frame_source_resource(SerializedVideoFrameSourceResource const&);

WEB_API ErrorOr<SerializedVideoFrameUpdate> serialize_video_frame_update(Painting::VideoFrameResourceId, u64 frame_sequence_id, Media::VideoFrame const&);
WEB_API ErrorOr<NonnullRefPtr<Media::VideoFrame>> deserialize_video_frame_update(SerializedVideoFrameUpdate const&);

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::DisplayListResourceTransaction const&);

template<>
WEB_API ErrorOr<Web::Painting::DisplayListResourceTransaction> decode(Decoder&);

}
