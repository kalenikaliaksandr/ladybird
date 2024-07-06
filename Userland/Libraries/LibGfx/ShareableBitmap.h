/*
 * Copyright (c) 2020, Andreas Kling <kling@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/RefPtr.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/Size.h>
#include <LibIPC/Forward.h>

#ifdef USE_VULKAN
#    include <LibCore/VulkanContext.h>
#endif

namespace Gfx {

class ShareableBitmap {
public:
    ShareableBitmap() = default;

    enum Tag { ConstructWithKnownGoodBitmap };
    ShareableBitmap(NonnullRefPtr<Gfx::Bitmap>, Tag);

#ifdef USE_VULKAN
    ShareableBitmap(NonnullRefPtr<Core::VulkanImage>);
#endif

    bool is_valid() const { return m_bitmap; }

    Bitmap const* bitmap() const { return m_bitmap; }
    Bitmap* bitmap() { return m_bitmap; }

    bool is_bitmap() const { return m_type == Type::Bitmap; }
    bool is_vulkan_image() const { return m_type == Type::VulkanImage; }

private:
    friend class Bitmap;

    enum class Type {
        Bitmap,
        VulkanImage
    };
    Type m_type { Type::Bitmap };

    RefPtr<Bitmap> m_bitmap;

#ifdef USE_VULKAN
    RefPtr<Core::VulkanImage> m_vulkan_image;
#endif
};

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder&, Gfx::ShareableBitmap const&);

template<>
ErrorOr<Gfx::ShareableBitmap> decode(Decoder&);

}
