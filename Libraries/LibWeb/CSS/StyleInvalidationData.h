/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/NonnullRefPtr.h>
#include <AK/RefCounted.h>
#include <AK/Vector.h>
#include <LibWeb/CSS/InvalidationSet.h>
#include <LibWeb/CSS/Selector.h>
#include <LibWeb/Forward.h>

namespace Web::CSS {

enum class ExcludePropertiesNestedInNotPseudoClass : bool {
    No,
    Yes,
};

enum class InsideNthChildPseudoClass {
    No,
    Yes,
};

struct InvalidationPlan;

struct DescendantInvalidationRule {
    InvalidationSet match_set;
    bool match_any { false };
    NonnullRefPtr<InvalidationPlan> payload;
};

enum class SiblingInvalidationReach {
    Adjacent,
    Subsequent,
};

struct SiblingInvalidationRule {
    SiblingInvalidationReach reach;
    InvalidationSet match_set;
    bool match_any { false };
    NonnullRefPtr<InvalidationPlan> payload;
};

struct InvalidationPlan final : RefCounted<InvalidationPlan> {
    static NonnullRefPtr<InvalidationPlan> create() { return adopt_ref(*new InvalidationPlan); }

    bool is_empty() const;
    void include_all_from(InvalidationPlan const&);

    bool invalidate_self { false };
    bool invalidate_whole_subtree { false };
    Vector<DescendantInvalidationRule> descendant_rules;
    Vector<SiblingInvalidationRule> sibling_rules;
};

enum class HasAncestorTraversal : u8 {
    None,
    Parent,
    Ancestors,
};

enum class HasSiblingTraversal : u8 {
    None,
    PrevSibling,
    EarlierSiblings,
};

struct HasInvalidationTraversal {
    HasAncestorTraversal ancestor { HasAncestorTraversal::None };
    HasSiblingTraversal sibling { HasSiblingTraversal::None };

    void widen(HasInvalidationTraversal const& other)
    {
        if (to_underlying(other.ancestor) > to_underlying(ancestor))
            ancestor = other.ancestor;
        if (to_underlying(other.sibling) > to_underlying(sibling))
            sibling = other.sibling;
    }
};

struct StyleInvalidationData;

void build_invalidation_sets_for_simple_selector(Selector::SimpleSelector const&, InvalidationSet&, ExcludePropertiesNestedInNotPseudoClass, StyleInvalidationData&, InsideNthChildPseudoClass);

struct StyleInvalidationData {
    HashMap<InvalidationSet::Property, NonnullRefPtr<InvalidationPlan>> invalidation_plans;
    HashMap<InvalidationSet::Property, HasInvalidationTraversal> has_invalidation_map;

    void build_invalidation_sets_for_selector(Selector const& selector);
};

}
