/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Span.h>
#include <AK/Variant.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/SharedImage.h>

#ifdef AK_OS_MACOS
#    include <LibCore/IOSurface.h>
#elif defined(USE_VULKAN_IMAGES)
#    include <LibGfx/VulkanContext.h>
#    include <LibIPC/File.h>
#endif

namespace Gfx {

class SharedImageBuffer {
    AK_MAKE_NONCOPYABLE(SharedImageBuffer);

public:
    static SharedImageBuffer allocate_for_compositing(IntSize);
    static SharedImageBuffer import_from_shared_image(SharedImage);
#ifdef USE_VULKAN_IMAGES
    static SharedImageBuffer allocate_for_compositing_with_linux_dmabuf(VulkanContext const&, IntSize, ReadonlySpan<uint64_t>);
#endif

    SharedImageBuffer(SharedImageBuffer&&);
    SharedImageBuffer& operator=(SharedImageBuffer&&);
    ~SharedImageBuffer();

    SharedImage export_shared_image() const;

    NonnullRefPtr<Bitmap> bitmap() const;

#ifdef AK_OS_MACOS
    Core::IOSurfaceHandle const& iosurface_handle() const;
#endif

#ifdef USE_VULKAN_IMAGES
    int duplicate_linux_dmabuf_fd() const;
    LinuxDmaBufInfo const& linux_dmabuf_info() const;
    NonnullRefPtr<VulkanImage> vulkan_image() const;
#endif

private:
#ifdef AK_OS_MACOS
    SharedImageBuffer(Core::IOSurfaceHandle&&, NonnullRefPtr<Bitmap>);
    Core::IOSurfaceHandle m_iosurface_handle;
    NonnullRefPtr<Bitmap> m_bitmap;
#else
    explicit SharedImageBuffer(NonnullRefPtr<Bitmap>);
#    ifdef USE_VULKAN_IMAGES
    explicit SharedImageBuffer(NonnullRefPtr<VulkanImage>);
    SharedImageBuffer(IPC::File&&, LinuxDmaBufInfo);

    struct AllocatedLinuxDmaBuf {
        NonnullRefPtr<VulkanImage> image;
        LinuxDmaBufInfo info;
    };

    struct ImportedLinuxDmaBuf {
        IPC::File file;
        LinuxDmaBufInfo info;
    };

    Variant<NonnullRefPtr<Bitmap>, AllocatedLinuxDmaBuf, ImportedLinuxDmaBuf> m_storage;
#    else
    NonnullRefPtr<Bitmap> m_bitmap;
#    endif
#endif
};

}
