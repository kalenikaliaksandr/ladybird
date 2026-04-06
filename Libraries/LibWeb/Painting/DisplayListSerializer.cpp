/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/AnonymousBuffer.h>
#include <LibWeb/Painting/DisplayListSerializer.h>

namespace Web::Painting {

static constexpr u32 DISPLAY_LIST_MAGIC = 0x444C5354; // "DLST"
static constexpr u32 DISPLAY_LIST_VERSION = 1;

DisplayListSerializer::DisplayListSerializer(GPUResourceRegistry& registry)
    : m_registry(registry)
{
}

ErrorOr<void> DisplayListSerializer::write_bytes(ReadonlyBytes bytes)
{
    TRY(m_buffer.try_append(bytes.data(), bytes.size()));
    return {};
}

template<typename T>
ErrorOr<void> DisplayListSerializer::write(T const& value)
{
    return write_bytes({ &value, sizeof(T) });
}

ErrorOr<void> DisplayListSerializer::write_u8(u8 value) { return write(value); }
ErrorOr<void> DisplayListSerializer::write_u32(u32 value) { return write(value); }
ErrorOr<void> DisplayListSerializer::write_u64(u64 value) { return write(value); }
ErrorOr<void> DisplayListSerializer::write_i32(i32 value) { return write(value); }
ErrorOr<void> DisplayListSerializer::write_float(float value) { return write(value); }
ErrorOr<void> DisplayListSerializer::write_bool(bool value) { return write_u8(value ? 1 : 0); }

ErrorOr<void> DisplayListSerializer::write_color(Gfx::Color color)
{
    return write_u32(color.value());
}

ErrorOr<void> DisplayListSerializer::write_int_rect(Gfx::IntRect rect)
{
    TRY(write_i32(rect.x()));
    TRY(write_i32(rect.y()));
    TRY(write_i32(rect.width()));
    TRY(write_i32(rect.height()));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_float_point(Gfx::FloatPoint point)
{
    TRY(write_float(point.x()));
    TRY(write_float(point.y()));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_int_point(Gfx::IntPoint point)
{
    TRY(write_i32(point.x()));
    TRY(write_i32(point.y()));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_int_size(Gfx::IntSize size)
{
    TRY(write_i32(size.width()));
    TRY(write_i32(size.height()));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_corner_radii(CornerRadii const& radii)
{
    TRY(write_i32(radii.top_left.horizontal_radius));
    TRY(write_i32(radii.top_left.vertical_radius));
    TRY(write_i32(radii.top_right.horizontal_radius));
    TRY(write_i32(radii.top_right.vertical_radius));
    TRY(write_i32(radii.bottom_right.horizontal_radius));
    TRY(write_i32(radii.bottom_right.vertical_radius));
    TRY(write_i32(radii.bottom_left.horizontal_radius));
    TRY(write_i32(radii.bottom_left.vertical_radius));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_gradient_paint_data(GradientPaintData const& gradient)
{
    TRY(write_u32(gradient.color_stops.size()));
    for (auto const& stop : gradient.color_stops) {
        TRY(write_color(stop.color));
        TRY(write_float(stop.position));
        TRY(write_bool(stop.transition_hint.has_value()));
        if (stop.transition_hint.has_value())
            TRY(write_float(*stop.transition_hint));
    }
    TRY(write_bool(gradient.repeat_length.has_value()));
    if (gradient.repeat_length.has_value())
        TRY(write_float(*gradient.repeat_length));
    TRY(write_u8(static_cast<u8>(gradient.spread_method)));
    TRY(write_u8(static_cast<u8>(gradient.color_space)));
    // FIXME: Serialize gradient_transform
    TRY(write_bool(gradient.gradient_transform.has_value()));
    return {};
}

ErrorOr<void> DisplayListSerializer::write_paint_style_or_color(PaintStyleOrColor const& style)
{
    // 0 = Color, 1 = SVGLinearGradient, 2 = SVGRadialGradient, 3 = SVGPattern
    return style.visit(
        [&](Gfx::Color const& color) -> ErrorOr<void> {
            TRY(write_u8(0));
            TRY(write_color(color));
            return {};
        },
        [&](SVGLinearGradientPaintStyle const& linear) -> ErrorOr<void> {
            TRY(write_u8(1));
            TRY(write_gradient_paint_data(linear.gradient));
            TRY(write_float_point(linear.start_point));
            TRY(write_float_point(linear.end_point));
            return {};
        },
        [&](SVGRadialGradientPaintStyle const& radial) -> ErrorOr<void> {
            TRY(write_u8(2));
            TRY(write_gradient_paint_data(radial.gradient));
            TRY(write_float_point(radial.start_center));
            TRY(write_float(radial.start_radius));
            TRY(write_float_point(radial.end_center));
            TRY(write_float(radial.end_radius));
            return {};
        },
        [&](SVGPatternPaintStyle const&) -> ErrorOr<void> {
            // FIXME: Serialize SVGPatternPaintStyle (needs nested display list)
            TRY(write_u8(3));
            return {};
        });
}

ErrorOr<void> DisplayListSerializer::serialize_visual_context_tree(AccumulatedVisualContextTree const& tree)
{
    TRY(write_u32(tree.node_count()));

    for (size_t i = 0; i < tree.node_count(); ++i) {
        auto const& node = tree.node_at(VisualContextIndex(i));

        TRY(write_u32(node.parent_index.value()));
        TRY(write_u32(node.depth));
        TRY(write_bool(node.has_empty_effective_clip));

        // Serialize the variant data
        TRY(write_u8(node.data.index()));

        TRY(node.data.visit(
            [&](ScrollData const& data) -> ErrorOr<void> {
                TRY(write_u64(data.scroll_frame_index.value()));
                TRY(write_bool(data.is_sticky));
                return {};
            },
            [&](ClipData const& data) -> ErrorOr<void> {
                TRY(write_i32(data.rect.x().value()));
                TRY(write_i32(data.rect.y().value()));
                TRY(write_i32(data.rect.width().value()));
                TRY(write_i32(data.rect.height().value()));
                TRY(write_corner_radii(data.corner_radii));
                return {};
            },
            [&](TransformData const& data) -> ErrorOr<void> {
                TRY(write_bytes({ reinterpret_cast<u8 const*>(data.matrix.elements()), sizeof(float) * 16 }));
                TRY(write_float_point(data.origin));
                return {};
            },
            [&](PerspectiveData const& data) -> ErrorOr<void> {
                TRY(write_bytes({ reinterpret_cast<u8 const*>(data.matrix.elements()), sizeof(float) * 16 }));
                return {};
            },
            [&](ClipPathData const&) -> ErrorOr<void> {
                // FIXME: Serialize Gfx::Path for clip paths
                return {};
            },
            [&](EffectsData const& data) -> ErrorOr<void> {
                TRY(write_float(data.opacity));
                TRY(write_u8(static_cast<u8>(data.blend_mode)));
                TRY(write_bool(data.gfx_filter.has_value()));
                // FIXME: Serialize Gfx::Filter
                return {};
            }));
    }

    return {};
}

ErrorOr<void> DisplayListSerializer::serialize_scroll_state(
    ScrollStateSnapshotByDisplayList const& scroll_states,
    DisplayList const& main_display_list)
{
    // For now, only serialize the main display list's scroll state.
    // FIXME: Serialize scroll states for nested display lists too.
    auto it = scroll_states.find(NonnullRefPtr<DisplayList>(const_cast<DisplayList&>(main_display_list)));
    if (it != scroll_states.end()) {
        auto const& offsets = it->value.device_offsets();
        TRY(write_u32(offsets.size()));
        for (auto const& offset : offsets) {
            TRY(write_float_point(offset));
        }
    } else {
        TRY(write_u32(0));
    }

    return {};
}

ErrorOr<void> DisplayListSerializer::serialize_command(DisplayListCommand const& command)
{
    // Write the variant index (command type)
    TRY(write_u8(static_cast<u8>(command.index())));

    return command.visit(
        [&](DrawGlyphRun const& cmd) -> ErrorOr<void> {
            auto font_id = m_registry.ensure_font_id(cmd.glyph_run->font());
            TRY(write_u64(font_id));
            TRY(write_u32(cmd.glyph_run->glyphs().size()));
            for (auto const& glyph : cmd.glyph_run->glyphs()) {
                TRY(write_float_point(glyph.position));
                TRY(write_u32(glyph.glyph_id));
                TRY(write_float(glyph.glyph_width));
            }
            TRY(write_int_rect(cmd.rect));
            TRY(write_float_point(cmd.translation));
            TRY(write_color(cmd.color));
            TRY(write_u8(static_cast<u8>(cmd.orientation)));
            return {};
        },
        [&](FillRect const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_color(cmd.color));
            return {};
        },
        [&](DrawScaledImmutableBitmap const& cmd) -> ErrorOr<void> {
            auto image_id = m_registry.ensure_image_id(*cmd.bitmap);
            TRY(write_int_rect(cmd.dst_rect));
            TRY(write_int_rect(cmd.clip_rect));
            TRY(write_u64(image_id));
            TRY(write_u8(static_cast<u8>(cmd.scaling_mode)));
            return {};
        },
        [&](DrawRepeatedImmutableBitmap const& cmd) -> ErrorOr<void> {
            auto image_id = m_registry.ensure_image_id(*cmd.bitmap);
            TRY(write_int_rect(cmd.dst_rect));
            TRY(write_int_rect(cmd.clip_rect));
            TRY(write_u64(image_id));
            TRY(write_u8(static_cast<u8>(cmd.scaling_mode)));
            TRY(write_bool(cmd.repeat.x));
            TRY(write_bool(cmd.repeat.y));
            return {};
        },
        [&](DrawExternalContent const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.dst_rect));
            // Snapshot external content to an image
            auto bitmap = cmd.source->current_bitmap();
            if (bitmap) {
                auto image_id = m_registry.ensure_image_id(*bitmap);
                TRY(write_bool(true));
                TRY(write_u64(image_id));
            } else {
                TRY(write_bool(false));
            }
            TRY(write_u8(static_cast<u8>(cmd.scaling_mode)));
            return {};
        },
        [&](Save const&) -> ErrorOr<void> {
            return {};
        },
        [&](SaveLayer const&) -> ErrorOr<void> {
            return {};
        },
        [&](Restore const&) -> ErrorOr<void> {
            return {};
        },
        [&](Translate const& cmd) -> ErrorOr<void> {
            TRY(write_int_point(cmd.delta));
            return {};
        },
        [&](AddClipRect const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            return {};
        },
        [&](PaintLinearGradient const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.gradient_rect));
            TRY(write_float(cmd.linear_gradient_data.gradient_angle));
            auto const& stops = cmd.linear_gradient_data.color_stops.list;
            TRY(write_u32(stops.size()));
            for (auto const& stop : stops) {
                TRY(write_color(stop.color));
                TRY(write_float(stop.position));
            }
            TRY(write_bool(cmd.linear_gradient_data.color_stops.repeat_length.has_value()));
            if (cmd.linear_gradient_data.color_stops.repeat_length.has_value())
                TRY(write_float(*cmd.linear_gradient_data.color_stops.repeat_length));
            return {};
        },
        [&](PaintRadialGradient const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_int_point(cmd.center));
            TRY(write_int_size(cmd.size));
            auto const& stops = cmd.radial_gradient_data.color_stops.list;
            TRY(write_u32(stops.size()));
            for (auto const& stop : stops) {
                TRY(write_color(stop.color));
                TRY(write_float(stop.position));
            }
            TRY(write_bool(cmd.radial_gradient_data.color_stops.repeat_length.has_value()));
            if (cmd.radial_gradient_data.color_stops.repeat_length.has_value())
                TRY(write_float(*cmd.radial_gradient_data.color_stops.repeat_length));
            return {};
        },
        [&](PaintConicGradient const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_int_point(cmd.position));
            TRY(write_float(cmd.conic_gradient_data.start_angle));
            auto const& stops = cmd.conic_gradient_data.color_stops.list;
            TRY(write_u32(stops.size()));
            for (auto const& stop : stops) {
                TRY(write_color(stop.color));
                TRY(write_float(stop.position));
            }
            TRY(write_bool(cmd.conic_gradient_data.color_stops.repeat_length.has_value()));
            if (cmd.conic_gradient_data.color_stops.repeat_length.has_value())
                TRY(write_float(*cmd.conic_gradient_data.color_stops.repeat_length));
            return {};
        },
        [&](PaintOuterBoxShadow const& cmd) -> ErrorOr<void> {
            TRY(write_color(cmd.color));
            TRY(write_i32(cmd.blur_radius));
            TRY(write_int_rect(cmd.device_content_rect));
            TRY(write_corner_radii(cmd.content_corner_radii));
            TRY(write_int_rect(cmd.shadow_rect));
            TRY(write_corner_radii(cmd.shadow_corner_radii));
            return {};
        },
        [&](PaintInnerBoxShadow const& cmd) -> ErrorOr<void> {
            TRY(write_color(cmd.color));
            TRY(write_i32(cmd.blur_radius));
            TRY(write_int_rect(cmd.device_content_rect));
            TRY(write_corner_radii(cmd.content_corner_radii));
            TRY(write_int_rect(cmd.outer_shadow_rect));
            TRY(write_int_rect(cmd.inner_shadow_rect));
            TRY(write_corner_radii(cmd.inner_shadow_corner_radii));
            return {};
        },
        [&](PaintTextShadow const& cmd) -> ErrorOr<void> {
            auto font_id = m_registry.ensure_font_id(cmd.glyph_run->font());
            TRY(write_u64(font_id));
            TRY(write_u32(cmd.glyph_run->glyphs().size()));
            for (auto const& glyph : cmd.glyph_run->glyphs()) {
                TRY(write_float_point(glyph.position));
                TRY(write_u32(glyph.glyph_id));
                TRY(write_float(glyph.glyph_width));
            }
            TRY(write_int_rect(cmd.shadow_bounding_rect));
            TRY(write_int_rect(cmd.text_rect));
            TRY(write_float_point(cmd.draw_location));
            TRY(write_i32(cmd.blur_radius));
            TRY(write_color(cmd.color));
            return {};
        },
        [&](FillRectWithRoundedCorners const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_color(cmd.color));
            TRY(write_corner_radii(cmd.corner_radii));
            return {};
        },
        [&](FillPath const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.path_bounding_rect));
            auto path_bytes = TRY(cmd.path.serialize_to_bytes());
            TRY(write_u32(path_bytes.size()));
            TRY(write_bytes(path_bytes));
            TRY(write_float(cmd.opacity));
            TRY(write_paint_style_or_color(cmd.paint_style_or_color));
            TRY(write_u8(static_cast<u8>(cmd.winding_rule)));
            TRY(write_u8(static_cast<u8>(cmd.should_anti_alias)));
            return {};
        },
        [&](StrokePath const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.path_bounding_rect));
            auto path_bytes = TRY(cmd.path.serialize_to_bytes());
            TRY(write_u32(path_bytes.size()));
            TRY(write_bytes(path_bytes));
            TRY(write_float(cmd.opacity));
            TRY(write_paint_style_or_color(cmd.paint_style_or_color));
            TRY(write_float(cmd.thickness));
            TRY(write_u8(static_cast<u8>(cmd.cap_style)));
            TRY(write_u8(static_cast<u8>(cmd.join_style)));
            TRY(write_float(cmd.miter_limit));
            TRY(write_u32(cmd.dash_array.size()));
            for (auto dash : cmd.dash_array)
                TRY(write_float(dash));
            TRY(write_float(cmd.dash_offset));
            TRY(write_u8(static_cast<u8>(cmd.should_anti_alias)));
            return {};
        },
        [&](DrawEllipse const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_color(cmd.color));
            TRY(write_i32(cmd.thickness));
            return {};
        },
        [&](FillEllipse const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_color(cmd.color));
            return {};
        },
        [&](DrawLine const& cmd) -> ErrorOr<void> {
            TRY(write_color(cmd.color));
            TRY(write_int_point(cmd.from));
            TRY(write_int_point(cmd.to));
            TRY(write_i32(cmd.thickness));
            TRY(write_u8(static_cast<u8>(cmd.style)));
            TRY(write_color(cmd.alternate_color));
            return {};
        },
        [&](ApplyBackdropFilter const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.backdrop_region));
            TRY(write_corner_radii(cmd.corner_radii));
            // FIXME: Serialize Gfx::Filter
            TRY(write_bool(cmd.backdrop_filter.has_value()));
            return {};
        },
        [&](DrawRect const& cmd) -> ErrorOr<void> {
            TRY(write_int_rect(cmd.rect));
            TRY(write_color(cmd.color));
            TRY(write_bool(cmd.rough));
            return {};
        },
        [&](AddRoundedRectClip const& cmd) -> ErrorOr<void> {
            TRY(write_corner_radii(cmd.corner_radii));
            TRY(write_int_rect(cmd.border_rect));
            TRY(write_u8(static_cast<u8>(cmd.corner_clip)));
            return {};
        },
        [&](PaintNestedDisplayList const& cmd) -> ErrorOr<void> {
            TRY(write_bool(cmd.display_list != nullptr));
            if (cmd.display_list) {
                auto index = register_nested_display_list(cmd.display_list);
                TRY(write_u32(index));
            }
            TRY(write_int_rect(cmd.rect));
            return {};
        },
        [&](PaintScrollBar const& cmd) -> ErrorOr<void> {
            TRY(write_u64(cmd.scroll_frame_index.value()));
            TRY(write_int_rect(cmd.gutter_rect));
            TRY(write_int_rect(cmd.thumb_rect));
            TRY(write(cmd.scroll_size));
            TRY(write_color(cmd.thumb_color));
            TRY(write_color(cmd.track_color));
            TRY(write_bool(cmd.vertical));
            return {};
        },
        [&](ApplyEffects const& cmd) -> ErrorOr<void> {
            TRY(write_float(cmd.opacity));
            TRY(write_u8(static_cast<u8>(cmd.compositing_and_blending_operator)));
            // FIXME: Serialize Gfx::Filter
            TRY(write_bool(cmd.filter.has_value()));
            TRY(write_bool(cmd.mask_kind.has_value()));
            if (cmd.mask_kind.has_value())
                TRY(write_u8(static_cast<u8>(*cmd.mask_kind)));
            return {};
        });
}

