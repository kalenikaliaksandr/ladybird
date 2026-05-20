/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/Vector.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/ColorSpace.h>
#include <LibGfx/Font/FontVariationSettings.h>
#include <LibGfx/ShapeFeature.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibIPC/Forward.h>
#include <LibMedia/VideoFrame.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Web::Painting {

struct FontTransport {
    FontResourceId id;
    Core::AnonymousBuffer font_data;
    u32 ttc_index { 0 };
    float point_size { 0 };
    Gfx::FontVariationSettings variations;
    Gfx::ShapeFeatures features;
};

struct ImageFrameTransport {
    ImageFrameResourceId id;
    Gfx::ShareableBitmap bitmap;
    Gfx::ColorSpace color_space;
};

struct VideoFrameTransport {
    VideoFrameResourceId id;
    Optional<NonnullRefPtr<Media::VideoFrame const>> frame;
};

struct DisplayListResourceTransactionTransport {
    Vector<FontTransport> fonts;
    Vector<ImageFrameTransport> image_frames;
    Vector<VideoFrameTransport> video_frames;
    Vector<NonnullRefPtr<DisplayList const>> display_lists;

    Vector<FontResourceId> font_ids_to_remove;
    Vector<ImageFrameResourceId> image_frame_ids_to_remove;
    Vector<VideoFrameResourceId> video_frame_ids_to_remove;
    Vector<DisplayListResourceId> display_list_ids_to_remove;
};

WEB_API ErrorOr<DisplayListResourceTransactionTransport> create_display_list_resource_transaction_transport(DisplayListResourceTransaction&&);
WEB_API VideoFrameTransport create_video_frame_transport(VideoFrameResourceId, Media::VideoFrame const&);

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::FontTransport const&);
template<>
WEB_API ErrorOr<Web::Painting::FontTransport> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ImageFrameTransport const&);
template<>
WEB_API ErrorOr<Web::Painting::ImageFrameTransport> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::VideoFrameTransport const&);
template<>
WEB_API ErrorOr<Web::Painting::VideoFrameTransport> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::DisplayListResourceTransactionTransport const&);
template<>
WEB_API ErrorOr<Web::Painting::DisplayListResourceTransactionTransport> decode(Decoder&);

}
