/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/DOM/Node.h>

namespace Web::CSS {

class StyleScope {
public:
    virtual ~StyleScope() { }

    virtual void style_scope__invalidate_style(DOM::StyleInvalidationReason) = 0;
};

}
