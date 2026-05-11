/*
 * Copyright (c) 2025, Lucien Fiorini <lucienfiorini@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Array.h>
#include <AK/Atomic.h>
#include <AK/Math.h>
#include <AK/MemoryStream.h>
#include <AK/NumericLimits.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Filter.h>
#include <LibGfx/FilterImpl.h>
#include <LibGfx/MorphologyOperator.h>
#include <LibGfx/SkiaUtils.h>
#include <core/SkBlendMode.h>
#include <core/SkColorFilter.h>
#include <core/SkScalar.h>
#include <effects/SkColorMatrix.h>
#include <effects/SkImageFilters.h>
#include <effects/SkPerlinNoiseShader.h>

namespace Gfx {

static Atomic<u64> s_next_id { 1 };

using Impl = FilterImpl;

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

    ErrorOr<void> append_bytes(ReadonlyBytes bytes)
    {
        return m_stream.write_until_depleted(bytes);
    }

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

    ErrorOr<void> encode_node_reference(size_t node_index, size_t input_index)
    {
        if (input_index >= node_index)
            return Error::from_string_literal("Gfx::Filter serialization: invalid node reference");
        return encode_index(node_index - input_index);
    }

    ErrorOr<void> encode_optional_node_reference(size_t node_index, Optional<size_t> value)
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

    ErrorOr<void> encode_color_table(Optional<ReadonlyBytes> table)
    {
        TRY(encode_bool(table.has_value()));
        if (table.has_value()) {
            VERIFY(table->size() == 256);
            TRY(m_stream.write_until_depleted(*table));
        }
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
    size_t offset() const { return m_stream.offset(); }

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

    ErrorOr<size_t> decode_node_reference(size_t node_index)
    {
        auto distance = TRY(decode_u32());
        if (distance == 0 || distance > node_index)
            return Error::from_string_literal("Gfx::Filter deserialization: invalid node reference");
        return node_index - static_cast<size_t>(distance);
    }

    ErrorOr<Optional<size_t>> decode_optional_node_reference(size_t node_index)
    {
        if (!TRY(decode_bool()))
            return Optional<size_t> {};
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

struct SerializedFilterVisitor {
    ErrorOr<void> header(u32, u32) { return {}; }
    ErrorOr<void> node_bytes(SerializedFilterNodeType, ReadonlyBytes) { return {}; }
    ErrorOr<void> arithmetic(u32, Optional<size_t>, Optional<size_t>, float, float, float, float) { return {}; }
    ErrorOr<void> compose(u32, size_t, size_t) { return {}; }
    ErrorOr<void> blend(u32, Optional<size_t>, Optional<size_t>, CompositingAndBlendingOperator) { return {}; }
    ErrorOr<void> flood(u32, Gfx::Color, float) { return {}; }
    ErrorOr<void> displacement_map(u32, Optional<size_t>, Optional<size_t>, float, ChannelSelector, ChannelSelector) { return {}; }
    ErrorOr<void> drop_shadow(u32, Optional<size_t>, float, float, float, Gfx::Color) { return {}; }
    ErrorOr<void> blur(u32, Optional<size_t>, float, float) { return {}; }
    ErrorOr<void> color_filter(u32, Optional<size_t>, ColorFilterType, float) { return {}; }
    ErrorOr<void> color_matrix(u32, Optional<size_t>, Array<float, 20> const&) { return {}; }
    ErrorOr<void> color_table(u32, Optional<size_t>, Optional<Array<u8, 256>> const&, Optional<Array<u8, 256>> const&, Optional<Array<u8, 256>> const&, Optional<Array<u8, 256>> const&) { return {}; }
    ErrorOr<void> saturate(u32, Optional<size_t>, float) { return {}; }
    ErrorOr<void> hue_rotate(u32, Optional<size_t>, float) { return {}; }
    ErrorOr<void> image(u32, u64, Gfx::IntRect const&, Gfx::IntRect const&, Gfx::ScalingMode) { return {}; }
    ErrorOr<void> merge(u32, Vector<Optional<size_t>>&&) { return {}; }
    ErrorOr<void> offset(u32, Optional<size_t>, float, float) { return {}; }
    ErrorOr<void> morphology(u32, Optional<size_t>, Gfx::MorphologyOperator, float, float) { return {}; }
    ErrorOr<void> turbulence(u32, TurbulenceType, float, float, i32, float, Gfx::IntSize const&) { return {}; }
};

template<typename Visitor>
static ErrorOr<u32> decode_serialized_filter(ReadonlyBytes bytes, Visitor& visitor)
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
    TRY(visitor.header(node_count, root_node_index));

    for (u32 i = 0; i < node_count; ++i) {
        auto node_start = deserializer.offset();
        auto type = TRY(deserializer.decode_node_type());
        switch (type) {
        case SerializedFilterNodeType::Arithmetic: {
            auto background = TRY(deserializer.decode_optional_node_reference(i));
            auto foreground = TRY(deserializer.decode_optional_node_reference(i));
            auto k1 = TRY(deserializer.decode_float());
            auto k2 = TRY(deserializer.decode_float());
            auto k3 = TRY(deserializer.decode_float());
            auto k4 = TRY(deserializer.decode_float());
            TRY(visitor.arithmetic(i, background, foreground, k1, k2, k3, k4));
            break;
        }
        case SerializedFilterNodeType::Compose: {
            auto outer = TRY(deserializer.decode_node_reference(i));
            auto inner = TRY(deserializer.decode_node_reference(i));
            TRY(visitor.compose(i, outer, inner));
            break;
        }
        case SerializedFilterNodeType::Blend: {
            auto background = TRY(deserializer.decode_optional_node_reference(i));
            auto foreground = TRY(deserializer.decode_optional_node_reference(i));
            auto mode = TRY(deserializer.decode_enum<CompositingAndBlendingOperator>());
            TRY(visitor.blend(i, background, foreground, mode));
            break;
        }
        case SerializedFilterNodeType::Flood: {
            auto color = TRY(deserializer.decode_color());
            auto opacity = TRY(deserializer.decode_float());
            TRY(visitor.flood(i, color, opacity));
            break;
        }
        case SerializedFilterNodeType::DisplacementMap: {
            auto color = TRY(deserializer.decode_optional_node_reference(i));
            auto displacement = TRY(deserializer.decode_optional_node_reference(i));
            auto scale = TRY(deserializer.decode_float());
            auto x_channel_selector = TRY(deserializer.decode_enum<ChannelSelector>());
            auto y_channel_selector = TRY(deserializer.decode_enum<ChannelSelector>());
            TRY(visitor.displacement_map(i, color, displacement, scale, x_channel_selector, y_channel_selector));
            break;
        }
        case SerializedFilterNodeType::DropShadow: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto offset_x = TRY(deserializer.decode_float());
            auto offset_y = TRY(deserializer.decode_float());
            auto radius = TRY(deserializer.decode_float());
            auto color = TRY(deserializer.decode_color());
            TRY(visitor.drop_shadow(i, input, offset_x, offset_y, radius, color));
            break;
        }
        case SerializedFilterNodeType::Blur: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto radius_x = TRY(deserializer.decode_float());
            auto radius_y = TRY(deserializer.decode_float());
            TRY(visitor.blur(i, input, radius_x, radius_y));
            break;
        }
        case SerializedFilterNodeType::ColorFilter: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto filter_type = TRY(deserializer.decode_enum<ColorFilterType>());
            auto amount = TRY(deserializer.decode_float());
            TRY(visitor.color_filter(i, input, filter_type, amount));
            break;
        }
        case SerializedFilterNodeType::ColorMatrix: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            Array<float, 20> matrix;
            for (auto& value : matrix)
                value = TRY(deserializer.decode_float());
            TRY(visitor.color_matrix(i, input, matrix));
            break;
        }
        case SerializedFilterNodeType::ColorTable: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto a = TRY(deserializer.decode_color_table());
            auto r = TRY(deserializer.decode_color_table());
            auto g = TRY(deserializer.decode_color_table());
            auto b = TRY(deserializer.decode_color_table());
            TRY(visitor.color_table(i, input, a, r, g, b));
            break;
        }
        case SerializedFilterNodeType::Saturate: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto value = TRY(deserializer.decode_float());
            TRY(visitor.saturate(i, input, value));
            break;
        }
        case SerializedFilterNodeType::HueRotate: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto angle_degrees = TRY(deserializer.decode_float());
            TRY(visitor.hue_rotate(i, input, angle_degrees));
            break;
        }
        case SerializedFilterNodeType::Image: {
            auto image_id = TRY(deserializer.decode_u64());
            auto src_rect = TRY(deserializer.decode_int_rect());
            auto dest_rect = TRY(deserializer.decode_int_rect());
            auto scaling_mode = TRY(deserializer.decode_enum<ScalingMode>());
            TRY(visitor.image(i, image_id, src_rect, dest_rect, scaling_mode));
            break;
        }
        case SerializedFilterNodeType::Merge: {
            auto input_count = TRY(deserializer.decode_u32());
            Vector<Optional<size_t>> inputs;
            TRY(inputs.try_ensure_capacity(input_count));
            for (u32 input_index = 0; input_index < input_count; ++input_index)
                inputs.unchecked_append(TRY(deserializer.decode_optional_node_reference(i)));
            TRY(visitor.merge(i, move(inputs)));
            break;
        }
        case SerializedFilterNodeType::Offset: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto dx = TRY(deserializer.decode_float());
            auto dy = TRY(deserializer.decode_float());
            TRY(visitor.offset(i, input, dx, dy));
            break;
        }
        case SerializedFilterNodeType::Morphology: {
            auto input = TRY(deserializer.decode_optional_node_reference(i));
            auto morphology_operator = TRY(deserializer.decode_enum<MorphologyOperator>());
            auto radius_x = TRY(deserializer.decode_float());
            auto radius_y = TRY(deserializer.decode_float());
            TRY(visitor.morphology(i, input, morphology_operator, radius_x, radius_y));
            break;
        }
        case SerializedFilterNodeType::Turbulence: {
            auto turbulence_type = TRY(deserializer.decode_enum<TurbulenceType>());
            auto base_frequency_x = TRY(deserializer.decode_float());
            auto base_frequency_y = TRY(deserializer.decode_float());
            auto num_octaves = TRY(deserializer.decode_i32());
            auto seed = TRY(deserializer.decode_float());
            auto tile_stitch_size = TRY(deserializer.decode_int_size());
            TRY(visitor.turbulence(i, turbulence_type, base_frequency_x, base_frequency_y, num_octaves, seed, tile_stitch_size));
            break;
        }
        }
        TRY(visitor.node_bytes(type, bytes.slice(node_start, deserializer.offset() - node_start)));
    }

    if (!deserializer.is_eof())
        return Error::from_string_literal("Gfx::Filter deserialization: trailing bytes");

    return root_node_index;
}

struct SerializedFilterView {
    u32 node_count { 0 };
    u32 root_node_index { 0 };
    ReadonlyBytes node_bytes;
};

static ErrorOr<SerializedFilterView> serialized_filter_view(ReadonlyBytes bytes)
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
    return SerializedFilterView { node_count, static_cast<u32>(root_node_index), bytes.slice(deserializer.offset()) };
}

class FilterSerializationBuilder {
public:
    size_t node_count() const { return m_node_count; }

    ErrorOr<size_t> append_filter(Filter const& filter)
    {
        auto view = TRY(serialized_filter_view(filter.impl().serialized_bytes.span()));
        auto offset = m_node_count;
        m_node_bytes.append(view.node_bytes.data(), view.node_bytes.size());
        m_node_count += view.node_count;
        for (auto const& entry : filter.impl().image_frames)
            m_image_frames.set(entry.key, entry.value);
        return view.root_node_index + offset;
    }

    ErrorOr<Optional<size_t>> append_optional_filter(Optional<Filter const&> filter)
    {
        if (!filter.has_value())
            return Optional<size_t> {};
        return TRY(append_filter(*filter));
    }

    ErrorOr<size_t> append_node(ReadonlyBytes node_bytes)
    {
        auto index = m_node_count;
        m_node_bytes.append(node_bytes.data(), node_bytes.size());
        ++m_node_count;
        return index;
    }

    u64 add_image_frame(DecodedImageFrame const& frame)
    {
        auto id = frame.id();
        m_image_frames.set(id, frame);
        return id;
    }

    HashMap<u64, DecodedImageFrame> take_image_frames()
    {
        return move(m_image_frames);
    }

    ErrorOr<Vector<u8>> finish(size_t root_node_index)
    {
        if (m_node_count == 0)
            return Error::from_string_literal("Gfx::Filter serialization: empty filter");
        if (root_node_index >= m_node_count)
            return Error::from_string_literal("Gfx::Filter serialization: invalid root node index");

        FilterSerializer serializer;
        TRY(serializer.encode_u32(serialized_filter_magic));
        TRY(serializer.encode_u32(serialized_filter_version));
        TRY(serializer.encode_size(m_node_count));
        TRY(serializer.encode_index(root_node_index));
        TRY(serializer.append_bytes(m_node_bytes.span()));
        return TRY(serializer.bytes());
    }

private:
    Vector<u8> m_node_bytes;
    size_t m_node_count { 0 };
    HashMap<u64, DecodedImageFrame> m_image_frames;
};

template<typename Callback>
static ErrorOr<Vector<u8>> serialize_filter_node(SerializedFilterNodeType type, Callback callback)
{
    FilterSerializer serializer;
    TRY(serializer.encode_node_type(type));
    TRY(callback(serializer));
    return TRY(serializer.bytes());
}

static NonnullOwnPtr<FilterImpl> create_filter_impl(sk_sp<SkImageFilter> filter, FilterSerializationBuilder&& builder, size_t root_node_index)
{
    auto serialized_bytes = MUST(builder.finish(root_node_index));
    return Impl::create(move(filter), move(serialized_bytes), builder.take_image_frames());
}

static SkColorChannel to_skia_color_channel(ChannelSelector channel_selector)
{
    switch (channel_selector) {
    case ChannelSelector::Red:
        return SkColorChannel::kR;
    case ChannelSelector::Green:
        return SkColorChannel::kG;
    case ChannelSelector::Blue:
        return SkColorChannel::kB;
    case ChannelSelector::Alpha:
        return SkColorChannel::kA;
    }
    VERIFY_NOT_REACHED();
}

static sk_sp<SkColorFilter> make_color_filter(ColorFilterType type, float amount)
{
    // Matrices are taken from https://drafts.fxtf.org/filter-effects-1/#FilterPrimitiveRepresentation
    switch (type) {
    case ColorFilterType::Grayscale: {
        float matrix[20] = {
            0.2126f + 0.7874f * (1 - amount), 0.7152f - 0.7152f * (1 - amount),
            0.0722f - 0.0722f * (1 - amount), 0, 0,
            0.2126f - 0.2126f * (1 - amount), 0.7152f + 0.2848f * (1 - amount),
            0.0722f - 0.0722f * (1 - amount), 0, 0,
            0.2126f - 0.2126f * (1 - amount), 0.7152f - 0.7152f * (1 - amount),
            0.0722f + 0.9278f * (1 - amount), 0, 0,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case Gfx::ColorFilterType::Brightness: {
        float matrix[20] = {
            amount, 0, 0, 0, 0,
            0, amount, 0, 0, 0,
            0, 0, amount, 0, 0,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
    }
    case Gfx::ColorFilterType::Contrast: {
        float intercept = -(0.5f * amount) + 0.5f;
        float matrix[20] = {
            amount, 0, 0, 0, intercept,
            0, amount, 0, 0, intercept,
            0, 0, amount, 0, intercept,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
    }
    case Gfx::ColorFilterType::Invert: {
        float matrix[20] = {
            1 - 2 * amount, 0, 0, 0, amount,
            0, 1 - 2 * amount, 0, 0, amount,
            0, 0, 1 - 2 * amount, 0, amount,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case Gfx::ColorFilterType::Opacity: {
        float matrix[20] = {
            1, 0, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 0, 1, 0, 0,
            0, 0, 0, amount, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case Gfx::ColorFilterType::Sepia: {
        float matrix[20] = {
            0.393f + 0.607f * (1 - amount), 0.769f - 0.769f * (1 - amount), 0.189f - 0.189f * (1 - amount), 0,
            0,
            0.349f - 0.349f * (1 - amount), 0.686f + 0.314f * (1 - amount), 0.168f - 0.168f * (1 - amount), 0,
            0,
            0.272f - 0.272f * (1 - amount), 0.534f - 0.534f * (1 - amount), 0.131f + 0.869f * (1 - amount), 0,
            0,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case Gfx::ColorFilterType::Saturate: {
        float matrix[20] = {
            0.213f + 0.787f * amount, 0.715f - 0.715f * amount, 0.072f - 0.072f * amount, 0, 0,
            0.213f - 0.213f * amount, 0.715f + 0.285f * amount, 0.072f - 0.072f * amount, 0, 0,
            0.213f - 0.213f * amount, 0.715f - 0.715f * amount, 0.072f + 0.928f * amount, 0, 0,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
    }
    }
    VERIFY_NOT_REACHED();
}

static sk_sp<SkColorFilter> make_hue_rotate_color_filter(float angle_degrees)
{
    float radians = AK::to_radians(angle_degrees);

    auto cosA = cos(radians);
    auto sinA = sin(radians);

    auto a00 = 0.213f + cosA * 0.787f - sinA * 0.213f;
    auto a01 = 0.715f - cosA * 0.715f - sinA * 0.715f;
    auto a02 = 0.072f - cosA * 0.072f + sinA * 0.928f;
    auto a10 = 0.213f - cosA * 0.213f + sinA * 0.143f;
    auto a11 = 0.715f + cosA * 0.285f + sinA * 0.140f;
    auto a12 = 0.072f - cosA * 0.072f - sinA * 0.283f;
    auto a20 = 0.213f - cosA * 0.213f - sinA * 0.787f;
    auto a21 = 0.715f - cosA * 0.715f + sinA * 0.715f;
    auto a22 = 0.072f + cosA * 0.928f + sinA * 0.072f;

    float matrix[20] = {
        a00, a01, a02, 0, 0,
        a10, a11, a12, 0, 0,
        a20, a21, a22, 0, 0,
        0, 0, 0, 1, 0
    };

    return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
}

static sk_sp<SkImageFilter> make_color_table_filter(Optional<Array<u8, 256>> const& a, Optional<Array<u8, 256>> const& r, Optional<Array<u8, 256>> const& g, Optional<Array<u8, 256>> const& b, sk_sp<SkImageFilter> input)
{
    auto* a_table = a.has_value() ? a->data() : nullptr;
    auto* r_table = r.has_value() ? r->data() : nullptr;
    auto* g_table = g.has_value() ? g->data() : nullptr;
    auto* b_table = b.has_value() ? b->data() : nullptr;

    // Color tables are applied in linear space by default, so we need to convert twice.
    // FIXME: support sRGB space as well (i.e. don't perform these conversions).
    auto srgb_to_linear = SkImageFilters::ColorFilter(SkColorFilters::SRGBToLinearGamma(), input);
    auto color_table = SkImageFilters::ColorFilter(SkColorFilters::TableARGB(a_table, r_table, g_table, b_table), srgb_to_linear);
    return SkImageFilters::ColorFilter(SkColorFilters::LinearToSRGBGamma(), color_table);
}

static sk_sp<SkImageFilter> make_turbulence_filter(TurbulenceType turbulence_type, float base_frequency_x, float base_frequency_y, i32 num_octaves, float seed, Gfx::IntSize const& tile_stitch_size)
{
    sk_sp<SkShader> turbulence_shader = [&] {
        auto skia_size = SkISize::Make(tile_stitch_size.width(), tile_stitch_size.height());
        switch (turbulence_type) {
        case TurbulenceType::Turbulence:
            return SkShaders::MakeTurbulence(base_frequency_x, base_frequency_y, num_octaves, seed, &skia_size);
        case TurbulenceType::FractalNoise:
            return SkShaders::MakeFractalNoise(base_frequency_x, base_frequency_y, num_octaves, seed, &skia_size);
        }
        VERIFY_NOT_REACHED();
    }();

    return SkImageFilters::Shader(move(turbulence_shader));
}

struct BuildSkiaFilterVisitor : SerializedFilterVisitor {
    explicit BuildSkiaFilterVisitor(Function<ErrorOr<DecodedImageFrame>(u64)> const& decode_image)
        : decode_image(decode_image)
    {
    }

    ErrorOr<void> header(u32 node_count, u32)
    {
        TRY(filters.try_ensure_capacity(node_count));
        return {};
    }

    sk_sp<SkImageFilter> input(size_t index) const
    {
        VERIFY(index < filters.size());
        return filters[index];
    }

    sk_sp<SkImageFilter> optional_input(Optional<size_t> index) const
    {
        if (!index.has_value())
            return nullptr;
        return input(*index);
    }

    ErrorOr<void> arithmetic(u32, Optional<size_t> background, Optional<size_t> foreground, float k1, float k2, float k3, float k4)
    {
        filters.unchecked_append(SkImageFilters::Arithmetic(
            SkFloatToScalar(k1), SkFloatToScalar(k2), SkFloatToScalar(k3), SkFloatToScalar(k4), false, optional_input(background), optional_input(foreground)));
        return {};
    }

    ErrorOr<void> compose(u32, size_t outer, size_t inner)
    {
        filters.unchecked_append(SkImageFilters::Compose(input(outer), input(inner)));
        return {};
    }

    ErrorOr<void> blend(u32, Optional<size_t> background, Optional<size_t> foreground, CompositingAndBlendingOperator mode)
    {
        filters.unchecked_append(SkImageFilters::Blend(to_skia_blender(mode), optional_input(background), optional_input(foreground)));
        return {};
    }

    ErrorOr<void> flood(u32, Gfx::Color color, float opacity)
    {
        auto color_skia = to_skia_color(color);
        color_skia = SkColorSetA(color_skia, static_cast<u8>(opacity * 255));
        filters.unchecked_append(SkImageFilters::Shader(SkShaders::Color(color_skia)));
        return {};
    }

    ErrorOr<void> displacement_map(u32, Optional<size_t> color, Optional<size_t> displacement, float scale, ChannelSelector x_channel_selector, ChannelSelector y_channel_selector)
    {
        filters.unchecked_append(SkImageFilters::DisplacementMap(to_skia_color_channel(x_channel_selector), to_skia_color_channel(y_channel_selector), scale, optional_input(displacement), optional_input(color)));
        return {};
    }

    ErrorOr<void> drop_shadow(u32, Optional<size_t> input_index, float offset_x, float offset_y, float radius, Gfx::Color color)
    {
        filters.unchecked_append(SkImageFilters::DropShadow(offset_x, offset_y, radius, radius, to_skia_color(color), optional_input(input_index)));
        return {};
    }

    ErrorOr<void> blur(u32, Optional<size_t> input_index, float radius_x, float radius_y)
    {
        filters.unchecked_append(SkImageFilters::Blur(radius_x, radius_y, optional_input(input_index)));
        return {};
    }

    ErrorOr<void> color_filter(u32, Optional<size_t> input_index, ColorFilterType type, float amount)
    {
        filters.unchecked_append(SkImageFilters::ColorFilter(make_color_filter(type, amount), optional_input(input_index)));
        return {};
    }

    ErrorOr<void> color_matrix(u32, Optional<size_t> input_index, Array<float, 20> const& matrix)
    {
        filters.unchecked_append(SkImageFilters::ColorFilter(SkColorFilters::Matrix(matrix.data()), optional_input(input_index)));
        return {};
    }

    ErrorOr<void> color_table(u32, Optional<size_t> input_index, Optional<Array<u8, 256>> const& a, Optional<Array<u8, 256>> const& r, Optional<Array<u8, 256>> const& g, Optional<Array<u8, 256>> const& b)
    {
        filters.unchecked_append(make_color_table_filter(a, r, g, b, optional_input(input_index)));
        return {};
    }

    ErrorOr<void> saturate(u32, Optional<size_t> input_index, float value)
    {
        SkColorMatrix matrix;
        matrix.setSaturation(value);
        filters.unchecked_append(SkImageFilters::ColorFilter(SkColorFilters::Matrix(matrix), optional_input(input_index)));
        return {};
    }

    ErrorOr<void> hue_rotate(u32, Optional<size_t> input_index, float angle_degrees)
    {
        filters.unchecked_append(SkImageFilters::ColorFilter(make_hue_rotate_color_filter(angle_degrees), optional_input(input_index)));
        return {};
    }

    ErrorOr<void> image(u32, u64 image_id, Gfx::IntRect const& src_rect, Gfx::IntRect const& dest_rect, Gfx::ScalingMode scaling_mode)
    {
        auto frame = TRY(decode_image(image_id));
        image_frames.set(image_id, frame);
        auto image = sk_image_from_bitmap(frame.bitmap(), frame.color_space());
        filters.unchecked_append(SkImageFilters::Image(move(image), to_skia_rect(src_rect), to_skia_rect(dest_rect), to_skia_sampling_options(scaling_mode)));
        return {};
    }

    ErrorOr<void> merge(u32, Vector<Optional<size_t>>&& inputs)
    {
        Vector<sk_sp<SkImageFilter>> skia_filters;
        TRY(skia_filters.try_ensure_capacity(inputs.size()));
        for (auto input_index : inputs)
            skia_filters.unchecked_append(optional_input(input_index));
        filters.unchecked_append(SkImageFilters::Merge(skia_filters.data(), skia_filters.size()));
        return {};
    }

    ErrorOr<void> offset(u32, Optional<size_t> input_index, float dx, float dy)
    {
        filters.unchecked_append(SkImageFilters::Offset(dx, dy, optional_input(input_index)));
        return {};
    }

    ErrorOr<void> morphology(u32, Optional<size_t> input_index, Gfx::MorphologyOperator morphology_operator, float radius_x, float radius_y)
    {
        switch (morphology_operator) {
        case MorphologyOperator::Erode:
            filters.unchecked_append(SkImageFilters::Erode(radius_x, radius_y, optional_input(input_index)));
            return {};
        case MorphologyOperator::Dilate:
            filters.unchecked_append(SkImageFilters::Dilate(radius_x, radius_y, optional_input(input_index)));
            return {};
        case MorphologyOperator::Unknown:
            VERIFY_NOT_REACHED();
        }
        VERIFY_NOT_REACHED();
    }

    ErrorOr<void> turbulence(u32, TurbulenceType turbulence_type, float base_frequency_x, float base_frequency_y, i32 num_octaves, float seed, Gfx::IntSize const& tile_stitch_size)
    {
        filters.unchecked_append(make_turbulence_filter(turbulence_type, base_frequency_x, base_frequency_y, num_octaves, seed, tile_stitch_size));
        return {};
    }

    Function<ErrorOr<DecodedImageFrame>(u64)> const& decode_image;
    Vector<sk_sp<SkImageFilter>> filters;
    HashMap<u64, DecodedImageFrame> image_frames;
};

struct VisitImageIdsVisitor : SerializedFilterVisitor {
    explicit VisitImageIdsVisitor(Function<ErrorOr<void>(u64)> const& callback)
        : callback(callback)
    {
    }

    ErrorOr<void> image(u32, u64 image_id, Gfx::IntRect const&, Gfx::IntRect const&, Gfx::ScalingMode)
    {
        return callback(image_id);
    }

    Function<ErrorOr<void>(u64)> const& callback;
};

struct RewriteImageIdsVisitor : SerializedFilterVisitor {
    explicit RewriteImageIdsVisitor(Function<ErrorOr<u64>(u64)> const& encode_image)
        : encode_image(encode_image)
    {
    }

    ErrorOr<void> header(u32 node_count, u32 root_node_index)
    {
        TRY(serializer.encode_u32(serialized_filter_magic));
        TRY(serializer.encode_u32(serialized_filter_version));
        TRY(serializer.encode_u32(node_count));
        TRY(serializer.encode_u32(root_node_index));
        return {};
    }

    ErrorOr<void> image(u32, u64 image_id, Gfx::IntRect const& src_rect, Gfx::IntRect const& dest_rect, Gfx::ScalingMode scaling_mode)
    {
        TRY(serializer.encode_node_type(SerializedFilterNodeType::Image));
        TRY(serializer.encode_u64(TRY(encode_image(image_id))));
        TRY(serializer.encode_int_rect(src_rect));
        TRY(serializer.encode_int_rect(dest_rect));
        TRY(serializer.encode_enum(scaling_mode));
        m_current_node_was_rewritten = true;
        return {};
    }

    ErrorOr<void> node_bytes(SerializedFilterNodeType, ReadonlyBytes node_bytes)
    {
        if (!m_current_node_was_rewritten)
            TRY(serializer.append_bytes(node_bytes));
        m_current_node_was_rewritten = false;
        return {};
    }

    ErrorOr<Vector<u8>> bytes() const
    {
        return TRY(serializer.bytes());
    }

    Function<ErrorOr<u64>(u64)> const& encode_image;
    FilterSerializer serializer;
    bool m_current_node_was_rewritten { false };
};

NonnullOwnPtr<FilterImpl> FilterImpl::create(sk_sp<SkImageFilter> filter, Vector<u8> serialized_bytes, HashMap<u64, DecodedImageFrame> image_frames)
{
    auto impl = adopt_own(*new FilterImpl);
    impl->filter = move(filter);
    impl->serialized_bytes = move(serialized_bytes);
    impl->image_frames = move(image_frames);
    return impl;
}

NonnullOwnPtr<FilterImpl> FilterImpl::clone() const
{
    return create(filter, serialized_bytes, image_frames);
}

Filter::Filter(Filter const& other)
    : m_id(other.m_id)
    , m_impl(other.m_impl->clone())
{
}

Filter& Filter::operator=(Filter const& other)
{
    if (this != &other) {
        m_id = other.m_id;
        m_impl = other.m_impl->clone();
    }
    return *this;
}

Filter::~Filter() = default;

Filter::Filter(NonnullOwnPtr<FilterImpl>&& impl)
    : m_id(s_next_id.fetch_add(1, AK::MemoryOrder::memory_order_relaxed))
    , m_impl(move(impl))
{
}

FilterImpl const& Filter::impl() const
{
    return *m_impl;
}

ErrorOr<Vector<u8>> Filter::serialize_to_bytes(Function<ErrorOr<u64>(DecodedImageFrame const&)> const& encode_image) const
{
    Function<ErrorOr<u64>(u64)> encode_image_id = [&](u64 image_id) -> ErrorOr<u64> {
        auto it = m_impl->image_frames.find(image_id);
        if (it == m_impl->image_frames.end())
            return Error::from_string_literal("Gfx::Filter serialization: missing image frame");
        return TRY(encode_image((*it).value));
    };

    RewriteImageIdsVisitor visitor { encode_image_id };
    TRY(decode_serialized_filter(m_impl->serialized_bytes, visitor));
    return TRY(visitor.bytes());
}

ErrorOr<Filter> Filter::from_serialized_bytes(ReadonlyBytes bytes, Function<ErrorOr<DecodedImageFrame>(u64)> const& decode_image)
{
    BuildSkiaFilterVisitor visitor { decode_image };
    auto root_node_index = TRY(decode_serialized_filter(bytes, visitor));
    VERIFY(root_node_index < visitor.filters.size());

    Vector<u8> serialized_bytes;
    TRY(serialized_bytes.try_resize(bytes.size()));
    bytes.copy_to(serialized_bytes.span());

    return Filter(Impl::create(visitor.filters[root_node_index], move(serialized_bytes), move(visitor.image_frames)));
}

ErrorOr<void> Filter::for_each_serialized_image_id(ReadonlyBytes bytes, Function<ErrorOr<void>(u64)> const& callback)
{
    VisitImageIdsVisitor visitor { callback };
    TRY(decode_serialized_filter(bytes, visitor));
    return {};
}

Filter Filter::arithmetic(Optional<Filter const&> background, Optional<Filter const&> foreground, float k1, float k2, float k3, float k4)
{
    sk_sp<SkImageFilter> background_skia = background.has_value() ? background->m_impl->filter : nullptr;
    sk_sp<SkImageFilter> foreground_skia = foreground.has_value() ? foreground->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Arithmetic(
        SkFloatToScalar(k1), SkFloatToScalar(k2), SkFloatToScalar(k3), SkFloatToScalar(k4), false, move(background_skia), move(foreground_skia));

    FilterSerializationBuilder builder;
    auto background_index = MUST(builder.append_optional_filter(background));
    auto foreground_index = MUST(builder.append_optional_filter(foreground));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Arithmetic, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, background_index));
        TRY(serializer.encode_optional_node_reference(node_index, foreground_index));
        TRY(serializer.encode_float(k1));
        TRY(serializer.encode_float(k2));
        TRY(serializer.encode_float(k3));
        TRY(serializer.encode_float(k4));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::compose(Filter const& outer, Filter const& inner)
{
    auto inner_skia = inner.m_impl->filter;
    auto outer_skia = outer.m_impl->filter;
    auto filter = SkImageFilters::Compose(outer_skia, inner_skia);

    FilterSerializationBuilder builder;
    auto outer_index = MUST(builder.append_filter(outer));
    auto inner_index = MUST(builder.append_filter(inner));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Compose, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_node_reference(node_index, outer_index));
        TRY(serializer.encode_node_reference(node_index, inner_index));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::blend(Optional<Filter const&> background, Optional<Filter const&> foreground, Gfx::CompositingAndBlendingOperator mode)
{
    sk_sp<SkImageFilter> background_skia = background.has_value() ? background->m_impl->filter : nullptr;
    sk_sp<SkImageFilter> foreground_skia = foreground.has_value() ? foreground->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Blend(to_skia_blender(mode), background_skia, foreground_skia);

    FilterSerializationBuilder builder;
    auto background_index = MUST(builder.append_optional_filter(background));
    auto foreground_index = MUST(builder.append_optional_filter(foreground));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Blend, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, background_index));
        TRY(serializer.encode_optional_node_reference(node_index, foreground_index));
        TRY(serializer.encode_enum(mode));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::blur(float radius_x, float radius_y, Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Blur(radius_x, radius_y, input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Blur, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_float(radius_x));
        TRY(serializer.encode_float(radius_y));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::flood(Gfx::Color color, float opacity)
{
    auto color_skia = to_skia_color(color);
    color_skia = SkColorSetA(color_skia, static_cast<u8>(opacity * 255));
    auto filter = SkImageFilters::Shader(SkShaders::Color(color_skia));

    FilterSerializationBuilder builder;
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Flood, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_color(color));
        TRY(serializer.encode_float(opacity));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::displacement_map(Optional<Filter const&> color, Optional<Filter const&> displacement, float scale, ChannelSelector x_channel_selector, ChannelSelector y_channel_selector)
{
    sk_sp<SkImageFilter> color_skia = color.has_value() ? color->m_impl->filter : nullptr;
    sk_sp<SkImageFilter> displacement_skia = displacement.has_value() ? displacement->m_impl->filter : nullptr;
    auto x_channel_selector_skia = to_skia_color_channel(x_channel_selector);
    auto y_channel_selector_skia = to_skia_color_channel(y_channel_selector);
    auto filter = SkImageFilters::DisplacementMap(x_channel_selector_skia, y_channel_selector_skia, scale, displacement_skia, color_skia);

    FilterSerializationBuilder builder;
    auto color_index = MUST(builder.append_optional_filter(color));
    auto displacement_index = MUST(builder.append_optional_filter(displacement));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::DisplacementMap, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, color_index));
        TRY(serializer.encode_optional_node_reference(node_index, displacement_index));
        TRY(serializer.encode_float(scale));
        TRY(serializer.encode_enum(x_channel_selector));
        TRY(serializer.encode_enum(y_channel_selector));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::drop_shadow(float offset_x, float offset_y, float radius, Gfx::Color color,
    Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto shadow_color = to_skia_color(color);

    auto filter = SkImageFilters::DropShadow(offset_x, offset_y, radius, radius, shadow_color, input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::DropShadow, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_float(offset_x));
        TRY(serializer.encode_float(offset_y));
        TRY(serializer.encode_float(radius));
        TRY(serializer.encode_color(color));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::color(ColorFilterType type, float amount, Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::ColorFilter(make_color_filter(type, amount), input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::ColorFilter, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_enum(type));
        TRY(serializer.encode_float(amount));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::color_matrix(float matrix[20], Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::ColorFilter(SkColorFilters::Matrix(matrix), input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::ColorMatrix, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        for (size_t i = 0; i < 20; ++i)
            TRY(serializer.encode_float(matrix[i]));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::color_table(Optional<ReadonlyBytes> a, Optional<ReadonlyBytes> r, Optional<ReadonlyBytes> g,
    Optional<ReadonlyBytes> b, Optional<Filter const&> input)
{
    VERIFY(!a.has_value() || a->size() == 256);
    VERIFY(!r.has_value() || r->size() == 256);
    VERIFY(!g.has_value() || g->size() == 256);
    VERIFY(!b.has_value() || b->size() == 256);

    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;

    auto* a_table = a.has_value() ? a->data() : nullptr;
    auto* r_table = r.has_value() ? r->data() : nullptr;
    auto* g_table = g.has_value() ? g->data() : nullptr;
    auto* b_table = b.has_value() ? b->data() : nullptr;

    // Color tables are applied in linear space by default, so we need to convert twice.
    // FIXME: support sRGB space as well (i.e. don't perform these conversions).
    auto srgb_to_linear = SkImageFilters::ColorFilter(SkColorFilters::SRGBToLinearGamma(), input_skia);
    auto color_table = SkImageFilters::ColorFilter(SkColorFilters::TableARGB(a_table, r_table, g_table, b_table), srgb_to_linear);
    auto linear_to_srgb = SkImageFilters::ColorFilter(SkColorFilters::LinearToSRGBGamma(), color_table);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::ColorTable, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_color_table(a));
        TRY(serializer.encode_color_table(r));
        TRY(serializer.encode_color_table(g));
        TRY(serializer.encode_color_table(b));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(linear_to_srgb), move(builder), root_node_index));
}

Filter Filter::saturate(float value, Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;

    SkColorMatrix matrix;
    matrix.setSaturation(value);
    auto filter = SkImageFilters::ColorFilter(SkColorFilters::Matrix(matrix), input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Saturate, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_float(value));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::hue_rotate(float angle_degrees, Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::ColorFilter(make_hue_rotate_color_filter(angle_degrees), input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::HueRotate, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_float(angle_degrees));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::image(Gfx::DecodedImageFrame const& frame, Gfx::IntRect const& src_rect, Gfx::IntRect const& dest_rect, Gfx::ScalingMode scaling_mode)
{
    auto skia_src_rect = to_skia_rect(src_rect);
    auto skia_dest_rect = to_skia_rect(dest_rect);
    auto sampling_options = to_skia_sampling_options(scaling_mode);

    auto image = sk_image_from_bitmap(frame.bitmap(), frame.color_space());
    auto filter = SkImageFilters::Image(move(image), skia_src_rect, skia_dest_rect, sampling_options);

    FilterSerializationBuilder builder;
    auto image_id = builder.add_image_frame(frame);
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Image, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_u64(image_id));
        TRY(serializer.encode_int_rect(src_rect));
        TRY(serializer.encode_int_rect(dest_rect));
        TRY(serializer.encode_enum(scaling_mode));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::merge(Vector<Optional<Filter>> const& inputs)
{
    Vector<sk_sp<SkImageFilter>> skia_filters;
    skia_filters.ensure_capacity(inputs.size());
    for (auto& filter : inputs)
        skia_filters.unchecked_append(filter.has_value() ? filter->m_impl->filter : nullptr);
    auto skia_filter = SkImageFilters::Merge(skia_filters.data(), skia_filters.size());

    FilterSerializationBuilder builder;
    Vector<Optional<size_t>> input_indices;
    input_indices.ensure_capacity(inputs.size());
    for (auto const& input : inputs) {
        if (input.has_value())
            input_indices.unchecked_append(MUST(builder.append_filter(input.value())));
        else
            input_indices.unchecked_append({});
    }
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Merge, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_size(input_indices.size()));
        for (auto input_index : input_indices)
            TRY(serializer.encode_optional_node_reference(node_index, input_index));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(skia_filter), move(builder), root_node_index));
}

Filter Filter::erode(float radius_x, float radius_y, Optional<Filter> const& input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Erode(radius_x, radius_y, input_skia);

    FilterSerializationBuilder builder;
    Optional<size_t> input_index;
    if (input.has_value())
        input_index = MUST(builder.append_filter(input.value()));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Morphology, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_enum(Gfx::MorphologyOperator::Erode));
        TRY(serializer.encode_float(radius_x));
        TRY(serializer.encode_float(radius_y));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::dilate(float radius_x, float radius_y, Optional<Filter> const& input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Dilate(radius_x, radius_y, input_skia);

    FilterSerializationBuilder builder;
    Optional<size_t> input_index;
    if (input.has_value())
        input_index = MUST(builder.append_filter(input.value()));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Morphology, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_enum(Gfx::MorphologyOperator::Dilate));
        TRY(serializer.encode_float(radius_x));
        TRY(serializer.encode_float(radius_y));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::offset(float dx, float dy, Optional<Filter const&> input)
{
    sk_sp<SkImageFilter> input_skia = input.has_value() ? input->m_impl->filter : nullptr;
    auto filter = SkImageFilters::Offset(dx, dy, input_skia);

    FilterSerializationBuilder builder;
    auto input_index = MUST(builder.append_optional_filter(input));
    auto node_index = builder.node_count();
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Offset, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_optional_node_reference(node_index, input_index));
        TRY(serializer.encode_float(dx));
        TRY(serializer.encode_float(dy));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

Filter Filter::turbulence(TurbulenceType turbulence_type, float base_frequency_x, float base_frequency_y, i32 num_octaves, float seed, Gfx::IntSize const& tile_stitch_size)
{
    auto filter = make_turbulence_filter(turbulence_type, base_frequency_x, base_frequency_y, num_octaves, seed, tile_stitch_size);

    FilterSerializationBuilder builder;
    auto node_bytes = MUST(serialize_filter_node(SerializedFilterNodeType::Turbulence, [&](FilterSerializer& serializer) -> ErrorOr<void> {
        TRY(serializer.encode_enum(turbulence_type));
        TRY(serializer.encode_float(base_frequency_x));
        TRY(serializer.encode_float(base_frequency_y));
        TRY(serializer.encode_i32(num_octaves));
        TRY(serializer.encode_float(seed));
        TRY(serializer.encode_int_size(tile_stitch_size));
        return {};
    }));
    auto root_node_index = MUST(builder.append_node(node_bytes.span()));
    return Filter(create_filter_impl(move(filter), move(builder), root_node_index));
}

}
