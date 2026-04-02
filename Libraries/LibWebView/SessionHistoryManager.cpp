/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWebView/SessionHistoryManager.h>

namespace WebView {

static void clear_forward_history_in_entries(Vector<UISessionHistoryEntry>& entries, int from_step)
{
    entries.remove_all_matching([from_step](auto& entry) {
        return entry.step.has_value() && entry.step.value() > from_step;
    });

    for (auto& entry : entries) {
        for (auto& nested_history : entry.document_state.nested_histories)
            clear_forward_history_in_entries(nested_history.entries, from_step);
    }
}

static UISessionHistoryEntry* find_entry_by_id_in_entries(Vector<UISessionHistoryEntry>& entries, u64 id)
{
    for (auto& entry : entries) {
        if (entry.id == id)
            return &entry;
        for (auto& nested_history : entry.document_state.nested_histories) {
            if (auto* nested_entry = find_entry_by_id_in_entries(nested_history.entries, id))
                return nested_entry;
        }
    }
    return nullptr;
}

static UISessionHistoryEntry* find_active_entry_for_step(Vector<UISessionHistoryEntry>& entries, int step)
{
    UISessionHistoryEntry* active_entry = nullptr;
    for (auto& entry : entries) {
        if (entry.step.has_value() && entry.step.value() <= step)
            active_entry = &entry;
    }
    return active_entry;
}

static Vector<UISessionHistoryEntry>* entry_list_for_navigable(Vector<UISessionHistoryEntry>& top_level_entries, HashMap<u64, SessionHistoryManager::NavigableInfo> const& navigable_tree, u64 navigable_id, int step, bool create_missing_nested_history)
{
    auto info = navigable_tree.get(navigable_id);
    if (!info.has_value())
        return nullptr;
    if (info->parent_id == 0)
        return &top_level_entries;

    Vector<u64> descendant_path;
    descendant_path.append(navigable_id);

    auto current_parent_id = info->parent_id;
    while (current_parent_id != 0) {
        auto parent_info = navigable_tree.get(current_parent_id);
        if (!parent_info.has_value())
            return nullptr;
        if (parent_info->parent_id != 0)
            descendant_path.prepend(current_parent_id);
        current_parent_id = parent_info->parent_id;
    }

    auto* current_entries = &top_level_entries;
    for (auto descendant_navigable_id : descendant_path) {
        auto* active_entry = find_active_entry_for_step(*current_entries, step);
        if (!active_entry)
            return nullptr;

        if (auto index = active_entry->document_state.nested_histories.find_first_index_if([descendant_navigable_id](auto const& nested_history) {
                return nested_history.navigable_id == descendant_navigable_id;
            });
            index.has_value()) {
            current_entries = &active_entry->document_state.nested_histories[*index].entries;
            continue;
        }

        if (!create_missing_nested_history)
            return nullptr;

        active_entry->document_state.nested_histories.append({
            .navigable_id = descendant_navigable_id,
            .entries = {},
        });
        current_entries = &active_entry->document_state.nested_histories.last().entries;
    }

    return current_entries;
}

void SessionHistoryManager::append_entry(u64 navigable_id, UISessionHistoryEntry entry)
{
    if (auto* entries = entry_list_for_navigable(m_entries, m_navigable_tree, navigable_id, m_current_step, true))
        entries->append(move(entry));
}

void SessionHistoryManager::replace_entry(u64 navigable_id, UISessionHistoryEntry entry)
{
    auto* entries = entry_list_for_navigable(m_entries, m_navigable_tree, navigable_id, m_current_step, true);
    if (!entries)
        return;

    if (!entry.step.has_value()) {
        entries->append(move(entry));
        return;
    }

    auto step = entry.step.value();
    for (auto& existing : *entries) {
        if (existing.step.has_value() && existing.step.value() == step) {
            existing = move(entry);
            return;
        }
    }

    entries->append(move(entry));
}

void SessionHistoryManager::clear_forward_history(int from_step)
{
    clear_forward_history_in_entries(m_entries, from_step);
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
    return find_entry_by_id_in_entries(m_entries, id);
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

static void append_plan_data_for_nested_histories(SessionHistoryManager::PlanDataForStep& plan, Vector<UINestedHistory> const& nested_histories, int target_step, u64 history_length, u64 history_index)
{
    for (auto const& nested_history : nested_histories) {
        auto const* child_target = get_target_entry_at_step(nested_history.entries, target_step);
        if (child_target) {
            auto child_nav_api = get_navigation_api_entries(nested_history.entries, target_step);
            plan.changing.append({
                .navigable_id = nested_history.navigable_id,
                .target_entry = *child_target,
                .history_length = history_length,
                .history_index = history_index,
                .navigation_api_entries = move(child_nav_api),
            });
            append_plan_data_for_nested_histories(plan, child_target->document_state.nested_histories, target_step, history_length, history_index);
        } else {
            plan.non_changing.append({
                .navigable_id = nested_history.navigable_id,
                .target_entry = {},
                .history_length = history_length,
                .history_index = history_index,
            });
        }
    }
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

    append_plan_data_for_nested_histories(plan, target_entry->document_state.nested_histories, target_step, history_length, history_index);

    return plan;
}

}
