/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Filter.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/DisplayListResourceStorage.h>

namespace Web::Painting {

DisplayListResourceStorage::~DisplayListResourceStorage() = default;

FontResourceId DisplayListResourceStorage::add_font(Gfx::Font const& font)
{
    auto id = font.id();
    set_font(FontResourceId { id }, font);
    return { id };
}

ImageFrameResourceId DisplayListResourceStorage::add_image_frame(Gfx::DecodedImageFrame const& frame)
{
    auto id = frame.id();
    set_image_frame(ImageFrameResourceId { id }, frame);
    return { id };
}

VideoFrameResourceId DisplayListResourceStorage::add_video_frame_source(NonnullRefPtr<VideoFrameSource> source)
{
    auto id = source->id();
    set_video_frame_source(VideoFrameResourceId { id }, move(source));
    return { id };
}

DisplayListResourceId DisplayListResourceStorage::add_display_list(NonnullRefPtr<DisplayList const> display_list)
{
    auto id = display_list->id();
    set_display_list(DisplayListResourceId { id }, move(display_list));
    return { id };
}

void DisplayListResourceStorage::set_font(FontResourceId id, Gfx::Font const& font)
{
    m_fonts.set(id.value(), font);
}

void DisplayListResourceStorage::set_image_frame(ImageFrameResourceId id, Gfx::DecodedImageFrame const& frame)
{
    m_image_frames.set(id.value(), frame);
}

void DisplayListResourceStorage::set_video_frame_source(VideoFrameResourceId id, NonnullRefPtr<VideoFrameSource> source)
{
    m_video_frame_sources.set(id.value(), move(source));
}

void DisplayListResourceStorage::set_display_list(DisplayListResourceId id, NonnullRefPtr<DisplayList const> display_list)
{
    m_display_lists.set(id.value(), move(display_list));
}

static ReadonlyBytes inline_data(ReadonlyBytes payload, DisplayListDataSpan span)
{
    VERIFY(static_cast<size_t>(span.offset) + span.size <= payload.size());
    return payload.slice(span.offset, span.size);
}

static void collect_referenced_filter_resources(
    Gfx::Filter const& filter,
    DisplayListResourceStorage* storage,
    DisplayListResourceSet& referenced_resources)
{
    (void)Gfx::serialize_filter(filter, [&](Gfx::DecodedImageFrame const& frame) {
        auto resource_id = ImageFrameResourceId { frame.id() };
        if (storage && !storage->contains_image_frame(resource_id))
            storage->set_image_frame(resource_id, frame);
        referenced_resources.image_frames.set(resource_id, AK::HashSetExistingEntryBehavior::Keep);
        return resource_id.value();
    });
}

static void collect_referenced_visual_context_resources(
    AccumulatedVisualContextTree const& visual_context_tree,
    DisplayListResourceStorage* storage,
    DisplayListResourceSet& referenced_resources)
{
    for (size_t i = 1; i < visual_context_tree.node_count(); ++i) {
        auto const& node = visual_context_tree.node_at(VisualContextIndex { i });
        node.data.visit(
            [&](EffectsData const& effects) {
                if (effects.gfx_filter.has_value())
                    collect_referenced_filter_resources(*effects.gfx_filter, storage, referenced_resources);
            },
            [](auto const&) {});
    }
}

void DisplayListResourceStorage::append_referenced_resources_from(
    DisplayListResourceStorage const& source,
    ReadonlyBytes command_bytes)
{
    auto referenced_resources = source.collect_referenced_resources(command_bytes);
    for (auto id : referenced_resources.fonts)
        add_font(source.font(id));
    for (auto id : referenced_resources.image_frames)
        add_image_frame(source.image_frame(id));
    for (auto id : referenced_resources.video_frame_sources)
        add_video_frame_source(source.video_frame_source_ref(id));
    for (auto id : referenced_resources.display_lists)
        add_display_list(source.display_list(id));
}

void DisplayListResourceStorage::collect_referenced_resources(
    ReadonlyBytes command_bytes,
    DisplayListResourceSet& referenced_resources,
    DisplayListResourceStorage* storage_for_visual_context_resources) const
{
    auto add_display_list_resource = [&](DisplayListResourceId id, Optional<ReadonlyBytes> command_bytes_to_collect) {
        if (referenced_resources.display_lists.set(id, AK::HashSetExistingEntryBehavior::Keep) != HashSetResult::InsertedNewEntry)
            return;
        auto const& nested_display_list = display_list(id);
        collect_referenced_visual_context_resources(nested_display_list.visual_context_tree(), storage_for_visual_context_resources, referenced_resources);
        collect_referenced_resources(command_bytes_to_collect.value_or(nested_display_list.command_bytes()), referenced_resources, storage_for_visual_context_resources);
    };

    DisplayList::for_each_command_header(command_bytes, [&](DisplayListCommandHeader const& header, ReadonlyBytes payload) {
        visit_display_list_command(header.type, payload, [&](auto const& command) {
            if constexpr (requires { command.font_id; })
                referenced_resources.fonts.set(command.font_id, AK::HashSetExistingEntryBehavior::Keep);
            if constexpr (requires { command.frame_id; })
                referenced_resources.image_frames.set(command.frame_id, AK::HashSetExistingEntryBehavior::Keep);
            if constexpr (requires { video_frame_source(command.source_id); })
                referenced_resources.video_frame_sources.set(command.source_id, AK::HashSetExistingEntryBehavior::Keep);
            if constexpr (requires { command.paint_style; command.paint_kind; }) {
                if (command.paint_kind == decltype(command.paint_kind)::PaintStyle
                    && command.paint_style.type == DisplayListPaintStyleType::Pattern)
                    add_display_list_resource(command.paint_style.pattern_tile_display_list_id, {});
            }
            if constexpr (requires { command.backdrop_filter_data; }) {
                if (command.has_backdrop_filter) {
                    Gfx::deserialize_filter(inline_data(payload, command.backdrop_filter_data), [&](u64 image_id) {
                        referenced_resources.image_frames.set(ImageFrameResourceId { image_id }, AK::HashSetExistingEntryBehavior::Keep);
                        return image_frame(ImageFrameResourceId { image_id });
                    });
                }
            }
            if constexpr (requires { command.filter_data; }) {
                if (command.has_filter) {
                    Gfx::deserialize_filter(inline_data(payload, command.filter_data), [&](u64 image_id) {
                        referenced_resources.image_frames.set(ImageFrameResourceId { image_id }, AK::HashSetExistingEntryBehavior::Keep);
                        return image_frame(ImageFrameResourceId { image_id });
                    });
                }
            }
            if constexpr (requires { command.display_list_id; }) {
                if constexpr (requires { command.command_bytes; })
                    add_display_list_resource(command.display_list_id, inline_data(payload, command.command_bytes));
                else
                    add_display_list_resource(command.display_list_id, {});
            }
        });
    });
}

DisplayListResourceSet DisplayListResourceStorage::collect_referenced_resources(ReadonlyBytes command_bytes) const
{
    DisplayListResourceSet referenced_resources;
    collect_referenced_resources(command_bytes, referenced_resources, nullptr);
    return referenced_resources;
}

DisplayListResourceSet DisplayListResourceStorage::collect_referenced_resources(DisplayList const& display_list)
{
    DisplayListResourceSet referenced_resources;
    collect_referenced_visual_context_resources(display_list.visual_context_tree(), this, referenced_resources);
    collect_referenced_resources(display_list.command_bytes(), referenced_resources, this);
    return referenced_resources;
}

DisplayListResourceTransaction DisplayListResourceStorage::create_transaction(
    DisplayListResourceSet const& previous,
    DisplayListResourceSet const& current) const
{
    DisplayListResourceTransaction transaction;

    for (auto id : current.fonts) {
        if (!previous.fonts.contains(id))
            transaction.fonts.append({ id, font(id) });
    }
    for (auto id : current.image_frames) {
        if (!previous.image_frames.contains(id))
            transaction.image_frames.append({ id, image_frame(id) });
    }
    HashTable<ImageFrameResourceId> image_frames_in_transaction;
    for (auto const& image_frame : transaction.image_frames)
        image_frames_in_transaction.set(image_frame.id);

    for (auto id : current.video_frame_sources) {
        if (!previous.video_frame_sources.contains(id))
            transaction.video_frame_sources.append({ id, video_frame_source_ref(id) });
    }
    for (auto id : current.display_lists) {
        if (!previous.display_lists.contains(id)) {
            transaction.display_lists.append({ id, display_list(id) });

            DisplayListResourceSet display_list_resources;
            auto const& display_list_resource = display_list(id);
            collect_referenced_visual_context_resources(display_list_resource.visual_context_tree(), nullptr, display_list_resources);
            collect_referenced_resources(display_list_resource.command_bytes(), display_list_resources, nullptr);
            for (auto image_frame_id : display_list_resources.image_frames) {
                if (!image_frames_in_transaction.contains(image_frame_id)) {
                    transaction.image_frames.append({ image_frame_id, image_frame(image_frame_id) });
                    image_frames_in_transaction.set(image_frame_id);
                }
            }
        }
    }

    for (auto id : previous.fonts) {
        if (!current.fonts.contains(id))
            transaction.font_ids_to_remove.append(id);
    }
    for (auto id : previous.image_frames) {
        if (!current.image_frames.contains(id))
            transaction.image_frame_ids_to_remove.append(id);
    }
    for (auto id : previous.video_frame_sources) {
        if (!current.video_frame_sources.contains(id))
            transaction.video_frame_source_ids_to_remove.append(id);
    }
    for (auto id : previous.display_lists) {
        if (!current.display_lists.contains(id))
            transaction.display_list_ids_to_remove.append(id);
    }
    return transaction;
}

void DisplayListResourceStorage::apply_transaction(DisplayListResourceTransaction&& transaction)
{
    for (auto const& font : transaction.fonts)
        set_font(font.id, *font.resource);
    for (auto const& frame : transaction.image_frames)
        set_image_frame(frame.id, frame.resource);
    for (auto& source : transaction.video_frame_sources)
        set_video_frame_source(source.id, move(source.resource));
    for (auto& display_list : transaction.display_lists)
        set_display_list(display_list.id, move(display_list.resource));

    for (auto id : transaction.font_ids_to_remove)
        m_fonts.remove(id.value());
    for (auto id : transaction.image_frame_ids_to_remove)
        m_image_frames.remove(id.value());
    for (auto id : transaction.video_frame_source_ids_to_remove)
        m_video_frame_sources.remove(id.value());
    for (auto id : transaction.display_list_ids_to_remove)
        m_display_lists.remove(id.value());
}

void DisplayListResourceStorage::retain_only(DisplayListResourceSet const& resource_set)
{
    m_fonts.remove_all_matching([&](auto id, auto const&) { return !resource_set.fonts.contains(FontResourceId { id }); });
    m_image_frames.remove_all_matching([&](auto id, auto const&) { return !resource_set.image_frames.contains(ImageFrameResourceId { id }); });
    m_video_frame_sources.remove_all_matching([&](auto id, auto const&) { return !resource_set.video_frame_sources.contains(VideoFrameResourceId { id }); });
    m_display_lists.remove_all_matching([&](auto id, auto const&) { return !resource_set.display_lists.contains(DisplayListResourceId { id }); });
}

void DisplayListResourceStorage::update_compositor_surface(CompositorSurfaceId surface_id, Gfx::SharedImage&& shared_image)
{
    auto shared_image_buffer = Gfx::SharedImageBuffer::import_from_shared_image(move(shared_image));
    m_compositor_surfaces.set(surface_id.value(), Gfx::DecodedImageFrame { *shared_image_buffer.bitmap() });
}

void DisplayListResourceStorage::clear_compositor_surface(CompositorSurfaceId surface_id)
{
    m_compositor_surfaces.remove(surface_id.value());
}

}
