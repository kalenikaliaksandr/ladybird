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

void StyleScope::visit_document_or_shadow_root(GC::Cell::Visitor& visitor)
{
    visitor.visit(document_or_shadow_root());
}

void StyleScope::notify_about_added_stylesheet(CSS::CSSStyleSheet& sheet)
{
    style_computer().invalidate_rule_cache();
    style_computer().load_fonts_from_sheet(sheet);
    document_or_shadow_root().invalidate_style(DOM::StyleInvalidationReason::StyleSheetListAddSheet);

    if (sheet.has_host_selectors()) {
        for (auto* ancestor = document_or_shadow_root().parent_or_shadow_host(); ancestor; ancestor = ancestor->parent_or_shadow_host()) {
            if (ancestor->is_shadow_root() || ancestor->is_document()) {
                auto& style_scope = ancestor->style_scope();
                style_scope.style_computer().invalidate_rule_cache();
                style_scope.style_computer().load_fonts_from_sheet(sheet);
                // FIXME: Use correct invalidation reason
                style_scope.document_or_shadow_root().invalidate_style(StyleInvalidationReason::StyleSheetListAddSheet);
                break;
            }
        }
    }
}

void StyleScope::notify_about_removed_stylesheet(CSS::CSSStyleSheet& sheet)
{
    style_computer().invalidate_rule_cache();
    style_computer().unload_fonts_from_sheet(sheet);
    // FIXME: Use correct invalidation reason
    document_or_shadow_root().invalidate_style(StyleInvalidationReason::AdoptedStyleSheetsList);

    if (sheet.has_host_selectors()) {
        for (auto* ancestor = document_or_shadow_root().parent_or_shadow_host(); ancestor; ancestor = ancestor->parent_or_shadow_host()) {
            if (ancestor->is_shadow_root() || ancestor->is_document()) {
                auto& style_scope = ancestor->style_scope();
                style_scope.style_computer().invalidate_rule_cache();
                style_scope.style_computer().unload_fonts_from_sheet(sheet);
                // FIXME: Use correct invalidation reason
                style_scope.document_or_shadow_root().invalidate_style(StyleInvalidationReason::StyleSheetListAddSheet);
                break;
            }
        }
    }
}

void StyleScope::notify_media_query_changed_match_state(CSS::CSSStyleSheet& sheet)
{
    style_computer().invalidate_rule_cache();
    // FIXME: Use correct invalidation reason
    document_or_shadow_root().invalidate_style(StyleInvalidationReason::MediaQueryChangedMatchState);

    if (sheet.has_host_selectors()) {
        if (auto* ancestor = document_or_shadow_root().parent_or_shadow_host()) {
            if (!ancestor->is_connected())
                return;
            auto& style_scope = ancestor->style_scope();
            style_scope.style_computer().invalidate_rule_cache();
            // FIXME: Use correct invalidation reason
            style_scope.document_or_shadow_root().invalidate_style(DOM::StyleInvalidationReason::MediaQueryChangedMatchState);
        }
    }
}

}
