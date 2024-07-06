/*
 * Copyright (c) 2020, Andreas Kling <kling@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Bitmap.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibGfx/Size.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/File.h>

namespace Gfx {

ShareableBitmap::ShareableBitmap(NonnullRefPtr<Bitmap> bitmap, Tag)
    : m_type(Type::Bitmap)
    , m_bitmap(move(bitmap))
{
}

#ifdef USE_VULKAN
ShareableBitmap::ShareableBitmap(NonnullRefPtr<Core::VulkanImage> vulkan_image)
    : m_type(Type::VulkanImage)
    , m_vulkan_image(move(vulkan_image))
{
}
#endif

}

namespace IPC {

static u8 BITMAP_TYPE = 1;
static u8 VULKAN_IMAGE_TYPE = 2;

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::ShareableBitmap const& shareable_bitmap)
{
    if (shareable_bitmap.is_bitmap()) {
        TRY(encoder.encode(BITMAP_TYPE));
        TRY(encoder.encode(shareable_bitmap.is_valid()));
        if (!shareable_bitmap.is_valid())
            return {};

        auto& bitmap = *shareable_bitmap.bitmap();
        TRY(encoder.encode(TRY(IPC::File::clone_fd(bitmap.anonymous_buffer().fd()))));
        TRY(encoder.encode(bitmap.size()));
        TRY(encoder.encode(static_cast<u32>(bitmap.format())));
        return {};
    }
    VERIFY(shareable_bitmap.is_vulkan_image());
    TRY(encoder.encode(VULKAN_IMAGE_TYPE));
    return {};
}

template<>
ErrorOr<Gfx::ShareableBitmap> decode(Decoder& decoder)
{
    auto type = TRY(decoder.decode<u8>());
    if (type == VULKAN_IMAGE_TYPE) {
        dbgln(">>>>>>GOT VULKAN IMAGE SHAREABLE BITMAP");
        return Gfx::ShareableBitmap {};
    }
    VERIFY(type == BITMAP_TYPE);

    if (auto valid = TRY(decoder.decode<bool>()); !valid)
        return Gfx::ShareableBitmap {};

    auto anon_file = TRY(decoder.decode<IPC::File>());
    auto size = TRY(decoder.decode<Gfx::IntSize>());
    auto raw_bitmap_format = TRY(decoder.decode<u32>());
    if (!Gfx::is_valid_bitmap_format(raw_bitmap_format))
        return Error::from_string_literal("IPC: Invalid Gfx::ShareableBitmap format");

    auto bitmap_format = static_cast<Gfx::BitmapFormat>(raw_bitmap_format);

    auto buffer = TRY(Core::AnonymousBuffer::create_from_anon_fd(anon_file.take_fd(), Gfx::Bitmap::size_in_bytes(Gfx::Bitmap::minimum_pitch(size.width(), bitmap_format), size.height())));
    auto bitmap = TRY(Gfx::Bitmap::create_with_anonymous_buffer(bitmap_format, move(buffer), size));

    return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

}
