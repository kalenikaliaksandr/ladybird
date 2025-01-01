/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/CSSKeyframesRule.h>
#include <LibWeb/CSS/CSSNestedDeclarations.h>
#include <LibWeb/CSS/CSSStyleSheet.h>
#include <LibWeb/CSS/RuleCache.h>
#include <LibWeb/CSS/SelectorEngine.h>
#include <LibWeb/CSS/StyleValues/BorderRadiusStyleValue.h>
#include <LibWeb/CSS/StyleValues/CSSKeywordValue.h>
#include <LibWeb/CSS/StyleValues/EasingStyleValue.h>
#include <LibWeb/CSS/StyleValues/PositionStyleValue.h>
#include <LibWeb/CSS/StyleValues/ShorthandStyleValue.h>
#include <LibWeb/CSS/StyleValues/StyleValueList.h>
#include <LibWeb/CSS/StyleValues/TimeStyleValue.h>
#include <LibWeb/CSS/StyleValues/TransitionStyleValue.h>
#include <LibWeb/CSS/StyleValues/UnresolvedStyleValue.h>

namespace Web::CSS {

enum class AllowUnresolved {
    Yes,
    No,
};

static void for_each_property_expanding_shorthands(PropertyID property_id, CSSStyleValue const& value, AllowUnresolved allow_unresolved, Function<void(PropertyID, CSSStyleValue const&)> const& set_longhand_property)
{
    auto map_logical_property_to_real_property = [](PropertyID property_id) -> Optional<PropertyID> {
        // FIXME: Honor writing-mode, direction and text-orientation.
        switch (property_id) {
        case PropertyID::MarginBlockStart:
            return PropertyID::MarginTop;
        case PropertyID::MarginBlockEnd:
            return PropertyID::MarginBottom;
        case PropertyID::MarginInlineStart:
            return PropertyID::MarginLeft;
        case PropertyID::MarginInlineEnd:
            return PropertyID::MarginRight;
        case PropertyID::PaddingBlockStart:
            return PropertyID::PaddingTop;
        case PropertyID::PaddingBlockEnd:
            return PropertyID::PaddingBottom;
        case PropertyID::PaddingInlineStart:
            return PropertyID::PaddingLeft;
        case PropertyID::PaddingInlineEnd:
            return PropertyID::PaddingRight;
        case PropertyID::InlineSize:
            return PropertyID::Width;
        case PropertyID::InsetBlockStart:
            return PropertyID::Top;
        case PropertyID::InsetBlockEnd:
            return PropertyID::Bottom;
        case PropertyID::InsetInlineStart:
            return PropertyID::Left;
        case PropertyID::InsetInlineEnd:
            return PropertyID::Right;
        default:
            return {};
        }
    };

    struct StartAndEndPropertyIDs {
        PropertyID start;
        PropertyID end;
    };
    auto map_logical_property_to_real_properties = [](PropertyID property_id) -> Optional<StartAndEndPropertyIDs> {
        // FIXME: Honor writing-mode, direction and text-orientation.
        switch (property_id) {
        case PropertyID::MarginBlock:
            return StartAndEndPropertyIDs { PropertyID::MarginTop, PropertyID::MarginBottom };
        case PropertyID::MarginInline:
            return StartAndEndPropertyIDs { PropertyID::MarginLeft, PropertyID::MarginRight };
        case PropertyID::PaddingBlock:
            return StartAndEndPropertyIDs { PropertyID::PaddingTop, PropertyID::PaddingBottom };
        case PropertyID::PaddingInline:
            return StartAndEndPropertyIDs { PropertyID::PaddingLeft, PropertyID::PaddingRight };
        case PropertyID::InsetBlock:
            return StartAndEndPropertyIDs { PropertyID::Top, PropertyID::Bottom };
        case PropertyID::InsetInline:
            return StartAndEndPropertyIDs { PropertyID::Left, PropertyID::Right };
        default:
            return {};
        }
    };

    if (auto real_property_id = map_logical_property_to_real_property(property_id); real_property_id.has_value()) {
        for_each_property_expanding_shorthands(real_property_id.value(), value, allow_unresolved, set_longhand_property);
        return;
    }

    if (auto real_property_ids = map_logical_property_to_real_properties(property_id); real_property_ids.has_value()) {
        if (value.is_value_list() && value.as_value_list().size() == 2) {
            auto const& start = value.as_value_list().values()[0];
            auto const& end = value.as_value_list().values()[1];
            for_each_property_expanding_shorthands(real_property_ids->start, start, allow_unresolved, set_longhand_property);
            for_each_property_expanding_shorthands(real_property_ids->end, end, allow_unresolved, set_longhand_property);
            return;
        }
        for_each_property_expanding_shorthands(real_property_ids->start, value, allow_unresolved, set_longhand_property);
        for_each_property_expanding_shorthands(real_property_ids->end, value, allow_unresolved, set_longhand_property);
        return;
    }

    if (value.is_shorthand()) {
        auto& shorthand_value = value.as_shorthand();
        auto& properties = shorthand_value.sub_properties();
        auto& values = shorthand_value.values();
        for (size_t i = 0; i < properties.size(); ++i)
            for_each_property_expanding_shorthands(properties[i], values[i], allow_unresolved, set_longhand_property);
        return;
    }

    auto assign_edge_values = [&](PropertyID top_property, PropertyID right_property, PropertyID bottom_property, PropertyID left_property, auto const& values) {
        if (values.size() == 4) {
            set_longhand_property(top_property, values[0]);
            set_longhand_property(right_property, values[1]);
            set_longhand_property(bottom_property, values[2]);
            set_longhand_property(left_property, values[3]);
        } else if (values.size() == 3) {
            set_longhand_property(top_property, values[0]);
            set_longhand_property(right_property, values[1]);
            set_longhand_property(bottom_property, values[2]);
            set_longhand_property(left_property, values[1]);
        } else if (values.size() == 2) {
            set_longhand_property(top_property, values[0]);
            set_longhand_property(right_property, values[1]);
            set_longhand_property(bottom_property, values[0]);
            set_longhand_property(left_property, values[1]);
        } else if (values.size() == 1) {
            set_longhand_property(top_property, values[0]);
            set_longhand_property(right_property, values[0]);
            set_longhand_property(bottom_property, values[0]);
            set_longhand_property(left_property, values[0]);
        }
    };

    if (property_id == CSS::PropertyID::Border) {
        for_each_property_expanding_shorthands(CSS::PropertyID::BorderTop, value, allow_unresolved, set_longhand_property);
        for_each_property_expanding_shorthands(CSS::PropertyID::BorderRight, value, allow_unresolved, set_longhand_property);
        for_each_property_expanding_shorthands(CSS::PropertyID::BorderBottom, value, allow_unresolved, set_longhand_property);
        for_each_property_expanding_shorthands(CSS::PropertyID::BorderLeft, value, allow_unresolved, set_longhand_property);
        // FIXME: Also reset border-image, in line with the spec: https://www.w3.org/TR/css-backgrounds-3/#border-shorthands
        return;
    }

    if (property_id == CSS::PropertyID::BorderStyle) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::BorderTopStyle, PropertyID::BorderRightStyle, PropertyID::BorderBottomStyle, PropertyID::BorderLeftStyle, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::BorderTopStyle, value);
        set_longhand_property(CSS::PropertyID::BorderRightStyle, value);
        set_longhand_property(CSS::PropertyID::BorderBottomStyle, value);
        set_longhand_property(CSS::PropertyID::BorderLeftStyle, value);
        return;
    }

    if (property_id == CSS::PropertyID::BorderWidth) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::BorderTopWidth, PropertyID::BorderRightWidth, PropertyID::BorderBottomWidth, PropertyID::BorderLeftWidth, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::BorderTopWidth, value);
        set_longhand_property(CSS::PropertyID::BorderRightWidth, value);
        set_longhand_property(CSS::PropertyID::BorderBottomWidth, value);
        set_longhand_property(CSS::PropertyID::BorderLeftWidth, value);
        return;
    }

    if (property_id == CSS::PropertyID::BorderColor) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::BorderTopColor, PropertyID::BorderRightColor, PropertyID::BorderBottomColor, PropertyID::BorderLeftColor, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::BorderTopColor, value);
        set_longhand_property(CSS::PropertyID::BorderRightColor, value);
        set_longhand_property(CSS::PropertyID::BorderBottomColor, value);
        set_longhand_property(CSS::PropertyID::BorderLeftColor, value);
        return;
    }

    if (property_id == CSS::PropertyID::BackgroundPosition) {
        if (value.is_position()) {
            auto const& position = value.as_position();
            set_longhand_property(CSS::PropertyID::BackgroundPositionX, position.edge_x());
            set_longhand_property(CSS::PropertyID::BackgroundPositionY, position.edge_y());
        } else if (value.is_value_list()) {
            // Expand background-position layer list into separate lists for x and y positions:
            auto const& values_list = value.as_value_list();
            StyleValueVector x_positions {};
            StyleValueVector y_positions {};
            x_positions.ensure_capacity(values_list.size());
            y_positions.ensure_capacity(values_list.size());
            for (auto& layer : values_list.values()) {
                if (layer->is_position()) {
                    auto const& position = layer->as_position();
                    x_positions.unchecked_append(position.edge_x());
                    y_positions.unchecked_append(position.edge_y());
                } else {
                    x_positions.unchecked_append(layer);
                    y_positions.unchecked_append(layer);
                }
            }
            set_longhand_property(CSS::PropertyID::BackgroundPositionX, StyleValueList::create(move(x_positions), values_list.separator()));
            set_longhand_property(CSS::PropertyID::BackgroundPositionY, StyleValueList::create(move(y_positions), values_list.separator()));
        } else {
            set_longhand_property(CSS::PropertyID::BackgroundPositionX, value);
            set_longhand_property(CSS::PropertyID::BackgroundPositionY, value);
        }

        return;
    }

    if (property_id == CSS::PropertyID::Inset) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::Top, PropertyID::Right, PropertyID::Bottom, PropertyID::Left, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::Top, value);
        set_longhand_property(CSS::PropertyID::Right, value);
        set_longhand_property(CSS::PropertyID::Bottom, value);
        set_longhand_property(CSS::PropertyID::Left, value);
        return;
    }

    if (property_id == CSS::PropertyID::Margin) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::MarginTop, PropertyID::MarginRight, PropertyID::MarginBottom, PropertyID::MarginLeft, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::MarginTop, value);
        set_longhand_property(CSS::PropertyID::MarginRight, value);
        set_longhand_property(CSS::PropertyID::MarginBottom, value);
        set_longhand_property(CSS::PropertyID::MarginLeft, value);
        return;
    }

    if (property_id == CSS::PropertyID::Padding) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            assign_edge_values(PropertyID::PaddingTop, PropertyID::PaddingRight, PropertyID::PaddingBottom, PropertyID::PaddingLeft, values_list.values());
            return;
        }

        set_longhand_property(CSS::PropertyID::PaddingTop, value);
        set_longhand_property(CSS::PropertyID::PaddingRight, value);
        set_longhand_property(CSS::PropertyID::PaddingBottom, value);
        set_longhand_property(CSS::PropertyID::PaddingLeft, value);
        return;
    }

    if (property_id == CSS::PropertyID::Gap) {
        if (value.is_value_list()) {
            auto const& values_list = value.as_value_list();
            set_longhand_property(CSS::PropertyID::RowGap, values_list.values()[0]);
            set_longhand_property(CSS::PropertyID::ColumnGap, values_list.values()[1]);
            return;
        }
        set_longhand_property(CSS::PropertyID::RowGap, value);
        set_longhand_property(CSS::PropertyID::ColumnGap, value);
        return;
    }

    if (property_id == CSS::PropertyID::MaxInlineSize || property_id == CSS::PropertyID::MinInlineSize) {
        // FIXME: Use writing-mode to determine if we should set width or height.
        bool is_horizontal = true;

        if (is_horizontal) {
            if (property_id == CSS::PropertyID::MaxInlineSize) {
                set_longhand_property(CSS::PropertyID::MaxWidth, value);
            } else {
                set_longhand_property(CSS::PropertyID::MinWidth, value);
            }
        } else {
            if (property_id == CSS::PropertyID::MaxInlineSize) {
                set_longhand_property(CSS::PropertyID::MaxHeight, value);
            } else {
                set_longhand_property(CSS::PropertyID::MinHeight, value);
            }
        }
        return;
    }

    if (property_id == CSS::PropertyID::Transition) {
        if (!value.is_transition()) {
            // Handle `none` as a shorthand for `all 0s ease 0s`.
            set_longhand_property(CSS::PropertyID::TransitionProperty, CSSKeywordValue::create(Keyword::All));
            set_longhand_property(CSS::PropertyID::TransitionDuration, TimeStyleValue::create(CSS::Time::make_seconds(0)));
            set_longhand_property(CSS::PropertyID::TransitionDelay, TimeStyleValue::create(CSS::Time::make_seconds(0)));
            set_longhand_property(CSS::PropertyID::TransitionTimingFunction, CSSKeywordValue::create(Keyword::Ease));
            return;
        }
        auto const& transitions = value.as_transition().transitions();
        Array<Vector<ValueComparingNonnullRefPtr<CSSStyleValue const>>, 4> transition_values;
        for (auto const& transition : transitions) {
            transition_values[0].append(*transition.property_name);
            transition_values[1].append(transition.duration.as_style_value());
            transition_values[2].append(transition.delay.as_style_value());
            if (transition.easing)
                transition_values[3].append(*transition.easing);
        }

        set_longhand_property(CSS::PropertyID::TransitionProperty, StyleValueList::create(move(transition_values[0]), StyleValueList::Separator::Comma));
        set_longhand_property(CSS::PropertyID::TransitionDuration, StyleValueList::create(move(transition_values[1]), StyleValueList::Separator::Comma));
        set_longhand_property(CSS::PropertyID::TransitionDelay, StyleValueList::create(move(transition_values[2]), StyleValueList::Separator::Comma));
        set_longhand_property(CSS::PropertyID::TransitionTimingFunction, StyleValueList::create(move(transition_values[3]), StyleValueList::Separator::Comma));
        return;
    }

    if (property_id == CSS::PropertyID::Float) {
        auto keyword = value.to_keyword();

        // FIXME: Honor writing-mode, direction and text-orientation.
        if (keyword == Keyword::InlineStart) {
            set_longhand_property(CSS::PropertyID::Float, CSSKeywordValue::create(Keyword::Left));
            return;
        } else if (keyword == Keyword::InlineEnd) {
            set_longhand_property(CSS::PropertyID::Float, CSSKeywordValue::create(Keyword::Right));
            return;
        }
    }

    if (property_is_shorthand(property_id)) {
        // ShorthandStyleValue was handled already.
        // That means if we got here, that `value` must be a CSS-wide keyword, which we should apply to our longhand properties.
        // We don't directly call `set_longhand_property()` because the longhands might have longhands of their own.
        // (eg `grid` -> `grid-template` -> `grid-template-areas` & `grid-template-rows` & `grid-template-columns`)
        // Forget this requirement if we're ignoring unresolved values and the value is unresolved.
        VERIFY(value.is_css_wide_keyword() || (allow_unresolved == AllowUnresolved::Yes && value.is_unresolved()));
        for (auto longhand : longhands_for_shorthand(property_id))
            for_each_property_expanding_shorthands(longhand, value, allow_unresolved, set_longhand_property);
        return;
    }

    set_longhand_property(property_id, value);
}

