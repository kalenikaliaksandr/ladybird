/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <WebContent/RemotePageCompositor.h>

#include <AK/Debug.h>
#include <AK/StdLibExtras.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibMedia/VideoFrame.h>
#include <LibWeb/Compositor/DisplayListResourceSerialization.h>
#include <LibWeb/Compositor/DisplayListSerialization.h>
#include <LibWeb/Compositor/ScrollStateSerialization.h>

namespace WebContent {

RemotePageCompositor::RemotePageCompositor(ConnectionToCompositor& connection, u64 page_id, Optional<PresentationMode> presentation_mode)
    : m_connection(connection)
    , m_page_id(page_id)
    , m_context_id(Web::Compositor::allocate_compositor_context_id())
    , m_presentation_mode(move(presentation_mode))
{
}

RemotePageCompositor::~RemotePageCompositor()
{
    if (m_context_created && m_connection->is_open())
        m_connection->destroy_context(m_context_id);
}

RemotePageCompositor::SerializedPresentationMode RemotePageCompositor::serialized_presentation_mode() const
{
    VERIFY(m_presentation_mode.has_value());
    return m_presentation_mode->visit(
        [](PresentToUI const& present_to_ui) -> SerializedPresentationMode {
            return {
                .kind = Web::Compositor::SerializedPresentationModeKind::PresentToUI,
                .presentation_id = present_to_ui.presentation_id,
                .presentation_capability = present_to_ui.presentation_capability,
                .target_context_id = {},
                .compositor_surface_id = {},
            };
        },
        [](PublishToCompositorSurface const& publish_to_compositor_surface) -> SerializedPresentationMode {
            return {
                .kind = Web::Compositor::SerializedPresentationModeKind::PublishToCompositorSurface,
                .presentation_id = {},
                .presentation_capability = {},
                .target_context_id = publish_to_compositor_surface.target_context_id,
                .compositor_surface_id = publish_to_compositor_surface.surface_id,
            };
        });
}

void RemotePageCompositor::send_create_context()
{
    VERIFY(!m_context_created);
    auto presentation_mode = serialized_presentation_mode();
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] WebContent creating remote context {} for page {} mode {}",
        m_context_id.value(), m_page_id, to_underlying(presentation_mode.kind));
    m_connection->create_context(
        m_context_id,
        m_page_id,
        presentation_mode.kind,
        presentation_mode.presentation_id,
        presentation_mode.presentation_capability,
        presentation_mode.target_context_id,
        presentation_mode.compositor_surface_id,
        m_display_list_player_type);
    m_context_created = true;
}

void RemotePageCompositor::start(Web::DisplayListPlayerType display_list_player_type)
{
    if (m_started)
        return;
    m_display_list_player_type = display_list_player_type;
    m_started = true;
    if (m_presentation_mode.has_value())
        send_create_context();
}

void RemotePageCompositor::stop_presenting_to_client()
{
    if (m_context_created && m_connection->is_open())
        m_connection->stop_presenting_to_client(m_context_id);
}

void RemotePageCompositor::set_presentation_mode(PresentationMode presentation_mode)
{
    m_presentation_mode = move(presentation_mode);
    if (!m_started || !m_connection->is_open())
        return;

    if (!m_context_created) {
        send_create_context();
        return;
    }

    auto serialized_mode = serialized_presentation_mode();
    m_connection->set_presentation_mode(
        m_context_id,
        serialized_mode.kind,
        serialized_mode.presentation_id,
        serialized_mode.presentation_capability,
        serialized_mode.target_context_id,
        serialized_mode.compositor_surface_id);
}

void RemotePageCompositor::update_display_list(NonnullRefPtr<Web::Painting::DisplayList> display_list, Web::Painting::DisplayListResourceTransaction&& resource_transaction, Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    if (!m_context_created || !m_connection->is_open())
        return;

    struct VideoFrameUpdate {
        Web::Painting::VideoFrameResourceId source_id;
        NonnullRefPtr<Media::VideoFrame> frame;
    };

    Vector<VideoFrameUpdate> video_frame_updates;
    for (auto const& video_frame_source : resource_transaction.video_frame_sources) {
        if (auto frame = video_frame_source.resource->current_frame())
            video_frame_updates.append({ video_frame_source.id, frame.release_nonnull() });
    }

    m_connection->update_display_list(
        m_context_id,
        display_list,
        resource_transaction,
        scroll_state_snapshot);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] WebContent sent remote display list for context {}",
        m_context_id.value());

    for (auto const& video_frame_update : video_frame_updates)
        send_video_frame_update(video_frame_update.source_id, video_frame_update.frame);
}

void RemotePageCompositor::update_compositor_surface(Web::Painting::CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image)
{
    if (m_context_created && m_connection->is_open())
        m_connection->update_compositor_surface(m_context_id, surface_id, move(shared_image));
}

void RemotePageCompositor::clear_compositor_surface(Web::Painting::CompositorSurfaceId surface_id)
{
    if (m_context_created && m_connection->is_open())
        m_connection->clear_compositor_surface(m_context_id, surface_id);
}

