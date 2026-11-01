/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/Mutex.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/NonnullRefPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <LibCompositing/DisplayList/AccumulatedVisualContext.h>
#include <LibCompositing/DisplayList/DisplayList.h>
#include <LibCompositing/DisplayList/DisplayListResourceStorage.h>
#include <LibCompositing/Scrolling/ScrollState.h>
#include <LibWeb/Compositor/CompositorFrame.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/HTML/PaintConfig.h>
#include <LibWeb/Painting/DisplayListRecording.h>
#include <LibWebCommon/Page/AsyncScrollNodeStableID.h>
#include <LibWebCommon/Page/CompositorContextId.h>

namespace Web::Layout::RustFFI {

struct FfiPresentedRecording;

}

namespace Web::Compositor {

// A display list a recording published, whether the recording made it the next one's paint command cache source, and
// the resources it references, once it is presented.
struct PublishedDisplayList {
    NonnullRefPtr<Compositing::DisplayList> display_list;
    bool replaces_paint_command_cache_source { false };
    Compositing::DisplayListResourceSet referenced_resources;
};

// What presented a navigable's last frame: the recording of a frame a rendering update committed, a tick of the
// document's clock lane, or a refresh, which records the frame a rendering update committed last again around the rects
// the compositor asked for.
enum class PresentedBy : u8 {
    Commit,
    Clock,
    Refresh,
};

// Where a smooth scroll that a navigable asked for takes a box the compositor scrolls, in device pixels.
struct SmoothScrollDestination {
    Web::AsyncScrollNodeStableID scroll_node;
    Gfx::FloatPoint offset;
};

// What a frame is built from that its navigable and document held where the frame began, sealed there: the frame reaches
// neither of them again before it is presented.
struct SealedPresentation {
    AK_ALLOC_WITH_KMALLOC;

    // The recording's paint config.
    HTML::PaintConfig paint_config;
    // The visual context tree a new display list is cut from, or the one the compositor's display list takes where the
    // document's tree changed since it took one. Only a frame that sends a tree seals one.
    Optional<Compositing::AccumulatedVisualContextTree> visual_context_tree;
    // Whether the document's tree changed since the compositor took one.
    bool sends_visual_context_tree { false };
    // The scroll state, which seeds the offsets of the boxes new to the compositor.
    Compositing::ScrollStateSnapshot scroll_state_snapshot;
    // The scrolls the navigable asked for since the frame before, which the compositor applies once it has the frame's
    // boxes. Only the frame a rendering update commits carries them.
    Vector<Compositing::ScrollRequest> scroll_requests;
    // What keyboard scrolling targets, for a top-level traversable.
    Optional<Compositing::KeyboardScrollState> keyboard_scroll_state;
    // Where the smooth scrolls the navigable asked for, which are still under way, take their boxes: a recording around
    // the scroll positions records around those destinations too.
    Vector<SmoothScrollDestination> smooth_scroll_destinations;
    // The display list a recording copies paint commands from, and the resources it references, which stay live while
    // it does.
    RefPtr<Compositing::DisplayList> paint_command_cache_source;
    Compositing::DisplayListResourceSet paint_command_cache_source_resources;

    // For frames presented beside the event loop: the recording they publish, sealed where the recording began, where
    // they go and the rect they are presented in, what presents them, and, once one is presented, the display list it
    // published. A clock lane presents a frame from the seal at each tick that records one.
    Optional<Painting::DisplayListRecording> recording;
    Web::CompositorContextId context_id;
    Optional<Gfx::IntRect> present_viewport_rect;
    PresentedBy presented_by { PresentedBy::Commit };
    Optional<PublishedDisplayList> published;
    // Whether the frame sends a visual context tree the compositor's display list was not recorded against, for a test.
    bool visual_context_tree_mismatches_for_testing { false };
    // Whether a clock lane's frames copy paint commands from a refresh's display list, whose resources the presenter
    // keeps only while the compositor shows it.
    bool copies_paint_commands_from_refresh { false };
    // The frame a rendering update committed, counted per navigable, or the one a clock lane's frames or a refresh start
    // from: a lane's frame or a refresh of an older one than the presenter presented last shows what the screen no
    // longer shows.
    u64 generation { 0 };
};

// The display lists of the SVG images a frame renders, which the main thread renders for the frame into a resource
// storage of their own, with what they reference: the Paint thread hands them to the frame's presenter.
struct VectorImageResources {
    AK_ALLOC_WITH_KMALLOC;

