/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Forward.h>
#include <AK/HashMap.h>
#include <AK/HashTable.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/Span.h>
#include <AK/Vector.h>
#include <LibGfx/DecodedImageFrame.h>
#include <LibGfx/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>
#include <LibWeb/Painting/VideoFrameSource.h>

namespace Web::Painting {

struct DisplayListResourceSet {
    HashTable<FontResourceId> fonts;
    HashTable<ImageFrameResourceId> image_frames;
    HashTable<VideoFrameResourceId> video_frame_sources;
    HashTable<DisplayListResourceId> display_lists;
};

template<typename ResourceID, typename Resource>
struct DisplayListResourceAddition {
    ResourceID id;
    Resource resource;
};

using FontResourceAddition = DisplayListResourceAddition<FontResourceId, NonnullRefPtr<Gfx::Font const>>;
using ImageFrameResourceAddition = DisplayListResourceAddition<ImageFrameResourceId, Gfx::DecodedImageFrame>;
using VideoFrameSourceResourceAddition = DisplayListResourceAddition<VideoFrameResourceId, NonnullRefPtr<VideoFrameSource>>;
using DisplayListResourceAdditionRecord = DisplayListResourceAddition<DisplayListResourceId, NonnullRefPtr<DisplayList const>>;

struct DisplayListResourceTransaction {
    Vector<FontResourceAddition> fonts;
    Vector<ImageFrameResourceAddition> image_frames;
    Vector<VideoFrameSourceResourceAddition> video_frame_sources;
    Vector<DisplayListResourceAdditionRecord> display_lists;

    Vector<FontResourceId> font_ids_to_remove;
    Vector<ImageFrameResourceId> image_frame_ids_to_remove;
    Vector<VideoFrameResourceId> video_frame_source_ids_to_remove;
    Vector<DisplayListResourceId> display_list_ids_to_remove;
};

class WEB_API DisplayListResourceStorage {
    AK_MAKE_NONCOPYABLE(DisplayListResourceStorage);
    AK_MAKE_DEFAULT_MOVABLE(DisplayListResourceStorage);

public:
    DisplayListResourceStorage() = default;
    ~DisplayListResourceStorage();

    FontResourceId add_font(Gfx::Font const&);
    ImageFrameResourceId add_image_frame(Gfx::DecodedImageFrame const&);
    VideoFrameResourceId add_video_frame_source(NonnullRefPtr<VideoFrameSource>);
    DisplayListResourceId add_display_list(NonnullRefPtr<DisplayList const>);
    void set_font(FontResourceId, Gfx::Font const&);
    void set_image_frame(ImageFrameResourceId, Gfx::DecodedImageFrame const&);
    void set_video_frame_source(VideoFrameResourceId, NonnullRefPtr<VideoFrameSource>);
    void set_display_list(DisplayListResourceId, NonnullRefPtr<DisplayList const>);
    void append_referenced_resources_from(DisplayListResourceStorage const& source, ReadonlyBytes command_bytes);
    void apply_transaction(DisplayListResourceTransaction&&);
    DisplayListResourceTransaction create_transaction(DisplayListResourceSet const& previous, DisplayListResourceSet const& current) const;
    DisplayListResourceSet collect_referenced_resources(DisplayList const&) const;
    DisplayListResourceSet collect_referenced_resources(ReadonlyBytes command_bytes) const;
    void retain_only(DisplayListResourceSet const&);
    void update_compositor_surface(CompositorSurfaceId, Gfx::SharedImage&&);
    void clear_compositor_surface(CompositorSurfaceId);

    Gfx::Font const& font(FontResourceId id) const { return *m_fonts.get(id.value()).value(); }
    Gfx::DecodedImageFrame const& image_frame(ImageFrameResourceId id) const { return m_image_frames.get(id.value()).value(); }
    NonnullRefPtr<VideoFrameSource> video_frame_source_ref(VideoFrameResourceId id) const { return const_cast<VideoFrameSource&>(*m_video_frame_sources.get(id.value()).value()); }
    VideoFrameSource& video_frame_source(VideoFrameResourceId id) { return *m_video_frame_sources.get(id.value()).value(); }
    VideoFrameSource const& video_frame_source(VideoFrameResourceId id) const { return *m_video_frame_sources.get(id.value()).value(); }
    DisplayList const& display_list(DisplayListResourceId id) const { return *m_display_lists.get(id.value()).value(); }
    Optional<Gfx::DecodedImageFrame const&> compositor_surface(CompositorSurfaceId id) const { return m_compositor_surfaces.get(id.value()); }

private:
    void collect_referenced_resources(ReadonlyBytes command_bytes, DisplayListResourceSet&) const;

    HashMap<u64, NonnullRefPtr<Gfx::Font const>> m_fonts;
    HashMap<u64, Gfx::DecodedImageFrame> m_image_frames;
    HashMap<u64, NonnullRefPtr<VideoFrameSource>> m_video_frame_sources;
    HashMap<u64, NonnullRefPtr<DisplayList const>> m_display_lists;
    HashMap<u64, Gfx::DecodedImageFrame> m_compositor_surfaces;
};

}
