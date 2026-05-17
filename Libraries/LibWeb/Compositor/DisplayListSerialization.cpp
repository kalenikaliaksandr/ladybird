/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Checked.h>
#include <AK/HashMap.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/SerializedPayload.h>
#include <LibWeb/Compositor/VisualContextSerialization.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayList::AsyncScrollingMetadata const& metadata)
{
    TRY(encoder.encode(metadata.viewport_rect));
    TRY(encoder.encode(metadata.wheel_event_listener_state_generation));
    TRY(encoder.encode(metadata.has_blocking_wheel_event_listeners));
    TRY(encoder.encode(metadata.has_blocking_wheel_event_region_covering_viewport));
    return {};
}

template<>
ErrorOr<Web::Painting::DisplayList::AsyncScrollingMetadata> decode(Decoder& decoder)
{
    return Web::Painting::DisplayList::AsyncScrollingMetadata {
        .viewport_rect = TRY(decoder.decode<Gfx::IntRect>()),
        .wheel_event_listener_state_generation = TRY(decoder.decode<u64>()),
        .has_blocking_wheel_event_listeners = TRY(decoder.decode<bool>()),
        .has_blocking_wheel_event_region_covering_viewport = TRY(decoder.decode<bool>()),
    };
}

}

namespace Web::Compositor {

struct DisplayListWireData {
    u64 id { 0 };
    SerializedPayload payload;
    size_t command_bytes_offset { 0 };
    size_t command_bytes_size { 0 };
    size_t accumulated_visual_context_tree_offset { 0 };
    size_t accumulated_visual_context_tree_size { 0 };
    Optional<Painting::DisplayList::AsyncScrollingMetadata> async_scrolling_metadata;
};

struct DisplayListFilterImageFrameWireData {
    Painting::ImageFrameResourceId image_frame_id;
    Gfx::ShareableBitmap bitmap;
    Gfx::ColorSpace color_space;
};

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::DisplayListFilterImageFrameWireData const& image_frame)
{
    TRY(encoder.encode(image_frame.image_frame_id.value()));
    TRY(encoder.encode(image_frame.bitmap));
    TRY(encoder.encode(image_frame.color_space));
    return {};
}

template<>
ErrorOr<Web::Compositor::DisplayListFilterImageFrameWireData> decode(Decoder& decoder)
{
    return Web::Compositor::DisplayListFilterImageFrameWireData {
        .image_frame_id = Web::Painting::ImageFrameResourceId { TRY(decoder.decode<u64>()) },
        .bitmap = TRY(decoder.decode<Gfx::ShareableBitmap>()),
        .color_space = TRY(decoder.decode<Gfx::ColorSpace>()),
    };
}

}

namespace Web::Compositor {

static ErrorOr<ReadonlyBytes> payload_section(SerializedPayload const& payload, size_t offset, size_t size)
{
    auto payload_bytes = payload.bytes();
    if (offset > payload_bytes.size())
        return Error::from_string_literal("Serialized display list section offset is out of bounds");
    if (size > payload_bytes.size() - offset)
        return Error::from_string_literal("Serialized display list section size is out of bounds");
    return payload_bytes.slice(offset, size);
}

static ErrorOr<DisplayListFilterImageFrameWireData> create_display_list_filter_image_frame_wire_data(Gfx::DecodedImageFrame const& image_frame)
{
    auto bitmap = TRY(image_frame.bitmap().to_bitmap_backed_by_anonymous_buffer());

    return DisplayListFilterImageFrameWireData {
        .image_frame_id = Painting::ImageFrameResourceId { image_frame.id() },
        .bitmap = Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap },
        .color_space = image_frame.color_space(),
    };
}

static ErrorOr<DisplayListWireData> create_display_list_wire_data(
    Painting::DisplayList const& display_list,
    Vector<DisplayListFilterImageFrameWireData>& filter_image_frames)
{
    HashTable<Painting::ImageFrameResourceId> encoded_filter_image_frames;
    Optional<Error> filter_image_serialization_error;
    EncodeFilterImage encode_filter_image = [&](Gfx::DecodedImageFrame const& image_frame) -> u64 {
        auto resource_id = Painting::ImageFrameResourceId { image_frame.id() };
        if (!filter_image_serialization_error.has_value()
            && encoded_filter_image_frames.set(resource_id, AK::HashSetExistingEntryBehavior::Keep) == HashSetResult::InsertedNewEntry) {
            auto serialized_image_frame = create_display_list_filter_image_frame_wire_data(image_frame);
            if (serialized_image_frame.is_error())
                filter_image_serialization_error = serialized_image_frame.release_error();
            else if (auto result = filter_image_frames.try_append(serialized_image_frame.release_value()); result.is_error())
                filter_image_serialization_error = result.release_error();
        }
        return resource_id.value();
    };

    auto serialized_tree = TRY(serialize_accumulated_visual_context_tree(display_list.visual_context_tree(), encode_filter_image));
    if (filter_image_serialization_error.has_value())
        return filter_image_serialization_error.release_value();
    auto serialized_tree_bytes = TRY(encode_serialized_accumulated_visual_context_tree(serialized_tree));
    auto command_bytes = display_list.command_bytes();

    Checked<size_t> payload_size { command_bytes.size() };
    payload_size += serialized_tree_bytes.size();
    if (payload_size.has_overflow())
        return Error::from_string_literal("Serialized display list payload size overflowed");

    auto payload = TRY(SerializedPayload::create(payload_size.value()));
    command_bytes.copy_to(payload.bytes().slice(0, command_bytes.size()));
    serialized_tree_bytes.bytes().copy_to(payload.bytes().slice(command_bytes.size(), serialized_tree_bytes.size()));

    return DisplayListWireData {
        .id = display_list.id(),
        .payload = move(payload),
        .command_bytes_offset = 0,
        .command_bytes_size = command_bytes.size(),
        .accumulated_visual_context_tree_offset = command_bytes.size(),
        .accumulated_visual_context_tree_size = serialized_tree_bytes.size(),
        .async_scrolling_metadata = display_list.async_scrolling_metadata(),
    };
}

