/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/Painting/AccumulatedVisualContext.h>

using namespace Web::Painting;

static TransformData make_transform(float translation)
{
    auto matrix = Gfx::FloatMatrix4x4::identity();
    matrix[0, 3] = translation;
    matrix[1, 3] = translation;
    return { matrix, { translation, translation } };
}

TEST_CASE(patching_a_node_keeps_its_slot_and_reports_topology_changes)
{
    auto tree = AccumulatedVisualContextTree::create();
    auto parent = tree.append(make_transform(1), VISUAL_VIEWPORT_NODE_INDEX);
    auto child = tree.append(make_transform(2), parent);

    auto value_only = tree.patch_node(child, make_transform(3), parent);
    EXPECT(!value_only.parent_or_depth_changed);
    EXPECT(!value_only.empty_effective_clip_flipped);

    auto reparented = tree.patch_node(child, make_transform(3), VISUAL_VIEWPORT_NODE_INDEX);
    EXPECT(reparented.parent_or_depth_changed);
}

TEST_CASE(patching_derives_the_empty_effective_clip_flag)
{
    auto tree = AccumulatedVisualContextTree::create();
    auto clip = tree.append(ClipData { Web::DevicePixelRect { 0, 0, 1, 1 }, {} }, VISUAL_VIEWPORT_NODE_INDEX);
    EXPECT(!tree.has_empty_effective_clip(clip));

    auto flipped = tree.patch_node(clip, ClipData { Web::DevicePixelRect {}, {} }, VISUAL_VIEWPORT_NODE_INDEX);
    EXPECT(flipped.empty_effective_clip_flipped);
    EXPECT(tree.has_empty_effective_clip(clip));

    auto nested = tree.append(make_transform(1), clip);
    EXPECT(tree.has_empty_effective_clip(nested));
}

TEST_CASE(freed_slots_are_reused_only_after_promotion)
{
    auto tree = AccumulatedVisualContextTree::create();
    auto first = tree.append(make_transform(1), VISUAL_VIEWPORT_NODE_INDEX);
    tree.free_node(first);

    auto quarantined = tree.allocate_node(make_transform(2), VISUAL_VIEWPORT_NODE_INDEX);
    EXPECT_NE(quarantined.value(), first.value());

    tree.promote_quarantined_free_slots();
    auto reused = tree.allocate_node(make_transform(3), VISUAL_VIEWPORT_NODE_INDEX);
    EXPECT_EQ(reused.value(), first.value());
}

TEST_CASE(minting_gives_a_tree_a_fresh_version)
{
    auto tree = AccumulatedVisualContextTree::create();
    auto other_tree = AccumulatedVisualContextTree::create();
    EXPECT_NE(tree.version(), other_tree.version());

    auto version_before_mint = tree.version();
    tree.mint_new_version();
    EXPECT_NE(tree.version(), version_before_mint);
}
