/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/HashMap.h>
#include <AK/NeverDestroyed.h>
#include <AK/Variant.h>
#include <LibWeb/CSS/ComputedValues.h>
#include <LibWeb/CSS/Display.h>
#include <LibWeb/CSS/LengthBox.h>
#include <LibWeb/CSS/Size.h>
#include <LibWeb/CSS/StyleValues/CalcNodeRef.h>
#include <LibWeb/CSS/StyleValues/CalculatedStyleValue.h>
#include <LibWeb/CSS/ValueType.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/HTML/AttributeNames.h>
#include <LibWeb/HTML/HTMLElement.h>
#include <LibWeb/HTML/HTMLInputElement.h>
#include <LibWeb/HTML/HTMLTableCellElement.h>
#include <LibWeb/HTML/HTMLTableColElement.h>
#include <LibWeb/Layout/BlockFormattingContext.h>
#include <LibWeb/Layout/FlexLayoutData.h>
#include <LibWeb/Layout/LayoutRustBridge.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/Node.h>
#include <LibWeb/Layout/TextInputBox.h>

namespace Web::Layout {

static Atomic<size_t> s_outstanding_calc_handles;

struct RetainedCalcHandle {
    CSS::CalculatedStyleValue const* style_value;
    size_t retain_count;
};

static HashMap<void const*, RetainedCalcHandle>& retained_calc_handles()
{
    static NeverDestroyed<HashMap<void const*, RetainedCalcHandle>> handles;
    return *handles;
}

static RustFFI::FfiSizeValue size_value_with_kind(RustFFI::FfiSizeKind kind)
{
    return {
        .kind = to_underlying(kind),
        .px = 0,
        .fraction = 0,
        .calc = nullptr,
        .contains_percentage = false,
    };
}

static RustFFI::FfiSizeValue retain_calculated(CSS::CalculatedStyleValue const& calculated, bool contains_percentage, RustFFI::FfiSizeKind kind = RustFFI::FfiSizeKind::Calc)
{
    auto const* handle = calculated.rust_style_value_data();
    calculated.ref();
    auto& retained = retained_calc_handles().ensure(handle, [&] {
        return RetainedCalcHandle {
            .style_value = &calculated,
            .retain_count = 0,
        };
    });
    VERIFY(retained.style_value == &calculated);
    ++retained.retain_count;
    ++s_outstanding_calc_handles;
    RustFFI::rust_layout_ffi_note_calc_handle_retain();
    return {
        .kind = to_underlying(kind),
        .px = 0,
        .fraction = 0,
        .calc = handle,
        .contains_percentage = contains_percentage,
    };
}

RustFFI::FfiSizeValue build_style_size_value(CSS::LengthPercentage const& value)
{
    if (value.is_length()) {
        VERIFY(value.length().is_absolute());
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Px),
            .px = value.length().absolute_length_to_px().raw_value(),
            .fraction = 0,
            .calc = nullptr,
            .contains_percentage = false,
        };
    }
    if (value.is_percentage()) {
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
            .contains_percentage = true,
        };
    }
    return retain_calculated(*value.calculated(), value.contains_percentage());
}

RustFFI::FfiSizeValue build_style_size_value(CSS::LengthPercentageOrAuto const& value)
{
    if (value.is_auto())
        return size_value_with_kind(RustFFI::FfiSizeKind::Auto);
    return build_style_size_value(value.length_percentage());
}

RustFFI::FfiSizeValue build_style_size_value(CSS::Size const& value)
{
    switch (value.type()) {
    case CSS::Size::Type::Auto:
        return size_value_with_kind(RustFFI::FfiSizeKind::Auto);
    case CSS::Size::Type::Calculated:
        return retain_calculated(value.calculated(), value.contains_percentage());
    case CSS::Size::Type::Length:
        VERIFY(value.length().is_absolute());
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Px),
            .px = value.length().absolute_length_to_px().raw_value(),
            .fraction = 0,
            .calc = nullptr,
            .contains_percentage = false,
        };
    case CSS::Size::Type::Percentage:
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
            .contains_percentage = true,
        };
    case CSS::Size::Type::MinContent:
        return size_value_with_kind(RustFFI::FfiSizeKind::MinContent);
    case CSS::Size::Type::MaxContent:
        return size_value_with_kind(RustFFI::FfiSizeKind::MaxContent);
    case CSS::Size::Type::FitContent: {
        auto result = size_value_with_kind(RustFFI::FfiSizeKind::FitContent);
        if (value.fit_content_available_space().has_value()) {
            result = build_style_size_value(*value.fit_content_available_space());
            result.kind = to_underlying(RustFFI::FfiSizeKind::FitContent);
        }
        return result;
    }
    case CSS::Size::Type::None:
        return size_value_with_kind(RustFFI::FfiSizeKind::None_);
    }
    VERIFY_NOT_REACHED();
}

StyleVerticalAlignFacts build_style_vertical_align_value(Variant<CSS::VerticalAlign, CSS::LengthPercentage> const& value)
{
    if (value.has<CSS::VerticalAlign>()) {
        return {
            .is_keyword = true,
            .keyword = to_underlying(value.get<CSS::VerticalAlign>()),
            .value = size_value_with_kind(RustFFI::FfiSizeKind::Auto),
        };
    }
    return {
        .is_keyword = false,
        .keyword = 0,
        .value = build_style_size_value(value.get<CSS::LengthPercentage>()),
    };
}

static RustFFI::FfiDisplay encode_display(CSS::Display const& display)
{
    static_assert(to_underlying(CSS::Display::Type::OutsideAndInside) == 0);
    static_assert(to_underlying(CSS::Display::Type::Internal) == 1);
    static_assert(to_underlying(CSS::Display::Type::Box) == 2);
    static_assert(to_underlying(CSS::DisplayBox::Contents) == 0);
    static_assert(to_underlying(CSS::DisplayBox::None) == 1);
    static_assert(to_underlying(CSS::DisplayOutside::Block) == 0);
    static_assert(to_underlying(CSS::DisplayOutside::Inline) == 1);
    static_assert(to_underlying(CSS::DisplayInside::Flow) == 0);
    static_assert(to_underlying(CSS::DisplayInside::FlowRoot) == 1);
    static_assert(to_underlying(CSS::DisplayInside::Table) == 2);
    static_assert(to_underlying(CSS::DisplayInside::Flex) == 3);
    static_assert(to_underlying(CSS::DisplayInside::Grid) == 4);
    static_assert(to_underlying(CSS::DisplayInside::Ruby) == 5);
    static_assert(to_underlying(CSS::DisplayInside::Math) == 6);
    static_assert(to_underlying(CSS::DisplayInternal::TableRowGroup) == 0);
    static_assert(to_underlying(CSS::DisplayInternal::TableHeaderGroup) == 1);
    static_assert(to_underlying(CSS::DisplayInternal::TableFooterGroup) == 2);
    static_assert(to_underlying(CSS::DisplayInternal::TableRow) == 3);
    static_assert(to_underlying(CSS::DisplayInternal::TableCell) == 4);
    static_assert(to_underlying(CSS::DisplayInternal::TableColumnGroup) == 5);
    static_assert(to_underlying(CSS::DisplayInternal::TableColumn) == 6);
    static_assert(to_underlying(CSS::DisplayInternal::TableCaption) == 7);
    static_assert(to_underlying(CSS::LengthUnit::Px) == 29);

    switch (display.type()) {
    case CSS::Display::Type::OutsideAndInside:
        return {
            .tag = to_underlying(display.type()),
            .outside = to_underlying(display.outside()),
            .inside = to_underlying(display.inside()),
            .list_item = display.is_list_item(),
            .internal = 0,
            .box_value = 0,
        };
    case CSS::Display::Type::Internal:
        return {
            .tag = to_underlying(display.type()),
            .outside = 0,
            .inside = 0,
            .list_item = false,
            .internal = to_underlying(display.internal()),
            .box_value = 0,
        };
    case CSS::Display::Type::Box:
        return {
            .tag = to_underlying(display.type()),
            .outside = 0,
            .inside = 0,
            .list_item = false,
            .internal = 0,
            .box_value = display.is_none() ? to_underlying(CSS::DisplayBox::None) : to_underlying(CSS::DisplayBox::Contents),
        };
    }
    VERIFY_NOT_REACHED();
}

