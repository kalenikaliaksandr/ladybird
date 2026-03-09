/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ScopeGuard.h>
#include <LibWeb/CSS/StyleInvalidationTracing.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/DOM/ShadowRoot.h>
#include <LibWeb/DOM/StyleInvalidator.h>

namespace Web::DOM {

GC_DEFINE_ALLOCATOR(StyleInvalidator);

static bool element_matches_invalidation_rule(Element const& element, CSS::InvalidationSet const& match_set, bool match_any)
{
    return match_any || element.includes_properties_from_invalidation_set(match_set);
}

void StyleInvalidator::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    for (auto const& it : m_pending_invalidations)
        visitor.visit(it.key);
}

void StyleInvalidator::invalidate(Node& node)
{
    m_tracing_nodes_visited = 0;
    m_tracing_nodes_marked_subtree = 0;
    m_tracing_nodes_matched_rule = 0;

    perform_pending_style_invalidations(node, false);

    if (CSS::g_enable_style_invalidation_tracing) {
        dbgln("[STYLE_INVAL] {{\"type\":\"invalidator_walk\",\"cycle\":{},\"nodes_visited\":{},\"nodes_marked_subtree\":{},\"nodes_matched_rule\":{},\"pending_count\":{}}}",
            CSS::g_style_invalidation_cycle_counter, m_tracing_nodes_visited, m_tracing_nodes_marked_subtree,
            m_tracing_nodes_matched_rule, m_pending_invalidations.size());
        if (auto* stats = CSS::g_current_cycle_stats) {
            stats->nodes_visited_in_invalidator = m_tracing_nodes_visited;
            stats->nodes_marked_subtree = m_tracing_nodes_marked_subtree;
            stats->nodes_matched_rule = m_tracing_nodes_matched_rule;
        }
    }

    m_pending_invalidations.clear();
}

bool StyleInvalidator::enqueue_invalidation_plan(Node& node, StyleInvalidationReason reason, CSS::InvalidationPlan const& plan)
{
    if (plan.is_empty())
        return false;

    if (CSS::g_enable_style_invalidation_tracing) {
        dbgln("[STYLE_INVAL] {{\"type\":\"plan\",\"cycle\":{},\"element\":\"{}\",\"whole_subtree_fallback\":{},\"self\":{},\"desc_rules\":{},\"sib_rules\":{}}}",
            CSS::g_style_invalidation_cycle_counter, node.debug_description(),
            plan.invalidate_whole_subtree, plan.invalidate_self,
            plan.descendant_rules.size(), plan.sibling_rules.size());
    }

    if (plan.invalidate_whole_subtree) {
        node.invalidate_style(reason);
        return true;
    }

    if (plan.invalidate_self)
        node.set_needs_style_update(true);

    add_pending_invalidation(node, reason, plan);

    if (auto* element = as_if<Element>(node)) {
        for (auto const& sibling_rule : plan.sibling_rules)
            apply_sibling_invalidation(*element, reason, sibling_rule);
    }

    return false;
}

void StyleInvalidator::add_pending_invalidation(GC::Ref<Node> node, StyleInvalidationReason reason, CSS::InvalidationPlan const& plan)
{
    if (plan.descendant_rules.is_empty())
        return;

    auto& pending_invalidations = m_pending_invalidations.ensure(node, [] {
        return Vector<PendingDescendantInvalidation> {};
    });
    for (auto const& descendant_rule : plan.descendant_rules)
        pending_invalidations.append({ reason, descendant_rule });
}

void StyleInvalidator::apply_invalidation_plan(Element& element, StyleInvalidationReason reason, CSS::InvalidationPlan const& plan, bool& invalidate_entire_subtree)
{
    if (plan.is_empty())
        return;

    if (plan.invalidate_whole_subtree) {
        element.invalidate_style(reason);
        invalidate_entire_subtree = true;
        element.set_needs_style_update_internal(true);
        if (element.has_child_nodes())
            element.set_child_needs_style_update(true);
        return;
    }

    if (plan.invalidate_self)
        element.set_needs_style_update(true);

    for (auto const& descendant_rule : plan.descendant_rules)
        m_active_descendant_invalidations.append({ reason, descendant_rule });

    for (auto const& sibling_rule : plan.sibling_rules)
        apply_sibling_invalidation(element, reason, sibling_rule);
}

