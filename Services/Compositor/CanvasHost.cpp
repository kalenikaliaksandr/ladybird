/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <Compositor/CanvasHost.h>
#include <Compositor/HostWebGLContext.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/CanvasCommandPlayer.h>
#include <LibGfx/PaintingSurface.h>
#include <LibGfx/SkiaBackendContext.h>
#include <LibWeb/Painting/Canvas2DCommandStream.h>
#include <LibWeb/Painting/CanvasSurfaceRegistry.h>

namespace Compositor {

CanvasHost::CanvasHost(RefPtr<Gfx::SkiaBackendContext> skia_backend_context, Web::Painting::CanvasSurfaceRegistry& canvas_surface_registry)
    : m_skia_backend_context(move(skia_backend_context))
    , m_canvas_surface_registry(canvas_surface_registry)
{
}

CanvasHost::~CanvasHost()
{
    for (auto canvas_id : m_contexts.keys()) {
        // Unclaim is a no-op for ids this host allocated itself; only reserved ids
        // adopted through create_*_with_id have a claim to release.
        m_canvas_surface_registry.unclaim_canvas_id(canvas_id);
        m_canvas_surface_registry.remove_canvas_surface(canvas_id);
    }
}

OwnPtr<Gfx::CanvasCommandPlayer> CanvasHost::create_2d_command_player(Gfx::IntSize size, bool alpha)
{
    if (size.is_empty() || static_cast<i64>(size.width()) * static_cast<i64>(size.height()) > Gfx::max_canvas_area)
        return nullptr;

    auto format = alpha ? Gfx::BitmapFormat::BGRA8888 : Gfx::BitmapFormat::BGRx8888;
    auto surface_resolver = [this](u64 canvas_id, bool committed_only) -> Gfx::PaintingSurface const* {
        auto id = Web::Painting::CanvasId { canvas_id };
        // A 2D source resolves to its live draw surface: the shared command
        // stream replays in recording order, so at this point the surface holds
        // exactly the commands recorded before the referencing DrawCanvas.
        // Placeholder sources ask for the committed surface instead — when the
        // producer shares this connection, the live surface may hold pixels it
        // has not committed (and whose taint the watcher has not learned).
        if (!committed_only) {
            if (auto* context = this->context(id)) {
                if (auto* canvas_context = context->get_pointer<Canvas2DContext>())
                    return canvas_context->command_player->surface().ptr();
            }
        }
        if (!may_read_registry_surface(id))
            return nullptr;
        if (committed_only)
            return m_canvas_surface_registry.committed_canvas_surface(id);
        // WebGL sources are presented separately and resolve via the registry.
        return m_canvas_surface_registry.canvas_surface(id);
    };
    auto taint_resolver = [this](u64 canvas_id) {
        return !m_canvas_surface_registry.canvas_surface_origin_clean(Web::Painting::CanvasId { canvas_id });
    };
    auto player = make<Gfx::CanvasCommandPlayer>(m_skia_backend_context, size, format, Gfx::AlphaType::Premultiplied, move(surface_resolver), move(taint_resolver));

    // https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-settings:concept-canvas-alpha
    // "Thus, the bitmap of such a context starts off as opaque black instead of transparent black"
    // AD-HOC: Skia hands out a fully transparent surface by default; only clear when alpha is disabled.
    if (!alpha)
        player->clear(Gfx::Color::Black);

    return player;
}

static void copy_surface_contents(Gfx::PaintingSurface& source, Gfx::PaintingSurface& destination)
{
    destination.copy_from_surface(source);
}

static NonnullRefPtr<Gfx::PaintingSurface> create_presented_canvas_surface(Gfx::PaintingSurface& source)
{
    auto surface = Gfx::PaintingSurface::create_with_size(
        source.size(),
        Gfx::BitmapFormat::BGRA8888,
        Gfx::AlphaType::Premultiplied,
        source.skia_backend_context());
    copy_surface_contents(source, *surface);
    return surface;
}

HostWebGLContext& CanvasHost::as_webgl(Context& context)
{
    auto* webgl_context = context.get_pointer<WebGLContext>();
    VERIFY(webgl_context);
    return **webgl_context;
}

Optional<Web::Painting::CanvasId> CanvasHost::create_2d_context(Gfx::IntSize size, bool alpha)
{
    auto canvas_id = m_canvas_surface_registry.allocate_canvas_id();
    if (!create_2d_context_with_id(canvas_id, size, alpha))
        return {};
    return canvas_id;
}

bool CanvasHost::create_2d_context_with_id(Web::Painting::CanvasId canvas_id, Gfx::IntSize size, bool alpha)
{
    auto command_player = create_2d_command_player(size, alpha);
    if (!command_player)
        return false;

    auto presented_surface = create_presented_canvas_surface(command_player->surface());
    m_canvas_surface_registry.set_canvas_surface(canvas_id, presented_surface);
    Canvas2DContext context {
        .command_player = command_player.release_nonnull(),
        .presented_surface = move(presented_surface),
    };
    m_contexts.set(canvas_id, move(context));
    return true;
}

CanvasHost::CreateWebGLContextResult CanvasHost::create_webgl_context(Web::WebGL::WebGLVersion version, Gfx::IntSize size, bool depth, bool stencil, bool antialias)
{
    return create_webgl_context_with_id(m_canvas_surface_registry.allocate_canvas_id(), version, size, depth, stencil, antialias);
}

CanvasHost::CreateWebGLContextResult CanvasHost::create_webgl_context_with_id(Web::Painting::CanvasId canvas_id, Web::WebGL::WebGLVersion version, Gfx::IntSize size, bool depth, bool stencil, bool antialias)
{
    auto context = HostWebGLContext::create(m_skia_backend_context, version, { .depth = depth, .stencil = stencil, .antialias = antialias }, size);
    if (!context)
        return {};

    auto supported_extensions = context->gl_context().get_supported_opengl_extensions();
    m_contexts.set(canvas_id, context.release_nonnull());
    return { .success = true, .canvas_id = canvas_id, .supported_extensions = move(supported_extensions) };
}

void CanvasHost::destroy_context(Web::Painting::CanvasId canvas_id)
{
    m_contexts.remove(canvas_id);
    // Unclaim is a no-op for ids this host allocated itself; only reserved ids
    // adopted through create_*_with_id have a claim to release.
    m_canvas_surface_registry.unclaim_canvas_id(canvas_id);
    m_canvas_surface_registry.remove_canvas_surface(canvas_id);
}

bool CanvasHost::has_context(Web::Painting::CanvasId canvas_id) const
{
    return m_contexts.contains(canvas_id);
}

CanvasHost::Context* CanvasHost::context(Web::Painting::CanvasId canvas_id)
{
    auto it = m_contexts.find(canvas_id);
    if (it == m_contexts.end())
        return nullptr;
    return &it->value;
}

void CanvasHost::present_canvas_2d_context(Web::Painting::CanvasId canvas_id, Canvas2DContext& context)
{
    copy_surface_contents(context.command_player->surface(), context.presented_surface);
    m_canvas_surface_registry.set_canvas_surface(canvas_id, context.presented_surface);
    // For reserved (placeholder-linked) ids the presented copy is also the
    // committed frame; a no-op for plain element canvases.
    m_canvas_surface_registry.set_committed_canvas_surface(canvas_id, context.presented_surface);
}

Vector<Web::Painting::CanvasId> CanvasHost::execute_canvas_2d_stream(Vector<Web::Painting::Canvas2DCommandStreamSegment> const& segments)
{
    Vector<Web::Painting::CanvasId> committed_canvas_ids;
    for (auto const& segment : segments) {
        // The canvas may have been destroyed while this segment was pending in
        // WebContent, so a missing context is not a protocol violation.
        auto* context = this->context(segment.canvas_id);
        auto* canvas_context = context ? context->get_pointer<Canvas2DContext>() : nullptr;
        if (!canvas_context)
            continue;

        if (!segment.commands.is_empty())
            canvas_context->command_player->play(segment.commands);

        // Present markers are only recorded deliberately, and a present with no
        // recorded commands is meaningful: the initial bitmap of a never-drawn
        // context and the cleared bitmap after a resize must reach placeholder
        // watchers too. Only presents carrying commit metadata count as commits;
        // the metadata lands on the reservation so late watchers and pixel
        // readbacks observe the committed size and taint together.
        if (segment.present) {
            present_canvas_2d_context(segment.canvas_id, *canvas_context);
            if (segment.commit_size.has_value()) {
                // The producer's flag cannot account for taint the player picked
                // up compositing committed sources; combine both.
                bool origin_clean = segment.origin_clean && !canvas_context->command_player->has_composited_tainted_source();
                m_canvas_surface_registry.record_canvas_commit(segment.canvas_id, *segment.commit_size, origin_clean);
                if (!committed_canvas_ids.contains_slow(segment.canvas_id))
                    committed_canvas_ids.append(segment.canvas_id);
            }
        }
    }
    return committed_canvas_ids;
}

void CanvasHost::execute_webgl_commands(Web::Painting::CanvasId canvas_id, ReadonlyBytes commands, Vector<Gfx::DecodedImageFrame> const& bitmaps)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    auto& webgl_context = as_webgl(*context);
    MUST(webgl_context.execute_commands(commands, bitmaps));
    if (auto surface = webgl_context.surface())
        m_canvas_surface_registry.set_canvas_surface(canvas_id, surface.release_nonnull());
}

