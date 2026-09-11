/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <AK/Queue.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Message.h>
#include <LibTest/TestCase.h>
#include <LibWeb/Compositor/AsyncScrollTree.h>
#include <LibWeb/Compositor/ScrollSnapState.h>
#include <LibWeb/Compositor/Types.h>

using namespace Web;
using namespace Web::Compositor;

static AsyncScrollNodeID node_id { UniqueNodeID { 1 }, Painting::SpatialNodeIndex { 1 } };
static AsyncScrollNodeStableID stable_id { UniqueNodeID { 2 }, AsyncScrollNodeKind::Element };

static AsyncScrollingState scroll_tree_state()
{
    AsyncScrollingState state;
    AsyncScrollNode node;
    node.node_id = node_id;
    node.stable_node_id = stable_id;
    state.scroll_nodes.append(move(node));
    return state;
}

static ScrollSnapStateSnapshot snap_state(u64 revision = 1)
{
    ScrollSnapStateSnapshot state { .document_id = node_id.document_id, .revision = revision, .device_pixels_per_css_pixel = 2, .containers = {} };
    SnapContainerData data {
        .axes = { false, true },
        .strictness = CSS::ScrollSnapStrictness::Mandatory,
        .snapport_size = { 200, 200 },
        .min_scroll_offset = {},
        .max_scroll_offset = { 0, 800 },
        .candidates = {},
    };
    data.candidates.y_candidates.append({
        .offset = CSSPixels::from_raw(200 * 64 + 1),
        .area = { UniqueNodeID { 3 }, CSS::PseudoElement::Before },
        .covering_ranges = { { 200, 400 } },
        .cross_axis_visible_range_start = -200,
        .cross_axis_visible_range_end = 200,
        .always_stop = true,
    });
    state.containers.append({ stable_id, move(data) });
    return state;
}

template<typename T>
static ErrorOr<T> round_trip(T const& state)
{
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };
    TRY(encoder.encode(state));
    auto bytes = buffer.take_data();
    FixedMemoryStream stream { bytes.span() };
    Queue<IPC::Attachment> attachments;
    IPC::Decoder decoder { stream, attachments };
    return decoder.decode<T>();
}

TEST_CASE(snapshot_round_trip_preserves_fixed_point_geometry_and_pseudo_identity)
{
    auto original = snap_state();
    auto decoded = MUST(round_trip(original));
    EXPECT(decoded == original);
}

TEST_CASE(snapshot_decode_rejects_invalid_scale_and_pseudo_element)
{
    auto state = snap_state();
    state.device_pixels_per_css_pixel = 0;
    EXPECT(round_trip(state).is_error());
    state.device_pixels_per_css_pixel = 2;
    state.containers[0].data.candidates.y_candidates[0].area.pseudo_element = static_cast<CSS::PseudoElement>(255);
    EXPECT(round_trip(state).is_error());
}

TEST_CASE(candidates_refresh_without_replacing_the_scroll_tree)
{
    AsyncScrollTree tree;
    tree.set_state(scroll_tree_state());
    tree.set_snap_state(snap_state());
    EXPECT(tree.snap_data_for_node(node_id));
    EXPECT_EQ(tree.snap_device_pixels_per_css_pixel(), 2);

    auto replacement = snap_state(2);
    replacement.containers[0].data.candidates.y_candidates[0].offset = 600;
    replacement.device_pixels_per_css_pixel = 3;
    tree.set_snap_state(move(replacement));
    EXPECT_EQ(tree.snap_data_for_node(node_id)->candidates.y_candidates[0].offset, CSSPixels { 600 });
    EXPECT_EQ(tree.snap_device_pixels_per_css_pixel(), 3);

    tree.set_snap_state(snap_state(1));
    EXPECT_EQ(tree.snap_data_for_node(node_id)->candidates.y_candidates[0].offset, CSSPixels { 600 });
}

TEST_CASE(empty_replacement_clears_candidates)
{
    AsyncScrollTree tree;
    tree.set_state(scroll_tree_state());
    tree.set_snap_state(snap_state());
    auto empty = snap_state(2);
    empty.containers.clear();
    tree.set_snap_state(move(empty));
    EXPECT(!tree.snap_data_for_node(node_id));
}

