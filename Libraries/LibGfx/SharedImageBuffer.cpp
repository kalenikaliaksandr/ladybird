/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/System.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/SharedImageBuffer.h>

#ifdef USE_VULKAN_IMAGES
#    include <sys/mman.h>
#endif

namespace Gfx {

#ifdef AK_OS_MACOS
static constexpr auto shared_image_buffer_format = BitmapFormat::BGRA8888;
static constexpr auto shared_image_buffer_alpha_type = AlphaType::Premultiplied;

static NonnullRefPtr<Bitmap> create_bitmap_from_iosurface(Core::IOSurfaceHandle const& iosurface_handle)
{
    auto size = IntSize(static_cast<int>(iosurface_handle.width()), static_cast<int>(iosurface_handle.height()));
    auto bitmap_handle = Core::IOSurfaceHandle::from_mach_port(iosurface_handle.create_mach_port());
    return MUST(Bitmap::create_wrapper(shared_image_buffer_format, shared_image_buffer_alpha_type, size, iosurface_handle.bytes_per_row(), iosurface_handle.data(), [handle = move(bitmap_handle)] { }));
}

SharedImageBuffer::SharedImageBuffer(Core::IOSurfaceHandle&& iosurface_handle, NonnullRefPtr<Bitmap> bitmap)
    : m_iosurface_handle(move(iosurface_handle))
    , m_bitmap(move(bitmap))
{
}
#else
static constexpr auto shared_image_buffer_format = BitmapFormat::BGRA8888;
static constexpr auto shared_image_buffer_alpha_type = AlphaType::Premultiplied;
#    ifdef USE_VULKAN_IMAGES
static constexpr auto shared_image_buffer_vulkan_format = VK_FORMAT_B8G8R8A8_UNORM;

static NonnullRefPtr<Bitmap> create_bitmap_from_linux_dmabuf(int fd, LinuxDmaBufInfo const& dmabuf_info)
{
    VERIFY(dmabuf_info.modifier == DRM_FORMAT_MOD_LINEAR);
    auto data_size = Bitmap::size_in_bytes(dmabuf_info.pitch, dmabuf_info.size.height());
    auto* data = ::mmap(nullptr, data_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    VERIFY(data != MAP_FAILED);
    return MUST(Bitmap::create_wrapper(dmabuf_info.bitmap_format, dmabuf_info.alpha_type, dmabuf_info.size, dmabuf_info.pitch, data, [data, data_size] {
        auto rc = ::munmap(data, data_size);
        VERIFY(rc == 0);
    }));
}
#    endif

SharedImageBuffer::SharedImageBuffer(NonnullRefPtr<Bitmap> bitmap)
#    ifdef USE_VULKAN_IMAGES
    : m_storage(move(bitmap))
#    else
    : m_bitmap(move(bitmap))
#    endif
{
}

#    ifdef USE_VULKAN_IMAGES
SharedImageBuffer::SharedImageBuffer(NonnullRefPtr<VulkanImage> vulkan_image)
    : m_storage(AllocatedLinuxDmaBuf {
          .image = vulkan_image,
          .info = {
              .bitmap_format = shared_image_buffer_format,
              .alpha_type = shared_image_buffer_alpha_type,
              .size = IntSize(static_cast<int>(vulkan_image->info.extent.width), static_cast<int>(vulkan_image->info.extent.height)),
              .drm_format = vk_format_to_drm_format(vulkan_image->info.format),
              .pitch = static_cast<size_t>(vulkan_image->info.row_pitch),
              .modifier = vulkan_image->info.modifier,
          },
      })
{
}

SharedImageBuffer::SharedImageBuffer(IPC::File&& dmabuf_file, LinuxDmaBufInfo dmabuf_info)
    : m_storage(ImportedLinuxDmaBuf { .file = move(dmabuf_file), .info = dmabuf_info })
{
}
#    endif
#endif

SharedImageBuffer SharedImageBuffer::allocate_for_compositing(IntSize size)
{
#ifdef AK_OS_MACOS
    auto iosurface_handle = Core::IOSurfaceHandle::create(size.width(), size.height());
    auto bitmap = create_bitmap_from_iosurface(iosurface_handle);
    return SharedImageBuffer(move(iosurface_handle), move(bitmap));
#else
    return SharedImageBuffer(MUST(Bitmap::create_shareable(shared_image_buffer_format, shared_image_buffer_alpha_type, size)));
#endif
}

#ifdef USE_VULKAN_IMAGES
SharedImageBuffer SharedImageBuffer::allocate_for_compositing_with_linux_dmabuf(VulkanContext const& context, IntSize size, ReadonlySpan<uint64_t> modifiers)
{
    auto vulkan_image = MUST(create_shared_vulkan_image(context, size.width(), size.height(), shared_image_buffer_vulkan_format, modifiers.size(), modifiers.data()));
    return SharedImageBuffer(move(vulkan_image));
}
#endif

SharedImageBuffer SharedImageBuffer::import_from_shared_image(SharedImage shared_image)
{
#ifdef AK_OS_MACOS
    auto iosurface_handle = Core::IOSurfaceHandle::from_mach_port(shared_image.m_port);
    auto bitmap = create_bitmap_from_iosurface(iosurface_handle);
    return SharedImageBuffer(move(iosurface_handle), move(bitmap));
#else
#    ifdef USE_VULKAN_IMAGES
    if (shared_image.m_data.has<LinuxDmaBufHandle>()) {
        auto& dmabuf = shared_image.m_data.get<LinuxDmaBufHandle>();
        return SharedImageBuffer(move(dmabuf.file), dmabuf.info);
    }

    auto* bitmap = shared_image.m_data.get<ShareableBitmap>().bitmap();
#    else
    auto* bitmap = shared_image.m_shareable_bitmap.bitmap();
#    endif
    VERIFY(bitmap);
    return SharedImageBuffer(NonnullRefPtr { *bitmap });
#endif
}

SharedImageBuffer::SharedImageBuffer(SharedImageBuffer&&) = default;

SharedImageBuffer& SharedImageBuffer::operator=(SharedImageBuffer&&) = default;

SharedImageBuffer::~SharedImageBuffer() = default;

SharedImage SharedImageBuffer::export_shared_image() const
{
#ifdef AK_OS_MACOS
    return SharedImage { m_iosurface_handle.create_mach_port() };
#else
#    ifdef USE_VULKAN_IMAGES
    if (!m_storage.has<NonnullRefPtr<Bitmap>>()) {
        return SharedImage { LinuxDmaBufHandle {
            .file = IPC::File::adopt_fd(duplicate_linux_dmabuf_fd()),
            .info = linux_dmabuf_info(),
        } };
    }
#    endif
    return SharedImage { ShareableBitmap { bitmap(), ShareableBitmap::ConstructWithKnownGoodBitmap } };
#endif
}

NonnullRefPtr<Bitmap> SharedImageBuffer::bitmap() const
{
#ifdef USE_VULKAN_IMAGES
    if (auto* bitmap = m_storage.get_pointer<NonnullRefPtr<Bitmap>>())
        return *bitmap;

    auto fd = duplicate_linux_dmabuf_fd();
    auto bitmap = create_bitmap_from_linux_dmabuf(fd, linux_dmabuf_info());
    (void)Core::System::close(fd);
    return bitmap;
#else
    return m_bitmap;
#endif
}

#ifdef AK_OS_MACOS
Core::IOSurfaceHandle const& SharedImageBuffer::iosurface_handle() const
{
    return m_iosurface_handle;
}
#endif

#ifdef USE_VULKAN_IMAGES
int SharedImageBuffer::duplicate_linux_dmabuf_fd() const
{
    if (auto* dmabuf = m_storage.get_pointer<AllocatedLinuxDmaBuf>()) {
        auto fd = dmabuf->image->get_dma_buf_fd();
        VERIFY(fd >= 0);
        return fd;
    }

    if (auto* dmabuf = m_storage.get_pointer<ImportedLinuxDmaBuf>()) {
        VERIFY(dmabuf->file.fd() >= 0);
        return MUST(Core::System::dup(dmabuf->file.fd()));
    }

    VERIFY_NOT_REACHED();
}

LinuxDmaBufInfo const& SharedImageBuffer::linux_dmabuf_info() const
{
    if (auto* dmabuf = m_storage.get_pointer<AllocatedLinuxDmaBuf>())
        return dmabuf->info;
    if (auto* dmabuf = m_storage.get_pointer<ImportedLinuxDmaBuf>())
        return dmabuf->info;
    VERIFY_NOT_REACHED();
}

NonnullRefPtr<VulkanImage> SharedImageBuffer::vulkan_image() const
{
    return m_storage.get<AllocatedLinuxDmaBuf>().image;
}
#endif

}