void CanvasHost::set_webgl_shared_command_buffer(Web::Painting::CanvasId canvas_id, Web::WebGL::WebGLSharedCommandBuffer shared_command_buffer)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    as_webgl(*context).set_shared_command_buffer(move(shared_command_buffer));
}

bool CanvasHost::execute_webgl_commands_from_shared_buffer(Web::Painting::CanvasId canvas_id, u64 offset, u64 size_in_bytes, u64 flush_sequence_number, Vector<Gfx::DecodedImageFrame> const& bitmaps)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    auto& webgl_context = as_webgl(*context);

    auto commands = webgl_context.shared_command_buffer_range(offset, size_in_bytes);
    if (!commands.has_value())
        return false;

    // WebContent can rewrite shared bytes while they execute, so a malformed stream is
    // a misbehaving peer rather than a Compositor bug; report instead of crashing.
    if (webgl_context.execute_commands(*commands, bitmaps).is_error())
        return false;
    webgl_context.store_executed_flush_sequence_number(flush_sequence_number);

    if (auto surface = webgl_context.surface())
        m_canvas_surface_registry.set_canvas_surface(canvas_id, surface.release_nonnull());
    return true;
}

ErrorOr<ByteBuffer> CanvasHost::execute_webgl_sync_call(Web::Painting::CanvasId canvas_id, ByteBuffer request)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    return as_webgl(*context).execute_sync_call(request);
}

