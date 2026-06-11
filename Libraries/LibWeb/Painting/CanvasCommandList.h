/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibGfx/AffineTransform.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/Color.h>
#include <LibGfx/CompositingAndBlendingOperator.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Filter.h>
#include <LibGfx/Gradients.h>
#include <LibGfx/PaintStyle.h>
#include <LibGfx/Path.h>
#include <LibGfx/Point.h>
#include <LibGfx/Rect.h>
#include <LibGfx/ScalingMode.h>
#include <LibGfx/Size.h>
#include <LibGfx/WindingRule.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Web::Painting {

// 2D canvas draw calls are recorded into a CanvasCommandList in WebContent and replayed
// onto a persistent surface in the Compositor. Unlike the page DisplayList, which is a
// replayable scene description, a canvas command list is an apply-exactly-once mutation
// log: geometry stays in canvas-local float coordinates, and transform/clip/save state
// are ops in the stream that persist on the playback side across deltas.

struct CanvasLinearGradient {
    Gfx::FloatPoint start_point;
    Gfx::FloatPoint end_point;
    Vector<Gfx::ColorStop> color_stops;
    Optional<float> repeat_length;
};

struct CanvasRadialGradient {
    Gfx::FloatPoint start_center;
    float start_radius { 0 };
    Gfx::FloatPoint end_center;
    float end_radius { 0 };
    Vector<Gfx::ColorStop> color_stops;
    Optional<float> repeat_length;
};

struct CanvasConicGradient {
    Gfx::FloatPoint center;
    float start_angle { 0 };
    Vector<Gfx::ColorStop> color_stops;
    Optional<float> repeat_length;
};

struct CanvasPatternStyle {
    Optional<Gfx::DecodedImageFrame> image;
    Gfx::CanvasPatternPaintStyle::Repetition repetition { Gfx::CanvasPatternPaintStyle::Repetition::Repeat };
    Optional<Gfx::AffineTransform> transform;
};

using CanvasPaintStyle = Variant<Gfx::Color, CanvasLinearGradient, CanvasRadialGradient, CanvasConicGradient, CanvasPatternStyle>;

// Maximum area (in pixels) of a canvas surface, enforced both when the canvas
// element allocates its backing store and when the player validates Initialize
// commands that arrive over IPC.
inline constexpr i64 max_canvas_area = 16384 * 16384;

namespace CanvasCommands {

// First op after canvas context (re)allocation: (re)creates the surface and resets
// player state.
struct Initialize {
    Gfx::IntSize size;
    Gfx::BitmapFormat format { Gfx::BitmapFormat::BGRA8888 };
    Gfx::AlphaType alpha_type { Gfx::AlphaType::Premultiplied };
};

struct ClearRect {
    Gfx::FloatRect rect;
    Gfx::Color color;
};

struct FillRect {
    Gfx::FloatRect rect;
    Gfx::Color color;
};

struct DrawBitmap {
    Gfx::DecodedImageFrame frame;
    Gfx::FloatRect dst_rect;
    Gfx::IntRect src_rect;
    Gfx::ScalingMode scaling_mode { Gfx::ScalingMode::NearestNeighbor };
    Optional<Gfx::Filter> filter;
    float global_alpha { 1 };
    Gfx::CompositingAndBlendingOperator compositing_and_blending_operator { Gfx::CompositingAndBlendingOperator::SourceOver };
};

struct FillPath {
    Gfx::Path path;
    CanvasPaintStyle style;
    Gfx::WindingRule winding_rule { Gfx::WindingRule::Nonzero };
    float blur_radius { 0 };
    Optional<Gfx::Filter> filter;
    float global_alpha { 1 };
    Gfx::CompositingAndBlendingOperator compositing_and_blending_operator { Gfx::CompositingAndBlendingOperator::SourceOver };
};

struct StrokePath {
    Gfx::Path path;
    CanvasPaintStyle style;
    float thickness { 1 };
    Gfx::Path::CapStyle cap_style { Gfx::Path::CapStyle::Butt };
    Gfx::Path::JoinStyle join_style { Gfx::Path::JoinStyle::Miter };
    float miter_limit { 10 };
    Vector<float> dash_array;
    float dash_offset { 0 };
    float blur_radius { 0 };
    Optional<Gfx::Filter> filter;
    float global_alpha { 1 };
    Gfx::CompositingAndBlendingOperator compositing_and_blending_operator { Gfx::CompositingAndBlendingOperator::SourceOver };
};

struct SetTransform {
    Gfx::AffineTransform transform;
};

struct Save { };

struct Restore { };

struct ClipPath {
    Gfx::Path path;
    Gfx::WindingRule winding_rule { Gfx::WindingRule::Nonzero };
};

struct Reset { };

// Samples the live surface of another canvas context (canvas-to-canvas drawImage).
struct DrawCanvasContext {
    CanvasContextId source_context_id;
    Gfx::FloatRect dst_rect;
    Gfx::IntRect src_rect;
    Gfx::ScalingMode scaling_mode { Gfx::ScalingMode::NearestNeighbor };
    Optional<Gfx::Filter> filter;
    float global_alpha { 1 };
    Gfx::CompositingAndBlendingOperator compositing_and_blending_operator { Gfx::CompositingAndBlendingOperator::SourceOver };
};

}

using CanvasCommand = Variant<
    CanvasCommands::Initialize,
    CanvasCommands::ClearRect,
    CanvasCommands::FillRect,
    CanvasCommands::DrawBitmap,
    CanvasCommands::FillPath,
    CanvasCommands::StrokePath,
    CanvasCommands::SetTransform,
    CanvasCommands::Save,
    CanvasCommands::Restore,
    CanvasCommands::ClipPath,
    CanvasCommands::Reset,
    CanvasCommands::DrawCanvasContext>;

class CanvasCommandList {
public:
    CanvasCommandList() = default;
    explicit CanvasCommandList(Vector<CanvasCommand> commands)
        : m_commands(move(commands))
    {
    }

    void append(CanvasCommand&& command) { m_commands.append(move(command)); }

    bool is_empty() const { return m_commands.is_empty(); }
    size_t size() const { return m_commands.size(); }

    Vector<CanvasCommand> const& commands() const { return m_commands; }

private:
    Vector<CanvasCommand> m_commands;
};

WEB_API CanvasPaintStyle to_canvas_paint_style(Gfx::PaintStyle const&);

}

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasLinearGradient const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasLinearGradient> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasRadialGradient const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasRadialGradient> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasConicGradient const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasConicGradient> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasPatternStyle const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasPatternStyle> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Initialize const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::Initialize> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::ClearRect const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::ClearRect> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::FillRect const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::FillRect> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::DrawBitmap const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::DrawBitmap> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::FillPath const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::FillPath> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::StrokePath const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::StrokePath> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::SetTransform const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::SetTransform> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Save const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::Save> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Restore const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::Restore> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::ClipPath const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::ClipPath> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::Reset const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::Reset> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommands::DrawCanvasContext const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommands::DrawCanvasContext> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::CanvasCommandList const&);
template<>
WEB_API ErrorOr<Web::Painting::CanvasCommandList> decode(Decoder&);

}
