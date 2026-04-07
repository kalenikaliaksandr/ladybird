/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Noncopyable.h>
#include <AK/Variant.h>
#include <LibIPC/Forward.h>

#ifdef AK_OS_MACOS
#    include <LibCore/MachPort.h>
#else
#    include <LibGfx/Bitmap.h>
#    include <LibGfx/ShareableBitmap.h>
#    ifdef USE_VULKAN_DMABUF_IMAGES
#        include <LibIPC/File.h>
#    endif
#endif

namespace Gfx {

class SharedImageBuffer;

#ifdef USE_VULKAN_DMABUF_IMAGES
struct LinuxDmaBufInfo {
    BitmapFormat bitmap_format { BitmapFormat::Invalid };
    AlphaType alpha_type { AlphaType::Premultiplied };
    IntSize size;
    u32 drm_format { 0 };
    size_t pitch { 0 };
    u64 modifier { 0 };
};

struct LinuxDmaBufHandle {
    IPC::File file;
    LinuxDmaBufInfo info;
};
#endif

class SharedImage {
    AK_MAKE_NONCOPYABLE(SharedImage);

public:
    SharedImage(SharedImage&&) = default;
    SharedImage& operator=(SharedImage&&) = default;
    ~SharedImage() = default;

private:
#ifdef AK_OS_MACOS
    explicit SharedImage(Core::MachPort&&);
    Core::MachPort m_port;
#else
    explicit SharedImage(ShareableBitmap);
#    ifdef USE_VULKAN_DMABUF_IMAGES
    explicit SharedImage(LinuxDmaBufHandle&&);
    Variant<ShareableBitmap, LinuxDmaBufHandle> m_data;
#    else
    ShareableBitmap m_shareable_bitmap;
#    endif
#endif

    friend class SharedImageBuffer;

    template<typename U>
    friend ErrorOr<void> IPC::encode(IPC::Encoder&, U const&);

    template<typename U>
    friend ErrorOr<U> IPC::decode(IPC::Decoder&);
};

}

namespace IPC {

#ifdef USE_VULKAN_DMABUF_IMAGES
template<>
ErrorOr<void> encode(Encoder&, Gfx::LinuxDmaBufInfo const&);

template<>
ErrorOr<Gfx::LinuxDmaBufInfo> decode(Decoder&);

template<>
ErrorOr<void> encode(Encoder&, Gfx::LinuxDmaBufHandle const&);

template<>
ErrorOr<Gfx::LinuxDmaBufHandle> decode(Decoder&);
#endif

template<>
ErrorOr<void> encode(Encoder&, Gfx::SharedImage const&);

template<>
ErrorOr<Gfx::SharedImage> decode(Decoder&);

}
