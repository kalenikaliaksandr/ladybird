/*
 * Copyright (c) 2024-2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Bitmap.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibGfx/SkiaUtils.h>

#include <core/SkColorSpace.h>
#include <core/SkImage.h>
#include <core/SkSurface.h>

#pragma push_macro("TODO")
#undef TODO
#include <gpu/graphite/Context.h>
#include <gpu/graphite/Surface.h>
#pragma pop_macro("TODO")

#include <cstring>

#ifdef AK_OS_MACOS
#    pragma push_macro("TODO")
#    undef TODO
#    include <gpu/graphite/mtl/MtlGraphiteTypes_cpp.h>
#    pragma pop_macro("TODO")
#elif defined(USE_VULKAN_DMABUF_IMAGES)
#    include <LibGfx/VulkanImage.h>
#    pragma push_macro("TODO")
#    undef TODO
#    include <gpu/graphite/vk/VulkanGraphiteTypes.h>
#    pragma pop_macro("TODO")
#endif

namespace Gfx {

struct PaintingSurface::Impl {
    RefPtr<SkiaBackendContext> context;
    IntSize size;
    PaintingSurface::Origin origin { PaintingSurface::Origin::TopLeft };
    sk_sp<SkSurface> surface;
    RefPtr<Bitmap> bitmap;
};

#ifdef USE_VULKAN_DMABUF_IMAGES
static SkColorType vk_format_to_sk_color_type(VkFormat format)
{
    switch (format) {
    case VK_FORMAT_B8G8R8A8_UNORM:
        return kBGRA_8888_SkColorType;
    // add more as needed
    default:
        VERIFY_NOT_REACHED();
        return kUnknown_SkColorType;
    }
}

static void release_vulkan_image(void* context)
{
    VulkanImage* image = static_cast<VulkanImage*>(context);
    image->unref();
}

NonnullRefPtr<PaintingSurface> PaintingSurface::create_from_vkimage(NonnullRefPtr<SkiaBackendContext> context, NonnullRefPtr<VulkanImage> vulkan_image, Origin origin)
{
    IntSize size(vulkan_image->info.extent.width, vulkan_image->info.extent.height);
    skgpu::graphite::VulkanTextureInfo texture_info;
    texture_info.fFormat = vulkan_image->info.format;
    // Graphite's Vulkan caps validate only optimal/linear tiling. These images are selected from
    // renderable DRM modifiers, so describe them as optimal for Graphite's feature checks.
    texture_info.fImageTiling = VK_IMAGE_TILING_OPTIMAL;
    texture_info.fImageUsageFlags = vulkan_image->info.usage;
    texture_info.fSharingMode = vulkan_image->info.sharing_mode;

    skgpu::VulkanAlloc alloc;
    alloc.fMemory = vulkan_image->memory;
    auto backend_texture = skgpu::graphite::BackendTextures::MakeVulkan(
        SkISize::Make(size.width(), size.height()),
        texture_info,
        vulkan_image->info.layout,
        VK_QUEUE_FAMILY_IGNORED,
        vulkan_image->image,
        alloc);

    // Note, we're implicitly giving Skia a reference to vulkan_image. It will eventually be released by the callback function.
    vulkan_image->ref();
    sk_sp<SkSurface> surface = SkSurfaces::WrapBackendTexture(context->recorder(), backend_texture, vk_format_to_sk_color_type(vulkan_image->info.format),
        SkColorSpace::MakeSRGB(), nullptr, release_vulkan_image, vulkan_image.ptr());
    VERIFY(surface);
    return adopt_ref(*new PaintingSurface(make<Impl>(context, size, origin, surface, nullptr)));
}
#endif

NonnullRefPtr<PaintingSurface> PaintingSurface::create_with_size(IntSize size, BitmapFormat color_type, AlphaType alpha_type, RefPtr<SkiaBackendContext> context)
{
    auto sk_color_type = to_skia_color_type(color_type);
    auto sk_alpha_type = to_skia_alpha_type(color_type, alpha_type);
    auto image_info = SkImageInfo::Make(size.width(), size.height(), sk_color_type, sk_alpha_type, SkColorSpace::MakeSRGB());

    if (context) {
        auto surface = SkSurfaces::RenderTarget(context->recorder(), image_info);
        if (surface)
            return adopt_ref(*new PaintingSurface(make<Impl>(context, size, Origin::TopLeft, surface, nullptr)));
        dbgln("Unable to create GPU surface for size {}x{}, falling back to CPU", size.width(), size.height());
        context = nullptr;
    }

    auto bitmap = Bitmap::create(color_type, alpha_type, size).value();
    auto surface = SkSurfaces::WrapPixels(image_info, bitmap->begin(), bitmap->pitch());
    VERIFY(surface);
    return adopt_ref(*new PaintingSurface(make<Impl>(context, size, Origin::TopLeft, surface, bitmap)));
}

NonnullRefPtr<PaintingSurface> PaintingSurface::wrap_bitmap(Bitmap& bitmap)
{
    auto color_type = to_skia_color_type(bitmap.format());
    auto alpha_type = to_skia_alpha_type(bitmap.format(), bitmap.alpha_type());
    auto size = bitmap.size();
    auto image_info = SkImageInfo::Make(bitmap.width(), bitmap.height(), color_type, alpha_type, SkColorSpace::MakeSRGB());
    auto surface = SkSurfaces::WrapPixels(image_info, bitmap.begin(), bitmap.pitch());
    return adopt_ref(*new PaintingSurface(make<Impl>(RefPtr<SkiaBackendContext> {}, size, Origin::TopLeft, surface, bitmap)));
}

#ifdef AK_OS_MACOS
NonnullRefPtr<PaintingSurface> PaintingSurface::create_from_shared_image_buffer(SharedImageBuffer& shared_image_buffer, NonnullRefPtr<SkiaBackendContext> context, Origin origin)
{
    auto const& iosurface_handle = shared_image_buffer.iosurface_handle();
    auto metal_texture = context->metal_context().create_texture_from_iosurface(iosurface_handle);
    IntSize const size { metal_texture->width(), metal_texture->height() };
    auto backend_texture = skgpu::graphite::BackendTextures::MakeMetal(SkISize::Make(size.width(), size.height()), metal_texture->texture());
    auto surface = SkSurfaces::WrapBackendTexture(context->recorder(), backend_texture, kBGRA_8888_SkColorType, SkColorSpace::MakeSRGB(), nullptr);
    return adopt_ref(*new PaintingSurface(make<Impl>(context, size, origin, surface, nullptr)));
}
#endif

PaintingSurface::PaintingSurface(NonnullOwnPtr<Impl>&& impl)
    : m_impl(move(impl))
{
}

PaintingSurface::~PaintingSurface()
{
    m_impl->surface = nullptr;
}

NonnullRefPtr<Bitmap> PaintingSurface::snapshot_bitmap() const
{
    auto bitmap = MUST(Bitmap::create(BitmapFormat::BGRA8888, AlphaType::Premultiplied, size()));
    read_into_bitmap(*bitmap);
    return bitmap;
}

SharedImage PaintingSurface::snapshot_into_shared_image() const
{
    auto shared_image_buffer = SharedImageBuffer::create(size());
    read_into_bitmap(*shared_image_buffer.bitmap());
    return shared_image_buffer.export_shared_image();
}

void PaintingSurface::read_into_bitmap(Bitmap& bitmap) const
{
    auto color_type = to_skia_color_type(bitmap.format());
    auto alpha_type = to_skia_alpha_type(bitmap.format(), bitmap.alpha_type());
    auto image_info = SkImageInfo::Make(bitmap.width(), bitmap.height(), color_type, alpha_type, SkColorSpace::MakeSRGB());
    SkPixmap const pixmap(image_info, bitmap.begin(), bitmap.pitch());

    if (!m_impl->context) {
        m_impl->surface->readPixels(pixmap, 0, 0);
        return;
    }

    m_impl->context->flush_and_submit(m_impl->surface.get());

    struct ReadContext {
        SkPixmap const& pixmap;
        PaintingSurface::Origin origin;
        bool was_called { false };
        bool succeeded { false };
    } read_context { pixmap, m_impl->origin };

    auto callback = [](void* raw_context, std::unique_ptr<SkImage::AsyncReadResult const> result) {
        auto& context = *static_cast<ReadContext*>(raw_context);
        context.was_called = true;
        if (!result || result->count() != 1)
            return;

        auto row_bytes_to_copy = context.pixmap.info().minRowBytes();
        for (int row = 0; row < context.pixmap.height(); ++row) {
            auto source_row = context.origin == PaintingSurface::Origin::BottomLeft
                ? context.pixmap.height() - row - 1
                : row;
            auto* destination = context.pixmap.writable_addr(0, row);
            auto const* source = static_cast<u8 const*>(result->data(0)) + (static_cast<size_t>(source_row) * result->rowBytes(0));
            memcpy(destination, source, row_bytes_to_copy);
        }
        context.succeeded = true;
    };

    auto src_rect = SkIRect::MakeWH(size().width(), size().height());
    m_impl->context->graphite_context()->asyncRescaleAndReadPixels(
        &*m_impl->surface,
        image_info,
        src_rect,
        SkImage::RescaleGamma::kSrc,
        SkImage::RescaleMode::kNearest,
        callback,
        &read_context);
    m_impl->context->graphite_context()->submit(skgpu::graphite::SyncToCpu::kYes);
    m_impl->context->graphite_context()->checkAsyncWorkCompletion();
    VERIFY(read_context.was_called);
    VERIFY(read_context.succeeded);
}

void PaintingSurface::write_from_bitmap(Bitmap const& bitmap)
{
    auto color_type = to_skia_color_type(bitmap.format());
    auto alpha_type = to_skia_alpha_type(bitmap.format(), bitmap.alpha_type());
    auto image_info = SkImageInfo::Make(bitmap.width(), bitmap.height(), color_type, alpha_type, SkColorSpace::MakeSRGB());
    SkPixmap const pixmap(image_info, bitmap.begin(), bitmap.pitch());
    m_impl->surface->writePixels(pixmap, 0, 0);
}

IntSize PaintingSurface::size() const
{
    return m_impl->size;
}

IntRect PaintingSurface::rect() const
{
    return { {}, m_impl->size };
}

SkCanvas& PaintingSurface::canvas() const
{
    return *m_impl->surface->getCanvas();
}

SkSurface& PaintingSurface::sk_surface() const
{
    return *m_impl->surface;
}

void PaintingSurface::notify_content_will_change()
{
    m_impl->surface->notifyContentWillChange(SkSurface::kDiscard_ContentChangeMode);
}

template<>
sk_sp<SkImage> PaintingSurface::sk_image_snapshot() const
{
    return m_impl->surface->makeImageSnapshot();
}

RefPtr<SkiaBackendContext> PaintingSurface::skia_backend_context() const
{
    return m_impl->context;
}

void PaintingSurface::flush()
{
    if (on_flush)
        on_flush(*this);
}

}
