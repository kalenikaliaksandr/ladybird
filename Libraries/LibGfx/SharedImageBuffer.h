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
#elif defined(USE_VULKAN_DMABUF_IMAGES)
#    include <LibGfx/VulkanImage.h>
#    include <LibIPC/File.h>
#endif

namespace Gfx {

class SharedImageBuffer {
    AK_MAKE_NONCOPYABLE(SharedImageBuffer);

public:
    static SharedImageBuffer create(IntSize);
    static SharedImageBuffer import_from_shared_image(SharedImage);
#ifdef USE_VULKAN_DMABUF_IMAGES
    static SharedImageBuffer allocate_for_compositing_with_linux_dmabuf(VulkanContext const&, IntSize, ReadonlySpan<uint64_t>);
#endif

    SharedImageBuffer(SharedImageBuffer&&);
    SharedImageBuffer& operator=(SharedImageBuffer&&);
    ~SharedImageBuffer();

    SharedImage export_shared_image() const;

#ifdef USE_VULKAN_DMABUF_IMAGES
    NonnullRefPtr<Bitmap> bitmap() const;
#else
    NonnullRefPtr<Bitmap> bitmap() const { return m_bitmap; }
#endif

#ifdef AK_OS_MACOS
    Core::IOSurfaceHandle const& iosurface_handle() const { return m_iosurface_handle; }
#endif

#ifdef USE_VULKAN_DMABUF_IMAGES
    LinuxDmaBufHandle duplicate_linux_dmabuf_handle() const;
    NonnullRefPtr<VulkanImage> vulkan_image() const;
#endif

private:
#ifdef AK_OS_MACOS
    SharedImageBuffer(Core::IOSurfaceHandle&&, NonnullRefPtr<Bitmap>);
    Core::IOSurfaceHandle m_iosurface_handle;
    NonnullRefPtr<Bitmap> m_bitmap;
#else
    explicit SharedImageBuffer(NonnullRefPtr<Bitmap>);
#    ifdef USE_VULKAN_DMABUF_IMAGES
    explicit SharedImageBuffer(NonnullRefPtr<VulkanImage>);
    SharedImageBuffer(IPC::File&&, LinuxDmaBufInfo);
    Variant<NonnullRefPtr<Bitmap>, NonnullRefPtr<VulkanImage>> m_storage;
#    else
    NonnullRefPtr<Bitmap> m_bitmap;
#    endif
#endif
};

}
