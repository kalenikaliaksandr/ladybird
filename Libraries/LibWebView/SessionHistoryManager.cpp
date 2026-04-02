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

}