TEST_CASE(tree_replacement_retains_matching_identities_and_drops_old_documents)
{
    AsyncScrollTree tree;
    tree.set_state(scroll_tree_state());
    tree.set_snap_state(snap_state());
    tree.set_state(scroll_tree_state());
    EXPECT(tree.snap_data_for_node(node_id));

    auto new_document = scroll_tree_state();
    new_document.scroll_nodes[0].node_id.document_id = UniqueNodeID { 10 };
    tree.set_state(move(new_document));
    EXPECT(!tree.snap_data_for_node(node_id));
    EXPECT(!tree.snap_data_for_node({ UniqueNodeID { 10 }, node_id.scroll_node_index }));
    tree.set_snap_state(snap_state(3));
    EXPECT(!tree.snap_data_for_node({ UniqueNodeID { 10 }, node_id.scroll_node_index }));
}

TEST_CASE(input_round_trip_preserves_precision_and_zero_delta_gesture_end)
{
    AsyncScrollInput input { WheelDeltaPrecision::Precise, ScrollGesturePhase::Ended };
    auto decoded = MUST(round_trip(input));
    EXPECT_EQ(decoded.precision, input.precision);
    EXPECT_EQ(decoded.phase, input.phase);
    input.phase = static_cast<ScrollGesturePhase>(255);
    EXPECT(round_trip(input).is_error());
}

TEST_CASE(user_scroll_completion_is_separate_from_input_acknowledgements)
{
    PendingAsyncScrollUpdates updates;
    updates.sequence = 42;
    updates.completed_operation_ids.append(17);
    UserScrollUpdate gesture {
        .stable_node_id = stable_id,
        .gesture_id = 3,
        .status = UserScrollStatus::Active,
        .initial_scroll_offset = { 0, CSSPixels::from_raw(65) },
        .snap_destination = select_snap_destination(snap_state().containers[0].data, { 0, 250 }),
    };
    updates.user_scroll_updates.append(gesture);
    auto decoded = MUST(round_trip(updates));
    EXPECT_EQ(decoded.completed_operation_ids[0], 17u);
    EXPECT_EQ(decoded.user_scroll_updates[0].status, UserScrollStatus::Active);
    EXPECT_EQ(decoded.user_scroll_updates[0].initial_scroll_offset, gesture.initial_scroll_offset);
    EXPECT_EQ(decoded.user_scroll_updates[0].snap_destination->position, gesture.snap_destination->position);
    EXPECT(decoded.user_scroll_updates[0].snap_destination->snapped_areas == gesture.snap_destination->snapped_areas);
}

TEST_CASE(merging_publications_preserves_settlement_before_the_next_gesture)
{
    PendingAsyncScrollUpdates pending;
    PendingAsyncScrollUpdates finished;
    finished.sequence = 10;
    finished.user_scroll_updates.append({
        .stable_node_id = stable_id,
        .gesture_id = 1,
        .status = UserScrollStatus::Settled,
        .initial_scroll_offset = {},
        .snap_destination = {},
        .did_scroll = true,
    });
    merge_async_scroll_updates(pending, move(finished));
    PendingAsyncScrollUpdates started;
    started.sequence = 11;
    started.user_scroll_updates.append({
        .stable_node_id = stable_id,
        .gesture_id = 2,
        .status = UserScrollStatus::Active,
        .initial_scroll_offset = { 0, 200 },
        .snap_destination = {},
    });
    merge_async_scroll_updates(pending, move(started));
    EXPECT_EQ(pending.sequence, 11u);
    EXPECT_EQ(pending.user_scroll_updates.size(), 2u);
    EXPECT_EQ(pending.user_scroll_updates[0].status, UserScrollStatus::Settled);
    EXPECT_EQ(pending.user_scroll_updates[1].status, UserScrollStatus::Active);
    EXPECT(pending.scroll_offsets.is_empty());
}

TEST_CASE(visual_viewport_pans_are_not_merged_into_absolute_layout_scrolls)
{
    PendingAsyncScrollUpdates pending;
    PendingAsyncScrollUpdates layout;
    layout.scroll_offsets.append({ stable_id, { 0, 100 }, { 0, 20 } });
    merge_async_scroll_updates(pending, move(layout));
    PendingAsyncScrollUpdates visual;
    visual.scroll_offsets.append({ stable_id, { 0, 100 }, { 0, 30 }, true });
    merge_async_scroll_updates(pending, move(visual));
    auto decoded = MUST(round_trip(pending));
    EXPECT_EQ(decoded.scroll_offsets.size(), 2u);
    EXPECT(!decoded.scroll_offsets[0].is_visual_viewport_pan);
    EXPECT(decoded.scroll_offsets[1].is_visual_viewport_pan);
    EXPECT_EQ(decoded.scroll_offsets[0].unadopted_scroll_delta, Gfx::FloatPoint(0, 20));
    EXPECT_EQ(decoded.scroll_offsets[1].unadopted_scroll_delta, Gfx::FloatPoint(0, 30));
}