    Compositing::DisplayListResourceStorage storage;
};

// What a navigable presents to its compositor context from: the resource storage its recordings add to, and the
// display list the compositor context holds with the resources it holds for it. The navigable and what presents a frame
// of it beside the event loop share it: the Paint thread presents, and the main thread reads what it presented. Only the
// Paint thread reaches the resource storage, but for a recording of the main thread's own, which holds the document's
// clock lane meanwhile. The presenter holds no GC pointer.
class WEB_API NavigablePresenter : public AtomicRefCounted<NavigablePresenter> {
    AK_MAKE_NONCOPYABLE(NavigablePresenter);
    AK_MAKE_NONMOVABLE(NavigablePresenter);

public:
    AK_ALLOC_WITH_KMALLOC;

    static NonnullRefPtr<NavigablePresenter> create() { return adopt_ref(*new NavigablePresenter); }

    Compositing::DisplayListResourceStorage& display_list_resource_storage() { return m_resource_storage; }
    Compositing::DisplayListResourceStorage const& display_list_resource_storage() const { return m_resource_storage; }

    // The display list the compositor context holds, and the paint config it was recorded with.
    RefPtr<Compositing::DisplayList> compositor_display_list() const
    {
        MutexLocker locker(m_mutex);
        return m_compositor_display_list;
    }
    Optional<HTML::PaintConfig> compositor_display_list_paint_config() const
    {
        MutexLocker locker(m_mutex);
        return m_compositor_display_list_paint_config;
    }

    // Forgets what the compositor context holds: a new compositor process holds nothing.
    void forget_compositor_display_list();

    Optional<PresentedBy> last_frame_presented_by() const
    {
        MutexLocker locker(m_mutex);
        return m_last_frame_presented_by;
    }
    // The generation of the keyboard scroll state the last frame handed the compositor, if it handed one.
    Optional<u64> last_keyboard_scroll_state_generation() const
    {
        MutexLocker locker(m_mutex);
        return m_last_keyboard_scroll_state_generation;
    }

    // The seal a clock lane presents its frames with, which start from `committed`, the frame presented last, or none where
    // the compositor shows another display list than the one that frame records from: the lane's recordings copy paint
    // commands from it. On the Paint thread.
    OwnPtr<SealedPresentation> seal_for_clock_lane(SealedPresentation const& committed) const;

    // The seal a clock lane presents its frames with, which start from `committed`, a frame that keeps the display list
    // the compositor has, where that display list is the one `refreshed`, the last refresh of the frame a rendering update
    // committed before, presented, or none where the compositor shows another: the lane's recordings copy paint commands
    // from it. On the Paint thread.
    OwnPtr<SealedPresentation> seal_for_clock_lane_of_refresh_chain(SealedPresentation const& committed, SealedPresentation const& refreshed) const;

    // A clock lane's frame shows samples the frame a rendering update committed last lacks, which a refresh of that frame
    // would take off the screen.
    bool shows_clock_frame() const;

    // The seal of a refresh, which records the frame a rendering update committed last again around other rects, from
    // `retained`, the seal that frame or the last refresh of it presented with, or none where that frame no longer
    // shows: the compositor shows another display list than `retained` presented, or a clock lane's frame since, or has
    // a tree of another structure or a viewport of another size. The refresh copies paint commands from the compositor's
    // display list, with the tree the compositor has and the scroll state the last committed frame presented with,
    // which an unrecorded frame may have moved past `retained`'s. On the Paint thread.
    OwnPtr<SealedPresentation> seal_for_refresh(SealedPresentation const& retained) const;

private:
    // Only the Paint thread builds and presents frames.
    friend struct PresenterFFI;

    // Builds the frame that brings the compositor context `sealed` names up to date with `published`, the display list a
    // recording just published, or with what changed for the one the compositor has where none was published, from what
    // `sealed` sealed. Reads no navigable or document.
    CompositorFrame build_frame(SealedPresentation const&, Optional<PublishedDisplayList>);

    // Builds the frame `sealed` sealed, with `display_list`, the display list its recording published, beside the event
    // loop, keeping what it published in the seal, which the next frame from the seal copies paint commands from.
    CompositorFrame build_frame_beside_event_loop(SealedPresentation&, NonnullRefPtr<Compositing::DisplayList>);

