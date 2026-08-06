/*
 * Copyright (c) 2021, Tim Flynn <trflynn89@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/DOM/Element.h>
#include <LibWeb/Layout/BlockContainer.h>

namespace Web::Layout {

class RadioButton final : public BlockContainer {
    LAYOUT_NODE(RadioButton, BlockContainer);

public:
    RadioButton(DOM::Document&, GC::Ptr<DOM::Element>, NonnullRefPtr<CSS::ComputedValues const>);

    virtual ~RadioButton() override = default;

private:
    virtual CSS::SizeWithAspectRatio compute_auto_content_box_size() const override { return { 12, 12, {} }; }
};

}