NonnullOwnPtr<RuleCache> RuleCache::create(CascadeOrigin cascade_origin)
{
    return make<RuleCache>(cascade_origin);
}

struct SimplifiedSelectorForBucketing {
    Selector::SimpleSelector::Type type;
    FlyString name;
};

static Optional<SimplifiedSelectorForBucketing> is_roundabout_selector_bucketable_as_something_simpler(CSS::Selector::SimpleSelector const& simple_selector)
{
    if (simple_selector.type != Selector::SimpleSelector::Type::PseudoClass)
        return {};

    if (simple_selector.pseudo_class().type != PseudoClass::Is
        && simple_selector.pseudo_class().type != PseudoClass::Where)
        return {};

    if (simple_selector.pseudo_class().argument_selector_list.size() != 1)
        return {};

    auto const& argument_selector = *simple_selector.pseudo_class().argument_selector_list.first();

    auto const& compound_selector = argument_selector.compound_selectors().last();
    if (compound_selector.simple_selectors.size() != 1)
        return {};

    auto const& inner_simple_selector = compound_selector.simple_selectors.first();
    if (inner_simple_selector.type == Selector::SimpleSelector::Type::Class
        || inner_simple_selector.type == Selector::SimpleSelector::Type::Id) {
        return SimplifiedSelectorForBucketing { inner_simple_selector.type, inner_simple_selector.name() };
    }

    if (inner_simple_selector.type == Selector::SimpleSelector::Type::TagName) {
        return SimplifiedSelectorForBucketing { inner_simple_selector.type, inner_simple_selector.qualified_name().name.lowercase_name };
    }

    return {};
}

