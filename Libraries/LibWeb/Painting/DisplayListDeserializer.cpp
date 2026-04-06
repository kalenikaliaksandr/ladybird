/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/TextLayout.h>
#include <LibWeb/CSS/StyleValues/ColorInterpolationMethodStyleValue.h>
#include <LibWeb/Painting/DisplayListDeserializer.h>

namespace Web::Painting {

static constexpr u32 DISPLAY_LIST_MAGIC = 0x444C5354;
static constexpr u32 DISPLAY_LIST_VERSION = 1;

DisplayListDeserializer::DisplayListDeserializer(ReadonlyBytes buffer, ResourceRegistries const& registries)
    : m_buffer(buffer)
    , m_registries(registries)
{
}

template<typename T>
ErrorOr<T> DisplayListDeserializer::read()
{
    if (m_offset + sizeof(T) > m_buffer.size())
        return Error::from_string_literal("Buffer underflow");
    T value;
    memcpy(&value, m_buffer.data() + m_offset, sizeof(T));
    m_offset += sizeof(T);
    return value;
}

ErrorOr<u8> DisplayListDeserializer::read_u8() { return read<u8>(); }
ErrorOr<u32> DisplayListDeserializer::read_u32() { return read<u32>(); }
ErrorOr<u64> DisplayListDeserializer::read_u64() { return read<u64>(); }
ErrorOr<i32> DisplayListDeserializer::read_i32() { return read<i32>(); }
ErrorOr<float> DisplayListDeserializer::read_float() { return read<float>(); }

ErrorOr<bool> DisplayListDeserializer::read_bool()
{
    auto value = TRY(read_u8());
    return value != 0;
}

ErrorOr<Gfx::Color> DisplayListDeserializer::read_color()
{
    return Gfx::Color::from_bgra(TRY(read_u32()));
}

ErrorOr<Gfx::IntRect> DisplayListDeserializer::read_int_rect()
{
    auto x = TRY(read_i32());
    auto y = TRY(read_i32());
    auto w = TRY(read_i32());
    auto h = TRY(read_i32());
    return Gfx::IntRect { x, y, w, h };
}

ErrorOr<Gfx::FloatPoint> DisplayListDeserializer::read_float_point()
{
    auto x = TRY(read_float());
    auto y = TRY(read_float());
    return Gfx::FloatPoint { x, y };
}

ErrorOr<Gfx::IntPoint> DisplayListDeserializer::read_int_point()
{
    auto x = TRY(read_i32());
    auto y = TRY(read_i32());
    return Gfx::IntPoint { x, y };
}

ErrorOr<Gfx::IntSize> DisplayListDeserializer::read_int_size()
{
    auto w = TRY(read_i32());
    auto h = TRY(read_i32());
    return Gfx::IntSize { w, h };
}

ErrorOr<CornerRadii> DisplayListDeserializer::read_corner_radii()
{
    CornerRadii radii;
    radii.top_left.horizontal_radius = TRY(read_i32());
    radii.top_left.vertical_radius = TRY(read_i32());
    radii.top_right.horizontal_radius = TRY(read_i32());
    radii.top_right.vertical_radius = TRY(read_i32());
    radii.bottom_right.horizontal_radius = TRY(read_i32());
    radii.bottom_right.vertical_radius = TRY(read_i32());
    radii.bottom_left.horizontal_radius = TRY(read_i32());
    radii.bottom_left.vertical_radius = TRY(read_i32());
    return radii;
}

ErrorOr<DisplayListCommand> DisplayListDeserializer::deserialize_command(u8 type_index)
{
    switch (type_index) {
    case 0: { // DrawGlyphRun
        auto font_id = TRY(read_u64());
        auto glyph_count = TRY(read_u32());
        Vector<Gfx::DrawGlyph> glyphs;
        TRY(glyphs.try_ensure_capacity(glyph_count));
        for (u32 i = 0; i < glyph_count; ++i) {
            Gfx::DrawGlyph glyph;
            glyph.position = TRY(read_float_point());
            glyph.glyph_id = TRY(read_u32());
            glyph.glyph_width = TRY(read_float());
            glyphs.unchecked_append(glyph);
        }
        auto rect = TRY(read_int_rect());
        auto translation = TRY(read_float_point());
        auto color = TRY(read_color());
        auto orientation = static_cast<Gfx::Orientation>(TRY(read_u8()));

        auto font_it = m_registries.fonts.find(font_id);
        if (font_it == m_registries.fonts.end())
            return Error::from_string_literal("Unknown font ID");

        auto glyph_run = adopt_ref(*new Gfx::GlyphRun(move(glyphs), font_it->value, Gfx::GlyphRun::TextType::Common, 0));
        return DisplayListCommand(DrawGlyphRun {
            .glyph_run = move(glyph_run),
            .rect = rect,
            .translation = translation,
            .color = color,
            .orientation = orientation,
        });
    }
    case 1: { // FillRect
        auto rect = TRY(read_int_rect());
        auto color = TRY(read_color());
        return DisplayListCommand(FillRect {
            .rect = rect,
            .color = color,
        });
    }
    case 2: { // DrawScaledImmutableBitmap
        auto dst_rect = TRY(read_int_rect());
        auto clip_rect = TRY(read_int_rect());
        auto image_id = TRY(read_u64());
        auto scaling_mode = static_cast<Gfx::ScalingMode>(TRY(read_u8()));

        auto image_it = m_registries.images.find(image_id);
        if (image_it == m_registries.images.end())
            return Error::from_string_literal("Unknown image ID");

        return DisplayListCommand(DrawScaledImmutableBitmap {
            .dst_rect = dst_rect,
            .clip_rect = clip_rect,
            .bitmap = image_it->value,
            .scaling_mode = scaling_mode,
        });
    }
    case 3: { // DrawRepeatedImmutableBitmap
        auto dst_rect = TRY(read_int_rect());
        auto clip_rect = TRY(read_int_rect());
        auto image_id = TRY(read_u64());
        auto scaling_mode = static_cast<Gfx::ScalingMode>(TRY(read_u8()));
        auto repeat_x = TRY(read_bool());
        auto repeat_y = TRY(read_bool());

        auto image_it = m_registries.images.find(image_id);
        if (image_it == m_registries.images.end())
            return Error::from_string_literal("Unknown image ID");

        return DisplayListCommand(DrawRepeatedImmutableBitmap {
            .dst_rect = dst_rect,
            .clip_rect = clip_rect,
            .bitmap = image_it->value,
            .scaling_mode = scaling_mode,
            .repeat = { .x = repeat_x, .y = repeat_y },
        });
    }
    case 4: { // DrawExternalContent
        auto dst_rect = TRY(read_int_rect());
        auto has_bitmap = TRY(read_bool());
        if (has_bitmap)
            TRY(read_u64()); // image_id — skip for now
        auto scaling_mode = static_cast<Gfx::ScalingMode>(TRY(read_u8()));
        (void)scaling_mode;
        // FIXME: Reconstruct DrawExternalContent from image
        return DisplayListCommand(FillRect { .rect = dst_rect, .color = Gfx::Color::Transparent });
    }
    case 5: // Save
        return DisplayListCommand(Save {});
    case 6: // SaveLayer
        return DisplayListCommand(SaveLayer {});
    case 7: // Restore
        return DisplayListCommand(Restore {});
    case 8: { // Translate
        auto delta = TRY(read_int_point());
        return DisplayListCommand(Translate { .delta = delta });
    }
    case 9: { // AddClipRect
        auto rect = TRY(read_int_rect());
        return DisplayListCommand(AddClipRect { .rect = rect });
    }
    case 10: { // PaintLinearGradient
        auto gradient_rect = TRY(read_int_rect());
        auto gradient_angle = TRY(read_float());
        auto stop_count = TRY(read_u32());
        ColorStopList stops;
        for (u32 i = 0; i < stop_count; ++i) {
            auto color = TRY(read_color());
            auto position = TRY(read_float());
            stops.append({ color, position });
        }
        auto has_repeat = TRY(read_bool());
        Optional<float> repeat_length;
        if (has_repeat)
            repeat_length = TRY(read_float());

        return DisplayListCommand(PaintLinearGradient {
            .gradient_rect = gradient_rect,
            .linear_gradient_data = LinearGradientData {
                .gradient_angle = gradient_angle,
                .color_stops = ColorStopData { .list = move(stops), .repeat_length = repeat_length, .repeating = repeat_length.has_value() },
                .interpolation_method = CSS::RectangularColorSpace::Srgb,
            },
        });
    }
    case 11: { // PaintRadialGradient
        auto rect = TRY(read_int_rect());
        auto center = TRY(read_int_point());
        auto size = TRY(read_int_size());
        auto stop_count = TRY(read_u32());
        ColorStopList stops;
        for (u32 i = 0; i < stop_count; ++i) {
            auto color = TRY(read_color());
            auto position = TRY(read_float());
            stops.append({ color, position });
        }
        auto has_repeat = TRY(read_bool());
        Optional<float> repeat_length;
        if (has_repeat)
            repeat_length = TRY(read_float());

        return DisplayListCommand(PaintRadialGradient {
            .rect = rect,
            .radial_gradient_data = RadialGradientData {
                .color_stops = ColorStopData { .list = move(stops), .repeat_length = repeat_length, .repeating = repeat_length.has_value() },
                .interpolation_method = CSS::RectangularColorSpace::Srgb,
            },
            .center = center,
            .size = size,
        });
    }
    case 12: { // PaintConicGradient
        auto rect = TRY(read_int_rect());
        auto position = TRY(read_int_point());
        auto start_angle = TRY(read_float());
        auto stop_count = TRY(read_u32());
        ColorStopList stops;
        for (u32 i = 0; i < stop_count; ++i) {
            auto color = TRY(read_color());
            auto pos = TRY(read_float());
            stops.append({ color, pos });
        }
        auto has_repeat = TRY(read_bool());
        Optional<float> repeat_length;
        if (has_repeat)
            repeat_length = TRY(read_float());

        return DisplayListCommand(PaintConicGradient {
            .rect = rect,
            .conic_gradient_data = ConicGradientData {
                .start_angle = start_angle,
                .color_stops = ColorStopData { .list = move(stops), .repeat_length = repeat_length, .repeating = repeat_length.has_value() },
                .interpolation_method = CSS::RectangularColorSpace::Srgb,
            },
            .position = position,
        });
    }
    case 13: { // PaintOuterBoxShadow
        auto color = TRY(read_color());
        auto blur_radius = TRY(read_i32());
        auto device_content_rect = TRY(read_int_rect());
        auto content_corner_radii = TRY(read_corner_radii());
        auto shadow_rect = TRY(read_int_rect());
        auto shadow_corner_radii = TRY(read_corner_radii());
        return DisplayListCommand(PaintOuterBoxShadow {
            .color = color,
            .blur_radius = blur_radius,
            .device_content_rect = device_content_rect,
            .content_corner_radii = content_corner_radii,
            .shadow_rect = shadow_rect,
            .shadow_corner_radii = shadow_corner_radii,
        });
    }
    case 14: { // PaintInnerBoxShadow
        auto color = TRY(read_color());
        auto blur_radius = TRY(read_i32());
        auto device_content_rect = TRY(read_int_rect());
        auto content_corner_radii = TRY(read_corner_radii());
        auto outer_shadow_rect = TRY(read_int_rect());
        auto inner_shadow_rect = TRY(read_int_rect());
        auto inner_shadow_corner_radii = TRY(read_corner_radii());
        return DisplayListCommand(PaintInnerBoxShadow {
            .color = color,
            .blur_radius = blur_radius,
            .device_content_rect = device_content_rect,
            .content_corner_radii = content_corner_radii,
            .outer_shadow_rect = outer_shadow_rect,
            .inner_shadow_rect = inner_shadow_rect,
            .inner_shadow_corner_radii = inner_shadow_corner_radii,
        });
    }
    case 15: { // PaintTextShadow
        auto font_id = TRY(read_u64());
        auto glyph_count = TRY(read_u32());
        Vector<Gfx::DrawGlyph> glyphs;
        TRY(glyphs.try_ensure_capacity(glyph_count));
        for (u32 i = 0; i < glyph_count; ++i) {
            Gfx::DrawGlyph glyph;
            glyph.position = TRY(read_float_point());
            glyph.glyph_id = TRY(read_u32());
            glyph.glyph_width = TRY(read_float());
            glyphs.unchecked_append(glyph);
        }
        auto shadow_bounding_rect = TRY(read_int_rect());
        auto text_rect = TRY(read_int_rect());
        auto draw_location = TRY(read_float_point());
        auto blur_radius = TRY(read_i32());
        auto color = TRY(read_color());

        auto font_it = m_registries.fonts.find(font_id);
        if (font_it == m_registries.fonts.end())
            return Error::from_string_literal("Unknown font ID");

        auto glyph_run = adopt_ref(*new Gfx::GlyphRun(move(glyphs), font_it->value, Gfx::GlyphRun::TextType::Common, 0));
        return DisplayListCommand(PaintTextShadow {
            .glyph_run = move(glyph_run),
            .shadow_bounding_rect = shadow_bounding_rect,
            .text_rect = text_rect,
            .draw_location = draw_location,
            .blur_radius = blur_radius,
            .color = color,
        });
    }
    case 16: { // FillRectWithRoundedCorners
        auto rect = TRY(read_int_rect());
        auto color = TRY(read_color());
        auto corner_radii = TRY(read_corner_radii());
        return DisplayListCommand(FillRectWithRoundedCorners {
            .rect = rect,
            .color = color,
            .corner_radii = corner_radii,
        });
    }
    case 17: { // FillPath
        auto path_bounding_rect = TRY(read_int_rect());
        // FIXME: Deserialize Gfx::Path
        // FIXME: Deserialize PaintStyleOrColor
        auto opacity = TRY(read_float());
        auto winding_rule = static_cast<Gfx::WindingRule>(TRY(read_u8()));
        auto should_anti_alias = static_cast<ShouldAntiAlias>(TRY(read_u8()));
        (void)opacity;
        (void)winding_rule;
        (void)should_anti_alias;
        // FIXME: Return actual FillPath command once Path serialization is implemented
        return DisplayListCommand(FillRect { .rect = path_bounding_rect, .color = Gfx::Color::Transparent });
    }
    case 18: { // StrokePath
        auto path_bounding_rect = TRY(read_int_rect());
        // FIXME: Deserialize Gfx::Path
        // FIXME: Deserialize PaintStyleOrColor
        auto opacity = TRY(read_float());
        auto thickness = TRY(read_float());
        auto cap_style = static_cast<Gfx::Path::CapStyle>(TRY(read_u8()));
        auto join_style = static_cast<Gfx::Path::JoinStyle>(TRY(read_u8()));
        auto miter_limit = TRY(read_float());
        auto should_anti_alias = static_cast<ShouldAntiAlias>(TRY(read_u8()));
        (void)opacity;
        (void)thickness;
        (void)cap_style;
        (void)join_style;
        (void)miter_limit;
        (void)should_anti_alias;
        // FIXME: Return actual StrokePath command once Path serialization is implemented
        return DisplayListCommand(FillRect { .rect = path_bounding_rect, .color = Gfx::Color::Transparent });
    }
    case 19: { // DrawEllipse
        auto rect = TRY(read_int_rect());
        auto color = TRY(read_color());
        auto thickness = TRY(read_i32());
        return DisplayListCommand(DrawEllipse {
            .rect = rect,
            .color = color,
            .thickness = thickness,
        });
    }
    case 20: { // FillEllipse
        auto rect = TRY(read_int_rect());
        auto color = TRY(read_color());
        return DisplayListCommand(FillEllipse {
            .rect = rect,
            .color = color,
        });
    }
    case 21: { // DrawLine
        auto color = TRY(read_color());
        auto from = TRY(read_int_point());
        auto to = TRY(read_int_point());
        auto thickness = TRY(read_i32());
        auto style = static_cast<Gfx::LineStyle>(TRY(read_u8()));
        auto alternate_color = TRY(read_color());
        return DisplayListCommand(DrawLine {
            .color = color,
            .from = from,
            .to = to,
            .thickness = thickness,
            .style = style,
            .alternate_color = alternate_color,
        });
    }
    case 22: { // ApplyBackdropFilter
        auto backdrop_region = TRY(read_int_rect());
        auto corner_radii = TRY(read_corner_radii());
        auto has_filter = TRY(read_bool());
        (void)has_filter;
        // FIXME: Deserialize Gfx::Filter
        return DisplayListCommand(ApplyBackdropFilter {
            .backdrop_region = backdrop_region,
            .corner_radii = corner_radii,
            .backdrop_filter = {},
        });
    }
    case 23: { // DrawRect
        auto rect = TRY(read_int_rect());
        auto color = TRY(read_color());
        auto rough = TRY(read_bool());
        return DisplayListCommand(DrawRect {
            .rect = rect,
            .color = color,
            .rough = rough,
        });
    }
    case 24: { // AddRoundedRectClip
        auto corner_radii = TRY(read_corner_radii());
        auto border_rect = TRY(read_int_rect());
        auto corner_clip = static_cast<CornerClip>(TRY(read_u8()));
        return DisplayListCommand(AddRoundedRectClip {
            .corner_radii = corner_radii,
            .border_rect = border_rect,
            .corner_clip = corner_clip,
        });
    }
    case 25: { // PaintNestedDisplayList
        auto has_display_list = TRY(read_bool());
        auto rect = TRY(read_int_rect());
        (void)has_display_list;
        // FIXME: Deserialize nested display list
        return DisplayListCommand(PaintNestedDisplayList {
            .display_list = nullptr,
            .rect = rect,
        });
    }
    case 26: { // PaintScrollBar
        auto scroll_frame_index = ScrollFrameIndex(TRY(read_u64()));
        auto gutter_rect = TRY(read_int_rect());
        auto thumb_rect = TRY(read_int_rect());
        auto scroll_size = TRY(read<double>());
        auto thumb_color = TRY(read_color());
        auto track_color = TRY(read_color());
        auto vertical = TRY(read_bool());
        return DisplayListCommand(PaintScrollBar {
            .scroll_frame_index = scroll_frame_index,
            .gutter_rect = gutter_rect,
            .thumb_rect = thumb_rect,
            .scroll_size = scroll_size,
            .thumb_color = thumb_color,
            .track_color = track_color,
            .vertical = vertical,
        });
    }
    case 27: { // ApplyEffects
        auto opacity = TRY(read_float());
        auto compositing_op = static_cast<Gfx::CompositingAndBlendingOperator>(TRY(read_u8()));
        auto has_filter = TRY(read_bool());
        auto has_mask_kind = TRY(read_bool());
        Optional<Gfx::MaskKind> mask_kind;
        if (has_mask_kind)
            mask_kind = static_cast<Gfx::MaskKind>(TRY(read_u8()));
        (void)has_filter;
        // FIXME: Deserialize Gfx::Filter
        return DisplayListCommand(ApplyEffects {
            .opacity = opacity,
            .compositing_and_blending_operator = compositing_op,
            .mask_kind = mask_kind,
        });
    }
    default:
        return Error::from_string_literal("Unknown command type");
    }
}

ErrorOr<NonnullRefPtr<AccumulatedVisualContextTree>> DisplayListDeserializer::deserialize_visual_context_tree()
{
    auto tree = AccumulatedVisualContextTree::create();

    auto node_count = TRY(read_u32());
    for (u32 i = 0; i < node_count; ++i) {
        auto parent_index = VisualContextIndex(TRY(read_u32()));
        auto depth = TRY(read_u32());
        auto has_empty_effective_clip = TRY(read_bool());
        (void)depth;
        (void)has_empty_effective_clip;

        auto variant_index = TRY(read_u8());
        VisualContextData data = ScrollData { {}, false };

        switch (variant_index) {
        case 0: { // ScrollData
            auto scroll_frame_index = ScrollFrameIndex(TRY(read_u64()));
            auto is_sticky = TRY(read_bool());
            data = ScrollData { scroll_frame_index, is_sticky };
            break;
        }
        case 1: { // ClipData
            auto x = TRY(read_i32());
            auto y = TRY(read_i32());
            auto w = TRY(read_i32());
            auto h = TRY(read_i32());
            auto corner_radii = TRY(read_corner_radii());
            data = ClipData(DevicePixelRect(x, y, w, h), corner_radii);
            break;
        }
        case 2: { // TransformData
            Gfx::FloatMatrix4x4 matrix;
            if (m_offset + sizeof(float) * 16 > m_buffer.size())
                return Error::from_string_literal("Buffer underflow");
            memcpy(matrix.elements(), m_buffer.data() + m_offset, sizeof(float) * 16);
            m_offset += sizeof(float) * 16;
            auto origin = TRY(read_float_point());
            data = TransformData { matrix, origin };
            break;
        }
        case 3: { // PerspectiveData
            Gfx::FloatMatrix4x4 matrix;
            if (m_offset + sizeof(float) * 16 > m_buffer.size())
                return Error::from_string_literal("Buffer underflow");
            memcpy(matrix.elements(), m_buffer.data() + m_offset, sizeof(float) * 16);
            m_offset += sizeof(float) * 16;
            data = PerspectiveData { matrix };
            break;
        }
        case 4: { // ClipPathData
            // FIXME: Deserialize Gfx::Path
            data = ClipPathData { {}, {}, Gfx::WindingRule::Nonzero };
            break;
        }
        case 5: { // EffectsData
            auto opacity = TRY(read_float());
            auto blend_mode = static_cast<Gfx::CompositingAndBlendingOperator>(TRY(read_u8()));
            auto has_filter = TRY(read_bool());
            (void)has_filter;
            // FIXME: Deserialize Gfx::Filter
            data = EffectsData { opacity, blend_mode, {} };
            break;
        }
        default:
            return Error::from_string_literal("Unknown visual context variant");
        }

        if (i > 0)
            tree->append(move(data), parent_index);
    }

    return tree;
}

ErrorOr<ScrollStateSnapshot> DisplayListDeserializer::deserialize_scroll_state()
{
    auto offset_count = TRY(read_u32());
    Vector<Gfx::FloatPoint> offsets;
    TRY(offsets.try_ensure_capacity(offset_count));
    for (u32 i = 0; i < offset_count; ++i) {
        offsets.unchecked_append(TRY(read_float_point()));
    }
    return ScrollStateSnapshot::create_from_offsets(move(offsets));
}

ErrorOr<NonnullRefPtr<DisplayList>> DisplayListDeserializer::do_deserialize()
{
    // Read and validate header
    auto magic = TRY(read_u32());
    if (magic != DISPLAY_LIST_MAGIC)
        return Error::from_string_literal("Invalid display list magic");

    auto version = TRY(read_u32());
    if (version != DISPLAY_LIST_VERSION)
        return Error::from_string_literal("Unsupported display list version");

    // Deserialize visual context tree
    auto visual_context_tree = TRY(deserialize_visual_context_tree());

    // Deserialize scroll state
    m_scroll_state = TRY(deserialize_scroll_state());

    // Deserialize commands
    auto command_count = TRY(read_u32());
    auto display_list = DisplayList::create(move(visual_context_tree));

    for (u32 i = 0; i < command_count; ++i) {
        auto context_index = VisualContextIndex(TRY(read_u32()));
        auto type_index = TRY(read_u8());
        auto command = TRY(deserialize_command(type_index));
        display_list->append(move(command), context_index);
    }

    return display_list;
}

ErrorOr<DisplayListDeserializer::Result> DisplayListDeserializer::deserialize(
    ReadonlyBytes buffer,
    ResourceRegistries const& registries)
{
    DisplayListDeserializer deserializer(buffer, registries);
    auto display_list = TRY(deserializer.do_deserialize());
    return Result {
        .display_list = move(display_list),
        .scroll_state = move(deserializer.m_scroll_state),
    };
}

}
