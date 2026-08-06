/*
 * Copyright (c) 2021, Tim Flynn <trflynn89@serenityos.org>
 * Copyright (c) 2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/RadioButton.h>

namespace Web::Layout {

RadioButton::RadioButton(DOM::Document& document, GC::Ptr<DOM::Element> element, NonnullRefPtr<CSS::ComputedValues const> style)
    : BlockContainer(document, element, style)
{
}

}
