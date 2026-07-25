/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Atomic.h>
#include <AK/HashMap.h>
#include <AK/NeverDestroyed.h>
#include <AK/NumericLimits.h>
#include <AK/ScopeGuard.h>
#include <AK/Utf16StringBuilder.h>
#include <AK/Variant.h>
#include <AK/Utf16StringBuilder.h>
#include <LibWeb/CSS/ComputedValues.h>
#include <LibWeb/CSS/Display.h>
#include <LibWeb/CSS/GridTrackPlacement.h>
#include <LibWeb/CSS/GridTrackSize.h>
#include <LibWeb/CSS/LengthBox.h>
#include <LibWeb/CSS/PropertyNameAndID.h>
#include <LibWeb/CSS/Size.h>
#include <LibWeb/CSS/StyleValues/AnchorStyleValue.h>
#include <LibWeb/CSS/StyleValues/CalcNodeRef.h>
#include <LibWeb/CSS/StyleValues/CalculatedStyleValue.h>
#include <LibWeb/CSS/StyleValues/FlexStyleValue.h>
#include <LibWeb/CSS/StyleValues/FunctionStyleValue.h>
#include <LibWeb/CSS/StyleValues/GridTrackSizeListStyleValue.h>
#include <LibWeb/CSS/StyleValues/IntegerStyleValue.h>
#include <LibWeb/CSS/StyleValues/LengthStyleValue.h>
#include <LibWeb/CSS/StyleValues/PercentageStyleValue.h>
#include <LibWeb/CSS/ValueType.h>
#include <LibUnicode/Bidi.h>
#include <LibWeb/DOM/AbstractElement.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/DOM/Position.h>
#include <LibWeb/DOM/ShadowRoot.h>
#include <LibWeb/DOM/Text.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Path.h>
#include <LibGfx/TextLayout.h>
#include <LibWeb/HTML/AttributeNames.h>
#include <LibWeb/HTML/HTMLElement.h>
#include <LibWeb/HTML/HTMLInputElement.h>
#include <LibWeb/HTML/HTMLTableCellElement.h>
#include <LibWeb/HTML/HTMLTableColElement.h>
#include <LibWeb/HTML/FormAssociatedElement.h>
#include <LibWeb/Layout/DominantBaseline.h>
#include <LibWeb/Layout/FieldSetBox.h>
#include <LibWeb/Layout/FlexLayoutData.h>
#include <LibWeb/Layout/GridLayoutData.h>
#include <LibWeb/Layout/LayoutRustBridge.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/AbsposLayoutInputs.h>
#include <LibWeb/Layout/InlineNode.h>
#include <LibWeb/Layout/ListItemBox.h>
#include <LibWeb/Layout/ListItemMarkerBox.h>
#include <LibWeb/Layout/Node.h>
#include <LibWeb/Layout/SVGClipBox.h>
#include <LibWeb/Layout/SVGGeometryBox.h>
#include <LibWeb/Layout/SVGImageBox.h>
#include <LibWeb/Layout/SVGMaskBox.h>
#include <LibWeb/Layout/SVGPatternBox.h>
#include <LibWeb/Layout/SVGSVGBox.h>
#include <LibWeb/Layout/SVGTextBox.h>
#include <LibWeb/Layout/SVGTextPathBox.h>
#include <LibWeb/Layout/TextInputBox.h>
#include <LibWeb/Layout/TextNode.h>
#include <LibWeb/Painting/PaintableWithLines.h>
#include <LibWeb/Painting/SVGForeignObjectPaintable.h>
#include <LibWeb/Painting/SVGGraphicsPaintable.h>
#include <LibWeb/Painting/SVGPathPaintable.h>
#include <LibWeb/Painting/SVGSVGPaintable.h>
#include <LibWeb/SVG/SVGAElement.h>
#include <LibWeb/SVG/SVGClipPathElement.h>
#include <LibWeb/SVG/SVGForeignObjectElement.h>
#include <LibWeb/SVG/SVGGElement.h>
#include <LibWeb/SVG/SVGMaskElement.h>
#include <LibWeb/SVG/SVGSVGElement.h>
#include <LibWeb/SVG/SVGSwitchElement.h>
#include <LibWeb/SVG/SVGSymbolElement.h>
#include <LibWeb/SVG/SVGUseElement.h>

namespace Web::Layout {

static Atomic<size_t> s_outstanding_calc_handles;
static Atomic<size_t> s_outstanding_grid_name_handles;
static Atomic<size_t> s_outstanding_anchor_name_handles;
static Atomic<size_t> s_outstanding_svg_path_handles;
static Atomic<size_t> s_outstanding_shaped_run_handles;

struct TextFactsSnapshotArena {
    Vector<u16> text;
    Vector<RustFFI::FfiTextChunk> chunks;
};

struct UsedGridTracksCommitData {
    RefPtr<CSS::GridTrackSizeListStyleValue const> columns;
    RefPtr<CSS::GridTrackSizeListStyleValue const> rows;
};

static bool is_empty_editable_text_node(TextNode const& text_node)
{
    if (!text_node.text_for_rendering().is_empty())
        return false;
    auto const* dom_text = text_node.dom_text();
    if (!dom_text)
        return false;

    auto is_empty_editable = false;
    if (auto const* shadow_root = as_if<DOM::ShadowRoot>(dom_text->root())) {
        if (auto const* form_associated_element = as_if<HTML::FormAssociatedTextControlElement>(shadow_root->host()))
            is_empty_editable = form_associated_element->text_control_to_html_element().is_mutable();
    }
    is_empty_editable |= dom_text->parent() && dom_text->parent()->is_editing_host();
    return is_empty_editable;
}

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
        .contains_anchor_function = false,
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
        .contains_anchor_function = calculated.contains_anchor_function(),
    };
}

static constexpr u32 no_grid_index = static_cast<u32>(-1);

static Utf16FlyString make_grid_implicit_line_name(Utf16View name, StringView suffix)
{
    Utf16StringBuilder builder;
    builder.append(name);
    builder.append_ascii(suffix);
    auto line_name = builder.to_string();
    return Utf16FlyString::from_utf16(line_name.utf16_view());
}

struct GridFactsSnapshotArena {
    Vector<size_t> names;
    Vector<u32> name_indices;
    Vector<RustFFI::FfiGridTrackEntry> entries;
    Vector<RustFFI::FfiGridArea> areas;
    HashMap<Utf16FlyString, u32> indices_by_name;

    u32 intern_name(Utf16FlyString const& name)
    {
        if (auto existing = indices_by_name.get(name); existing.has_value())
            return existing.value();

        VERIFY(names.size() < no_grid_index);
        auto index = static_cast<u32>(names.size());
        names.append(name.to_raw_leaked());
        indices_by_name.set(name, index);
        ++s_outstanding_grid_name_handles;
        RustFFI::rust_layout_ffi_note_grid_name_retain();
        return index;
    }
};

static RustFFI::FfiGridTrackBreadth grid_track_breadth(CSS::GridSize const& grid_size)
{
    if (grid_size.is_flexible_length()) {
        return {
            .kind = to_underlying(RustFFI::FfiGridTrackBreadthKind::Flex),
            .value = size_value_with_kind(RustFFI::FfiSizeKind::Auto),
            .flex_factor = grid_size.flex_factor(),
        };
    }

    auto size = grid_size.css_size();
    auto kind = [&] {
        switch (size.type()) {
        case CSS::Size::Type::Auto:
            return RustFFI::FfiGridTrackBreadthKind::Auto;
        case CSS::Size::Type::Calculated:
        case CSS::Size::Type::Length:
        case CSS::Size::Type::Percentage:
            return RustFFI::FfiGridTrackBreadthKind::LengthPercentage;
        case CSS::Size::Type::MinContent:
            return RustFFI::FfiGridTrackBreadthKind::MinContent;
        case CSS::Size::Type::MaxContent:
            return RustFFI::FfiGridTrackBreadthKind::MaxContent;
        case CSS::Size::Type::FitContent:
            return RustFFI::FfiGridTrackBreadthKind::FitContent;
        case CSS::Size::Type::None:
            VERIFY_NOT_REACHED();
        }
        VERIFY_NOT_REACHED();
    }();
    return {
        .kind = to_underlying(kind),
        .value = build_style_size_value(size),
        .flex_factor = 0,
    };
}

static RustFFI::FfiGridTrackList build_grid_track_list(CSS::GridTrackSizeList const& list, GridFactsSnapshotArena& arena)
{
    RustFFI::FfiGridTrackList result {
        .is_subgrid = list.is_subgrid(),
        .preserves_line_name_sets = list.preserves_line_name_sets(),
        .first_entry = no_grid_index,
    };
    u32 previous_entry = no_grid_index;

    for (auto const& item : list.list()) {
        VERIFY(arena.entries.size() < no_grid_index);
        auto entry_index = static_cast<u32>(arena.entries.size());
        auto auto_breadth = grid_track_breadth(CSS::GridSize::make_auto());
        arena.entries.append({
            .kind = 0,
            .next_sibling = no_grid_index,
            .name_index_start = 0,
            .name_index_count = 0,
            .size = auto_breadth,
            .min_size = auto_breadth,
            .max_size = auto_breadth,
            .repeat_type = 0,
            .repeat_count = 0,
            .repeat_list = {
                .is_subgrid = false,
                .preserves_line_name_sets = false,
                .first_entry = no_grid_index,
            },
        });

        if (result.first_entry == no_grid_index)
            result.first_entry = entry_index;
        if (previous_entry != no_grid_index)
            arena.entries[previous_entry].next_sibling = entry_index;
        previous_entry = entry_index;

        item.visit(
            [&](CSS::GridLineNames const& line_names) {
                auto name_index_start = arena.name_indices.size();
                for (auto const& line_name : line_names.names())
                    arena.name_indices.append(arena.intern_name(line_name.name));
                auto& entry = arena.entries[entry_index];
                entry.kind = to_underlying(RustFFI::FfiGridTrackEntryKind::LineNames);
                entry.name_index_start = name_index_start;
                entry.name_index_count = arena.name_indices.size() - name_index_start;
            },
            [&](CSS::ExplicitGridTrack const& track) {
                if (track.is_default()) {
                    auto& entry = arena.entries[entry_index];
                    entry.kind = to_underlying(RustFFI::FfiGridTrackEntryKind::TrackSize);
                    entry.size = grid_track_breadth(track.grid_size());
                    return;
                }
                if (track.is_minmax()) {
                    auto& entry = arena.entries[entry_index];
                    entry.kind = to_underlying(RustFFI::FfiGridTrackEntryKind::MinMax);
                    entry.min_size = grid_track_breadth(track.minmax().min_grid_size());
                    entry.max_size = grid_track_breadth(track.minmax().max_grid_size());
                    return;
                }

                auto const& repeat = track.repeat();
                auto repeat_list = build_grid_track_list(repeat.grid_track_size_list(), arena);
                auto& entry = arena.entries[entry_index];
                entry.kind = to_underlying(RustFFI::FfiGridTrackEntryKind::Repeat);
                entry.repeat_type = to_underlying(repeat.type());
                entry.repeat_count = repeat.is_fixed() ? repeat.repeat_count() : 0;
                entry.repeat_list = repeat_list;
            });
    }
    return result;
}

static RustFFI::FfiGridPlacement build_grid_placement(CSS::GridTrackPlacement const& placement, GridFactsSnapshotArena& arena)
{
    RustFFI::FfiGridPlacement result {
        .kind = to_underlying(RustFFI::FfiGridPlacementKind::Auto),
        .has_line_number = false,
        .line_number = 0,
        .has_name = false,
        .name_index = no_grid_index,
        .implicit_start_name_index = no_grid_index,
        .implicit_end_name_index = no_grid_index,
    };
    if (placement.is_auto())
        return result;

    if (placement.is_span()) {
        result.kind = to_underlying(RustFFI::FfiGridPlacementKind::Span);
        result.has_line_number = true;
        result.line_number = CSS::int_from_style_value(placement.span());
        if (placement.span_name().has_value()) {
            result.has_name = true;
            result.name_index = arena.intern_name(*placement.span_name());
        }
        return result;
    }

    result.kind = to_underlying(RustFFI::FfiGridPlacementKind::Line);
    if (placement.has_line_number()) {
        result.has_line_number = true;
        result.line_number = CSS::int_from_style_value(placement.line_number());
    }
    if (placement.has_identifier()) {
        result.has_name = true;
        result.name_index = arena.intern_name(placement.identifier());
        result.implicit_start_name_index = arena.intern_name(
            make_grid_implicit_line_name(placement.identifier().view(), "-start"sv));
        result.implicit_end_name_index = arena.intern_name(
            make_grid_implicit_line_name(placement.identifier().view(), "-end"sv));
    }
    return result;
}

