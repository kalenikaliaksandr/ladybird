/*
 * Copyright (c) 2024, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Bitmap.h>
#include <LibGfx/BitmapExport.h>
#include <LibGfx/ColorSpace.h>
#include <LibGfx/SkiaUtils.h>

#include <core/SkCanvas.h>
#include <core/SkColorSpace.h>
#include <core/SkImage.h>
#include <core/SkSurface.h>

namespace Gfx {

StringView export_format_name(ExportFormat format)
{
    switch (format) {
#define ENUMERATE_EXPORT_FORMAT(format) \
    case Gfx::ExportFormat::format:     \
        return #format##sv;
        ENUMERATE_EXPORT_FORMATS(ENUMERATE_EXPORT_FORMAT)
#undef ENUMERATE_EXPORT_FORMAT
    }
    VERIFY_NOT_REACHED();
}

static int bytes_per_pixel_for_export_format(ExportFormat format)
{
    switch (format) {
    case ExportFormat::Gray8:
    case ExportFormat::Alpha8:
        return 1;
    case ExportFormat::RGB565:
    case ExportFormat::RGBA5551:
    case ExportFormat::RGBA4444:
        return 2;
    case ExportFormat::RGB888:
        return 3;
    case ExportFormat::RGBA8888:
        return 4;
    default:
        VERIFY_NOT_REACHED();
    }
}

static SkColorType export_format_to_skia_color_type(ExportFormat format)
{
    switch (format) {
    case ExportFormat::Gray8:
        return SkColorType::kGray_8_SkColorType;
    case ExportFormat::Alpha8:
        return SkColorType::kAlpha_8_SkColorType;
    case ExportFormat::RGB565:
        return SkColorType::kRGB_565_SkColorType;
    case ExportFormat::RGBA5551:
        // This one needs to be converted manually because Skia has no valid
        // RGBA5551 color type.
        VERIFY_NOT_REACHED();
    case ExportFormat::RGBA4444:
        return SkColorType::kARGB_4444_SkColorType;
    case ExportFormat::RGB888:
        // This one needs to be converted manually because Skia has no valid 24-bit color type.
        VERIFY_NOT_REACHED();
    case ExportFormat::RGBA8888:
        return SkColorType::kRGBA_8888_SkColorType;
    default:
        VERIFY_NOT_REACHED();
    }
}

static u8 quantize_to_n_bits(u8 value, u8 bits)
{
    VERIFY(bits > 0);
    VERIFY(bits <= 8);
    auto maximum_value = (1u << bits) - 1u;
    return static_cast<u8>((static_cast<u16>(value) * maximum_value + 127u) / 255u);
}

static u8 premultiply_channel(u8 channel, u8 alpha)
{
    return static_cast<u8>((static_cast<u16>(channel) * alpha + 127u) / 255u);
}

static u8 unpremultiply_channel(u8 channel, u8 alpha)
{
    if (alpha == 0)
        return 0;
    return min(255u, (static_cast<u16>(channel) * 255u + alpha / 2u) / alpha);
}

static Color color_for_export(Bitmap const& bitmap, int x, int y, bool premultiply_alpha)
{
    auto color = bitmap.get_pixel(x, y);
    auto source_is_premultiplied = bitmap.alpha_type() == AlphaType::Premultiplied;
    if (source_is_premultiplied == premultiply_alpha)
        return color;

    if (premultiply_alpha) {
        auto alpha = color.alpha();
        return Color {
            premultiply_channel(color.red(), alpha),
            premultiply_channel(color.green(), alpha),
            premultiply_channel(color.blue(), alpha),
            alpha,
        };
    }

    auto alpha = color.alpha();
    return Color {
        unpremultiply_channel(color.red(), alpha),
        unpremultiply_channel(color.green(), alpha),
        unpremultiply_channel(color.blue(), alpha),
        alpha,
    };
}

static u16 pack_color_as_rgba5551(Color color)
{
    return (quantize_to_n_bits(color.red(), 5) << 11)
        | (quantize_to_n_bits(color.green(), 5) << 6)
        | (quantize_to_n_bits(color.blue(), 5) << 1)
        | quantize_to_n_bits(color.alpha(), 1);
}

ErrorOr<BitmapExportResult> export_bitmap_to_byte_buffer(
    Bitmap const& bitmap,
    ColorSpace const& color_space,
    ExportFormat format,
    int flags,
    Optional<int> target_width,
    Optional<int> target_height)
{
    int width = target_width.value_or(bitmap.width());
    int height = target_height.value_or(bitmap.height());

    if (format == ExportFormat::RGB888 && (width != bitmap.width() || height != bitmap.height())) {
        dbgln("FIXME: Ignoring target width and height because scaling is not implemented for this export format.");
        width = bitmap.width();
        height = bitmap.height();
    }

    Checked<size_t> buffer_pitch = width;
    int number_of_bytes = bytes_per_pixel_for_export_format(format);
    buffer_pitch *= number_of_bytes;
    if (buffer_pitch.has_overflow())
        return Error::from_string_literal("Gfx::export_bitmap_to_byte_buffer size overflow");

    if (Checked<size_t>::multiplication_would_overflow(buffer_pitch.value(), height))
        return Error::from_string_literal("Gfx::export_bitmap_to_byte_buffer size overflow");

    auto buffer = MUST(ByteBuffer::create_zeroed(buffer_pitch.value() * height));

    if (width > 0 && height > 0) {
        if (format == ExportFormat::RGB888) {
            // 24 bit RGB is not supported by Skia, so we need to handle this format ourselves.
            auto* raw_buffer = buffer.data();
            for (auto y = 0; y < height; y++) {
                auto target_y = flags & ExportFlags::FlipY ? height - y - 1 : y;
                for (auto x = 0; x < width; x++) {
                    auto pixel = bitmap.get_pixel(x, y);
                    auto buffer_offset = (target_y * buffer_pitch.value()) + (x * 3ull);
                    raw_buffer[buffer_offset + 0] = pixel.red();
                    raw_buffer[buffer_offset + 1] = pixel.green();
                    raw_buffer[buffer_offset + 2] = pixel.blue();
                }
            }
        } else if (format == ExportFormat::RGBA5551) {
            VERIFY(bitmap.width() > 0);
            VERIFY(bitmap.height() > 0);

            auto* raw_buffer = buffer.data();
            for (auto y = 0; y < height; y++) {
                auto source_y = y * bitmap.height() / height;
                auto target_y = flags & ExportFlags::FlipY ? height - y - 1 : y;
                for (auto x = 0; x < width; x++) {
                    auto source_x = x * bitmap.width() / width;
                    auto color = color_for_export(bitmap, source_x, source_y, flags & ExportFlags::PremultiplyAlpha);
                    auto packed_pixel = pack_color_as_rgba5551(color);
                    auto buffer_offset = (target_y * buffer_pitch.value()) + (x * 2ull);
                    __builtin_memcpy(raw_buffer + buffer_offset, &packed_pixel, sizeof(packed_pixel));
                }
            }
        } else {
            auto image = sk_image_from_bitmap(bitmap, color_space);
            if (!image)
                return Error::from_string_literal("Failed to create a Skia image for this Bitmap");

            auto skia_format = export_format_to_skia_color_type(format);
            auto skia_color_space = SkColorSpace::MakeSRGB();

            auto image_info = SkImageInfo::Make(
                width,
                height,
                skia_format,
                flags & ExportFlags::PremultiplyAlpha ? SkAlphaType::kPremul_SkAlphaType : SkAlphaType::kUnpremul_SkAlphaType,
                skia_color_space);
            auto surface = SkSurfaces::WrapPixels(image_info, buffer.data(), buffer_pitch.value());
            VERIFY(surface);
            auto* surface_canvas = surface->getCanvas();
            auto dst_rect = Gfx::to_skia_rect(Gfx::Rect { 0, 0, width, height });

            if (flags & ExportFlags::FlipY) {
                surface_canvas->translate(0, dst_rect.height());
                surface_canvas->scale(1, -1);
            }

            surface_canvas->drawImageRect(
                image.get(),
                dst_rect,
                Gfx::to_skia_sampling_options(Gfx::ScalingMode::NearestNeighbor));
        }
    } else {
        VERIFY(buffer.is_empty());
    }

    return BitmapExportResult {
        .buffer = move(buffer),
        .width = width,
        .height = height,
    };
}

}
