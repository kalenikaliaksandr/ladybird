/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/LayoutRustBridge.h>

namespace Web::Layout {

class RustFormattingContext final : public FormattingContext {
public:
    RustFormattingContext(Type, LayoutMode, LayoutState&, Box const&, FormattingContext* parent);
    virtual ~RustFormattingContext() override;

    virtual void run(LayoutInput const&) override;
    virtual CSSPixels automatic_content_inline_size() const override;
    virtual CSSPixels automatic_content_block_size() const override;
    virtual bool inhibits_floating() const override { return type() == Type::Flex; }
    virtual void* rust_context_handle() const override { return m_rust_context; }
    virtual void parent_context_did_dimension_child_root_box() override;
    virtual void set_pending_table_box_content_offset_in_wrapper(LogicalOffset) override;
    virtual LogicalOffset pending_table_box_content_offset_in_wrapper() const override;
    virtual void run_until_table_inline_size_calculation(LayoutInput const&, bool skip_row_measurement) override;

private:
    LayoutRustBridge m_bridge;
    void* m_rust_context { nullptr };
};

}
