/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/NumericLimits.h>
#include <AK/TemporaryChange.h>
#include <LibGfx/PaintingSurface.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWeb/Painting/DisplayList.h>

namespace Web::Painting {

static Atomic<u64> s_next_id { 1 };

static void set_command_sequence_visual_context(Bytes command_bytes, VisualContextIndex context_index)
{
    for (size_t offset = 0; offset < command_bytes.size();) {
        VERIFY(offset + sizeof(DisplayListCommandHeader) <= command_bytes.size());
        auto* header_data = command_bytes.data() + offset;
        auto header = read_display_list_object<DisplayListCommandHeader>({ header_data, command_bytes.size() - offset });
        header.context_index = context_index;
        write_display_list_object(Bytes { header_data, sizeof(header) }, header);
        offset += sizeof(header) + header.payload_size;
        VERIFY(offset <= command_bytes.size());
    }
}

DisplayList::DisplayList(u64 compatible_visual_context_tree_version)
    : m_compatible_visual_context_tree_version(compatible_visual_context_tree_version)
    , m_id(s_next_id.fetch_add(1, AK::MemoryOrder::memory_order_relaxed))
{
}

DisplayList::DisplayList(u64 compatible_visual_context_tree_version, u64 id, ByteBuffer&& command_bytes, Optional<AsyncScrollingMetadata> async_scrolling_metadata)
    : m_compatible_visual_context_tree_version(compatible_visual_context_tree_version)
    , m_id(id)
    , m_command_bytes(move(command_bytes))
    , m_async_scrolling_metadata(move(async_scrolling_metadata))
{
}

bool DisplayList::append_bytes(
    DisplayListCommandType type,
    ReadonlyBytes payload,
    ReadonlyBytes inline_data,
    AccumulatedVisualContextTree const& visual_context_tree,
    VisualContextIndex context_index,
    bool context_geometry_only,
    Optional<Gfx::IntRect> bounding_rect,
    bool is_clip)
{
    VERIFY(visual_context_tree.version() == m_compatible_visual_context_tree_version);
    // Geometry-only commands ignore the chain's clips, so an empty effective clip doesn't make them invisible.
    if (!context_geometry_only && visual_context_tree.has_empty_effective_clip(context_index))
        return false;
    VERIFY(m_command_bytes.size() % DisplayList::command_alignment == 0);
    VERIFY(payload.size() <= NumericLimits<u32>::max());
    VERIFY(inline_data.size() <= NumericLimits<u32>::max() - payload.size());
    auto payload_size = payload.size() + inline_data.size();
    auto record_size = sizeof(DisplayListCommandHeader) + payload_size;
    constexpr auto command_alignment = DisplayList::command_alignment;
    auto trailing_padding = align_up_to(record_size, command_alignment) - record_size;
    VERIFY(trailing_padding <= NumericLimits<u32>::max() - payload_size);
    DisplayListCommandHeader header {
        .type = type,
        .payload_size = static_cast<u32>(payload_size + trailing_padding),
        .context_index = context_index,
        .context_geometry_only = context_geometry_only,
        .has_bounding_rect = bounding_rect.has_value(),
        .is_clip = is_clip,
        .bounding_rect = bounding_rect.value_or({}),
    };
    auto header_bytes = display_list_object_bytes(header);
    m_command_bytes.append(header_bytes.data(), header_bytes.size());
    m_command_bytes.append(payload.data(), payload.size());
    if (!inline_data.is_empty())
        m_command_bytes.append(inline_data.data(), inline_data.size());
    m_command_bytes.resize(m_command_bytes.size() + trailing_padding, ByteBuffer::ZeroFillNewElements::Yes);
    return true;
}

u32 DisplayList::append_command_range_from(
    DisplayList const& source_display_list,
    DisplayListCommandRange source_range,
    AccumulatedVisualContextTree const& visual_context_tree,
    VisualContextIndex recorded_context_index,
    VisualContextIndex current_context_index)
{
    VERIFY(&source_display_list != this);
    VERIFY(visual_context_tree.version() == m_compatible_visual_context_tree_version);
    VERIFY(m_command_bytes.size() % DisplayList::command_alignment == 0);
    VERIFY(source_range.size % DisplayList::command_alignment == 0);
    VERIFY(static_cast<size_t>(source_range.offset) + source_range.size <= source_display_list.m_command_bytes.size());

    auto destination_offset = m_command_bytes.size();
    VERIFY(destination_offset + source_range.size <= NumericLimits<u32>::max());
    if (source_range.is_empty())
        return static_cast<u32>(destination_offset);

    m_command_bytes.append(source_display_list.m_command_bytes.data() + source_range.offset, source_range.size);
    // The copied headers already carry the index the range was recorded under, so they only need rewriting
    // when the paintable's context was assigned a different index since then.
    if (recorded_context_index != current_context_index)
        set_command_sequence_visual_context(m_command_bytes.span().slice(destination_offset, source_range.size), current_context_index);
    return static_cast<u32>(destination_offset);
}

void DisplayListPlayer::execute(
    DisplayList const& display_list,
    AccumulatedVisualContextTree const& visual_context_tree,
    DisplayListResourceStorage const& resource_storage,
    ScrollStateSnapshot const& scroll_state_snapshot,
    RefPtr<Gfx::PaintingSurface> surface,
    CanvasSurfaceRegistry const* canvas_surface_registry)
{
    VERIFY(display_list.compatible_visual_context_tree_version() == visual_context_tree.version());
    m_surface = surface;
    m_active_display_list = &display_list;
    m_active_visual_context_tree = &visual_context_tree;
    m_resource_storage = &resource_storage;
    m_canvas_surface_registry = canvas_surface_registry;
    execute_impl(display_list, scroll_state_snapshot);
    m_canvas_surface_registry = nullptr;
    m_resource_storage = nullptr;
    m_active_visual_context_tree = nullptr;
    m_active_display_list = nullptr;
    m_surface = nullptr;
}

void DisplayListPlayer::execute_display_list_into_surface(DisplayList const& display_list, AccumulatedVisualContextTree const& visual_context_tree, Gfx::PaintingSurface& target_surface)
{
    VERIFY(display_list.compatible_visual_context_tree_version() == visual_context_tree.version());
    TemporaryChange surface_change { m_surface, RefPtr<Gfx::PaintingSurface> { target_surface } };
    TemporaryChange display_list_change { m_active_display_list, &display_list };
    TemporaryChange visual_context_tree_change { m_active_visual_context_tree, &visual_context_tree };
    VERIFY(m_resource_storage);
    ScrollStateSnapshot scroll_state_snapshot;
    execute_impl(display_list, scroll_state_snapshot);
}

void DisplayListPlayer::execute_nested_display_list(
    DisplayList const& display_list,
    AccumulatedVisualContextTree const& visual_context_tree,
    ScrollStateSnapshot const& scroll_state_snapshot,
    ReadonlyBytes command_bytes)
{
    VERIFY(display_list.compatible_visual_context_tree_version() == visual_context_tree.version());
    TemporaryChange display_list_change { m_active_display_list, &display_list };
    TemporaryChange visual_context_tree_change { m_active_visual_context_tree, &visual_context_tree };
    VERIFY(m_resource_storage);
    execute_impl(display_list, scroll_state_snapshot, command_bytes);
}

void DisplayListPlayer::execute_impl(DisplayList const& display_list, ScrollStateSnapshot const& scroll_state)
{
    execute_impl(display_list, scroll_state, display_list.command_bytes());
}

void DisplayListPlayer::execute_impl(
    DisplayList const& display_list,
    ScrollStateSnapshot const& scroll_state,
    ReadonlyBytes commands)
{
    auto const& visual_context_tree = active_visual_context_tree();
    VERIFY(display_list.compatible_visual_context_tree_version() == visual_context_tree.version());

    VERIFY(m_surface);

    auto apply_accumulated_visual_context =
        [&](VisualContextIndex node_index, AccumulatedVisualContextNode const& node, bool geometry_only) {
            // Geometry-only application skips nodes that don't affect coordinates, but still pushes a
            // save so the chain keeps one canvas save per node, which restore_to_depth relies on.
            if (geometry_only && (node.data.has<ClipData>() || node.data.has<ClipPathData>() || node.data.has<EffectsData>())) {
                play_command(Save {});
                return;
            }
            node.data.visit(
                [&](EffectsData const& effects) {
                    play_command(ApplyEffects {
                                     .opacity = effects.opacity,
                                     .compositing_and_blending_operator = effects.blend_mode,
                                     .has_filter = effects.gfx_filter.has_value(),
                                     .filter_data = {},
                                 },
                        effects.gfx_filter.has_value() ? &effects.gfx_filter.value() : nullptr);
                },
                [&](PerspectiveData const& perspective) {
                    play_command(Save {});
                    apply_transform({ 0, 0 }, perspective.matrix);
                },
                [&](ScrollData const&) {
                    play_command(Save {});
                    auto offset = scroll_state.device_offset_for_index(to_scroll_node_index(node_index));
                    if (!offset.is_zero())
                        play_command(Translate { .delta = offset.to_type<int>() });
                },
                [&](ScrollCompensation const& compensation) {
                    play_command(Save {});
                    auto offset = -scroll_state.device_offset_for_index(compensation.scroll_node_index);
                    if (!offset.is_zero())
                        play_command(Translate { .delta = offset.to_type<int>() });
                },
                [&](AnchorScrollShift const& shift) {
                    play_command(Save {});
                    auto offset = shift.masked_offset(scroll_state);
                    if (!offset.is_zero())
                        play_command(Translate { .delta = offset.to_type<int>() });
                },
                [&](TransformData const& transform) {
                    play_command(Save {});
                    apply_transform(transform.origin, transform.matrix);
                },
                [&](ClipData const& clip) {
                    play_command(Save {});
                    if (clip.corner_radii.has_any_radius()) {
                        play_command(AddRoundedRectClip {
                            .corner_radii = clip.corner_radii,
                            .border_rect = clip.rect.to_type<int>(),
                            .corner_clip = Gfx::CornerClip::Outside,
                        });
                    } else {
                        play_command(AddClipRect { .rect = clip.rect.to_type<int>() });
                    }
                },
                [&](ClipPathData const& clip_path) {
                    play_command(Save {});
                    add_clip_path(clip_path.path, clip_path.fill_rule);
                });
        };

    // Derived per-kind references for one recording context: the deepest coordinate-affecting
    // (spatial), clip, and effect node on the chain from the root to the context. The chain's tip
    // always has one of the three kinds, so the union of the three references' ancestor chains
    // reproduces the recorded chain exactly; tracking the triple instead of the raw context index
    // is what the split into spatial/clip/effect trees generalizes.
    struct DerivedContextRefs {
        VisualContextIndex spatial { VISUAL_VIEWPORT_NODE_INDEX };
        VisualContextIndex clip { VISUAL_VIEWPORT_NODE_INDEX };
        VisualContextIndex effect { VISUAL_VIEWPORT_NODE_INDEX };
        bool operator==(DerivedContextRefs const&) const = default;
    };

    Vector<Optional<DerivedContextRefs>> derived_refs_by_context;
    derived_refs_by_context.resize(visual_context_tree.nodes().size());
    derived_refs_by_context[VISUAL_VIEWPORT_NODE_INDEX.value()] = DerivedContextRefs {};

    auto derive_context_refs = [&](VisualContextIndex context_index) -> DerivedContextRefs {
        Vector<VisualContextIndex, 16> pending;
        auto index = context_index;
        while (!derived_refs_by_context[index.value()].has_value()) {
            pending.append(index);
            index = visual_context_tree.node_at(index).parent_index;
        }
        auto refs = *derived_refs_by_context[index.value()];
        for (size_t i = pending.size(); i > 0; --i) {
            auto node_index = pending[i - 1];
            auto const& node = visual_context_tree.node_at(node_index);
            if (node.data.has<ClipData>() || node.data.has<ClipPathData>())
                refs.clip = node_index;
            else if (node.data.has<EffectsData>())
                refs.effect = node_index;
            else
                refs.spatial = node_index;
            derived_refs_by_context[node_index.value()] = refs;
        }
        return refs;
    };

    // The chain of visual context nodes currently applied to the canvas, root node included and
    // first. Every entry holds exactly one canvas save (a Save, or the layer pushed by
    // ApplyEffects), so unwinding is one Restore per popped entry.
    Vector<VisualContextIndex, 32> applied_chain;
    Vector<VisualContextIndex, 32> target_chain;
    DerivedContextRefs applied_refs;
    bool has_applied_context { false };
    bool applied_context_geometry_only { false };

    auto restore_to_length = [&](size_t length) {
        while (applied_chain.size() > length) {
            play_command(Restore {});
            applied_chain.take_last();
        }
    };

    // OPTIMIZATION: When walking down to apply effects (opacity, filters, blend modes), check culling before applying
    //               each effect. Effects don't affect clip state, so the culling check is valid before applying them.
    //               This avoids expensive saveLayer/restore cycles for off-screen elements with effects like blur.
    enum class SwitchResult : u8 {
        Switched,
        CulledByEffect,
    };
    auto switch_to_context = [&](DerivedContextRefs const& refs, VisualContextIndex target_index, bool geometry_only, Optional<Gfx::IntRect> bounding_rect = {}) -> SwitchResult {
        if (has_applied_context && applied_refs == refs && applied_context_geometry_only == geometry_only)
            return SwitchResult::Switched;

        // A chain applied in one mode can't be partially reused in the other: the modes disagree on what
        // every shared ancestor contributes. Unwind fully and reapply from the root instead.
        if (has_applied_context && applied_context_geometry_only != geometry_only) {
            restore_to_length(0);
            has_applied_context = false;
        }

        // Parents always have smaller indices than their children, so the deepest of the three
        // per-kind references is the recorded context itself.
        VERIFY(max(refs.spatial.value(), max(refs.clip.value(), refs.effect.value())) == target_index.value());
        target_chain.clear_with_capacity();
        for (auto index = target_index;; index = visual_context_tree.node_at(index).parent_index) {
            target_chain.append(index);
            if (index == VISUAL_VIEWPORT_NODE_INDEX)
                break;
        }
        target_chain.reverse();

        size_t common_prefix_length = 0;
        while (common_prefix_length < applied_chain.size()
            && common_prefix_length < target_chain.size()
            && applied_chain[common_prefix_length] == target_chain[common_prefix_length])
            ++common_prefix_length;

        restore_to_length(common_prefix_length);

        for (size_t i = common_prefix_length; i < target_chain.size(); ++i) {
            auto node_index = target_chain[i];
            auto const& node = visual_context_tree.node_at(node_index);
            if (!geometry_only && bounding_rect.has_value() && node.data.has<EffectsData>()) {
                // A coordinate-affecting node sits below this effect exactly when the target's
                // deepest spatial reference is deeper than the effect, in which case the bounding
                // rect cannot be culled against the current canvas state.
                auto can_cull_before_effect = refs.spatial.value() <= node_index.value();
                if (bounding_rect->is_empty() || (can_cull_before_effect && would_be_fully_clipped_by_painter(*bounding_rect))) {
                    restore_to_length(common_prefix_length);
                    // The canvas is unwound to the shared prefix, so the applied state has to
                    // follow: keeping the previous, deeper context would let the next switch reuse
                    // nodes the unwind just popped.
                    if (applied_chain.is_empty()) {
                        has_applied_context = false;
                    } else {
                        applied_refs = derive_context_refs(applied_chain.last());
                        applied_context_geometry_only = geometry_only;
                        has_applied_context = true;
                    }
                    return SwitchResult::CulledByEffect;
                }
            }
            apply_accumulated_visual_context(node_index, node, geometry_only);
            applied_chain.append(node_index);
        }

        applied_refs = refs;
        applied_context_geometry_only = geometry_only;
        has_applied_context = true;
        return SwitchResult::Switched;
    };

    DisplayList::for_each_command_header(commands, [&](DisplayListCommandHeader const& header, ReadonlyBytes payload) {
        if (display_list_command_is_compositor_metadata(header.type))
            return;

        auto bounding_rect = header.has_bounding_rect
            ? Optional<Gfx::IntRect>(header.bounding_rect)
            : Optional<Gfx::IntRect> {};

        if (switch_to_context(derive_context_refs(header.context_index), header.context_index, header.context_geometry_only, bounding_rect) == SwitchResult::CulledByEffect)
            return;

        if (bounding_rect.has_value() && (bounding_rect->is_empty() || would_be_fully_clipped_by_painter(*bounding_rect))) {
            // Any clip that's located outside of the visible region is equivalent to a simple clip-rect,
            // so replace it with one to avoid doing unnecessary work.
            if (header.is_clip) {
                if (header.type == DisplayListCommandType::AddClipRect)
                    play_command(read_display_list_command_payload<AddClipRect>(payload));
                else
                    play_command(AddClipRect { bounding_rect.release_value() });
            }
            return;
        }

        TemporaryChange current_command_payload_change { m_current_command_payload, payload };
        auto dispatch_command = [&]<DisplayListCommand Command>(auto&& callback) {
            auto command = read_display_list_command_payload<Command>(payload);
            if constexpr (IsSame<Command, PaintScrollBar>) {
                auto device_offset = scroll_state.device_offset_for_index(command.scroll_node_index);
                if (command.vertical)
                    command.thumb_rect.translate_by(0, static_cast<int>(-device_offset.y() * command.scroll_size));
                else
                    command.thumb_rect.translate_by(static_cast<int>(-device_offset.x() * command.scroll_size), 0);
            }
            callback(command);
        };

        switch (header.type) {
#define DISPATCH_DISPLAY_LIST_COMMAND(command_type, player_method)                    \
    case DisplayListCommandType::command_type:                                        \
        dispatch_command.template operator()<command_type>([&](auto const& command) { \
            play_command(command);                                                    \
        });                                                                           \
        break;
            ENUMERATE_DISPLAY_LIST_COMMANDS(DISPATCH_DISPLAY_LIST_COMMAND)
#undef DISPATCH_DISPLAY_LIST_COMMAND
        }
    });

    restore_to_length(0);
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayList::AsyncScrollingMetadata const& metadata)
{
    TRY(encoder.encode(metadata.viewport_rect));
    TRY(encoder.encode(metadata.wheel_event_listener_state_generation));
    TRY(encoder.encode(metadata.has_blocking_wheel_event_listeners));
    TRY(encoder.encode(metadata.has_blocking_wheel_event_region_covering_viewport));
    return {};
}

template<>
ErrorOr<Web::Painting::DisplayList::AsyncScrollingMetadata> decode(Decoder& decoder)
{
    return Web::Painting::DisplayList::AsyncScrollingMetadata {
        .viewport_rect = TRY(decoder.decode<Gfx::IntRect>()),
        .wheel_event_listener_state_generation = TRY(decoder.decode<u64>()),
        .has_blocking_wheel_event_listeners = TRY(decoder.decode<bool>()),
        .has_blocking_wheel_event_region_covering_viewport = TRY(decoder.decode<bool>()),
    };
}

template<>
ErrorOr<void> encode(Encoder& encoder, Web::Painting::DisplayList const& display_list)
{
    TRY(encoder.encode(display_list.m_id));
    TRY(encoder.encode(display_list.m_command_bytes));
    TRY(encoder.encode(display_list.m_compatible_visual_context_tree_version));
    TRY(encoder.encode(display_list.m_async_scrolling_metadata));
    return {};
}

template<>
ErrorOr<void> encode(Encoder& encoder, NonnullRefPtr<Web::Painting::DisplayList> const& display_list)
{
    return encoder.encode(*display_list);
}

template<>
ErrorOr<NonnullRefPtr<Web::Painting::DisplayList>> decode(Decoder& decoder)
{
    auto id = TRY(decoder.decode<u64>());
    auto command_bytes = TRY(decoder.decode<ByteBuffer>());
    auto compatible_visual_context_tree_version = TRY(decoder.decode<u64>());
    auto async_scrolling_metadata = TRY(decoder.decode<Optional<Web::Painting::DisplayList::AsyncScrollingMetadata>>());
    return adopt_ref(*new Web::Painting::DisplayList(compatible_visual_context_tree_version, id, move(command_bytes), move(async_scrolling_metadata)));
}

}
