/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Compositor/DisplayListResourceSerialization.h>

#include <LibGfx/Font/Typeface.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>

namespace Web::Compositor {

static Optional<size_t> find_font_data_buffer(Vector<ReadonlyBytes> const& font_data_sources, ReadonlyBytes font_data)
{
    for (size_t i = 0; i < font_data_sources.size(); ++i) {
        auto candidate = font_data_sources[i];
        if (candidate.data() == font_data.data() && candidate.size() == font_data.size())
            return i;
    }
    return {};
}

static ErrorOr<SerializedFontResource> serialize_font_resource(
    Painting::FontResourceAddition const& font,
    Vector<SerializedFontDataBuffer>& font_data_buffers,
    Vector<ReadonlyBytes>& font_data_sources)
{
    auto font_data = font.resource->typeface().buffer();
    auto font_data_buffer_index = find_font_data_buffer(font_data_sources, font_data);
    if (!font_data_buffer_index.has_value()) {
        TRY(font_data_buffers.try_append(SerializedFontDataBuffer {
            .payload = TRY(SerializedPayload::copy_from(font_data)),
        }));
        TRY(font_data_sources.try_append(font_data));
        font_data_buffer_index = font_data_buffers.size() - 1;
    }

    return SerializedFontResource {
        .font_id = font.id,
        .font_data_buffer_index = font_data_buffer_index.value(),
        .ttc_index = font.resource->typeface().ttc_index(),
        .point_size = font.resource->point_size(),
        .font_variation_settings = font.resource->font_variation_settings(),
        .shape_features = font.resource->features(),
    };
}

ErrorOr<SerializedImageFrameResource> serialize_image_frame_resource(Painting::ImageFrameResourceAddition const& image_frame)
{
    auto bitmap = TRY(image_frame.resource.bitmap().to_bitmap_backed_by_anonymous_buffer());

    return SerializedImageFrameResource {
        .image_frame_id = image_frame.id,
        .bitmap = Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap },
        .color_space = image_frame.resource.color_space(),
    };
}

SerializedVideoFrameSourceResource serialize_video_frame_source_resource(Painting::VideoFrameSourceResourceAddition const& video_frame_source)
{
    return {
        .video_frame_source_id = video_frame_source.id,
    };
}

ErrorOr<Painting::FontResourceAddition> deserialize_font_resource(SerializedFontResource const& font, Vector<SerializedFontDataBuffer> const& font_data_buffers)
{
    if (font.font_data_buffer_index >= font_data_buffers.size())
        return Error::from_string_literal("Serialized font resource references an out-of-bounds font data buffer");

    auto font_data = font_data_buffers[font.font_data_buffer_index].payload.bytes();
    if (font_data.is_empty())
        return Error::from_string_literal("Serialized font resource has empty font data");

    auto typeface = TRY(Gfx::Typeface::try_load_from_temporary_memory(font_data, font.ttc_index));
    auto reconstructed_font = typeface->font(font.point_size, font.font_variation_settings, font.shape_features);

    return Painting::FontResourceAddition {
        .id = font.font_id,
        .resource = move(reconstructed_font),
    };
}

ErrorOr<Painting::ImageFrameResourceAddition> deserialize_image_frame_resource(SerializedImageFrameResource const& image_frame)
{
    if (!image_frame.bitmap.is_valid())
        return Error::from_string_literal("Serialized image frame resource has no bitmap");

    return Painting::ImageFrameResourceAddition {
        .id = image_frame.image_frame_id,
        .resource = Gfx::DecodedImageFrame { *image_frame.bitmap.bitmap(), image_frame.color_space },
    };
}

}