ErrorOr<void> DisplayListSerializer::serialize_commands(DisplayList const& display_list)
{
    auto const& commands = display_list.commands();
    TRY(write_u32(commands.size()));

    for (auto const& item : commands) {
        TRY(write_u32(item.context_index.value()));
        TRY(serialize_command(item.command));
    }

    return {};
}

u32 DisplayListSerializer::register_nested_display_list(RefPtr<DisplayList> const& display_list)
{
    if (!display_list)
        return UINT32_MAX;

    auto it = m_nested_display_list_indices.find(display_list.ptr());
    if (it != m_nested_display_list_indices.end())
        return it->value;

    auto index = static_cast<u32>(m_nested_display_lists.size());
    m_nested_display_lists.append(display_list);
    m_nested_display_list_indices.set(display_list.ptr(), index);
    return index;
}

ErrorOr<void> DisplayListSerializer::serialize_nested_display_lists(ScrollStateSnapshotByDisplayList const& scroll_states)
{
    TRY(write_u32(m_nested_display_lists.size()));

    for (auto const& nested_dl : m_nested_display_lists) {
        if (!nested_dl) {
            TRY(write_u32(0)); // 0 commands
            TRY(write_u32(0)); // 0 context nodes
            TRY(write_u32(0)); // 0 scroll offsets
            continue;
        }

        // Serialize the nested display list's visual context tree
        TRY(serialize_visual_context_tree(nested_dl->visual_context_tree()));

        // Serialize nested scroll state
        auto it = scroll_states.find(*nested_dl);
        if (it != scroll_states.end()) {
            auto const& offsets = it->value.device_offsets();
            TRY(write_u32(offsets.size()));
            for (auto const& offset : offsets)
                TRY(write_float_point(offset));
        } else {
            TRY(write_u32(0));
        }

        // Serialize nested commands
        TRY(serialize_commands(*nested_dl));
    }

    return {};
}

