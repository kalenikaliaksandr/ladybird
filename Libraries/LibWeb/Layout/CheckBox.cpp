/*
 * Copyright (c) 2020-2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/CheckBox.h>

namespace Web::Layout {

CheckBox::CheckBox(DOM::Document& document, GC::Ptr<DOM::Element> element, NonnullRefPtr<CSS::ComputedValues const> style)
    : BlockContainer(document, element, style)
{
}

}
