/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/Timer.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibWeb/HTML/TraversableNavigable.h>
#include <LibWeb/Painting/BackingStoreManager.h>
#include <WebContent/PageClient.h>

#ifdef USE_VULKAN_DMABUF_IMAGES
#    include <AK/Array.h>
#    include <libdrm/drm_fourcc.h>
#endif

namespace Web::Painting {

static void publish_backing_store_pair_if_needed(HTML::Navigable& navigable, i32 front_bitmap_id, Gfx::SharedImageBuffer const& front_buffer, i32 back_bitmap_id, Gfx::SharedImageBuffer const& back_buffer)
{
    if (!navigable.is_top_level_traversable())
        return;

    auto& page_client = navigable.top_level_traversable()->page().client();
    page_client.page_did_allocate_backing_stores(front_bitmap_id, front_buffer.export_shared_image(), back_bitmap_id, back_buffer.export_shared_image());
}

#ifdef USE_VULKAN
static NonnullRefPtr<Gfx::PaintingSurface> create_gpu_painting_surface_with_bitmap_flush(Gfx::IntSize size, Gfx::SharedImageBuffer& buffer)
{
    auto surface = Gfx::PaintingSurface::create_with_size(size, Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied);
    auto bitmap = buffer.bitmap();
    surface->on_flush = [bitmap = move(bitmap)](auto& surface) {
        surface.read_into_bitmap(*bitmap);
    };
    return surface;
}
#endif

GC_DEFINE_ALLOCATOR(BackingStoreManager);

BackingStoreManager::BackingStoreManager(HTML::Navigable& navigable)
    : m_navigable(navigable)
{
    m_backing_store_shrink_timer = Core::Timer::create_single_shot(3000, [this] {
        resize_backing_stores_if_needed(WindowResizingInProgress::No);
    });
}

void BackingStoreManager::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_navigable);
}

void BackingStoreManager::restart_resize_timer()
{
    m_backing_store_shrink_timer->restart();
}

void BackingStoreManager::reallocate_backing_stores(Gfx::IntSize size)
{
    auto skia_backend_context = Gfx::SkiaBackendContext::the();
    [[maybe_unused]] bool const is_top_level_traversable = m_navigable->is_top_level_traversable();

    RefPtr<Gfx::PaintingSurface> front_store;
    RefPtr<Gfx::PaintingSurface> back_store;

    m_front_bitmap_id = m_next_bitmap_id++;
    m_back_bitmap_id = m_next_bitmap_id++;

#ifdef AK_OS_MACOS
    auto front_buffer = Gfx::SharedImageBuffer::create(size);
    auto back_buffer = Gfx::SharedImageBuffer::create(size);
    publish_backing_store_pair_if_needed(*m_navigable, m_front_bitmap_id, front_buffer, m_back_bitmap_id, back_buffer);

    if (skia_backend_context) {
        front_store = Gfx::PaintingSurface::create_from_shared_image_buffer(front_buffer, *skia_backend_context);
        back_store = Gfx::PaintingSurface::create_from_shared_image_buffer(back_buffer, *skia_backend_context);
    } else {
        front_store = Gfx::PaintingSurface::wrap_bitmap(*front_buffer.bitmap());
        back_store = Gfx::PaintingSurface::wrap_bitmap(*back_buffer.bitmap());
    }
#else
#    ifdef USE_VULKAN_DMABUF_IMAGES
    if (is_top_level_traversable && skia_backend_context) {
        static constexpr Array<uint64_t, 1> linear_modifier = { DRM_FORMAT_MOD_LINEAR };
        auto front_buffer = Gfx::SharedImageBuffer::allocate_for_compositing_with_linux_dmabuf(skia_backend_context->vulkan_context(), size, linear_modifier.span());
        auto back_buffer = Gfx::SharedImageBuffer::allocate_for_compositing_with_linux_dmabuf(skia_backend_context->vulkan_context(), size, linear_modifier.span());
        publish_backing_store_pair_if_needed(*m_navigable, m_front_bitmap_id, front_buffer, m_back_bitmap_id, back_buffer);
        front_store = Gfx::PaintingSurface::create_from_shared_image_buffer(front_buffer, *skia_backend_context);
        back_store = Gfx::PaintingSurface::create_from_shared_image_buffer(back_buffer, *skia_backend_context);
    } else
#    endif
    {
        auto front_buffer = Gfx::SharedImageBuffer::create(size);
        auto back_buffer = Gfx::SharedImageBuffer::create(size);
        publish_backing_store_pair_if_needed(*m_navigable, m_front_bitmap_id, front_buffer, m_back_bitmap_id, back_buffer);

#    ifdef USE_VULKAN
        if (skia_backend_context) {
            front_store = create_gpu_painting_surface_with_bitmap_flush(size, front_buffer);
            back_store = create_gpu_painting_surface_with_bitmap_flush(size, back_buffer);
        } else
#    endif
        {
            front_store = Gfx::PaintingSurface::wrap_bitmap(*front_buffer.bitmap());
            back_store = Gfx::PaintingSurface::wrap_bitmap(*back_buffer.bitmap());
        }
    }
#endif

    m_allocated_size = size;

    m_navigable->rendering_thread().update_backing_stores(front_store, back_store, m_front_bitmap_id, m_back_bitmap_id);
}

void BackingStoreManager::resize_backing_stores_if_needed(WindowResizingInProgress window_resize_in_progress)
{
    if (m_navigable->is_svg_page())
        return;

    auto viewport_size = m_navigable->page().css_to_device_rect(m_navigable->viewport_rect()).size();
    if (viewport_size.is_empty())
        return;

    Web::DevicePixelSize minimum_needed_size;
    bool force_reallocate = false;
    if (window_resize_in_progress == WindowResizingInProgress::Yes && m_navigable->is_top_level_traversable()) {
        // Pad the minimum needed size so that we don't have to keep reallocating backing stores while the window is being resized.
        minimum_needed_size = { viewport_size.width() + 256, viewport_size.height() + 256 };
    } else {
        // If we're not in the middle of a resize, we can shrink the backing store size to match the viewport size.
        minimum_needed_size = viewport_size;
        force_reallocate = m_allocated_size != minimum_needed_size.to_type<int>();
    }

    if (force_reallocate || m_allocated_size.is_empty() || !m_allocated_size.contains(minimum_needed_size.to_type<int>())) {
        reallocate_backing_stores(minimum_needed_size.to_type<int>());
    }
}

}
