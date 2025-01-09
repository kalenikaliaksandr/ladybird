/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/DOM/StyleScope.h>
#include <LibWeb/Forward.h>

namespace Web::DOM {

class StyleScope {
public:
    virtual CSS::StyleComputer& style_computer() = 0;
    virtual Node& dom_node() = 0;

    void notify_about_added_stylesheet(CSS::CSSStyleSheet&);
    void notify_about_removed_stylesheet(Optional<CSS::CSSStyleSheet&>);
    void notify_media_query_changed_match_state(CSS::CSSStyleSheet&);
};

}
