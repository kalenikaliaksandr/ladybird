/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/OwnPtr.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/Color.h>
#include <LibGfx/Filter.h>
#include <LibGfx/Rect.h>
#include <LibGfx/ScalingMode.h>

namespace Gfx {

// Serializable description of a Filter construction tree.
// Each node describes a factory call with its parameters and optional inputs.
struct FilterDescription {
    struct Blur {
        float radius_x;
        float radius_y;
    };

    struct ColorFilter {
        ColorFilterType type;
        float amount;
    };

    struct ColorMatrix {
        float matrix[20];
    };

    struct DropShadow {
        float offset_x;
        float offset_y;
        float radius;
        Color color;
    };

    struct Compose {
        // outer and inner are indices into the description's node list
    };

    struct Blend {
        CompositingAndBlendingOperator mode;
    };

    struct Flood {
        Color color;
        float opacity;
    };

    struct Saturate {
        float value;
    };

    struct HueRotate {
        float angle_degrees;
    };

    struct Offset {
        float dx;
        float dy;
    };

    struct Erode {
        float radius_x;
        float radius_y;
    };

    struct Dilate {
        float radius_x;
        float radius_y;
    };

    struct Merge {
        // inputs are indices into the node list
    };

    struct Arithmetic {
        float k1, k2, k3, k4;
    };

    struct Turbulence {
        TurbulenceType type;
        float base_frequency_x;
        float base_frequency_y;
        i32 num_octaves;
        float seed;
        IntSize tile_stitch_size;
    };

    // Placeholder for unsupported filter types
    struct Unsupported { };

    using Data = Variant<Blur, ColorFilter, ColorMatrix, DropShadow, Compose, Blend, Flood,
        Saturate, HueRotate, Offset, Erode, Dilate, Merge, Arithmetic, Turbulence, Unsupported>;

    Data data;
    Vector<Optional<size_t>> input_indices; // indices into a flat node array

    // Reconstruct a Filter from a description tree (flat array of nodes)
    static Optional<Filter> reconstruct(ReadonlySpan<FilterDescription> nodes, size_t root_index);
};

}
