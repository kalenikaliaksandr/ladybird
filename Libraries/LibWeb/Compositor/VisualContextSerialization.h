/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/Error.h>
#include <AK/Function.h>
#include <AK/Optional.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>

namespace Web::Compositor {

struct SerializedEffectsData {
    float opacity { 1.0f };
    Gfx::CompositingAndBlendingOperator blend_mode { Gfx::CompositingAndBlendingOperator::Normal };
    Optional<ByteBuffer> serialized_filter;
};

using SerializedVisualContextData = Variant<
    Painting::ScrollData,
    Painting::ClipData,
    Painting::TransformData,
    Painting::PerspectiveData,
    Painting::ClipPathData,
    SerializedEffectsData,
    Painting::ScrollCompensation>;

struct SerializedVisualContextNode {
    size_t parent_index { 0 };
    SerializedVisualContextData data;
};

struct SerializedAccumulatedVisualContextTree {
    Vector<SerializedVisualContextNode> nodes;
};

using EncodeFilterImage = Function<u64(Gfx::DecodedImageFrame const&)>;
using DecodeFilterImage = Function<Gfx::DecodedImageFrame(u64)>;

WEB_API ErrorOr<SerializedAccumulatedVisualContextTree> serialize_accumulated_visual_context_tree(
    Painting::AccumulatedVisualContextTree const&,
    EncodeFilterImage const&);
WEB_API ErrorOr<NonnullRefPtr<Painting::AccumulatedVisualContextTree>> deserialize_accumulated_visual_context_tree(
    SerializedAccumulatedVisualContextTree const&,
    DecodeFilterImage const&);

}
