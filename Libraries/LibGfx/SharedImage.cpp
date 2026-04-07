/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/SharedImage.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#ifdef USE_VULKAN_IMAGES
#    include <LibIPC/File.h>
#endif

#ifdef AK_OS_MACOS
static Core::MachPort copy_send_right(Core::MachPort const& port)
{
    auto result = mach_port_mod_refs(mach_task_self(), port.port(), MACH_PORT_RIGHT_SEND, +1);
    VERIFY(result == KERN_SUCCESS);
    return Core::MachPort::adopt_right(port.port(), Core::MachPort::PortRight::Send);
}
#endif

namespace Gfx {

#ifdef AK_OS_MACOS
SharedImage::SharedImage(Core::MachPort&& port)
    : m_port(move(port))
{
}
#else
#    ifdef USE_VULKAN_IMAGES
SharedImage::SharedImage(ShareableBitmap shareable_bitmap)
    : m_data(move(shareable_bitmap))
{
}

SharedImage::SharedImage(LinuxDmaBufHandle&& dmabuf)
    : m_data(move(dmabuf))
{
}
#    else
SharedImage::SharedImage(ShareableBitmap shareable_bitmap)
    : m_shareable_bitmap(move(shareable_bitmap))
{
}
#    endif
#endif

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::SharedImage const& shared_image)
{
#ifdef AK_OS_MACOS
    TRY(encoder.append_attachment(Attachment::from_mach_port(copy_send_right(shared_image.m_port), Core::MachPort::MessageRight::MoveSend)));
#else
#    ifdef USE_VULKAN_IMAGES
    if (auto const* shareable_bitmap = shared_image.m_data.get_pointer<Gfx::ShareableBitmap>()) {
        TRY(encoder.encode(false));
        TRY(encoder.encode(*shareable_bitmap));
        return {};
    }

    auto const& dmabuf = shared_image.m_data.get<Gfx::LinuxDmaBufHandle>();
    TRY(encoder.encode(true));
    TRY(encoder.encode(to_underlying(dmabuf.info.bitmap_format)));
    TRY(encoder.encode(to_underlying(dmabuf.info.alpha_type)));
    TRY(encoder.encode(dmabuf.info.size));
    TRY(encoder.encode(dmabuf.info.drm_format));
    TRY(encoder.encode(dmabuf.info.pitch));
    TRY(encoder.encode(dmabuf.info.modifier));
    TRY(encoder.encode(TRY(IPC::File::clone_fd(dmabuf.file.fd()))));
    return {};
#    else
    TRY(encoder.encode(shared_image.m_shareable_bitmap));
#    endif
#endif
    return {};
}

template<>
ErrorOr<Gfx::SharedImage> decode(Decoder& decoder)
{
#ifdef AK_OS_MACOS
    auto attachment = decoder.attachments().dequeue();
    VERIFY(attachment.message_right() == Core::MachPort::MessageRight::MoveSend);
    return Gfx::SharedImage { attachment.release_mach_port() };
#else
#    ifdef USE_VULKAN_IMAGES
    if (TRY(decoder.decode<bool>())) {
        auto raw_bitmap_format = TRY(decoder.decode<u32>());
        auto raw_alpha_type = TRY(decoder.decode<u32>());
        VERIFY(Gfx::is_valid_bitmap_format(raw_bitmap_format));
        VERIFY(Gfx::is_valid_alpha_type(raw_alpha_type));

        auto size = TRY(decoder.decode<Gfx::IntSize>());
        auto drm_format = TRY(decoder.decode<u32>());
        auto pitch = TRY(decoder.decode<size_t>());
        auto modifier = TRY(decoder.decode<u64>());
        auto dmabuf_file = TRY(decoder.decode<IPC::File>());
        return Gfx::SharedImage { Gfx::LinuxDmaBufHandle {
            .file = move(dmabuf_file),
            .info = {
                .bitmap_format = static_cast<Gfx::BitmapFormat>(raw_bitmap_format),
                .alpha_type = static_cast<Gfx::AlphaType>(raw_alpha_type),
                .size = size,
                .drm_format = drm_format,
                .pitch = pitch,
                .modifier = modifier,
            },
        } };
    }
#    endif
    auto shareable_bitmap = TRY(decoder.decode<Gfx::ShareableBitmap>());
    VERIFY(shareable_bitmap.is_valid());
    return Gfx::SharedImage { move(shareable_bitmap) };
#endif
}

}
