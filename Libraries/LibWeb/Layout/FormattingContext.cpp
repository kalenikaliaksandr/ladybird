/*
 * Copyright (c) 2020-2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/ComputedProperties.h>
#include <LibWeb/CSS/StyleValues/CalculatedStyleValue.h>
#include <LibWeb/CSS/StyleValues/KeywordStyleValue.h>
#include <LibWeb/CSS/StyleValues/PercentageStyleValue.h>
#include <LibWeb/DOM/AbstractElement.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/Dump.h>
#include <LibWeb/HTML/HTMLElement.h>
#include <LibWeb/HTML/HTMLInputElement.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/FormattingContext.h>
#include <LibWeb/Layout/ReplacedBox.h>
#include <LibWeb/Layout/ReplacedWithChildrenFormattingContext.h>
#include <LibWeb/Layout/RustFormattingContext.h>
#include <LibWeb/Layout/SVGSVGBox.h>
#include <LibWeb/Layout/TableWrapper.h>
#include <LibWeb/Layout/TextInputBox.h>
#include <LibWeb/Layout/TextNode.h>
#include <LibWeb/Layout/Viewport.h>

namespace Web::Layout {

enum class SizeDimension {
    Inline,
    Block,
};

// NB: Intrinsic grid sizing can transfer an aspect ratio before block-axis border metrics are copied into the layout
//     state. Border widths are already definite at computed-value time, while padding remains resolved in the state.
static CSSPixels content_block_size_from_aspect_ratio(Box const& box, LayoutState::UsedValues const& box_state, CSSPixels content_inline_size)
{
    VERIFY(box.has_preferred_aspect_ratio());

    auto aspect_ratio = *box.preferred_aspect_ratio();
    if (aspect_ratio == 0)
        return 0;

    if (box.computed_values().box_sizing_for_aspect_ratio() == CSS::BoxSizing::BorderBox) {
        auto const& computed_values = box.computed_values();
        auto border_box_left = computed_values.border_left().width + box_state.padding_left();
        auto border_box_right = computed_values.border_right().width + box_state.padding_right();
        auto border_box_top = computed_values.border_top().width + box_state.padding_top();
        auto border_box_bottom = computed_values.border_bottom().width + box_state.padding_bottom();
        auto border_box_inline_size = content_inline_size + border_box_left + border_box_right;
        auto border_box_block_size = border_box_inline_size / aspect_ratio;
        return max(border_box_block_size - border_box_top - border_box_bottom, 0);
    }

    return content_inline_size / aspect_ratio;
}

static CSSPixels content_block_size_from_aspect_ratio(Box const& box, LayoutState::UsedValues const& box_state)
{
    return content_block_size_from_aspect_ratio(box, box_state, box_state.content_inline_size());
}

static CSSPixels content_inline_size_from_aspect_ratio(Box const& box, LayoutState::UsedValues const& box_state, CSSPixels content_block_size)
{
    VERIFY(box.has_preferred_aspect_ratio());

    auto aspect_ratio = *box.preferred_aspect_ratio();
    if (aspect_ratio == 0)
        return 0;

    if (box.computed_values().box_sizing_for_aspect_ratio() == CSS::BoxSizing::BorderBox) {
        auto const& computed_values = box.computed_values();
        auto border_box_left = computed_values.border_left().width + box_state.padding_left();
        auto border_box_right = computed_values.border_right().width + box_state.padding_right();
        auto border_box_top = computed_values.border_top().width + box_state.padding_top();
        auto border_box_bottom = computed_values.border_bottom().width + box_state.padding_bottom();
        auto border_box_block_size = content_block_size + border_box_top + border_box_bottom;
        auto border_box_inline_size = border_box_block_size * aspect_ratio;
        return max(border_box_inline_size - border_box_left - border_box_right, 0);
    }

    return content_block_size * aspect_ratio;
}

struct ReplacedMaxContentSizeConstraints {
    Optional<CSSPixels> definite_size_in_ratio_determining_axis;
    Optional<CSSPixels> minimum_inline_size;
    Optional<CSSPixels> minimum_block_size;
};

// https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
static Optional<CSSPixels> max_content_size_for_replaced_element_without_natural_size(Box const& box, CSS::SizeWithAspectRatio const& natural_size, LayoutState::UsedValues const& box_state, SizeDimension dimension, ReplacedMaxContentSizeConstraints const& constraints = {})
{
    // the intrinsic sizes of replaced elements without natural sizes are defined below:
    auto is_inline_axis = dimension == SizeDimension::Inline;
    if (!box.is_replaced_box() || (is_inline_axis ? natural_size.has_width() : natural_size.has_height()))
        return {};

    // SVG Integration says that a non-top-level <svg> starts with auto width/height, and that with a viewBox, missing
    // width/height attributes "keep" their auto value. The resulting width, height, and aspect ratio are then
    // "used in CSS sizing as intrinsic element size properties".
    //
    // CSS Sizing defines max-content as the size the box would have "if it was a float" with an auto preferred size.
    // CSS2 replaced sizing then resolves auto width from "(used height) * (intrinsic ratio)", and auto height from
    // "(used width) / (intrinsic ratio)". Keep this SVG specific bridge before falling through to CSS Sizing's fallback
    // for replaced elements without natural sizes.
    //  - https://svgwg.org/specs/integration/#svg-css-sizing
    //  - https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
    //  - https://drafts.csswg.org/css2/#inline-replaced-width
    //  - https://drafts.csswg.org/css2/#inline-replaced-height
    if (box.is_svg_svg_box() && natural_size.has_aspect_ratio()) {
        if (is_inline_axis && natural_size.has_height())
            return natural_size.height.value() * natural_size.aspect_ratio.value();
        if (!is_inline_axis && natural_size.has_width())
            return natural_size.width.value() / natural_size.aspect_ratio.value();
    }

    // For the max-content size:
    // If it has a preferred aspect ratio:
    if (box.has_preferred_aspect_ratio()) {
        // If the available space is definite in the inline axis, use the stretch fit into that size for the inline size
        // and calculate the block size using the aspect ratio.
        //
        // NB: This helper is only for the max-content size, which has no definite available inline size. Callers may
        //     still know a definite used size in the opposite axis when the box lacks a natural size in that axis.
        if (constraints.definite_size_in_ratio_determining_axis.has_value())
            return is_inline_axis
                ? content_inline_size_from_aspect_ratio(box, box_state, constraints.definite_size_in_ratio_determining_axis.value())
                : content_block_size_from_aspect_ratio(box, box_state, constraints.definite_size_in_ratio_determining_axis.value());

        // Otherwise if the box has a <length> as its computed value for min-width or min-height, use that size and
        // calculate the other dimension using the aspect ratio; if both dimensions have a <length> minimum, choose the
        // one that results in the larger overall size.
        //
        // NOTE: This case was previous calculated from a 300x150 default size, rather than the box’s min size. This is
        //       believed to be a better behavior, and likely to be Web-compatible, but please send feedback to the CSSWG
        //       if there are any problems.
        Optional<CSSPixels> size_from_min_inline_size;
        if (constraints.minimum_inline_size.has_value()) {
            auto inline_size = constraints.minimum_inline_size.value();
            size_from_min_inline_size = is_inline_axis ? inline_size : content_block_size_from_aspect_ratio(box, box_state, inline_size);
        } else {
            auto const& min_inline_size = box.computed_values().min_width();
            if (min_inline_size.is_length_percentage() && !min_inline_size.contains_percentage()) {
                auto inline_size = min_inline_size.to_px(0);
                size_from_min_inline_size = is_inline_axis ? inline_size : content_block_size_from_aspect_ratio(box, box_state, inline_size);
            }
        }

        Optional<CSSPixels> size_from_min_block_size;
        if (constraints.minimum_block_size.has_value()) {
            auto block_size = constraints.minimum_block_size.value();
            size_from_min_block_size = is_inline_axis ? content_inline_size_from_aspect_ratio(box, box_state, block_size) : block_size;
        } else {
            auto const& min_block_size = box.computed_values().min_height();
            if (min_block_size.is_length_percentage() && !min_block_size.contains_percentage()) {
                auto block_size = min_block_size.to_px(0);
                size_from_min_block_size = is_inline_axis ? content_inline_size_from_aspect_ratio(box, box_state, block_size) : block_size;
            }
        }

        if (size_from_min_inline_size.has_value() && size_from_min_block_size.has_value())
            return max(size_from_min_inline_size.value(), size_from_min_block_size.value());
        if (size_from_min_inline_size.has_value())
            return size_from_min_inline_size.value();
        if (size_from_min_block_size.has_value())
            return size_from_min_block_size.value();

        // Otherwise use an inline size matching the corresponding dimension of the initial containing block and calculate
        // the other dimension using the aspect ratio.
        //
        // NOTE: This author-controllable behavior is made possible by the new auto value for the min size properties.
        //       This is believed to be a better behavior, but it is not yet clear if it is Web-compatible, so please
        //       send feedback to the CSSWG if there are any problems.
        auto initial_containing_block_inline_size = box.document().viewport_rect().width();
        return is_inline_axis ? initial_containing_block_inline_size : content_block_size_from_aspect_ratio(box, box_state, initial_containing_block_inline_size);
    }

    // If it has no preferred aspect ratio:
    // For both the min-content size and max-content size:
    // If the box has a <length> as its computed minimum size (min-width/min-height) in that dimension, use that size.
    auto const& min_size = is_inline_axis ? box.computed_values().min_width() : box.computed_values().min_height();
    if (min_size.is_length_percentage() && !min_size.contains_percentage())
        return min_size.to_px(0);

    // Otherwise, use 300px for the width and/or 150px for the height as needed.
    return is_inline_axis ? CSSPixels(300) : CSSPixels(150);
}

static bool is_text_control_input(HTML::HTMLInputElement const& input_element)
{
    switch (input_element.type_state()) {
    case HTML::HTMLInputElement::TypeAttributeState::Text:
    case HTML::HTMLInputElement::TypeAttributeState::Search:
    case HTML::HTMLInputElement::TypeAttributeState::URL:
    case HTML::HTMLInputElement::TypeAttributeState::Telephone:
    case HTML::HTMLInputElement::TypeAttributeState::Email:
    case HTML::HTMLInputElement::TypeAttributeState::Password:
    case HTML::HTMLInputElement::TypeAttributeState::Number:
        return true;
    default:
        return false;
    }
}

static Optional<CSS::SizeWithAspectRatio> default_preferred_size_for_appearance_none_text_input(Box const& box)
{
    if (box.computed_values().appearance() != CSS::Appearance::None)
        return {};

    auto const* input_element = as_if<HTML::HTMLInputElement>(box.dom_node());
    if (!input_element || !is_text_control_input(*input_element))
        return {};

    // https://drafts.csswg.org/css-ui-4/#appearance-switching
    // The element is rendered following the usual rules of CSS. Replaced elements other than widgets are not affected
    // by this and remain replaced elements. Widgets must not have their native appearance, and instead must have their
    // primitive appearance.
    //
    // https://html.spec.whatwg.org/multipage/rendering.html#the-input-element-as-a-text-entry-widget
    // An input element whose type attribute is in one of the above states is an element with default preferred size,
    // and user agents are expected to apply the 'field-sizing' CSS property to the element.
    return TextInputBox::default_preferred_size_for_text_control(*input_element, box);
}

static CSS::SizeWithAspectRatio intrinsic_size_for_replaced_sizing(Box const& box)
{
    auto intrinsic_size = box.auto_content_box_size();
    if (intrinsic_size.has_width() || intrinsic_size.has_height() || intrinsic_size.has_aspect_ratio())
        return intrinsic_size;

    return default_preferred_size_for_appearance_none_text_input(box).value_or(intrinsic_size);
}

FormattingContext::FormattingContext(Type type, LayoutMode layout_mode, LayoutState& state, Box const& context_box, FormattingContext* parent)
    : m_type(type)
    , m_layout_mode(layout_mode)
    , m_parent(parent)
    , m_context_box(context_box)
    , m_state(state)
{
}

FormattingContext::~FormattingContext() = default;

void FormattingContext::place_child(Box const& child, CSSPixelPoint content_offset)
{
    m_state.get_mutable(child).place(content_offset);
}

// https://developer.mozilla.org/en-US/docs/Web/Guide/CSS/Block_formatting_context
bool FormattingContext::creates_block_formatting_context(Box const& box)
{
    // NOTE: Replaced elements never create a BFC.
    if (box.is_replaced_box())
        return false;

    // AD-HOC: We create a BFC for SVG foreignObject.
    if (box.is_svg_foreign_object_box())
        return true;

    // display: table
    if (box.display().is_table_inside()) {
        return false;
    }

    // display: flex
    if (box.display().is_flex_inside()) {
        return false;
    }

    // display: grid
    if (box.display().is_grid_inside()) {
        return false;
    }

    // NOTE: This function uses MDN as a reference, not because it's authoritative,
    //       but because they've gathered all the conditions in one convenient location.

    // The root element of the document (<html>).
    if (box.is_root_element())
        return true;

    // Floats (elements where float isn't none).
    if (box.is_floating())
        return true;

    // Absolutely positioned elements (elements where position is absolute or fixed).
    if (box.is_absolutely_positioned())
        return true;

    // Inline-blocks (elements with display: inline-block).
    if (box.display().is_inline_block())
        return true;

    // Table cells (elements with display: table-cell, which is the default for HTML table cells).
    if (box.display().is_table_cell())
        return true;

    // Table captions (elements with display: table-caption, which is the default for HTML table captions).
    if (box.display().is_table_caption())
        return true;

    // FIXME: Anonymous table cells implicitly created by the elements with display: table, table-row, table-row-group, table-header-group, table-footer-group
    //        (which is the default for HTML tables, table rows, table bodies, table headers, and table footers, respectively), or inline-table.

    // Block elements where overflow has a value other than visible and clip.
    CSS::Overflow overflow_x = box.computed_values().overflow_x();
    if ((overflow_x != CSS::Overflow::Visible) && (overflow_x != CSS::Overflow::Clip))
        return true;
    CSS::Overflow overflow_y = box.computed_values().overflow_y();
    if ((overflow_y != CSS::Overflow::Visible) && (overflow_y != CSS::Overflow::Clip))
        return true;

    // display: flow-root.
    if (box.display().is_flow_root_inside())
        return true;

    // https://drafts.csswg.org/css-contain-2/#containment-types
    // 1. The layout containment box establishes an independent formatting context.
    // 4. The paint containment box establishes an independent formatting context.
    if (box.has_layout_containment() || box.has_paint_containment())
        return true;

    // https://drafts.csswg.org/css-conditional-5/#valdef-container-type-size
    // Applies style containment and size containment to the principal box, and establishes an independent formatting
    // context.
    if (box.computed_values().container_type().is_size_container || box.computed_values().container_type().is_inline_size_container)
        return true;

    if (box.parent()) {
        auto parent_display = box.parent()->display();

        // Flex items (direct children of the element with display: flex or inline-flex) if they are neither flex nor grid nor table containers themselves.
        if (parent_display.is_flex_inside())
            return true;
        // Grid items (direct children of the element with display: grid or inline-grid) if they are neither flex nor grid nor table containers themselves.
        if (parent_display.is_grid_inside())
            return true;
    }

    // https://drafts.csswg.org/css-multicol-2/#the-multi-column-model
    // An element whose 'column-width', 'column-count', or 'column-height' property is not 'auto' establishes a multi-
    // column container (or multicol container for short), and therefore acts as a container for multi-column layout.
    // FIXME: Maybe add column-height, depending on the resolution for https://github.com/w3c/csswg-drafts/issues/12688
    if (!box.computed_values().column_width().is_auto() || !box.computed_values().column_count().is_auto())
        return true;

    // FIXME: column-span: all should always create a new formatting context, even when the column-span: all element isn't contained by a multicol container (Spec change, Chrome bug).

    // https://html.spec.whatwg.org/multipage/rendering.html#the-fieldset-and-legend-elements
    if (box.is_fieldset_box())
        // The fieldset element, when it generates a CSS box, is expected to act as follows:
        // The element is expected to establish a new block formatting context.
        return true;

    // https://html.spec.whatwg.org/multipage/rendering.html#button-layout
    // An element using button layout establishes a new formatting context for its contents.
    if (auto const* html_element = as_if<HTML::HTMLElement>(box.dom_node()); html_element && html_element->uses_button_layout())
        return true;

    return false;
}

Optional<FormattingContext::Type> FormattingContext::formatting_context_type_created_by_box(Box const& box)
{
    if (is<SVGSVGBox>(box))
        return Type::SVG;

    if (box.is_replaced_box_with_children())
        return Type::ReplacedWithChildren;

    if (box.is_replaced_box())
        return Type::InternalReplaced;

    if (!box.can_have_children())
        return {};

    auto display = box.display();

    // Native controls can use a generic box to host their shadow tree. When a table-specific
    // display was adjusted to inline or block, keep that box atomic and give its contents an
    // independent context without changing the formatting of ordinary controls.
    if (box.has_replaced_element_table_display_adjustment()) {
        if (is<BlockContainer>(box))
            return Type::Block;
        return Type::InternalReplaced;
    }

    if (display.is_flex_inside())
        return Type::Flex;

    if (display.is_table_inside())
        return Type::Table;

    if (display.is_grid_inside())
        return Type::Grid;

    if (display.is_math_inside())
        // FIXME: We should create a MathML-specific formatting context here, but for now use a BFC, so _something_ is displayed
        return Type::Block;

    if (creates_block_formatting_context(box))
        return Type::Block;

    if (box.children_are_inline())
        return {};

    if (display.is_table_column() || display.is_table_row_group() || display.is_table_header_group() || display.is_table_footer_group() || display.is_table_row() || display.is_table_column_group())
        return {};

    // The box is a block container that doesn't create its own BFC.
    // It will be formatted by the containing BFC.
    if (!display.is_flow_inside()) {
        // HACK: Instead of crashing, create a dummy formatting context that does nothing.
        // FIXME: We need this for <math> elements
        return Type::InternalDummy;
    }
    return {};
}

Box const& FormattingContext::box_establishing_containing_formatting_context(Box const& child)
{
    auto const* box = child.containing_block();
    VERIFY(box);
    while (box->containing_block() && !formatting_context_type_created_by_box(*box).has_value())
        box = box->containing_block();
    return *box;
}

void FormattingContext::register_contained_abspos_child(Box const& child, StaticPositionRect const& static_position_rect)
{
    if (!child.containing_block())
        return;
    m_state.register_contained_abspos_child(box_establishing_containing_formatting_context(child), child, static_position_rect);
}

// FIXME: This is a hack. Get rid of it.
struct ReplacedFormattingContext : public FormattingContext {
    ReplacedFormattingContext(LayoutState& state, LayoutMode layout_mode, Box const& box)
        : FormattingContext(Type::InternalReplaced, layout_mode, state, box)
    {
    }
    virtual CSSPixels automatic_content_inline_size() const override { return 0; }
    virtual CSSPixels automatic_content_block_size() const override { return 0; }
    virtual void run(LayoutInput const&) override { }
};

// FIXME: This is a hack. Get rid of it.
struct DummyFormattingContext : public FormattingContext {
    DummyFormattingContext(LayoutState& state, LayoutMode layout_mode, Box const& box)
        : FormattingContext(Type::InternalDummy, layout_mode, state, box)
    {
    }
    virtual CSSPixels automatic_content_inline_size() const override { return 0; }
    virtual CSSPixels automatic_content_block_size() const override { return 0; }
    virtual void run(LayoutInput const&) override { }
};

OwnPtr<FormattingContext> FormattingContext::create_independent_formatting_context_if_needed(LayoutState& state, LayoutMode layout_mode, Box const& child_box, FormattingContext* parent)
{
    auto type = formatting_context_type_created_by_box(child_box);
    if (!type.has_value())
        return nullptr;

    if (RustFFI::rust_layout_owns_fc_type(to_underlying(*type)))
        return make<RustFormattingContext>(*type, layout_mode, state, child_box, parent);

    switch (type.value()) {
    case Type::Block:
        VERIFY_NOT_REACHED();
    case Type::SVG:
        VERIFY_NOT_REACHED();
    case Type::Flex:
        VERIFY_NOT_REACHED();
    case Type::Grid:
        VERIFY_NOT_REACHED();
    case Type::Table:
        VERIFY_NOT_REACHED();
    case Type::ReplacedWithChildren:
        return make<ReplacedWithChildrenFormattingContext>(state, layout_mode, child_box, parent);
    case Type::InternalReplaced:
        return make<ReplacedFormattingContext>(state, layout_mode, child_box);
    case Type::InternalDummy:
        return make<DummyFormattingContext>(state, layout_mode, child_box);
    case Type::Inline:
        // IFC should always be created by a parent BFC directly.
        VERIFY_NOT_REACHED();
        break;
    default:
        VERIFY_NOT_REACHED();
    }
}

NonnullOwnPtr<FormattingContext> FormattingContext::create_independent_formatting_context(LayoutState& state, LayoutMode layout_mode, Box const& child_box, FormattingContext* parent)
{
    if (auto context = create_independent_formatting_context_if_needed(state, layout_mode, child_box, parent))
        return context.release_nonnull();

    if (is<BlockContainer>(child_box))
        return make<RustFormattingContext>(Type::Block, layout_mode, state, child_box, nullptr);

    // HACK: Instead of crashing in scenarios that assume the formatting context can be created, create a dummy formatting context that does nothing.
    dbgln("FIXME: An independent formatting context was requested from a Box that does not have a formatting context type. A dummy formatting context will be created instead.");
    return make<DummyFormattingContext>(state, layout_mode, child_box);
}

OwnPtr<FormattingContext> FormattingContext::layout_inside(Box const& child_box, LayoutMode layout_mode, LayoutInput const& layout_input)
{
    {
        // OPTIMIZATION: If we're doing intrinsic sizing and `child_box` has definite size in both axes,
        //               we don't need to layout its insides. The size is resolvable without learning
        //               the metrics of whatever's inside the box.
        //
        // https://drafts.csswg.org/css2/#propdef-vertical-align
        // The baseline of an inline-block is the baseline of its last line box in the normal flow, unless it has
        // either no in-flow line boxes or if its 'overflow' property has a computed value other than visible, in which
        // case the baseline is the bottom margin edge.
        //
        // Inline-level boxes can contribute a baseline to their parent line box, so they still need their contents
        // laid out even when their own intrinsic size is already definite.
        auto const& used_values = m_state.get(child_box);
        if (layout_mode == LayoutMode::IntrinsicSizing
            && !child_box.is_inline()
            && used_values.inline_size_constraint() == SizeConstraint::None
            && used_values.block_size_constraint() == SizeConstraint::None
            && used_values.has_definite_inline_size()
            && used_values.has_definite_block_size()) {
            return nullptr;
        }
    }

    if (!child_box.can_have_children())
        return {};

    auto independent_formatting_context = create_independent_formatting_context_if_needed(m_state, layout_mode, child_box, this);
    if (independent_formatting_context)
        independent_formatting_context->run(layout_input);
    else
        run(layout_input);

    return independent_formatting_context;
}

LogicalSize FormattingContext::solve_replaced_size_constraint(CSSPixels input_inline_size, CSSPixels input_block_size, Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // 10.4 Minimum and maximum widths: 'min-width' and 'max-width'
    // https://www.w3.org/TR/CSS22/visudet.html#min-max-widths

    auto min_inline_size = box.computed_values().min_width().is_auto() ? 0 : calculate_inner_inline_size(box, available_space.inline_size, box.computed_values().min_width(), containing_block_constraints);
    auto specified_max_inline_size = should_treat_max_inline_size_as_none(box, available_space.inline_size, containing_block_constraints) ? input_inline_size : calculate_inner_inline_size(box, available_space.inline_size, box.computed_values().max_width(), containing_block_constraints);
    auto max_inline_size = max(min_inline_size, specified_max_inline_size);

    auto min_block_size = box.computed_values().min_height().is_auto() ? 0 : calculate_inner_block_size(box, available_space, box.computed_values().min_height(), containing_block_constraints);
    auto specified_max_block_size = should_treat_max_block_size_as_none(box, available_space.block_size, containing_block_constraints) ? input_block_size : calculate_inner_block_size(box, available_space, box.computed_values().max_height(), containing_block_constraints);
    auto max_block_size = max(min_block_size, specified_max_block_size);

    auto const& box_state = m_state.get(box);
    // These are from the "Constraint Violation" table in spec, but reordered so that each condition is
    // interpreted as mutually exclusive to any other.
    if (input_inline_size < min_inline_size && input_block_size > max_block_size)
        return { min_inline_size, max_block_size };
    if (input_inline_size > max_inline_size && input_block_size < min_block_size)
        return { max_inline_size, min_block_size };

    if (input_inline_size > 0 && input_block_size > 0) {
        if (input_inline_size > max_inline_size && input_block_size > max_block_size && max_inline_size / input_inline_size <= max_block_size / input_block_size)
            return { max_inline_size, max(min_block_size, content_block_size_from_aspect_ratio(box, box_state, max_inline_size)) };
        if (input_inline_size > max_inline_size && input_block_size > max_block_size && max_inline_size / input_inline_size > max_block_size / input_block_size)
            return { max(min_inline_size, content_inline_size_from_aspect_ratio(box, box_state, max_block_size)), max_block_size };
        if (input_inline_size < min_inline_size && input_block_size < min_block_size && min_inline_size / input_inline_size <= min_block_size / input_block_size)
            return { min(max_inline_size, content_inline_size_from_aspect_ratio(box, box_state, min_block_size)), min_block_size };
        if (input_inline_size < min_inline_size && input_block_size < min_block_size && min_inline_size / input_inline_size > min_block_size / input_block_size)
            return { min_inline_size, min(max_block_size, content_block_size_from_aspect_ratio(box, box_state, min_inline_size)) };
    }

    if (input_inline_size > max_inline_size)
        return { max_inline_size, max(content_block_size_from_aspect_ratio(box, box_state, max_inline_size), min_block_size) };
    if (input_inline_size < min_inline_size)
        return { min_inline_size, min(content_block_size_from_aspect_ratio(box, box_state, min_inline_size), max_block_size) };
    if (input_block_size > max_block_size)
        return { max(content_inline_size_from_aspect_ratio(box, box_state, max_block_size), min_inline_size), max_block_size };
    if (input_block_size < min_block_size)
        return { min(content_inline_size_from_aspect_ratio(box, box_state, min_block_size), max_inline_size), min_block_size };

    return { input_inline_size, input_block_size };
}

CSSPixels FormattingContext::measure_automatic_content_block_size(Box const& box, AvailableSpace const& inner_available_space, ContainingBlockConstraints const& containing_block_constraints)
{
    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);
    throwaway_state.create(box, containing_block_constraints.percentage_basis_inline_size, containing_block_constraints.percentage_basis_block_size);
    auto measuring_context = create_independent_formatting_context_if_needed(throwaway_state, m_layout_mode, box, this);
    measuring_context->run(LayoutInput { inner_available_space, containing_block_constraints });
    return measuring_context->automatic_content_block_size();
}

void FormattingContext::make_button_content_box_definite(Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints, Optional<CSSPixels> measured_content_block_size)
{
    auto const* html_element = as_if<HTML::HTMLElement>(box.dom_node());
    if (!html_element || !html_element->uses_button_layout())
        return;

    // Flex/grid-inside buttons are their own flex/grid container and get no anonymous content wrapper,
    // so there is nothing to make definite for centering.
    auto display = box.display();
    if (display.is_flex_inside() || display.is_grid_inside())
        return;

    auto const& computed_values = box.computed_values();

    // With auto height and no min-height the content box already exactly wraps the content, so there is
    // no extra space to center within and no need to force a definite content box.
    if (computed_values.height().is_auto() && computed_values.min_height().is_auto())
        return;

    auto& box_state = m_state.get_mutable(box);
    if (box_state.has_definite_block_size())
        return;

    auto natural_content_block_size = measured_content_block_size.value_or_lazy_evaluated([&] {
        return measure_automatic_content_block_size(box, box_state.available_inner_space_or_constraints_from(available_space), containing_block_constraints);
    });

    auto used_block_size = should_treat_block_size_as_auto(box, available_space, containing_block_constraints)
        ? natural_content_block_size
        : calculate_inner_block_size(box, available_space, computed_values.height(), containing_block_constraints);
    if (!should_treat_max_block_size_as_none(box, available_space.block_size, containing_block_constraints) && !computed_values.max_height().is_auto())
        used_block_size = min(used_block_size, calculate_inner_block_size(box, available_space, computed_values.max_height(), containing_block_constraints));
    if (!computed_values.min_height().is_auto())
        used_block_size = max(used_block_size, calculate_inner_block_size(box, available_space, computed_values.min_height(), containing_block_constraints));

    // Only force a definite content box when the button's used block size exceeds its content block size, so a larger
    // preferred or minimum size has room to center within. A content-sized box stays indefinite, so an intrinsic
    // keyword does not resolve percentage-sized descendants.
    if (used_block_size <= natural_content_block_size)
        return;

    box_state.set_content_block_size(used_block_size);
    box_state.set_has_definite_block_size(true);
}

// 17.5.2 Table width algorithms: the 'table-layout' property
// https://www.w3.org/TR/CSS22/tables.html#width-layout
CSSPixels FormattingContext::compute_table_box_inline_size_inside_table_wrapper(
    Box const& box,
    AvailableSpace const& available_space,
    ContainingBlockConstraints const& table_wrapper_constraints,
    Optional<CSSPixels> table_wrapper_containing_block_inline_size,
    TableWrapperInlineSizeMode table_wrapper_inline_size_mode)
{
    // CSS 2 says the table wrapper inline size is the border-edge inline size of the table grid box inside it.

    auto const& computed_values = box.computed_values();

    auto containing_block_inline_size = table_wrapper_containing_block_inline_size.value_or(available_space.inline_size.to_px_or_zero());

    // If 'margin-left', or 'margin-right' are computed as 'auto', their used value is '0'.
    auto margin_left = computed_values.margin().left().to_px_or_zero(containing_block_inline_size);
    auto margin_right = computed_values.margin().right().to_px_or_zero(containing_block_inline_size);

    // table-wrapper can't have borders or paddings but it might have margin taken from table-root.
    auto available_inline_size = containing_block_inline_size - margin_left - margin_right;

    Optional<Box const&> table_box;
    box.for_each_in_subtree_of_type<Box>([&](Box const& child_box) {
        if (child_box.display().is_table_inside()) {
            table_box = child_box;
            return TraversalDecision::Break;
        }
        return TraversalDecision::Continue;
    });
    VERIFY(table_box.has_value());

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);

    // The table wrapper is invisible to percentage resolution, so the table box gets the
    // wrapper's constraints unchanged. Callers measuring a table wrapper for grid alignment
    // pass the grid-area inline size as the wrapper's percentage basis.
    throwaway_state.create(box, table_wrapper_constraints.percentage_basis_inline_size, table_wrapper_constraints.percentage_basis_block_size);
    auto const& table_constraints = table_wrapper_constraints;
    auto& table_box_state = throwaway_state.create(*table_box, table_constraints.percentage_basis_inline_size, table_constraints.percentage_basis_block_size);
    auto const& table_box_computed_values = table_box->computed_values();
    table_box_state.set_border_left(table_box_computed_values.border_left().width);
    table_box_state.set_border_right(table_box_computed_values.border_right().width);
    table_box_state.set_padding_left(table_box_computed_values.padding().left().to_px_or_zero(containing_block_inline_size));
    table_box_state.set_padding_right(table_box_computed_values.padding().right().to_px_or_zero(containing_block_inline_size));

    auto context = create_independent_formatting_context(throwaway_state, LayoutMode::IntrinsicSizing, *table_box, this);
    context->run_until_table_inline_size_calculation(
        LayoutInput { table_box_state.available_inner_space_or_constraints_from(available_space), table_constraints },
        true);

    auto table_used_inline_size = throwaway_state.get(*table_box).border_box_inline_size();
    if (table_wrapper_inline_size_mode == TableWrapperInlineSizeMode::UseTableUsedInlineSizeIfNotAuto
        && !table_box->computed_values().width().is_auto()) {
        return table_used_inline_size;
    }
    return available_space.inline_size.is_definite() ? min(table_used_inline_size, available_inline_size) : table_used_inline_size;
}

// 17.5.3 Table height algorithms
// https://www.w3.org/TR/CSS22/tables.html#height-layout
CSSPixels FormattingContext::compute_table_box_block_size_inside_table_wrapper(Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& table_wrapper_constraints)
{
    // The table wrapper block size should equal the block size of the table box it contains.

    auto const& computed_values = box.computed_values();

    auto containing_block_inline_size = available_space.inline_size.to_px_or_zero();
    auto containing_block_block_size = available_space.block_size.to_px_or_zero();

    // If 'margin-top', or 'margin-bottom' are computed as 'auto', their used value is '0'.
    auto margin_top = computed_values.margin().top().resolved_or_auto(containing_block_inline_size).to_px_or_zero();
    auto margin_bottom = computed_values.margin().bottom().resolved_or_auto(containing_block_inline_size).to_px_or_zero();

    // table-wrapper can't have borders or paddings but it might have margin taken from table-root.
    auto available_block_size = containing_block_block_size - margin_top - margin_bottom;

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);
    throwaway_state.create(box, table_wrapper_constraints.percentage_basis_inline_size, table_wrapper_constraints.percentage_basis_block_size);

    auto context = create_independent_formatting_context_if_needed(throwaway_state, LayoutMode::IntrinsicSizing, box, this);
    VERIFY(context);
    context->run(LayoutInput { m_state.get(box).available_inner_space_or_constraints_from(available_space), table_wrapper_constraints });

    Optional<Box const&> table_box;
    box.for_each_in_subtree_of_type<Box>([&](Box const& child_box) {
        if (child_box.display().is_table_inside()) {
            table_box = child_box;
            return TraversalDecision::Break;
        }
        return TraversalDecision::Continue;
    });
    VERIFY(table_box.has_value());

    auto table_used_block_size = throwaway_state.get(*table_box).border_box_block_size();
    return available_space.block_size.is_definite() ? min(table_used_block_size, available_block_size) : table_used_block_size;
}

ContainingBlockConstraints FormattingContext::constraints_for_child_context(
    LayoutState::UsedValues const& containing_block_used_values,
    ContainingBlockConstraints const& containing_block_constraints)
{
    auto const& containing_block = containing_block_used_values.node();
    auto const* containing_block_box = as_if<Box>(containing_block);
    // Anonymous boxes are invisible to percentage resolution: their children resolve percentages
    // against the closest non-anonymous ancestor, so an anonymous containing block without a
    // definite size of its own passes the constraints it was given through. Anonymous table
    // cells are the exception: they are proper containing blocks with their own size semantics.
    auto should_forward_indefinite_basis = containing_block_box
        && containing_block_box->is_anonymous()
        && !containing_block_box->display().is_table_cell()
        && !containing_block_box->has_auto_content_box_size()
        && containing_block_used_values.inline_size_constraint() == SizeConstraint::None
        && containing_block_used_values.block_size_constraint() == SizeConstraint::None;

    auto percentage_basis_inline_size = containing_block_used_values.has_definite_inline_size()
        ? Optional<CSSPixels> { containing_block_used_values.content_inline_size() }
        : should_forward_indefinite_basis ? containing_block_constraints.percentage_basis_inline_size
                                          : Optional<CSSPixels> {};
    auto percentage_basis_block_size = containing_block_used_values.has_definite_block_size()
        ? Optional<CSSPixels> { containing_block_used_values.content_block_size() }
        : should_forward_indefinite_basis ? containing_block_constraints.percentage_basis_block_size
                                          : Optional<CSSPixels> {};

    // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
    auto quirks_mode_percentage_basis_block_size = [&]() -> Optional<CSSPixels> {
        // 1. Let element be the nearest ancestor containing block of element, if there is one.
        //    Otherwise, return the initial containing block.
        if (containing_block.is_viewport())
            return containing_block_used_values.content_block_size();

        // 2. If element has a computed value of the display property that is table-cell, then return a
        //    UA-defined value.
        if (containing_block.display().is_table_cell()) {
            // FIXME: Likely UA-defined value should not be 0.
            return CSSPixels(0);
        }

        // 3. If element has a computed value of the height property that is not auto, then return element.
        if (!containing_block.computed_values().height().is_auto())
            return containing_block_used_values.content_block_size();

        // 4. If element has a computed value of the position property that is absolute, or if element is a
        //    not a block container or a table wrapper box, then return element.
        if (containing_block.is_absolutely_positioned() || !is<BlockContainer>(containing_block) || is<TableWrapper>(containing_block))
            return containing_block_used_values.content_block_size();

        // 5. Jump to the first step.
        // NOTE: Evaluated incrementally: in-flow auto-height block containers pass the basis they
        //       inherited from their own containing block through to their children.
        return containing_block_constraints.quirks_mode_percentage_basis_block_size;
    }();

    return { percentage_basis_inline_size, percentage_basis_block_size, quirks_mode_percentage_basis_block_size };
}

LayoutInput FormattingContext::layout_input_for_child_context(
    LayoutState::UsedValues const& containing_block_used_values,
    LayoutInput const& containing_block_layout_input,
    AvailableSpace available_space)
{
    return LayoutInput {
        available_space,
        constraints_for_child_context(containing_block_used_values, containing_block_layout_input.containing_block_constraints),
        containing_block_layout_input.content_box_position_in_bfc_root,
    };
}

// 10.3.2 Inline, replaced elements, https://www.w3.org/TR/CSS22/visudet.html#inline-replaced-width
CSSPixels FormattingContext::tentative_inline_size_for_replaced_element(Box const& box, CSS::Size const& computed_inline_size, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // Treat percentages of indefinite containing block widths as 0 (the initial width).
    if (computed_inline_size.is_percentage() && !containing_block_constraints.percentage_basis_inline_size.has_value())
        return 0;

    auto computed_block_size = should_treat_block_size_as_auto(box, available_space, containing_block_constraints) ? CSS::Size::make_auto() : box.computed_values().height();

    CSSPixels used_inline_size = 0;
    if (computed_inline_size.is_auto()) {
        used_inline_size = computed_inline_size.to_px(available_space.inline_size.to_px_or_zero());
    } else {
        used_inline_size = calculate_inner_inline_size(box, available_space.inline_size, computed_inline_size, containing_block_constraints);
    }

    // If 'height' and 'width' both have computed values of 'auto' and the element also has an intrinsic width,
    // then that intrinsic width is the used value of 'width'.
    auto intrinsic = intrinsic_size_for_replaced_sizing(box);
    if (computed_block_size.is_auto() && computed_inline_size.is_auto() && intrinsic.has_width())
        return intrinsic.width.value();

    // If 'height' and 'width' both have computed values of 'auto' and the element has no intrinsic width,
    // but does have an intrinsic height and intrinsic ratio;
    // or if 'width' has a computed value of 'auto',
    // 'height' has some other computed value, and the element does have an intrinsic ratio; then the used value of 'width' is:
    //
    //     (used height) * (intrinsic ratio)
    if ((computed_block_size.is_auto() && computed_inline_size.is_auto() && !intrinsic.has_width() && intrinsic.has_height() && box.has_preferred_aspect_ratio())
        || (computed_inline_size.is_auto() && !computed_block_size.is_auto() && box.has_preferred_aspect_ratio())) {
        auto content_block_size = compute_block_size_for_replaced_element(box, available_space, containing_block_constraints);
        return content_inline_size_from_aspect_ratio(box, m_state.get(box), content_block_size);
    }

    // If 'height' and 'width' both have computed values of 'auto' and the element has an intrinsic ratio but no intrinsic height or width,
    // then the used value of 'width' is undefined in CSS 2.2. However, it is suggested that, if the containing block's width does not itself
    // depend on the replaced element's width, then the used value of 'width' is calculated from the constraint equation used for block-level,
    // non-replaced elements in normal flow.
    if (computed_block_size.is_auto() && computed_inline_size.is_auto() && !intrinsic.has_width() && !intrinsic.has_height() && box.has_preferred_aspect_ratio()) {
        if (!available_space.inline_size.is_intrinsic_sizing_constraint())
            return calculate_stretch_fit_inline_size(box, available_space.inline_size);

        switch (cyclic_percentage_intrinsic_contribution(box, box.computed_values().width(), available_space.inline_size, CyclicPercentageSizeProperty::PreferredOrMaxSize)) {
        case CyclicPercentageIntrinsicContribution::ResolveAsZero:
            return 0;
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            break;
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            return calculate_stretch_fit_inline_size(box, available_space.inline_size);
        }
    }

    // Otherwise, if 'width' has a computed value of 'auto', and the element has an intrinsic width, then that intrinsic width is the used value of 'width'.
    if (computed_inline_size.is_auto() && intrinsic.has_width())
        return intrinsic.width.value();

    // Otherwise, if 'width' has a computed value of 'auto', but none of the conditions above are met, then the used value of 'width' becomes 300px.
    // If 300px is too wide to fit the device, UAs should use the width of the largest rectangle that has a 2:1 ratio and fits the device instead.
    if (computed_inline_size.is_auto())
        return 300;

    return used_inline_size;
}

CSSPixels FormattingContext::compute_inline_size_for_replaced_element(Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // 10.3.4 Block-level, replaced elements in normal flow...
    // 10.3.2 Inline, replaced elements

    auto computed_inline_size = should_treat_inline_size_as_auto(box, available_space) ? CSS::Size::make_auto() : box.computed_values().width();
    auto computed_block_size = should_treat_block_size_as_auto(box, available_space, containing_block_constraints) ? CSS::Size::make_auto() : box.computed_values().height();

    // 1. The tentative used width is calculated (without 'min-width' and 'max-width')
    auto used_inline_size = tentative_inline_size_for_replaced_element(box, computed_inline_size, available_space, containing_block_constraints);

    if (computed_inline_size.is_auto() && computed_block_size.is_auto() && box.has_preferred_aspect_ratio()) {
        CSSPixels inline_size = used_inline_size;
        CSSPixels block_size = tentative_block_size_for_replaced_element(box, computed_block_size, available_space, containing_block_constraints);
        used_inline_size = solve_replaced_size_constraint(inline_size, block_size, box, available_space, containing_block_constraints).inline_size;
    }

    // 2. If the tentative used width is greater than 'max-width', the rules above are applied again,
    //    but this time using the computed value of 'max-width' as the computed value for 'width'.
    if (!should_treat_max_inline_size_as_none(box, available_space.inline_size, containing_block_constraints)) {
        auto const& computed_max_inline_size = box.computed_values().max_width();
        auto max_inline_size = calculate_inner_inline_size(box, available_space.inline_size, computed_max_inline_size, containing_block_constraints);
        if (used_inline_size > max_inline_size)
            used_inline_size = tentative_inline_size_for_replaced_element(box, computed_max_inline_size, available_space, containing_block_constraints);
    }

    // 3. If the resulting width is smaller than 'min-width', the rules above are applied again,
    //    but this time using the value of 'min-width' as the computed value for 'width'.
    auto computed_min_inline_size = box.computed_values().min_width();
    if (!computed_min_inline_size.is_auto()) {
        auto min_inline_size = calculate_inner_inline_size(box, available_space.inline_size, computed_min_inline_size, containing_block_constraints);
        if (used_inline_size < min_inline_size)
            used_inline_size = tentative_inline_size_for_replaced_element(box, computed_min_inline_size, available_space, containing_block_constraints);
    }

    return used_inline_size;
}

// 10.6.2 Inline replaced elements, block-level replaced elements in normal flow, 'inline-block' replaced elements in normal flow and floating replaced elements
// https://www.w3.org/TR/CSS22/visudet.html#inline-replaced-height
CSSPixels FormattingContext::tentative_block_size_for_replaced_element(Box const& box, CSS::Size const& computed_block_size, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    auto intrinsic = intrinsic_size_for_replaced_sizing(box);
    // If 'height' and 'width' both have computed values of 'auto' and the element also has
    // an intrinsic height, then that intrinsic height is the used value of 'height'.
    if (should_treat_inline_size_as_auto(box, available_space) && should_treat_block_size_as_auto(box, available_space, containing_block_constraints) && intrinsic.has_height())
        return intrinsic.height.value();

    // Otherwise, if 'height' has a computed value of 'auto', and the element has an intrinsic ratio then the used value of 'height' is:
    //
    //     (used width) / (intrinsic ratio)
    if (computed_block_size.is_auto() && box.has_preferred_aspect_ratio())
        return content_block_size_from_aspect_ratio(box, m_state.get(box));

    // Otherwise, if 'height' has a computed value of 'auto', and the element has an intrinsic height, then that intrinsic height is the used value of 'height'.
    if (computed_block_size.is_auto() && intrinsic.has_height())
        return intrinsic.height.value();

    // Otherwise, if 'height' has a computed value of 'auto', but none of the conditions above are met,
    // then the used value of 'height' must be set to the height of the largest rectangle that has a 2:1 ratio, has a height not greater than 150px,
    // and has a width not greater than the device width.
    if (computed_block_size.is_auto())
        return 150;

    // FIXME: Handle cases when available_space is not definite.
    return calculate_inner_block_size(box, available_space, computed_block_size, containing_block_constraints);
}

CSSPixels FormattingContext::compute_block_size_for_replaced_element(Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // 10.6.2 Inline replaced elements
    // 10.6.4 Block-level replaced elements in normal flow
    // 10.6.6 Floating replaced elements
    // 10.6.10 'inline-block' replaced elements in normal flow

    auto computed_inline_size = should_treat_inline_size_as_auto(box, available_space) ? CSS::Size::make_auto() : box.computed_values().width();
    auto computed_block_size = should_treat_block_size_as_auto(box, available_space, containing_block_constraints) ? CSS::Size::make_auto() : box.computed_values().height();

    // 1. The tentative used height is calculated (without 'min-height' and 'max-height')
    CSSPixels used_block_size = tentative_block_size_for_replaced_element(box, computed_block_size, available_space, containing_block_constraints);

    // However, for replaced elements with both 'width' and 'height' computed as 'auto',
    // use the algorithm under 'Minimum and maximum widths'
    // https://www.w3.org/TR/CSS22/visudet.html#min-max-widths
    // to find the used width and height.
    if ((computed_inline_size.is_auto() && computed_block_size.is_auto() && box.has_preferred_aspect_ratio())) {
        // NOTE: This is a special case where calling tentative_inline_size_for_replaced_element() would call us right back,
        //       and we'd end up in an infinite loop. So we need to handle this case separately.
        if (auto intrinsic = intrinsic_size_for_replaced_sizing(box); intrinsic.has_width() || !intrinsic.has_height()) {
            CSSPixels inline_size = tentative_inline_size_for_replaced_element(box, computed_inline_size, available_space, containing_block_constraints);
            CSSPixels block_size = used_block_size;
            used_block_size = solve_replaced_size_constraint(inline_size, block_size, box, available_space, containing_block_constraints).block_size;
        }
    }
    // 2. If this tentative height is greater than 'max-height', the rules above are applied again,
    //    but this time using the value of 'max-height' as the computed value for 'height'.
    if (!should_treat_max_block_size_as_none(box, available_space.block_size, containing_block_constraints)) {
        auto const& computed_max_block_size = box.computed_values().max_height();
        auto max_block_size = calculate_inner_block_size(box, available_space, computed_max_block_size, containing_block_constraints);
        if (used_block_size > max_block_size)
            used_block_size = tentative_block_size_for_replaced_element(box, computed_max_block_size, available_space, containing_block_constraints);
    }

    // 3. If the resulting height is smaller than 'min-height', the rules above are applied again,
    //    but this time using the value of 'min-height' as the computed value for 'height'.
    auto computed_min_block_size = box.computed_values().min_height();
    if (!computed_min_block_size.is_auto()) {
        auto min_block_size = calculate_inner_block_size(box, available_space, computed_min_block_size, containing_block_constraints);
        if (used_block_size < min_block_size)
            used_block_size = tentative_block_size_for_replaced_element(box, computed_min_block_size, available_space, containing_block_constraints);
    }

    return used_block_size;
}

static bool style_value_contains_anchor(CSS::StyleValue const& value)
{
    if (value.is_anchor())
        return true;
    if (value.is_calculated())
        return value.as_calculated().contains_anchor_function();
    return false;
}

// NB: Generated boxes for pseudo-elements are anonymous, so their computed values live in
//     the generator element under the relevant pseudo-element rather than on a DOM node of
//     their own.
static Optional<DOM::AbstractElement> abstract_element_for_box(Box const& box)
{
    if (box.is_generated_for_pseudo_element())
        return DOM::AbstractElement { *box.pseudo_element_generator(), box.generated_for_pseudo_element() };
    if (auto const* element = as_if<DOM::Element>(box.dom_node()))
        return DOM::AbstractElement { *element };
    return {};
}

bool FormattingContext::box_inset_properties_contain_anchor_functions(Box const& box)
{
    auto abstract_element = abstract_element_for_box(box);
    if (!abstract_element.has_value())
        return false;

    auto const* computed = abstract_element->computed_values();
    if (!computed)
        return false;
    // Anchor functions in insets only survive to used-value time inside calculated values, so
    // when no inset is calculated (the common case), skip reconstructing the style values.
    auto const& inset = computed->inset();
    if (!inset.top().is_calculated() && !inset.right().is_calculated() && !inset.bottom().is_calculated() && !inset.left().is_calculated())
        return false;

    auto top = computed->computed_style_value(CSS::PropertyID::Top);
    auto right = computed->computed_style_value(CSS::PropertyID::Right);
    auto bottom = computed->computed_style_value(CSS::PropertyID::Bottom);
    auto left = computed->computed_style_value(CSS::PropertyID::Left);
    VERIFY(top && right && bottom && left);
    return style_value_contains_anchor(*top)
        || style_value_contains_anchor(*right)
        || style_value_contains_anchor(*bottom)
        || style_value_contains_anchor(*left);
}

void FormattingContext::layout_absolutely_positioned_children()
{
    // The table formatting context handles cell abspos layout after vertical alignment.
    if (context_box().display().is_table_cell())
        return;
    layout_absolutely_positioned_children(context_box());
}

void FormattingContext::layout_absolutely_positioned_children(Box const& box)
{
    LayoutRustBridge bridge(*this);
    auto callbacks = bridge.formatting_context_callbacks();
    RustFFI::rust_layout_abspos_layout_children(
        m_state.rust_state_handle(),
        const_cast<Box*>(&box),
        to_underlying(m_layout_mode),
        &callbacks);
}

bool FormattingContext::can_replay_saved_abspos_layout_inputs_after_style_change(Box const& box)
{
    if (!box.containing_block())
        return false;

    auto const& inputs = *box.saved_abspos_layout_inputs();
    if (inputs.containing_block_info.derives_from_own_computed_values)
        return false;

    auto const& inset = box.computed_values().inset();
    bool uses_static_position = (inset.left().is_auto() && inset.right().is_auto())
        || (inset.top().is_auto() && inset.bottom().is_auto());
    if (uses_static_position && inputs.static_position_rect.alignment_derives_from_own_computed_values)
        return false;

    return true;
}

void FormattingContext::layout_absolutely_positioned_element_from_saved_inputs(LayoutState& state, Box& box)
{
    auto* containing_block = box.containing_block();
    VERIFY(containing_block);
    VERIFY(box.saved_abspos_layout_inputs());
    RustFormattingContext context(Type::AbsposReplay, LayoutMode::Normal, state, *containing_block, nullptr);
    context.replay_absolutely_positioned_element(box);
}

void FormattingContext::compute_inset(NodeWithStyleAndBoxModelMetrics const& box, CSSPixelSize containing_block_size)
{
    LayoutRustBridge bridge(*this);
    auto callbacks = bridge.formatting_context_callbacks();
    RustFFI::rust_layout_abspos_compute_inset(
        m_state.rust_state_handle(),
        const_cast<Box*>(&m_context_box),
        const_cast<NodeWithStyleAndBoxModelMetrics*>(&box),
        containing_block_size.width().raw_value(),
        containing_block_size.height().raw_value(),
        &callbacks);
}

// https://drafts.csswg.org/css-sizing-3/#fit-content-size
CSSPixels FormattingContext::calculate_fit_content_inline_size(Layout::Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // If the available space in a given axis is definite, equal to clamp(min-content size, stretch-fit size,
    // max-content size) (i.e. max(min-content size, min(max-content size, stretch-fit size))).
    if (available_space.inline_size.is_definite()) {
        auto stretch_fit_inline_size = calculate_stretch_fit_inline_size(box, available_space.inline_size);
        auto max_content_inline_size = calculate_max_content_inline_size(box, containing_block_constraints);
        if (max_content_inline_size <= stretch_fit_inline_size)
            return max_content_inline_size;
        return max(calculate_min_content_inline_size(box, containing_block_constraints), stretch_fit_inline_size);
    }

    // When sizing under a min-content constraint, equal to the min-content size.
    if (available_space.inline_size.is_min_content())
        return calculate_min_content_inline_size(box, containing_block_constraints);

    // Otherwise, equal to the max-content size in that axis.
    return calculate_max_content_inline_size(box, containing_block_constraints);
}

// https://drafts.csswg.org/css-sizing-3/#fit-content-size
CSSPixels FormattingContext::calculate_fit_content_block_size(Layout::Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    // If the available space in a given axis is definite,
    // equal to clamp(min-content size, stretch-fit size, max-content size)
    // (i.e. max(min-content size, min(max-content size, stretch-fit size))).
    if (available_space.block_size.is_definite()) {
        auto inline_size = available_space.inline_size.to_px_or_zero();
        auto stretch_fit_block_size = calculate_stretch_fit_block_size(box, available_space.block_size);
        auto max_content_block_size = calculate_max_content_block_size(box, inline_size, containing_block_constraints);
        if (max_content_block_size <= stretch_fit_block_size)
            return max_content_block_size;
        return max(calculate_min_content_block_size(box, inline_size, containing_block_constraints), stretch_fit_block_size);
    }

    // When sizing under a min-content constraint, equal to the min-content size.
    if (available_space.block_size.is_min_content())
        return calculate_min_content_block_size(box, available_space.inline_size.to_px_or_zero(), containing_block_constraints);

    // Otherwise, equal to the max-content size in that axis.
    return calculate_max_content_block_size(box, available_space.inline_size.to_px_or_zero(), containing_block_constraints);
}

static IntrinsicSizeCacheKey intrinsic_size_cache_key(ContainingBlockConstraints const& containing_block_constraints)
{
    return {
        .measured_at_inline_size = {},
        .percentage_basis_inline_size = containing_block_constraints.percentage_basis_inline_size,
        .percentage_basis_block_size = containing_block_constraints.percentage_basis_block_size,
        .quirks_mode_percentage_basis_block_size = containing_block_constraints.quirks_mode_percentage_basis_block_size,
    };
}

CSSPixels FormattingContext::calculate_min_content_inline_size(Layout::Box const& box, ContainingBlockConstraints const& containing_block_constraints) const
{
    if (box.is_replaced_box()) {
        // https://www.w3.org/TR/css-sizing-3/#replaced-percentage-min-contribution
        // NOTE: If the box is replaced, a cyclic percentage in the value of any max size property or
        //       preferred size property (width/max-width/height/max-height), is resolved against zero
        //       when calculating the min-content contribution in the corresponding axis.
        // FIXME: If the box also has a preferred aspect ratio, then this min-content contribution is
        //        floored by any <length-percentage> minimum size from the opposite axis—resolving any
        //        such percentage against zero—transferred through the preferred aspect ratio.
        // Note: The min-content contribution is, as always, also floored by the minimum size in its own axis.
        if (box.computed_values().width().contains_percentage() || box.computed_values().max_width().contains_percentage()) {
            auto const& min_inline_size = box.computed_values().min_width();
            if (!min_inline_size.is_length_percentage())
                return 0;

            auto zero_percentage_basis_constraints = containing_block_constraints;
            zero_percentage_basis_constraints.percentage_basis_inline_size = 0;
            return calculate_inner_inline_size(box, AvailableSize::make_min_content(), min_inline_size, zero_percentage_basis_constraints);
        }
    }
    if (auto transferred_inline_size = calculate_transferred_inline_size_for_replaced_element(box, containing_block_constraints); transferred_inline_size.has_value())
        return transferred_inline_size.value();
    auto auto_size = box.auto_content_box_size();
    if (auto_size.has_width())
        return auto_size.width.value();
    if (box.is_replaced_box() && !box.has_preferred_aspect_ratio()) {
        if (auto fallback_inline_size = max_content_size_for_replaced_element_without_natural_size(box, auto_size, m_state.get(box), SizeDimension::Inline); fallback_inline_size.has_value())
            return fallback_inline_size.value();
    }

    // Boxes with no children have zero intrinsic inline size.
    if (!box.has_children())
        return 0;

    auto cache_key = intrinsic_size_cache_key(containing_block_constraints);
    auto& cache = box.cached_intrinsic_sizes().min_content_inline_size;
    if (auto cached_value = cache.get(cache_key); cached_value.has_value())
        return cached_value.value();

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);

    auto& box_state = throwaway_state.create(box, containing_block_constraints.percentage_basis_inline_size, containing_block_constraints.percentage_basis_block_size);
    box_state.set_inline_size_constraint(SizeConstraint::MinContent);
    box_state.set_indefinite_content_inline_size();

    auto context = create_independent_formatting_context(throwaway_state, LayoutMode::IntrinsicSizing, box, const_cast<FormattingContext*>(this));

    auto available_inline_size = AvailableSize::make_min_content();
    auto available_block_size = box_state.has_definite_block_size()
        ? AvailableSize::make_definite(box_state.content_block_size())
        : AvailableSize::make_indefinite();

    auto available_space = AvailableSpace(available_inline_size, available_block_size);
    context->run(LayoutInput { available_space, containing_block_constraints });

    auto min_content_inline_size = clamp_to_max_dimension_value(context->automatic_content_inline_size());
    cache.set(cache_key, min_content_inline_size);
    return min_content_inline_size;
}

// https://drafts.csswg.org/css-sizing-3/#intrinsic-sizes
// "size constraints in the opposite dimension will transfer through and can affect the auto size in the considered one"
Optional<CSSPixels> FormattingContext::calculate_transferred_inline_size_for_replaced_element(Layout::Box const& box, ContainingBlockConstraints const& containing_block_constraints) const
{
    if (!box.is_replaced_box() || !box.has_preferred_aspect_ratio())
        return {};

    // https://drafts.csswg.org/css2/#inline-replaced-width
    // "'width' has a computed value of 'auto', 'height' has some other computed value, and the element does have an intrinsic ratio"
    if (!box.computed_values().width().is_auto())
        return {};

    auto const& computed_block_size = box.computed_values().height();
    if (computed_block_size.is_auto() || computed_block_size.is_intrinsic_sizing_constraint())
        return {};

    auto available_space = m_state.get(box).available_inner_space_or_constraints_from(
        AvailableSpace(AvailableSize::make_max_content(), AvailableSize::make_indefinite()));
    if (should_treat_block_size_as_auto(box, available_space, containing_block_constraints))
        return {};

    // https://drafts.csswg.org/css2/#inline-replaced-width
    // "(used height) * (intrinsic ratio)"
    return compute_inline_size_for_replaced_element(box, available_space, {});
}

CSSPixels FormattingContext::calculate_max_content_inline_size(Layout::Box const& box, ContainingBlockConstraints const& containing_block_constraints) const
{
    auto auto_size = box.auto_content_box_size();
    if (auto transferred_inline_size = calculate_transferred_inline_size_for_replaced_element(box, containing_block_constraints); transferred_inline_size.has_value())
        return transferred_inline_size.value();
    if (!auto_size.has_width()) {
        // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
        // "If the box is non-replaced, then the entire value of any max size property or preferred size property
        // ('width'/'max-width'/'height'/'max-height') specified as an expression containing a percentage [...] that is
        // cyclic is treated for the purpose of calculating the box's intrinsic size contributions only as that
        // property's initial value."
        //
        // This means an `appearance: none` text input with a cyclic `width: 100%` still contributes its `width: auto`
        // size to max-content sizing. Do not use this for min-content sizing: CSS Sizing's "Compressible Replaced
        // Elements" section considers non-button-like <input> controls replaced for the percentage-sized replaced
        // element rule, so their cyclic-percentage min-content contribution can still compress toward zero.
        if (auto default_preferred_size = default_preferred_size_for_appearance_none_text_input(box);
            default_preferred_size.has_value()) {
            auto_size = default_preferred_size.value();
        }
    }
    if (auto_size.has_width())
        return auto_size.width.value();

    Optional<CSSPixels> definite_block_size;
    if (box.is_replaced_box() && !auto_size.has_height()) {
        if (auto const& box_state = m_state.get(box); box_state.has_definite_block_size())
            definite_block_size = box_state.content_block_size();
    }

    auto max_content_available_inline_size = AvailableSize::make_max_content();
    auto intrinsic_available_space = AvailableSpace(max_content_available_inline_size, AvailableSize::make_indefinite());

    auto resolve_destination_inline_size = [&](CSS::Size const& size, CyclicPercentageSizeProperty size_property) -> Optional<CSSPixels> {
        if (!size.is_length_percentage())
            return {};

        switch (cyclic_percentage_intrinsic_contribution(box, size, max_content_available_inline_size, size_property)) {
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            return {};
        case CyclicPercentageIntrinsicContribution::ResolveAsZero: {
            auto zero_percentage_basis_constraints = containing_block_constraints;
            zero_percentage_basis_constraints.percentage_basis_inline_size = 0;
            return calculate_inner_inline_size(box, max_content_available_inline_size, size, zero_percentage_basis_constraints);
        }
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            if (size.contains_percentage() && !containing_block_constraints.percentage_basis_inline_size.has_value())
                return {};
            return calculate_inner_inline_size(box, max_content_available_inline_size, size, containing_block_constraints);
        }
        VERIFY_NOT_REACHED();
    };

    auto resolve_block_size = [&](CSS::Size const& size, CyclicPercentageSizeProperty size_property) -> Optional<CSSPixels> {
        if (!size.is_length_percentage())
            return {};
        if (!size.contains_percentage() || containing_block_constraints.percentage_basis_block_size.has_value())
            return calculate_inner_block_size(box, intrinsic_available_space, size, containing_block_constraints);

        switch (cyclic_percentage_intrinsic_contribution(box, size, max_content_available_inline_size, size_property)) {
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            return {};
        case CyclicPercentageIntrinsicContribution::ResolveAsZero: {
            auto zero_percentage_basis_constraints = containing_block_constraints;
            zero_percentage_basis_constraints.percentage_basis_block_size = 0;
            return calculate_inner_block_size(box, intrinsic_available_space, size, zero_percentage_basis_constraints);
        }
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            return {};
        }
        VERIFY_NOT_REACHED();
    };

    auto definite_minimum_inline_size = resolve_destination_inline_size(box.computed_values().min_width(), CyclicPercentageSizeProperty::MinSize);
    auto definite_minimum_block_size = resolve_block_size(box.computed_values().min_height(), CyclicPercentageSizeProperty::MinSize);
    ReplacedMaxContentSizeConstraints constraints {
        .definite_size_in_ratio_determining_axis = definite_block_size,
        .minimum_inline_size = definite_minimum_inline_size,
        .minimum_block_size = definite_minimum_block_size,
    };

    if (auto max_content_inline_size = max_content_size_for_replaced_element_without_natural_size(box, auto_size, m_state.get(box), SizeDimension::Inline, constraints); max_content_inline_size.has_value()) {
        if (!definite_block_size.has_value() && box.has_preferred_aspect_ratio()) {
            if (auto definite_maximum_block_size = resolve_block_size(box.computed_values().max_height(), CyclicPercentageSizeProperty::PreferredOrMaxSize); definite_maximum_block_size.has_value()) {
                // https://drafts.csswg.org/css-sizing-4/#aspect-ratio-size-transfers
                // First, any definite minimum size is converted and transferred from the origin to destination axis.
                // This transferred minimum is capped by any definite preferred or maximum size in the destination axis.
                Optional<CSSPixels> transferred_minimum;
                if (definite_minimum_block_size.has_value()) {
                    transferred_minimum = content_inline_size_from_aspect_ratio(box, m_state.get(box), definite_minimum_block_size.value());

                    auto cap_transferred_minimum = [&](CSS::Size const& size, CyclicPercentageSizeProperty size_property) {
                        if (auto resolved_size = resolve_destination_inline_size(size, size_property); resolved_size.has_value())
                            transferred_minimum = min(transferred_minimum.value(), resolved_size.value());
                    };
                    cap_transferred_minimum(box.computed_values().width(), CyclicPercentageSizeProperty::PreferredOrMaxSize);
                    cap_transferred_minimum(box.computed_values().max_width(), CyclicPercentageSizeProperty::PreferredOrMaxSize);
                }

                // Then, any definite maximum size is converted and transferred from the origin to destination.
                // This transferred maximum is floored by any definite preferred or minimum size in the destination axis
                // as well as by the transferred minimum, if any.
                auto transferred_maximum = content_inline_size_from_aspect_ratio(box, m_state.get(box), definite_maximum_block_size.value());

                auto floor_transferred_maximum = [&](CSS::Size const& size, CyclicPercentageSizeProperty size_property) {
                    if (auto resolved_size = resolve_destination_inline_size(size, size_property); resolved_size.has_value())
                        transferred_maximum = max(transferred_maximum, resolved_size.value());
                };
                floor_transferred_maximum(box.computed_values().width(), CyclicPercentageSizeProperty::PreferredOrMaxSize);
                floor_transferred_maximum(box.computed_values().min_width(), CyclicPercentageSizeProperty::MinSize);
                if (transferred_minimum.has_value())
                    transferred_maximum = max(transferred_maximum, transferred_minimum.value());

                return min(max_content_inline_size.value(), transferred_maximum);
            }
        }
        return max_content_inline_size.value();
    }

    // Boxes with no children have zero intrinsic inline size.
    if (!box.has_children())
        return 0;

    auto cache_key = intrinsic_size_cache_key(containing_block_constraints);
    auto& cache = box.cached_intrinsic_sizes().max_content_inline_size;
    if (auto cached_value = cache.get(cache_key); cached_value.has_value())
        return cached_value.value();

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);

    auto& box_state = throwaway_state.create(box, containing_block_constraints.percentage_basis_inline_size, containing_block_constraints.percentage_basis_block_size);
    box_state.set_inline_size_constraint(SizeConstraint::MaxContent);
    box_state.set_indefinite_content_inline_size();

    auto context = create_independent_formatting_context(throwaway_state, LayoutMode::IntrinsicSizing, box, const_cast<FormattingContext*>(this));

    auto available_inline_size = AvailableSize::make_max_content();
    auto available_block_size = box_state.has_definite_block_size()
        ? AvailableSize::make_definite(box_state.content_block_size())
        : AvailableSize::make_indefinite();

    auto available_space = AvailableSpace(available_inline_size, available_block_size);
    context->run(LayoutInput { available_space, containing_block_constraints });

    auto max_content_inline_size = clamp_to_max_dimension_value(context->automatic_content_inline_size());
    cache.set(cache_key, max_content_inline_size);
    return max_content_inline_size;
}

// https://www.w3.org/TR/css-sizing-3/#min-content-block-size
CSSPixels FormattingContext::calculate_min_content_block_size(Layout::Box const& box, CSSPixels inline_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    // For block containers, tables, and inline boxes, this is equivalent to the max-content block size.
    if (box.is_block_container() || box.display().is_table_inside())
        return calculate_max_content_block_size(box, inline_size, containing_block_constraints);

    if (auto auto_size = box.auto_content_box_size(); auto_size.has_height()) {
        if (auto_size.has_aspect_ratio())
            return inline_size / auto_size.aspect_ratio.value();
        return auto_size.height.value();
    }

    // Boxes with no children have zero intrinsic height.
    if (!box.has_children())
        return 0;

    auto cache_key = intrinsic_size_cache_key(containing_block_constraints);
    cache_key.measured_at_inline_size = inline_size;
    auto& cache = box.cached_intrinsic_sizes().min_content_block_size;
    if (auto cached_value = cache.get(cache_key); cached_value.has_value())
        return cached_value.value();

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);

    auto& box_state = throwaway_state.create(box, containing_block_constraints.percentage_basis_inline_size, containing_block_constraints.percentage_basis_block_size);
    box_state.set_block_size_constraint(SizeConstraint::MinContent);
    box_state.set_indefinite_content_block_size();
    box_state.set_content_inline_size(inline_size);

    auto context = create_independent_formatting_context(throwaway_state, LayoutMode::IntrinsicSizing, box, const_cast<FormattingContext*>(this));

    auto available_space = AvailableSpace(AvailableSize::make_definite(inline_size), AvailableSize::make_min_content());
    context->run(LayoutInput { available_space, containing_block_constraints });

    auto min_content_block_size = clamp_to_max_dimension_value(context->automatic_content_block_size());
    cache.set(cache_key, min_content_block_size);
    return min_content_block_size;
}

CSSPixels FormattingContext::calculate_max_content_block_size(Layout::Box const& box, CSSPixels inline_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    if (box.has_preferred_aspect_ratio())
        return inline_size / *box.preferred_aspect_ratio();

    if (auto auto_size = box.auto_content_box_size(); auto_size.has_height())
        return auto_size.height.value();
    if (auto max_content_block_size = max_content_size_for_replaced_element_without_natural_size(box, box.auto_content_box_size(), m_state.get(box), SizeDimension::Block); max_content_block_size.has_value())
        return max_content_block_size.value();

    // Boxes with no children have zero intrinsic height.
    if (!box.has_children())
        return 0;

    auto cache_key = intrinsic_size_cache_key(containing_block_constraints);
    cache_key.measured_at_inline_size = inline_size;
    auto& cache = box.cached_intrinsic_sizes().max_content_block_size;
    if (auto cached_value = cache.get(cache_key); cached_value.has_value())
        return cached_value.value();

    LayoutState throwaway_state(box, LayoutState::Purpose::Measurement);

    auto& box_state = throwaway_state.create(box, containing_block_constraints.percentage_basis_inline_size, containing_block_constraints.percentage_basis_block_size);
    box_state.set_block_size_constraint(SizeConstraint::MaxContent);
    box_state.set_indefinite_content_block_size();
    box_state.set_content_inline_size(inline_size);

    auto context = create_independent_formatting_context(throwaway_state, LayoutMode::IntrinsicSizing, box, const_cast<FormattingContext*>(this));

    auto available_space = AvailableSpace(AvailableSize::make_definite(inline_size), AvailableSize::make_max_content());
    context->run(LayoutInput { available_space, containing_block_constraints });

    auto max_content_block_size = clamp_to_max_dimension_value(context->automatic_content_block_size());
    cache.set(cache_key, max_content_block_size);
    return max_content_block_size;
}

CSSPixels FormattingContext::calculate_inner_inline_size(Layout::Box const& box, AvailableSize const& available_inline_size, CSS::Size const& preferred_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    VERIFY(!preferred_size.is_auto());

    auto const& box_state = m_state.get(box);
    auto containing_block_inline_size = preferred_size.contains_percentage()
        ? containing_block_constraints.percentage_basis_inline_size.value_or(available_inline_size.to_px_or_zero())
        : available_inline_size.to_px_or_zero();
    if (preferred_size.is_fit_content()) {
        return calculate_fit_content_inline_size(box, AvailableSpace { available_inline_size, AvailableSize::make_indefinite() }, containing_block_constraints);
    }
    if (preferred_size.is_max_content()) {
        return calculate_max_content_inline_size(box, containing_block_constraints);
    }
    if (preferred_size.is_min_content()) {
        return calculate_min_content_inline_size(box, containing_block_constraints);
    }

    auto& computed_values = box.computed_values();
    if (computed_values.box_sizing() == CSS::BoxSizing::BorderBox) {
        auto inner_inline_size = preferred_size.to_px(containing_block_inline_size)
            - computed_values.border_left().width
            - box_state.padding_left()
            - computed_values.border_right().width
            - box_state.padding_right();
        return max(inner_inline_size, 0);
    }

    return preferred_size.to_px(containing_block_inline_size);
}

CSSPixels FormattingContext::calculate_inner_block_size(Box const& box, AvailableSpace const& available_space, CSS::Size const& preferred_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    auto const& box_state = m_state.get(box);

    if (preferred_size.is_auto() && box.has_preferred_aspect_ratio())
        return content_block_size_from_aspect_ratio(box, box_state);

    VERIFY(!preferred_size.is_auto());

    if (preferred_size.is_fit_content()) {
        return calculate_fit_content_block_size(box, available_space, containing_block_constraints);
    }
    if (preferred_size.is_max_content()) {
        return calculate_max_content_block_size(box, available_space.inline_size.to_px_or_zero(), containing_block_constraints);
    }
    if (preferred_size.is_min_content()) {
        return calculate_min_content_block_size(box, available_space.inline_size.to_px_or_zero(), containing_block_constraints);
    }

    CSSPixels containing_block_block_size = available_space.block_size.to_px_or_zero();
    // NOTE: Percentage heights are resolved against the containing block's used height,
    //       not the available space height. The containing block's height must be definite
    //       for percentage resolution to work (otherwise should_treat_block_size_as_auto
    //       should have returned true and we wouldn't be here).
    // NOTE: We only do this when available space height is indefinite. If it's definite,
    //       we trust that the caller has set it up correctly (e.g., grid/flex items get
    //       their cell/area size as available space).
    if (preferred_size.contains_percentage() && available_space.block_size.is_indefinite()) {
        // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
        // NOTE: Flex/grid items resolve percentage heights against their container, not via quirk.
        bool is_flex_or_grid_item = box.parent() && (box.parent()->display().is_flex_inside() || box.parent()->display().is_grid_inside());
        auto shadow_root = box.dom_node() ? box.dom_node()->containing_shadow_root() : nullptr;
        bool is_in_ua_shadow_tree = shadow_root && shadow_root->is_user_agent_internal();
        if (box.document().in_quirks_mode() && !box.is_anonymous() && !is_flex_or_grid_item && !is_in_ua_shadow_tree) {
            containing_block_block_size = containing_block_constraints.quirks_mode_percentage_basis_block_size.value_or(0);
        } else if (containing_block_constraints.percentage_basis_block_size.has_value()) {
            containing_block_block_size = containing_block_constraints.percentage_basis_block_size.value();
        }
    }
    auto& computed_values = box.computed_values();

    if (computed_values.box_sizing() == CSS::BoxSizing::BorderBox) {
        auto inner_block_size = preferred_size.to_px(containing_block_block_size)
            - computed_values.border_top().width
            - box_state.padding_top()
            - computed_values.border_bottom().width
            - box_state.padding_bottom();
        return max(inner_block_size, 0);
    }

    return preferred_size.to_px(containing_block_block_size);
}

// https://drafts.csswg.org/css-sizing-3/#stretch-fit-size
CSSPixels FormattingContext::calculate_stretch_fit_inline_size(Box const& box, AvailableSize const& available_inline_size) const
{
    // The size a box would take if its outer size filled the available space in the given axis;
    // in other words, the stretch fit into the available space, if that is definite.

    // Undefined if the available space is indefinite.
    if (!available_inline_size.is_definite())
        return 0;

    auto const& box_state = m_state.get(box);
    return available_inline_size.to_px_or_zero()
        - box_state.margin_left()
        - box_state.margin_right()
        - box_state.padding_left()
        - box_state.padding_right()
        - box_state.border_left()
        - box_state.border_right();
}

// https://drafts.csswg.org/css-sizing-3/#stretch-fit-size
CSSPixels FormattingContext::calculate_stretch_fit_block_size(Box const& box, AvailableSize const& available_block_size) const
{
    // The size a box would take if its outer size filled the available space in the given axis;
    // in other words, the stretch fit into the available space, if that is definite.
    // Undefined if the available space is indefinite.
    auto const& box_state = m_state.get(box);
    return available_block_size.to_px_or_zero()
        - box_state.margin_top()
        - box_state.margin_bottom()
        - box_state.padding_top()
        - box_state.padding_bottom()
        - box_state.border_top()
        - box_state.border_bottom();
}

bool FormattingContext::should_treat_inline_size_as_auto(Box const& box, AvailableSpace const& available_space) const
{
    auto const& computed_inline_size = box.computed_values().width();
    if (computed_inline_size.is_auto())
        return true;

    // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
    if (computed_inline_size.contains_percentage()) {
        switch (cyclic_percentage_intrinsic_contribution(box, computed_inline_size, available_space.inline_size, CyclicPercentageSizeProperty::PreferredOrMaxSize)) {
        case CyclicPercentageIntrinsicContribution::ResolveAsZero:
            return false;
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            return true;
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            break;
        }
        if (available_space.inline_size.is_indefinite())
            return true;
    }
    // AD-HOC: If the box has a preferred aspect ratio and an intrinsic keyword for width...
    if (box.has_preferred_aspect_ratio() && computed_inline_size.is_intrinsic_sizing_constraint()) {
        // If the box has no natural height to resolve the aspect ratio, we treat the width as auto.
        if (!box.auto_content_box_size().has_height())
            return true;
        // If the box has definite height, we can resolve the width through the aspect ratio.
        if (m_state.get(box).has_definite_block_size())
            return true;
    }
    return false;
}

bool FormattingContext::should_treat_block_size_as_auto(Box const& box, AvailableSpace const& available_space, ContainingBlockConstraints const& containing_block_constraints) const
{
    auto computed_block_size = box.computed_values().height();
    if (computed_block_size.is_auto()) {
        auto const& box_state = m_state.get(box);
        if (box_state.has_definite_inline_size() && box.has_preferred_aspect_ratio())
            return false;
        return true;
    }

    // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
    if (computed_block_size.contains_percentage()) {
        switch (cyclic_percentage_intrinsic_contribution(box, computed_block_size, available_space.block_size, CyclicPercentageSizeProperty::PreferredOrMaxSize)) {
        case CyclicPercentageIntrinsicContribution::ResolveAsZero:
            return false;
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            return true;
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            break;
        }
        // https://www.w3.org/TR/CSS22/visudet.html#the-height-property
        // If the height of the containing block is not specified explicitly (i.e., it depends on
        // content height), and this element is not absolutely positioned, the percentage value
        // is treated as 'auto'.
        // https://quirks.spec.whatwg.org/#the-percentage-height-calculation-quirk
        // In quirks mode, percentage heights can resolve even without explicit containing block
        // height. The quirk applies to DOM elements only (not anonymous boxes), and excludes
        // table-related display types.
        if (!box.is_absolutely_positioned()) {
            auto percentage_block_size_quirk_applies = [&] {
                if (!box.document().in_quirks_mode() || box.is_anonymous())
                    return false;
                if (box.display().is_table_inside())
                    return false;
                // Flex/grid items resolve percentage heights against their container, not via quirk.
                if (auto* parent = box.parent(); parent && parent->display().is_flex_inside())
                    return false;
                if (auto* parent = box.parent(); parent && parent->display().is_grid_inside())
                    return false;
                // The quirk should not apply inside user agent shadow trees.
                if (auto const* dom_node = box.dom_node()) {
                    if (auto shadow_root = dom_node->containing_shadow_root(); shadow_root && shadow_root->is_user_agent_internal())
                        return false;
                }
                return true;
            }();
            if (!percentage_block_size_quirk_applies) {
                if (!containing_block_constraints.percentage_basis_block_size.has_value())
                    return true;
            }
        }
    }

    // AD-HOC: If the box has a preferred aspect ratio and an intrinsic keyword for height...
    if (box.has_preferred_aspect_ratio() && computed_block_size.is_intrinsic_sizing_constraint()) {
        // If the box has no natural width to resolve the aspect ratio, we treat the height as auto.
        if (!box.auto_content_box_size().has_width())
            return true;
        // If the box has definite width, we can resolve the height through the aspect ratio.
        if (m_state.get(box).has_definite_inline_size())
            return true;
    }
    return false;
}

bool FormattingContext::can_skip_is_anonymous_text_run(Box& box)
{
    if (box.is_anonymous() && !box.is_generated_for_pseudo_element() && !box.first_child_of_type<BlockContainer>()) {
        bool contains_only_white_space = true;
        box.for_each_in_subtree([&](auto const& node) {
            if (!is<TextNode>(node) || !static_cast<TextNode const&>(node).text().is_ascii_whitespace()) {
                contains_only_white_space = false;
                return TraversalDecision::Break;
            }
            return TraversalDecision::Continue;
        });
        if (contains_only_white_space)
            return true;
    }
    return false;
}

void FormattingContext::compute_and_store_baselines(LayoutState::UsedValues& used_values) const
{
    // NOTE: This may run more than once for the same UsedValues (e.g. table cells are laid out twice),
    //       so reset both baselines before deriving them anew.
    used_values.set_first_baseline({});
    used_values.set_last_baseline({});

    auto const& box = as<Box>(used_values.node());

    auto line_count = m_state.rust_line_count(box);
    if (line_count > 0) {
        auto baseline_for_line_box = [&](size_t line_index, BaselineSet baseline_set) -> CSSPixels {
            auto line = m_state.rust_line_summary(box, line_index);
            VERIFY(line.has_value());
            if (!line->has_block_level_box) {
                auto line_box_block_start = CSSPixels::from_raw(line->physical_vertical_end) - CSSPixels::from_raw(line->block_length);
                return line_box_block_start + CSSPixels::from_raw(line->baseline);
            }

            VERIFY(line->fragment_count == 1);
            auto const* fragment_node = m_state.rust_line_first_fragment_node(box, line_index);
            VERIFY(fragment_node);
            auto const& block_child = as<Box>(*fragment_node);
            auto const& block_child_state = m_state.get(block_child);
            auto child_offset_from_margin_edge = block_child_state.content_logical_offset().block_offset - block_child_state.margin_box_top();
            return child_offset_from_margin_edge + box_baseline(block_child, baseline_set);
        };

        size_t first_line_index = 0;
        for (; first_line_index < line_count; ++first_line_index) {
            auto line = m_state.rust_line_summary(box, first_line_index);
            VERIFY(line.has_value());
            if (!line->is_empty)
                break;
        }
        if (first_line_index == line_count)
            first_line_index = 0;
        used_values.set_first_baseline(baseline_for_line_box(first_line_index, BaselineSet::First));

        size_t last_line_index = line_count - 1;
        while (last_line_index > 0) {
            auto line = m_state.rust_line_summary(box, last_line_index);
            VERIFY(line.has_value());
            if (!line->is_empty)
                break;
            --last_line_index;
        }
        used_values.set_last_baseline(baseline_for_line_box(last_line_index, BaselineSet::Last));
        return;
    }

    if (!box.has_children() || box.children_are_inline())
        return;

    // Derive baselines from the first/last in-flow child that has a baseline set of its own.
    // https://drafts.csswg.org/css-flexbox-1/#flex-baselines
    // Otherwise, if the flex container has at least one flex item, the flex container's first/last main-axis baseline
    // set is generated from the alignment baseline of the startmost/endmost flex item.
    // https://drafts.csswg.org/css-grid-1/#grid-baselines
    // Otherwise, the grid container's first (last) baseline set is generated from the alignment baseline of the first
    // (last) grid item in row-major grid order.
    // FIXME: This does not yet select the spec-defined startmost/endmost flex item, or the first/last grid item in
    //        row-major grid order.
    auto baseline_from_children = [&](BaselineSet baseline_set) -> Optional<CSSPixels> {
        auto deriving_first_baseline = baseline_set == BaselineSet::First;
        for (auto child = deriving_first_baseline ? box.first_child() : box.last_child(); child;
            child = deriving_first_baseline ? child->next_sibling() : child->previous_sibling()) {
            auto const* child_box = as_if<Box>(*child);
            if (!child_box)
                continue;
            if (child_box->is_out_of_flow(*this))
                continue;
            auto const* child_state = m_state.try_get(*child_box);
            if (!child_state)
                continue;
            auto const& child_baseline = deriving_first_baseline ? child_state->first_baseline() : child_state->last_baseline();
            if (!child_baseline.has_value())
                continue;
            auto child_offset_from_margin_edge = child_state->content_logical_offset().block_offset - child_state->margin_box_top();
            return child_offset_from_margin_edge + box_baseline(*child_box, baseline_set);
        }
        return {};
    };
    used_values.set_first_baseline(baseline_from_children(BaselineSet::First));
    used_values.set_last_baseline(baseline_from_children(BaselineSet::Last));
}

CSSPixels FormattingContext::box_baseline(Box const& box, BaselineSet baseline_set) const
{
    auto const& box_state = m_state.get(box);

    // https://drafts.csswg.org/css2/#propdef-vertical-align
    auto const& vertical_align = box.computed_values().vertical_align();
    if (box.vertical_align_applies() && vertical_align.has<CSS::VerticalAlign>()) {
        switch (vertical_align.get<CSS::VerticalAlign>()) {
        case CSS::VerticalAlign::Top:
            // Top: Align the top of the aligned subtree with the top of the line box.
            return box_state.border_box_top();
        case CSS::VerticalAlign::Middle:
            // Middle: Align the vertical midpoint of the box with the baseline of the parent box plus half the x-height of the parent.
            return box_state.margin_box_block_size() / 2 + CSSPixels::nearest_value_for(box.containing_block()->first_available_font().pixel_metrics().x_height / 2);
        case CSS::VerticalAlign::Bottom:
            // Bottom: Align the bottom of the aligned subtree with the bottom of the line box.
            return box_state.content_block_size() + box_state.margin_box_top();
        case CSS::VerticalAlign::TextTop:
            // TextTop: Align the top of the box with the top of the parent's content area (see 10.6.1).
            return box.computed_values().font_size();
        case CSS::VerticalAlign::TextBottom:
            // TextBottom: Align the bottom of the box with the bottom of the parent's content area (see 10.6.1).
            return box_state.margin_box_block_size() - CSSPixels::nearest_value_for(box.containing_block()->first_available_font().pixel_metrics().descent);
        default:
            break;
        }
    }

    // https://drafts.csswg.org/css-inline-3/#baseline-source
    // auto: Specifies last-baseline alignment for inline-block, first-baseline alignment for everything else.
    // NB: Callers ask an inline-level box for its last baseline set, since that is what CSS2's inline-block rule below
    //     describes; inline-level flex and grid containers participate with their first baseline set instead.
    auto const& display = box.display();
    bool is_flex_or_grid_container = display.is_flex_inside() || display.is_grid_inside();
    if (display.is_inline_outside() && is_flex_or_grid_container)
        baseline_set = BaselineSet::First;

    // https://drafts.csswg.org/css2/#propdef-vertical-align
    // The baseline of an 'inline-block' is the baseline of its last line box in the normal flow, unless it has either
    // no in-flow line boxes or if its 'overflow' property has a computed value other than 'visible', in which case the
    // baseline is the bottom margin edge.
    // https://drafts.csswg.org/css-align-3/#baseline-rules
    // CSS Align restates this overflow exception as only applying to the last baseline set: "for legacy reasons if its
    // baseline-source is auto (the initial value) a block-level or inline-level block container that is a scroll
    // container always has a last baseline set, whose baselines all correspond to its block-end margin edge". First
    // baseline sets always derive from content; so do flex and grid containers, which are not block containers.
    // FIXME: Per CSS Align, a scroll container's content-derived baseline position should be clamped to its border
    //        edge.
    auto const& overflow_x = box.computed_values().overflow_x();
    auto const& overflow_y = box.computed_values().overflow_y();
    bool has_visible_overflow = overflow_x == CSS::Overflow::Visible && overflow_y == CSS::Overflow::Visible;
    bool derive_baseline_from_content = baseline_set == BaselineSet::First || is_flex_or_grid_container || has_visible_overflow;

    // AD-HOC: We also use the content-derived baseline for <input> elements with block children. Per the HTML spec,
    //         inputs have `overflow: clip !important`, so CSS2 says to use bottom margin edge. However, the internal
    //         shadow tree baseline should determine the control's baseline for proper alignment with adjacent text.
    //         https://html.spec.whatwg.org/multipage/rendering.html#form-controls
    bool input_derives_from_children = is<HTML::HTMLInputElement>(box.dom_node()) && !box.children_are_inline();

    auto const& content_baseline = baseline_set == BaselineSet::First ? box_state.first_baseline() : box_state.last_baseline();
    if (content_baseline.has_value() && (derive_baseline_from_content || input_derives_from_children))
        return box_state.margin_box_top() + *content_baseline;

    // If the box has no baseline set, the bottom margin edge of the box is used.
    return box_state.margin_box_block_size();
}

CSSPixelRect FormattingContext::margin_box_rect(LayoutState::UsedValues const& used_values)
{
    return {
        {
            -max(used_values.margin_box_left(), 0),
            -max(used_values.margin_box_top(), 0),
        },
        {
            max(used_values.margin_box_left(), 0) + used_values.content_inline_size() + max(used_values.margin_box_right(), 0),
            max(used_values.margin_box_top(), 0) + used_values.content_block_size() + max(used_values.margin_box_bottom(), 0),
        },
    };
}

CSSPixelRect FormattingContext::content_box_rect(Box const& box) const
{
    return content_box_rect(m_state.get(box));
}

CSSPixelRect FormattingContext::content_box_rect(LayoutState::UsedValues const& used_values) const
{
    return CSSPixelRect { used_values.content_offset(), used_values.content_size() };
}

bool FormattingContext::should_treat_max_inline_size_as_none(Box const& box, AvailableSize const& available_inline_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    auto const& max_inline_size = box.computed_values().max_width();
    if (max_inline_size.is_none())
        return true;
    if (available_inline_size.is_max_content() && max_inline_size.is_max_content())
        return true;
    // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
    if (max_inline_size.contains_percentage()) {
        switch (cyclic_percentage_intrinsic_contribution(box, max_inline_size, available_inline_size, CyclicPercentageSizeProperty::PreferredOrMaxSize)) {
        case CyclicPercentageIntrinsicContribution::ResolveAsZero:
            return false;
        case CyclicPercentageIntrinsicContribution::TreatAsInitialValue:
            return true;
        case CyclicPercentageIntrinsicContribution::NotCyclic:
            break;
        }
        if (!containing_block_constraints.percentage_basis_inline_size.has_value())
            return true;
    }
    if (max_inline_size.is_fit_content() && available_inline_size.is_intrinsic_sizing_constraint())
        return true;
    if (max_inline_size.is_max_content() && available_inline_size.is_max_content())
        return true;
    if (max_inline_size.is_min_content() && available_inline_size.is_min_content())
        return true;
    return false;
}

bool FormattingContext::should_treat_max_block_size_as_none(Box const& box, AvailableSize const& available_block_size, ContainingBlockConstraints const& containing_block_constraints) const
{
    // https://www.w3.org/TR/CSS22/visudet.html#min-max-heights
    // If the height of the containing block is not specified explicitly (i.e., it depends on content height),
    // and this element is not absolutely positioned, the percentage value is treated as '0' (for 'min-height')
    // or 'none' (for 'max-height').
    auto const& max_block_size = box.computed_values().max_height();
    if (max_block_size.is_none())
        return true;
    if (max_block_size.contains_percentage()) {
        if (available_block_size.is_min_content())
            return false;
        if (!containing_block_constraints.percentage_basis_block_size.has_value())
            return true;
    }
    if (max_block_size.is_fit_content() && available_block_size.is_intrinsic_sizing_constraint())
        return true;
    if (max_block_size.is_max_content() && available_block_size.is_max_content())
        return true;
    if (max_block_size.is_min_content() && available_block_size.is_min_content())
        return true;
    return false;
}

FormattingContext::CyclicPercentageIntrinsicContribution FormattingContext::cyclic_percentage_intrinsic_contribution(Box const& box, CSS::Size const& size, AvailableSize const& available_size, CyclicPercentageSizeProperty size_property) const
{
    if (!size.contains_percentage())
        return CyclicPercentageIntrinsicContribution::NotCyclic;

    // https://drafts.csswg.org/css-sizing-3/#cyclic-percentage-contribution
    // For the min size properties, as well as for margins and paddings (and gutters), a cyclic percentage is resolved
    // against zero for determining intrinsic size contributions.
    if (size_property == CyclicPercentageSizeProperty::MinSize && available_size.is_intrinsic_sizing_constraint())
        return CyclicPercentageIntrinsicContribution::ResolveAsZero;

    // If the box is non-replaced, then the entire value of any max size property or preferred size property
    // ('width'/'max-width'/'height'/'max-height') specified as an expression containing a percentage (such as '10%' or
    // 'calc(10px + 0%)') that is cyclic is treated for the purpose of calculating the box's intrinsic size contributions
    // only as that property's initial value.
    if (available_size.is_min_content()) {
        // If the box is replaced, a cyclic percentage in the value of any max size property or preferred size property
        // ('width'/'max-width'/'height'/'max-height'), is resolved against zero when calculating the min-content
        // contribution in the corresponding axis.
        if (box.is_replaced_box())
            return CyclicPercentageIntrinsicContribution::ResolveAsZero;
        return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
    }

    if (available_size.is_max_content()) {
        // Likewise, if the box is replaced, then the entire value of any max size property or preferred size property
        // specified as an expression containing a percentage that is cyclic is treated for the purpose of calculating
        // the box's max-content contributions only as that property's initial value.
        return CyclicPercentageIntrinsicContribution::TreatAsInitialValue;
    }

    return CyclicPercentageIntrinsicContribution::NotCyclic;
}

}
