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

RustFormattingContext::RustFormattingContext(Type type, LayoutMode layout_mode, LayoutState& state, Box const& box, FormattingContext* parent, void* rust_context)
    : FormattingContext(type, layout_mode, state, box, parent)
    , m_rust_context(rust_context)
{
}

RustFormattingContext::~RustFormattingContext() = default;

void RustFormattingContext::run(LayoutInput const&)
{
    VERIFY(m_rust_context);
    VERIFY_NOT_REACHED();
}

CSSPixels RustFormattingContext::automatic_content_inline_size() const
{
    VERIFY(m_rust_context);
    VERIFY_NOT_REACHED();
}

CSSPixels RustFormattingContext::automatic_content_block_size() const
{
    VERIFY(m_rust_context);
    VERIFY_NOT_REACHED();
}

}
