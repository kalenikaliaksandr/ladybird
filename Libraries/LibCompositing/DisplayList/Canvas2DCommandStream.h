/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Function.h>
#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/RefCounted.h>
#include <AK/Span.h>
#include <AK/Vector.h>
#include <LibCompositing/DisplayList/DisplayListResourceIds.h>
#include <LibCompositing/DisplayList/DisplayListResourceStorage.h>
#include <LibCompositing/Export.h>
#include <LibCompositing/Video/VideoFrameHold.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibIPC/Forward.h>

namespace Compositing {

// The canvas 2D commands that the canvases of a WebContent connection recorded, in order, as segments: the records one
// canvas recorded in a row, on the display list tape, and whether the canvas presented after them. Rust owns them and
// gives them their IPC codec (see Rust/src/display_list/canvas_2d.rs).
class COMPOSITING_API Canvas2DCommandSegments {
    AK_MAKE_NONCOPYABLE(Canvas2DCommandSegments);

public:
    Canvas2DCommandSegments();
    Canvas2DCommandSegments(Canvas2DCommandSegments&&);
    Canvas2DCommandSegments& operator=(Canvas2DCommandSegments&&);
    ~Canvas2DCommandSegments();

    static Canvas2DCommandSegments adopt_rust_handle(void* rust_segments);

    void append_command(CanvasId, ReadonlyBytes record);
    void record_present(CanvasId);

    bool is_empty() const;
    size_t command_count() const;

    // Moves the segments out, and leaves none here.
    Canvas2DCommandSegments take();

    // Checks the tapes of segments that came from WebContent.
    ErrorOr<void> validate() const;

    // Calls the callback with the canvas, the tape and whether it presents of each segment, in order.
    void for_each(Function<void(CanvasId, ReadonlyBytes tape, bool present)> const&) const;

    void const* rust_handle() const { return m_rust_segments; }

private:
    explicit Canvas2DCommandSegments(void* rust_segments);

    void* m_rust_segments { nullptr };
};

// An ordered stream of canvas 2D commands covering all canvases of a WebContent
// connection. Replaying segments strictly in order guarantees that a command
// referencing another canvas (DrawCanvas) observes all of that canvas's earlier
// commands without requiring an eager flush at record time.
class COMPOSITING_API Canvas2DCommandStream : public RefCounted<Canvas2DCommandStream> {
public:
    void append_command(CanvasId, ReadonlyBytes record);
    void record_present(CanvasId);

    bool is_empty() const { return m_segments.is_empty(); }
    size_t total_command_count() const { return m_segments.command_count(); }

    Canvas2DCommandSegments take_segments() { return m_segments.take(); }
    FontResourceId add_font(Gfx::Font const&);
    Vector<DisplayListFontResource> take_fonts();
    ImageFrameResourceId add_image_frame(Gfx::DecodedImageFrame const&);
    Vector<DisplayListImageFrameResource> take_image_frames();

    void retain_video_frame_hold(NonnullRefPtr<VideoFrameHold>);
    Vector<NonnullRefPtr<VideoFrameHold>> take_video_frame_holds() { return move(m_video_frame_holds); }

private:
    Canvas2DCommandSegments m_segments;
    HashMap<u64, NonnullRefPtr<Gfx::Font const>> m_fonts;
    HashMap<u64, Gfx::DecodedImageFrame> m_image_frames;
    Vector<NonnullRefPtr<VideoFrameHold>> m_video_frame_holds;
};

}

namespace IPC {

template<>
COMPOSITING_API ErrorOr<void> encode(Encoder&, Compositing::Canvas2DCommandSegments const&);
template<>
COMPOSITING_API ErrorOr<Compositing::Canvas2DCommandSegments> decode(Decoder&);

}
