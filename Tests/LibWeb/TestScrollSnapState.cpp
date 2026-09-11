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

static ErrorOr<ScrollSnapStateSnapshot> round_trip(ScrollSnapStateSnapshot const& state)
{
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };
    TRY(encoder.encode(state));
    auto bytes = buffer.take_data();
    FixedMemoryStream stream { bytes.span() };
    Queue<IPC::Attachment> attachments;
    IPC::Decoder decoder { stream, attachments };
    return decoder.decode<ScrollSnapStateSnapshot>();
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