RustFFI::FfiLayoutBoxFacts build_layout_box_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_box_facts_build();
    auto const* box = as_if<Box>(node);
    auto natural_size = box ? box->natural_size() : CSS::SizeWithAspectRatio {};
    auto auto_content_size = box ? box->auto_content_box_size() : CSS::SizeWithAspectRatio {};
    auto preferred_aspect_ratio = box ? box->preferred_aspect_ratio() : Optional<CSSPixelFraction> {};
    auto display = node.display();
    auto const* dom_node = node.dom_node();
    Optional<CSS::SizeWithAspectRatio> default_preferred_size;
    if (box && box->computed_values().appearance() == CSS::Appearance::None) {
        if (auto const* input = as_if<HTML::HTMLInputElement>(dom_node)) {
            switch (input->type_state()) {
            case HTML::HTMLInputElement::TypeAttributeState::Text:
            case HTML::HTMLInputElement::TypeAttributeState::Search:
            case HTML::HTMLInputElement::TypeAttributeState::URL:
            case HTML::HTMLInputElement::TypeAttributeState::Telephone:
            case HTML::HTMLInputElement::TypeAttributeState::Email:
            case HTML::HTMLInputElement::TypeAttributeState::Password:
            case HTML::HTMLInputElement::TypeAttributeState::Number:
                default_preferred_size = TextInputBox::default_preferred_size_for_text_control(*input, *box);
                break;
            default:
                break;
            }
        }
    }

    return {
        .is_box = node.is_box(),
        .is_block_container = node.is_block_container(),
        .is_replaced_box = node.is_replaced_box(),
        .is_replaced_box_with_children = node.is_replaced_box_with_children(),
        .is_floating = node.is_floating(),
        .is_absolutely_positioned = node.is_absolutely_positioned(),
        .is_inline = node.is_inline(),
        .is_inline_block = node.is_inline_block(),
        .children_are_inline = node.children_are_inline(),
        .is_anonymous = node.is_anonymous(),
        .can_have_children = node.can_have_children(),
        .has_replaced_element_table_display_adjustment = node.has_replaced_element_table_display_adjustment(),
        .creates_block_formatting_context = box && FormattingContext::creates_block_formatting_context(*box),
        .has_definite_natural_width = natural_size.has_width(),
        .natural_width = natural_size.width.value_or(0).raw_value(),
        .has_definite_natural_height = natural_size.has_height(),
        .natural_height = natural_size.height.value_or(0).raw_value(),
        .has_definite_natural_aspect_ratio = natural_size.has_aspect_ratio(),
        .natural_aspect_ratio = natural_size.aspect_ratio.has_value() ? natural_size.aspect_ratio->to_double() : 0,
        .has_auto_content_width = auto_content_size.has_width(),
        .auto_content_width = auto_content_size.width.value_or(0).raw_value(),
        .has_auto_content_height = auto_content_size.has_height(),
        .auto_content_height = auto_content_size.height.value_or(0).raw_value(),
        .has_auto_content_aspect_ratio = auto_content_size.has_aspect_ratio(),
        .auto_content_aspect_ratio_numerator = auto_content_size.aspect_ratio.has_value() ? auto_content_size.aspect_ratio->numerator().raw_value() : 0,
        .auto_content_aspect_ratio_denominator = auto_content_size.aspect_ratio.has_value() ? auto_content_size.aspect_ratio->denominator().raw_value() : 0,
        .has_auto_content_box_size = box && box->has_auto_content_box_size(),
        .has_preferred_aspect_ratio = preferred_aspect_ratio.has_value(),
        .preferred_aspect_ratio_numerator = preferred_aspect_ratio.has_value() ? preferred_aspect_ratio->numerator().raw_value() : 0,
        .preferred_aspect_ratio_denominator = preferred_aspect_ratio.has_value() ? preferred_aspect_ratio->denominator().raw_value() : 0,
        .has_default_preferred_width = default_preferred_size.has_value() && default_preferred_size->has_width(),
        .default_preferred_width = default_preferred_size.has_value() ? default_preferred_size->width.value_or(0).raw_value() : 0,
        .has_default_preferred_height = default_preferred_size.has_value() && default_preferred_size->has_height(),
        .default_preferred_height = default_preferred_size.has_value() ? default_preferred_size->height.value_or(0).raw_value() : 0,
        .initial_containing_block_inline_size = node.document().viewport_rect().width().raw_value(),
        .is_scroll_container = node.is_scroll_container(),
        .layout_index = node.layout_index(),
        .display = encode_display(display),
        .is_svg_box = node.is_svg_box(),
        .is_svg_svg_box = node.is_svg_svg_box(),
        .is_table_box = display.is_table_inside(),
        .is_table_wrapper = node.is_table_wrapper(),
        .is_table_row_group = display.is_table_row_group(),
        .is_table_header_group = display.is_table_header_group(),
        .is_table_footer_group = display.is_table_footer_group(),
        .is_table_row = display.is_table_row(),
        .is_table_cell = display.is_table_cell(),
        .is_table_column_group = display.is_table_column_group(),
        .is_table_column = display.is_table_column(),
        .is_table_caption = display.is_table_caption(),
        .is_viewport = node.is_viewport(),
        .document_in_quirks_mode = node.document().in_quirks_mode(),
        .is_in_user_agent_shadow_tree = dom_node && dom_node->containing_shadow_root() && dom_node->containing_shadow_root()->is_user_agent_internal(),
        .is_html_html_element = dom_node && dom_node->is_html_html_element(),
        .is_html_body_element = dom_node && dom_node->is_html_body_element(),
    };
}

RustFFI::FfiTableBoxFacts build_table_box_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_table_facts_build();
    auto const& values = node.computed_values();

    size_t cell_column_span = 1;
    size_t cell_row_span = 1;
    u32 column_span = 1;
    u32 raw_column_span = 1;
    if (auto const* dom_node = node.dom_node()) {
        if (auto const* cell = as_if<HTML::HTMLTableCellElement>(*dom_node)) {
            cell_column_span = cell->col_span();
            cell_row_span = cell->row_span();
        }
        if (auto const* column = as_if<HTML::HTMLTableColElement>(*dom_node))
            column_span = column->span();
        if (auto const* element = as_if<HTML::HTMLElement>(*dom_node))
            raw_column_span = element->get_attribute_value(HTML::AttributeNames::span).to_number<u32>().value_or(1);
    }

    return {
        .cell_column_span = cell_column_span,
        .cell_row_span = cell_row_span,
        .column_span = column_span,
        .raw_column_span = raw_column_span,
        .border_top_color = values.border_top().color.value(),
        .border_right_color = values.border_right().color.value(),
        .border_bottom_color = values.border_bottom().color.value(),
        .border_left_color = values.border_left().color.value(),
    };
}

LayoutRustBridge::LayoutRustBridge(FormattingContext& formatting_context)
    : m_formatting_context(formatting_context)
{
}

LayoutRustBridge::~LayoutRustBridge() = default;

static RustFFI::AvailableSize to_ffi_available_size(AvailableSize const& size)
{
    RustFFI::AvailableSizeType type;
    if (size.is_definite())
        type = RustFFI::AvailableSizeType::Definite;
    else if (size.is_indefinite())
        type = RustFFI::AvailableSizeType::Indefinite;
    else if (size.is_min_content())
        type = RustFFI::AvailableSizeType::MinContent;
    else
        type = RustFFI::AvailableSizeType::MaxContent;
    return {
        .type_ = type,
        .value = size.to_px_or_zero().raw_value(),
    };
}

