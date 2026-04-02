/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWebView/SessionHistoryManager.h>

namespace WebView {

void SessionHistoryManager::replace_entry(UISessionHistoryEntry entry)
{
    if (!entry.step.has_value())
        return;
    auto step = entry.step.value();
    for (auto& existing : m_entries) {
        if (existing.step.has_value() && existing.step.value() == step) {
            existing = move(entry);
            return;
        }
    }
}

void SessionHistoryManager::clear_forward_history(int from_step)
{
    m_entries.remove_all_matching([from_step](auto& entry) {
        return entry.step.has_value() && entry.step.value() > from_step;
    });
}

bool SessionHistoryManager::can_go_forward() const
{
    Vector<Vector<UISessionHistoryEntry> const*> entry_lists;
    entry_lists.append(&m_entries);
    while (!entry_lists.is_empty()) {
        auto const* entry_list = entry_lists.take_first();
        for (auto const& entry : *entry_list) {
            if (entry.step.has_value() && entry.step.value() > m_current_step)
                return true;
            for (auto const& nested : entry.document_state.nested_histories)
                entry_lists.append(&nested.entries);
        }
    }
    return false;
}

Vector<int> SessionHistoryManager::get_all_used_history_steps() const
{
    return WebView::get_all_used_history_steps(m_entries);
}

Optional<int> SessionHistoryManager::get_target_step_for_delta(int delta) const
{
    auto all_steps = get_all_used_history_steps();
    auto current_step_index = all_steps.find_first_index(m_current_step);
    if (!current_step_index.has_value())
        return {};

    auto target_step_index = static_cast<int>(*current_step_index) + delta;
    if (target_step_index < 0 || target_step_index >= static_cast<int>(all_steps.size()))
        return {};

    return all_steps[target_step_index];
}

int SessionHistoryManager::reserve_next_step()
{
    m_pending_step = m_current_step + 1;
    return m_pending_step;
}

u64 SessionHistoryManager::allocate_entry_id()
{
    return m_next_entry_id++;
}

UISessionHistoryEntry* SessionHistoryManager::find_entry_by_id(u64 id)
{
    for (auto& entry : m_entries) {
        if (entry.id == id)
            return &entry;
    }
    return nullptr;
}

u64 SessionHistoryManager::allocate_navigable_id()
{
    return m_next_navigable_id++;
}

void SessionHistoryManager::register_navigable(u64 navigable_id, u64 parent_navigable_id)
{
    m_navigable_tree.set(navigable_id, NavigableInfo { .parent_id = parent_navigable_id });
}

void SessionHistoryManager::unregister_navigable(u64 navigable_id)
{
    m_navigable_tree.remove(navigable_id);
}

void SessionHistoryManager::set_navigable_ready_for_navigation(u64 navigable_id)
{
    if (auto it = m_navigable_tree.find(navigable_id); it != m_navigable_tree.end())
        it->value.ready_for_navigation = true;
}

bool SessionHistoryManager::is_navigable_ready_for_navigation(u64 navigable_id) const
{
    if (auto it = m_navigable_tree.find(navigable_id); it != m_navigable_tree.end())
        return it->value.ready_for_navigation;
    return false;
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
static void compute_history_length_and_index(Vector<UISessionHistoryEntry> const& entries, int target_step, u64& out_length, u64& out_index)
{
    // Count top-level entries with assigned steps.
    u64 length = 0;
    u64 index = 0;
    for (auto const& entry : entries) {
        if (entry.step.has_value()) {
            if (entry.step.value() <= target_step)
                index = length;
            length++;
        }
    }
    out_length = length;
    out_index = index;
}

// Get target entry for the top-level navigable at a given step.
static UISessionHistoryEntry const* get_target_entry_at_step(Vector<UISessionHistoryEntry> const& entries, int target_step)
{
    UISessionHistoryEntry const* best = nullptr;
    for (auto const& entry : entries) {
        if (entry.step.has_value() && entry.step.value() <= target_step)
            best = &entry;
    }
    return best;
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#getting-session-history-entries-for-the-navigation-api
static Vector<UISessionHistoryEntry> get_navigation_api_entries(Vector<UISessionHistoryEntry> const& raw_entries, int target_step)
{
    if (raw_entries.is_empty())
        return {};

    // Find starting index: entry with greatest step <= target_step
    int starting_index = 0;
    int max_step = -1;
    for (int i = 0; i < static_cast<int>(raw_entries.size()); ++i) {
        auto const& entry = raw_entries[i];
        if (entry.step.has_value()) {
            auto step = entry.step.value();
            if (step <= target_step && step > max_step) {
                starting_index = i;
                max_step = step;
            }
        }
    }

    Vector<UISessionHistoryEntry> result;
    result.append(raw_entries[starting_index]);

    auto const& starting_origin = raw_entries[starting_index].document_state.origin;

    // Walk backward from starting_index
    for (int i = starting_index - 1; i >= 0; --i) {
        auto const& entry_origin = raw_entries[i].document_state.origin;
        if (starting_origin.has_value() && entry_origin.has_value() && !entry_origin->is_same_origin(*starting_origin))
            break;
        result.prepend(raw_entries[i]);
    }

    // Walk forward from starting_index
    for (int i = starting_index + 1; i < static_cast<int>(raw_entries.size()); ++i) {
        auto const& entry_origin = raw_entries[i].document_state.origin;
        if (starting_origin.has_value() && entry_origin.has_value() && !entry_origin->is_same_origin(*starting_origin))
            break;
        result.append(raw_entries[i]);
    }

    return result;
}

SessionHistoryManager::PlanDataForStep SessionHistoryManager::build_plan_data_for_step(int target_step) const
{
    PlanDataForStep plan;

    u64 history_length = 0;
    u64 history_index = 0;
    compute_history_length_and_index(m_entries, target_step, history_length, history_index);

    // For the top-level navigable: determine if it's changing.
    auto const* target_entry = get_target_entry_at_step(m_entries, target_step);
    if (!target_entry)
        return plan;

    auto nav_api_entries = get_navigation_api_entries(m_entries, target_step);

    // Find the top-level navigable ID.
    u64 top_level_nav_id = 0;
    for (auto const& [nav_id, info] : m_navigable_tree) {
        if (info.parent_id == 0) {
            top_level_nav_id = nav_id;
            break;
        }
    }

    if (top_level_nav_id != 0) {
        plan.changing.append({
            .navigable_id = top_level_nav_id,
            .target_entry = *target_entry,
            .history_length = history_length,
            .history_index = history_index,
            .navigation_api_entries = move(nav_api_entries),
        });
    }

    // Child navigables: check nested histories for entries at target_step.
    // For each child navigable tracked by UI, check if it has an entry at target_step
    // in its parent's nested histories.
    for (auto const& nested : target_entry->document_state.nested_histories) {
        // Find the navigable ID for this nested history.
        u64 child_nav_id = 0;
        for (auto const& [nav_id, info] : m_navigable_tree) {
            if (info.parent_id == top_level_nav_id) {
                // FIXME: Match by navigable ID stored in nested history vs ui_navigable_id.
                // For now, use the first child navigable.
                child_nav_id = nav_id;
                break;
            }
        }

        if (child_nav_id == 0)
            continue;

        auto const* child_target = get_target_entry_at_step(nested.entries, target_step);
        if (child_target) {
            auto child_nav_api = get_navigation_api_entries(nested.entries, target_step);
            plan.changing.append({
                .navigable_id = child_nav_id,
                .target_entry = *child_target,
                .history_length = history_length,
                .history_index = history_index,
                .navigation_api_entries = move(child_nav_api),
            });
        } else {
            plan.non_changing.append({
                .navigable_id = child_nav_id,
                .target_entry = {},
                .history_length = history_length,
                .history_index = history_index,
            });
        }
    }

    return plan;
}

}
