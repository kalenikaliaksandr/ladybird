/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/StyleComputer.h>
#include <LibWeb/DOM/AdoptedStyleSheets.h>
#include <LibWeb/DOM/Document.h>

namespace Web::DOM {

GC::Ref<WebIDL::ObservableArray> create_adopted_style_sheets_list(StyleScope& style_scope)
{
    auto& document = style_scope.dom_node().document();
    auto adopted_style_sheets = WebIDL::ObservableArray::create(document.realm());
    adopted_style_sheets->set_on_set_an_indexed_value_callback([&document, &style_scope](JS::Value value) -> WebIDL::ExceptionOr<void> {
        auto& vm = document.vm();
        if (!value.is_object())
            return vm.throw_completion<JS::TypeError>(JS::ErrorType::NotAnObjectOfType, "CSSStyleSheet");
        auto& object = value.as_object();
        if (!is<CSS::CSSStyleSheet>(object))
            return vm.throw_completion<JS::TypeError>(JS::ErrorType::NotAnObjectOfType, "CSSStyleSheet");
        auto& style_sheet = static_cast<CSS::CSSStyleSheet&>(object);

        // The set an indexed value algorithm for adoptedStyleSheets, given value and index, is the following:
        // 1. If value’s constructed flag is not set, or its constructor document is not equal to this
        //    DocumentOrShadowRoot's node document, throw a "NotAllowedError" DOMException.
        if (!style_sheet.constructed())
            return WebIDL::NotAllowedError::create(document.realm(), "StyleSheet's constructed flag is not set."_string);
        if (!style_sheet.constructed() || style_sheet.constructor_document().ptr() != &document)
            return WebIDL::NotAllowedError::create(document.realm(), "Sharing a StyleSheet between documents is not allowed."_string);

        style_sheet.add_owning_style_scope(style_scope);
        style_scope.notify_about_added_stylesheet(style_sheet);
        return {};
    });
    adopted_style_sheets->set_on_delete_an_indexed_value_callback([&document, &style_scope](JS::Value value) -> WebIDL::ExceptionOr<void> {
        auto& vm = document.vm();
        auto& object = value.as_object();
        if (!is<CSS::CSSStyleSheet>(object))
            return vm.throw_completion<JS::TypeError>(JS::ErrorType::NotAnObjectOfType, "CSSStyleSheet");
        auto& style_sheet = static_cast<CSS::CSSStyleSheet&>(object);
        style_sheet.remove_owning_style_scope(style_scope);
        style_scope.notify_about_removed_stylesheet(style_sheet);
        return {};
    });

    return adopted_style_sheets;
}

}
