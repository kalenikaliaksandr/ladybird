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
#include <LibWeb/Layout/LayoutRustBridge.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/Node.h>

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
    };
}

static RustFFI::FfiSizeValue retain_calculated(CSS::CalculatedStyleValue const& calculated, RustFFI::FfiSizeKind kind = RustFFI::FfiSizeKind::Calc)
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
        };
    }
    if (value.is_percentage()) {
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
        };
    }
    return retain_calculated(*value.calculated());
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
        return retain_calculated(value.calculated());
    case CSS::Size::Type::Length:
        VERIFY(value.length().is_absolute());
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Px),
            .px = value.length().absolute_length_to_px().raw_value(),
            .fraction = 0,
            .calc = nullptr,
        };
    case CSS::Size::Type::Percentage:
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
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
    auto display = node.display();
    auto const* dom_node = node.dom_node();

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
        .is_html_html_element = dom_node && dom_node->is_html_html_element(),
        .is_html_body_element = dom_node && dom_node->is_html_body_element(),
    };
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
