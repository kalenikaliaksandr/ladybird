/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/SharedImage.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#ifdef USE_VULKAN_DMABUF_IMAGES
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
#    ifdef USE_VULKAN_DMABUF_IMAGES
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

#ifdef USE_VULKAN_DMABUF_IMAGES
enum class SharedImageBackingType : u8 {
    ShareableBitmap,
    LinuxDmaBuf,
};

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::LinuxDmaBufInfo const& dmabuf_info)
{
    TRY(encoder.encode(dmabuf_info.bitmap_format));
    TRY(encoder.encode(dmabuf_info.alpha_type));
    TRY(encoder.encode(dmabuf_info.size));
    TRY(encoder.encode(dmabuf_info.drm_format));
    TRY(encoder.encode(dmabuf_info.pitch));
    TRY(encoder.encode(dmabuf_info.modifier));
    return {};
}

template<>
ErrorOr<Gfx::LinuxDmaBufInfo> decode(Decoder& decoder)
{
    auto dmabuf_info = Gfx::LinuxDmaBufInfo {
        .bitmap_format = TRY(decoder.decode<Gfx::BitmapFormat>()),
        .alpha_type = TRY(decoder.decode<Gfx::AlphaType>()),
        .size = TRY(decoder.decode<Gfx::IntSize>()),
        .drm_format = TRY(decoder.decode<u32>()),
        .pitch = TRY(decoder.decode<size_t>()),
        .modifier = TRY(decoder.decode<u64>()),
    };
    VERIFY(Gfx::is_valid_bitmap_format(to_underlying(dmabuf_info.bitmap_format)));
    VERIFY(Gfx::is_valid_alpha_type(to_underlying(dmabuf_info.alpha_type)));
    return dmabuf_info;
}

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::LinuxDmaBufHandle const& dmabuf)
{
    TRY(encoder.encode(dmabuf.info));
    TRY(encoder.encode(TRY(IPC::File::clone_fd(dmabuf.file.fd()))));
    return {};
}

template<>
ErrorOr<Gfx::LinuxDmaBufHandle> decode(Decoder& decoder)
{
    auto info = TRY(decoder.decode<Gfx::LinuxDmaBufInfo>());
    auto file = TRY(decoder.decode<IPC::File>());
    return Gfx::LinuxDmaBufHandle {
        .file = move(file),
        .info = move(info),
    };
}
#endif

template<>
ErrorOr<void> encode(Encoder& encoder, Gfx::SharedImage const& shared_image)
{
#ifdef AK_OS_MACOS
    TRY(encoder.append_attachment(Attachment::from_mach_port(copy_send_right(shared_image.m_port), Core::MachPort::MessageRight::MoveSend)));
#else
#    ifdef USE_VULKAN_DMABUF_IMAGES
    if (auto const* shareable_bitmap = shared_image.m_data.get_pointer<Gfx::ShareableBitmap>()) {
        TRY(encoder.encode(SharedImageBackingType::ShareableBitmap));
        TRY(encoder.encode(*shareable_bitmap));
        return {};
    }

    TRY(encoder.encode(SharedImageBackingType::LinuxDmaBuf));
    TRY(encoder.encode(shared_image.m_data.get<Gfx::LinuxDmaBufHandle>()));
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
#    ifdef USE_VULKAN_DMABUF_IMAGES
    switch (TRY(decoder.decode<SharedImageBackingType>())) {
    case SharedImageBackingType::ShareableBitmap:
        break;
    case SharedImageBackingType::LinuxDmaBuf:
        return Gfx::SharedImage { TRY(decoder.decode<Gfx::LinuxDmaBufHandle>()) };
    }
#    endif
    auto shareable_bitmap = TRY(decoder.decode<Gfx::ShareableBitmap>());
    VERIFY(shareable_bitmap.is_valid());
    return Gfx::SharedImage { move(shareable_bitmap) };
#endif
}

}
