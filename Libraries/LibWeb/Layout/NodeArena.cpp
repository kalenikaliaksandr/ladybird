/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/LayoutRustBridge.h>
#include <LibWeb/Layout/NodeArena.h>

namespace Web::Layout {

NodeArena::NodeArena()
    : m_handle(RustFFI::layout_arena_create())
{
    VERIFY(m_handle);
}

NodeArena::~NodeArena()
{
    RustFFI::layout_arena_destroy(m_handle);
}

RustFFI::NodeAllocation NodeArena::allocate()
{
    auto allocation = RustFFI::layout_arena_allocate(m_handle);
    VERIFY(allocation.data);
    return allocation;
}

void NodeArena::free(RustFFI::NodeSlotId slot, u32 generation)
{
    RustFFI::layout_arena_free(m_handle, slot, generation);
}

u64 NodeArena::formatting_context_run_cache_hit_count() const
{
    return RustFFI::layout_arena_fc_run_cache_hit_count(m_handle);
}

void NodeArena::drop_intrinsic_size_cache(RustFFI::NodeData const& node_data) const
{
    RustFFI::layout_arena_drop_intrinsic_size_cache(m_handle, &node_data);
}

void NodeArena::enroll_node_for_replaced_content_facts_sync(Node const& node)
{
    m_nodes_enrolled_for_replaced_content_facts_sync.append(node.make_weak_ptr<Node>());
}

void NodeArena::sync_enrolled_content_for_layout()
{
    if (layout_pass_currently_running())
        return;
    sync_enrolled_replaced_content_facts();
}

void NodeArena::sync_enrolled_replaced_content_facts()
{
    bool any_enrolled_node_died = false;
    for (auto& weak_node : m_nodes_enrolled_for_replaced_content_facts_sync) {
        auto* node = weak_node.ptr();
        if (!node) {
            any_enrolled_node_died = true;
            continue;
        }
        RustFFI::FfiReplacedContentFacts facts {};
        if (auto const* box = as_if<Box>(*node))
            facts = box->build_replaced_content_facts_for_arena();
        // Changed facts invalidate cached formatting-context runs regardless of which
        // channel produced the change, including sources with no invalidation of their own.
        if (RustFFI::layout_arena_set_replaced_content_facts(m_handle, Node::slot_id(node), facts))
            node->bump_fragment_cache_epoch_of_self_and_ancestors();
    }
    if (any_enrolled_node_died)
        m_nodes_enrolled_for_replaced_content_facts_sync.remove_all_matching([](auto& weak_node) { return !weak_node.ptr(); });
}

}
