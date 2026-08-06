/*
 * Copyright (c) 2020, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/DOM/Element.h>
#include <LibWeb/Layout/BlockContainer.h>

namespace Web::Layout {

class CheckBox final : public BlockContainer {
    LAYOUT_NODE(CheckBox, BlockContainer);

public:
    CheckBox(DOM::Document&, GC::Ptr<DOM::Element>, NonnullRefPtr<CSS::ComputedValues const>);

    virtual ~CheckBox() override = default;

private:
    virtual CSS::SizeWithAspectRatio compute_auto_content_box_size() const override { return { 13, 13, {} }; }
};

}
