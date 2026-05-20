/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Bitmap.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/Typeface.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>
#include <LibWeb/Painting/DisplayListResourceTransport.h>

#include <string.h>

namespace Web::Painting {

static ErrorOr<Core::AnonymousBuffer> anonymous_buffer_from_bytes(ReadonlyBytes bytes)
{
    auto buffer = TRY(Core::AnonymousBuffer::create_with_size(bytes.size()));
    memcpy(buffer.data<void>(), bytes.data(), bytes.size());
    return buffer;
}

static ErrorOr<FontTransport> create_font_transport(Gfx::Font const& font)
{
    Core::AnonymousBuffer font_data;
    if (auto anonymous_font_data = font.typeface().anonymous_font_data(); anonymous_font_data.has_value())
        font_data = anonymous_font_data.release_value();
    else
        font_data = TRY(anonymous_buffer_from_bytes(font.typeface().font_data_bytes()));

    return FontTransport {
        .id = FontResourceId { font.id() },
        .font_data = move(font_data),
        .ttc_index = font.typeface().font_data_ttc_index(),
        .point_size = font.point_size(),
        .variations = font.variation_settings(),
        .features = font.features(),
    };
}

static ErrorOr<NonnullRefPtr<Gfx::Font const>> create_font_from_transport(FontTransport&& font)
{
    if (!font.font_data.is_valid())
        return Error::from_string_literal("Display-list resource transport contained invalid font data");

    auto typeface = TRY(Gfx::Typeface::try_load_from_anonymous_buffer(move(font.font_data), font.ttc_index));
    return typeface->font(font.point_size, font.variations, font.features);
}

static ErrorOr<ImageFrameTransport> create_image_frame_transport(Gfx::DecodedImageFrame const& frame)
{
    auto bitmap = frame.bitmap().to_shareable_bitmap();
    if (!bitmap.is_valid())
        return Error::from_string_literal("Display-list resource transport failed to create image-frame bitmap");

    return ImageFrameTransport {
        .id = ImageFrameResourceId { frame.id() },
        .bitmap = move(bitmap),
        .color_space = frame.color_space(),
    };
}

static ErrorOr<Gfx::DecodedImageFrame> create_image_frame_from_transport(ImageFrameTransport&& frame)
{
    if (!frame.bitmap.is_valid() || !frame.bitmap.bitmap())
        return Error::from_string_literal("Display-list resource transport contained invalid image-frame bitmap");
    return Gfx::DecodedImageFrame { *frame.bitmap.bitmap(), move(frame.color_space) };
}

VideoFrameTransport create_video_frame_transport(VideoFrameResourceId id, Media::VideoFrame const& frame)
{
    return VideoFrameTransport {
        .id = id,
        .frame = NonnullRefPtr<Media::VideoFrame const> { frame },
    };
}

ErrorOr<DisplayListResourceTransactionTransport> create_display_list_resource_transaction_transport(DisplayListResourceTransaction&& transaction)
{
    DisplayListResourceTransactionTransport transport;

    TRY(transport.fonts.try_ensure_capacity(transaction.fonts.size()));
    for (auto const& font : transaction.fonts)
        transport.fonts.unchecked_append(TRY(create_font_transport(*font)));

    TRY(transport.image_frames.try_ensure_capacity(transaction.image_frames.size()));
    for (auto const& frame : transaction.image_frames)
        transport.image_frames.unchecked_append(TRY(create_image_frame_transport(frame)));

    TRY(transport.video_frames.try_ensure_capacity(transaction.video_frames.size()));
    for (auto const& video_frame : transaction.video_frames) {
        if (video_frame.frame)
            transport.video_frames.unchecked_append(create_video_frame_transport(video_frame.id, *video_frame.frame));
        else
            transport.video_frames.unchecked_append(VideoFrameTransport { .id = video_frame.id, .frame = {} });
    }

    transport.display_lists = move(transaction.display_lists);

    transport.font_ids_to_remove = move(transaction.font_ids_to_remove);
    transport.image_frame_ids_to_remove = move(transaction.image_frame_ids_to_remove);
    transport.video_frame_ids_to_remove = move(transaction.video_frame_ids_to_remove);
    transport.display_list_ids_to_remove = move(transaction.display_list_ids_to_remove);
    return transport;
}

ErrorOr<void> DisplayListResourceStorage::apply_transport_transaction(DisplayListResourceTransactionTransport&& transaction)
{
    for (auto& font : transaction.fonts)
        set_font(font.id, TRY(create_font_from_transport(move(font))));
    for (auto& frame : transaction.image_frames)
        set_image_frame(frame.id, TRY(create_image_frame_from_transport(move(frame))));
    for (auto& frame : transaction.video_frames) {
        auto id = frame.id;
        if (frame.frame.has_value())
            set_video_frame(id, frame.frame.release_value());
        else
            add_video_frame(id);
    }
    for (auto& display_list : transaction.display_lists)
        set_display_list(DisplayListResourceId { display_list->id() }, move(display_list));

    for (auto id : transaction.font_ids_to_remove)
        m_fonts.remove(id.value());
    for (auto id : transaction.image_frame_ids_to_remove)
        m_image_frames.remove(id.value());
    for (auto id : transaction.video_frame_ids_to_remove)
        m_video_frames.remove(id.value());
    for (auto id : transaction.display_list_ids_to_remove)
        m_display_lists.remove(id.value());
    return {};
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::FontTransport const& font)
{
    TRY(encoder.encode(font.id));
    TRY(encoder.encode(font.font_data));
    TRY(encoder.encode(font.ttc_index));
    TRY(encoder.encode(font.point_size));
    TRY(encoder.encode(font.variations));
    TRY(encoder.encode(font.features));
    return {};
}

template<>
ErrorOr<Web::Painting::FontTransport> decode(Decoder& decoder)
{
    return Web::Painting::FontTransport {
        .id = TRY(decoder.decode<Web::Painting::FontResourceId>()),
        .font_data = TRY(decoder.decode<Core::AnonymousBuffer>()),
        .ttc_index = TRY(decoder.decode<u32>()),
        .point_size = TRY(decoder.decode<float>()),
        .variations = TRY(decoder.decode<Gfx::FontVariationSettings>()),
        .features = TRY(decoder.decode<Gfx::ShapeFeatures>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ImageFrameTransport const& frame)
{
    TRY(encoder.encode(frame.id));
    TRY(encoder.encode(frame.bitmap));
    TRY(encoder.encode(frame.color_space));
    return {};
}

template<>
ErrorOr<Web::Painting::ImageFrameTransport> decode(Decoder& decoder)
{
    return Web::Painting::ImageFrameTransport {
        .id = TRY(decoder.decode<Web::Painting::ImageFrameResourceId>()),
        .bitmap = TRY(decoder.decode<Gfx::ShareableBitmap>()),
        .color_space = TRY(decoder.decode<Gfx::ColorSpace>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::VideoFrameTransport const& frame)
{
    TRY(encoder.encode(frame.id));
    TRY(encoder.encode(frame.frame));
    return {};
}

template<>
ErrorOr<Web::Painting::VideoFrameTransport> decode(Decoder& decoder)
{
    return Web::Painting::VideoFrameTransport {
        .id = TRY(decoder.decode<Web::Painting::VideoFrameResourceId>()),
        .frame = TRY(decoder.decode<Optional<NonnullRefPtr<Media::VideoFrame const>>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayListResourceTransactionTransport const& transaction)
{
    TRY(encoder.encode(transaction.fonts));
    TRY(encoder.encode(transaction.image_frames));
    TRY(encoder.encode(transaction.video_frames));
    TRY(encoder.encode_size(transaction.display_lists.size()));
    for (auto const& display_list : transaction.display_lists)
        TRY(encoder.encode(*display_list));
    TRY(encoder.encode(transaction.font_ids_to_remove));
    TRY(encoder.encode(transaction.image_frame_ids_to_remove));
    TRY(encoder.encode(transaction.video_frame_ids_to_remove));
    TRY(encoder.encode(transaction.display_list_ids_to_remove));
    return {};
}

template<>
ErrorOr<Web::Painting::DisplayListResourceTransactionTransport> decode(Decoder& decoder)
{
    auto fonts = TRY(decoder.decode<Vector<Web::Painting::FontTransport>>());
    auto image_frames = TRY(decoder.decode<Vector<Web::Painting::ImageFrameTransport>>());
    auto video_frames = TRY(decoder.decode<Vector<Web::Painting::VideoFrameTransport>>());

    auto display_list_count = TRY(decoder.decode_size());
    Vector<NonnullRefPtr<Web::Painting::DisplayList const>> display_lists;
    TRY(display_lists.try_ensure_capacity(display_list_count));
    for (size_t i = 0; i < display_list_count; ++i)
        display_lists.unchecked_append(TRY(decoder.decode<NonnullRefPtr<Web::Painting::DisplayList>>()));

    return Web::Painting::DisplayListResourceTransactionTransport {
        .fonts = move(fonts),
        .image_frames = move(image_frames),
        .video_frames = move(video_frames),
        .display_lists = move(display_lists),
        .font_ids_to_remove = TRY(decoder.decode<Vector<Web::Painting::FontResourceId>>()),
        .image_frame_ids_to_remove = TRY(decoder.decode<Vector<Web::Painting::ImageFrameResourceId>>()),
        .video_frame_ids_to_remove = TRY(decoder.decode<Vector<Web::Painting::VideoFrameResourceId>>()),
        .display_list_ids_to_remove = TRY(decoder.decode<Vector<Web::Painting::DisplayListResourceId>>()),
    };
}

}
