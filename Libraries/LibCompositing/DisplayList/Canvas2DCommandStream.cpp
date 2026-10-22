/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCompositing/DisplayList/Canvas2DCommandStream.h>
#include <LibCompositing/RustFFI.h>
#include <LibCompositing/RustIPC.h>
#include <LibGfx/Font/Font.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>

namespace Compositing {

Canvas2DCommandSegments::Canvas2DCommandSegments()
    : m_rust_segments(RustFFI::canvas_2d_segments_create())
{
}

Canvas2DCommandSegments::Canvas2DCommandSegments(void* rust_segments)
    : m_rust_segments(rust_segments)
{
    VERIFY(m_rust_segments);
}

Canvas2DCommandSegments Canvas2DCommandSegments::adopt_rust_handle(void* rust_segments)
{
    return Canvas2DCommandSegments { rust_segments };
}

Canvas2DCommandSegments::Canvas2DCommandSegments(Canvas2DCommandSegments&& other)
    : m_rust_segments(exchange(other.m_rust_segments, nullptr))
{
}

Canvas2DCommandSegments& Canvas2DCommandSegments::operator=(Canvas2DCommandSegments&& other)
{
    if (this != &other) {
        if (m_rust_segments)
            RustFFI::canvas_2d_segments_destroy(m_rust_segments);
        m_rust_segments = exchange(other.m_rust_segments, nullptr);
    }
    return *this;
}

Canvas2DCommandSegments::~Canvas2DCommandSegments()
{
    if (m_rust_segments)
        RustFFI::canvas_2d_segments_destroy(m_rust_segments);
}

void Canvas2DCommandSegments::append_command(CanvasId canvas_id, ReadonlyBytes record)
{
    RustFFI::canvas_2d_segments_append_command(m_rust_segments, canvas_id.value(), record);
}

void Canvas2DCommandSegments::record_present(CanvasId canvas_id)
{
    RustFFI::canvas_2d_segments_record_present(m_rust_segments, canvas_id.value());
}

bool Canvas2DCommandSegments::is_empty() const
{
    return RustFFI::canvas_2d_segments_is_empty(m_rust_segments);
}

size_t Canvas2DCommandSegments::command_count() const
{
    return RustFFI::canvas_2d_segments_command_count(m_rust_segments);
}

Canvas2DCommandSegments Canvas2DCommandSegments::take()
{
    return Canvas2DCommandSegments { RustFFI::canvas_2d_segments_take(m_rust_segments) };
}

ErrorOr<void> Canvas2DCommandSegments::validate() const
{
    u8 const* error = nullptr;
    size_t error_size = 0;
    if (!RustFFI::canvas_2d_segments_validate(m_rust_segments, &error, &error_size))
        return Error::from_string_view({ reinterpret_cast<char const*>(error), error_size });
    return {};
}

void Canvas2DCommandSegments::for_each(Function<void(CanvasId, ReadonlyBytes tape, bool present)> const& callback) const
{
    RustFFI::canvas_2d_segments_for_each(m_rust_segments, const_cast<void*>(static_cast<void const*>(&callback)), [](void* context, u64 canvas_id, ReadonlyBytes tape, bool present) {
        (*static_cast<Function<void(CanvasId, ReadonlyBytes, bool)> const*>(context))(CanvasId { canvas_id }, tape, present);
    });
}

FontResourceId Canvas2DCommandStream::add_font(Gfx::Font const& font)
{
    m_fonts.ensure(font.id(), [&]() -> NonnullRefPtr<Gfx::Font const> { return font; });
    return { font.id() };
}

Vector<DisplayListFontResource> Canvas2DCommandStream::take_fonts()
{
    Vector<DisplayListFontResource> fonts;
    fonts.ensure_capacity(m_fonts.size());
    for (auto& entry : m_fonts)
        fonts.unchecked_append({ .id = FontResourceId { entry.key }, .font = move(entry.value) });
    m_fonts.clear();
    return fonts;
}

ImageFrameResourceId Canvas2DCommandStream::add_image_frame(Gfx::DecodedImageFrame const& frame)
{
    m_image_frames.ensure(frame.id(), [&] { return frame; });
    return { frame.id() };
}

Vector<DisplayListImageFrameResource> Canvas2DCommandStream::take_image_frames()
{
    Vector<DisplayListImageFrameResource> frames;
    frames.ensure_capacity(m_image_frames.size());
    for (auto& entry : m_image_frames)
        frames.unchecked_append({ .id = ImageFrameResourceId { entry.key }, .frame = move(entry.value) });
    m_image_frames.clear();
    return frames;
}

void Canvas2DCommandStream::retain_video_frame_hold(NonnullRefPtr<VideoFrameHold> hold)
{
    if (!m_video_frame_holds.is_empty() && m_video_frame_holds.last().ptr() == hold.ptr())
        return;
    m_video_frame_holds.append(move(hold));
}

void Canvas2DCommandStream::append_command(CanvasId canvas_id, ReadonlyBytes record)
{
    m_segments.append_command(canvas_id, record);
}

void Canvas2DCommandStream::record_present(CanvasId canvas_id)
{
    m_segments.record_present(canvas_id);
}

}

namespace IPC {

// The segments go in the wire format Rust gives them, with tapes the receiver checks (see validate()).
template<>
ErrorOr<void> encode(Encoder& encoder, Compositing::Canvas2DCommandSegments const& segments)
{
    return Compositing::encode_with_rust(encoder, [&](void* cpp_encoder, auto append) {
        return Compositing::RustFFI::canvas_2d_segments_ipc_encode(segments.rust_handle(), cpp_encoder, append);
    });
}

template<>
ErrorOr<Compositing::Canvas2DCommandSegments> decode(Decoder& decoder)
{
    auto* rust_segments = TRY(Compositing::decode_with_rust(decoder, [](u8 const* bytes, size_t length, size_t& consumed) {
        return Compositing::RustFFI::canvas_2d_segments_ipc_decode(bytes, length, &consumed);
    }));
    return Compositing::Canvas2DCommandSegments::adopt_rust_handle(rust_segments);
}

}
