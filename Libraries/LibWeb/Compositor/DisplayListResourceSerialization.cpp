/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Compositor/DisplayListResourceSerialization.h>

#include <AK/Checked.h>
#include <AK/NumericLimits.h>
#include <AK/StdLibExtras.h>
#include <LibGfx/Font/Typeface.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/YUVData.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibMedia/VideoFrame.h>
#include <LibWeb/Painting/VideoFrameSource.h>

namespace Web::Compositor {

static ErrorOr<ReadonlyBytes> payload_section(SerializedPayload const& payload, size_t offset, size_t size)
{
    auto payload_bytes = payload.bytes();
    if (offset > payload_bytes.size())
        return Error::from_string_literal("Serialized payload section offset is out of bounds");
    if (size > payload_bytes.size() - offset)
        return Error::from_string_literal("Serialized payload section size is out of bounds");
    return payload_bytes.slice(offset, size);
}

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

ErrorOr<Painting::VideoFrameSourceResourceAddition> deserialize_video_frame_source_resource(SerializedVideoFrameSourceResource const& video_frame_source)
{
    return Painting::VideoFrameSourceResourceAddition {
        .id = video_frame_source.video_frame_source_id,
        .resource = Painting::VideoFrameSource::create(),
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

struct ValidatedYUVPlaneLayout {
    Gfx::IntSize size;
    size_t y_plane_size { 0 };
    size_t u_plane_size { 0 };
    size_t v_plane_size { 0 };
};

static ErrorOr<size_t> checked_plane_size(Gfx::Size<u32> size, u8 bit_depth, Media::Subsampling subsampling, bool chroma_plane)
{
    auto component_size = bit_depth <= 8 ? 1 : 2;
    auto plane_size = chroma_plane ? subsampling.subsampled_size(size) : size;

    Checked<size_t> bytes { plane_size.width() };
    bytes *= plane_size.height();
    bytes *= component_size;
    if (bytes.has_overflow())
        return Error::from_string_literal("Serialized video frame plane size overflowed");
    return bytes.value();
}

static ErrorOr<ValidatedYUVPlaneLayout> validate_yuv_plane_layout(Gfx::Size<u32> size, u8 bit_depth, Media::Subsampling subsampling)
{
    if (size.is_empty())
        return Error::from_string_literal("Serialized video frame has empty size");
    if (size.width() > static_cast<u32>(NumericLimits<int>::max()) || size.height() > static_cast<u32>(NumericLimits<int>::max()))
        return Error::from_string_literal("Serialized video frame size exceeds supported range");
    if (bit_depth == 0 || bit_depth > 16)
        return Error::from_string_literal("Serialized video frame has invalid bit depth");

    auto y_plane_size = TRY(checked_plane_size(size, bit_depth, subsampling, false));
    auto uv_plane_size = TRY(checked_plane_size(size, bit_depth, subsampling, true));

    return ValidatedYUVPlaneLayout {
        .size = Gfx::IntSize { static_cast<int>(size.width()), static_cast<int>(size.height()) },
        .y_plane_size = y_plane_size,
        .u_plane_size = uv_plane_size,
        .v_plane_size = uv_plane_size,
    };
}

ErrorOr<SerializedVideoFrameUpdate> serialize_video_frame_update(Painting::VideoFrameResourceId video_frame_source_id, u64 frame_sequence_id, Media::VideoFrame const& frame)
{
    auto const& yuv_data = frame.yuv_data();
    auto yuv_size = yuv_data.size();
    if (yuv_size.width() < 0 || yuv_size.height() < 0)
        return Error::from_string_literal("Video frame YUV data has invalid size");
    if (static_cast<u32>(yuv_size.width()) != frame.width() || static_cast<u32>(yuv_size.height()) != frame.height())
        return Error::from_string_literal("Video frame YUV data size does not match frame size");

    auto plane_layout = TRY(validate_yuv_plane_layout(frame.size(), yuv_data.bit_depth(), yuv_data.subsampling()));

    auto y_data = yuv_data.y_data();
    auto u_data = yuv_data.u_data();
    auto v_data = yuv_data.v_data();
    if (y_data.size() != plane_layout.y_plane_size || u_data.size() != plane_layout.u_plane_size || v_data.size() != plane_layout.v_plane_size)
        return Error::from_string_literal("Video frame YUV plane size does not match metadata");

    Checked<size_t> payload_size { y_data.size() };
    payload_size += u_data.size();
    payload_size += v_data.size();
    if (payload_size.has_overflow())
        return Error::from_string_literal("Serialized video frame payload size overflowed");

    auto payload = TRY(SerializedPayload::create(payload_size.value()));
    auto y_plane_offset = static_cast<size_t>(0);
    auto u_plane_offset = y_data.size();
    auto v_plane_offset = u_plane_offset + u_data.size();

    y_data.copy_to(payload.bytes().slice(y_plane_offset, y_data.size()));
    u_data.copy_to(payload.bytes().slice(u_plane_offset, u_data.size()));
    v_data.copy_to(payload.bytes().slice(v_plane_offset, v_data.size()));

    return SerializedVideoFrameUpdate {
        .video_frame_source_id = video_frame_source_id,
        .frame_sequence_id = frame_sequence_id,
        .size = frame.size(),
        .bit_depth = yuv_data.bit_depth(),
        .subsampling = yuv_data.subsampling(),
        .cicp = yuv_data.cicp(),
        .color_space = frame.color_space(),
        .timestamp = frame.timestamp(),
        .duration = frame.duration(),
        .payload = move(payload),
        .y_plane_offset = y_plane_offset,
        .y_plane_size = y_data.size(),
        .u_plane_offset = u_plane_offset,
        .u_plane_size = u_data.size(),
        .v_plane_offset = v_plane_offset,
        .v_plane_size = v_data.size(),
    };
}

ErrorOr<NonnullRefPtr<Media::VideoFrame>> deserialize_video_frame_update(SerializedVideoFrameUpdate const& video_frame_update)
{
    auto plane_layout = TRY(validate_yuv_plane_layout(video_frame_update.size, video_frame_update.bit_depth, video_frame_update.subsampling));
    if (video_frame_update.y_plane_size != plane_layout.y_plane_size
        || video_frame_update.u_plane_size != plane_layout.u_plane_size
        || video_frame_update.v_plane_size != plane_layout.v_plane_size) {
        return Error::from_string_literal("Serialized video frame plane size does not match metadata");
    }

    auto y_data = TRY(payload_section(video_frame_update.payload, video_frame_update.y_plane_offset, video_frame_update.y_plane_size));
    auto u_data = TRY(payload_section(video_frame_update.payload, video_frame_update.u_plane_offset, video_frame_update.u_plane_size));
    auto v_data = TRY(payload_section(video_frame_update.payload, video_frame_update.v_plane_offset, video_frame_update.v_plane_size));

    auto yuv_data = TRY(Gfx::YUVData::create(plane_layout.size, video_frame_update.bit_depth, video_frame_update.subsampling, video_frame_update.cicp));
    y_data.copy_to(yuv_data->y_data());
    u_data.copy_to(yuv_data->u_data());
    v_data.copy_to(yuv_data->v_data());

    return TRY(try_make_ref_counted<Media::VideoFrame>(
        video_frame_update.timestamp,
        video_frame_update.duration,
        video_frame_update.size,
        video_frame_update.bit_depth,
        video_frame_update.color_space,
        move(yuv_data)));
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

    TRY(transaction.video_frame_sources.try_ensure_capacity(serialized_video_frame_sources.size()));
    for (auto const& video_frame_source : serialized_video_frame_sources)
        transaction.video_frame_sources.unchecked_append(TRY(Web::Compositor::deserialize_video_frame_source_resource(video_frame_source)));

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
