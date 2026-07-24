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
    if (m_layout_mode != LayoutMode::Normal)
        return;
    layout_absolutely_positioned_children();
}

AbsposContainingBlockInfo RustFormattingContext::resolve_abspos_containing_block_info(Box const& box)
{
    if (type() != Type::Grid)
        return FormattingContext::resolve_abspos_containing_block_info(box);
    auto base_info = FormattingContext::resolve_abspos_containing_block_info(box);

    static_assert(to_underlying(AbsposAxisMode::StaticPosition) == to_underlying(RustFFI::FfiAbsposAxisMode::StaticPosition));
    static_assert(to_underlying(AbsposAxisMode::InsetFromRect) == to_underlying(RustFFI::FfiAbsposAxisMode::InsetFromRect));
    static_assert(to_underlying(Alignment::Baseline) == to_underlying(RustFFI::FfiAbsposAlignment::Baseline));
    static_assert(to_underlying(Alignment::Center) == to_underlying(RustFFI::FfiAbsposAlignment::Center));
    static_assert(to_underlying(Alignment::End) == to_underlying(RustFFI::FfiAbsposAlignment::End));
    static_assert(to_underlying(Alignment::Normal) == to_underlying(RustFFI::FfiAbsposAlignment::Normal));
    static_assert(to_underlying(Alignment::Safe) == to_underlying(RustFFI::FfiAbsposAlignment::Safe));
    static_assert(to_underlying(Alignment::SelfEnd) == to_underlying(RustFFI::FfiAbsposAlignment::SelfEnd));
    static_assert(to_underlying(Alignment::SelfStart) == to_underlying(RustFFI::FfiAbsposAlignment::SelfStart));
    static_assert(to_underlying(Alignment::SpaceAround) == to_underlying(RustFFI::FfiAbsposAlignment::SpaceAround));
    static_assert(to_underlying(Alignment::SpaceBetween) == to_underlying(RustFFI::FfiAbsposAlignment::SpaceBetween));
    static_assert(to_underlying(Alignment::SpaceEvenly) == to_underlying(RustFFI::FfiAbsposAlignment::SpaceEvenly));
    static_assert(to_underlying(Alignment::Start) == to_underlying(RustFFI::FfiAbsposAlignment::Start));
    static_assert(to_underlying(Alignment::Stretch) == to_underlying(RustFFI::FfiAbsposAlignment::Stretch));
    static_assert(to_underlying(Alignment::Unsafe) == to_underlying(RustFFI::FfiAbsposAlignment::Unsafe));

    RustFFI::FfiAbsposContainingBlockInfo info {};
    RustFFI::rust_layout_fc_resolve_abspos_containing_block_info(
        m_rust_context, const_cast<Box*>(&box), &info);
    auto const uses_grid_area_as_static_position = box.static_position_containing_block() == &context_box();
    return {
        .rect = {
            .offset = {
                .inline_offset = CSSPixels::from_raw(info.rect.offset.inline_offset),
                .block_offset = CSSPixels::from_raw(info.rect.offset.block_offset),
            },
            .size = {
                .inline_size = CSSPixels::from_raw(info.rect.size.inline_size),
                .block_size = CSSPixels::from_raw(info.rect.size.block_size),
            },
        },
        .inline_axis_mode = uses_grid_area_as_static_position
            ? static_cast<AbsposAxisMode>(info.inline_axis_mode)
            : base_info.inline_axis_mode,
        .block_axis_mode = uses_grid_area_as_static_position
            ? static_cast<AbsposAxisMode>(info.block_axis_mode)
            : base_info.block_axis_mode,
        .inline_alignment = info.has_inline_alignment
            ? Optional<Alignment> { static_cast<Alignment>(info.inline_alignment) }
            : Optional<Alignment> {},
        .block_alignment = info.has_block_alignment
            ? Optional<Alignment> { static_cast<Alignment>(info.block_alignment) }
            : Optional<Alignment> {},
        .derives_from_own_computed_values = info.derives_from_own_computed_values,
    };
}

void RustFormattingContext::set_pending_table_box_content_offset_in_wrapper(LogicalOffset offset)
{
    RustFFI::rust_layout_fc_set_table_box_content_offset_in_wrapper(m_rust_context, {
                                                                                       .inline_offset = offset.inline_offset.raw_value(),
                                                                                       .block_offset = offset.block_offset.raw_value(),
                                                                                   });
}

LogicalOffset RustFormattingContext::pending_table_box_content_offset_in_wrapper() const
{
    auto offset = RustFFI::rust_layout_fc_table_box_content_offset_in_wrapper(m_rust_context);
    return {
        CSSPixels::from_raw(offset.inline_offset),
        CSSPixels::from_raw(offset.block_offset),
    };
}

void RustFormattingContext::run_until_table_inline_size_calculation(LayoutInput const& input, bool skip_row_measurement)
{
    RustFFI::rust_layout_fc_run_until_table_inline_size_calculation(
        m_rust_context, LayoutRustBridge::to_ffi(input), skip_row_measurement);
}

}
