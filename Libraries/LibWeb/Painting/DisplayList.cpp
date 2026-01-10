/*
 * Copyright (c) 2024-2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/TemporaryChange.h>
#include <LibWeb/Painting/DevicePixelConverter.h>
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Painting {

void DisplayList::append(DisplayListCommand&& command, RefPtr<StackedRenderState const> context)
{
    m_commands.append({ move(context), move(command) });
}

String DisplayList::dump() const
{
    StringBuilder builder;
    int indentation = 0;
    for (auto const& command_list_item : m_commands) {
        auto const& command = command_list_item.command;
        auto const& context = command_list_item.context;

        command.visit([&indentation](auto const& command) {
            if constexpr (requires { command.nesting_level_change; }) {
                if (command.nesting_level_change < 0 && indentation >= -command.nesting_level_change)
                    indentation += command.nesting_level_change;
            }
        });

        if (indentation > 0)
            builder.append(MUST(String::repeated("  "_string, indentation)));
        command.visit([&builder](auto const& cmd) { cmd.dump(builder); });

        if (context) {
            if (auto clip_frame = context->clip_frame()) {
                builder.appendff(", clip_rect={}", clip_frame->clip_rect().rect);
            }
        }
        builder.append('\n');

        command.visit([&indentation](auto const& command) {
            if constexpr (requires { command.nesting_level_change; }) {
                if (command.nesting_level_change > 0)
                    indentation += command.nesting_level_change;
            }
        });
    }
    return builder.to_string_without_validation();
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

static bool command_is_clip_or_mask(DisplayListCommand const& command)
{
    return command.visit(
        [&](auto const& command) -> bool {
            if constexpr (requires { command.is_clip_or_mask(); })
                return command.is_clip_or_mask();
            else
                return false;
        });
}

void DisplayListPlayer::execute(DisplayList& display_list, ScrollStateSnapshotByDisplayList&& scroll_state_snapshot_by_display_list, RefPtr<Gfx::PaintingSurface> surface)
{
    TemporaryChange change { m_scroll_state_snapshots_by_display_list, move(scroll_state_snapshot_by_display_list) };
    if (surface) {
        surface->lock_context();
    }
    auto scroll_state_snapshot = m_scroll_state_snapshots_by_display_list.get(display_list).value_or({});
    execute_impl(display_list, scroll_state_snapshot, surface);
    if (surface) {
        surface->unlock_context();
    }
}

static RefPtr<StackedRenderState const> find_common_ancestor(RefPtr<StackedRenderState const> a, RefPtr<StackedRenderState const> b)
{
    if (!a || !b)
        return {};

    while (a->depth() > b->depth())
        a = a->parent();
    while (b->depth() > a->depth())
        b = b->parent();

    while (a != b) {
        a = a->parent();
        b = b->parent();
    }
    return a;
}

void DisplayListPlayer::execute_impl(DisplayList& display_list, ScrollStateSnapshot const& scroll_state, RefPtr<Gfx::PaintingSurface> surface)
{
    if (surface)
        m_surfaces.append(*surface);
    ScopeGuard guard = [&surfaces = m_surfaces, pop_surface_from_stack = !!surface] {
        if (pop_surface_from_stack)
            (void)surfaces.take_last();
    };

    auto const& commands = display_list.commands();
    auto device_pixels_per_css_pixel = display_list.device_pixels_per_css_pixel();

    DevicePixelConverter device_pixel_converter { device_pixels_per_css_pixel };

    VERIFY(!m_surfaces.is_empty());

    auto for_each_node_from_common_ancestor_to_target = [](this auto const& self, RefPtr<StackedRenderState const> common_ancestor, RefPtr<StackedRenderState const> node, auto&& callback) -> void {
        if (!node || node == common_ancestor)
            return;
        self(common_ancestor, node->parent(), callback);
        callback(*node);
    };

    auto apply_stacked_render_state = [&](StackedRenderState const& node) {
        save({});

        auto clip_frame = node.clip_frame();
        auto scroll_frame_id = node.scroll_frame_id();

        auto apply_clip = [&] {
            auto const& clip = clip_frame->clip_rect();
            auto device_rect = device_pixel_converter.rounded_device_rect(clip.rect).to_type<int>();
            auto corner_radii = clip.corner_radii.as_corners(device_pixel_converter);
            if (corner_radii.has_any_radius())
                add_rounded_rect_clip({ .corner_radii = corner_radii, .border_rect = device_rect, .corner_clip = CornerClip::Outside });
            else
                add_clip_rect({ .rect = device_rect });
        };

        // Scroll container clip: When both clip and scroll are present on the same node,
        // the clip defines the scroll viewport and must be applied BEFORE scroll translation.
        // This is because the clip rect is in the scroll container's border-box coordinates.
        if (clip_frame && scroll_frame_id.has_value())
            apply_clip();

        if (auto const& perspective = node.perspective(); perspective.has_value())
            apply_transform({ .origin = { 0, 0 }, .matrix = perspective.value() });

        if (scroll_frame_id.has_value()) {
            auto own_offset = scroll_state.own_offset_for_frame_with_id(scroll_frame_id.value());
            if (!own_offset.is_zero()) {
                auto scroll_offset = own_offset.to_type<double>().scaled(device_pixels_per_css_pixel).to_type<int>();
                translate({ .delta = scroll_offset });
            }
        }

        if (auto transform_frame = node.transform_frame(); transform_frame) {
            auto origin = transform_frame->origin().to_type<double>().scaled(device_pixels_per_css_pixel).to_type<float>();
            apply_transform({ .origin = { origin.x(), origin.y() }, .matrix = transform_frame->matrix() });
        }

        // Non-scroll clip: When clip is present without scroll, it's something like overflow:hidden
        // or clip-path on a non-scrollable element. These are applied AFTER transforms since they're
        // defined in the element's local coordinate space.
        if (clip_frame && !scroll_frame_id.has_value())
            apply_clip();
    };

    RefPtr<StackedRenderState const> applied_context;
    size_t applied_depth = 0;

    auto switch_to_context = [&](RefPtr<StackedRenderState const> target_context) {
        if (applied_context == target_context)
            return;

        auto common_ancestor = find_common_ancestor(applied_context, target_context);
        auto common_ancestor_depth = common_ancestor ? common_ancestor->depth() : 0;

        while (applied_depth > common_ancestor_depth) {
            restore({});
            applied_depth--;
        }

        for_each_node_from_common_ancestor_to_target(common_ancestor, target_context, [&](StackedRenderState const& node) {
            apply_stacked_render_state(node);
            applied_depth++;
        });

        applied_context = target_context;
    };

    for (size_t command_index = 0; command_index < commands.size(); command_index++) {
        auto [context, command] = commands[command_index];

        switch_to_context(context);

        if (command.has<PaintScrollBar>()) {
            auto& paint_scroll_bar = command.get<PaintScrollBar>();
            auto scroll_offset = scroll_state.own_offset_for_frame_with_id(paint_scroll_bar.scroll_frame_id);
            if (paint_scroll_bar.vertical) {
                auto offset = scroll_offset.y() * paint_scroll_bar.scroll_size;
                paint_scroll_bar.thumb_rect.translate_by(0, -offset.to_int() * device_pixels_per_css_pixel);
            } else {
                auto offset = scroll_offset.x() * paint_scroll_bar.scroll_size;
                paint_scroll_bar.thumb_rect.translate_by(-offset.to_int() * device_pixels_per_css_pixel, 0);
            }
        }

        auto bounding_rect = command_bounding_rectangle(command);

        // FIXME: Reimplement stacking context bounds optimization.
        // The previous implementation computed bounding rects for stacking context children
        // to skip fully-clipped stacking contexts. However, it didn't correctly account for
        // scroll offsets from the context node tree, causing elements inside scrolled
        // stacking contexts to be incorrectly clipped away.

        if (bounding_rect.has_value() && (bounding_rect->is_empty() || would_be_fully_clipped_by_painter(*bounding_rect))) {
            // Any clip or mask that's located outside of the visible region is equivalent to a simple clip-rect,
            // so replace it with one to avoid doing unnecessary work.
            if (command_is_clip_or_mask(command)) {
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
        else HANDLE_COMMAND(DrawPaintingSurface, draw_painting_surface)
        else HANDLE_COMMAND(DrawScaledImmutableBitmap, draw_scaled_immutable_bitmap)
        else HANDLE_COMMAND(DrawRepeatedImmutableBitmap, draw_repeated_immutable_bitmap)
        else HANDLE_COMMAND(AddClipRect, add_clip_rect)
        else HANDLE_COMMAND(Save, save)
        else HANDLE_COMMAND(SaveLayer, save_layer)
        else HANDLE_COMMAND(Restore, restore)
        else HANDLE_COMMAND(Translate, translate)
        else HANDLE_COMMAND(PushStackingContext, push_stacking_context)
        else HANDLE_COMMAND(PopStackingContext, pop_stacking_context)
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
        else HANDLE_COMMAND(AddMask, add_mask)
        else HANDLE_COMMAND(PaintScrollBar, paint_scrollbar)
        else HANDLE_COMMAND(PaintNestedDisplayList, paint_nested_display_list)
        else HANDLE_COMMAND(ApplyOpacity, apply_opacity)
        else HANDLE_COMMAND(ApplyCompositeAndBlendingOperator, apply_composite_and_blending_operator)
        else HANDLE_COMMAND(ApplyFilter, apply_filter)
        else HANDLE_COMMAND(ApplyTransform, apply_transform)
        else HANDLE_COMMAND(ApplyMaskBitmap, apply_mask_bitmap)
        else VERIFY_NOT_REACHED();
        // clang-format on
    }

    // Restore all remaining context
    while (applied_depth > 0) {
        restore({});
        applied_depth--;
    }

    if (surface)
        flush();
}

}
