/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/FilterDescription.h>

namespace Gfx {

static Optional<Filter> reconstruct_node(ReadonlySpan<FilterDescription> nodes, size_t index)
{
    if (index >= nodes.size())
        return {};

    auto const& node = nodes[index];

    // Resolve input filters recursively
    auto resolve_input = [&](size_t input_idx) -> Optional<Filter> {
        if (input_idx < node.input_indices.size() && node.input_indices[input_idx].has_value())
            return reconstruct_node(nodes, *node.input_indices[input_idx]);
        return {};
    };

    return node.data.visit(
        [&](FilterDescription::Blur const& blur) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::blur(blur.radius_x, blur.radius_y, input);
        },
        [&](FilterDescription::ColorFilter const& cf) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::color(cf.type, cf.amount, input);
        },
        [&](FilterDescription::ColorMatrix const& cm) -> Optional<Filter> {
            auto input = resolve_input(0);
            float matrix[20];
            memcpy(matrix, cm.matrix, sizeof(matrix));
            return Filter::color_matrix(matrix, input);
        },
        [&](FilterDescription::DropShadow const& ds) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::drop_shadow(ds.offset_x, ds.offset_y, ds.radius, ds.color, input);
        },
        [&](FilterDescription::Compose const&) -> Optional<Filter> {
            auto outer = resolve_input(0);
            auto inner = resolve_input(1);
            if (!outer.has_value() || !inner.has_value())
                return {};
            return Filter::compose(*outer, *inner);
        },
        [&](FilterDescription::Blend const& blend) -> Optional<Filter> {
            auto bg = resolve_input(0);
            auto fg = resolve_input(1);
            return Filter::blend(bg, fg, blend.mode);
        },
        [&](FilterDescription::Flood const& flood) -> Optional<Filter> {
            return Filter::flood(flood.color, flood.opacity);
        },
        [&](FilterDescription::Saturate const& sat) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::saturate(sat.value, input);
        },
        [&](FilterDescription::HueRotate const& hr) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::hue_rotate(hr.angle_degrees, input);
        },
        [&](FilterDescription::Offset const& offset) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::offset(offset.dx, offset.dy, input);
        },
        [&](FilterDescription::Erode const& erode) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::erode(erode.radius_x, erode.radius_y, input);
        },
        [&](FilterDescription::Dilate const& dilate) -> Optional<Filter> {
            auto input = resolve_input(0);
            return Filter::dilate(dilate.radius_x, dilate.radius_y, input);
        },
        [&](FilterDescription::Merge const&) -> Optional<Filter> {
            Vector<Optional<Filter>> inputs;
            for (size_t i = 0; i < node.input_indices.size(); ++i)
                inputs.append(resolve_input(i));
            return Filter::merge(inputs);
        },
        [&](FilterDescription::Arithmetic const& arith) -> Optional<Filter> {
            auto bg = resolve_input(0);
            auto fg = resolve_input(1);
            return Filter::arithmetic(bg, fg, arith.k1, arith.k2, arith.k3, arith.k4);
        },
        [&](FilterDescription::Turbulence const& turb) -> Optional<Filter> {
            return Filter::turbulence(turb.type, turb.base_frequency_x, turb.base_frequency_y,
                turb.num_octaves, turb.seed, turb.tile_stitch_size);
        },
        [&](FilterDescription::Unsupported const&) -> Optional<Filter> {
            return {};
        });
}

Optional<Filter> FilterDescription::reconstruct(ReadonlySpan<FilterDescription> nodes, size_t root_index)
{
    return reconstruct_node(nodes, root_index);
}

}
