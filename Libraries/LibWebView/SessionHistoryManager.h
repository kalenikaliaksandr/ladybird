/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/Vector.h>
#include <LibWebView/UISessionHistoryEntry.h>
#include <LibWebView/UITraversalQueue.h>

namespace WebView {

class SessionHistoryManager {
public:
    SessionHistoryManager() = default;

    Vector<UISessionHistoryEntry>& entries() { return m_entries; }
    Vector<UISessionHistoryEntry> const& entries() const { return m_entries; }

    int current_step() const { return m_current_step; }
    void set_current_step(int step) { m_current_step = step; }

    void append_entry(u64 navigable_id, UISessionHistoryEntry entry);
    void replace_entry(u64 navigable_id, UISessionHistoryEntry entry);

    void clear_forward_history(int from_step);

    int reserve_next_step();
    void commit_step(int step) { m_current_step = step; }

    bool can_go_back() const { return m_current_step > 0; }
    bool can_go_forward() const;

    Vector<int> get_all_used_history_steps() const;
    Optional<int> get_target_step_for_delta(int delta) const;

    u64 allocate_entry_id();
    UISessionHistoryEntry* find_entry_by_id(u64 id);
    u64 allocate_navigable_id();

    UITraversalQueue& traversal_queue() { return m_traversal_queue; }

    struct NavigableInfo {
        u64 parent_id { 0 };
        bool ready_for_navigation { false };
        Optional<u64> current_entry_document_id;
    };

    // Plan data built by UI, to be sent to WebContent for traversal execution.
    struct NavigablePlanData {
        u64 navigable_id { 0 };
        UISessionHistoryEntry target_entry;
        u64 history_length { 0 };
        u64 history_index { 0 };
        Vector<UISessionHistoryEntry> navigation_api_entries;
    };
    struct PlanDataForStep {
        Vector<NavigablePlanData> changing;
        Vector<NavigablePlanData> non_changing;
    };
    PlanDataForStep build_plan_data_for_step(int target_step) const;
    void register_navigable(u64 navigable_id, u64 parent_navigable_id);
    void unregister_navigable(u64 navigable_id);
    void set_navigable_ready_for_navigation(u64 navigable_id);
    bool is_navigable_ready_for_navigation(u64 navigable_id) const;

private:
    Vector<UISessionHistoryEntry> m_entries;
    int m_current_step { 0 };
    int m_pending_step { 0 };
    u64 m_next_entry_id { 1 };
    u64 m_next_navigable_id { 1 };
    HashMap<u64, NavigableInfo> m_navigable_tree;
    UITraversalQueue m_traversal_queue;
};

}
