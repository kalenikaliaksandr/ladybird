/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Layout/FormattingContext.h>

namespace Web::Layout {

class RustFormattingContext final : public FormattingContext {
public:
    RustFormattingContext(Type, LayoutMode, LayoutState&, Box const&, FormattingContext* parent, void* rust_context);
    virtual ~RustFormattingContext() override;

    virtual void run(LayoutInput const&) override;
    virtual CSSPixels automatic_content_inline_size() const override;
    virtual CSSPixels automatic_content_block_size() const override;

private:
    void* m_rust_context { nullptr };
};

}