static AvailableSize from_ffi_available_size(RustFFI::AvailableSize const& size)
{
    switch (size.type_) {
    case RustFFI::AvailableSizeType::Definite:
        return AvailableSize::make_definite(CSSPixels::from_raw(size.value));
    case RustFFI::AvailableSizeType::Indefinite:
        return AvailableSize::make_indefinite();
    case RustFFI::AvailableSizeType::MinContent:
        return AvailableSize::make_min_content();
    case RustFFI::AvailableSizeType::MaxContent:
        return AvailableSize::make_max_content();
    }
    VERIFY_NOT_REACHED();
}

static RustFFI::FfiContainingBlockConstraints to_ffi_constraints(ContainingBlockConstraints const& constraints)
{
    return {
        .has_percentage_basis_inline_size = constraints.percentage_basis_inline_size.has_value(),
        .percentage_basis_inline_size = constraints.percentage_basis_inline_size.value_or(0).raw_value(),
        .has_percentage_basis_block_size = constraints.percentage_basis_block_size.has_value(),
        .percentage_basis_block_size = constraints.percentage_basis_block_size.value_or(0).raw_value(),
        .has_quirks_mode_percentage_basis_block_size = constraints.quirks_mode_percentage_basis_block_size.has_value(),
        .quirks_mode_percentage_basis_block_size = constraints.quirks_mode_percentage_basis_block_size.value_or(0).raw_value(),
    };
}

static ContainingBlockConstraints from_ffi_constraints(RustFFI::FfiContainingBlockConstraints const& constraints)
{
    return {
        .percentage_basis_inline_size = constraints.has_percentage_basis_inline_size
            ? Optional<CSSPixels> { CSSPixels::from_raw(constraints.percentage_basis_inline_size) }
            : Optional<CSSPixels> {},
        .percentage_basis_block_size = constraints.has_percentage_basis_block_size
            ? Optional<CSSPixels> { CSSPixels::from_raw(constraints.percentage_basis_block_size) }
            : Optional<CSSPixels> {},
        .quirks_mode_percentage_basis_block_size = constraints.has_quirks_mode_percentage_basis_block_size
            ? Optional<CSSPixels> { CSSPixels::from_raw(constraints.quirks_mode_percentage_basis_block_size) }
            : Optional<CSSPixels> {},
    };
}

RustFFI::FfiLayoutInput LayoutRustBridge::to_ffi(LayoutInput const& input)
{
    return {
        .available_space = {
            .inline_size = to_ffi_available_size(input.available_space.inline_size),
            .block_size = to_ffi_available_size(input.available_space.block_size),
        },
        .containing_block_constraints = to_ffi_constraints(input.containing_block_constraints),
        .has_content_box_position_in_bfc_root = input.content_box_position_in_bfc_root.has_value(),
        .content_box_position_in_bfc_root = {
            .x = input.content_box_position_in_bfc_root.value_or({}).x().raw_value(),
            .y = input.content_box_position_in_bfc_root.value_or({}).y().raw_value(),
        },
        .has_table_grid_min_border_box_block_size = input.table_grid_min_border_box_block_size.has_value(),
        .table_grid_min_border_box_block_size = input.table_grid_min_border_box_block_size.value_or(0).raw_value(),
    };
}

LayoutInput LayoutRustBridge::from_ffi(RustFFI::FfiLayoutInput const& input)
{
    auto available_space = AvailableSpace {
        from_ffi_available_size(input.available_space.inline_size),
        from_ffi_available_size(input.available_space.block_size),
    };
    auto constraints = from_ffi_constraints(input.containing_block_constraints);
    auto content_box_position = input.has_content_box_position_in_bfc_root
        ? Optional<CSSPixelPoint> { CSSPixelPoint { CSSPixels::from_raw(input.content_box_position_in_bfc_root.x), CSSPixels::from_raw(input.content_box_position_in_bfc_root.y) } }
        : Optional<CSSPixelPoint> {};
    auto table_min_size = input.has_table_grid_min_border_box_block_size
        ? Optional<CSSPixels> { CSSPixels::from_raw(input.table_grid_min_border_box_block_size) }
        : Optional<CSSPixels> {};
    return LayoutInput { available_space, constraints, content_box_position, table_min_size };
}

RustFFI::FfiLayoutNavCallbacks LayoutRustBridge::navigation_callbacks()
{
    return {
        .context = this,
        .parent = [](void* context, void* node) -> void* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return const_cast<Node*>(bridge.parent(*static_cast<Node const*>(node)));
        },
        .first_child = [](void* context, void* node) -> void* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return const_cast<Node*>(bridge.first_child(*static_cast<Node const*>(node)));
        },
        .next_sibling = [](void* context, void* node) -> void* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return const_cast<Node*>(bridge.next_sibling(*static_cast<Node const*>(node)));
        },
        .previous_sibling = [](void* context, void* node) -> void* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return const_cast<Node*>(bridge.previous_sibling(*static_cast<Node const*>(node)));
        },
        .containing_block = [](void* context, void* node) -> void* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return const_cast<Box*>(bridge.containing_block(*static_cast<Node const*>(node)));
        },
    };
}

static LayoutMode layout_mode_from_ffi(u8 mode)
{
    static_assert(to_underlying(LayoutMode::Normal) == 0);
    static_assert(to_underlying(LayoutMode::IntrinsicSizing) == 1);
    VERIFY(mode <= to_underlying(LayoutMode::IntrinsicSizing));
    return static_cast<LayoutMode>(mode);
}

static CSS::BorderData from_ffi_border_data(RustFFI::FfiBorderData const& border)
{
    return {
        .color = Gfx::Color::from_bgra(border.color),
        .line_style = static_cast<CSS::LineStyle>(border.line_style),
        .width = CSSPixels::from_raw(border.width),
    };
}

static StaticPositionRect from_ffi_static_position_rect(RustFFI::FfiStaticPositionRect const& rect)
{
    return {
        .rect = {
            .offset = {
                .inline_offset = CSSPixels::from_raw(rect.rect.offset.inline_offset),
                .block_offset = CSSPixels::from_raw(rect.rect.offset.block_offset),
            },
            .size = {
                .inline_size = CSSPixels::from_raw(rect.rect.size.inline_size),
                .block_size = CSSPixels::from_raw(rect.rect.size.block_size),
            },
        },
        .inline_alignment = static_cast<StaticPositionRect::Alignment>(rect.inline_alignment),
        .block_alignment = static_cast<StaticPositionRect::Alignment>(rect.block_alignment),
        .alignment_derives_from_own_computed_values = rect.alignment_derives_from_own_computed_values,
    };
}

