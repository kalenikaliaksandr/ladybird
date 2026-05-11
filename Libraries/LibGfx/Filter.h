/*
 * Copyright (c) 2024-2025, Lucien Fiorini <lucienfiorini@gmail.com>
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Array.h>
#include <AK/Error.h>
#include <AK/Function.h>
#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Types.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/Color.h>
#include <LibGfx/CompositingAndBlendingOperator.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Forward.h>
#include <LibGfx/MorphologyOperator.h>
#include <LibGfx/Rect.h>
#include <LibGfx/ScalingMode.h>

namespace Gfx {

enum class ChannelSelector {
    Red,
    Green,
    Blue,
    Alpha,
};

enum class ColorFilterType {
    Brightness,
    Contrast,
    Grayscale,
    Invert,
    Opacity,
    Saturate,
    Sepia
};

enum class TurbulenceType {
    FractalNoise,
    Turbulence,
};

class Filter {
public:
    struct NodeReference {
        size_t distance { 0 };
    };

    struct Arithmetic {
        Optional<NodeReference> background;
        Optional<NodeReference> foreground;
        float k1 { 0 };
        float k2 { 0 };
        float k3 { 0 };
        float k4 { 0 };
    };

    struct Compose {
        NodeReference outer;
        NodeReference inner;
    };

    struct Blend {
        Optional<NodeReference> background;
        Optional<NodeReference> foreground;
        CompositingAndBlendingOperator mode { CompositingAndBlendingOperator::Normal };
    };

    struct Flood {
        Gfx::Color color;
        float opacity { 1 };
    };

    struct DisplacementMap {
        Optional<NodeReference> color;
        Optional<NodeReference> displacement;
        float scale { 0 };
        ChannelSelector x_channel_selector { ChannelSelector::Alpha };
        ChannelSelector y_channel_selector { ChannelSelector::Alpha };
    };

    struct DropShadow {
        Optional<NodeReference> input;
        float offset_x { 0 };
        float offset_y { 0 };
        float radius { 0 };
        Gfx::Color color;
    };

    struct Blur {
        Optional<NodeReference> input;
        float radius_x { 0 };
        float radius_y { 0 };
    };

    struct ColorFilter {
        Optional<NodeReference> input;
        ColorFilterType type { ColorFilterType::Brightness };
        float amount { 1 };
    };

    struct ColorMatrix {
        Optional<NodeReference> input;
        Array<float, 20> matrix;
    };

    struct ColorTable {
        Optional<NodeReference> input;
        Optional<Array<u8, 256>> a;
        Optional<Array<u8, 256>> r;
        Optional<Array<u8, 256>> g;
        Optional<Array<u8, 256>> b;
    };

    struct Saturate {
        Optional<NodeReference> input;
        float value { 1 };
    };

    struct HueRotate {
        Optional<NodeReference> input;
        float angle_degrees { 0 };
    };

    struct Image {
        Gfx::DecodedImageFrame frame;
        Gfx::IntRect src_rect;
        Gfx::IntRect dest_rect;
        Gfx::ScalingMode scaling_mode { Gfx::ScalingMode::NearestNeighbor };
    };

    struct Merge {
        Vector<Optional<NodeReference>> inputs;
    };

    struct Offset {
        Optional<NodeReference> input;
        float dx { 0 };
        float dy { 0 };
    };

    struct Morphology {
        Optional<NodeReference> input;
        Gfx::MorphologyOperator morphology_operator { Gfx::MorphologyOperator::Erode };
        float radius_x { 0 };
        float radius_y { 0 };
    };

    struct Turbulence {
        TurbulenceType turbulence_type { TurbulenceType::Turbulence };
        float base_frequency_x { 0 };
        float base_frequency_y { 0 };
        i32 num_octaves { 0 };
        float seed { 0 };
        Gfx::IntSize tile_stitch_size;
    };

    using NodeData = Variant<Arithmetic, Compose, Blend, Flood, DisplacementMap, DropShadow, Blur, ColorFilter, ColorMatrix, ColorTable, Saturate, HueRotate, Image, Merge, Offset, Morphology, Turbulence>;

    struct Node {
        NodeData data;
    };

    Filter(Filter const&) = default;
    Filter(Filter&&) = default;
    Filter& operator=(Filter const&) = default;
    Filter& operator=(Filter&&) = default;

    ~Filter() = default;

    static Filter arithmetic(Optional<Filter const&> background, Optional<Filter const&> foreground, float k1, float k2, float k3, float k4);
    static Filter compose(Filter const& outer, Filter const& inner);
    static Filter blend(Optional<Filter const&> background, Optional<Filter const&> foreground, CompositingAndBlendingOperator mode);
    static Filter flood(Gfx::Color color, float opacity);
    static Filter displacement_map(Optional<Filter const&> color, Optional<Filter const&> displacement, float scale, ChannelSelector x_channel_selector, ChannelSelector y_channel_selector);
    static Filter drop_shadow(float offset_x, float offset_y, float radius, Gfx::Color color, Optional<Filter const&> input = {});
    static Filter blur(float radius_x, float radius_y, Optional<Filter const&> input = {});
    static Filter color(ColorFilterType type, float amount, Optional<Filter const&> input = {});
    static Filter color_matrix(float matrix[20], Optional<Filter const&> input = {});
    static Filter color_table(Optional<ReadonlyBytes> a, Optional<ReadonlyBytes> r, Optional<ReadonlyBytes> g, Optional<ReadonlyBytes> b, Optional<Filter const&> input = {});
    static Filter saturate(float value, Optional<Filter const&> input = {});
    static Filter hue_rotate(float angle_degrees, Optional<Filter const&> input = {});
    static Filter image(Gfx::DecodedImageFrame const&, Gfx::IntRect const& src_rect, Gfx::IntRect const& dest_rect, Gfx::ScalingMode scaling_mode);
    static Filter merge(Vector<Optional<Filter>> const&);
    static Filter offset(float dx, float dy, Optional<Filter const&> input = {});
    static Filter erode(float radius_x, float radius_y, Optional<Filter> const& input = {});
    static Filter dilate(float radius_x, float radius_y, Optional<Filter> const& input = {});
    static Filter turbulence(TurbulenceType turbulence_type, float base_frequency_x, float base_frequency_y, i32 num_octaves, float seed, Gfx::IntSize const& tile_stitch_size);

    static ErrorOr<Filter> from_serialized_bytes(ReadonlyBytes, Function<ErrorOr<DecodedImageFrame>(u64)> const& decode_image);
    static ErrorOr<void> for_each_serialized_image_id(ReadonlyBytes, Function<ErrorOr<void>(u64)> const&);

    ErrorOr<Vector<u8>> serialize_to_bytes(Function<ErrorOr<u64>(DecodedImageFrame const&)> const& encode_image) const;
    ReadonlySpan<Node> nodes() const { return m_nodes; }
    size_t root_node_index() const { return m_root_node_index; }

private:
    Filter(Vector<Node>&&, size_t root_node_index);

    size_t append_filter(Filter const&);
    size_t append_node(NodeData);
    static Optional<size_t> append_optional_filter(Filter&, Optional<Filter const&>);
    static NodeReference node_reference_from_index(size_t node_index, size_t input_index);
    static Optional<NodeReference> node_reference_from_index(size_t node_index, Optional<size_t> input_index);

    Vector<Node> m_nodes;
    size_t m_root_node_index { 0 };
};

}