static RustFFI::FfiGridStyleFacts build_grid_style_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_grid_facts_build();
    auto arena = make<GridFactsSnapshotArena>();
    auto const& values = node.computed_values();

    auto template_columns = build_grid_track_list(values.grid_template_columns(), *arena);
    auto template_rows = build_grid_track_list(values.grid_template_rows(), *arena);
    auto auto_columns = build_grid_track_list(values.grid_auto_columns(), *arena);
    auto auto_rows = build_grid_track_list(values.grid_auto_rows(), *arena);

    auto const& template_areas = values.grid_template_areas();
    arena->areas.ensure_capacity(template_areas.areas.size());
    for (auto const& [name, area] : template_areas.areas) {
        arena->areas.unchecked_append({
            .name_index = arena->intern_name(name),
            .implicit_start_name_index = arena->intern_name(
                make_grid_implicit_line_name(name.view(), "-start"sv)),
            .implicit_end_name_index = arena->intern_name(
                make_grid_implicit_line_name(name.view(), "-end"sv)),
            .row_start = area.row_start,
            .row_end = area.row_end,
            .column_start = area.column_start,
            .column_end = area.column_end,
        });
    }

    auto column_start = build_grid_placement(values.grid_column_start(), *arena);
    auto column_end = build_grid_placement(values.grid_column_end(), *arena);
    auto row_start = build_grid_placement(values.grid_row_start(), *arena);
    auto row_end = build_grid_placement(values.grid_row_end(), *arena);

    auto* owner = arena.leak_ptr();
    return {
        .snapshot_owner = owner,
        .names = owner->names.data(),
        .name_count = owner->names.size(),
        .name_indices = owner->name_indices.data(),
        .name_index_count = owner->name_indices.size(),
        .entries = owner->entries.data(),
        .entry_count = owner->entries.size(),
        .template_columns = template_columns,
        .template_rows = template_rows,
        .auto_columns = auto_columns,
        .auto_rows = auto_rows,
        .areas = owner->areas.data(),
        .area_count = owner->areas.size(),
        .area_row_count = template_areas.row_count,
        .area_column_count = template_areas.column_count,
        .column_start = column_start,
        .column_end = column_end,
        .row_start = row_start,
        .row_end = row_end,
    };
}

static CSS::GridTrackSizeList build_used_grid_track_list(RustFFI::FfiUsedGridTrackList const& list)
{
    auto result = list.is_subgrid ? CSS::GridTrackSizeList::make_subgrid() : CSS::GridTrackSizeList::make_none();
    VERIFY(list.is_subgrid ? list.track_count == 0 : list.track_count + 1 == list.line_count);

    for (size_t line_index = 0; line_index < list.line_count; ++line_index) {
        CSS::GridLineNames line_names;
        auto const& line = list.lines[line_index];
        for (size_t name_index = 0; name_index < line.name_count; ++name_index)
            line_names.append(Utf16FlyString::from_raw(line.names[name_index]));
        if (list.is_subgrid || !line_names.is_empty())
            result.append(move(line_names));

        if (line_index < list.track_count) {
            auto size = CSSPixels::from_raw(list.track_sizes[line_index]);
            result.append(CSS::ExplicitGridTrack {
                CSS::GridSize { CSS::LengthStyleValue::create(CSS::Length::make_px(size)) },
            });
        }
    }
    return result;
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
            .contains_anchor_function = false,
        };
    }
    if (value.is_percentage()) {
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
            .contains_percentage = true,
            .contains_anchor_function = false,
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
            .contains_anchor_function = false,
        };
    case CSS::Size::Type::Percentage:
        return {
            .kind = to_underlying(RustFFI::FfiSizeKind::Percentage),
            .px = 0,
            .fraction = value.percentage().as_fraction(),
            .calc = nullptr,
            .contains_percentage = true,
            .contains_anchor_function = false,
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

static RustFFI::FfiAffineTransform to_ffi_affine_transform(Gfx::AffineTransform const& transform)
{
    return {
        .a = transform.a(),
        .b = transform.b(),
        .c = transform.c(),
        .d = transform.d(),
        .e = transform.e(),
        .f = transform.f(),
    };
}

static Gfx::AffineTransform from_ffi_affine_transform(RustFFI::FfiAffineTransform const& transform)
{
    return {
        transform.a,
        transform.b,
        transform.c,
        transform.d,
        transform.e,
        transform.f,
    };
}

static RustFFI::FfiSvgViewBox to_ffi_svg_view_box(SVG::ViewBox const& view_box)
{
    return {
        .min_x = view_box.min_x,
        .min_y = view_box.min_y,
        .width = view_box.width,
        .height = view_box.height,
    };
}

// https://svgwg.org/svg2-draft/struct.html#GroupsOverview
static bool is_svg_container_element(Node const& node)
{
    auto* dom_node = node.dom_node();
    if (!dom_node)
        return false;
    if (is<SVG::SVGAElement>(dom_node))
        return true;
    // FIXME: clipPath
    // FIXME: defs
    if (is<SVG::SVGGElement>(dom_node))
        return true;
    // FIXME: marker
    if (is<SVG::SVGMaskElement>(dom_node))
        return true;
    // FIXME: pattern
    if (is<SVG::SVGSVGElement>(dom_node))
        return true;
    if (is<SVG::SVGSwitchElement>(dom_node))
        return true;
    if (is<SVG::SVGSymbolElement>(dom_node))
        return true;
    // AD-HOC: Do we need `use` to be here?
    if (is<SVG::SVGUseElement>(dom_node))
        return true;
    return false;
}

static RustFFI::FfiSvgElementFacts build_svg_element_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_svg_facts_build();
    auto const* dom_node = node.dom_node();
    if (!dom_node)
        return {};

    Optional<SVG::ViewBox> active_view_box;
    if (auto const* svg_graphics_element = as_if<SVG::SVGGraphicsElement>(*dom_node))
        active_view_box = svg_graphics_element->active_view_box();
    else if (auto const* svg_fit_to_view_box = as_if<SVG::SVGFitToViewBox>(*dom_node))
        active_view_box = svg_fit_to_view_box->view_box();

    SVG::PreserveAspectRatio preserve_aspect_ratio {};
    if (auto const* fit_to_view_box = as_if<SVG::SVGFitToViewBox>(*dom_node))
        preserve_aspect_ratio = fit_to_view_box->preserve_aspect_ratio().value_or(SVG::PreserveAspectRatio {});
    else if (is<SVG::SVGMaskElement>(*dom_node) || is<SVG::SVGClipPathElement>(*dom_node))
        preserve_aspect_ratio = { SVG::PreserveAspectRatio::Align::None, {} };

    Gfx::AffineTransform element_transform;
    float visible_stroke_width = 0;
    if (auto const* graphics_element = as_if<SVG::SVGGraphicsElement>(*dom_node)) {
        element_transform = graphics_element->element_transform();
        visible_stroke_width = graphics_element->visible_stroke_width();
    }

    SVG::SVGUnits content_units {};
    SVG::SVGUnits pattern_units {};
    SVG::NumberPercentage pattern_width = SVG::NumberPercentage::create_number(0);
    SVG::NumberPercentage pattern_height = SVG::NumberPercentage::create_number(0);
    if (auto const* mask_box = as_if<SVGMaskBox>(node))
        content_units = mask_box->dom_node().mask_content_units();
    else if (auto const* clip_box = as_if<SVGClipBox>(node))
        content_units = clip_box->dom_node().clip_path_units();
    else if (auto const* pattern_box = as_if<SVGPatternBox>(node)) {
        content_units = pattern_box->dom_node().pattern_content_units();
        pattern_units = pattern_box->dom_node().pattern_units();
        pattern_width = pattern_box->dom_node().pattern_width();
        pattern_height = pattern_box->dom_node().pattern_height();
    }

    bool has_own_view_box = false;
    if (auto const* svg_element = as_if<SVG::SVGSVGElement>(*dom_node))
        has_own_view_box = svg_element->view_box().has_value();

    return {
        .is_document_element = node.document().document_element() == dom_node,
        .document_is_decoded_svg = node.document().is_decoded_svg(),
        .is_fit_to_view_box = is<SVG::SVGFitToViewBox>(*dom_node),
        .is_svg_svg_element = is<SVG::SVGSVGElement>(*dom_node),
        .is_container_element = is_svg_container_element(node),
        .is_graphics_box = is<SVGGraphicsBox>(node),
        .is_geometry_box = is<SVGGeometryBox>(node),
        .is_text_box = is<SVGTextBox>(node),
        .is_text_path_box = is<SVGTextPathBox>(node),
        .is_image_box = is<SVGImageBox>(node),
        .is_foreign_object_box = node.is_svg_foreign_object_box(),
        .is_mask_box = is<SVGMaskBox>(node),
        .is_clip_box = is<SVGClipBox>(node),
        .is_pattern_box = is<SVGPatternBox>(node),
        .has_active_view_box = active_view_box.has_value(),
        .active_view_box = active_view_box.has_value() ? to_ffi_svg_view_box(*active_view_box) : RustFFI::FfiSvgViewBox {},
        .has_own_view_box = has_own_view_box,
        .preserve_aspect_ratio_align = static_cast<u8>(to_underlying(preserve_aspect_ratio.align)),
        .preserve_aspect_ratio_meet_or_slice = static_cast<u8>(to_underlying(preserve_aspect_ratio.meet_or_slice)),
        .element_transform = to_ffi_affine_transform(element_transform),
        .visible_stroke_width = visible_stroke_width,
        .content_units = static_cast<u8>(to_underlying(content_units)),
        .pattern_units = static_cast<u8>(to_underlying(pattern_units)),
        .pattern_width = {
            .value = pattern_width.value(),
            .is_percentage = pattern_width.is_percentage(),
        },
        .pattern_height = {
            .value = pattern_height.value(),
            .is_percentage = pattern_height.is_percentage(),
        },
    };
}

static Utf16String rendered_svg_text_contents(SVG::SVGTextContentElement const& element)
{
    Utf16StringBuilder builder;
    element.for_each_in_subtree_of_type<DOM::Text>([&](auto const& text_node) {
        if (text_node.parent() && text_node.parent()->unsafe_layout_node()) {
            if (auto content = text_node.text_content(); content.has_value())
                builder.append(*content);
        }
        return TraversalDecision::Continue;
    });
    return builder.to_string().trim_ascii_whitespace();
}

