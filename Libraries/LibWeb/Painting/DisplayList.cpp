/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/TemporaryChange.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Painting {

bool DisplayList::append(DisplayListCommand&& command, VisualContextIndex state)
{
    if (m_visual_context_tree->has_empty_effective_clip(state))
        return false;
    m_commands.append({ state, move(command) });
    return true;
}

static Optional<Gfx::IntRect> command_bounding_rectangle(DisplayListCommand const& command)
{
    return command.visit(
        [&](auto const& command) -> Optional<Gfx::IntRect> {
            if constexpr (requires { command.bounding_rect(); })
                return command.bounding_rect();
            else
                return {};
        });
}

static bool command_is_clip(DisplayListCommand const& command)
{
    return command.visit(
        [&](auto const& command) -> bool {
            if constexpr (requires { command.is_clip(); })
                return command.is_clip();
            else
                return false;
        });
}

void DisplayListPlayer::execute(DisplayList& display_list, ScrollStateSnapshot const& scroll_state_snapshot, RefPtr<Gfx::PaintingSurface> surface)
{
    m_surface = surface;
    execute_impl(display_list, scroll_state_snapshot);
    if (surface)
        flush();
    m_surface = nullptr;
}

void DisplayListPlayer::execute_display_list_into_surface(DisplayList& display_list, Gfx::PaintingSurface& target_surface)
{
    TemporaryChange surface_change { m_surface, RefPtr<Gfx::PaintingSurface> { target_surface } };
    ScrollStateSnapshot scroll_state_snapshot;
    execute_impl(display_list, scroll_state_snapshot);
}

void DisplayListPlayer::apply_visual_context_node(AccumulatedVisualContextTree const& visual_context_tree, OrderedVisualContextNode const& ordered_node, ScrollStateSnapshot const& scroll_state)
{
    switch (ordered_node.type) {
    case OrderedVisualContextNode::Type::Effect: {
        auto const& effects = visual_context_tree.node_at(EffectContextIndex(ordered_node.index)).data;
        apply_effects({ .opacity = effects.opacity, .compositing_and_blending_operator = effects.blend_mode, .filter = effects.gfx_filter });
        break;
    }
    case OrderedVisualContextNode::Type::Spatial: {
        auto const& spatial_data = visual_context_tree.node_at(SpatialContextIndex(ordered_node.index)).data;
        auto apply_perspective = [&](PerspectiveData const& perspective) {
            save({});
            apply_transform({ 0, 0 }, perspective.matrix);
        };
        auto apply_scroll = [&](ScrollData const& scroll) {
            save({});
            auto offset = scroll_state.device_offset_for_index(scroll.scroll_frame_index);
            if (!offset.is_zero())
                translate({ .delta = offset.to_type<int>() });
        };
        auto apply_transform_data = [&](TransformData const& transform) {
            save({});
            apply_transform(transform.origin, transform.matrix);
        };
        spatial_data.visit(move(apply_perspective), move(apply_scroll), move(apply_transform_data));
        break;
    }
    case OrderedVisualContextNode::Type::Clip: {
        auto const& clip_data = visual_context_tree.node_at(ClipContextIndex(ordered_node.index)).data;
        auto apply_clip = [&](ClipData const& clip) {
            save({});
            if (clip.corner_radii.has_any_radius())
                add_rounded_rect_clip({ .corner_radii = clip.corner_radii, .border_rect = clip.rect.to_type<int>(), .corner_clip = CornerClip::Outside });
            else
                add_clip_rect({ .rect = clip.rect.to_type<int>() });
        };
        auto apply_clip_path = [&](ClipPathData const& clip_path) {
            save({});
            add_clip_path(clip_path.path);
        };
        clip_data.visit(move(apply_clip), move(apply_clip_path));
        break;
    }
    }
}

void DisplayListPlayer::execute_impl(DisplayList& display_list, ScrollStateSnapshot const& scroll_state)
{
    auto const& commands = display_list.commands();
    auto const& visual_context_tree = display_list.visual_context_tree();

    VERIFY(m_surface);

    Vector<OrderedVisualContextNode, 16> applied_nodes;
    Optional<VisualContextIndex> fully_applied_state { VisualContextIndex {} };

    // OPTIMIZATION: When walking down to apply effects (opacity, filters, blend modes), check culling before applying
    //               each effect. Effects don't affect clip state, so the culling check is valid before applying them.
    //               This avoids expensive saveLayer/restore cycles for off-screen elements with effects like blur.
    enum class SwitchResult : u8 {
        Switched,
        CulledByEffect,
    };
    auto switch_to_state = [&](VisualContextIndex target_state, Optional<Gfx::IntRect> bounding_rect = {}) -> SwitchResult {
        if (fully_applied_state.has_value() && fully_applied_state.value() == target_state)
            return SwitchResult::Switched;

        fully_applied_state.clear();
        auto target_nodes = visual_context_tree.build_ordered_context_chain(target_state);

        size_t common_prefix_length = 0;
        while (common_prefix_length < applied_nodes.size()
            && common_prefix_length < target_nodes.size()
            && applied_nodes[common_prefix_length] == target_nodes[common_prefix_length]) {
            common_prefix_length++;
        }

        while (applied_nodes.size() > common_prefix_length) {
            restore({});
            applied_nodes.take_last();
        }

        auto result = SwitchResult::Switched;
        for (size_t i = common_prefix_length; i < target_nodes.size(); i++) {
            auto const& node = target_nodes[i];
            if (bounding_rect.has_value() && node.type == OrderedVisualContextNode::Type::Effect) {
                if (bounding_rect->is_empty() || would_be_fully_clipped_by_painter(*bounding_rect)) {
                    result = SwitchResult::CulledByEffect;
                    break;
                }
            }
            apply_visual_context_node(visual_context_tree, node, scroll_state);
            applied_nodes.append(node);
        }

        if (result == SwitchResult::Switched)
            fully_applied_state = target_state;

        return result;
    };

    for (size_t command_index = 0; command_index < commands.size(); command_index++) {
        auto const& [state, command] = commands[command_index];

        auto bounding_rect = command_bounding_rectangle(command);

        if (switch_to_state(state, bounding_rect) == SwitchResult::CulledByEffect)
            continue;

        if (command.has<PaintScrollBar>()) {
            auto translated_command = command;
            auto& paint_scroll_bar = translated_command.get<PaintScrollBar>();
            auto device_offset = scroll_state.device_offset_for_index(paint_scroll_bar.scroll_frame_index);
            if (paint_scroll_bar.vertical)
                paint_scroll_bar.thumb_rect.translate_by(0, static_cast<int>(-device_offset.y() * paint_scroll_bar.scroll_size));
            else
                paint_scroll_bar.thumb_rect.translate_by(static_cast<int>(-device_offset.x() * paint_scroll_bar.scroll_size), 0);
            paint_scrollbar(paint_scroll_bar);
            continue;
        }

        if (bounding_rect.has_value() && (bounding_rect->is_empty() || would_be_fully_clipped_by_painter(*bounding_rect))) {
            // Any clip that's located outside of the visible region is equivalent to a simple clip-rect,
            // so replace it with one to avoid doing unnecessary work.
            if (command_is_clip(command)) {
                if (command.has<AddClipRect>()) {
                    add_clip_rect(command.get<AddClipRect>());
                } else {
                    add_clip_rect({ bounding_rect.release_value() });
                }
            }
            continue;
        }

#define HANDLE_COMMAND(command_type, executor_method) \
    if (command.has<command_type>()) {                \
        executor_method(command.get<command_type>()); \
    }

        // clang-format off
        HANDLE_COMMAND(DrawGlyphRun, draw_glyph_run)
        else HANDLE_COMMAND(FillRect, fill_rect)
        else HANDLE_COMMAND(DrawScaledDecodedImageFrame, draw_scaled_decoded_image_frame)
        else HANDLE_COMMAND(DrawRepeatedDecodedImageFrame, draw_repeated_decoded_image_frame)
        else HANDLE_COMMAND(DrawExternalContent, draw_external_content)
        else HANDLE_COMMAND(DrawVideoFrameSource, draw_video_frame_source)
        else HANDLE_COMMAND(AddClipRect, add_clip_rect)
        else HANDLE_COMMAND(Save, save)
        else HANDLE_COMMAND(SaveLayer, save_layer)
        else HANDLE_COMMAND(Restore, restore)
        else HANDLE_COMMAND(Translate, translate)
        else HANDLE_COMMAND(PaintLinearGradient, paint_linear_gradient)
        else HANDLE_COMMAND(PaintRadialGradient, paint_radial_gradient)
        else HANDLE_COMMAND(PaintConicGradient, paint_conic_gradient)
        else HANDLE_COMMAND(PaintOuterBoxShadow, paint_outer_box_shadow)
        else HANDLE_COMMAND(PaintInnerBoxShadow, paint_inner_box_shadow)
        else HANDLE_COMMAND(PaintTextShadow, paint_text_shadow)
        else HANDLE_COMMAND(FillRectWithRoundedCorners, fill_rect_with_rounded_corners)
        else HANDLE_COMMAND(FillPath, fill_path)
        else HANDLE_COMMAND(StrokePath, stroke_path)
        else HANDLE_COMMAND(DrawEllipse, draw_ellipse)
        else HANDLE_COMMAND(FillEllipse, fill_ellipse)
        else HANDLE_COMMAND(DrawLine, draw_line)
        else HANDLE_COMMAND(ApplyBackdropFilter, apply_backdrop_filter)
        else HANDLE_COMMAND(DrawRect, draw_rect)
        else HANDLE_COMMAND(AddRoundedRectClip, add_rounded_rect_clip)
        else HANDLE_COMMAND(PaintNestedDisplayList, paint_nested_display_list)
        else HANDLE_COMMAND(ApplyEffects, apply_effects)
        else VERIFY_NOT_REACHED();
        // clang-format on

#undef HANDLE_COMMAND
    }

    while (!applied_nodes.is_empty()) {
        restore({});
        applied_nodes.take_last();
    }
}

}