    // A recording beside the event loop that copies paint commands from the compositor's display list, against the tree
    // the compositor has. With `m_mutex` held, while the compositor has both.
    Painting::DisplayListRecording recording_from_compositor_display_list() const;

    // The seal of a clock lane's frames that start from `committed` and copy paint commands from the compositor's display
    // list. With `m_mutex` held, while the compositor has a display list and its tree.
    NonnullOwnPtr<SealedPresentation> clock_lane_seal_from_compositor_display_list(SealedPresentation const& committed) const;

    NavigablePresenter() = default;

    // Guards what the main thread reads of what the Paint thread presented.
    mutable Mutex m_mutex;
    Compositing::DisplayListResourceStorage m_resource_storage;
    Optional<HTML::PaintConfig> m_compositor_display_list_paint_config;
    RefPtr<Compositing::DisplayList> m_compositor_display_list;
    u64 m_compositor_display_list_visual_context_tree_structural_epoch { 0 };
    Compositing::DisplayListResourceSet m_compositor_display_list_resources;
    Compositing::DisplayListResourceSet m_compositor_display_list_command_resources;
    Optional<PresentedBy> m_last_frame_presented_by;
    Optional<u64> m_last_keyboard_scroll_state_generation;
    // The visual context tree the compositor's display list goes with, which a clock lane's recordings are cut from.
    Optional<Compositing::AccumulatedVisualContextTree> m_compositor_visual_context_tree;
    // The generation of the last frame a rendering update committed that the presenter presented.
    u64 m_last_committed_generation { 0 };
    // What the display list the next frame a rendering update commits copies paint commands from references, which the
    // frames of a clock lane and refreshes keep beside their own.
    Compositing::DisplayListResourceSet m_committed_command_resources;
    // What the display list of the last refresh presented since the last frame a rendering update committed references,
    // which the frames of a clock lane and later refreshes keep beside their own.
    Compositing::DisplayListResourceSet m_refresh_chain_command_resources;
    // The scroll state, keyboard scroll state and viewport rect the last frame a rendering update committed presented
    // with, which a refresh presents with.
    Compositing::ScrollStateSnapshot m_last_committed_scroll_state_snapshot;
    Optional<Compositing::KeyboardScrollState> m_last_committed_keyboard_scroll_state;
    Optional<Gfx::IntRect> m_last_committed_present_viewport_rect;
    // The content generation of the last display list the presenter sent the compositor, which each new display list of
    // a committed frame or a clock lane's frame counts up, and a refresh's keeps.
    u64 m_content_generation { 0 };
};

// A navigable's presenter and a frame sealed for it, which what presents the frame beside the event loop holds until it
// lands.
struct FlightPresentation {
    NonnullRefPtr<NavigablePresenter> presenter;
    NonnullOwnPtr<SealedPresentation> sealed;
};

}

// What a recording that presents beside the event loop reaches of its presenter and seal, which it owns meanwhile.
extern "C" {
WEB_API void web_navigable_presenter_unref(void* presenter);
WEB_API void* web_navigable_presenter_seal_for_clock_lane(void* presenter, void const* committed);
WEB_API void* web_navigable_presenter_seal_for_clock_lane_of_refresh_chain(void* presenter, void const* committed, void const* refreshed);
WEB_API void* web_navigable_presenter_seal_for_refresh(void* presenter, void const* retained);
WEB_API void* web_navigable_presenter_keep_presented_seal(void* presenter, void const* presented);
WEB_API bool web_navigable_presenter_shows_clock_frame(void const* presenter);
WEB_API void web_sealed_presentation_destroy(void* sealed);
WEB_API u64 web_sealed_presentation_context_id(void const* sealed);
WEB_API void web_sealed_presentation_take_generation(void* sealed, void const* committed);
WEB_API void web_sealed_presentation_take_visual_context_tree(void* sealed, void const* tree, Gfx::FloatPoint const* restructured_scroll_offsets, size_t scroll_offset_count);
WEB_API void web_navigable_presenter_add_font(void* presenter, void const* font);
WEB_API void web_navigable_presenter_add_image_frame(void* presenter, void const* frame);
WEB_API void web_navigable_presenter_add_video_sink(void* presenter, u64 resource_id, u64 sink_handle);
WEB_API void web_vector_image_resources_destroy(void* resources);
WEB_API void web_navigable_presenter_take_vector_image_resources(void* presenter, void* resources, u64 const* display_list_ids, size_t display_list_id_count);
}