Web::WebGL::ReadPixelsResult CanvasHost::webgl_read_pixels_robust_angle(Web::Painting::CanvasId canvas_id, Web::WebGL::GLint x, Web::WebGL::GLint y, Web::WebGL::GLsizei width, Web::WebGL::GLsizei height, Web::WebGL::GLenum format, Web::WebGL::GLenum type, Web::WebGL::GLsizei buf_size, Core::AnonymousBuffer pixels)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    return as_webgl(*context).read_pixels_robust_angle(x, y, width, height, format, type, buf_size, move(pixels));
}

bool CanvasHost::webgl_read_buffer_sub_data(Web::Painting::CanvasId canvas_id, Web::WebGL::GLenum target, Web::WebGL::GLintptr offset, Web::WebGL::GLintptr size, Core::AnonymousBuffer data)
{
    auto* context = this->context(canvas_id);
    VERIFY(context);
    return as_webgl(*context).read_buffer_sub_data(target, offset, size, move(data));
}

Optional<Gfx::IntSize> CanvasHost::present_webgl_canvas(Web::Painting::CanvasId canvas_id, bool preserve_drawing_buffer)
{
    auto* context = this->context(canvas_id);
    if (!context)
        return {};

    auto surface = MUST(as_webgl(*context).prepare_for_compositing(preserve_drawing_buffer));
    auto presented_surface_size = surface->size();
    m_canvas_surface_registry.set_canvas_surface(canvas_id, move(surface));
    return presented_surface_size;
}