static ErrorOr<NonnullRefPtr<Painting::DisplayList>> create_display_list_from_wire_data(
    DisplayListWireData const& display_list,
    DecodeFilterImage const& decode_filter_image)
{
    auto command_bytes = TRY(payload_section(display_list.payload, display_list.command_bytes_offset, display_list.command_bytes_size));
    auto serialized_tree_bytes = TRY(payload_section(display_list.payload, display_list.accumulated_visual_context_tree_offset, display_list.accumulated_visual_context_tree_size));

    auto serialized_tree = TRY(decode_serialized_accumulated_visual_context_tree(serialized_tree_bytes));
    auto visual_context_tree = TRY(deserialize_accumulated_visual_context_tree(serialized_tree, decode_filter_image));
    auto copied_command_bytes = TRY(ByteBuffer::copy(command_bytes));

    return Painting::DisplayList::create_from_serialized(
        move(visual_context_tree),
        display_list.id,
        move(copied_command_bytes),
        display_list.async_scrolling_metadata);
}

static ErrorOr<HashMap<u64, Gfx::DecodedImageFrame>> decode_display_list_filter_image_frames(
    Vector<DisplayListFilterImageFrameWireData> const& serialized_filter_image_frames)
{
    HashMap<u64, Gfx::DecodedImageFrame> filter_image_frames;
    TRY(filter_image_frames.try_ensure_capacity(serialized_filter_image_frames.size()));

    for (auto const& image_frame : serialized_filter_image_frames) {
        if (!image_frame.bitmap.is_valid())
            return Error::from_string_literal("Serialized display list filter image has no bitmap");

        TRY(filter_image_frames.try_set(
            image_frame.image_frame_id.value(),
            Gfx::DecodedImageFrame { *image_frame.bitmap.bitmap(), image_frame.color_space }));
    }

    return filter_image_frames;
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayList const& display_list)
{
    Vector<Web::Compositor::DisplayListFilterImageFrameWireData> filter_image_frames;
    auto wire_data = TRY(Web::Compositor::create_display_list_wire_data(display_list, filter_image_frames));

    TRY(encoder.encode(wire_data.id));
    TRY(encoder.encode(wire_data.payload));
    TRY(encoder.encode(wire_data.command_bytes_offset));
    TRY(encoder.encode(wire_data.command_bytes_size));
    TRY(encoder.encode(wire_data.accumulated_visual_context_tree_offset));
    TRY(encoder.encode(wire_data.accumulated_visual_context_tree_size));
    TRY(encoder.encode(wire_data.async_scrolling_metadata));
    TRY(encoder.encode(filter_image_frames));
    return {};
}

template<>
ErrorOr<void> encode(Encoder& encoder, NonnullRefPtr<Web::Painting::DisplayList> const& display_list)
{
    return encoder.encode(*display_list);
}

template<>
ErrorOr<void> encode(Encoder& encoder, NonnullRefPtr<Web::Painting::DisplayList const> const& display_list)
{
    return encoder.encode(*display_list);
}

template<>
ErrorOr<NonnullRefPtr<Web::Painting::DisplayList>> decode(Decoder& decoder)
{
    auto wire_data = Web::Compositor::DisplayListWireData {
        .id = TRY(decoder.decode<u64>()),
        .payload = TRY(decoder.decode<Web::Compositor::SerializedPayload>()),
        .command_bytes_offset = TRY(decoder.decode<size_t>()),
        .command_bytes_size = TRY(decoder.decode<size_t>()),
        .accumulated_visual_context_tree_offset = TRY(decoder.decode<size_t>()),
        .accumulated_visual_context_tree_size = TRY(decoder.decode<size_t>()),
        .async_scrolling_metadata = TRY(decoder.decode<Optional<Web::Painting::DisplayList::AsyncScrollingMetadata>>()),
    };

    auto serialized_filter_image_frames = TRY(decoder.decode<Vector<Web::Compositor::DisplayListFilterImageFrameWireData>>());
    auto filter_image_frames = TRY(Web::Compositor::decode_display_list_filter_image_frames(serialized_filter_image_frames));
    Web::Compositor::DecodeFilterImage decode_filter_image = [&](u64 image_id) {
        auto image_frame = filter_image_frames.get(image_id);
        VERIFY(image_frame.has_value());
        return image_frame.value();
    };

    return Web::Compositor::create_display_list_from_wire_data(wire_data, decode_filter_image);
}

template<>
ErrorOr<NonnullRefPtr<Web::Painting::DisplayList const>> decode(Decoder& decoder)
{
    auto display_list = TRY(decoder.decode<NonnullRefPtr<Web::Painting::DisplayList>>());
    return NonnullRefPtr<Web::Painting::DisplayList const> { display_list };
}

}