static Gfx::Path compute_path_for_svg_text(SVGTextBox const& text_box, Gfx::FloatPoint current_text_position)
{
    auto& text_element = text_box.dom_node();
    // FIXME: Use per-code-point fonts.
    auto& font = text_box.first_available_font();
    auto text_contents = text_element.text_contents();
    auto text_width = font.width(text_contents);
    auto text_offset = current_text_position;

    switch (text_element.text_anchor().value_or(SVG::TextAnchor::Start)) {
    case SVG::TextAnchor::Start:
        break;
    case SVG::TextAnchor::Middle:
        text_offset.translate_by(-text_width / 2, 0);
        break;
    case SVG::TextAnchor::End:
        text_offset.translate_by(-text_width, 0);
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    auto baseline_metric = resolve_dominant_baseline_metric(text_box.computed_values());
    text_offset.translate_by(0, dominant_baseline_offset(baseline_metric, font.pixel_metrics()));

    Gfx::Path path;
    path.move_to(text_offset);
    path.text(text_contents, font);
    return path;
}

static Gfx::Path compute_path_for_svg_text_path(SVGTextPathBox const& text_path_box, CSSPixelSize viewport_size)
{
    auto& text_path_element = static_cast<SVG::SVGTextPathElement const&>(text_path_box.dom_node());
    auto path_or_shape = text_path_element.path_or_shape();
    if (!path_or_shape)
        return {};

    // FIXME: Use per-code-point fonts.
    auto& font = text_path_box.first_available_font();
    auto text_contents = rendered_svg_text_contents(text_path_element);

    auto shape_path = const_cast<SVG::SVGGeometryElement&>(*path_or_shape).get_path(viewport_size);
    auto start_offset = text_path_element.start_offset_for_path_length(shape_path.length());

    // FIXME: Take writing mode and text direction into account.
    auto total_advance = font.width(text_contents);
    switch (text_path_element.text_anchor().value_or(SVG::TextAnchor::Start)) {
    case SVG::TextAnchor::Start:
        break;
    case SVG::TextAnchor::Middle:
        start_offset -= total_advance / 2;
        break;
    case SVG::TextAnchor::End:
        start_offset -= total_advance;
        break;
    default:
        VERIFY_NOT_REACHED();
    }

    return shape_path.place_text_along(text_contents, font, start_offset);
}

static RustFFI::FfiSvgPathResult compute_svg_path(NodeWithStyle const& node, RustFFI::FfiSvgPathRequest const& request)
{
    auto const& graphics_box = as<SVGGraphicsBox>(node);
    CSSPixelSize viewport_size {
        CSSPixels::from_raw(request.viewport_width),
        CSSPixels::from_raw(request.viewport_height),
    };
    Gfx::FloatPoint text_position {
        request.current_text_position.x,
        request.current_text_position.y,
    };

    Gfx::Path path;
    if (auto const* geometry_box = as_if<SVGGeometryBox>(graphics_box)) {
        path = const_cast<SVGGeometryBox&>(*geometry_box).dom_node().get_path(viewport_size);
    } else if (auto const* text_box = as_if<SVGTextBox>(graphics_box)) {
        auto text_positioning = text_box->dom_node().text_positioning();
        text_positioning.apply_to_text_position(viewport_size, text_position, 0u);
        path = compute_path_for_svg_text(*text_box, text_position);
    } else if (auto const* text_path_box = as_if<SVGTextPathBox>(graphics_box)) {
        path = compute_path_for_svg_text_path(*text_path_box, viewport_size);
    }

    auto bounding_box = path.bounding_box();
    auto* path_handle = new Gfx::Path(move(path));
    ++s_outstanding_svg_path_handles;
    RustFFI::rust_layout_ffi_note_svg_path_retain();
    return {
        .path_handle = path_handle,
        .bounding_box = {
            .x = bounding_box.x(),
            .y = bounding_box.y(),
            .width = bounding_box.width(),
            .height = bounding_box.height(),
        },
        .text_position_for_children = {
            .x = text_position.x(),
            .y = text_position.y(),
        },
    };
}

static void release_svg_path_handle(void const* handle)
{
    VERIFY(handle);
    VERIFY(s_outstanding_svg_path_handles.load() > 0);
    --s_outstanding_svg_path_handles;
    RustFFI::rust_layout_ffi_note_svg_path_release();
    delete static_cast<Gfx::Path const*>(handle);
}

// https://developer.mozilla.org/en-US/docs/Web/Guide/CSS/Block_formatting_context
static bool creates_block_formatting_context(Box const& box)
{
    // NOTE: Replaced elements never create a BFC.
    if (box.is_replaced_box())
        return false;

    // AD-HOC: We create a BFC for SVG foreignObject.
    if (box.is_svg_foreign_object_box())
        return true;

    // display: table
    if (box.display().is_table_inside())
        return false;

    // display: flex
    if (box.display().is_flex_inside())
        return false;

    // display: grid
    if (box.display().is_grid_inside())
        return false;

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

RustFFI::FfiLayoutBoxFacts build_layout_box_facts(NodeWithStyle const& node)
{
    RustFFI::rust_layout_ffi_note_box_facts_build();
    auto const* box = as_if<Box>(node);
    auto natural_size = box ? box->natural_size() : CSS::SizeWithAspectRatio {};
    auto auto_content_size = box ? box->auto_content_box_size() : CSS::SizeWithAspectRatio {};
    auto preferred_aspect_ratio = box ? box->preferred_aspect_ratio() : Optional<CSSPixelFraction> {};
    auto display = node.display();
    auto const* dom_node = node.dom_node();
    auto const* fieldset_box = as_if<FieldSetBox>(node);
    auto const* list_item_box = as_if<ListItemBox>(node);
    auto const* marker = as_if<ListItemMarkerBox>(node);
    bool has_css_marker_content = false;
    if (list_item_box && dom_node) {
        if (auto const* element = as_if<DOM::Element>(dom_node)) {
            if (auto const computed_values = element->computed_values(CSS::PseudoElement::Marker))
                has_css_marker_content = computed_values->content().has_value();
        }
    }
    CSSPixels marker_content_inline_size = 0;
    CSSPixels marker_content_block_size = 0;
    CSSPixels marker_distance = 0;
    if (marker) {
        auto marker_text = marker->text();
        if (auto const* list_style_image = marker->list_style_image()) {
            marker_content_inline_size = list_style_image->natural_width(marker->document()).value_or(0);
            marker_content_block_size = list_style_image->natural_height(marker->document()).value_or(0);
        } else {
            auto marker_size = marker->relative_size();
            marker_content_block_size = marker_size;
            auto const& marker_font = marker->first_available_font();
            if (marker_text.has_value()) {
                // FIXME: Use per-code-point fonts to measure text.
                marker_content_inline_size = CSSPixels::nearest_value_for(marker_font.width(marker_text.value()));
            } else {
                marker_content_inline_size = marker_size;
            }
        }
        if (!marker_text.has_value())
            marker_distance = CSSPixels::nearest_value_for(.5f * marker->first_available_font().pixel_size());
    }
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
        .is_text_node = node.is_text_node(),
        .is_break_node = node.is_break_node(),
        .is_box = node.is_box(),
        .is_block_container = node.is_block_container(),
        .is_replaced_box = node.is_replaced_box(),
        .is_replaced_box_with_children = node.is_replaced_box_with_children(),
        .is_floating = node.is_floating(),
        .is_absolutely_positioned = node.is_absolutely_positioned(),
        .is_inline = node.is_inline(),
        .is_inline_block = node.is_inline_block(),
        .is_atomic_inline = node.is_atomic_inline(),
        .is_inline_node = is<InlineNode>(node),
        .has_box_model_metrics = is<NodeWithStyleAndBoxModelMetrics>(node),
        .is_fragmented_inline = node.is_fragmented_inline(),
        .is_inline_flow_interrupting_block = is<NodeWithStyleAndBoxModelMetrics>(node) && static_cast<NodeWithStyleAndBoxModelMetrics const&>(node).is_inline_flow_interrupting_block(),
        .is_list_item_marker_box = node.is_list_item_marker_box(),
        .is_list_item_box = node.is_list_item_box(),
        .is_svg_mask_box = node.is_svg_mask_box(),
        .is_svg_clip_box = node.is_svg_clip_box(),
        .is_replaced_element = node.is_replaced_element(),
        .display_before_box_type_transformation_is_block_outside = node.display_before_box_type_transformation().is_block_outside(),
        .inline_axis_is_reverse = node.computed_values().inline_axis_is_reverse(),
        .has_dom_node = node.dom_node() != nullptr,
        .children_are_inline = node.children_are_inline(),
        .is_anonymous = node.is_anonymous(),
        .can_have_children = node.can_have_children(),
        .has_replaced_element_table_display_adjustment = node.has_replaced_element_table_display_adjustment(),
        .creates_block_formatting_context = box && creates_block_formatting_context(*box),
        .is_flex_item = node.is_flex_item(),
        .is_grid_item = node.is_grid_item(),
        .is_editing_host = dom_node && dom_node->is_element() && static_cast<DOM::Element const&>(*dom_node).is_editing_host(),
        .uses_button_layout = dom_node && is<HTML::HTMLElement>(*dom_node) && static_cast<HTML::HTMLElement const&>(*dom_node).uses_button_layout(),
        .vertical_align_applies = box && box->vertical_align_applies(),
        .is_html_input_element = dom_node && is<HTML::HTMLInputElement>(*dom_node),
        .may_reuse_precreated_used_values = false,
        .is_fieldset_box = fieldset_box != nullptr,
        .rendered_legend = fieldset_box && fieldset_box->rendered_legend() ? const_cast<LegendBox*>(fieldset_box->rendered_legend().ptr()) : nullptr,
        .list_item_marker = list_item_box && list_item_box->marker() ? const_cast<ListItemMarkerBox*>(list_item_box->marker()) : nullptr,
        .has_css_marker_content = has_css_marker_content,
        .marker_content_inline_size = marker_content_inline_size.raw_value(),
        .marker_content_block_size = marker_content_block_size.raw_value(),
        .marker_distance = marker_distance.raw_value(),
        .marker_list_style_position = static_cast<u8>(marker ? to_underlying(marker->list_style_position()) : 0),
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
        .has_layout_index = true,
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

Optional<RustFFI::FfiFormattingContextType> formatting_context_type_created_by_box(Box const& box)
{
    RustFFI::FfiLayoutBoxFacts facts {};
    facts.is_block_container = box.is_block_container();
    facts.is_replaced_box = box.is_replaced_box();
    facts.is_replaced_box_with_children = box.is_replaced_box_with_children();
    facts.children_are_inline = box.children_are_inline();
    facts.can_have_children = box.can_have_children();
    facts.has_replaced_element_table_display_adjustment = box.has_replaced_element_table_display_adjustment();
    facts.creates_block_formatting_context = creates_block_formatting_context(box);
    facts.display = encode_display(box.display());
    facts.is_svg_svg_box = box.is_svg_svg_box();

    auto type = RustFFI::rust_layout_formatting_context_type_for_box(facts);
    if (type == NumericLimits<u8>::max())
        return {};
    return static_cast<RustFFI::FfiFormattingContextType>(type);
}

StringView formatting_context_type_name(RustFFI::FfiFormattingContextType type)
{
    switch (type) {
    case RustFFI::FfiFormattingContextType::Block:
        return "BFC"sv;
    case RustFFI::FfiFormattingContextType::Inline:
        return "IFC"sv;
    case RustFFI::FfiFormattingContextType::Flex:
        return "FFC"sv;
    case RustFFI::FfiFormattingContextType::Grid:
        return "GFC"sv;
    case RustFFI::FfiFormattingContextType::Table:
        return "TFC"sv;
    case RustFFI::FfiFormattingContextType::Svg:
        return "SVG"sv;
    case RustFFI::FfiFormattingContextType::ReplacedWithChildren:
        return "Replaced, with children"sv;
    case RustFFI::FfiFormattingContextType::AbsposReplay:
        return "Abspos replay"sv;
    case RustFFI::FfiFormattingContextType::InternalReplaced:
        return "Replaced"sv;
    case RustFFI::FfiFormattingContextType::InternalDummy:
        return "Dummy"sv;
    }
    VERIFY_NOT_REACHED();
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

LayoutRustBridge::LayoutRustBridge(LayoutMode layout_mode)
    : m_layout_mode(layout_mode)
{
}

LayoutRustBridge::~LayoutRustBridge() = default;

void LayoutRustBridge::run_root_layout(Box& viewport, CSSPixels viewport_inline_size, CSSPixels viewport_block_size, u32 node_count, bool should_collect_devtools_layout_data)
{
    VERIFY(m_layout_mode == LayoutMode::Normal);
    VERIFY(!m_commit_root);
    m_commit_root = &viewport;
    ScopeGuard clear_commit_root = [&] {
        m_commit_root = nullptr;
    };

    viewport.document().invalidate_stacking_context_tree();
    auto callbacks = formatting_context_callbacks();
    auto sink = commit_sink();
    RustFFI::rust_layout_run_root_layout(
        &viewport,
        viewport_inline_size.raw_value(),
        viewport_block_size.raw_value(),
        node_count,
        should_collect_devtools_layout_data,
        &callbacks,
        &sink);
    VERIFY(!m_line_commit_context);
}

void LayoutRustBridge::compute_subtree_layout(Box& root, Painting::Paintable& paintable_to_replace)
{
    VERIFY(m_layout_mode == LayoutMode::Normal);
    VERIFY(!m_commit_root);
    m_commit_root = &root;
    ScopeGuard clear_commit_root = [&] {
        m_commit_root = nullptr;
    };

    root.document().invalidate_stacking_context_tree();
    auto callbacks = formatting_context_callbacks();
    auto sink = commit_sink();
    RustFFI::rust_layout_compute_subtree_layout(
        &root,
        &root.root(),
        &paintable_to_replace,
        &callbacks,
        &sink);
    VERIFY(!m_line_commit_context);
}

void LayoutRustBridge::replay_saved_abspos_layout(Box& box, Painting::Paintable& paintable_to_replace)
{
    VERIFY(m_layout_mode == LayoutMode::Normal);
    VERIFY(!m_commit_root);
    m_commit_root = &box;
    ScopeGuard clear_commit_root = [&] {
        m_commit_root = nullptr;
    };

    box.document().invalidate_stacking_context_tree();
    auto callbacks = formatting_context_callbacks();
    auto sink = commit_sink();
    RustFFI::rust_layout_replay_saved_abspos_layout(&box, &paintable_to_replace, &callbacks, &sink);
    VERIFY(!m_line_commit_context);
}

struct LayoutRustBridge::LineCommitContext {
    explicit LineCommitContext(Painting::PaintableWithLines& paintable)
        : paintable(paintable)
    {
    }

    Painting::PaintableWithLines& paintable;
    Vector<Painting::LineRecord> lines;
    Vector<Painting::InlineBoxPiece> pieces;
};

struct InlineAncestorChainRelativeOffset {
    CSSPixelPoint offset;
    bool found_fragmented_inline_node { false };
};

static CSS::BorderData from_ffi_border_data(RustFFI::FfiBorderData const&);
static AbsposLayoutInputs from_ffi_abspos_layout_inputs(RustFFI::FfiAbsposLayoutInputs const&);

// Accumulates relative position insets from a chain of inline-flow ancestors, starting at first_ancestor
// and walking up until stop_at or the first ancestor that is not inline-flow.
static InlineAncestorChainRelativeOffset accumulated_relative_insets_from_inline_ancestor_chain(Node const* first_ancestor, Node const* stop_at)
{
    InlineAncestorChainRelativeOffset result;
    for (auto const* ancestor = first_ancestor; ancestor && ancestor != stop_at; ancestor = ancestor->parent()) {
        if (!is<Layout::NodeWithStyleAndBoxModelMetrics>(*ancestor))
            break;
        auto const& ancestor_with_style = static_cast<Layout::NodeWithStyleAndBoxModelMetrics const&>(*ancestor);
        if (!ancestor_with_style.display().is_inline_outside() || !ancestor_with_style.display().is_flow_inside())
            break;
        result.found_fragmented_inline_node |= ancestor->is_fragmented_inline();
        if (ancestor_with_style.computed_values().position() == CSS::Positioning::Relative) {
            VERIFY(ancestor->paintable());
            auto const& ancestor_paintable_box = *ancestor->paintable();
            auto const& inset = ancestor_paintable_box.box_model().inset;
            result.offset.translate_by(inset.left, inset.top);
        }
    }
    return result;
}

static Painting::Paintable::BorderDataWithElementKind from_ffi_border_data_with_element_kind(RustFFI::FfiBorderDataWithElementKind const& border)
{
    return {
        .border_data = from_ffi_border_data(border.border_data),
        .element_kind = static_cast<Painting::Paintable::ConflictingElementKind>(border.element_kind),
    };
}

RustFFI::FfiCommitSink LayoutRustBridge::commit_sink()
{
    return {
        .context = this,
        .begin_commit = [](void* context, void* root_pointer, void* paintable_to_replace_pointer) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& root = *static_cast<Box*>(root_pointer);
            VERIFY(!bridge.m_replaced_paintable);
            VERIFY(!bridge.m_commit_parent_paintable);
            VERIFY(!bridge.m_commit_insert_before_paintable);

            if (paintable_to_replace_pointer) {
                bridge.m_replaced_paintable = *static_cast<Painting::Paintable*>(paintable_to_replace_pointer);
            } else if (!root.is_viewport()) {
                bridge.m_replaced_paintable = root.paintable();
            }

            if (bridge.m_replaced_paintable) {
                // Keep the old subtree alive while its replacement is spliced
                // into the exact same paint-order position.
                bridge.m_commit_parent_paintable = bridge.m_replaced_paintable->parent();
                bridge.m_commit_insert_before_paintable = bridge.m_replaced_paintable->next_sibling();
                if (bridge.m_commit_parent_paintable)
                    bridge.m_commit_parent_paintable->remove_child(*bridge.m_replaced_paintable);
            }

            return RustFFI::FfiCommitPosition {
                .parent_paintable = bridge.m_commit_parent_paintable.ptr(),
                .insert_before_paintable = bridge.m_commit_insert_before_paintable.ptr(),
            };
        },
        .finish_commit = [](void* context) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            bridge.m_commit_insert_before_paintable = nullptr;
            bridge.m_commit_parent_paintable = nullptr;
            bridge.m_replaced_paintable = nullptr;
        },
        .prepare_node = [](void*, void* node_pointer, bool has_used_values, bool has_abspos_layout_inputs, RustFFI::FfiAbsposLayoutInputs abspos_layout_inputs) -> void* {
            auto& node = *static_cast<Node*>(node_pointer);

            // Clearing on absence keeps saved inputs from a previous pass from surviving a pass
            // that no longer laid the box out as absolutely positioned.
            if (has_used_values) {
                if (auto* layout_box = as_if<Box>(node)) {
                    if (has_abspos_layout_inputs)
                        layout_box->set_saved_abspos_layout_inputs(from_ffi_abspos_layout_inputs(abspos_layout_inputs));
                    else
                        layout_box->clear_saved_abspos_layout_inputs();
                }
            }

            RefPtr<Painting::Paintable> paintable;
            if (has_used_values || (node.is_fragmented_inline() && node.dom_node())) {
                // Inline boxes that never went through inline layout (so they have no used values) still
                // need a paintable so DOM geometry queries have something to answer from.
                paintable = node.paintable();
                if (paintable)
                    paintable->reset_for_relayout();
                else
                    paintable = node.create_paintable();
                node.set_paintable(paintable);
            } else if (node.paintable_ptr()) {
                // A paintable surviving from a previous layout on a node this pass did not lay out is
                // stale; drop it so the layout tree only points into the paint tree built by this commit.
                node.clear_paintable();
            }
            return paintable.ptr();
        },
        .set_box_metrics = [](void*, void* paintable_pointer, RustFFI::FfiCommittedBoxMetrics metrics) {
            auto& paintable = *static_cast<Painting::Paintable*>(paintable_pointer);
            auto& box_model = paintable.box_model();
            box_model.inset = {
                CSSPixels::from_raw(metrics.inset_top),
                CSSPixels::from_raw(metrics.inset_right),
                CSSPixels::from_raw(metrics.inset_bottom),
                CSSPixels::from_raw(metrics.inset_left),
            };
            box_model.padding = {
                CSSPixels::from_raw(metrics.padding_top),
                CSSPixels::from_raw(metrics.padding_right),
                CSSPixels::from_raw(metrics.padding_bottom),
                CSSPixels::from_raw(metrics.padding_left),
            };
            box_model.border = {
                CSSPixels::from_raw(metrics.border_top),
                CSSPixels::from_raw(metrics.border_right),
                CSSPixels::from_raw(metrics.border_bottom),
                CSSPixels::from_raw(metrics.border_left),
            };
            box_model.margin = {
                CSSPixels::from_raw(metrics.margin_top),
                CSSPixels::from_raw(metrics.margin_right),
                CSSPixels::from_raw(metrics.margin_bottom),
                CSSPixels::from_raw(metrics.margin_left),
            };
            paintable.set_content_size(
                CSSPixels::from_raw(metrics.content_inline_size),
                CSSPixels::from_raw(metrics.content_block_size));
        },
        .set_override_borders = [](void*, void* paintable_pointer, RustFFI::FfiBordersData borders) {
            static_cast<Painting::Paintable*>(paintable_pointer)->set_override_borders_data({
                .top = from_ffi_border_data_with_element_kind(borders.top),
                .right = from_ffi_border_data_with_element_kind(borders.right),
                .bottom = from_ffi_border_data_with_element_kind(borders.bottom),
                .left = from_ffi_border_data_with_element_kind(borders.left),
            });
        },
        .set_table_cell_coordinates = [](void*, void* paintable_pointer, RustFFI::FfiTableCellCoordinates coordinates) {
            static_cast<Painting::Paintable*>(paintable_pointer)->set_table_cell_coordinates({
                .row_index = coordinates.row_index,
                .column_index = coordinates.column_index,
                .row_span = coordinates.row_span,
                .column_span = coordinates.column_span,
            });
        },
        .begin_line_data = [](void* context, void* paintable_pointer) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            VERIFY(!bridge.m_line_commit_context);
            auto* paintable = as_if<Painting::PaintableWithLines>(*static_cast<Painting::Paintable*>(paintable_pointer));
            if (!paintable)
                return false;
            bridge.m_line_commit_context = make<LineCommitContext>(*paintable);
            return true;
        },
        .begin_line = [](void* context, RustFFI::FfiLineRecord record) {
            auto& line_context = *static_cast<LayoutRustBridge*>(context)->m_line_commit_context;
            line_context.lines.append({
                .rect = {
                    CSSPixels::from_raw(record.rect.x),
                    CSSPixels::from_raw(record.rect.y),
                    CSSPixels::from_raw(record.rect.width),
                    CSSPixels::from_raw(record.rect.height),
                },
                .fragment_count = record.committed_fragment_count,
            });
        },
        .emit_fragment = [](void* context, RustFFI::FfiCommittedFragment fragment) {
            auto& line_context = *static_cast<LayoutRustBridge*>(context)->m_line_commit_context;
            VERIFY(fragment.layout_node);
            RefPtr<Gfx::GlyphRun> glyph_run;
            if (fragment.has_glyph_run) {
                VERIFY(fragment.glyph_font);
                VERIFY(fragment.glyphs || fragment.glyph_count == 0);
                Vector<Gfx::DrawGlyph> glyphs;
                glyphs.ensure_capacity(fragment.glyph_count);
                auto const* draw_glyphs = reinterpret_cast<Gfx::DrawGlyph const*>(fragment.glyphs);
                for (size_t index = 0; index < fragment.glyph_count; ++index)
                    glyphs.unchecked_append(draw_glyphs[index]);
                glyph_run = adopt_ref(*new Gfx::GlyphRun(
                    move(glyphs),
                    *static_cast<Gfx::Font const*>(fragment.glyph_font),
                    static_cast<Gfx::GlyphRun::TextType>(fragment.glyph_text_type),
                    fragment.glyph_run_width));
            }
            line_context.paintable.add_fragment({
                .layout_node = *static_cast<Node const*>(fragment.layout_node),
                .offset = {
                    CSSPixels::from_raw(fragment.offset.x),
                    CSSPixels::from_raw(fragment.offset.y),
                },
                .size = {
                    CSSPixels::from_raw(fragment.size.x),
                    CSSPixels::from_raw(fragment.size.y),
                },
                .line_index = static_cast<u32>(line_context.lines.size() - 1),
                .start_offset = fragment.start,
                .length_in_code_units = fragment.length_in_code_units,
                .glyph_run = move(glyph_run),
                .baseline = CSSPixels::from_raw(fragment.baseline),
                .writing_mode = static_cast<CSS::WritingMode>(fragment.writing_mode),
                .has_trailing_whitespace = fragment.has_trailing_whitespace,
            });
        },
        .emit_inline_box_piece = [](void* context, RustFFI::FfiInlineBoxPiece piece) {
            auto& line_context = *static_cast<LayoutRustBridge*>(context)->m_line_commit_context;
            VERIFY(piece.node);
            line_context.pieces.append({
                .node = *static_cast<Node const*>(piece.node),
                .first_fragment_index = piece.first_fragment_index,
                .fragment_count = piece.fragment_count,
                .border_box_rect = {
                    CSSPixels::from_raw(piece.border_box_rect.x),
                    CSSPixels::from_raw(piece.border_box_rect.y),
                    CSSPixels::from_raw(piece.border_box_rect.width),
                    CSSPixels::from_raw(piece.border_box_rect.height),
                },
                .present_edges = piece.present_edges,
                .is_geometry_only_placeholder = piece.is_geometry_only_placeholder,
            });
        },
        .finish_line_data = [](void* context) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto line_context = move(bridge.m_line_commit_context);
            VERIFY(line_context);
            line_context->paintable.set_lines(move(line_context->lines));
            line_context->paintable.set_inline_box_pieces(move(line_context->pieces));

            // Piece fragment ranges were counted against the same skip-fully-truncated
            // fragment stream during inline layout; a divergence would let piece
            // consumers read out of bounds.
            for (auto const& piece : line_context->paintable.inline_box_pieces())
                VERIFY(piece.first_fragment_index + piece.fragment_count <= line_context->paintable.fragments().size());
        },
        .set_computed_svg_transforms = [](void*, void* paintable_pointer, RustFFI::FfiSvgComputedTransforms transforms) {
            Painting::SVGGraphicsPaintable::ComputedTransforms converted {
                from_ffi_affine_transform(transforms.viewbox_transform),
                from_ffi_affine_transform(transforms.svg_transform),
            };
            auto& paintable = *static_cast<Painting::Paintable*>(paintable_pointer);
            if (auto* svg_graphics_paintable = as_if<Painting::SVGGraphicsPaintable>(paintable))
                svg_graphics_paintable->set_computed_transforms(converted);
            if (auto* svg_foreign_object_paintable = as_if<Painting::SVGForeignObjectPaintable>(paintable))
                svg_foreign_object_paintable->set_computed_transforms(converted);
            if (auto* svg_svg_paintable = as_if<Painting::SVGSVGPaintable>(paintable))
                svg_svg_paintable->set_computed_transforms(converted);
        },
        .set_computed_svg_path = [](void*, void* paintable_pointer, void* path_pointer) {
            VERIFY(path_pointer);
            auto& paintable = *static_cast<Painting::Paintable*>(paintable_pointer);
            auto* path = static_cast<Gfx::Path*>(path_pointer);
            if (auto* svg_path_paintable = as_if<Painting::SVGPathPaintable>(paintable))
                svg_path_paintable->set_computed_path(move(*path));
            release_svg_path_handle(path);
        },
        .set_grid_layout_data = [](void*, void* paintable_pointer, void* data) {
            VERIFY(data);
            static_cast<Painting::Paintable*>(paintable_pointer)->set_grid_layout_data(adopt_own(*static_cast<GridLayoutData*>(data)));
        },
        .set_flex_layout_data = [](void*, void* paintable_pointer, void* data) {
            VERIFY(data);
            static_cast<Painting::Paintable*>(paintable_pointer)->set_flex_layout_data(adopt_own(*static_cast<FlexLayoutData*>(data)));
        },
        .set_used_grid_tracks = [](void*, void* paintable_pointer, void* data) {
            VERIFY(data);
            auto tracks = adopt_own(*static_cast<UsedGridTracksCommitData*>(data));
            auto& paintable = *static_cast<Painting::Paintable*>(paintable_pointer);
            paintable.set_used_values_for_grid_template_columns(move(tracks->columns));
            paintable.set_used_values_for_grid_template_rows(move(tracks->rows));
        },
        .set_offset = [](void*, void* node_pointer, void* paintable_pointer, RustFFI::FfiCommittedOffset committed) {
            auto& node = *static_cast<Node*>(node_pointer);
            auto& paintable = *static_cast<Painting::Paintable*>(paintable_pointer);
            CSSPixelPoint offset {
                CSSPixels::from_raw(committed.offset.x),
                CSSPixels::from_raw(committed.offset.y),
            };
            // Used values materialized from a previous layout's paintable keep that paintable's
            // offset, which already includes any relative position inset. Offsets of fragmented
            // inlines are not meaningful; their geometry is assigned from pieces once relative
            // positions are resolved.
            if (node.is_box() && !node.is_fragmented_inline() && !committed.materialized_from_paintable) {
                if (committed.has_containing_line_box_fragment) {
                    // We know that `node` is an atomic inline because `containing_line_box_fragment` refers to the
                    // line box fragment in the parent block container that contains it. The fragment has the final
                    // offset for the atomic inline, but line box post-processing may remove fragments after the
                    // coordinate was recorded, in which case the content offset stands.
                    if (committed.line_fragment_lookup >= 1) {
                        paintable.set_containing_line_box_index(committed.containing_line_box_index);
                        if (committed.line_fragment_lookup == 2) {
                            offset = {
                                CSSPixels::from_raw(committed.line_fragment_offset.x),
                                CSSPixels::from_raw(committed.line_fragment_offset.y),
                            };
                        }
                    }
                }

                if (is<NodeWithStyleAndBoxModelMetrics>(node)
                    && static_cast<NodeWithStyleAndBoxModelMetrics const&>(node).computed_values().position() == CSS::Positioning::Relative)
                    offset.translate_by(CSSPixels::from_raw(committed.inset_left), CSSPixels::from_raw(committed.inset_top));
            }
            paintable.set_offset(offset);
        },
        .finish_node = [](void*, void* node_pointer, void* paintable_pointer, void* parent_paintable_pointer, void* insert_before_paintable_pointer) {
            auto& node = *static_cast<Node*>(node_pointer);
            auto* paintable = static_cast<Painting::Paintable*>(paintable_pointer);
            auto* parent_paintable = static_cast<Painting::Paintable*>(parent_paintable_pointer);
            auto* insert_before_paintable = static_cast<Painting::Paintable*>(insert_before_paintable_pointer);
            auto* dom_node = node.dom_node();
            Painting::Paintable* paintable_for_children = nullptr;
            if (paintable) {
                if (parent_paintable && !paintable->forms_unconnected_subtree()) {
                    VERIFY(!paintable->parent());
                    parent_paintable->insert_before(*paintable, insert_before_paintable);
                }
                paintable->set_dom_node(dom_node);
                if (dom_node)
                    dom_node->set_paintable(paintable);
                auto* containing_block = node.containing_block();
                paintable->set_containing_block(containing_block ? containing_block->paintable_ptr() : nullptr);
                paintable_for_children = paintable;
            } else {
                if (dom_node)
                    dom_node->clear_paintable();
                // An inline box without a paintable must not orphan its descendants' paintables; pass the
                // nearest ancestor paintable through. Other paintable-less nodes (e.g. non-rendered SVG
                // subtrees) keep their descendants disconnected on purpose.
                if (node.is_fragmented_inline())
                    paintable_for_children = parent_paintable;
            }
            return RustFFI::FfiCommitNodeResult {
                .paintable = paintable,
                .paintable_for_children = paintable_for_children,
            };
        },
        .resolve_relative_positions = [](void* context, void* node_pointer) {
            auto& bridge = *static_cast<LayoutRustBridge*>(context);
            auto& node = *static_cast<NodeWithStyle*>(node_pointer);

            // Nodes outside the committed subtree (materialized containing blocks) keep their
            // already-resolved offsets.
            VERIFY(bridge.m_commit_root);
            if (!bridge.m_commit_root->is_inclusive_ancestor_of(node))
                return;

            if (auto const* box = as_if<Box>(node); box && box->is_in_flow() && box->display().is_block_outside()) {
                auto accumulated = accumulated_relative_insets_from_inline_ancestor_chain(box->parent(), box->containing_block());
                if (accumulated.found_fragmented_inline_node) {
                    if (auto paintable = node.paintable())
                        paintable->set_offset(paintable->offset().translated(accumulated.offset));
                }
            }

            auto* paintable_with_lines = as_if<Painting::PaintableWithLines>(node.paintable().ptr());
            if (!paintable_with_lines)
                return;

            for (auto& fragment : paintable_with_lines->fragments()) {
                auto accumulated = accumulated_relative_insets_from_inline_ancestor_chain(fragment.layout_node().parent(), nullptr);
                if (!accumulated.offset.is_zero())
                    fragment.set_offset(fragment.offset().translated(accumulated.offset));
            }

            HashMap<Layout::Node const*, CSSPixelPoint> accumulated_offset_per_node;
            for (auto& piece : paintable_with_lines->inline_box_pieces()) {
                auto const* piece_node = piece.node.ptr();
                if (!piece_node)
                    continue;
                // The chain starts at the piece's own node: a relative inline box shifts its own pieces.
                auto offset = accumulated_offset_per_node.ensure(piece_node, [&] {
                    return accumulated_relative_insets_from_inline_ancestor_chain(piece_node, nullptr).offset;
                });
                if (!offset.is_zero())
                    piece.border_box_rect.translate_by(offset);
            }

            // Piece rects are final only now that this block's relative positions are resolved;
            // cross-block writes in this pass only touch box offsets, which piece geometry
            // assignment never reads.
            if (!paintable_with_lines->inline_box_pieces().is_empty())
                paintable_with_lines->assign_inline_box_geometry();
        },
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

static RustFFI::FfiStaticPositionRect to_ffi_static_position_rect(StaticPositionRect const& rect)
{
    return {
        .rect = {
            .offset = {
                .inline_offset = rect.rect.offset.inline_offset.raw_value(),
                .block_offset = rect.rect.offset.block_offset.raw_value(),
            },
            .size = {
                .inline_size = rect.rect.size.inline_size.raw_value(),
                .block_size = rect.rect.size.block_size.raw_value(),
            },
        },
        .inline_alignment = static_cast<RustFFI::FfiStaticPositionAlignment>(rect.inline_alignment),
        .block_alignment = static_cast<RustFFI::FfiStaticPositionAlignment>(rect.block_alignment),
        .alignment_derives_from_own_computed_values = rect.alignment_derives_from_own_computed_values,
    };
}

static RustFFI::FfiAbsposContainingBlockInfo to_ffi_abspos_containing_block_info(AbsposContainingBlockInfo const& info)
{
    return {
        .rect = {
            .offset = {
                .inline_offset = info.rect.offset.inline_offset.raw_value(),
                .block_offset = info.rect.offset.block_offset.raw_value(),
            },
            .size = {
                .inline_size = info.rect.size.inline_size.raw_value(),
                .block_size = info.rect.size.block_size.raw_value(),
            },
        },
        .inline_axis_mode = static_cast<RustFFI::FfiAbsposAxisMode>(info.inline_axis_mode),
        .block_axis_mode = static_cast<RustFFI::FfiAbsposAxisMode>(info.block_axis_mode),
        .has_inline_alignment = info.inline_alignment.has_value(),
        .inline_alignment = static_cast<RustFFI::FfiAbsposAlignment>(info.inline_alignment.value_or(Alignment::Normal)),
        .has_block_alignment = info.block_alignment.has_value(),
        .block_alignment = static_cast<RustFFI::FfiAbsposAlignment>(info.block_alignment.value_or(Alignment::Normal)),
        .derives_from_own_computed_values = info.derives_from_own_computed_values,
    };
}

static AbsposContainingBlockInfo from_ffi_abspos_containing_block_info(RustFFI::FfiAbsposContainingBlockInfo const& info)
{
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
        .inline_axis_mode = static_cast<AbsposAxisMode>(info.inline_axis_mode),
        .block_axis_mode = static_cast<AbsposAxisMode>(info.block_axis_mode),
        .inline_alignment = info.has_inline_alignment ? Optional<Alignment> { static_cast<Alignment>(info.inline_alignment) } : Optional<Alignment> {},
        .block_alignment = info.has_block_alignment ? Optional<Alignment> { static_cast<Alignment>(info.block_alignment) } : Optional<Alignment> {},
        .derives_from_own_computed_values = info.derives_from_own_computed_values,
    };
}

static RustFFI::FfiAbsposLayoutInputs to_ffi_abspos_layout_inputs(AbsposLayoutInputs const& inputs)
{
    return {
        .static_position_rect = to_ffi_static_position_rect(inputs.static_position_rect),
        .containing_block_info = to_ffi_abspos_containing_block_info(inputs.containing_block_info),
    };
}

static AbsposLayoutInputs from_ffi_abspos_layout_inputs(RustFFI::FfiAbsposLayoutInputs const& inputs)
{
    return {
        .static_position_rect = from_ffi_static_position_rect(inputs.static_position_rect),
        .containing_block_info = from_ffi_abspos_containing_block_info(inputs.containing_block_info),
    };
}

static Optional<DOM::AbstractElement> abstract_element_for_abspos_box(Box const& box)
{
    if (box.is_generated_for_pseudo_element())
        return DOM::AbstractElement { *box.pseudo_element_generator(), box.generated_for_pseudo_element() };
    if (auto const* element = as_if<DOM::Element>(box.dom_node()))
        return DOM::AbstractElement { *element };
    return {};
}

static bool style_value_contains_anchor(CSS::StyleValue const& value)
{
    if (value.is_anchor())
        return true;
    if (value.is_calculated())
        return value.as_calculated().contains_anchor_function();
    return false;
}

bool box_inset_properties_contain_anchor_functions(Box const& box)
{
    auto abstract_element = abstract_element_for_abspos_box(box);
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

bool can_replay_saved_abspos_layout_inputs_after_style_change(Box const& box)
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

static bool can_skip_is_anonymous_text_run(Box& box)
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

RustFFI::FfiLayoutFcCallbacks LayoutRustBridge::formatting_context_callbacks()
{
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Cell) == 0);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Row) == 1);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::RowGroup) == 2);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Column) == 3);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::ColumnGroup) == 4);
    static_assert(to_underlying(Painting::Paintable::ConflictingElementKind::Table) == 5);
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
    static_assert(to_underlying(CSS::GridRepeatType::AutoFit) == 0);
    static_assert(to_underlying(CSS::GridRepeatType::AutoFill) == 1);
    static_assert(to_underlying(CSS::GridRepeatType::Fixed) == 2);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::None) == 0);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMinYMin) == 1);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMidYMin) == 2);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMaxYMin) == 3);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMinYMid) == 4);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMidYMid) == 5);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMaxYMid) == 6);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMinYMax) == 7);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMidYMax) == 8);
    static_assert(to_underlying(SVG::PreserveAspectRatio::Align::xMaxYMax) == 9);
    static_assert(to_underlying(SVG::PreserveAspectRatio::MeetOrSlice::Meet) == 0);
    static_assert(to_underlying(SVG::PreserveAspectRatio::MeetOrSlice::Slice) == 1);
    static_assert(to_underlying(SVG::SVGUnits::ObjectBoundingBox) == 0);
    static_assert(to_underlying(SVG::SVGUnits::UserSpaceOnUse) == 1);
    static_assert(to_underlying(CSS::Positioning::Relative) == 2);
    static_assert(to_underlying(CSS::Direction::Ltr) == 0);
    static_assert(to_underlying(CSS::WritingMode::HorizontalTb) == 0);
    static_assert(to_underlying(StaticPositionRect::Alignment::Start) == to_underlying(RustFFI::FfiStaticPositionAlignment::Start));
    static_assert(to_underlying(StaticPositionRect::Alignment::Center) == to_underlying(RustFFI::FfiStaticPositionAlignment::Center));
    static_assert(to_underlying(StaticPositionRect::Alignment::End) == to_underlying(RustFFI::FfiStaticPositionAlignment::End));
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
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Invalid) == 0);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Top) == 1);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Right) == 2);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Bottom) == 3);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Left) == 4);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Center) == 5);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Start) == 6);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::End) == 7);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::SelfStart) == 8);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::SelfEnd) == 9);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Inside) == 10);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Outside) == 11);
    static_assert(to_underlying(RustFFI::FfiAnchorSideKind::Percentage) == 12);
    static_assert(to_underlying(Gfx::GlyphRun::TextType::Common) == 0);
    static_assert(to_underlying(Gfx::GlyphRun::TextType::ContextDependent) == 1);
    static_assert(to_underlying(Gfx::GlyphRun::TextType::EndPadding) == 2);
    static_assert(to_underlying(Gfx::GlyphRun::TextType::Ltr) == 3);
    static_assert(to_underlying(Gfx::GlyphRun::TextType::Rtl) == 4);
    static_assert(sizeof(RustFFI::FfiDrawGlyph) == sizeof(Gfx::DrawGlyph));
    static_assert(alignof(RustFFI::FfiDrawGlyph) == alignof(Gfx::DrawGlyph));
    static_assert(sizeof(Gfx::FloatPoint) == 2 * sizeof(float));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, x) == offsetof(Gfx::DrawGlyph, position));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, y) == offsetof(Gfx::DrawGlyph, position) + sizeof(float));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, length_in_code_units) == offsetof(Gfx::DrawGlyph, length_in_code_units));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, glyph_width) == offsetof(Gfx::DrawGlyph, glyph_width));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, glyph_id) == offsetof(Gfx::DrawGlyph, glyph_id));
    static_assert(offsetof(RustFFI::FfiDrawGlyph, should_paint) == offsetof(Gfx::DrawGlyph, should_paint));

    return {
        .context = this,
        .navigation = navigation_callbacks(),
        .static_position_containing_block = [](void*, void* node) -> void* {
            return const_cast<Box*>(static_cast<Box const*>(node)->static_position_containing_block());
        },
        .inline_containing_block = [](void*, void* node) -> void* {
            return const_cast<InlineNode*>(static_cast<Box const*>(node)->inline_containing_block_if_applicable());
        },
        .non_anonymous_containing_block = [](void*, void* node) -> void* {
            return const_cast<Box*>(static_cast<InlineNode const*>(node)->non_anonymous_containing_block());
        },
        .node_is_ancestor = [](void*, void* ancestor, void* node) {
            return static_cast<Node const*>(ancestor)->is_ancestor_of(*static_cast<Node const*>(node));
        },
        .dom_node_is_inclusive_ancestor = [](void*, void* ancestor, void* node) {
            auto const* ancestor_dom_node = static_cast<Node const*>(ancestor)->dom_node();
            auto const* dom_node = static_cast<Node const*>(node)->dom_node();
            return ancestor_dom_node && dom_node && ancestor_dom_node->is_inclusive_ancestor_of(*dom_node);
        },
        .is_table_cell = [](void*, void* node) {
            return static_cast<Box const*>(node)->display().is_table_cell();
        },
        .needs_inset_resolution = [](void*, void* node) {
            auto const& styled_node = *static_cast<NodeWithStyleAndBoxModelMetrics const*>(node);
            if (styled_node.computed_values().position() == CSS::Positioning::Relative)
                return true;
            auto const* box = as_if<Box>(styled_node);
            return box && box_inset_properties_contain_anchor_functions(*box);
        },
        .build_style_facts = [](void*, void* node) {
            return build_style_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .build_box_facts = [](void*, void* node) {
            auto const& layout_node = *static_cast<Node const*>(node);
            auto const* node_with_style = as_if<NodeWithStyle>(layout_node);
            if (node_with_style)
                return build_layout_box_facts(*node_with_style);
            RustFFI::FfiLayoutBoxFacts facts {};
            facts.is_text_node = layout_node.is_text_node();
            facts.is_break_node = layout_node.is_break_node();
            facts.is_box = layout_node.is_box();
            facts.is_inline = layout_node.is_inline();
            facts.is_atomic_inline = layout_node.is_atomic_inline();
            facts.is_inline_node = is<InlineNode>(layout_node);
            facts.is_fragmented_inline = layout_node.is_fragmented_inline();
            facts.is_list_item_marker_box = layout_node.is_list_item_marker_box();
            facts.is_list_item_box = layout_node.is_list_item_box();
            facts.is_svg_mask_box = layout_node.is_svg_mask_box();
            facts.is_replaced_element = layout_node.is_replaced_element();
            facts.has_dom_node = layout_node.dom_node() != nullptr;
            facts.is_anonymous = layout_node.is_anonymous();
            facts.can_have_children = layout_node.can_have_children();
            facts.initial_containing_block_inline_size = layout_node.document().viewport_rect().width().raw_value();
            facts.document_in_quirks_mode = layout_node.document().in_quirks_mode();
            return facts;
        },
        .build_text_facts = [](void*, void* node, bool should_wrap_lines, bool should_respect_linebreaks, bool unidirectional_ltr, RustFFI::FfiTextNodeFacts* out) {
            VERIFY(out);
            auto const* text_node = as_if<TextNode>(*static_cast<Node const*>(node));
            if (!text_node)
                return false;

            RustFFI::rust_layout_ffi_note_text_facts_build();
            auto text_direction_mode = unidirectional_ltr
                ? TextNode::TextDirectionMode::UnidirectionalLeftToRight
                : TextNode::TextDirectionMode::PerCodePoint;
            auto const& chunk_list = text_node->chunks_for_layout(should_wrap_lines, should_respect_linebreaks, text_direction_mode);
            auto arena = make<TextFactsSnapshotArena>();
            auto const text = text_node->text_for_rendering().utf16_view();
            arena->text.ensure_capacity(text.length_in_code_units());
            for (size_t index = 0; index < text.length_in_code_units(); ++index)
                arena->text.unchecked_append(text.code_unit_at(index));
            arena->chunks.ensure_capacity(chunk_list.chunks.size());
            for (auto const& chunk : chunk_list.chunks) {
                arena->chunks.unchecked_append({
                    .start = chunk.start,
                    .length = chunk.length,
                    .font = chunk.font.ptr(),
                    .has_breaking_newline = chunk.has_breaking_newline,
                    .has_breaking_tab = chunk.has_breaking_tab,
                    .is_all_whitespace = chunk.is_all_whitespace,
                    .can_break_after = chunk.can_break_after,
                    .text_type = static_cast<u8>(to_underlying(chunk.text_type)),
                });
            }

            auto* owner = arena.leak_ptr();
            *out = {
                .text_utf16 = owner->text.data(),
                .text_length_in_code_units = owner->text.size(),
                .chunks = owner->chunks.data(),
                .chunk_count = owner->chunks.size(),
                .should_collapse_whitespace = chunk_list.should_collapse_whitespace,
                .is_generated_for_pseudo_element = text_node->is_generated_for_pseudo_element(),
                .is_empty_editable = is_empty_editable_text_node(*text_node),
                .has_dom_node = static_cast<Node const&>(*text_node).dom_node() != nullptr,
                .retained = owner,
            };
            return true;
        },
        .release_text_facts = [](void*, void* retained) {
            VERIFY(retained);
            RustFFI::rust_layout_ffi_note_text_facts_release();
            delete static_cast<TextFactsSnapshotArena*>(retained);
        },
        .text_may_require_bidi_processing = [](void*, void* node) {
            RustFFI::rust_layout_ffi_note_text_bidi_probe_callback();
            auto const& text_node = *static_cast<TextNode const*>(node);
            return Unicode::may_require_bidi_processing(text_node.text_for_rendering());
        },
        .document_cursor_is_on_node = [](void*, void* node) {
            RustFFI::rust_layout_ffi_note_document_cursor_probe_callback();
            auto const* dom_node = static_cast<Node const*>(node)->dom_node();
            if (!dom_node)
                return false;
            auto cursor_position = dom_node->document().cursor_position();
            return cursor_position && cursor_position->node() == dom_node;
        },
        .shape_text = [](void*, RustFFI::FfiShapeRequest request) {
            VERIFY(request.font);
            VERIFY(request.text_utf16 || request.length_in_code_units == 0);
            VERIFY(request.text_type <= to_underlying(Gfx::GlyphRun::TextType::Rtl));
            RustFFI::rust_layout_ffi_note_shape_text_callback();
            auto text = request.length_in_code_units == 0
                ? Utf16View {}
                : Utf16View { reinterpret_cast<char16_t const*>(request.text_utf16), request.length_in_code_units };
            auto run = Gfx::shape_text(
                { request.baseline_start_x, 0 },
                request.letter_spacing,
                text,
                *static_cast<Gfx::Font const*>(request.font),
                static_cast<Gfx::GlyphRun::TextType>(request.text_type));
            auto* retained = &run.leak_ref();
            ++s_outstanding_shaped_run_handles;
            return RustFFI::FfiShapedRunView {
                .glyphs = reinterpret_cast<RustFFI::FfiDrawGlyph const*>(retained->glyphs().data()),
                .glyph_count = retained->glyphs().size(),
                .width = retained->width(),
                .retained = retained,
            };
        },
        .release_shaped_run = [](void*, void* retained) {
            VERIFY(retained);
            VERIFY(s_outstanding_shaped_run_handles.load() > 0);
            --s_outstanding_shaped_run_handles;
            RustFFI::rust_layout_ffi_note_shaped_run_release();
            static_cast<Gfx::GlyphRun*>(retained)->unref();
        },
        .font_metrics = [](void*, void const* font_pointer, RustFFI::FfiFontPixelMetrics* out) {
            VERIFY(font_pointer);
            VERIFY(out);
            RustFFI::rust_layout_ffi_note_font_metrics_callback();
            auto const& font = *static_cast<Gfx::Font const*>(font_pointer);
            auto const& metrics = font.pixel_metrics();
            *out = {
                .x_height = metrics.x_height,
                .advance_of_ascii_zero = metrics.advance_of_ascii_zero,
                .ascent = metrics.ascent,
                .descent = metrics.descent,
                .pixel_size = font.pixel_size(),
            };
        },
        .font_glyph_width = [](void*, void const* font_pointer, u32 code_point) {
            VERIFY(font_pointer);
            RustFFI::rust_layout_ffi_note_font_glyph_width_callback();
            return static_cast<Gfx::Font const*>(font_pointer)->glyph_width(code_point);
        },
        .font_glyph_id = [](void*, void const* font_pointer, u32 code_point) {
            VERIFY(font_pointer);
            RustFFI::rust_layout_ffi_note_font_glyph_id_callback();
            return static_cast<Gfx::Font const*>(font_pointer)->glyph_id_for_code_point(code_point);
        },
        .build_table_box_facts = [](void*, void* node) {
            return build_table_box_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .build_grid_facts = [](void*, void* node) {
            return build_grid_style_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .release_grid_facts_snapshot = [](void*, void* snapshot) {
            delete static_cast<GridFactsSnapshotArena*>(snapshot);
        },
        .build_svg_facts = [](void*, void* node) {
            return build_svg_element_facts(*static_cast<NodeWithStyle const*>(node));
        },
        .read_paintable_geometry = [](void*, void* node, void* paintable_pointer, RustFFI::FfiPaintableGeometry* out) {
            VERIFY(out);
            auto const* paintable = paintable_pointer
                ? static_cast<Painting::Paintable const*>(paintable_pointer)
                : static_cast<Node const*>(node)->paintable_ptr();
            if (!paintable)
                return false;
            auto const& box_model = paintable->box_model();
            *out = {
                .content_inline_size = paintable->content_width().raw_value(),
                .content_block_size = paintable->content_height().raw_value(),
                .content_offset = {
                    .x = paintable->offset().x().raw_value(),
                    .y = paintable->offset().y().raw_value(),
                },
                .margin_left = box_model.margin.left.raw_value(),
                .margin_right = box_model.margin.right.raw_value(),
                .margin_top = box_model.margin.top.raw_value(),
                .margin_bottom = box_model.margin.bottom.raw_value(),
                .border_left = box_model.border.left.raw_value(),
                .border_right = box_model.border.right.raw_value(),
                .border_top = box_model.border.top.raw_value(),
                .border_bottom = box_model.border.bottom.raw_value(),
                .padding_left = box_model.padding.left.raw_value(),
                .padding_right = box_model.padding.right.raw_value(),
                .padding_top = box_model.padding.top.raw_value(),
                .padding_bottom = box_model.padding.bottom.raw_value(),
                .inset_left = box_model.inset.left.raw_value(),
                .inset_right = box_model.inset.right.raw_value(),
                .inset_top = box_model.inset.top.raw_value(),
                .inset_bottom = box_model.inset.bottom.raw_value(),
            };
            return true;
        },
        .read_paintable_svg_transforms = [](void*, void* node, RustFFI::FfiSvgComputedTransforms* out) {
            VERIFY(out);
            auto const* paintable = static_cast<Node const*>(node)->paintable_ptr();
            Painting::SVGGraphicsPaintable::ComputedTransforms const* transforms = nullptr;
            if (auto const* svg_graphics_paintable = as_if<Painting::SVGGraphicsPaintable>(paintable))
                transforms = &svg_graphics_paintable->computed_transforms();
            if (auto const* svg_foreign_object_paintable = as_if<Painting::SVGForeignObjectPaintable>(paintable))
                transforms = &svg_foreign_object_paintable->computed_transforms();
            if (auto const* svg_svg_paintable = as_if<Painting::SVGSVGPaintable>(paintable))
                transforms = &svg_svg_paintable->computed_transforms();
            if (!transforms)
                return false;
            *out = {
                .viewbox_transform = to_ffi_affine_transform(transforms->svg_to_viewbox_transform()),
                .svg_transform = to_ffi_affine_transform(transforms->svg_transform()),
            };
            return true;
        },
        .compute_svg_path = [](void*, void* node, RustFFI::FfiSvgPathRequest request) {
            return compute_svg_path(*static_cast<NodeWithStyle const*>(node), request);
        },
        .release_svg_path = [](void*, void* path_handle) {
            release_svg_path_handle(path_handle);
        },
        .svg_image_bounding_box = [](void*, void* node, i32 viewport_width, i32 viewport_height) {
            auto const& image_box = as<SVGImageBox>(*static_cast<Node const*>(node));
            auto bounding_box = image_box.dom_node().bounding_box({
                CSSPixels::from_raw(viewport_width),
                CSSPixels::from_raw(viewport_height),
            });
            return RustFFI::FfiFloatRect {
                .x = bounding_box.x(),
                .y = bounding_box.y(),
                .width = bounding_box.width(),
                .height = bounding_box.height(),
            };
        },
        .get_saved_abspos_layout_inputs = [](void*, void* node, RustFFI::FfiAbsposLayoutInputs* out) {
            VERIFY(out);
            auto const* inputs = static_cast<Box const*>(node)->saved_abspos_layout_inputs();
            if (!inputs)
                return false;
            *out = to_ffi_abspos_layout_inputs(*inputs);
            return true;
        },
        .anchor_lookup = [](void*, void* node, size_t anchor_name, void* const* eligible_anchor_boxes, size_t eligible_anchor_box_count, void** out_anchor_box) {
            VERIFY(out_anchor_box);
            *out_anchor_box = nullptr;
            auto const& box = *static_cast<Box const*>(node);
            auto abstract_element = abstract_element_for_abspos_box(box);
            if (!abstract_element.has_value())
                return false;
            auto const* containing_block = box.containing_block();
            if (!containing_block)
                return false;
            Function<bool(DOM::Element&)> is_acceptable_anchor_element = [&](DOM::Element& candidate) {
                auto const* anchor_box = as_if<Box>(candidate.unsafe_layout_node());
                if (!anchor_box || anchor_box == &box)
                    return false;
                bool has_used_values = false;
                for (size_t index = 0; index < eligible_anchor_box_count; ++index) {
                    if (eligible_anchor_boxes[index] == anchor_box) {
                        has_used_values = true;
                        break;
                    }
                }
                if (!has_used_values)
                    return false;
                for (auto const* ancestor = anchor_box->containing_block(); ancestor; ancestor = ancestor->containing_block()) {
                    if (ancestor == containing_block)
                        return true;
                }
                return false;
            };
            auto anchor_element = abstract_element->element().document().element_by_anchor_name(
                Utf16FlyString::from_raw(anchor_name),
                abstract_element->element(),
                is_acceptable_anchor_element);
            if (!anchor_element)
                return false;
            auto* anchor_box = as_if<Box>(anchor_element->unsafe_layout_node());
            if (!anchor_box)
                return false;
            *out_anchor_box = anchor_box;
            return true;
        },
        .build_anchor_function_facts = [](void*, void const* shell) {
            auto const& value = *static_cast<CSS::StyleValue const*>(shell);
            if (!value.is_anchor()) {
                return RustFFI::FfiAnchorFunctionFacts {
                    .has_anchor_name = false,
                    .anchor_name = 0,
                    .side_kind = RustFFI::FfiAnchorSideKind::Invalid,
                    .side_percentage = 0,
                };
            }
            auto const& anchor = value.as_anchor();
            auto const& side = *anchor.anchor_side();
            auto side_kind = RustFFI::FfiAnchorSideKind::Invalid;
            double side_percentage = 0;
            if (side.is_keyword()) {
                switch (side.to_keyword()) {
                case CSS::Keyword::Top:
                    side_kind = RustFFI::FfiAnchorSideKind::Top;
                    break;
                case CSS::Keyword::Right:
                    side_kind = RustFFI::FfiAnchorSideKind::Right;
                    break;
                case CSS::Keyword::Bottom:
                    side_kind = RustFFI::FfiAnchorSideKind::Bottom;
                    break;
                case CSS::Keyword::Left:
                    side_kind = RustFFI::FfiAnchorSideKind::Left;
                    break;
                case CSS::Keyword::Center:
                    side_kind = RustFFI::FfiAnchorSideKind::Center;
                    break;
                case CSS::Keyword::Start:
                    side_kind = RustFFI::FfiAnchorSideKind::Start;
                    break;
                case CSS::Keyword::End:
                    side_kind = RustFFI::FfiAnchorSideKind::End;
                    break;
                case CSS::Keyword::SelfStart:
                    side_kind = RustFFI::FfiAnchorSideKind::SelfStart;
                    break;
                case CSS::Keyword::SelfEnd:
                    side_kind = RustFFI::FfiAnchorSideKind::SelfEnd;
                    break;
                case CSS::Keyword::Inside:
                    side_kind = RustFFI::FfiAnchorSideKind::Inside;
                    break;
                case CSS::Keyword::Outside:
                    side_kind = RustFFI::FfiAnchorSideKind::Outside;
                    break;
                default:
                    break;
                }
            } else if (side.is_percentage()) {
                side_kind = RustFFI::FfiAnchorSideKind::Percentage;
                side_percentage = side.as_percentage().percentage().as_fraction();
            }
            size_t anchor_name = 0;
            if (auto name = anchor.anchor_name(); name.has_value()) {
                anchor_name = name->to_raw_leaked();
                ++s_outstanding_anchor_name_handles;
                RustFFI::rust_layout_ffi_note_anchor_name_retain();
            }
            return RustFFI::FfiAnchorFunctionFacts {
                .has_anchor_name = anchor_name != 0,
                .anchor_name = anchor_name,
                .side_kind = side_kind,
                .side_percentage = side_percentage,
            };
        },
        .anchor_function_fallback = [](void*, void const* shell) {
            auto const& value = *static_cast<CSS::StyleValue const*>(shell);
            if (!value.is_anchor())
                return RustFFI::FfiAnchorFallbackFacts {
                    .kind = RustFFI::FfiAnchorFallbackKind::None,
                    .px = 0,
                    .fraction = 0,
                    .value = nullptr,
                };
            auto fallback = value.as_anchor().fallback_value();
            if (!fallback)
                return RustFFI::FfiAnchorFallbackFacts {
                    .kind = RustFFI::FfiAnchorFallbackKind::None,
                    .px = 0,
                    .fraction = 0,
                    .value = nullptr,
                };
            if (fallback->is_length()) {
                VERIFY(fallback->as_length().length().is_absolute());
                return RustFFI::FfiAnchorFallbackFacts {
                    .kind = RustFFI::FfiAnchorFallbackKind::Px,
                    .px = fallback->as_length().length().absolute_length_to_px().raw_value(),
                    .fraction = 0,
                    .value = nullptr,
                };
            }
            if (fallback->is_percentage()) {
                return RustFFI::FfiAnchorFallbackFacts {
                    .kind = RustFFI::FfiAnchorFallbackKind::Percentage,
                    .px = 0,
                    .fraction = fallback->as_percentage().percentage().as_fraction(),
                    .value = nullptr,
                };
            }
            if (fallback->is_calculated()) {
                return RustFFI::FfiAnchorFallbackFacts {
                    .kind = RustFFI::FfiAnchorFallbackKind::Calculated,
                    .px = 0,
                    .fraction = 0,
                    .value = fallback->as_calculated().rust_style_value_data(),
                };
            }
            VERIFY(fallback->is_anchor());
            return RustFFI::FfiAnchorFallbackFacts {
                .kind = RustFFI::FfiAnchorFallbackKind::Anchor,
                .px = 0,
                .fraction = 0,
                .value = fallback.ptr(),
            };
        },
        .set_resolved_anchor_insets = [](void*, void* node, RustFFI::FfiResolvedAnchorInsets resolved) {
            auto& box = *static_cast<Box*>(node);
            auto const& existing = box.computed_values().inset();
            auto resolve = [](bool resolves, bool is_auto, i32 value, CSS::LengthPercentageOrAuto const& existing_value) {
                if (!resolves)
                    return existing_value;
                if (is_auto)
                    return CSS::LengthPercentageOrAuto::make_auto();
                return CSS::LengthPercentageOrAuto { CSS::LengthPercentage { CSS::Length::make_px(CSSPixels::from_raw(value)) } };
            };
            box.modify_computed_values([&](auto& values) {
                values.set_inset({
                    resolve(resolved.resolves_top, resolved.top_is_auto, resolved.top, existing.top()),
                    resolve(resolved.resolves_right, resolved.right_is_auto, resolved.right, existing.right()),
                    resolve(resolved.resolves_bottom, resolved.bottom_is_auto, resolved.bottom, existing.bottom()),
                    resolve(resolved.resolves_left, resolved.left_is_auto, resolved.left, existing.left()),
                });
            });
        },
        .set_default_scroll_shift = [](void*, void* node, void* anchor, bool horizontal, bool vertical) {
            auto& box = *static_cast<Box*>(node);
            if (!anchor) {
                box.set_default_scroll_shift({}, false, false);
                return;
            }
            box.set_default_scroll_shift(static_cast<Box*>(anchor)->make_weak_ptr(), horizontal, vertical);
        },
        .can_skip_is_anonymous_text_run = [](void*, void* box) {
            return can_skip_is_anonymous_text_run(*static_cast<Box*>(box));
        },
        .set_flex_item = [](void*, void* box, bool is_flex_item) {
            static_cast<Box*>(box)->set_flex_item(is_flex_item);
        },
        .set_grid_item = [](void*, void* box, bool is_grid_item) {
            static_cast<Box*>(box)->set_grid_item(is_grid_item);
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
        .create_flex_layout_data = [](void*, RustFFI::FfiFlexLayoutData const* ffi_data) -> void* {
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
            return data.leak_ptr();
        },
        .release_flex_layout_data = [](void*, void* data) {
            delete static_cast<FlexLayoutData*>(data);
        },
        .create_grid_layout_data = [](void*, RustFFI::FfiGridLayoutData const* ffi_data) -> void* {
            VERIFY(ffi_data);
            static_assert(to_underlying(GridTrackType::Explicit) == 0);
            static_assert(to_underlying(GridTrackType::Implicit) == 1);
            static_assert(to_underlying(GridTrackState::Static) == 0);
            static_assert(to_underlying(GridTrackState::Repeat) == 1);
            static_assert(to_underlying(GridTrackState::Removed) == 2);

            auto data = make<GridLayoutData>();
            data->direction = static_cast<CSS::Direction>(ffi_data->direction);
            data->writing_mode = static_cast<CSS::WritingMode>(ffi_data->writing_mode);
            data->is_subgrid = ffi_data->is_subgrid;

            auto build_dimension = [](RustFFI::FfiGridLayoutDimension const& ffi_dimension) {
                GridLayoutDimension dimension;
                dimension.lines.ensure_capacity(ffi_dimension.line_count);
                for (size_t line_index = 0; line_index < ffi_dimension.line_count; ++line_index) {
                    auto const& ffi_line = ffi_dimension.lines[line_index];
                    GridLayoutLine line {
                        .names = {},
                        .start = CSSPixels::from_raw(ffi_line.start),
                        .breadth = CSSPixels::from_raw(ffi_line.breadth),
                        .type = static_cast<GridTrackType>(ffi_line.type_),
                        .number = ffi_line.number,
                        .negative_number = ffi_line.negative_number,
                    };
                    line.names.ensure_capacity(ffi_line.name_count);
                    for (size_t name_index = 0; name_index < ffi_line.name_count; ++name_index)
                        line.names.unchecked_append(Utf16FlyString::from_raw(ffi_line.names[name_index]));
                    dimension.lines.unchecked_append(move(line));
                }

                dimension.tracks.ensure_capacity(ffi_dimension.track_count);
                for (size_t track_index = 0; track_index < ffi_dimension.track_count; ++track_index) {
                    auto const& ffi_track = ffi_dimension.tracks[track_index];
                    dimension.tracks.unchecked_append({
                        .start = CSSPixels::from_raw(ffi_track.start),
                        .breadth = CSSPixels::from_raw(ffi_track.breadth),
                        .type = static_cast<GridTrackType>(ffi_track.type_),
                        .state = static_cast<GridTrackState>(ffi_track.state),
                    });
                }
                return dimension;
            };

            data->fragments.ensure_capacity(ffi_data->fragment_count);
            for (size_t fragment_index = 0; fragment_index < ffi_data->fragment_count; ++fragment_index) {
                auto const& ffi_fragment = ffi_data->fragments[fragment_index];
                GridLayoutFragment fragment {
                    .areas = {},
                    .columns = build_dimension(ffi_fragment.columns),
                    .rows = build_dimension(ffi_fragment.rows),
                };
                fragment.areas.ensure_capacity(ffi_fragment.area_count);
                for (size_t area_index = 0; area_index < ffi_fragment.area_count; ++area_index) {
                    auto const& ffi_area = ffi_fragment.areas[area_index];
                    fragment.areas.unchecked_append({
                        .name = Utf16FlyString::from_raw(ffi_area.name),
                        .type = static_cast<GridTrackType>(ffi_area.type_),
                        .row_start = ffi_area.row_start,
                        .row_end = ffi_area.row_end,
                        .column_start = ffi_area.column_start,
                        .column_end = ffi_area.column_end,
                    });
                }
                data->fragments.unchecked_append(move(fragment));
            }
            return data.leak_ptr();
        },
        .release_grid_layout_data = [](void*, void* data) {
            delete static_cast<GridLayoutData*>(data);
        },
        .create_used_grid_tracks = [](void*, RustFFI::FfiUsedGridTrackList const* columns, RustFFI::FfiUsedGridTrackList const* rows) -> void* {
            VERIFY(columns);
            VERIFY(rows);
            return new UsedGridTracksCommitData {
                .columns = CSS::GridTrackSizeListStyleValue::create(build_used_grid_track_list(*columns)),
                .rows = CSS::GridTrackSizeListStyleValue::create(build_used_grid_track_list(*rows)),
            };
        },
        .release_used_grid_tracks = [](void*, void* tracks) {
            delete static_cast<UsedGridTracksCommitData*>(tracks);
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
    auto const& first_available_font = node.first_available_font();
    auto const& first_available_font_metrics = first_available_font.pixel_metrics();
    auto build_inset = [&](CSS::PropertyID property_id, CSS::LengthPercentageOrAuto const& inset) {
        auto anchor_inset = values.anchor_inset(property_id);
        if (!anchor_inset)
            return build_style_size_value(inset);

        VERIFY(anchor_inset->is_anchor());
        auto calculation_context = CSS::CalculationContext::for_property(CSS::PropertyNameAndID::from_id(property_id));
        auto root = CSS::CalcNodeRef::non_math_function(
            anchor_inset->as_anchor(),
            CSS::NumericType { CSS::NumericType::BaseType::Length, 1 });
        auto calculated = CSS::CalculatedStyleValue::create(
            move(root),
            CSS::NumericType { CSS::NumericType::BaseType::Length, 1 },
            calculation_context);
        return retain_calculated(*calculated, false);
    };

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
        .inset_top = build_inset(CSS::PropertyID::Top, values.inset().top()),
        .inset_right = build_inset(CSS::PropertyID::Right, values.inset().right()),
        .inset_bottom = build_inset(CSS::PropertyID::Bottom, values.inset().bottom()),
        .inset_left = build_inset(CSS::PropertyID::Left, values.inset().left()),
        .has_position_anchor = values.position_anchor().has_value(),
        .position_anchor_name = [&] {
            if (!values.position_anchor().has_value())
                return static_cast<size_t>(0);
            ++s_outstanding_anchor_name_handles;
            RustFFI::rust_layout_ffi_note_anchor_name_retain();
            return values.position_anchor()->to_raw_leaked();
        }(),
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
        .first_available_font = &first_available_font,
        .font_ascent = first_available_font_metrics.ascent,
        .font_descent = first_available_font_metrics.descent,
        .font_x_height = first_available_font_metrics.x_height,
        .font_pixel_size = first_available_font.pixel_size(),
        .box_sizing = to_underlying(values.box_sizing()),
        .box_sizing_for_aspect_ratio = to_underlying(values.box_sizing_for_aspect_ratio()),
        .overflow_x = to_underlying(values.overflow_x()),
        .overflow_y = to_underlying(values.overflow_y()),
        .text_overflow = to_underlying(values.text_overflow()),
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
    VERIFY(s_outstanding_grid_name_handles.load() == 0);
    VERIFY(s_outstanding_anchor_name_handles.load() == 0);
    VERIFY(s_outstanding_svg_path_handles.load() == 0);
    VERIFY(s_outstanding_shaped_run_handles.load() == 0);
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
    if (facts.has_position_anchor)
        ladybird_layout_release_anchor_name_handle(facts.position_anchor_name);
}

}

extern "C" WEB_API void ladybird_layout_release_calc_handle(void const* handle)
{
    Web::Layout::release_calc_handle(handle);
}

extern "C" WEB_API void ladybird_layout_release_grid_name_handle(size_t raw)
{
    VERIFY(Web::Layout::s_outstanding_grid_name_handles.load() > 0);
    --Web::Layout::s_outstanding_grid_name_handles;
    Web::Layout::RustFFI::rust_layout_ffi_note_grid_name_release();
    Utf16FlyString::unref_raw(raw);
}

extern "C" WEB_API void ladybird_layout_release_anchor_name_handle(size_t raw)
{
    VERIFY(Web::Layout::s_outstanding_anchor_name_handles.load() > 0);
    --Web::Layout::s_outstanding_anchor_name_handles;
    Web::Layout::RustFFI::rust_layout_ffi_note_anchor_name_release();
    Utf16FlyString::unref_raw(raw);
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

extern "C" WEB_API Web::Layout::RustFFI::FfiGridStyleFacts ladybird_layout_test_build_grid_facts_snapshot()
{
    using namespace Web;
    auto arena = make<Layout::GridFactsSnapshotArena>();
    auto alpha = "alpha"_utf16_fly_string;
    auto beta = "beta"_utf16_fly_string;

    CSS::GridTrackSizeList list;
    CSS::GridLineNames alpha_names;
    alpha_names.append(alpha);
    list.append(move(alpha_names));
    list.append(CSS::ExplicitGridTrack {
        CSS::GridSize { CSS::LengthStyleValue::create(CSS::Length::make_px(10)) },
    });
    list.append(CSS::ExplicitGridTrack {
        CSS::GridMinMax {
            CSS::GridSize { CSS::PercentageStyleValue::create(CSS::Percentage { 25 }) },
            CSS::GridSize { CSS::FlexStyleValue::create(CSS::Flex::make_fr(2)) },
        },
    });

    CSS::GridTrackSizeList repeated_list;
    CSS::GridLineNames beta_names;
    beta_names.append(beta);
    repeated_list.append(move(beta_names));
    repeated_list.append(CSS::ExplicitGridTrack {
        CSS::GridSize {
            CSS::FunctionStyleValue::create(
                "fit-content"_utf16_fly_string,
                CSS::LengthStyleValue::create(CSS::Length::make_px(40))),
        },
    });
    list.append(CSS::ExplicitGridTrack {
        CSS::GridRepeat {
            CSS::GridRepeatType::Fixed,
            move(repeated_list),
            CSS::IntegerStyleValue::create(3),
        },
    });

    auto template_columns = Layout::build_grid_track_list(list, *arena);
    auto empty_list = Layout::build_grid_track_list(CSS::GridTrackSizeList::make_none(), *arena);
    arena->areas.append({
        .name_index = arena->intern_name(alpha),
        .implicit_start_name_index = arena->intern_name(
            Layout::make_grid_implicit_line_name(alpha.view(), "-start"sv)),
        .implicit_end_name_index = arena->intern_name(
            Layout::make_grid_implicit_line_name(alpha.view(), "-end"sv)),
        .row_start = 1,
        .row_end = 3,
        .column_start = 2,
        .column_end = 4,
    });
    auto column_start = Layout::build_grid_placement(
        CSS::GridTrackPlacement::make_line(CSS::IntegerStyleValue::create(-1), alpha), *arena);
    auto column_end = Layout::build_grid_placement(
        CSS::GridTrackPlacement::make_span(CSS::IntegerStyleValue::create(2), beta), *arena);

    auto* owner = arena.leak_ptr();
    return {
        .snapshot_owner = owner,
        .names = owner->names.data(),
        .name_count = owner->names.size(),
        .name_indices = owner->name_indices.data(),
        .name_index_count = owner->name_indices.size(),
        .entries = owner->entries.data(),
        .entry_count = owner->entries.size(),
        .template_columns = template_columns,
        .template_rows = empty_list,
        .auto_columns = empty_list,
        .auto_rows = empty_list,
        .areas = owner->areas.data(),
        .area_count = owner->areas.size(),
        .area_row_count = 3,
        .area_column_count = 4,
        .column_start = column_start,
        .column_end = column_end,
        .row_start = Layout::build_grid_placement(CSS::GridTrackPlacement::make_auto(), *owner),
        .row_end = Layout::build_grid_placement(CSS::GridTrackPlacement::make_auto(), *owner),
    };
}

extern "C" WEB_API void ladybird_layout_test_release_grid_facts_snapshot(Web::Layout::RustFFI::FfiGridStyleFacts const* facts)
{
    using namespace Web;
    VERIFY(facts);
    auto* owner = static_cast<Layout::GridFactsSnapshotArena*>(facts->snapshot_owner);
    VERIFY(owner);
    for (auto const& entry : owner->entries) {
        ladybird_layout_release_calc_handle(entry.size.value.calc);
        ladybird_layout_release_calc_handle(entry.min_size.value.calc);
        ladybird_layout_release_calc_handle(entry.max_size.value.calc);
    }
    for (auto raw : owner->names)
        ladybird_layout_release_grid_name_handle(raw);
    delete owner;
    Layout::verify_style_calc_handles_balanced();
}