RustFFI::FfiLayoutFcCallbacks LayoutRustBridge::formatting_context_callbacks()
{
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Cell) == 0);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Row) == 1);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::RowGroup) == 2);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Column) == 3);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::ColumnGroup) == 4);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Table) == 5);
    static_assert(to_underlying(FormattingContext::BaselineSet::First) == 0);
    static_assert(to_underlying(FormattingContext::BaselineSet::Last) == 1);
    static_assert(to_underlying(CSS::FlexDirection::Row) == 0);
    static_assert(to_underlying(CSS::FlexDirection::RowReverse) == 1);
    static_assert(to_underlying(CSS::FlexDirection::Column) == 2);
    static_assert(to_underlying(CSS::FlexDirection::ColumnReverse) == 3);
    static_assert(to_underlying(CSS::FlexWrap::Nowrap) == 0);
    static_assert(to_underlying(CSS::FlexWrap::Wrap) == 1);
    static_assert(to_underlying(CSS::FlexWrap::WrapReverse) == 2);
    static_assert(to_underlying(FlexLayoutGrowthState::Growing) == 0);
    static_assert(to_underlying(FlexLayoutGrowthState::Shrinking) == 1);
    static_assert(to_underlying(FlexLayoutClampState::Unclamped) == 0);
    static_assert(to_underlying(FlexLayoutClampState::ClampedToMin) == 1);
    static_assert(to_underlying(FlexLayoutClampState::ClampedToMax) == 2);
    static_assert(to_underlying(StaticPositionRect::Alignment::Start) == 0);
    static_assert(to_underlying(StaticPositionRect::Alignment::Center) == 1);
    static_assert(to_underlying(StaticPositionRect::Alignment::End) == 2);

    return {
        .context = this,
        .navigation = navigation_callbacks(),
        .build_style_facts = [](void*, void* node) {
            return build_style_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .build_box_facts = [](void*, void* node) {
            auto const* node_with_style = as_if<NodeWithStyle>(*static_cast<Node const*>(node));
            return node_with_style ? build_layout_box_facts(*node_with_style) : RustFFI::FfiLayoutBoxFacts {};
        },
        .build_table_box_facts = [](void*, void* node) {
            return build_table_box_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .create_used_values = [](void* context, void* node, bool has_inline_basis, i32 inline_basis, bool has_block_basis, i32 block_basis) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& used_values = bridge.m_formatting_context.m_state.create(
                *static_cast<NodeWithStyle const*>(node),
                has_inline_basis ? Optional<CSSPixels> { CSSPixels::from_raw(inline_basis) } : Optional<CSSPixels> {},
                has_block_basis ? Optional<CSSPixels> { CSSPixels::from_raw(block_basis) } : Optional<CSSPixels> {});
            return &used_values.core();
        },
        .get_used_values = [](void* context, void* node) -> RustFFI::UsedValuesCore* {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto* used_values = bridge.m_formatting_context.m_state.try_get_mutable(*static_cast<NodeWithStyle const*>(node));
            return used_values ? &used_values->core() : nullptr;
        },
        .layout_inside_child = [](void* context, void* child, u8 mode, RustFFI::FfiLayoutInput input, RustFFI::FfiChildLayoutResult* out) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& child_box = *static_cast<Box const*>(child);
            VERIFY(!bridge.m_child_contexts.contains(&child_box));
            auto child_context = bridge.m_formatting_context.layout_inside(child_box, layout_mode_from_ffi(mode), from_ffi(input));
            if (!child_context)
                return false;
            *out = {
                .automatic_content_inline_size = child_context->automatic_content_inline_size().raw_value(),
                .automatic_content_block_size = child_context->automatic_content_block_size().raw_value(),
            };
            bridge.m_child_contexts.set(&child_box, move(child_context));
            return true;
        },
        .parent_did_dimension_child_root_box = [](void* context, void* child) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const* child_box = static_cast<Box const*>(child);
            auto child_context = bridge.m_child_contexts.take(child_box);
            VERIFY(child_context.has_value());
            child_context.value()->parent_context_did_dimension_child_root_box();
        },
        .discard_child_context = [](void* context, void* child) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto child_context = bridge.m_child_contexts.take(static_cast<Box const*>(child));
            VERIFY(child_context.has_value());
        },
        .calculate_min_content_inline_size = [](void* context, void* box, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.calculate_min_content_inline_size(*static_cast<Box const*>(box), from_ffi_constraints(constraints)).raw_value();
        },
        .calculate_max_content_inline_size = [](void* context, void* box, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.calculate_max_content_inline_size(*static_cast<Box const*>(box), from_ffi_constraints(constraints)).raw_value();
        },
        .calculate_min_content_block_size = [](void* context, void* box, i32 inline_size, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.calculate_min_content_block_size(*static_cast<Box const*>(box), CSSPixels::from_raw(inline_size), from_ffi_constraints(constraints)).raw_value();
        },
        .calculate_max_content_block_size = [](void* context, void* box, i32 inline_size, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.calculate_max_content_block_size(*static_cast<Box const*>(box), CSSPixels::from_raw(inline_size), from_ffi_constraints(constraints)).raw_value();
        },
        .set_table_cell_coordinates = [](void* context, void* node, size_t row_index, size_t column_index, size_t row_span, size_t column_span) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.m_state.get_mutable(*static_cast<NodeWithStyle const*>(node)).set_table_cell_coordinates({
                .row_index = row_index,
                .column_index = column_index,
                .row_span = row_span,
                .column_span = column_span,
            });
        },
        .set_override_borders_data = [](void* context, void* node, RustFFI::FfiBordersData const* borders) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto convert = [](RustFFI::FfiBorderDataWithElementKind const& border) {
                return Painting::Paintable::BorderDataWithElementKind {
                    .border_data = from_ffi_border_data(border.border_data),
                    .element_kind = static_cast<Painting::Paintable::ConflictingElementKind>(border.element_kind),
                };
            };
            bridge.m_formatting_context.m_state.get_mutable(*static_cast<NodeWithStyle const*>(node)).set_override_borders_data({
                .top = convert(borders->top),
                .right = convert(borders->right),
                .bottom = convert(borders->bottom),
                .left = convert(borders->left),
            });
        },
        .place_child = [](void* context, void* node, RustFFI::FfiCssPixelPoint offset) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.place_child(
                *static_cast<Box const*>(node),
                { CSSPixels::from_raw(offset.x), CSSPixels::from_raw(offset.y) });
        },
        .box_baseline = [](void* context, void* node, u8 baseline_set) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.box_baseline(
                *static_cast<Box const*>(node),
                static_cast<FormattingContext::BaselineSet>(baseline_set))
                .raw_value();
        },
        .compute_and_store_baselines = [](void* context, void* node) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.compute_and_store_baselines(
                bridge.m_formatting_context.m_state.get_mutable(*static_cast<NodeWithStyle const*>(node)));
        },
        .layout_absolutely_positioned_children = [](void* context, void* node) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.layout_absolutely_positioned_children(*static_cast<Box const*>(node));
        },
        .layout_table_caption = [](void* context, void* child, u8 phase, RustFFI::AvailableSpace available_space, RustFFI::FfiContainingBlockConstraints constraints, RustFFI::FfiCaptionLayoutResult* out) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& formatting_context = bridge.m_formatting_context;
            auto const& child_box = *static_cast<Box const*>(child);
            auto caption_available_space = AvailableSpace {
                from_ffi_available_size(available_space.inline_size),
                from_ffi_available_size(available_space.block_size),
            };
            auto caption_constraints = from_ffi_constraints(constraints);
            auto caption_phase = static_cast<CSS::CaptionSide>(phase);
            bool caption_was_placed = false;

            if (auto caption_context = FormattingContext::create_independent_formatting_context_if_needed(
                    formatting_context.m_state, formatting_context.m_layout_mode, child_box, &formatting_context)) {
                auto inner_available_space = caption_available_space;
                auto* block_context = as_if<BlockFormattingContext>(caption_context.ptr());
                LogicalOffset caption_offset;
                if (block_context) {
                    auto available_inline_size = caption_available_space.inline_size.to_px_or_zero();
                    block_context->resolve_vertical_box_model_metrics(child_box, available_inline_size);
                    CSSPixelPoint caption_position_in_block_context {};
                    block_context->compute_inline_size(child_box, caption_available_space, caption_constraints, caption_position_in_block_context);
                    inner_available_space = formatting_context.m_state.get(child_box).available_inner_space_or_constraints_from(caption_available_space);

                    auto const& caption_state = formatting_context.m_state.get(child_box);
                    caption_offset = { caption_state.border_box_left(), 0 };
                    if (caption_phase == CSS::CaptionSide::Bottom)
                        caption_offset.block_offset = formatting_context.m_state.get(formatting_context.context_box()).margin_box_block_size() + caption_state.margin_box_top();
                }

                caption_context->run(LayoutInput { inner_available_space, caption_constraints });
                if (block_context) {
                    auto& caption_state = formatting_context.m_state.get_mutable(child_box);
                    if (formatting_context.should_treat_block_size_as_auto(child_box, caption_available_space, caption_constraints)) {
                        auto content_block_size = child_box.has_size_containment() ? 0 : caption_context->automatic_content_block_size();
                        caption_state.set_content_block_size(content_block_size);
                    }
                    formatting_context.place_child(child_box, { caption_offset.inline_offset, caption_offset.block_offset });
                    caption_was_placed = true;
                }
            }

            auto const& caption_state = formatting_context.m_state.get(child_box);
            if (caption_phase == CSS::CaptionSide::Bottom && !caption_was_placed) {
                formatting_context.place_child(child_box, {
                                                              caption_state.border_box_left(),
                                                              formatting_context.m_state.get(formatting_context.context_box()).margin_box_block_size() + caption_state.margin_box_top(),
                                                          });
            }
            *out = {
                .margin_box_block_size = caption_state.margin_box_block_size().raw_value(),
                .pending_table_block_offset = caption_phase == CSS::CaptionSide::Top
                    ? (caption_state.content_block_size() + caption_state.margin_box_bottom()).raw_value()
                    : 0,
            };
            return true;
        },
        .measure_table_cell_content = [](void* context, void* child, u8 mode, RustFFI::UsedValuesCore const*, RustFFI::AvailableSpace inner_available_space, RustFFI::FfiMeasuredCellContent* out) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& formatting_context = bridge.m_formatting_context;
            auto const& cell_box = *static_cast<Box const*>(child);
            auto const& cell_used_values = formatting_context.m_state.get(cell_box);
            auto layout_mode = layout_mode_from_ffi(mode);

            if (layout_mode == LayoutMode::IntrinsicSizing
                && !cell_box.is_inline()
                && cell_used_values.inline_size_constraint() == SizeConstraint::None
                && cell_used_values.block_size_constraint() == SizeConstraint::None
                && cell_used_values.has_definite_inline_size()
                && cell_used_values.has_definite_block_size())
                return false;
            if (!cell_box.can_have_children())
                return false;

            LayoutState throwaway_state(cell_box, LayoutState::Purpose::Measurement);
            auto& throwaway_values = throwaway_state.create(cell_box, {}, {});
            throwaway_values.set_inline_size_constraint(cell_used_values.inline_size_constraint());
            throwaway_values.set_block_size_constraint(cell_used_values.block_size_constraint());
            throwaway_values.set_content_inline_size(cell_used_values.content_inline_size());
            throwaway_values.set_content_block_size(cell_used_values.content_block_size());
            throwaway_values.set_has_definite_inline_size(cell_used_values.has_definite_inline_size());
            throwaway_values.set_has_definite_block_size(cell_used_values.has_definite_block_size());
            throwaway_values.set_margin_left(cell_used_values.margin_left());
            throwaway_values.set_margin_right(cell_used_values.margin_right());
            throwaway_values.set_margin_top(cell_used_values.margin_top());
            throwaway_values.set_margin_bottom(cell_used_values.margin_bottom());
            throwaway_values.set_border_left(cell_used_values.border_left());
            throwaway_values.set_border_right(cell_used_values.border_right());
            throwaway_values.set_border_top(cell_used_values.border_top());
            throwaway_values.set_border_bottom(cell_used_values.border_bottom());
            throwaway_values.set_padding_left(cell_used_values.padding_left());
            throwaway_values.set_padding_right(cell_used_values.padding_right());
            throwaway_values.set_padding_top(cell_used_values.padding_top());
            throwaway_values.set_padding_bottom(cell_used_values.padding_bottom());
            if (auto const& override_borders = cell_used_values.override_borders_data(); override_borders.has_value())
                throwaway_values.set_override_borders_data(*override_borders);

            auto measuring_context = FormattingContext::create_independent_formatting_context_if_needed(
                throwaway_state, layout_mode, cell_box, &formatting_context);
            if (!measuring_context)
                return false;
            auto available = AvailableSpace {
                from_ffi_available_size(inner_available_space.inline_size),
                from_ffi_available_size(inner_available_space.block_size),
            };
            measuring_context->run(LayoutInput { available });
            auto content_block_size = measuring_context->automatic_content_block_size();
            throwaway_values.set_content_block_size(content_block_size);
            *out = {
                .content_block_size = content_block_size.raw_value(),
                .first_baseline = measuring_context->box_baseline(cell_box, FormattingContext::BaselineSet::First).raw_value(),
            };
            return true;
        },
        .should_treat_max_inline_size_as_none = [](void* context, void* box, RustFFI::AvailableSize available_size, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.should_treat_max_inline_size_as_none(
                *static_cast<Box const*>(box),
                from_ffi_available_size(available_size),
                from_ffi_constraints(constraints));
        },
        .calculate_inner_inline_size = [](void* context, void* box, RustFFI::AvailableSize available_size, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& child_box = *static_cast<Box const*>(box);
            return bridge.m_formatting_context.calculate_inner_inline_size(
                child_box,
                from_ffi_available_size(available_size),
                child_box.computed_values().width(),
                from_ffi_constraints(constraints))
                .raw_value();
        },
        .constraints_for_child_context = [](void* context, void* box, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& used_values = bridge.m_formatting_context.m_state.get(*static_cast<Box const*>(box));
            return to_ffi_constraints(FormattingContext::constraints_for_child_context(used_values, from_ffi_constraints(constraints)));
        },
        .can_skip_is_anonymous_text_run = [](void* context, void* box) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.can_skip_is_anonymous_text_run(*static_cast<Box*>(box));
        },
        .set_flex_item = [](void*, void* box, bool is_flex_item) {
            static_cast<Box*>(box)->set_flex_item(is_flex_item);
        },
        .calculate_fit_content_size = [](void* context, void* box, RustFFI::FfiFlexAxis axis, RustFFI::AvailableSpace available_space, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& child_box = *static_cast<Box const*>(box);
            auto space = AvailableSpace {
                from_ffi_available_size(available_space.inline_size),
                from_ffi_available_size(available_space.block_size),
            };
            if (axis == RustFFI::FfiFlexAxis::Inline)
                return bridge.m_formatting_context.calculate_fit_content_inline_size(child_box, space, from_ffi_constraints(constraints)).raw_value();
            return bridge.m_formatting_context.calculate_fit_content_block_size(child_box, space, from_ffi_constraints(constraints)).raw_value();
        },
        .calculate_inner_size_for_property = [](void* context, void* box, RustFFI::FfiFlexAxis axis, RustFFI::FfiFlexSizeProperty property, RustFFI::AvailableSpace available_space, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& child_box = *static_cast<Box const*>(box);
            auto const& values = child_box.computed_values();
            CSS::Size const* size = nullptr;
            switch (property) {
            case RustFFI::FfiFlexSizeProperty::Width:
                size = &values.width();
                break;
            case RustFFI::FfiFlexSizeProperty::Height:
                size = &values.height();
                break;
            case RustFFI::FfiFlexSizeProperty::MinWidth:
                size = &values.min_width();
                break;
            case RustFFI::FfiFlexSizeProperty::MinHeight:
                size = &values.min_height();
                break;
            case RustFFI::FfiFlexSizeProperty::MaxWidth:
                size = &values.max_width();
                break;
            case RustFFI::FfiFlexSizeProperty::MaxHeight:
                size = &values.max_height();
                break;
            case RustFFI::FfiFlexSizeProperty::FlexBasis:
                size = values.flex_basis().get_pointer<CSS::Size>();
                break;
            }
            VERIFY(size);
            auto space = AvailableSpace {
                from_ffi_available_size(available_space.inline_size),
                from_ffi_available_size(available_space.block_size),
            };
            auto converted_constraints = from_ffi_constraints(constraints);
            if (axis == RustFFI::FfiFlexAxis::Inline)
                return bridge.m_formatting_context.calculate_inner_inline_size(child_box, space.inline_size, *size, converted_constraints).raw_value();
            return bridge.m_formatting_context.calculate_inner_block_size(child_box, space, *size, converted_constraints).raw_value();
        },
        .should_treat_size_as_auto = [](void* context, void* box, RustFFI::FfiFlexAxis axis, RustFFI::AvailableSpace available_space, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto const& child_box = *static_cast<Box const*>(box);
            auto space = AvailableSpace {
                from_ffi_available_size(available_space.inline_size),
                from_ffi_available_size(available_space.block_size),
            };
            if (axis == RustFFI::FfiFlexAxis::Inline)
                return bridge.m_formatting_context.should_treat_inline_size_as_auto(child_box, space);
            return bridge.m_formatting_context.should_treat_block_size_as_auto(child_box, space, from_ffi_constraints(constraints));
        },
        .should_treat_max_block_size_as_none = [](void* context, void* box, RustFFI::AvailableSize available_size, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.should_treat_max_block_size_as_none(
                *static_cast<Box const*>(box),
                from_ffi_available_size(available_size),
                from_ffi_constraints(constraints));
        },
        .create_measurement_state = [](void*, void* box, RustFFI::FfiContainingBlockConstraints constraints) {
            auto converted_constraints = from_ffi_constraints(constraints);
            auto state = make<LayoutState>(*static_cast<Box const*>(box), LayoutState::Purpose::Measurement);
            auto& root_used_values = state->create(
                *static_cast<Box const*>(box),
                converted_constraints.percentage_basis_inline_size,
                converted_constraints.percentage_basis_block_size);
            auto* rust_state = state->rust_state_handle();
            return RustFFI::FfiMeasurementState {
                .cpp_state = state.leak_ptr(),
                .rust_state = rust_state,
                .root_used_values = &root_used_values.core(),
            };
        },
        .destroy_measurement_state = [](void*, void* state) {
            auto* measurement_state = static_cast<LayoutState*>(state);
            VERIFY(measurement_state);
            VERIFY(measurement_state->is_for_measurement());
            delete measurement_state;
        },
        .run_measurement_context = [](void* context, void* state, void* box, RustFFI::FfiLayoutInput input) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& measurement_state = *static_cast<LayoutState*>(state);
            VERIFY(measurement_state.is_for_measurement());
            auto measuring_context = FormattingContext::create_independent_formatting_context(
                measurement_state,
                LayoutMode::IntrinsicSizing,
                *static_cast<Box const*>(box),
                &bridge.m_formatting_context);
            measuring_context->run(from_ffi(input));
            return RustFFI::FfiChildLayoutResult {
                .automatic_content_inline_size = measuring_context->automatic_content_inline_size().raw_value(),
                .automatic_content_block_size = measuring_context->automatic_content_block_size().raw_value(),
            };
        },
        .intrinsic_size_cache_get = [](void*, void* box, RustFFI::FfiIntrinsicSizeCacheKind kind, RustFFI::FfiIntrinsicSizeCacheKey key, i32* out) {
            auto to_optional = [](bool has_value, i32 value) -> Optional<CSSPixels> {
                return has_value ? Optional<CSSPixels> { CSSPixels::from_raw(value) } : Optional<CSSPixels> {};
            };
            auto converted_key = IntrinsicSizeCacheKey {
                .measured_at_inline_size = to_optional(key.has_measured_at_inline_size, key.measured_at_inline_size),
                .percentage_basis_inline_size = to_optional(key.has_percentage_basis_inline_size, key.percentage_basis_inline_size),
                .percentage_basis_block_size = to_optional(key.has_percentage_basis_block_size, key.percentage_basis_block_size),
                .quirks_mode_percentage_basis_block_size = to_optional(key.has_quirks_mode_percentage_basis_block_size, key.quirks_mode_percentage_basis_block_size),
            };
            auto& sizes = static_cast<Box const*>(box)->cached_intrinsic_sizes();
            Optional<CSSPixels> cached;
            switch (kind) {
            case RustFFI::FfiIntrinsicSizeCacheKind::MinContentInline:
                cached = sizes.min_content_inline_size.get(converted_key);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MaxContentInline:
                cached = sizes.max_content_inline_size.get(converted_key);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MinContentBlock:
                cached = sizes.min_content_block_size.get(converted_key);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MaxContentBlock:
                cached = sizes.max_content_block_size.get(converted_key);
                break;
            }
            if (!cached.has_value())
                return false;
            *out = cached->raw_value();
            return true;
        },
        .intrinsic_size_cache_put = [](void*, void* box, RustFFI::FfiIntrinsicSizeCacheKind kind, RustFFI::FfiIntrinsicSizeCacheKey key, i32 value) {
            auto to_optional = [](bool has_value, i32 raw_value) -> Optional<CSSPixels> {
                return has_value ? Optional<CSSPixels> { CSSPixels::from_raw(raw_value) } : Optional<CSSPixels> {};
            };
            auto converted_key = IntrinsicSizeCacheKey {
                .measured_at_inline_size = to_optional(key.has_measured_at_inline_size, key.measured_at_inline_size),
                .percentage_basis_inline_size = to_optional(key.has_percentage_basis_inline_size, key.percentage_basis_inline_size),
                .percentage_basis_block_size = to_optional(key.has_percentage_basis_block_size, key.percentage_basis_block_size),
                .quirks_mode_percentage_basis_block_size = to_optional(key.has_quirks_mode_percentage_basis_block_size, key.quirks_mode_percentage_basis_block_size),
            };
            auto& sizes = static_cast<Box const*>(box)->cached_intrinsic_sizes();
            auto converted_value = CSSPixels::from_raw(value);
            switch (kind) {
            case RustFFI::FfiIntrinsicSizeCacheKind::MinContentInline:
                sizes.min_content_inline_size.set(converted_key, converted_value);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MaxContentInline:
                sizes.max_content_inline_size.set(converted_key, converted_value);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MinContentBlock:
                sizes.min_content_block_size.set(converted_key, converted_value);
                break;
            case RustFFI::FfiIntrinsicSizeCacheKind::MaxContentBlock:
                sizes.max_content_block_size.set(converted_key, converted_value);
                break;
            }
        },
        .compute_table_box_block_size_inside_wrapper = [](void* context, void* box, RustFFI::AvailableSpace available_space, RustFFI::FfiContainingBlockConstraints constraints) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            return bridge.m_formatting_context.compute_table_box_block_size_inside_table_wrapper(
                *static_cast<Box const*>(box),
                {
                    from_ffi_available_size(available_space.inline_size),
                    from_ffi_available_size(available_space.block_size),
                },
                from_ffi_constraints(constraints))
                .raw_value();
        },
        .register_contained_abspos_child = [](void* context, void* child, RustFFI::FfiStaticPositionRect rect) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.register_contained_abspos_child(
                *static_cast<Box const*>(child),
                from_ffi_static_position_rect(rect));
        },
        .compute_inset = [](void* context, void* box, i32 inline_size, i32 block_size) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_formatting_context.compute_inset(
                *static_cast<Box const*>(box),
                { CSSPixels::from_raw(inline_size), CSSPixels::from_raw(block_size) });
        },
        .set_flex_layout_data = [](void* context, void* box, RustFFI::FfiFlexLayoutData const* ffi_data) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            VERIFY(ffi_data);
            auto data = make<FlexLayoutData>();
            data->align_content = static_cast<CSS::AlignContent>(ffi_data->align_content);
            data->align_items = static_cast<CSS::AlignItems>(ffi_data->align_items);
            data->flex_direction = static_cast<CSS::FlexDirection>(ffi_data->flex_direction);
            data->flex_wrap = static_cast<CSS::FlexWrap>(ffi_data->flex_wrap);
            data->justify_content = static_cast<CSS::JustifyContent>(ffi_data->justify_content);

            auto axis_direction = [](u8 direction) -> String {
                switch (direction) {
                case 0:
                    return "horizontal-lr"_string;
                case 1:
                    return "horizontal-rl"_string;
                case 2:
                    return "vertical-tb"_string;
                case 3:
                    return "vertical-bt"_string;
                default:
                    VERIFY_NOT_REACHED();
                }
            };
            auto main_axis_direction = axis_direction(ffi_data->main_axis_direction);
            auto cross_axis_direction = axis_direction(ffi_data->cross_axis_direction);
            bool main_axis_is_horizontal = ffi_data->main_axis_direction <= 1;

            for (size_t line_index = 0; line_index < ffi_data->line_count; ++line_index) {
                auto const& ffi_line = ffi_data->lines[line_index];
                FlexLayoutLine line;
                line.growth_state = static_cast<FlexLayoutGrowthState>(ffi_line.growth_state);
                line.cross_start = CSSPixels::from_raw(ffi_line.cross_start);
                line.cross_size = CSSPixels::from_raw(ffi_line.cross_size);
                for (size_t item_index = 0; item_index < ffi_line.item_count; ++item_index) {
                    auto const& ffi_item = ffi_line.items[item_index];
                    auto const& item_box = *static_cast<Box const*>(ffi_item.node);
                    auto const& values = item_box.computed_values();
                    auto const& flex_basis = values.flex_basis();
                    auto const& main_size = main_axis_is_horizontal ? values.width() : values.height();
                    auto const& main_min_size = main_axis_is_horizontal ? values.min_width() : values.min_height();
                    auto const& main_max_size = main_axis_is_horizontal ? values.max_width() : values.max_height();

                    FlexLayoutItem item;
                    if (auto* dom_node = item_box.dom_node())
                        item.node_id = dom_node->unique_id();
                    item.main_axis_direction = main_axis_direction;
                    item.cross_axis_direction = cross_axis_direction;
                    item.rect = {
                        CSSPixels::from_raw(ffi_item.rect.x),
                        CSSPixels::from_raw(ffi_item.rect.y),
                        CSSPixels::from_raw(ffi_item.rect.width),
                        CSSPixels::from_raw(ffi_item.rect.height),
                    };
                    item.main_base_size = CSSPixels::from_raw(ffi_item.main_base_size);
                    item.main_delta_size = CSSPixels::from_raw(ffi_item.main_delta_size);
                    item.main_min_size = CSSPixels::from_raw(ffi_item.main_min_size);
                    item.main_max_size = CSSPixels::from_raw(ffi_item.main_max_size);
                    item.cross_min_size = CSSPixels::from_raw(ffi_item.cross_min_size);
                    item.cross_max_size = CSSPixels::from_raw(ffi_item.cross_max_size);
                    item.clamp_state = static_cast<FlexLayoutClampState>(ffi_item.clamp_state);
                    item.flex_basis = flex_basis.has<CSS::FlexBasisContent>()
                        ? "content"_string
                        : MUST(String::formatted("{}", flex_basis.get<CSS::Size>()));
                    item.main_size_property = MUST(String::formatted("{}", main_size));
                    item.main_min_size_property = MUST(String::formatted("{}", main_min_size));
                    item.main_max_size_property = MUST(String::formatted("{}", main_max_size));
                    item.flex_grow = ffi_item.flex_grow;
                    item.flex_shrink = ffi_item.flex_shrink;
                    line.items.append(move(item));
                }
                data->lines.append(move(line));
            }
            bridge.m_formatting_context.m_state.get_mutable(*static_cast<Box const*>(box)).set_flex_layout_data(move(data));
        },
    };
}

