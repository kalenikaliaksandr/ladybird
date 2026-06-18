/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <GLES2/gl2.h>

#include <AK/Checked.h>
#include <AK/Debug.h>
#include <LibWeb/WebGL/TextureUpload.h>

namespace Web::WebGL {

Optional<Gfx::ExportFormat> texture_export_format(GLenum format, GLenum type)
{
    switch (format) {
    case GL_RGB:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return Gfx::ExportFormat::RGB888;
        case GL_UNSIGNED_SHORT_5_6_5:
            return Gfx::ExportFormat::RGB565;
        default:
            break;
        }
        break;
    case GL_RGBA:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return Gfx::ExportFormat::RGBA8888;
        case GL_UNSIGNED_SHORT_4_4_4_4:
            // FIXME: This is not exactly the same as RGBA.
            return Gfx::ExportFormat::RGBA4444;
        case GL_UNSIGNED_SHORT_5_5_5_1:
            return Gfx::ExportFormat::RGBA5551;
        default:
            break;
        }
        break;
    case GL_ALPHA:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return Gfx::ExportFormat::Alpha8;
        default:
            break;
        }
        break;
    case GL_LUMINANCE:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return Gfx::ExportFormat::Gray8;
        default:
            break;
        }
        break;
    default:
        break;
    }

    dbgln("WebGL: Unsupported format and type combination. format: 0x{:04x}, type: 0x{:04x}", format, type);
    return {};
}

static Optional<size_t> pixel_size_for_format_and_type(GLenum format, GLenum type)
{
    switch (format) {
    case GL_ALPHA:
    case GL_LUMINANCE:
        return type == GL_UNSIGNED_BYTE ? Optional<size_t> { 1 } : OptionalNone {};
    case GL_LUMINANCE_ALPHA:
        return type == GL_UNSIGNED_BYTE ? Optional<size_t> { 2 } : OptionalNone {};
    case GL_RGB:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return 3;
        case GL_UNSIGNED_SHORT_5_6_5:
            return 2;
        default:
            return {};
        }
    case GL_RGBA:
        switch (type) {
        case GL_UNSIGNED_BYTE:
            return 4;
        case GL_UNSIGNED_SHORT_4_4_4_4:
        case GL_UNSIGNED_SHORT_5_5_5_1:
            return 2;
        default:
            return {};
        }
    default:
        return {};
    }
}

static u8 premultiply_channel(u8 channel, u8 alpha)
{
    return static_cast<u8>((static_cast<u16>(channel) * alpha + 127u) / 255u);
}

static u8 premultiply_channel_to_n_bits(u8 channel, u8 alpha, u8 maximum_value)
{
    return static_cast<u8>((static_cast<u16>(channel) * alpha + maximum_value / 2u) / maximum_value);
}

static void premultiply_row(ByteBuffer& buffer, size_t row_offset, size_t row_bytes, GLenum format, GLenum type)
{
    if (format == GL_LUMINANCE_ALPHA && type == GL_UNSIGNED_BYTE) {
        for (size_t offset = row_offset; offset < row_offset + row_bytes; offset += 2)
            buffer[offset] = premultiply_channel(buffer[offset], buffer[offset + 1]);
        return;
    }

    if (format != GL_RGBA)
        return;

    switch (type) {
    case GL_UNSIGNED_BYTE:
        for (size_t offset = row_offset; offset < row_offset + row_bytes; offset += 4) {
            auto alpha = buffer[offset + 3];
            buffer[offset + 0] = premultiply_channel(buffer[offset + 0], alpha);
            buffer[offset + 1] = premultiply_channel(buffer[offset + 1], alpha);
            buffer[offset + 2] = premultiply_channel(buffer[offset + 2], alpha);
        }
        return;
    case GL_UNSIGNED_SHORT_4_4_4_4:
        for (size_t offset = row_offset; offset < row_offset + row_bytes; offset += 2) {
            u16 pixel;
            __builtin_memcpy(&pixel, buffer.data() + offset, sizeof(pixel));
            auto alpha = static_cast<u8>(pixel & 0x000f);
            auto red = premultiply_channel_to_n_bits(static_cast<u8>((pixel >> 12) & 0x000f), alpha, 15);
            auto green = premultiply_channel_to_n_bits(static_cast<u8>((pixel >> 8) & 0x000f), alpha, 15);
            auto blue = premultiply_channel_to_n_bits(static_cast<u8>((pixel >> 4) & 0x000f), alpha, 15);
            pixel = (red << 12) | (green << 8) | (blue << 4) | alpha;
            __builtin_memcpy(buffer.data() + offset, &pixel, sizeof(pixel));
        }
        return;
    case GL_UNSIGNED_SHORT_5_5_5_1:
        for (size_t offset = row_offset; offset < row_offset + row_bytes; offset += 2) {
            u16 pixel;
            __builtin_memcpy(&pixel, buffer.data() + offset, sizeof(pixel));
            if ((pixel & 0x0001) == 0) {
                pixel = 0;
                __builtin_memcpy(buffer.data() + offset, &pixel, sizeof(pixel));
            }
        }
        return;
    default:
        return;
    }
}

Optional<ByteBuffer> texture_upload_data_for_unpack_parameters(
    ReadonlyBytes source, GLenum format, GLenum type, GLsizei width, GLsizei height,
    u8 unpack_alignment, bool flip_y, bool premultiply_alpha)
{
    if (!flip_y && !premultiply_alpha)
        return {};

    if (width <= 0 || height <= 0)
        return {};

    auto pixel_size = pixel_size_for_format_and_type(format, type);
    if (!pixel_size.has_value())
        return {};

    Checked<size_t> checked_row_bytes = static_cast<size_t>(width);
    checked_row_bytes *= pixel_size.value();
    if (checked_row_bytes.has_overflow())
        return {};

    auto row_bytes = checked_row_bytes.value();
    auto row_stride = align_up_to(row_bytes, static_cast<size_t>(unpack_alignment));
    Checked<size_t> checked_required_size = row_stride;
    checked_required_size *= static_cast<size_t>(height);
    if (checked_required_size.has_overflow())
        return {};
    if (checked_required_size.value() > source.size())
        return {};

    auto buffer = MUST(ByteBuffer::copy(source));
    if (flip_y) {
        for (GLsizei y = 0; y < height; ++y) {
            auto destination_offset = static_cast<size_t>(y) * row_stride;
            auto source_offset = static_cast<size_t>(height - y - 1) * row_stride;
            __builtin_memcpy(buffer.data() + destination_offset, source.data() + source_offset, row_stride);
        }
    }

    if (premultiply_alpha) {
        for (GLsizei y = 0; y < height; ++y)
            premultiply_row(buffer, static_cast<size_t>(y) * row_stride, row_bytes, format, type);
    }

    return buffer;
}

}