namespace IPC {

static ErrorOr<void> encode_font_variation_settings(Encoder& encoder, Gfx::FontVariationSettings const& font_variation_settings)
{
    auto axes = font_variation_settings.to_sorted_list();
    TRY(encoder.encode_size(axes.size()));
    for (auto const& axis : axes) {
        TRY(encoder.encode(axis.tag.to_u32()));
        TRY(encoder.encode(axis.value));
    }
    return {};
}

static ErrorOr<Gfx::FontVariationSettings> decode_font_variation_settings(Decoder& decoder)
{
    Gfx::FontVariationSettings font_variation_settings;
    auto axis_count = TRY(decoder.decode_size());
    for (size_t i = 0; i < axis_count; ++i) {
        auto tag = Gfx::FourCC::from_u32(TRY(decoder.decode<u32>()));
        auto value = TRY(decoder.decode<float>());
        TRY(font_variation_settings.axes.try_set(tag, value));
    }
    return font_variation_settings;
}

static ErrorOr<void> encode_shape_features(Encoder& encoder, Gfx::ShapeFeatures const& shape_features)
{
    TRY(encoder.encode_size(shape_features.size()));
    for (auto const& feature : shape_features) {
        TRY(encoder.encode(Gfx::FourCC { feature.tag }.to_u32()));
        TRY(encoder.encode(feature.value));
    }
    return {};
}

static ErrorOr<Gfx::ShapeFeatures> decode_shape_features(Decoder& decoder)
{
    Gfx::ShapeFeatures shape_features;
    auto feature_count = TRY(decoder.decode_size());
    TRY(shape_features.try_ensure_capacity(feature_count));
    for (size_t i = 0; i < feature_count; ++i) {
        auto tag = Gfx::FourCC::from_u32(TRY(decoder.decode<u32>()));
        auto value = TRY(decoder.decode<u32>());
        Gfx::ShapeFeature feature {
            .tag = { tag.cc[0], tag.cc[1], tag.cc[2], tag.cc[3] },
            .value = value,
        };
        shape_features.unchecked_append(feature);
    }
    return shape_features;
}

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
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedFontDataBuffer const& font_data_buffer)
{
    TRY(encoder.encode(font_data_buffer.payload));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedFontDataBuffer> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedFontDataBuffer {
        .payload = TRY(decoder.decode<Web::Compositor::SerializedPayload>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedFontResource const& font)
{
    TRY(encoder.encode(font.font_id.value()));
    TRY(encoder.encode(font.font_data_buffer_index));
    TRY(encoder.encode(font.ttc_index));
    TRY(encoder.encode(font.point_size));
    TRY(encode_font_variation_settings(encoder, font.font_variation_settings));
    TRY(encode_shape_features(encoder, font.shape_features));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedFontResource> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedFontResource {
        .font_id = Web::Painting::FontResourceId { TRY(decoder.decode<u64>()) },
        .font_data_buffer_index = TRY(decoder.decode<size_t>()),
        .ttc_index = TRY(decoder.decode<u32>()),
        .point_size = TRY(decoder.decode<float>()),
        .font_variation_settings = TRY(decode_font_variation_settings(decoder)),
        .shape_features = TRY(decode_shape_features(decoder)),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedImageFrameResource const& image_frame)
{
    TRY(encoder.encode(image_frame.image_frame_id.value()));
    TRY(encoder.encode(image_frame.bitmap));
    TRY(encoder.encode(image_frame.color_space));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedImageFrameResource> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedImageFrameResource {
        .image_frame_id = Web::Painting::ImageFrameResourceId { TRY(decoder.decode<u64>()) },
        .bitmap = TRY(decoder.decode<Gfx::ShareableBitmap>()),
        .color_space = TRY(decoder.decode<Gfx::ColorSpace>()),
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
    Vector<Web::Compositor::SerializedFontDataBuffer> font_data_buffers;
    Vector<ReadonlyBytes> font_data_sources;
    Vector<Web::Compositor::SerializedFontResource> fonts;
    TRY(font_data_buffers.try_ensure_capacity(transaction.fonts.size()));
    TRY(font_data_sources.try_ensure_capacity(transaction.fonts.size()));
    TRY(fonts.try_ensure_capacity(transaction.fonts.size()));
    for (auto const& font : transaction.fonts)
        fonts.unchecked_append(TRY(Web::Compositor::serialize_font_resource(font, font_data_buffers, font_data_sources)));

    Vector<Web::Compositor::SerializedImageFrameResource> image_frames;
    TRY(image_frames.try_ensure_capacity(transaction.image_frames.size()));
    for (auto const& image_frame : transaction.image_frames)
        image_frames.unchecked_append(TRY(Web::Compositor::serialize_image_frame_resource(image_frame)));

    Vector<Web::Compositor::SerializedVideoFrameSourceResource> video_frame_sources;
    TRY(video_frame_sources.try_ensure_capacity(transaction.video_frame_sources.size()));
    for (auto const& video_frame_source : transaction.video_frame_sources)
        video_frame_sources.unchecked_append(Web::Compositor::serialize_video_frame_source_resource(video_frame_source));

    TRY(encoder.encode(font_data_buffers));
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
    auto font_data_buffers = TRY(decoder.decode<Vector<Web::Compositor::SerializedFontDataBuffer>>());
    auto serialized_fonts = TRY(decoder.decode<Vector<Web::Compositor::SerializedFontResource>>());
    auto serialized_image_frames = TRY(decoder.decode<Vector<Web::Compositor::SerializedImageFrameResource>>());
    auto serialized_video_frame_sources = TRY(decoder.decode<Vector<Web::Compositor::SerializedVideoFrameSourceResource>>());
    auto display_list_count = TRY(decoder.decode_size());

    Web::Painting::DisplayListResourceTransaction transaction;

    TRY(transaction.fonts.try_ensure_capacity(serialized_fonts.size()));
    for (auto const& font : serialized_fonts)
        transaction.fonts.unchecked_append(TRY(Web::Compositor::deserialize_font_resource(font, font_data_buffers)));

    TRY(transaction.image_frames.try_ensure_capacity(serialized_image_frames.size()));
    for (auto const& image_frame : serialized_image_frames)
        transaction.image_frames.unchecked_append(TRY(Web::Compositor::deserialize_image_frame_resource(image_frame)));

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