Node const* LayoutRustBridge::parent(Node const& node) const
{
    return node.parent();
}

Node const* LayoutRustBridge::first_child(Node const& node) const
{
    return node.first_child();
}

Node const* LayoutRustBridge::next_sibling(Node const& node) const
{
    return node.next_sibling();
}

Node const* LayoutRustBridge::previous_sibling(Node const& node) const
{
    return node.previous_sibling();
}

Box const* LayoutRustBridge::containing_block(Node const& node) const
{
    return node.containing_block();
}

RustFFI::FfiStyleFacts build_style_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_style_facts_build();
    auto const& values = node.computed_values();

    auto vertical_align = build_style_vertical_align_value(values.vertical_align());

    auto flex_basis_is_content = values.flex_basis().has<CSS::FlexBasisContent>();
    auto flex_basis = flex_basis_is_content
        ? size_value_with_kind(RustFFI::FfiSizeKind::Auto)
        : build_style_size_value(values.flex_basis().get<CSS::Size>());

    auto row_gap = values.row_gap().visit(
        [](CSS::LengthPercentage const& gap) { return build_style_size_value(gap); },
        [](CSS::NormalGap const&) { return size_value_with_kind(RustFFI::FfiSizeKind::Auto); });
    auto column_gap = values.column_gap().visit(
        [](CSS::LengthPercentage const& gap) { return build_style_size_value(gap); },
        [](CSS::NormalGap const&) { return size_value_with_kind(RustFFI::FfiSizeKind::Auto); });

    auto aspect_ratio = values.aspect_ratio().preferred_ratio;
    auto column_count = values.column_count();
    auto containment = values.contain();
    auto container_type = values.container_type();
    auto text_indent = values.text_indent();
    auto tab_size = values.tab_size();
    auto grid_auto_flow = values.grid_auto_flow();

    return {
        .width = build_style_size_value(values.width()),
        .height = build_style_size_value(values.height()),
        .min_width = build_style_size_value(values.min_width()),
        .min_height = build_style_size_value(values.min_height()),
        .max_width = build_style_size_value(values.max_width()),
        .max_height = build_style_size_value(values.max_height()),
        .margin_top = build_style_size_value(values.margin().top()),
        .margin_right = build_style_size_value(values.margin().right()),
        .margin_bottom = build_style_size_value(values.margin().bottom()),
        .margin_left = build_style_size_value(values.margin().left()),
        .padding_top = build_style_size_value(values.padding().top()),
        .padding_right = build_style_size_value(values.padding().right()),
        .padding_bottom = build_style_size_value(values.padding().bottom()),
        .padding_left = build_style_size_value(values.padding().left()),
        .inset_top = build_style_size_value(values.inset().top()),
        .inset_right = build_style_size_value(values.inset().right()),
        .inset_bottom = build_style_size_value(values.inset().bottom()),
        .inset_left = build_style_size_value(values.inset().left()),
        .border_top_width = values.border_top().width.raw_value(),
        .border_right_width = values.border_right().width.raw_value(),
        .border_bottom_width = values.border_bottom().width.raw_value(),
        .border_left_width = values.border_left().width.raw_value(),
        .border_top_style = to_underlying(values.border_top().line_style),
        .border_right_style = to_underlying(values.border_right().line_style),
        .border_bottom_style = to_underlying(values.border_bottom().line_style),
        .border_left_style = to_underlying(values.border_left().line_style),
        .display = encode_display(values.display()),
        .position = to_underlying(values.position()),
        .float_ = to_underlying(values.float_()),
        .clear = to_underlying(values.clear()),
        .writing_mode = to_underlying(values.writing_mode()),
        .direction = to_underlying(values.direction()),
        .text_align = to_underlying(values.text_align()),
        .text_justify = to_underlying(values.text_justify()),
        .white_space_collapse = to_underlying(values.white_space_collapse()),
        .text_wrap_mode = to_underlying(values.text_wrap_mode()),
        .vertical_align_is_keyword = vertical_align.is_keyword,
        .vertical_align_keyword = vertical_align.keyword,
        .vertical_align_value = vertical_align.value,
        .line_height = values.line_height().raw_value(),
        .font_size = values.font_size().raw_value(),
        .box_sizing = to_underlying(values.box_sizing()),
        .box_sizing_for_aspect_ratio = to_underlying(values.box_sizing_for_aspect_ratio()),
        .overflow_x = to_underlying(values.overflow_x()),
        .overflow_y = to_underlying(values.overflow_y()),
        .flex_direction = to_underlying(values.flex_direction()),
        .flex_wrap = to_underlying(values.flex_wrap()),
        .flex_grow = values.flex_grow(),
        .flex_shrink = values.flex_shrink(),
        .flex_basis_is_content = flex_basis_is_content,
        .flex_basis = flex_basis,
        .order = values.order(),
        .align_items = to_underlying(values.align_items()),
        .align_self = to_underlying(values.align_self()),
        .align_content = to_underlying(values.align_content()),
        .justify_content = to_underlying(values.justify_content()),
        .justify_items = to_underlying(values.justify_items()),
        .justify_self = to_underlying(values.justify_self()),
        .row_gap = row_gap,
        .column_gap = column_gap,
        .has_aspect_ratio = aspect_ratio.has_value(),
        .aspect_ratio_width = aspect_ratio.has_value() ? aspect_ratio->numerator() : 0,
        .aspect_ratio_height = aspect_ratio.has_value() ? aspect_ratio->denominator() : 0,
        .aspect_ratio_is_degenerate = aspect_ratio.has_value() && aspect_ratio->is_degenerate(),
        .appearance = to_underlying(values.appearance()),
        .border_collapse = to_underlying(values.border_collapse()),
        .border_spacing_horizontal = values.border_spacing_horizontal().raw_value(),
        .border_spacing_vertical = values.border_spacing_vertical().raw_value(),
        .caption_side = to_underlying(values.caption_side()),
        .table_layout = to_underlying(values.table_layout()),
        .column_width = build_style_size_value(values.column_width()),
        .has_column_count = !column_count.is_auto(),
        .column_count = column_count.is_auto() ? 0 : column_count.value(),
        .containment_bits = static_cast<u8>(static_cast<u8>(containment.size_containment)
            | static_cast<u8>(containment.inline_size_containment) << 1
            | static_cast<u8>(containment.layout_containment) << 2
            | static_cast<u8>(containment.style_containment) << 3
            | static_cast<u8>(containment.paint_containment) << 4),
        .container_type_bits = static_cast<u8>(static_cast<u8>(container_type.is_size_container)
            | static_cast<u8>(container_type.is_inline_size_container) << 1
            | static_cast<u8>(container_type.is_scroll_state_container) << 2),
        .content_visibility = to_underlying(values.content_visibility()),
        .visibility = to_underlying(values.visibility()),
        .word_break = to_underlying(values.word_break()),
        .has_z_index = values.z_index().has_value(),
        .z_index = values.z_index().value_or(0),
        .font_variant_emoji = to_underlying(values.font_variant_emoji()),
        .letter_spacing = values.letter_spacing().raw_value(),
        .word_spacing = values.word_spacing().raw_value(),
        .unicode_bidi = to_underlying(values.unicode_bidi()),
        .text_transform = to_underlying(values.text_transform()),
        .text_indent = build_style_size_value(text_indent.length_percentage),
        .text_indent_each_line = text_indent.each_line,
        .text_indent_hanging = text_indent.hanging,
        .tab_size_is_number = tab_size.has<double>(),
        .tab_size = tab_size.has<CSSPixels>() ? tab_size.get<CSSPixels>().raw_value() : 0,
        .tab_size_number = tab_size.has<double>() ? tab_size.get<double>() : 0,
        .grid_auto_flow_row = grid_auto_flow.row,
        .grid_auto_flow_dense = grid_auto_flow.dense,
        .x = build_style_size_value(values.x()),
        .y = build_style_size_value(values.y()),
        .user_select = to_underlying(values.user_select()),
        .opacity = values.opacity(),
        .isolation = to_underlying(values.isolation()),
        .mix_blend_mode = to_underlying(values.mix_blend_mode()),
        .transform_style = to_underlying(values.transform_style()),
        .has_perspective = values.perspective().has_value(),
        .perspective = values.perspective().value_or(0).raw_value(),
        .list_style_position = to_underlying(values.list_style_position()),
        .text_decoration_style = to_underlying(values.text_decoration_style()),
    };
}

