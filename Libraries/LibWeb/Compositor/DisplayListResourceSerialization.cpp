/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Compositor/DisplayListResourceSerialization.h>

#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>

namespace Web::Compositor {

SerializedFontResource serialize_font_resource(Painting::FontResourceAddition const& font)
{
    return {
        .font_id = font.id,
    };
}

SerializedImageFrameResource serialize_image_frame_resource(Painting::ImageFrameResourceAddition const& image_frame)
{
    return {
        .image_frame_id = image_frame.id,
    };
}

SerializedVideoFrameSourceResource serialize_video_frame_source_resource(Painting::VideoFrameSourceResourceAddition const& video_frame_source)
{
    return {
        .video_frame_source_id = video_frame_source.id,
    };
}

}

namespace IPC {

static ErrorOr<void> encode_resource_ids(Encoder& encoder, ReadonlySpan<Web::Painting::FontResourceId> resource_ids)
{
    TRY(encoder.encode_size(resource_ids.size()));
    for (auto resource_id : resource_ids)
        TRY(encoder.encode(resource_id.value()));
    return {};
}

static ErrorOr<void> encode_resource_ids(Encoder& encoder, ReadonlySpan<Web::Painting::ImageFrameResourceId> resource_ids)
{
    TRY(encoder.encode_size(resource_ids.size()));
    for (auto resource_id : resource_ids)
        TRY(encoder.encode(resource_id.value()));
    return {};
}

static ErrorOr<void> encode_resource_ids(Encoder& encoder, ReadonlySpan<Web::Painting::VideoFrameResourceId> resource_ids)
{
    TRY(encoder.encode_size(resource_ids.size()));
    for (auto resource_id : resource_ids)
        TRY(encoder.encode(resource_id.value()));
    return {};
}

static ErrorOr<void> encode_resource_ids(Encoder& encoder, ReadonlySpan<Web::Painting::DisplayListResourceId> resource_ids)
{
    TRY(encoder.encode_size(resource_ids.size()));
    for (auto resource_id : resource_ids)
        TRY(encoder.encode(resource_id.value()));
    return {};
}

template<typename ResourceId>
static ErrorOr<Vector<ResourceId>> decode_resource_ids(Decoder& decoder)
{
    Vector<ResourceId> resource_ids;
    auto resource_id_count = TRY(decoder.decode_size());
    TRY(resource_ids.try_ensure_capacity(resource_id_count));
    for (size_t i = 0; i < resource_id_count; ++i)
        resource_ids.unchecked_append(ResourceId { TRY(decoder.decode<u64>()) });
    return resource_ids;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedFontResource const& font)
{
    TRY(encoder.encode(font.font_id.value()));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedFontResource> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedFontResource {
        .font_id = Web::Painting::FontResourceId { TRY(decoder.decode<u64>()) },
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedImageFrameResource const& image_frame)
{
    TRY(encoder.encode(image_frame.image_frame_id.value()));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedImageFrameResource> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedImageFrameResource {
        .image_frame_id = Web::Painting::ImageFrameResourceId { TRY(decoder.decode<u64>()) },
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedVideoFrameSourceResource const& video_frame_source)
{
    TRY(encoder.encode(video_frame_source.video_frame_source_id.value()));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedVideoFrameSourceResource> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedVideoFrameSourceResource {
        .video_frame_source_id = Web::Painting::VideoFrameResourceId { TRY(decoder.decode<u64>()) },
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayListResourceTransaction const& transaction)
{
    Vector<Web::Compositor::SerializedFontResource> fonts;
    TRY(fonts.try_ensure_capacity(transaction.fonts.size()));
    for (auto const& font : transaction.fonts)
        fonts.unchecked_append(Web::Compositor::serialize_font_resource(font));

    Vector<Web::Compositor::SerializedImageFrameResource> image_frames;
    TRY(image_frames.try_ensure_capacity(transaction.image_frames.size()));
    for (auto const& image_frame : transaction.image_frames)
        image_frames.unchecked_append(Web::Compositor::serialize_image_frame_resource(image_frame));

    Vector<Web::Compositor::SerializedVideoFrameSourceResource> video_frame_sources;
    TRY(video_frame_sources.try_ensure_capacity(transaction.video_frame_sources.size()));
    for (auto const& video_frame_source : transaction.video_frame_sources)
        video_frame_sources.unchecked_append(Web::Compositor::serialize_video_frame_source_resource(video_frame_source));

    TRY(encoder.encode(fonts));
    TRY(encoder.encode(image_frames));
    TRY(encoder.encode(video_frame_sources));

    TRY(encoder.encode_size(transaction.display_lists.size()));
    for (auto const& display_list : transaction.display_lists) {
        TRY(encoder.encode(display_list.id.value()));
        TRY(encoder.encode(display_list.resource));
    }

    TRY(encode_resource_ids(encoder, transaction.font_ids_to_remove.span()));
    TRY(encode_resource_ids(encoder, transaction.image_frame_ids_to_remove.span()));
    TRY(encode_resource_ids(encoder, transaction.video_frame_source_ids_to_remove.span()));
    TRY(encode_resource_ids(encoder, transaction.display_list_ids_to_remove.span()));
    return {};
}

template<>
ErrorOr<Web::Painting::DisplayListResourceTransaction> decode(Decoder& decoder)
{
    auto serialized_fonts = TRY(decoder.decode<Vector<Web::Compositor::SerializedFontResource>>());
    auto serialized_image_frames = TRY(decoder.decode<Vector<Web::Compositor::SerializedImageFrameResource>>());
    auto serialized_video_frame_sources = TRY(decoder.decode<Vector<Web::Compositor::SerializedVideoFrameSourceResource>>());
    auto display_list_count = TRY(decoder.decode_size());

    Web::Painting::DisplayListResourceTransaction transaction;

    if (!serialized_fonts.is_empty())
        return Error::from_string_literal("Missing font resource payload");
    if (!serialized_image_frames.is_empty())
        return Error::from_string_literal("Missing image frame resource payload");
    if (!serialized_video_frame_sources.is_empty())
        return Error::from_string_literal("Missing video frame source resource payload");

    TRY(transaction.display_lists.try_ensure_capacity(display_list_count));
    for (size_t i = 0; i < display_list_count; ++i) {
        auto display_list_id = Web::Painting::DisplayListResourceId { TRY(decoder.decode<u64>()) };
        auto display_list = TRY(decoder.decode<NonnullRefPtr<Web::Painting::DisplayList>>());
        transaction.display_lists.unchecked_append({ display_list_id, move(display_list) });
    }

    transaction.font_ids_to_remove = TRY(decode_resource_ids<Web::Painting::FontResourceId>(decoder));
    transaction.image_frame_ids_to_remove = TRY(decode_resource_ids<Web::Painting::ImageFrameResourceId>(decoder));
    transaction.video_frame_source_ids_to_remove = TRY(decode_resource_ids<Web::Painting::VideoFrameResourceId>(decoder));
    transaction.display_list_ids_to_remove = TRY(decode_resource_ids<Web::Painting::DisplayListResourceId>(decoder));
    return transaction;
}

}
