/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Filter.h>
#include <LibGfx/Font/Font.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Painting {

DisplayListResourceStorage::~DisplayListResourceStorage() = default;

FontResourceId DisplayListResourceStorage::add_font(Gfx::Font const& font)
{
    auto id = font.id();
    m_fonts.ensure(id, [&]() -> NonnullRefPtr<Gfx::Font const> {
        return font;
    });
    return { id };
}

ImageFrameResourceId DisplayListResourceStorage::add_image_frame(Gfx::DecodedImageFrame const& frame)
{
    auto id = frame.id();
    m_image_frames.ensure(id, [&] {
        return frame;
    });
    return { id };
}

ExternalContentResourceId DisplayListResourceStorage::add_external_content_source(
    NonnullRefPtr<ExternalContentSource const> source)
{
    auto id = source->id();
    m_external_content_sources.ensure(id, [&] {
        return move(source);
    });
    return { id };
}

VideoFrameResourceId DisplayListResourceStorage::add_video_frame_source(NonnullRefPtr<VideoFrameSource const> source)
{
    auto id = source->id();
    m_video_frame_sources.ensure(id, [&] {
        return move(source);
    });
    return { id };
}

DisplayListResourceId DisplayListResourceStorage::add_display_list(NonnullRefPtr<DisplayList const> display_list)
{
    auto id = display_list->id();
    m_display_lists.ensure(id, [&] {
        return move(display_list);
    });
    return { id };
}

static ReadonlyBytes display_list_inline_data(ReadonlyBytes payload, DisplayListDataSpan span)
{
    VERIFY(static_cast<size_t>(span.offset) + span.size <= payload.size());
    return payload.slice(span.offset, span.size);
}

static void append_filter_referenced_resources(
    DisplayListResourceStorage& target,
    DisplayListResourceStorage const& source,
    ReadonlyBytes payload,
    DisplayListFilter filter)
{
    if (filter.is_empty())
        return;
    MUST(Gfx::Filter::for_each_serialized_image_id(display_list_inline_data(payload, filter.data), [&](u64 image_frame_id) -> ErrorOr<void> {
        target.add_image_frame(source.image_frame({ image_frame_id }));
        return {};
    }));
}

void DisplayListResourceStorage::append_referenced_resources_from(
    DisplayListResourceStorage const& source,
    ReadonlyBytes command_bytes)
{
    DisplayList::for_each_command_header(command_bytes, [&](DisplayListCommandHeader const& header, ReadonlyBytes payload) {
        visit_display_list_command(header.type, payload, [&](auto const& command) {
            if constexpr (requires { command.font_id; })
                add_font(source.font(command.font_id));
            if constexpr (requires { command.frame_id; })
                add_image_frame(source.image_frame(command.frame_id));
            if constexpr (requires { source.external_content_source(command.source_id); })
                add_external_content_source(source.external_content_source(command.source_id));
            if constexpr (requires { source.video_frame_source(command.source_id); })
                add_video_frame_source(source.video_frame_source(command.source_id));
            if constexpr (requires { command.paint_style; command.paint_kind; }) {
                if (command.paint_kind == decltype(command.paint_kind)::PaintStyle
                    && command.paint_style.type == DisplayListPaintStyleType::Pattern)
                    add_display_list(source.display_list(command.paint_style.pattern_tile_display_list_id));
            }
            if constexpr (requires { command.backdrop_filter; })
                append_filter_referenced_resources(*this, source, payload, command.backdrop_filter);
            if constexpr (requires { command.filter; })
                append_filter_referenced_resources(*this, source, payload, command.filter);
            if constexpr (requires { command.display_list_id; })
                add_display_list(source.display_list(command.display_list_id));
        });
    });
}

}