void RuleCache::collect_selector_insights(Selector const& selector, SelectorInsights& insights)
{
    for (auto const& compound_selector : selector.compound_selectors()) {
        for (auto const& simple_selector : compound_selector.simple_selectors) {
            if (simple_selector.type == Selector::SimpleSelector::Type::Attribute) {
                insights.all_names_used_in_attribute_selectors.set(simple_selector.attribute().qualified_name.name.lowercase_name);
            }
            if (simple_selector.type == Selector::SimpleSelector::Type::PseudoClass) {
                if (simple_selector.pseudo_class().type == PseudoClass::Has) {
                    insights.has_has_selectors = true;
                }
                if (simple_selector.pseudo_class().type == PseudoClass::Defined) {
                    insights.has_defined_selectors = true;
                }
                for (auto const& argument_selector : simple_selector.pseudo_class().argument_selector_list) {
                    collect_selector_insights(*argument_selector, insights);
                }
            }
        }
    }
}

void RuleCache::add_rules_from_stylesheet(CSSStyleSheet& sheet, GC::Ptr<DOM::ShadowRoot> shadow_root, SelectorInsights& insights)
{
    size_t rule_index = 0;
    sheet.for_each_effective_style_producing_rule([&](auto const& rule) {
        size_t selector_index = 0;
        SelectorList const& absolutized_selectors = [&]() {
            if (rule.type() == CSSRule::Type::Style)
                return static_cast<CSSStyleRule const&>(rule).absolutized_selectors();
            if (rule.type() == CSSRule::Type::NestedDeclarations)
                return static_cast<CSSNestedDeclarations const&>(rule).parent_style_rule().absolutized_selectors();
            VERIFY_NOT_REACHED();
        }();
        for (CSS::Selector const& selector : absolutized_selectors) {
            MatchingRule matching_rule {
                shadow_root,
                &rule,
                sheet,
                m_style_sheet_index,
                rule_index,
                selector_index,
                selector.specificity(),
                m_cascade_origin,
                false,
                SelectorEngine::can_use_fast_matches(selector),
                false,
            };

            bool contains_root_pseudo_class = false;
            Optional<Selector::PseudoElement::Type> pseudo_element;

            collect_selector_insights(selector, insights);

            for (auto const& simple_selector : selector.compound_selectors().last().simple_selectors) {
                if (!matching_rule.contains_pseudo_element) {
                    if (simple_selector.type == CSS::Selector::SimpleSelector::Type::PseudoElement) {
                        matching_rule.contains_pseudo_element = true;
                        pseudo_element = simple_selector.pseudo_element().type();
                        ++m_num_pseudo_element_rules;
                    }
                }
                if (!contains_root_pseudo_class) {
                    if (simple_selector.type == CSS::Selector::SimpleSelector::Type::PseudoClass
                        && simple_selector.pseudo_class().type == CSS::PseudoClass::Root) {
                        contains_root_pseudo_class = true;
                        ++m_num_root_rules;
                    }
                }

                if (!matching_rule.must_be_hovered) {
                    if (simple_selector.type == Selector::SimpleSelector::Type::PseudoClass && simple_selector.pseudo_class().type == CSS::PseudoClass::Hover) {
                        matching_rule.must_be_hovered = true;
                        ++m_num_hover_rules;
                    }
                    if (simple_selector.type == Selector::SimpleSelector::Type::PseudoClass
                        && (simple_selector.pseudo_class().type == CSS::PseudoClass::Is
                            || simple_selector.pseudo_class().type == CSS::PseudoClass::Where)) {
                        auto const& argument_selectors = simple_selector.pseudo_class().argument_selector_list;

                        if (argument_selectors.size() == 1) {
                            auto const& simple_argument_selector = argument_selectors.first()->compound_selectors().last().simple_selectors.last();
                            if (simple_argument_selector.type == CSS::Selector::SimpleSelector::Type::PseudoClass
                                && simple_argument_selector.pseudo_class().type == CSS::PseudoClass::Hover) {
                                matching_rule.must_be_hovered = true;
                                ++m_num_hover_rules;
                            }
                        }
                    }
                }
            }

            // NOTE: We traverse the simple selectors in reverse order to make sure that class/ID buckets are preferred over tag buckets
            //       in the common case of div.foo or div#foo selectors.
            bool added_to_bucket = false;

            auto add_to_id_bucket = [&](FlyString const& name) {
                m_rules_by_id.ensure(name).append(move(matching_rule));
                ++m_num_id_rules;
                added_to_bucket = true;
            };

            auto add_to_class_bucket = [&](FlyString const& name) {
                m_rules_by_class.ensure(name).append(move(matching_rule));
                ++m_num_class_rules;
                added_to_bucket = true;
            };

            auto add_to_tag_name_bucket = [&](FlyString const& name) {
                m_rules_by_tag_name.ensure(name).append(move(matching_rule));
                ++m_num_tag_name_rules;
                added_to_bucket = true;
            };

            for (auto const& simple_selector : selector.compound_selectors().last().simple_selectors.in_reverse()) {
                if (simple_selector.type == CSS::Selector::SimpleSelector::Type::Id) {
                    add_to_id_bucket(simple_selector.name());
                    break;
                }
                if (simple_selector.type == CSS::Selector::SimpleSelector::Type::Class) {
                    add_to_class_bucket(simple_selector.name());
                    break;
                }
                if (simple_selector.type == CSS::Selector::SimpleSelector::Type::TagName) {
                    add_to_tag_name_bucket(simple_selector.qualified_name().name.lowercase_name);
                    break;
                }
                // NOTE: Selectors like `:is/where(.foo)` and `:is/where(.foo .bar)` are bucketed as class selectors for `foo` and `bar` respectively.
                if (auto simplified = is_roundabout_selector_bucketable_as_something_simpler(simple_selector); simplified.has_value()) {
                    if (simplified->type == CSS::Selector::SimpleSelector::Type::TagName) {
                        add_to_tag_name_bucket(simplified->name);
                        break;
                    }
                    if (simplified->type == CSS::Selector::SimpleSelector::Type::Class) {
                        add_to_class_bucket(simplified->name);
                        break;
                    }
                    if (simplified->type == CSS::Selector::SimpleSelector::Type::Id) {
                        add_to_id_bucket(simplified->name);
                        break;
                    }
                }
            }
            if (!added_to_bucket) {
                if (matching_rule.contains_pseudo_element) {
                    if (Selector::PseudoElement::is_known_pseudo_element_type(pseudo_element.value())) {
                        m_rules_by_pseudo_element[to_underlying(pseudo_element.value())].append(move(matching_rule));
                    } else {
                        // NOTE: We don't cache rules for unknown pseudo-elements. They can't match anything anyway.
                    }
                } else if (contains_root_pseudo_class) {
                    m_root_rules.append(move(matching_rule));
                } else {
                    for (auto const& simple_selector : selector.compound_selectors().last().simple_selectors) {
                        if (simple_selector.type == CSS::Selector::SimpleSelector::Type::Attribute) {
                            m_rules_by_attribute_name.ensure(simple_selector.attribute().qualified_name.name.lowercase_name).append(move(matching_rule));
                            ++m_num_attribute_rules;
                            added_to_bucket = true;
                            break;
                        }
                    }
                    if (!added_to_bucket) {
                        m_other_rules.append(move(matching_rule));
                    }
                }
            }

            ++selector_index;
        }
        ++rule_index;
    });

    // Loosely based on https://drafts.csswg.org/css-animations-2/#keyframe-processing
    sheet.for_each_effective_keyframes_at_rule([&](CSSKeyframesRule const& rule) {
        auto keyframe_set = adopt_ref(*new Animations::KeyframeEffect::KeyFrameSet);
        HashTable<PropertyID> animated_properties;

        // Forwards pass, resolve all the user-specified keyframe properties.
        for (auto const& keyframe_rule : *rule.css_rules()) {
            auto const& keyframe = verify_cast<CSSKeyframeRule>(*keyframe_rule);
            Animations::KeyframeEffect::KeyFrameSet::ResolvedKeyFrame resolved_keyframe;

            auto key = static_cast<u64>(keyframe.key().value() * Animations::KeyframeEffect::AnimationKeyFrameKeyScaleFactor);
            auto const& keyframe_style = *keyframe.style_as_property_owning_style_declaration();
            for (auto const& it : keyframe_style.properties()) {
                // Unresolved properties will be resolved in collect_animation_into()
                for_each_property_expanding_shorthands(it.property_id, it.value, AllowUnresolved::Yes, [&](PropertyID shorthand_id, CSSStyleValue const& shorthand_value) {
                    animated_properties.set(shorthand_id);
                    resolved_keyframe.properties.set(shorthand_id, NonnullRefPtr<CSSStyleValue const> { shorthand_value });
                });
            }

            keyframe_set->keyframes_by_key.insert(key, resolved_keyframe);
        }

        Animations::KeyframeEffect::generate_initial_and_final_frames(keyframe_set, animated_properties);

        if constexpr (LIBWEB_CSS_DEBUG) {
            dbgln("Resolved keyframe set '{}' into {} keyframes:", rule.name(), keyframe_set->keyframes_by_key.size());
            for (auto it = keyframe_set->keyframes_by_key.begin(); it != keyframe_set->keyframes_by_key.end(); ++it)
                dbgln("    - keyframe {}: {} properties", it.key(), it->properties.size());
        }

        rules_by_animation_keyframes.set(rule.name(), move(keyframe_set));
    });

    ++m_style_sheet_index;
}

