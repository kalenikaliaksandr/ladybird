/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Painting/DisplayListSerializer.h>
#include <LibWeb/Painting/GPURasterizer.h>

namespace Web::Painting {

GPURasterizer::GPURasterizer(PresentationCallback callback, NonnullRefPtr<GPUProcessClient::Client> gpu_client)
    : m_presentation_callback(move(callback))
    , m_gpu_client(move(gpu_client))
{
}

GPURasterizer::~GPURasterizer() = default;

void GPURasterizer::update_display_list(NonnullRefPtr<DisplayList> display_list, ScrollStateSnapshotByDisplayList&& scroll_states)
{
    // Send any newly registered resources to the GPU process
    for (auto& pending : m_resource_registry.take_pending_typefaces()) {
        auto font_data_or_error = Core::AnonymousBuffer::create_with_size(pending.font_data.size());
        if (font_data_or_error.is_error())
            continue;
        auto font_data = font_data_or_error.release_value();
        memcpy(font_data.data<void>(), pending.font_data.data(), pending.font_data.size());
        m_gpu_client->async_register_typeface(pending.typeface_id, move(font_data), pending.ttc_index);
    }

    for (auto& pending : m_resource_registry.take_pending_fonts()) {
        m_gpu_client->async_register_font(pending.font_id, pending.typeface_id, pending.point_size);
    }

    for (auto& pending : m_resource_registry.take_pending_images()) {
        auto shareable = pending.bitmap->to_shareable_bitmap();
        m_gpu_client->async_register_image(pending.image_id, move(shareable));
    }

    // Serialize the display list into shared memory
    auto buffer_or_error = DisplayListSerializer::serialize(*display_list, scroll_states, m_resource_registry);
    if (buffer_or_error.is_error()) {
        dbgln("GPURasterizer: Failed to serialize display list: {}", buffer_or_error.error());
        return;
    }

    m_gpu_client->async_submit_display_list(m_page_id, buffer_or_error.release_value());
}

void GPURasterizer::update_backing_stores(i32 front_id, Gfx::ShareableBitmap front_bitmap, i32 back_id, Gfx::ShareableBitmap back_bitmap)
{
    m_gpu_client->async_update_backing_stores(m_page_id, front_id, move(front_bitmap), back_id, move(back_bitmap));
}

void GPURasterizer::present_frame(Gfx::IntRect viewport_rect)
{
    m_gpu_client->async_present_frame(m_page_id, viewport_rect);
}

void GPURasterizer::ready_to_paint()
{
    // FIXME: Signal backpressure release
}

}