void StyleInvalidator::apply_sibling_invalidation(Element& element, StyleInvalidationReason reason, CSS::SiblingInvalidationRule const& sibling_rule)
{
    auto apply_if_matching = [&](Element* sibling) {
        if (!sibling)
            return;
        if (!element_matches_invalidation_rule(*sibling, sibling_rule.match_set, sibling_rule.match_any))
            return;
        (void)enqueue_invalidation_plan(*sibling, reason, *sibling_rule.payload);
    };

    switch (sibling_rule.reach) {
    case CSS::SiblingInvalidationReach::Adjacent:
        apply_if_matching(element.next_element_sibling());
        break;
    case CSS::SiblingInvalidationReach::Subsequent:
        for (auto* sibling = element.next_element_sibling(); sibling; sibling = sibling->next_element_sibling())
            apply_if_matching(sibling);
        break;
    default:
        VERIFY_NOT_REACHED();
    }
}

// This function makes a full pass over the entire DOM and:
// - converts "entire subtree needs style update" into "needs style update" for each inclusive descendant where it's found.
// - applies descendant invalidation rules to matching elements
void StyleInvalidator::perform_pending_style_invalidations(Node& node, bool invalidate_entire_subtree)
{
    if (CSS::g_enable_style_invalidation_tracing)
        ++m_tracing_nodes_visited;

    bool was_already_subtree = invalidate_entire_subtree;
    invalidate_entire_subtree |= node.entire_subtree_needs_style_update();

    if (invalidate_entire_subtree) {
        if (CSS::g_enable_style_invalidation_tracing && !was_already_subtree && node.entire_subtree_needs_style_update())
            ++m_tracing_nodes_marked_subtree;
        node.set_needs_style_update_internal(true);
        if (node.has_child_nodes())
            node.set_child_needs_style_update(true);
    }

    auto previous_active_descendant_invalidations_size = m_active_descendant_invalidations.size();
    ScopeGuard restore_state = [this, previous_active_descendant_invalidations_size] {
        m_active_descendant_invalidations.shrink(previous_active_descendant_invalidations_size);
    };

    if (!invalidate_entire_subtree) {
        if (auto pending_invalidations = m_pending_invalidations.get(node); pending_invalidations.has_value())
            m_active_descendant_invalidations.extend(*pending_invalidations);

        if (auto* element = as_if<Element>(node)) {
            size_t invalidation_index = 0;
            while (invalidation_index < m_active_descendant_invalidations.size()) {
                auto const& pending_invalidation = m_active_descendant_invalidations[invalidation_index++];
                if (!element_matches_invalidation_rule(*element, pending_invalidation.rule.match_set, pending_invalidation.rule.match_any))
                    continue;

                if (CSS::g_enable_style_invalidation_tracing)
                    ++m_tracing_nodes_matched_rule;

                apply_invalidation_plan(*element, pending_invalidation.reason, *pending_invalidation.rule.payload, invalidate_entire_subtree);
                if (invalidate_entire_subtree)
                    break;
            }

            if (invalidate_entire_subtree) {
                node.set_needs_style_update_internal(true);
                if (node.has_child_nodes())
                    node.set_child_needs_style_update(true);
            }
        }
    }

    for (auto* child = node.first_child(); child; child = child->next_sibling())
        perform_pending_style_invalidations(*child, invalidate_entire_subtree);

    if (node.is_element()) {
        auto& element = static_cast<Element&>(node);
        if (auto shadow_root = element.shadow_root()) {
            perform_pending_style_invalidations(*shadow_root, invalidate_entire_subtree);
            if (invalidate_entire_subtree)
                node.set_child_needs_style_update(true);
        }
    }

    node.set_entire_subtree_needs_style_update(false);
}

}
