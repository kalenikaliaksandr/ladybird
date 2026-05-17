/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Filter.h>
#include <LibWeb/Compositor/VisualContextSerialization.h>

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

}
