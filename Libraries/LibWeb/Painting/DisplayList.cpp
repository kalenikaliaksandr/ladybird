/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/TemporaryChange.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Painting {

bool DisplayList::append(DisplayListCommand&& command, PaintContext context)
{
    if (context.clip_effect_context_index.value() && m_clip_effect_context_tree->has_empty_effective_clip(context.clip_effect_context_index))
        return false;
    m_commands.append({ context, move(command) });
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
    if (surface) {
        surface->lock_context();
    }
    m_surface = surface;
    execute_impl(display_list, scroll_state_snapshot);
    if (surface)
        flush();
    m_surface = nullptr;
    if (surface) {
        surface->unlock_context();
    }
}

void DisplayListPlayer::execute_display_list_into_surface(DisplayList& display_list, Gfx::PaintingSurface& target_surface)
{
    TemporaryChange surface_change { m_surface, RefPtr<Gfx::PaintingSurface> { target_surface } };
    ScrollStateSnapshot scroll_state_snapshot;
    execute_impl(display_list, scroll_state_snapshot);
}

void DisplayListPlayer::execute_impl(DisplayList& display_list, ScrollStateSnapshot const& scroll_state)
{
    auto const& commands = display_list.commands();
    auto const& spatial_context_tree = display_list.spatial_context_tree();
    auto const& clip_effect_context_tree = display_list.clip_effect_context_tree();

    VERIFY(m_surface);

    auto for_each_spatial_node_from_common_ancestor_to_target = [&](this auto const& self, SpatialContextIndex common_ancestor_index, SpatialContextIndex target_index, auto&& callback) -> void {
        if (!target_index.value() || target_index == common_ancestor_index)
            return;
        self(common_ancestor_index, spatial_context_tree.node_at(target_index).parent_index, callback);
        callback(spatial_context_tree.node_at(target_index));
    };

    auto for_each_clip_effect_node_from_common_ancestor_to_target = [&](this auto const& self, ClipEffectContextIndex common_ancestor_index, ClipEffectContextIndex target_index, auto&& callback) -> void {
        if (!target_index.value() || target_index == common_ancestor_index)
            return;
        self(common_ancestor_index, clip_effect_context_tree.node_at(target_index).parent_index, callback);
        callback(clip_effect_context_tree.node_at(target_index));
    };

    auto apply_spatial_context = [&](SpatialContextNode const& node) {
        node.data.visit(
            [&](PerspectiveData const& perspective) {
                save({});
                apply_transform({ 0, 0 }, perspective.matrix);
            },
            [&](ScrollData const& scroll) {
                save({});
                auto offset = scroll_state.device_offset_for_index(scroll.scroll_frame_index);
                if (!offset.is_zero())
                    translate({ .delta = offset.to_type<int>() });
            },
            [&](TransformData const& transform) {
                save({});
                apply_transform(transform.origin, transform.matrix);
            });
    };

    auto apply_clip_effect_context = [&](ClipEffectContextNode const& node) {
        node.data.visit(
            [&](ClipData const& clip) {
                save({});
                auto clip_matrix = spatial_context_tree.transform_matrix_to_viewport(clip.spatial_context_index, scroll_state);
                if (clip.corner_radii.has_any_radius())
                    add_device_rounded_rect_clip({ .corner_radii = clip.corner_radii, .border_rect = clip.rect.to_type<int>(), .corner_clip = CornerClip::Outside }, clip_matrix);
                else
                    add_device_clip_rect({ .rect = clip.rect.to_type<int>() }, clip_matrix);
            },
            [&](ClipPathData const& clip_path) {
                save({});
                add_device_clip_path(clip_path.path, spatial_context_tree.transform_matrix_to_viewport(clip_path.spatial_context_index, scroll_state));
            },
            [&](EffectsData const& effects) {
                apply_effects({ .opacity = effects.opacity, .compositing_and_blending_operator = effects.blend_mode, .filter = effects.gfx_filter });
            });
    };

    SpatialContextIndex applied_spatial_context_index;
    ClipEffectContextIndex applied_clip_effect_context_index;
    size_t applied_spatial_depth = 0;
    size_t applied_clip_effect_depth = 0;

    auto restore_clip_effect_context_to = [&](ClipEffectContextIndex target_index) {
        auto common_ancestor_depth = target_index.value() ? clip_effect_context_tree.node_at(target_index).depth : 0;
        while (applied_clip_effect_depth > common_ancestor_depth) {
            restore({});
            applied_clip_effect_depth--;
        }
        applied_clip_effect_context_index = target_index;
    };

    auto restore_spatial_context_to = [&](SpatialContextIndex target_index) {
        auto common_ancestor_depth = target_index.value() ? spatial_context_tree.node_at(target_index).depth : 0;
        while (applied_spatial_depth > common_ancestor_depth) {
            restore({});
            applied_spatial_depth--;
        }
        applied_spatial_context_index = target_index;
    };

    auto apply_spatial_context_to = [&](SpatialContextIndex target_index) {
        for_each_spatial_node_from_common_ancestor_to_target(applied_spatial_context_index, target_index, [&](SpatialContextNode const& node) {
            apply_spatial_context(node);
            applied_spatial_depth++;
        });
        applied_spatial_context_index = target_index;
    };

    auto apply_clip_effect_context_to = [&](ClipEffectContextIndex target_index) {
        for_each_clip_effect_node_from_common_ancestor_to_target(applied_clip_effect_context_index, target_index, [&](ClipEffectContextNode const& node) {
            apply_clip_effect_context(node);
            applied_clip_effect_depth++;
        });
        applied_clip_effect_context_index = target_index;
    };

    auto switch_clip_effect_context = [&](ClipEffectContextIndex target_index) {
        if (applied_clip_effect_context_index == target_index)
            return;

        auto common_ancestor_index = clip_effect_context_tree.find_common_ancestor(applied_clip_effect_context_index, target_index);
        restore_clip_effect_context_to(common_ancestor_index);
        apply_clip_effect_context_to(target_index);
    };

    auto switch_to_context = [&](PaintContext target_context) {
        if (applied_spatial_context_index != target_context.spatial_context_index) {
            // Spatial nodes live below clip/effect nodes on the painter save stack, so any spatial
            // change requires tearing down the clip/effect stack first and rebuilding it afterwards.
            restore_clip_effect_context_to({});

            auto common_ancestor_index = spatial_context_tree.find_common_ancestor(applied_spatial_context_index, target_context.spatial_context_index);
            restore_spatial_context_to(common_ancestor_index);
            apply_spatial_context_to(target_context.spatial_context_index);
            apply_clip_effect_context_to(target_context.clip_effect_context_index);
            return;
        }

        switch_clip_effect_context(target_context.clip_effect_context_index);
    };

    for (size_t command_index = 0; command_index < commands.size(); command_index++) {
        auto const& [context, command] = commands[command_index];

        auto bounding_rect = command_bounding_rectangle(command);

        // OPTIMIZATION: If the leaf context is an effect and we're switching to a new context,
        //               check culling before applying it. Effects (opacity, filters, blend modes) don't affect
        //               clip state, so would_be_fully_clipped_by_painter() returns the same result before and after
        //               applying effects.
        //               This avoids expensive saveLayer/restore cycles for off-screen elements with effects like blur.
        // NOTE: We must not do this for consecutive commands with the same context, as that would incorrectly restore
        //       and re-apply the effect layer, breaking blend mode compositing.
        if (context.clip_effect_context_index.value() && applied_clip_effect_context_index != context.clip_effect_context_index && clip_effect_context_tree.is_effect(context.clip_effect_context_index) && bounding_rect.has_value()) {
            switch_to_context({ context.spatial_context_index, clip_effect_context_tree.node_at(context.clip_effect_context_index).parent_index });
            if (bounding_rect->is_empty() || would_be_fully_clipped_by_painter(*bounding_rect))
                continue;
        }

        switch_to_context(context);

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
        else HANDLE_COMMAND(DrawScaledImmutableBitmap, draw_scaled_immutable_bitmap)
        else HANDLE_COMMAND(DrawRepeatedImmutableBitmap, draw_repeated_immutable_bitmap)
        else HANDLE_COMMAND(DrawExternalContent, draw_external_content)
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
    }

    while (applied_clip_effect_depth > 0) {
        restore({});
        applied_clip_effect_depth--;
    }
    while (applied_spatial_depth > 0) {
        restore({});
        applied_spatial_depth--;
    }
}

}