static Gfx::ShareableBitmap read_back_surface(Gfx::PaintingSurface& surface, Gfx::IntRect rect)
{
    auto clipped_rect = rect.intersected(surface.rect());
    if (clipped_rect.is_empty())
        return {};

    auto bitmap_or_error = Gfx::Bitmap::create_shareable(Gfx::BitmapFormat::BGRA8888, Gfx::AlphaType::Premultiplied, clipped_rect.size());
    if (bitmap_or_error.is_error())
        return {};

    auto bitmap = bitmap_or_error.release_value();
    surface.flush();
    surface.read_into_bitmap(*bitmap, clipped_rect.location());
    return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

bool CanvasHost::may_read_registry_surface(Web::Painting::CanvasId canvas_id) const
{
    // A connection may resolve its own canvases, and the committed frames of
    // placeholder ids it legitimately watches; anything else would let a
    // compromised renderer enumerate other connections' canvases through the
    // shared registry (ids are minted from one sequential counter).
    if (m_contexts.contains(canvas_id))
        return true;
    return m_cross_connection_read_authorizer && m_cross_connection_read_authorizer(canvas_id);
}

bool CanvasHost::canvas_pixels_origin_clean(Web::Painting::CanvasId canvas_id, ReadbackSurface readback_surface)
{
    if (readback_surface == ReadbackSurface::Live) {
        if (auto* context = this->context(canvas_id)) {
            if (auto* canvas_context = context->get_pointer<Canvas2DContext>())
                return !canvas_context->command_player->has_composited_tainted_source();
        }
    }
    return m_canvas_surface_registry.canvas_surface_origin_clean(canvas_id);
}

void CanvasHost::commit_presented_webgl_surface(Web::Painting::CanvasId canvas_id)
{
    auto* context = this->context(canvas_id);
    if (!context)
        return;
    auto surface = as_webgl(*context).surface();
    if (!surface)
        return;
    // Reuse the previous committed snapshot when the size still matches.
    auto const* existing = m_canvas_surface_registry.committed_canvas_surface(canvas_id);
    if (existing && existing->size() == surface->size()) {
        const_cast<Gfx::PaintingSurface&>(*existing).copy_from_surface(*surface);
        return;
    }
    auto committed_surface = create_presented_canvas_surface(*surface);
    m_canvas_surface_registry.set_committed_canvas_surface(canvas_id, move(committed_surface));
}

Gfx::ShareableBitmap CanvasHost::read_back_pixels(Web::Painting::CanvasId canvas_id, Gfx::IntRect rect, ReadbackSurface readback_surface)
{
    auto* context = this->context(canvas_id);
    if (!context || readback_surface == ReadbackSurface::CommittedOnly) {
        // Canvases owned by other connections — and placeholder readbacks, which
        // must never expose pixels the producer has not committed even when the
        // producer shares this connection — resolve through the shared registry:
        // a placeholder canvas element reading back pixels (toDataURL, drawImage
        // source, createImageBitmap) gets the last committed frame of the
        // OffscreenCanvas it was transferred from.
        if (!may_read_registry_surface(canvas_id))
            return {};
        auto const* surface = readback_surface == ReadbackSurface::CommittedOnly
            ? m_canvas_surface_registry.committed_canvas_surface(canvas_id)
            : m_canvas_surface_registry.canvas_surface(canvas_id);
        if (surface)
            return read_back_surface(const_cast<Gfx::PaintingSurface&>(*surface), rect);
        return {};
    }

    return context->visit(
        [rect](Canvas2DContext& canvas_context) {
            return read_back_surface(canvas_context.command_player->surface(), rect);
        },
        [rect](WebGLContext& webgl_context) {
            return webgl_context->read_back_drawing_buffer(rect);
        });
}

}