void RemotePageCompositor::send_video_frame_update(Web::Painting::VideoFrameResourceId video_frame_source_id, Media::VideoFrame const& frame)
{
    auto next_frame_sequence_id = m_video_frame_sequence_ids.get(video_frame_source_id).value_or(0) + 1;
    auto serialized_video_frame_update = Web::Compositor::serialize_video_frame_update(video_frame_source_id, next_frame_sequence_id, frame);
    if (serialized_video_frame_update.is_error()) {
        dbgln("Failed to serialize remote compositor video frame update: {}", serialized_video_frame_update.error());
        return;
    }

    m_video_frame_sequence_ids.set(video_frame_source_id, next_frame_sequence_id);
    m_connection->update_yuv_video_frame(m_context_id, serialized_video_frame_update.value());
}

void RemotePageCompositor::update_video_frame(Web::Painting::VideoFrameResourceId video_frame_source_id, Media::VideoFrame& frame)
{
    if (m_context_created && m_connection->is_open())
        send_video_frame_update(video_frame_source_id, frame);
}

void RemotePageCompositor::clear_video_frame(Web::Painting::VideoFrameResourceId video_frame_source_id)
{
    if (m_context_created && m_connection->is_open())
        m_connection->clear_video_frame(m_context_id, video_frame_source_id);
}

void RemotePageCompositor::update_scroll_state(Web::Painting::ScrollStateSnapshot&& scroll_state_snapshot)
{
    if (!m_context_created || !m_connection->is_open())
        return;

    m_connection->update_scroll_state(m_context_id, scroll_state_snapshot);
}

void RemotePageCompositor::invalidate_wheel_event_listener_state(u64 generation)
{
    if (!m_context_created || !m_connection->is_open())
        return;

    m_connection->invalidate_wheel_event_listener_state(m_context_id, generation);
}

RemotePageCompositor::AsyncScrollEnqueueResult RemotePageCompositor::async_scroll_by(
    Web::UniqueNodeID expected_document_id,
    Gfx::FloatPoint position,
    Gfx::FloatPoint delta_in_device_pixels,
    Gfx::IntRect viewport_rect,
    AsyncScrollOperationTracking operation_tracking)
{
    if (!m_context_created || !m_connection->is_open())
        return {};

    return m_connection->async_scroll_by(
        m_context_id,
        expected_document_id,
        position,
        delta_in_device_pixels,
        viewport_rect,
        operation_tracking);
}

bool RemotePageCompositor::should_defer_async_scroll_offset_adoption() const
{
    if (!m_context_created || !m_connection->is_open())
        return false;
    return m_connection->should_defer_async_scroll_offset_adoption(m_context_id);
}

bool RemotePageCompositor::should_defer_main_thread_present_for_async_scroll() const
{
    if (!m_context_created || !m_connection->is_open())
        return false;
    return m_connection->should_defer_main_thread_present_for_async_scroll(m_context_id);
}

RemotePageCompositor::PendingAsyncScrollUpdates RemotePageCompositor::take_pending_async_scroll_updates()
{
    if (!m_context_created || !m_connection->is_open())
        return {};
    return m_connection->take_pending_async_scroll_updates(m_context_id);
}

void RemotePageCompositor::viewport_size_updated(
    Gfx::IntSize viewport_size,
    bool is_top_level_traversable,
    Web::Compositor::WindowResizingInProgress window_resize_in_progress)
{
    if (m_context_created && m_connection->is_open())
        m_connection->viewport_size_updated(m_context_id, viewport_size, is_top_level_traversable, window_resize_in_progress);
}

u64 RemotePageCompositor::present_frame(Gfx::IntRect viewport_rect)
{
    if (!m_context_created || !m_connection->is_open())
        return 0;
    auto frame_id = m_connection->present_frame(m_context_id, viewport_rect);
    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] WebContent requested remote present for context {} frame {} viewport={}x{} at {},{}",
        m_context_id.value(), frame_id, viewport_rect.width(), viewport_rect.height(), viewport_rect.x(), viewport_rect.y());
    return frame_id;
}

void RemotePageCompositor::wait_for_frame(u64 frame_id)
{
    if (!m_context_created || !m_connection->is_open() || frame_id == 0)
        return;
    m_connection->wait_for_frame(m_context_id, frame_id);
}

void RemotePageCompositor::request_screenshot(NonnullRefPtr<Gfx::PaintingSurface> target_surface, Function<void()>&& callback)
{
    if (!m_context_created || !m_connection->is_open()) {
        callback();
        return;
    }

    dbgln_if(COMPOSITOR_DEBUG, "[Compositor] WebContent requested remote screenshot for context {} size={}x{}",
        m_context_id.value(), target_surface->size().width(), target_surface->size().height());
    m_connection->request_screenshot(
        m_context_id,
        target_surface->size(),
        [target_surface = move(target_surface), callback = move(callback)](Optional<Gfx::SharedImage> shared_image) mutable {
            dbgln_if(COMPOSITOR_DEBUG, "[Compositor] WebContent received remote screenshot result success={}",
                shared_image.has_value());
            if (shared_image.has_value()) {
                auto shared_image_buffer = Gfx::SharedImageBuffer::import_from_shared_image(shared_image.release_value());
                target_surface->write_from_bitmap(*shared_image_buffer.bitmap());
            }
            callback();
        });
}

}
