/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <AK/Queue.h>
#include <LibGfx/Filter.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Message.h>
#include <LibWeb/Compositor/VisualContextSerialization.h>

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollFrameIndex const& index)
{
    TRY(encoder.encode(index.value()));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollFrameIndex> decode(Decoder& decoder)
{
    return Web::Painting::ScrollFrameIndex { TRY(decoder.decode<size_t>()) };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::CornerRadius const& radius)
{
    TRY(encoder.encode(radius.horizontal_radius));
    TRY(encoder.encode(radius.vertical_radius));
    return {};
}

template<>
ErrorOr<Gfx::CornerRadius> decode(Decoder& decoder)
{
    return Gfx::CornerRadius {
        .horizontal_radius = TRY(decoder.decode<int>()),
        .vertical_radius = TRY(decoder.decode<int>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::CornerRadii const& corner_radii)
{
    TRY(encoder.encode(corner_radii.top_left));
    TRY(encoder.encode(corner_radii.top_right));
    TRY(encoder.encode(corner_radii.bottom_right));
    TRY(encoder.encode(corner_radii.bottom_left));
    return {};
}

template<>
ErrorOr<Gfx::CornerRadii> decode(Decoder& decoder)
{
    return Gfx::CornerRadii {
        .top_left = TRY(decoder.decode<Gfx::CornerRadius>()),
        .top_right = TRY(decoder.decode<Gfx::CornerRadius>()),
        .bottom_right = TRY(decoder.decode<Gfx::CornerRadius>()),
        .bottom_left = TRY(decoder.decode<Gfx::CornerRadius>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::FloatMatrix4x4 const& matrix)
{
    for (size_t row = 0; row < 4; ++row) {
        for (size_t column = 0; column < 4; ++column)
            TRY(encoder.encode(matrix[row, column]));
    }
    return {};
}

template<>
ErrorOr<Gfx::FloatMatrix4x4> decode(Decoder& decoder)
{
    float elements[4][4] {};
    for (size_t row = 0; row < 4; ++row) {
        for (size_t column = 0; column < 4; ++column)
            elements[row][column] = TRY(decoder.decode<float>());
    }

    return Gfx::FloatMatrix4x4 {
        elements[0][0], elements[0][1], elements[0][2], elements[0][3],
        elements[1][0], elements[1][1], elements[1][2], elements[1][3],
        elements[2][0], elements[2][1], elements[2][2], elements[2][3],
        elements[3][0], elements[3][1], elements[3][2], elements[3][3]
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::Path const& path)
{
    TRY(encoder.encode(path.serialize_to_bytes()));
    return {};
}

template<>
ErrorOr<Gfx::Path> decode(Decoder& decoder)
{
    auto bytes = TRY(decoder.decode<Vector<u8>>());
    return Gfx::Path::from_serialized_bytes(bytes.span());
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollData const& data)
{
    TRY(encoder.encode(data.scroll_frame_index));
    TRY(encoder.encode(data.is_sticky));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollData> decode(Decoder& decoder)
{
    return Web::Painting::ScrollData {
        .scroll_frame_index = TRY(decoder.decode<Web::Painting::ScrollFrameIndex>()),
        .is_sticky = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ClipData const& data)
{
    TRY(encoder.encode(data.rect));
    TRY(encoder.encode(data.corner_radii));
    return {};
}

template<>
ErrorOr<Web::Painting::ClipData> decode(Decoder& decoder)
{
    auto rect = TRY(decoder.decode<Web::DevicePixelRect>());
    auto corner_radii = TRY(decoder.decode<Gfx::CornerRadii>());
    return Web::Painting::ClipData { rect, corner_radii };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::TransformData const& data)
{
    TRY(encoder.encode(data.matrix));
    TRY(encoder.encode(data.origin));
    return {};
}

template<>
ErrorOr<Web::Painting::TransformData> decode(Decoder& decoder)
{
    return Web::Painting::TransformData {
        .matrix = TRY(decoder.decode<Gfx::FloatMatrix4x4>()),
        .origin = TRY(decoder.decode<Gfx::FloatPoint>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::PerspectiveData const& data)
{
    TRY(encoder.encode(data.matrix));
    return {};
}

template<>
ErrorOr<Web::Painting::PerspectiveData> decode(Decoder& decoder)
{
    return Web::Painting::PerspectiveData {
        .matrix = TRY(decoder.decode<Gfx::FloatMatrix4x4>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ClipPathData const& data)
{
    TRY(encoder.encode(data.path));
    TRY(encoder.encode(data.bounding_rect));
    TRY(encoder.encode(data.fill_rule));
    return {};
}

template<>
ErrorOr<Web::Painting::ClipPathData> decode(Decoder& decoder)
{
    return Web::Painting::ClipPathData {
        .path = TRY(decoder.decode<Gfx::Path>()),
        .bounding_rect = TRY(decoder.decode<Web::DevicePixelRect>()),
        .fill_rule = TRY(decoder.decode<Gfx::WindingRule>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedEffectsData const& data)
{
    TRY(encoder.encode(data.opacity));
    TRY(encoder.encode(data.blend_mode));
    TRY(encoder.encode(data.serialized_filter));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedEffectsData> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedEffectsData {
        .opacity = TRY(decoder.decode<float>()),
        .blend_mode = TRY(decoder.decode<Gfx::CompositingAndBlendingOperator>()),
        .serialized_filter = TRY(decoder.decode<Optional<ByteBuffer>>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::ScrollCompensation const& data)
{
    TRY(encoder.encode(data.scroll_frame_index));
    return {};
}

template<>
ErrorOr<Web::Painting::ScrollCompensation> decode(Decoder& decoder)
{
    return Web::Painting::ScrollCompensation {
        .scroll_frame_index = TRY(decoder.decode<Web::Painting::ScrollFrameIndex>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedVisualContextNode const& node)
{
    TRY(encoder.encode(node.parent_index));
    TRY(encoder.encode(node.data));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedVisualContextNode> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedVisualContextNode {
        .parent_index = TRY(decoder.decode<size_t>()),
        .data = TRY(decoder.decode<Web::Compositor::SerializedVisualContextData>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Compositor::SerializedAccumulatedVisualContextTree const& tree)
{
    TRY(encoder.encode(tree.nodes));
    return {};
}

template<>
ErrorOr<Web::Compositor::SerializedAccumulatedVisualContextTree> decode(Decoder& decoder)
{
    return Web::Compositor::SerializedAccumulatedVisualContextTree {
        .nodes = TRY(decoder.decode<Vector<Web::Compositor::SerializedVisualContextNode>>()),
    };
}

}

namespace Web::Compositor {

static SerializedVisualContextData serialize_visual_context_data(Painting::VisualContextData const& data, EncodeFilterImage const& encode_filter_image)
{
    return data.visit(
        [](Painting::ScrollData const& data) -> SerializedVisualContextData {
            return data;
        },
        [](Painting::ClipData const& data) -> SerializedVisualContextData {
            return data;
        },
        [](Painting::TransformData const& data) -> SerializedVisualContextData {
            return data;
        },
        [](Painting::PerspectiveData const& data) -> SerializedVisualContextData {
            return data;
        },
        [](Painting::ClipPathData const& data) -> SerializedVisualContextData {
            return data;
        },
        [&](Painting::EffectsData const& data) -> SerializedVisualContextData {
            Optional<ByteBuffer> serialized_filter;
            if (data.gfx_filter.has_value())
                serialized_filter = Gfx::serialize_filter(*data.gfx_filter, encode_filter_image);
            return SerializedEffectsData {
                .opacity = data.opacity,
                .blend_mode = data.blend_mode,
                .serialized_filter = move(serialized_filter),
            };
        },
        [](Painting::ScrollCompensation const& data) -> SerializedVisualContextData {
            return data;
        });
}

ErrorOr<SerializedAccumulatedVisualContextTree> serialize_accumulated_visual_context_tree(
    Painting::AccumulatedVisualContextTree const& tree,
    EncodeFilterImage const& encode_filter_image)
{
    SerializedAccumulatedVisualContextTree serialized_tree;
    serialized_tree.nodes.ensure_capacity(tree.node_count() - 1);

    for (size_t i = 1; i < tree.node_count(); ++i) {
        auto const& node = tree.node_at(Painting::VisualContextIndex { i });
        serialized_tree.nodes.unchecked_append({
            .parent_index = node.parent_index.value(),
            .data = serialize_visual_context_data(node.data, encode_filter_image),
        });
    }

    return serialized_tree;
}

static ErrorOr<Painting::VisualContextData> deserialize_visual_context_data(SerializedVisualContextData const& data, DecodeFilterImage const& decode_filter_image)
{
    return data.visit(
        [](Painting::ScrollData const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        },
        [](Painting::ClipData const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        },
        [](Painting::TransformData const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        },
        [](Painting::PerspectiveData const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        },
        [](Painting::ClipPathData const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        },
        [&](SerializedEffectsData const& data) -> ErrorOr<Painting::VisualContextData> {
            Optional<Gfx::Filter> filter;
            if (data.serialized_filter.has_value())
                filter = Gfx::deserialize_filter(data.serialized_filter->bytes(), decode_filter_image);
            return Painting::EffectsData {
                .opacity = data.opacity,
                .blend_mode = data.blend_mode,
                .gfx_filter = move(filter),
            };
        },
        [](Painting::ScrollCompensation const& data) -> ErrorOr<Painting::VisualContextData> {
            return data;
        });
}

ErrorOr<NonnullRefPtr<Painting::AccumulatedVisualContextTree>> deserialize_accumulated_visual_context_tree(
    SerializedAccumulatedVisualContextTree const& serialized_tree,
    DecodeFilterImage const& decode_filter_image)
{
    auto tree = Painting::AccumulatedVisualContextTree::create();

    for (size_t i = 0; i < serialized_tree.nodes.size(); ++i) {
        auto const& node = serialized_tree.nodes[i];
        auto reconstructed_index = i + 1;
        if (node.parent_index >= reconstructed_index)
            return Error::from_string_literal("Serialized visual context node parent must precede child");

        tree->append(TRY(deserialize_visual_context_data(node.data, decode_filter_image)), Painting::VisualContextIndex { node.parent_index });
    }

    return tree;
}

ErrorOr<ByteBuffer> encode_serialized_accumulated_visual_context_tree(SerializedAccumulatedVisualContextTree const& serialized_tree)
{
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };
    TRY(encoder.encode(serialized_tree));
    return ByteBuffer::copy(buffer.data().span());
}

ErrorOr<SerializedAccumulatedVisualContextTree> decode_serialized_accumulated_visual_context_tree(ReadonlyBytes bytes)
{
    FixedMemoryStream stream { bytes };
    Queue<IPC::Attachment> attachments;
    IPC::Decoder decoder { stream, attachments };
    auto serialized_tree = TRY(decoder.decode<SerializedAccumulatedVisualContextTree>());
    if (!stream.is_eof())
        return Error::from_string_literal("Serialized visual context tree has trailing bytes");
    return serialized_tree;
}

}