static void release_calc_handle(void const* handle)
{
    if (!handle)
        return;
    auto iterator = retained_calc_handles().find(handle);
    VERIFY(iterator != retained_calc_handles().end());
    auto const* calculated = iterator->value.style_value;
    VERIFY(iterator->value.retain_count > 0);
    if (--iterator->value.retain_count == 0)
        retained_calc_handles().remove(handle);
    VERIFY(s_outstanding_calc_handles.load() > 0);
    --s_outstanding_calc_handles;
    RustFFI::rust_layout_ffi_note_calc_handle_release();
    calculated->unref();
}

void verify_style_calc_handles_balanced()
{
    VERIFY(s_outstanding_calc_handles.load() == 0);
    VERIFY(retained_calc_handles().is_empty());
}

void release_style_facts(RustFFI::FfiStyleFacts const& facts)
{
    auto release = [](RustFFI::FfiSizeValue const& value) {
        ladybird_layout_release_calc_handle(value.calc);
    };
    release(facts.width);
    release(facts.height);
    release(facts.min_width);
    release(facts.min_height);
    release(facts.max_width);
    release(facts.max_height);
    release(facts.margin_top);
    release(facts.margin_right);
    release(facts.margin_bottom);
    release(facts.margin_left);
    release(facts.padding_top);
    release(facts.padding_right);
    release(facts.padding_bottom);
    release(facts.padding_left);
    release(facts.inset_top);
    release(facts.inset_right);
    release(facts.inset_bottom);
    release(facts.inset_left);
    release(facts.vertical_align_value);
    release(facts.flex_basis);
    release(facts.row_gap);
    release(facts.column_gap);
    release(facts.column_width);
    release(facts.text_indent);
    release(facts.x);
    release(facts.y);
}

}

