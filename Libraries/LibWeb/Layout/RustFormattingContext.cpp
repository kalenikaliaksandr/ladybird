/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Layout/LayoutRustFFI.h>
#include <LibWeb/Layout/RustFormattingContext.h>

namespace Web::Layout {

static_assert(to_underlying(FormattingContext::Type::Block) == to_underlying(RustFFI::FfiFormattingContextType::Block));
static_assert(to_underlying(FormattingContext::Type::Inline) == to_underlying(RustFFI::FfiFormattingContextType::Inline));
static_assert(to_underlying(FormattingContext::Type::Flex) == to_underlying(RustFFI::FfiFormattingContextType::Flex));
static_assert(to_underlying(FormattingContext::Type::Grid) == to_underlying(RustFFI::FfiFormattingContextType::Grid));
static_assert(to_underlying(FormattingContext::Type::Table) == to_underlying(RustFFI::FfiFormattingContextType::Table));
static_assert(to_underlying(FormattingContext::Type::SVG) == to_underlying(RustFFI::FfiFormattingContextType::Svg));
static_assert(to_underlying(FormattingContext::Type::ReplacedWithChildren) == to_underlying(RustFFI::FfiFormattingContextType::ReplacedWithChildren));
static_assert(to_underlying(FormattingContext::Type::AbsposReplay) == to_underlying(RustFFI::FfiFormattingContextType::AbsposReplay));
static_assert(to_underlying(FormattingContext::Type::InternalReplaced) == to_underlying(RustFFI::FfiFormattingContextType::InternalReplaced));
static_assert(to_underlying(FormattingContext::Type::InternalDummy) == to_underlying(RustFFI::FfiFormattingContextType::InternalDummy));

RustFormattingContext::RustFormattingContext(Type type, LayoutMode layout_mode, LayoutState& state, Box const& box, FormattingContext* parent)
    : FormattingContext(type, layout_mode, state, box, parent)
    , m_bridge(*this)
{
    auto callbacks = m_bridge.formatting_context_callbacks();
    m_rust_context = RustFFI::rust_layout_fc_create(
        state.rust_state_handle(),
        const_cast<Box*>(&box),
        parent ? parent->rust_context_handle() : nullptr,
        to_underlying(type),
        to_underlying(layout_mode),
        state.should_collect_devtools_layout_data(),
        &callbacks);
    VERIFY(m_rust_context);
}

RustFormattingContext::~RustFormattingContext()
{
    RustFFI::rust_layout_fc_destroy(m_rust_context);
}

void RustFormattingContext::run(LayoutInput const& input)
{
    VERIFY(m_rust_context);
    RustFFI::rust_layout_fc_run(m_rust_context, LayoutRustBridge::to_ffi(input));
}

CSSPixels RustFormattingContext::automatic_content_inline_size() const
{
    VERIFY(m_rust_context);
    return CSSPixels::from_raw(RustFFI::rust_layout_fc_automatic_content_inline_size(m_rust_context));
}

CSSPixels RustFormattingContext::automatic_content_block_size() const
{
    VERIFY(m_rust_context);
    return CSSPixels::from_raw(RustFFI::rust_layout_fc_automatic_content_block_size(m_rust_context));
}

void RustFormattingContext::parent_context_did_dimension_child_root_box()
{
    RustFFI::rust_layout_fc_parent_did_dimension(m_rust_context);
}

void RustFormattingContext::replay_absolutely_positioned_element(Box& box)
{
    VERIFY(type() == Type::AbsposReplay);
    RustFFI::rust_layout_fc_replay_abspos(m_rust_context, &box);
}

void RustFormattingContext::run_until_table_inline_size_calculation(LayoutInput const& input, bool skip_row_measurement)
{
    RustFFI::rust_layout_fc_run_until_table_inline_size_calculation(
        m_rust_context, LayoutRustBridge::to_ffi(input), skip_row_measurement);
}

}
