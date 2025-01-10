/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/CSSStyleSheet.h>
#include <LibWeb/CSS/StyleComputer.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/DOM/StyleScope.h>

namespace Web::DOM {

void StyleScope::visit_edges(GC::Cell::Visitor& visitor)
{
    visitor.visit(dom_node());
}

void StyleScope::notify_about_added_stylesheet(CSS::CSSStyleSheet& sheet)
{
    style_computer().invalidate_rule_cache();
    style_computer().load_fonts_from_sheet(sheet);
    dom_node().invalidate_style(DOM::StyleInvalidationReason::StyleSheetListAddSheet);

    if (sheet.has_host_selectors()) {
        for (auto* ancestor = dom_node().parent_or_shadow_host(); ancestor; ancestor = ancestor->parent_or_shadow_host()) {
            if (ancestor->is_shadow_root() || ancestor->is_document()) {
                auto& style_scope = ancestor->style_scope();
                style_scope.style_computer().invalidate_rule_cache();
                style_scope.style_computer().load_fonts_from_sheet(sheet);
                style_scope.dom_node().invalidate_style(StyleInvalidationReason::StyleSheetListAddSheet);
            }
        }
    }
}

void StyleScope::notify_about_removed_stylesheet(Optional<CSS::CSSStyleSheet&> sheet)
{
    style_computer().invalidate_rule_cache();
    if (sheet.has_value())
        style_computer().unload_fonts_from_sheet(sheet.value());
    dom_node().invalidate_style(StyleInvalidationReason::AdoptedStyleSheetsList);
}

void StyleScope::notify_media_query_changed_match_state(CSS::CSSStyleSheet& sheet)
{
    style_computer().invalidate_rule_cache();
    dom_node().invalidate_style(StyleInvalidationReason::MediaQueryChangedMatchState);

    if (sheet.has_host_selectors()) {
        if (auto* ancestor = dom_node().parent_or_shadow_host()) {
            if (!ancestor->is_connected())
                return;
            auto& style_scope = ancestor->style_scope();
            style_scope.style_computer().invalidate_rule_cache();
            style_scope.dom_node().invalidate_style(DOM::StyleInvalidationReason::MediaQueryChangedMatchState);
        }
    }
}

}