ErrorOr<Core::AnonymousBuffer> DisplayListSerializer::serialize(
    DisplayList const& display_list,
    ScrollStateSnapshotByDisplayList const& scroll_states,
    GPUResourceRegistry& registry)
{
    DisplayListSerializer serializer(registry);

    // Write header
    TRY(serializer.write_u32(DISPLAY_LIST_MAGIC));
    TRY(serializer.write_u32(DISPLAY_LIST_VERSION));

    // Serialize the visual context tree
    TRY(serializer.serialize_visual_context_tree(display_list.visual_context_tree()));

    // Serialize scroll state
    TRY(serializer.serialize_scroll_state(scroll_states, display_list));

    // Collect nested display lists from the command stream first
    for (auto const& item : display_list.commands()) {
        item.command.visit(
            [&](PaintNestedDisplayList const& cmd) {
                if (cmd.display_list)
                    serializer.register_nested_display_list(cmd.display_list);
            },
            [](auto const&) {});
    }

    // Serialize nested display lists before commands so they're
    // available during deserialization when commands reference them
    TRY(serializer.serialize_nested_display_lists(scroll_states));

    // Serialize main commands (nested DL indices are already assigned)
    TRY(serializer.serialize_commands(display_list));

    // Copy to anonymous buffer for shared memory transfer
    auto anon_buffer = TRY(Core::AnonymousBuffer::create_with_size(serializer.m_buffer.size()));
    memcpy(anon_buffer.data<void>(), serializer.m_buffer.data(), serializer.m_buffer.size());

    return anon_buffer;
}

}