extern "C" WEB_API void ladybird_layout_release_calc_handle(void const* handle)
{
    Web::Layout::release_calc_handle(handle);
}

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_size_value(u8 kind, i32 px_raw, double fraction)
{
    using namespace Web;
    using Layout::RustFFI::FfiSizeKind;
    switch (static_cast<FfiSizeKind>(kind)) {
    case FfiSizeKind::Auto:
        return Layout::build_style_size_value(CSS::Size::make_auto());
    case FfiSizeKind::Px:
        return Layout::build_style_size_value(CSS::Size::make_px(CSSPixels::from_raw(px_raw)));
    case FfiSizeKind::Percentage:
        return Layout::build_style_size_value(CSS::Size::make_percentage(CSS::Percentage { fraction * 100 }));
    case FfiSizeKind::MinContent:
        return Layout::build_style_size_value(CSS::Size::make_min_content());
    case FfiSizeKind::MaxContent:
        return Layout::build_style_size_value(CSS::Size::make_max_content());
    case FfiSizeKind::FitContent:
        return Layout::build_style_size_value(CSS::Size::make_fit_content());
    case FfiSizeKind::None_:
        return Layout::build_style_size_value(CSS::Size::make_none());
    case FfiSizeKind::Calc:
        VERIFY_NOT_REACHED();
    }
    VERIFY_NOT_REACHED();
}

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_auto_margin_value()
{
    return Web::Layout::build_style_size_value(Web::CSS::LengthPercentageOrAuto::make_auto());
}

