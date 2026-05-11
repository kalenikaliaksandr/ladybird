/*
 * Copyright (c) 2025, Lucien Fiorini <lucienfiorini@gmail.com>
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Filter.h>

#include <AK/MemoryStream.h>
#include <AK/NumericLimits.h>
#include <AK/StdLibExtras.h>

namespace Gfx {

static constexpr u32 serialized_filter_magic = 0x544c4647; // "GFLT"
static constexpr u32 serialized_filter_version = 1;

enum class SerializedFilterNodeType : u8 {
    Arithmetic,
    Compose,
    Blend,
    Flood,
    DisplacementMap,
    DropShadow,
    Blur,
    ColorFilter,
    ColorMatrix,
    ColorTable,
    Saturate,
    HueRotate,
    Image,
    Merge,
    Offset,
    Morphology,
    Turbulence,
};

class FilterSerializer {
public:
    ErrorOr<void> encode_u8(u8 value) { return m_stream.write_value(value); }
    ErrorOr<void> encode_i32(i32 value) { return m_stream.write_value(value); }
    ErrorOr<void> encode_u32(u32 value) { return m_stream.write_value(value); }
    ErrorOr<void> encode_u64(u64 value) { return m_stream.write_value(value); }
    ErrorOr<void> encode_float(float value) { return m_stream.write_value(value); }

    ErrorOr<void> encode_bool(bool value)
    {
        return encode_u8(value ? 1 : 0);
    }

    ErrorOr<void> encode_size(size_t value)
    {
        if (value > NumericLimits<u32>::max())
            return Error::from_string_literal("Gfx::Filter serialization: size exceeds u32");
        return encode_u32(static_cast<u32>(value));
    }

    ErrorOr<void> encode_index(size_t value)
    {
        if (value > NumericLimits<u32>::max())
            return Error::from_string_literal("Gfx::Filter serialization: node index exceeds u32");
        return encode_u32(static_cast<u32>(value));
    }

    ErrorOr<void> encode_node_reference(size_t node_index, Filter::NodeReference reference)
    {
        if (reference.distance == 0 || reference.distance > node_index)
            return Error::from_string_literal("Gfx::Filter serialization: invalid node reference");
        return encode_index(reference.distance);
    }

    ErrorOr<void> encode_optional_node_reference(size_t node_index, Optional<Filter::NodeReference> value)
    {
        TRY(encode_bool(value.has_value()));
        if (value.has_value())
            TRY(encode_node_reference(node_index, *value));
        return {};
    }

    template<typename T>
    ErrorOr<void> encode_enum(T value)
    {
        return encode_u32(static_cast<u32>(to_underlying(value)));
    }

    ErrorOr<void> encode_node_type(SerializedFilterNodeType type)
    {
        return encode_u8(to_underlying(type));
    }

    ErrorOr<void> encode_color(Gfx::Color color)
    {
        return encode_u32(color.value());
    }

    ErrorOr<void> encode_int_rect(Gfx::IntRect const& rect)
    {
        TRY(encode_i32(rect.x()));
        TRY(encode_i32(rect.y()));
        TRY(encode_i32(rect.width()));
        TRY(encode_i32(rect.height()));
        return {};
    }

    ErrorOr<void> encode_int_size(Gfx::IntSize const& size)
    {
        TRY(encode_i32(size.width()));
        TRY(encode_i32(size.height()));
        return {};
    }

    ErrorOr<void> encode_color_table(Optional<Array<u8, 256>> const& table)
    {
        TRY(encode_bool(table.has_value()));
        if (table.has_value())
            TRY(m_stream.write_until_depleted({ table->data(), table->size() }));
        return {};
    }

    ErrorOr<Vector<u8>> bytes() const
    {
        Vector<u8> buffer;
        TRY(buffer.try_resize(m_stream.used_buffer_size()));
        m_stream.peek_some(buffer.span());
        return buffer;
    }

private:
    AllocatingMemoryStream m_stream;
};

class FilterDeserializer {
public:
    explicit FilterDeserializer(ReadonlyBytes bytes)
        : m_stream(bytes)
    {
    }

    bool is_eof() const { return m_stream.is_eof(); }

    ErrorOr<u8> decode_u8() { return m_stream.read_value<u8>(); }
    ErrorOr<i32> decode_i32() { return m_stream.read_value<i32>(); }
    ErrorOr<u32> decode_u32() { return m_stream.read_value<u32>(); }
    ErrorOr<u64> decode_u64() { return m_stream.read_value<u64>(); }
    ErrorOr<float> decode_float() { return m_stream.read_value<float>(); }

    ErrorOr<bool> decode_bool()
    {
        auto value = TRY(decode_u8());
        if (value > 1)
            return Error::from_string_literal("Gfx::Filter deserialization: invalid bool");
        return value == 1;
    }

    ErrorOr<size_t> decode_index(size_t upper_bound)
    {
        auto index = TRY(decode_u32());
        if (index >= upper_bound)
            return Error::from_string_literal("Gfx::Filter deserialization: invalid node index");
        return static_cast<size_t>(index);
    }

    ErrorOr<Filter::NodeReference> decode_node_reference(size_t node_index)
    {
        auto distance = TRY(decode_u32());
        if (distance == 0 || distance > node_index)
            return Error::from_string_literal("Gfx::Filter deserialization: invalid node reference");
        return Filter::NodeReference { static_cast<size_t>(distance) };
    }

    ErrorOr<Optional<Filter::NodeReference>> decode_optional_node_reference(size_t node_index)
    {
        if (!TRY(decode_bool()))
            return Optional<Filter::NodeReference> {};
        return TRY(decode_node_reference(node_index));
    }

    template<typename T>
    ErrorOr<T> decode_enum()
    {
        return static_cast<T>(TRY(decode_u32()));
    }

    ErrorOr<SerializedFilterNodeType> decode_node_type()
    {
        auto type = TRY(decode_u8());
        switch (static_cast<SerializedFilterNodeType>(type)) {
        case SerializedFilterNodeType::Arithmetic:
        case SerializedFilterNodeType::Compose:
        case SerializedFilterNodeType::Blend:
        case SerializedFilterNodeType::Flood:
        case SerializedFilterNodeType::DisplacementMap:
        case SerializedFilterNodeType::DropShadow:
        case SerializedFilterNodeType::Blur:
        case SerializedFilterNodeType::ColorFilter:
        case SerializedFilterNodeType::ColorMatrix:
        case SerializedFilterNodeType::ColorTable:
        case SerializedFilterNodeType::Saturate:
        case SerializedFilterNodeType::HueRotate:
        case SerializedFilterNodeType::Image:
        case SerializedFilterNodeType::Merge:
        case SerializedFilterNodeType::Offset:
        case SerializedFilterNodeType::Morphology:
        case SerializedFilterNodeType::Turbulence:
            return static_cast<SerializedFilterNodeType>(type);
        }
        return Error::from_string_literal("Gfx::Filter deserialization: invalid node type");
    }

    ErrorOr<Gfx::Color> decode_color()
    {
        return Gfx::Color::from_bgra(TRY(decode_u32()));
    }

    ErrorOr<Gfx::IntRect> decode_int_rect()
    {
        auto x = TRY(decode_i32());
        auto y = TRY(decode_i32());
        auto width = TRY(decode_i32());
        auto height = TRY(decode_i32());
        return Gfx::IntRect { x, y, width, height };
    }

    ErrorOr<Gfx::IntSize> decode_int_size()
    {
        auto width = TRY(decode_i32());
        auto height = TRY(decode_i32());
        return Gfx::IntSize { width, height };
    }

    ErrorOr<Optional<Array<u8, 256>>> decode_color_table()
    {
        if (!TRY(decode_bool()))
            return Optional<Array<u8, 256>> {};
        Array<u8, 256> table;
        TRY(m_stream.read_until_filled({ table.data(), table.size() }));
        return table;
    }

private:
    FixedMemoryStream m_stream;
};

template<typename HeaderCallback, typename NodeCallback, typename ImageCallback>
static ErrorOr<size_t> decode_serialized_filter(ReadonlyBytes bytes, HeaderCallback&& header_callback, NodeCallback&& node_callback, ImageCallback&& image_callback)
{
    FilterDeserializer deserializer(bytes);
    if (TRY(deserializer.decode_u32()) != serialized_filter_magic)
        return Error::from_string_literal("Gfx::Filter deserialization: invalid magic");
    if (TRY(deserializer.decode_u32()) != serialized_filter_version)
        return Error::from_string_literal("Gfx::Filter deserialization: invalid version");

    auto node_count = TRY(deserializer.decode_u32());
    if (node_count == 0)
        return Error::from_string_literal("Gfx::Filter deserialization: empty filter");

    auto root_node_index = TRY(deserializer.decode_index(node_count));
    TRY(header_callback(node_count));

    for (u32 i = 0; i < node_count; ++i) {
        auto type = TRY(deserializer.decode_node_type());
        switch (type) {
        case SerializedFilterNodeType::Arithmetic: {
            auto background = TRY(deserializer.decode_optional_node_reference(i));
            auto foreground = TRY(deserializer.decode_optional_node_reference(i));
            auto k1 = TRY(deserializer.decode_float());
            auto k2 = TRY(deserializer.decode_float());
            auto k3 = TRY(deserializer.decode_float());
            auto k4 = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Arithmetic { background, foreground, k1, k2, k3, k4 } }));
            break;
        }
        case SerializedFilterNodeType::Compose: {
            auto outer = TRY(deserializer.decode_node_reference(i));
            auto inner = TRY(deserializer.decode_node_reference(i));
            TRY(node_callback(Filter::NodeData { Filter::Compose { outer, inner } }));
            break;
        }
        case SerializedFilterNodeType::Blend: {
            auto background = TRY(deserializer.decode_optional_node_reference(i));
            auto foreground = TRY(deserializer.decode_optional_node_reference(i));
            auto mode = TRY(deserializer.decode_enum<CompositingAndBlendingOperator>());
            TRY(node_callback(Filter::NodeData { Filter::Blend { background, foreground, mode } }));
            break;
        }
        case SerializedFilterNodeType::Flood: {
            auto color = TRY(deserializer.decode_color());
            auto opacity = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Flood { color, opacity } }));
            break;
        }
        case SerializedFilterNodeType::DisplacementMap: {
            auto color = TRY(deserializer.decode_optional_node_reference(i));
            auto displacement = TRY(deserializer.decode_optional_node_reference(i));
            auto scale = TRY(deserializer.decode_float());
            auto x_channel_selector = TRY(deserializer.decode_enum<ChannelSelector>());
            auto y_channel_selector = TRY(deserializer.decode_enum<ChannelSelector>());
            TRY(node_callback(Filter::NodeData { Filter::DisplacementMap { color, displacement, scale, x_channel_selector, y_channel_selector } }));
            break;
        }
        case SerializedFilterNodeType::DropShadow: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto offset_x = TRY(deserializer.decode_float());
            auto offset_y = TRY(deserializer.decode_float());
            auto radius = TRY(deserializer.decode_float());
            auto color = TRY(deserializer.decode_color());
            TRY(node_callback(Filter::NodeData { Filter::DropShadow { input, offset_x, offset_y, radius, color } }));
            break;
        }
        case SerializedFilterNodeType::Blur: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto radius_x = TRY(deserializer.decode_float());
            auto radius_y = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Blur { input, radius_x, radius_y } }));
            break;
        }
        case SerializedFilterNodeType::ColorFilter: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto filter_type = TRY(deserializer.decode_enum<ColorFilterType>());
            auto amount = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::ColorFilter { input, filter_type, amount } }));
            break;
        }
        case SerializedFilterNodeType::ColorMatrix: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            Array<float, 20> matrix;
            for (auto& value : matrix)
                value = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::ColorMatrix { input, matrix } }));
            break;
        }
        case SerializedFilterNodeType::ColorTable: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto a = TRY(deserializer.decode_color_table());
            auto r = TRY(deserializer.decode_color_table());
            auto g = TRY(deserializer.decode_color_table());
            auto b = TRY(deserializer.decode_color_table());
            TRY(node_callback(Filter::NodeData { Filter::ColorTable { input, a, r, g, b } }));
            break;
        }
        case SerializedFilterNodeType::Saturate: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto value = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Saturate { input, value } }));
            break;
        }
        case SerializedFilterNodeType::HueRotate: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto angle_degrees = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::HueRotate { input, angle_degrees } }));
            break;
        }
        case SerializedFilterNodeType::Image: {
            auto image_id = TRY(deserializer.decode_u64());
            auto src_rect = TRY(deserializer.decode_int_rect());
            auto dest_rect = TRY(deserializer.decode_int_rect());
            auto scaling_mode = TRY(deserializer.decode_enum<ScalingMode>());
            TRY(image_callback(image_id, src_rect, dest_rect, scaling_mode));
            break;
        }
        case SerializedFilterNodeType::Merge: {
            auto input_count = TRY(deserializer.decode_u32());
            Vector<Optional<Filter::NodeReference>> inputs;
            TRY(inputs.try_ensure_capacity(input_count));
            for (u32 input_index = 0; input_index < input_count; ++input_index)
                inputs.unchecked_append(TRY(deserializer.decode_optional_node_reference(i)));
            TRY(node_callback(Filter::NodeData { Filter::Merge { move(inputs) } }));
            break;
        }
        case SerializedFilterNodeType::Offset: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto dx = TRY(deserializer.decode_float());
            auto dy = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Offset { input, dx, dy } }));
            break;
        }
        case SerializedFilterNodeType::Morphology: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto morphology_operator = TRY(deserializer.decode_enum<MorphologyOperator>());
            auto radius_x = TRY(deserializer.decode_float());
            auto radius_y = TRY(deserializer.decode_float());
            TRY(node_callback(Filter::NodeData { Filter::Morphology { input, morphology_operator, radius_x, radius_y } }));
            break;
        }
        case SerializedFilterNodeType::Turbulence: {
            auto turbulence_type = TRY(deserializer.decode_enum<TurbulenceType>());
            auto base_frequency_x = TRY(deserializer.decode_float());
            auto base_frequency_y = TRY(deserializer.decode_float());
            auto num_octaves = TRY(deserializer.decode_i32());
            auto seed = TRY(deserializer.decode_float());
            auto tile_stitch_size = TRY(deserializer.decode_int_size());
            TRY(node_callback(Filter::NodeData { Filter::Turbulence { turbulence_type, base_frequency_x, base_frequency_y, num_octaves, seed, tile_stitch_size } }));
            break;
        }
        }
    }

    if (!deserializer.is_eof())
        return Error::from_string_literal("Gfx::Filter deserialization: trailing bytes");

    return root_node_index;
}

Filter::Filter(Vector<Node>&& nodes, size_t root_node_index)
    : m_nodes(move(nodes))
    , m_root_node_index(root_node_index)
{
    if (!m_nodes.is_empty())
        VERIFY(m_root_node_index < m_nodes.size());
}

ErrorOr<Vector<u8>> Filter::serialize_to_bytes(Function<ErrorOr<u64>(DecodedImageFrame const&)> const& encode_image) const
{
    VERIFY(!m_nodes.is_empty());

    FilterSerializer serializer;
    TRY(serializer.encode_u32(serialized_filter_magic));
    TRY(serializer.encode_u32(serialized_filter_version));
    TRY(serializer.encode_size(m_nodes.size()));
    TRY(serializer.encode_index(m_root_node_index));

    for (size_t node_index = 0; node_index < m_nodes.size(); ++node_index) {
        auto const& node = m_nodes[node_index];
        TRY(node.data.visit(
            [&](Arithmetic const& arithmetic) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Arithmetic));
                TRY(serializer.encode_optional_node_reference(node_index, arithmetic.background));
                TRY(serializer.encode_optional_node_reference(node_index, arithmetic.foreground));
                TRY(serializer.encode_float(arithmetic.k1));
                TRY(serializer.encode_float(arithmetic.k2));
                TRY(serializer.encode_float(arithmetic.k3));
                TRY(serializer.encode_float(arithmetic.k4));
                return {};
            },
            [&](Compose const& compose) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Compose));
                TRY(serializer.encode_node_reference(node_index, compose.outer));
                TRY(serializer.encode_node_reference(node_index, compose.inner));
                return {};
            },
            [&](Blend const& blend) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Blend));
                TRY(serializer.encode_optional_node_reference(node_index, blend.background));
                TRY(serializer.encode_optional_node_reference(node_index, blend.foreground));
                TRY(serializer.encode_enum(blend.mode));
                return {};
            },
            [&](Flood const& flood) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Flood));
                TRY(serializer.encode_color(flood.color));
                TRY(serializer.encode_float(flood.opacity));
                return {};
            },
            [&](DisplacementMap const& displacement_map) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::DisplacementMap));
                TRY(serializer.encode_optional_node_reference(node_index, displacement_map.color));
                TRY(serializer.encode_optional_node_reference(node_index, displacement_map.displacement));
                TRY(serializer.encode_float(displacement_map.scale));
                TRY(serializer.encode_enum(displacement_map.x_channel_selector));
                TRY(serializer.encode_enum(displacement_map.y_channel_selector));
                return {};
            },
            [&](DropShadow const& drop_shadow) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::DropShadow));
                TRY(serializer.encode_optional_node_reference(node_index, drop_shadow.input));
                TRY(serializer.encode_float(drop_shadow.offset_x));
                TRY(serializer.encode_float(drop_shadow.offset_y));
                TRY(serializer.encode_float(drop_shadow.radius));
                TRY(serializer.encode_color(drop_shadow.color));
                return {};
            },
            [&](Blur const& blur) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Blur));
                TRY(serializer.encode_optional_node_reference(node_index, blur.input));
                TRY(serializer.encode_float(blur.radius_x));
                TRY(serializer.encode_float(blur.radius_y));
                return {};
            },
            [&](ColorFilter const& color_filter) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::ColorFilter));
                TRY(serializer.encode_optional_node_reference(node_index, color_filter.input));
                TRY(serializer.encode_enum(color_filter.type));
                TRY(serializer.encode_float(color_filter.amount));
                return {};
            },
            [&](ColorMatrix const& color_matrix) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::ColorMatrix));
                TRY(serializer.encode_optional_node_reference(node_index, color_matrix.input));
                for (auto value : color_matrix.matrix)
                    TRY(serializer.encode_float(value));
                return {};
            },
            [&](ColorTable const& color_table) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::ColorTable));
                TRY(serializer.encode_optional_node_reference(node_index, color_table.input));
                TRY(serializer.encode_color_table(color_table.a));
                TRY(serializer.encode_color_table(color_table.r));
                TRY(serializer.encode_color_table(color_table.g));
                TRY(serializer.encode_color_table(color_table.b));
                return {};
            },
            [&](Saturate const& saturate) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Saturate));
                TRY(serializer.encode_optional_node_reference(node_index, saturate.input));
                TRY(serializer.encode_float(saturate.value));
                return {};
            },
            [&](HueRotate const& hue_rotate) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::HueRotate));
                TRY(serializer.encode_optional_node_reference(node_index, hue_rotate.input));
                TRY(serializer.encode_float(hue_rotate.angle_degrees));
                return {};
            },
            [&](Image const& image) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Image));
                TRY(serializer.encode_u64(TRY(encode_image(image.frame))));
                TRY(serializer.encode_int_rect(image.src_rect));
                TRY(serializer.encode_int_rect(image.dest_rect));
                TRY(serializer.encode_enum(image.scaling_mode));
                return {};
            },
            [&](Merge const& merge) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Merge));
                TRY(serializer.encode_size(merge.inputs.size()));
                for (auto input : merge.inputs)
                    TRY(serializer.encode_optional_node_reference(node_index, input));
                return {};
            },
            [&](Offset const& offset) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Offset));
                TRY(serializer.encode_optional_node_reference(node_index, offset.input));
                TRY(serializer.encode_float(offset.dx));
                TRY(serializer.encode_float(offset.dy));
                return {};
            },
            [&](Morphology const& morphology) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Morphology));
                TRY(serializer.encode_optional_node_reference(node_index, morphology.input));
                TRY(serializer.encode_enum(morphology.morphology_operator));
                TRY(serializer.encode_float(morphology.radius_x));
                TRY(serializer.encode_float(morphology.radius_y));
                return {};
            },
            [&](Turbulence const& turbulence) -> ErrorOr<void> {
                TRY(serializer.encode_node_type(SerializedFilterNodeType::Turbulence));
                TRY(serializer.encode_enum(turbulence.turbulence_type));
                TRY(serializer.encode_float(turbulence.base_frequency_x));
                TRY(serializer.encode_float(turbulence.base_frequency_y));
                TRY(serializer.encode_i32(turbulence.num_octaves));
                TRY(serializer.encode_float(turbulence.seed));
                TRY(serializer.encode_int_size(turbulence.tile_stitch_size));
                return {};
            }));
    }

    return TRY(serializer.bytes());
}

ErrorOr<Filter> Filter::from_serialized_bytes(ReadonlyBytes bytes, Function<ErrorOr<DecodedImageFrame>(u64)> const& decode_image)
{
    Vector<Node> nodes;
    auto root_node_index = TRY(decode_serialized_filter(
        bytes,
        [&](u32 node_count) -> ErrorOr<void> {
            TRY(nodes.try_ensure_capacity(node_count));
            return {};
        },
        [&](NodeData node_data) -> ErrorOr<void> {
            nodes.unchecked_append(Node { move(node_data) });
            return {};
        },
        [&](u64 image_id, IntRect const& src_rect, IntRect const& dest_rect, ScalingMode scaling_mode) -> ErrorOr<void> {
            auto frame = TRY(decode_image(image_id));
            nodes.unchecked_append(Node { NodeData { Image { move(frame), src_rect, dest_rect, scaling_mode } } });
            return {};
        }));

    return Filter(move(nodes), root_node_index);
}

ErrorOr<void> Filter::for_each_serialized_image_id(ReadonlyBytes bytes, Function<ErrorOr<void>(u64)> const& callback)
{
    TRY(decode_serialized_filter(
        bytes,
        [](u32) -> ErrorOr<void> {
            return {};
        },
        [](NodeData) -> ErrorOr<void> {
            return {};
        },
        [&](u64 image_id, IntRect const&, IntRect const&, ScalingMode) -> ErrorOr<void> {
            return callback(image_id);
        }));
    return {};
}

size_t Filter::append_filter(Filter const& filter)
{
    auto offset = m_nodes.size();
    m_nodes.ensure_capacity(m_nodes.size() + filter.m_nodes.size());
    for (auto node : filter.m_nodes)
        m_nodes.unchecked_append(move(node));
    return filter.m_root_node_index + offset;
}

size_t Filter::append_node(NodeData data)
{
    m_nodes.append(Node { move(data) });
    return m_nodes.size() - 1;
}

Optional<size_t> Filter::append_optional_filter(Filter& result, Optional<Filter const&> filter)
{
    if (!filter.has_value())
        return {};
    return result.append_filter(*filter);
}

Filter::NodeReference Filter::node_reference_from_index(size_t node_index, size_t input_index)
{
    VERIFY(input_index < node_index);
    return { node_index - input_index };
}

Optional<Filter::NodeReference> Filter::node_reference_from_index(size_t node_index, Optional<size_t> input_index)
{
    if (!input_index.has_value())
        return {};
    return node_reference_from_index(node_index, *input_index);
}

static Optional<Array<u8, 256>> copy_color_table(Optional<ReadonlyBytes> table)
{
    if (!table.has_value())
        return {};
    VERIFY(table->size() == 256);
    Array<u8, 256> copy;
    __builtin_memcpy(copy.data(), table->data(), copy.size());
    return copy;
}

Filter Filter::arithmetic(Optional<Filter const&> background, Optional<Filter const&> foreground, float k1, float k2, float k3, float k4)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto background_index = append_optional_filter(result, background);
    auto foreground_index = append_optional_filter(result, foreground);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Arithmetic {
        .background = node_reference_from_index(node_index, background_index),
        .foreground = node_reference_from_index(node_index, foreground_index),
        .k1 = k1,
        .k2 = k2,
        .k3 = k3,
        .k4 = k4,
    });
    return result;
}

Filter Filter::compose(Filter const& outer, Filter const& inner)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto outer_index = result.append_filter(outer);
    auto inner_index = result.append_filter(inner);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Compose {
        .outer = node_reference_from_index(node_index, outer_index),
        .inner = node_reference_from_index(node_index, inner_index),
    });
    return result;
}

Filter Filter::blend(Optional<Filter const&> background, Optional<Filter const&> foreground, Gfx::CompositingAndBlendingOperator mode)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto background_index = append_optional_filter(result, background);
    auto foreground_index = append_optional_filter(result, foreground);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Blend {
        .background = node_reference_from_index(node_index, background_index),
        .foreground = node_reference_from_index(node_index, foreground_index),
        .mode = mode,
    });
    return result;
}

Filter Filter::blur(float radius_x, float radius_y, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Blur {
        .input = node_reference_from_index(node_index, input_index),
        .radius_x = radius_x,
        .radius_y = radius_y,
    });
    return result;
}

Filter Filter::flood(Gfx::Color color, float opacity)
{
    Vector<Node> nodes;
    nodes.append(Node { Flood { color, opacity } });
    return Filter(move(nodes), 0);
}

Filter Filter::displacement_map(Optional<Filter const&> color, Optional<Filter const&> displacement, float scale, ChannelSelector x_channel_selector, ChannelSelector y_channel_selector)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto color_index = append_optional_filter(result, color);
    auto displacement_index = append_optional_filter(result, displacement);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(DisplacementMap {
        .color = node_reference_from_index(node_index, color_index),
        .displacement = node_reference_from_index(node_index, displacement_index),
        .scale = scale,
        .x_channel_selector = x_channel_selector,
        .y_channel_selector = y_channel_selector,
    });
    return result;
}

Filter Filter::drop_shadow(float offset_x, float offset_y, float radius, Gfx::Color color, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(DropShadow {
        .input = node_reference_from_index(node_index, input_index),
        .offset_x = offset_x,
        .offset_y = offset_y,
        .radius = radius,
        .color = color,
    });
    return result;
}

Filter Filter::color(ColorFilterType type, float amount, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(ColorFilter {
        .input = node_reference_from_index(node_index, input_index),
        .type = type,
        .amount = amount,
    });
    return result;
}

Filter Filter::color_matrix(float matrix[20], Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    Array<float, 20> matrix_copy;
    __builtin_memcpy(matrix_copy.data(), matrix, matrix_copy.size() * sizeof(float));
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(ColorMatrix {
        .input = node_reference_from_index(node_index, input_index),
        .matrix = matrix_copy,
    });
    return result;
}

Filter Filter::color_table(Optional<ReadonlyBytes> a, Optional<ReadonlyBytes> r, Optional<ReadonlyBytes> g, Optional<ReadonlyBytes> b, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(ColorTable {
        .input = node_reference_from_index(node_index, input_index),
        .a = copy_color_table(a),
        .r = copy_color_table(r),
        .g = copy_color_table(g),
        .b = copy_color_table(b),
    });
    return result;
}

Filter Filter::saturate(float value, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Saturate {
        .input = node_reference_from_index(node_index, input_index),
        .value = value,
    });
    return result;
}

Filter Filter::hue_rotate(float angle_degrees, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(HueRotate {
        .input = node_reference_from_index(node_index, input_index),
        .angle_degrees = angle_degrees,
    });
    return result;
}

Filter Filter::image(Gfx::DecodedImageFrame const& frame, Gfx::IntRect const& src_rect, Gfx::IntRect const& dest_rect, Gfx::ScalingMode scaling_mode)
{
    Vector<Node> nodes;
    nodes.append(Node { Image {
        .frame = frame,
        .src_rect = src_rect,
        .dest_rect = dest_rect,
        .scaling_mode = scaling_mode,
    } });
    return Filter(move(nodes), 0);
}

Filter Filter::merge(Vector<Optional<Filter>> const& inputs)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    Vector<Optional<size_t>> input_indices;
    input_indices.ensure_capacity(inputs.size());
    for (auto const& input : inputs) {
        if (input.has_value())
            input_indices.unchecked_append(result.append_filter(input.value()));
        else
            input_indices.unchecked_append({});
    }
    auto node_index = result.m_nodes.size();
    Vector<Optional<NodeReference>> input_references;
    input_references.ensure_capacity(input_indices.size());
    for (auto input_index : input_indices)
        input_references.unchecked_append(node_reference_from_index(node_index, input_index));
    result.m_root_node_index = result.append_node(Merge {
        .inputs = move(input_references),
    });
    return result;
}

Filter Filter::erode(float radius_x, float radius_y, Optional<Filter> const& input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = input.has_value() ? Optional<size_t>(result.append_filter(input.value())) : Optional<size_t> {};
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Morphology {
        .input = node_reference_from_index(node_index, input_index),
        .morphology_operator = Gfx::MorphologyOperator::Erode,
        .radius_x = radius_x,
        .radius_y = radius_y,
    });
    return result;
}

Filter Filter::dilate(float radius_x, float radius_y, Optional<Filter> const& input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = input.has_value() ? Optional<size_t>(result.append_filter(input.value())) : Optional<size_t> {};
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Morphology {
        .input = node_reference_from_index(node_index, input_index),
        .morphology_operator = Gfx::MorphologyOperator::Dilate,
        .radius_x = radius_x,
        .radius_y = radius_y,
    });
    return result;
}

Filter Filter::offset(float dx, float dy, Optional<Filter const&> input)
{
    Vector<Node> nodes;
    Filter result(move(nodes), 0);
    auto input_index = append_optional_filter(result, input);
    auto node_index = result.m_nodes.size();
    result.m_root_node_index = result.append_node(Offset {
        .input = node_reference_from_index(node_index, input_index),
        .dx = dx,
        .dy = dy,
    });
    return result;
}

Filter Filter::turbulence(TurbulenceType turbulence_type, float base_frequency_x, float base_frequency_y, i32 num_octaves, float seed, Gfx::IntSize const& tile_stitch_size)
{
    Vector<Node> nodes;
    nodes.append(Node { Turbulence {
        .turbulence_type = turbulence_type,
        .base_frequency_x = base_frequency_x,
        .base_frequency_y = base_frequency_y,
        .num_octaves = num_octaves,
        .seed = seed,
        .tile_stitch_size = tile_stitch_size,
    } });
    return Filter(move(nodes), 0);
}

}
