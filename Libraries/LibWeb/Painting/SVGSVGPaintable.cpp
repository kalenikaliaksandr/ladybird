/*
 * Copyright (c) 2018-2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/ImmutableBitmap.h>
#include <LibWeb/Layout/ImageBox.h>
#include <LibWeb/Painting/Blending.h>
#include <LibWeb/Painting/DisplayListRecorder.h>
#include <LibWeb/Painting/SVGSVGPaintable.h>
#include <LibWeb/Painting/StackingContext.h>

namespace Web::Painting {

GC_DEFINE_ALLOCATOR(SVGSVGPaintable);

GC::Ref<SVGSVGPaintable> SVGSVGPaintable::create(Layout::SVGSVGBox const& layout_box)
{
    return layout_box.heap().allocate<SVGSVGPaintable>(layout_box);
}

SVGSVGPaintable::SVGSVGPaintable(Layout::SVGSVGBox const& layout_box)
    : PaintableBox(layout_box)
{
}

void SVGSVGPaintable::paint_svg_box(DisplayListRecordingContext& context, PaintableBox const& svg_box, PaintPhase phase)
{
    auto const& computed_values = svg_box.computed_values();

    auto const& filter = computed_values.filter();
    auto masking_area = svg_box.get_masking_area();

    Gfx::CompositingAndBlendingOperator compositing_and_blending_operator = mix_blend_mode_to_compositing_and_blending_operator(computed_values.mix_blend_mode());

    auto needs_to_save_state = computed_values.isolation() == CSS::Isolation::Isolate || compositing_and_blending_operator != Gfx::CompositingAndBlendingOperator::Normal || masking_area.has_value() || computed_values.opacity() < 1;

    context.display_list_recorder().set_context(svg_box.stacked_render_state());

    if (needs_to_save_state) {
        context.display_list_recorder().save();
    }

    if (computed_values.opacity() < 1) {
        context.display_list_recorder().apply_opacity(computed_values.opacity());
    }

    auto filter_applied = false;
    if (filter.has_filters()) {
        if (auto resolved_filter = svg_box.resolve_filter(context, filter); resolved_filter.has_value()) {
            context.display_list_recorder().apply_filter(*resolved_filter);
            filter_applied = true;
        }
    }

    if (compositing_and_blending_operator != Gfx::CompositingAndBlendingOperator::Normal) {
        context.display_list_recorder().apply_compositing_and_blending_operator(compositing_and_blending_operator);
    }

    bool skip_painting = false;
    if (masking_area.has_value()) {
        if (masking_area->is_empty()) {
            skip_painting = true;
        } else {
            auto mask_bitmap = svg_box.calculate_mask(context, *masking_area);
            if (mask_bitmap) {
                auto source_paintable_rect = context.enclosing_device_rect(*masking_area).template to_type<int>();
                auto origin = source_paintable_rect.location();
                context.display_list_recorder().apply_mask_bitmap(origin, mask_bitmap.release_nonnull(), *svg_box.get_mask_type());
            }
        }
    }

    if (!skip_painting) {
        svg_box.paint(context, PaintPhase::Foreground);
        paint_descendants(context, svg_box, phase);
    }

    if (compositing_and_blending_operator != Gfx::CompositingAndBlendingOperator::Normal) {
        context.display_list_recorder().restore();
    }

    if (filter_applied) {
        context.display_list_recorder().restore();
    }

    if (computed_values.opacity() < 1) {
        context.display_list_recorder().restore();
    }

    if (needs_to_save_state) {
        context.display_list_recorder().restore();
    }
}

void SVGSVGPaintable::paint_descendants(DisplayListRecordingContext& context, PaintableBox const& paintable, PaintPhase phase)
{
    if (phase != PaintPhase::Foreground)
        return;

    paintable.for_each_child_of_type<PaintableBox>([&](PaintableBox& child) {
        paint_svg_box(context, child, phase);
        return IterationDecision::Continue;
    });
}

}
