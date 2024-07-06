/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Forward.h>
#include <AK/Noncopyable.h>
#include <LibGfx/Size.h>

#ifdef AK_OS_MACOS
#    include <LibCore/IOSurface.h>
#endif

#ifdef USE_VULKAN
#    include <LibCore/VulkanContext.h>
#endif

namespace Web::Painting {

class BackingStore {
    AK_MAKE_NONCOPYABLE(BackingStore);

public:
    virtual Gfx::IntSize size() const = 0;
    virtual Gfx::Bitmap& bitmap() const = 0;

    BackingStore() {};
    virtual ~BackingStore() {};
};

class BitmapBackingStore final : public BackingStore {
public:
    BitmapBackingStore(RefPtr<Gfx::Bitmap>);

    Gfx::IntSize size() const override { return m_bitmap->size(); }
    Gfx::Bitmap& bitmap() const override { return *m_bitmap; }

private:
    RefPtr<Gfx::Bitmap> m_bitmap;
};

#ifdef USE_VULKAN
class VulkanBackingStore final : public BackingStore {
public:
    VulkanBackingStore(Core::VulkanImage&& vulkan_image)
        : m_vulkan_image(move(vulkan_image))
    {
    }

    Gfx::IntSize size() const override
    {
        return { m_vulkan_image.width(), m_vulkan_image.height() };
    }
    Gfx::Bitmap& bitmap() const override { VERIFY_NOT_REACHED(); }

    Core::VulkanImage& vulkan_image() { return m_vulkan_image; }

    virtual ~VulkanBackingStore() {};

private:
    Core::VulkanImage m_vulkan_image;
};
#endif

#ifdef AK_OS_MACOS
class IOSurfaceBackingStore final : public BackingStore {
public:
    IOSurfaceBackingStore(Core::IOSurfaceHandle&&);

    Gfx::IntSize size() const override;

    Core::IOSurfaceHandle& iosurface_handle() { return m_iosurface_handle; }
    Gfx::Bitmap& bitmap() const override { return *m_bitmap_wrapper; }

private:
    Core::IOSurfaceHandle m_iosurface_handle;
    RefPtr<Gfx::Bitmap> m_bitmap_wrapper;
};
#endif

}