Vector<MatchingRule> RuleCache::get_by_id(FlyString id) const
{
    if (auto rules = m_rules_by_id.get(id); rules.has_value()) {
        return rules.value();
    }
    return {};
}

Vector<MatchingRule> RuleCache::get_by_class_name(FlyString class_name) const
{
    if (auto rules = m_rules_by_class.get(class_name); rules.has_value()) {
        return rules.value();
    }
    return {};
}

Vector<MatchingRule> RuleCache::get_by_tag_name(FlyString tag_name) const
{
    if (auto rules = m_rules_by_tag_name.get(tag_name); rules.has_value()) {
        return rules.value();
    }
    return {};
}

Vector<MatchingRule> RuleCache::get_by_attribute(FlyString attribute_name) const
{
    if (auto rules = m_rules_by_attribute_name.get(attribute_name); rules.has_value()) {
        return rules.value();
    }
    return {};
}

Vector<MatchingRule> RuleCache::get_by_pseudo_element(Selector::PseudoElement::Type type) const
{
    return m_rules_by_pseudo_element[to_underlying(type)];
}

Vector<MatchingRule> RuleCache::get_root_rules() const
{
    return m_root_rules;
}

Vector<MatchingRule> RuleCache::get_other_rules() const
{
    return m_other_rules;
}

}
