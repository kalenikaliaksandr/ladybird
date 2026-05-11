/*
 * Copyright (c) 2024, Lucien Fiorini <lucienfiorini@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <AK/Math.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/ColorSpace.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Filter.h>
#include <LibGfx/SkiaUtils.h>
#include <core/SkBitmap.h>
#include <core/SkBlender.h>
#include <core/SkColorFilter.h>
#include <core/SkColorSpace.h>
#include <core/SkImage.h>
#include <core/SkImageFilter.h>
#include <core/SkString.h>
#include <effects/SkColorMatrix.h>
#include <effects/SkImageFilters.h>
#include <effects/SkPerlinNoiseShader.h>
#include <effects/SkRuntimeEffect.h>

namespace Gfx {

SkPath to_skia_path(Path const& path)
{
    return static_cast<PathImplSkia const&>(path.impl()).sk_path();
}

sk_sp<SkImage> sk_image_from_bitmap(Bitmap const& bitmap, ColorSpace const& color_space)
{
    auto info = SkImageInfo::Make(bitmap.width(), bitmap.height(), to_skia_color_type(bitmap.format()), to_skia_alpha_type(bitmap.format(), bitmap.alpha_type()), color_space.color_space<sk_sp<SkColorSpace>>());
    SkBitmap sk_bitmap;
    sk_bitmap.installPixels(info, const_cast<void*>(static_cast<void const*>(bitmap.scanline(0))), bitmap.pitch());
    sk_bitmap.setImmutable();
    return sk_bitmap.asImage();
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

static sk_sp<SkColorFilter> to_skia_color_filter(Filter::ColorFilter const& filter)
{
    // Matrices are taken from https://drafts.fxtf.org/filter-effects-1/#FilterPrimitiveRepresentation
    switch (filter.type) {
    case ColorFilterType::Grayscale: {
        auto amount = filter.amount;
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
    case ColorFilterType::Brightness: {
        auto amount = filter.amount;
        float matrix[20] = {
            amount, 0, 0, 0, 0,
            0, amount, 0, 0, 0,
            0, 0, amount, 0, 0,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
    }
    case ColorFilterType::Contrast: {
        auto amount = filter.amount;
        float intercept = -(0.5f * amount) + 0.5f;
        float matrix[20] = {
            amount, 0, 0, 0, intercept,
            0, amount, 0, 0, intercept,
            0, 0, amount, 0, intercept,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo);
    }
    case ColorFilterType::Invert: {
        auto amount = filter.amount;
        float matrix[20] = {
            1 - 2 * amount, 0, 0, 0, amount,
            0, 1 - 2 * amount, 0, 0, amount,
            0, 0, 1 - 2 * amount, 0, amount,
            0, 0, 0, 1, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case ColorFilterType::Opacity: {
        auto amount = filter.amount;
        float matrix[20] = {
            1, 0, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 0, 1, 0, 0,
            0, 0, 0, amount, 0
        };
        return SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kYes);
    }
    case ColorFilterType::Sepia: {
        auto amount = filter.amount;
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
    case ColorFilterType::Saturate: {
        auto amount = filter.amount;
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

static sk_sp<SkImageFilter> to_skia_image_filter_impl(Filter const& filter)
{
    Vector<sk_sp<SkImageFilter>> cache;
    cache.resize(filter.nodes().size());

    auto build_node = [&](this auto& self, size_t node_index) -> sk_sp<SkImageFilter> {
        VERIFY(node_index < filter.nodes().size());
        if (cache[node_index])
            return cache[node_index];

        auto resolve_input = [&](Filter::NodeReference input) -> sk_sp<SkImageFilter> {
            VERIFY(input.distance > 0);
            VERIFY(input.distance <= node_index);
            return self(node_index - input.distance);
        };

        auto resolve_optional_input = [&](Optional<Filter::NodeReference> input) -> sk_sp<SkImageFilter> {
            if (!input.has_value())
                return nullptr;
            return resolve_input(*input);
        };

        auto const& node = filter.nodes()[node_index];
        auto image_filter = node.data.visit(
            [&](Filter::Arithmetic const& arithmetic) -> sk_sp<SkImageFilter> {
                return SkImageFilters::Arithmetic(
                    SkFloatToScalar(arithmetic.k1),
                    SkFloatToScalar(arithmetic.k2),
                    SkFloatToScalar(arithmetic.k3),
                    SkFloatToScalar(arithmetic.k4),
                    false,
                    resolve_optional_input(arithmetic.background),
                    resolve_optional_input(arithmetic.foreground));
            },
            [&](Filter::Compose const& compose) -> sk_sp<SkImageFilter> {
                return SkImageFilters::Compose(resolve_input(compose.outer), resolve_input(compose.inner));
            },
            [&](Filter::Blend const& blend) -> sk_sp<SkImageFilter> {
                return SkImageFilters::Blend(
                    to_skia_blender(blend.mode),
                    resolve_optional_input(blend.background),
                    resolve_optional_input(blend.foreground));
            },
            [&](Filter::Flood const& flood) -> sk_sp<SkImageFilter> {
                auto color = to_skia_color(flood.color);
                color = SkColorSetA(color, static_cast<u8>(flood.opacity * 255));
                return SkImageFilters::Shader(SkShaders::Color(color));
            },
            [&](Filter::DisplacementMap const& displacement_map) -> sk_sp<SkImageFilter> {
                return SkImageFilters::DisplacementMap(
                    to_skia_color_channel(displacement_map.x_channel_selector),
                    to_skia_color_channel(displacement_map.y_channel_selector),
                    displacement_map.scale,
                    resolve_optional_input(displacement_map.displacement),
                    resolve_optional_input(displacement_map.color));
            },
            [&](Filter::DropShadow const& drop_shadow) -> sk_sp<SkImageFilter> {
                return SkImageFilters::DropShadow(
                    drop_shadow.offset_x,
                    drop_shadow.offset_y,
                    drop_shadow.radius,
                    drop_shadow.radius,
                    to_skia_color(drop_shadow.color),
                    resolve_optional_input(drop_shadow.input));
            },
            [&](Filter::Blur const& blur) -> sk_sp<SkImageFilter> {
                return SkImageFilters::Blur(
                    blur.radius_x,
                    blur.radius_y,
                    resolve_optional_input(blur.input));
            },
            [&](Filter::ColorFilter const& color_filter) -> sk_sp<SkImageFilter> {
                return SkImageFilters::ColorFilter(
                    to_skia_color_filter(color_filter),
                    resolve_optional_input(color_filter.input));
            },
            [&](Filter::ColorMatrix const& color_matrix) -> sk_sp<SkImageFilter> {
                return SkImageFilters::ColorFilter(
                    SkColorFilters::Matrix(color_matrix.matrix.data()),
                    resolve_optional_input(color_matrix.input));
            },
            [&](Filter::ColorTable const& color_table) -> sk_sp<SkImageFilter> {
                auto input = resolve_optional_input(color_table.input);
                auto* a_table = color_table.a.has_value() ? color_table.a->data() : nullptr;
                auto* r_table = color_table.r.has_value() ? color_table.r->data() : nullptr;
                auto* g_table = color_table.g.has_value() ? color_table.g->data() : nullptr;
                auto* b_table = color_table.b.has_value() ? color_table.b->data() : nullptr;

                // Color tables are applied in linear space by default, so we need to convert twice.
                // FIXME: support sRGB space as well (i.e. don't perform these conversions).
                auto srgb_to_linear = SkImageFilters::ColorFilter(SkColorFilters::SRGBToLinearGamma(), input);
                auto table = SkImageFilters::ColorFilter(SkColorFilters::TableARGB(a_table, r_table, g_table, b_table), srgb_to_linear);
                return SkImageFilters::ColorFilter(SkColorFilters::LinearToSRGBGamma(), table);
            },
            [&](Filter::Saturate const& saturate) -> sk_sp<SkImageFilter> {
                SkColorMatrix matrix;
                matrix.setSaturation(saturate.value);
                return SkImageFilters::ColorFilter(
                    SkColorFilters::Matrix(matrix),
                    resolve_optional_input(saturate.input));
            },
            [&](Filter::HueRotate const& hue_rotate) -> sk_sp<SkImageFilter> {
                float radians = AK::to_radians(hue_rotate.angle_degrees);

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

                return SkImageFilters::ColorFilter(
                    SkColorFilters::Matrix(matrix, SkColorFilters::Clamp::kNo),
                    resolve_optional_input(hue_rotate.input));
            },
            [&](Filter::Image const& image_filter) -> sk_sp<SkImageFilter> {
                auto image = sk_image_from_bitmap(image_filter.frame.bitmap(), image_filter.frame.color_space());
                return SkImageFilters::Image(
                    move(image),
                    to_skia_rect(image_filter.src_rect),
                    to_skia_rect(image_filter.dest_rect),
                    to_skia_sampling_options(image_filter.scaling_mode));
            },
            [&](Filter::Merge const& merge) -> sk_sp<SkImageFilter> {
                Vector<sk_sp<SkImageFilter>> inputs;
                inputs.ensure_capacity(merge.inputs.size());
                for (auto input : merge.inputs)
                    inputs.unchecked_append(resolve_optional_input(input));
                return SkImageFilters::Merge(inputs.data(), inputs.size());
            },
            [&](Filter::Offset const& offset) -> sk_sp<SkImageFilter> {
                return SkImageFilters::Offset(
                    offset.dx,
                    offset.dy,
                    resolve_optional_input(offset.input));
            },
            [&](Filter::Morphology const& morphology) -> sk_sp<SkImageFilter> {
                switch (morphology.morphology_operator) {
                case MorphologyOperator::Erode:
                    return SkImageFilters::Erode(
                        morphology.radius_x,
                        morphology.radius_y,
                        resolve_optional_input(morphology.input));
                case MorphologyOperator::Dilate:
                    return SkImageFilters::Dilate(
                        morphology.radius_x,
                        morphology.radius_y,
                        resolve_optional_input(morphology.input));
                case MorphologyOperator::Unknown:
                    VERIFY_NOT_REACHED();
                }
                VERIFY_NOT_REACHED();
            },
            [&](Filter::Turbulence const& turbulence) -> sk_sp<SkImageFilter> {
                auto tile_stitch_size = SkISize::Make(turbulence.tile_stitch_size.width(), turbulence.tile_stitch_size.height());
                sk_sp<SkShader> turbulence_shader;
                switch (turbulence.turbulence_type) {
                case TurbulenceType::Turbulence:
                    turbulence_shader = SkShaders::MakeTurbulence(turbulence.base_frequency_x, turbulence.base_frequency_y, turbulence.num_octaves, turbulence.seed, &tile_stitch_size);
                    break;
                case TurbulenceType::FractalNoise:
                    turbulence_shader = SkShaders::MakeFractalNoise(turbulence.base_frequency_x, turbulence.base_frequency_y, turbulence.num_octaves, turbulence.seed, &tile_stitch_size);
                    break;
                }
                return SkImageFilters::Shader(move(turbulence_shader));
            });

        cache[node_index] = image_filter;
        return image_filter;
    };

    return build_node(filter.root_node_index());
}

sk_sp<SkImageFilter> to_skia_image_filter(Gfx::Filter const& filter)
{
    return to_skia_image_filter_impl(filter);
}

sk_sp<SkBlender> to_skia_blender(Gfx::CompositingAndBlendingOperator compositing_and_blending_operator)
{
    switch (compositing_and_blending_operator) {
    case CompositingAndBlendingOperator::Normal:
        return SkBlender::Mode(SkBlendMode::kSrcOver);
    case CompositingAndBlendingOperator::Multiply:
        return SkBlender::Mode(SkBlendMode::kMultiply);
    case CompositingAndBlendingOperator::Screen:
        return SkBlender::Mode(SkBlendMode::kScreen);
    case CompositingAndBlendingOperator::Overlay:
        return SkBlender::Mode(SkBlendMode::kOverlay);
    case CompositingAndBlendingOperator::Darken:
        return SkBlender::Mode(SkBlendMode::kDarken);
    case CompositingAndBlendingOperator::Lighten:
        return SkBlender::Mode(SkBlendMode::kLighten);
    case CompositingAndBlendingOperator::ColorDodge:
        return SkBlender::Mode(SkBlendMode::kColorDodge);
    case CompositingAndBlendingOperator::ColorBurn:
        return SkBlender::Mode(SkBlendMode::kColorBurn);
    case CompositingAndBlendingOperator::HardLight:
        return SkBlender::Mode(SkBlendMode::kHardLight);
    case CompositingAndBlendingOperator::SoftLight:
        return SkBlender::Mode(SkBlendMode::kSoftLight);
    case CompositingAndBlendingOperator::Difference:
        return SkBlender::Mode(SkBlendMode::kDifference);
    case CompositingAndBlendingOperator::Exclusion:
        return SkBlender::Mode(SkBlendMode::kExclusion);
    case CompositingAndBlendingOperator::Hue:
        return SkBlender::Mode(SkBlendMode::kHue);
    case CompositingAndBlendingOperator::Saturation:
        return SkBlender::Mode(SkBlendMode::kSaturation);
    case CompositingAndBlendingOperator::Color:
        return SkBlender::Mode(SkBlendMode::kColor);
    case CompositingAndBlendingOperator::Luminosity:
        return SkBlender::Mode(SkBlendMode::kLuminosity);
    case CompositingAndBlendingOperator::Clear:
        return SkBlender::Mode(SkBlendMode::kClear);
    case CompositingAndBlendingOperator::Copy:
        return SkBlender::Mode(SkBlendMode::kSrc);
    case CompositingAndBlendingOperator::SourceOver:
        return SkBlender::Mode(SkBlendMode::kSrcOver);
    case CompositingAndBlendingOperator::DestinationOver:
        return SkBlender::Mode(SkBlendMode::kDstOver);
    case CompositingAndBlendingOperator::SourceIn:
        return SkBlender::Mode(SkBlendMode::kSrcIn);
    case CompositingAndBlendingOperator::DestinationIn:
        return SkBlender::Mode(SkBlendMode::kDstIn);
    case CompositingAndBlendingOperator::SourceOut:
        return SkBlender::Mode(SkBlendMode::kSrcOut);
    case CompositingAndBlendingOperator::DestinationOut:
        return SkBlender::Mode(SkBlendMode::kDstOut);
    case CompositingAndBlendingOperator::SourceATop:
        return SkBlender::Mode(SkBlendMode::kSrcATop);
    case CompositingAndBlendingOperator::DestinationATop:
        return SkBlender::Mode(SkBlendMode::kDstATop);
    case CompositingAndBlendingOperator::Xor:
        return SkBlender::Mode(SkBlendMode::kXor);
    case CompositingAndBlendingOperator::Lighter:
        return SkBlender::Mode(SkBlendMode::kPlus);
    case CompositingAndBlendingOperator::PlusDarker:
        // https://drafts.fxtf.org/compositing/#porterduffcompositingoperators_plus_darker
        // FIXME: This does not match the spec, however it looks like Safari, the only popular browser supporting this operator.
        return SkRuntimeEffect::MakeForBlender(SkString(R"(
            vec4 main(vec4 source, vec4 destination) {
                return saturate(saturate(destination.a + source.a) - saturate(destination.a - destination) - saturate(source.a - source));
            }
        )"))
            .effect->makeBlender(nullptr);
    case CompositingAndBlendingOperator::PlusLighter:
        // https://drafts.fxtf.org/compositing/#porterduffcompositingoperators_plus_lighter
        return SkRuntimeEffect::MakeForBlender(SkString(R"(
            vec4 main(vec4 source, vec4 destination) {
                return saturate(source + destination);
            }
        )"))
            .effect->makeBlender(nullptr);
    default:
        VERIFY_NOT_REACHED();
    }
}

}
