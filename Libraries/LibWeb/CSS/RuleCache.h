/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/FlyString.h>
#include <AK/HashMap.h>
#include <LibGC/Ptr.h>
#include <LibWeb/Animations/KeyframeEffect.h>
#include <LibWeb/CSS/CascadeOrigin.h>
#include <LibWeb/CSS/Selector.h>
#include <LibWeb/Forward.h>

namespace Web::CSS {

struct SelectorInsights {
    bool has_has_selectors { false };
    bool has_defined_selectors { false };
    HashTable<FlyString> all_names_used_in_attribute_selectors;
};

struct MatchingRule {
    GC::Ptr<DOM::ShadowRoot const> shadow_root;
    GC::Ptr<CSSRule const> rule; // Either CSSStyleRule or CSSNestedDeclarations
    GC::Ptr<CSSStyleSheet const> sheet;
    size_t style_sheet_index { 0 };
    size_t rule_index { 0 };
    size_t selector_index { 0 };

    u32 specificity { 0 };
    CascadeOrigin cascade_origin;
    bool contains_pseudo_element { false };
    bool can_use_fast_matches { false };
    bool must_be_hovered { false };
    bool skip { false };

    // Helpers to deal with the fact that `rule` might be a CSSStyleRule or a CSSNestedDeclarations
    PropertyOwningCSSStyleDeclaration const& declaration() const;
    SelectorList const& absolutized_selectors() const;
    FlyString const& qualified_layer_name() const;
};

class RuleCache {
public:
    static NonnullOwnPtr<RuleCache> create(CascadeOrigin);

    void add_rules_from_stylesheet(CSSStyleSheet&, GC::Ptr<DOM::ShadowRoot> shadow_root, SelectorInsights& insights);

    HashMap<FlyString, Vector<MatchingRule>> rules_by_id;
    HashMap<FlyString, Vector<MatchingRule>> rules_by_class;
    HashMap<FlyString, Vector<MatchingRule>> rules_by_tag_name;
    HashMap<FlyString, Vector<MatchingRule>, AK::ASCIICaseInsensitiveFlyStringTraits> rules_by_attribute_name;
    Array<Vector<MatchingRule>, to_underlying(Selector::PseudoElement::Type::KnownPseudoElementCount)> rules_by_pseudo_element;
    Vector<MatchingRule> root_rules;
    Vector<MatchingRule> other_rules;

    HashMap<FlyString, NonnullRefPtr<Animations::KeyframeEffect::KeyFrameSet>> rules_by_animation_keyframes;

    RuleCache(CascadeOrigin cascade_origin)
        : m_cascade_origin(cascade_origin)
    {
    }

private:
    static void collect_selector_insights(Selector const& selector, SelectorInsights& insights);

    CascadeOrigin m_cascade_origin;

    size_t m_style_sheet_index { 0 };
    size_t m_num_pseudo_element_rules { 0 };
    size_t m_num_root_rules { 0 };
    size_t m_num_hover_rules { 0 };
    size_t m_num_id_rules { 0 };
    size_t m_num_class_rules { 0 };
    size_t m_num_tag_name_rules { 0 };
    size_t m_num_attribute_rules { 0 };

    // SelectorInsights m_insights;
};

}