extern "C" WEB_API Web::Layout::StyleVerticalAlignFacts ladybird_layout_test_build_vertical_align(bool is_keyword, u8 keyword, i32 px_raw)
{
    using namespace Web;
    if (is_keyword)
        return Layout::build_style_vertical_align_value(static_cast<CSS::VerticalAlign>(keyword));
    return Layout::build_style_vertical_align_value(CSS::LengthPercentage { CSS::Length::make_px(CSSPixels::from_raw(px_raw)) });
}

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_calc_size_value()
{
    using namespace Web;
    CSS::CalculationContext calculation_context {
        .percentages_resolve_as = CSS::ValueType::Length,
    };
    Vector<CSS::CalcNodeRef> terms;
    terms.append(CSS::CalcNodeRef::numeric(CSS::Length::make_px(10)));
    terms.append(CSS::CalcNodeRef::numeric(CSS::Percentage { 25 }));
    auto root = CSS::CalcNodeRef::sum(move(terms));
    auto numeric_type = root.determine_type(calculation_context);
    VERIFY(numeric_type.has_value());
    auto calculated = CSS::CalculatedStyleValue::create(move(root), numeric_type.release_value(), calculation_context);
    return Layout::build_style_size_value(CSS::Size::make_calculated(*calculated));
}

extern "C" WEB_API i32 ladybird_layout_test_resolve_calc_handle_cpp(void const* handle, i32 percentage_basis_raw)
{
    using namespace Web;
    auto iterator = Layout::retained_calc_handles().find(handle);
    VERIFY(iterator != Layout::retained_calc_handles().end());
    auto length_percentage = CSS::LengthPercentage { NonnullRefPtr<CSS::CalculatedStyleValue const> { *iterator->value.style_value } };
    return length_percentage.to_px(CSSPixels::from_raw(percentage_basis_raw)).raw_value();
}

extern "C" WEB_API void ladybird_layout_test_verify_calc_handles_balanced()
{
    Web::Layout::verify_style_calc_handles_balanced();
}
