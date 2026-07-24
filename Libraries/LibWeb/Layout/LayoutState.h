/*
 * Copyright (c) 2022, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/BumpAllocator.h>
#include <AK/HashMap.h>
#include <AK/OwnPtr.h>
#include <AK/kmalloc.h>
#include <LibGfx/Path.h>
#include <LibGfx/Point.h>
#include <LibWeb/Layout/AbsposLayoutInputs.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Layout/LayoutRustFFI.h>
#include <LibWeb/Layout/LineBox.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/Painting/SVGGraphicsPaintable.h>

namespace Web::Layout {

enum class SizeConstraint {
    None,
    MinContent,
    MaxContent,
};

class AvailableSize;
class AvailableSpace;
struct RustLayoutState;

// Sparse, index-based container using two-level page tables.
// Layout state is throwaway — rebuilt on every layout pass — so a
// flat vector pre-allocated for the entire tree wastes memory, while
// a hash map pays hashing overhead on every access. Page tables give
// O(1) lookup without hashing, allocating pages only on first write.
template<typename T>
class PagedStore {
    static constexpr u32 PageBits = 4;
    static constexpr u32 PageSize = 1u << PageBits;
    static constexpr u32 PageMask = PageSize - 1;

    struct Page {
        AK_ALLOC_WITH_KMALLOC_PARTITION(HeapPartition::Layout);

        T* entries[PageSize] {};
    };

public:
    PagedStore() = default;

    PagedStore(PagedStore const&) = delete;
    PagedStore& operator=(PagedStore const&) = delete;
    PagedStore(PagedStore&&) = delete;
    PagedStore& operator=(PagedStore&&) = delete;

    void ensure_capacity(u32 count)
    {
        m_pages.resize((count + PageSize - 1) >> PageBits);
    }

    T* get(u32 index) const
    {
        auto page_index = index >> PageBits;
        if (page_index >= m_pages.size())
            return nullptr;
        auto const& page = m_pages[page_index];
        if (!page)
            return nullptr;
        return page->entries[index & PageMask];
    }

    template<typename... Args>
    T& allocate(u32 index, Args&&... args)
    {
        auto page_index = index >> PageBits;
        if (page_index >= m_pages.size())
            m_pages.resize(page_index + 1);
        auto& page = m_pages[page_index];
        if (!page)
            page = make<Page>();
        auto& entry = page->entries[index & PageMask];
        VERIFY(!entry);
        entry = m_allocator.allocate(forward<Args>(args)...);
        VERIFY(entry);
        return *entry;
    }

    template<typename Callback>
    void for_each(Callback callback)
    {
        for (auto const& page : m_pages) {
            if (!page)
                continue;
            for (auto& entry : page->entries) {
                if (entry)
                    callback(*entry);
            }
        }
    }

private:
    static constexpr size_t BumpAllocatorChunkSize = 4 * KiB;

    Vector<OwnPtr<Page>> m_pages;
    UniformBumpAllocator<T, false, BumpAllocatorChunkSize> m_allocator;
};

struct LayoutState {
    AK_ALLOC_WITH_KMALLOC_PARTITION(HeapPartition::Layout);

    struct WEB_API UsedValues {
        explicit UsedValues(RustFFI::UsedValuesCore& core)
            : m_core(&core)
        {
        }
        ~UsedValues();

        NodeWithStyle const& node() const { return *static_cast<NodeWithStyle const*>(m_core->node); }
        NodeWithStyle& node() { return *static_cast<NodeWithStyle*>(m_core->node); }
        RustFFI::UsedValuesCore& core() { return *m_core; }
        RustFFI::UsedValuesCore const& core() const { return *m_core; }
        void set_node(NodeWithStyle const&, Optional<CSSPixels> percentage_basis_inline_size = {}, Optional<CSSPixels> percentage_basis_block_size = {});

        CSSPixels content_inline_size() const { return CSSPixels::from_raw(m_core->content_inline_size); }
        CSSPixels content_block_size() const { return CSSPixels::from_raw(m_core->content_block_size); }
        void set_content_inline_size(CSSPixels);
        void set_content_block_size(CSSPixels);

        CSSPixelSize content_size() const { return { content_inline_size(), content_block_size() }; }

        void set_indefinite_content_inline_size();
        void set_indefinite_content_block_size();

        void set_has_definite_inline_size(bool has_definite_inline_size) { m_core->has_definite_inline_size = has_definite_inline_size; }
        void set_has_definite_block_size(bool has_definite_block_size) { m_core->has_definite_block_size = has_definite_block_size; }

        bool has_definite_inline_size() const { return m_core->has_definite_inline_size && inline_size_constraint() == SizeConstraint::None; }
        bool has_definite_block_size() const { return m_core->has_definite_block_size && block_size_constraint() == SizeConstraint::None; }

        // Returns the available space for content inside this layout box.
        // If the space in an axis is indefinite, and the outer space is an intrinsic sizing constraint,
        // the constraint is used in that axis instead.
        AvailableSpace available_inner_space_or_constraints_from(AvailableSpace const& outer_space) const;

        void materialize_from_paintable(Painting::Paintable const&);
        bool is_materialized_from_paintable() const { return m_core->materialized_from_paintable; }

        CSSPixelPoint content_offset() const
        {
            if (!m_core->has_content_offset)
                return {};
            return { CSSPixels::from_raw(m_core->content_offset.x), CSSPixels::from_raw(m_core->content_offset.y) };
        }
        LogicalOffset content_logical_offset() const
        {
            auto offset = content_offset();
            return { offset.x(), offset.y() };
        }

        bool is_placed() const { return m_core->has_content_offset; }

        SizeConstraint inline_size_constraint() const { return static_cast<SizeConstraint>(m_core->inline_size_constraint); }
        void set_inline_size_constraint(SizeConstraint constraint) { m_core->inline_size_constraint = static_cast<RustFFI::FfiSizeConstraint>(constraint); }
        SizeConstraint block_size_constraint() const { return static_cast<SizeConstraint>(m_core->block_size_constraint); }
        void set_block_size_constraint(SizeConstraint constraint) { m_core->block_size_constraint = static_cast<RustFFI::FfiSizeConstraint>(constraint); }

        CSSPixels margin_left() const { return CSSPixels::from_raw(m_core->margin_left); }
        void set_margin_left(CSSPixels value) { m_core->margin_left = value.raw_value(); }
        CSSPixels margin_right() const { return CSSPixels::from_raw(m_core->margin_right); }
        void set_margin_right(CSSPixels value) { m_core->margin_right = value.raw_value(); }
        CSSPixels margin_top() const { return CSSPixels::from_raw(m_core->margin_top); }
        void set_margin_top(CSSPixels value) { m_core->margin_top = value.raw_value(); }
        CSSPixels margin_bottom() const { return CSSPixels::from_raw(m_core->margin_bottom); }
        void set_margin_bottom(CSSPixels value) { m_core->margin_bottom = value.raw_value(); }

        CSSPixels border_left() const { return CSSPixels::from_raw(m_core->border_left); }
        void set_border_left(CSSPixels value) { m_core->border_left = value.raw_value(); }
        CSSPixels border_right() const { return CSSPixels::from_raw(m_core->border_right); }
        void set_border_right(CSSPixels value) { m_core->border_right = value.raw_value(); }
        CSSPixels border_top() const { return CSSPixels::from_raw(m_core->border_top); }
        void set_border_top(CSSPixels value) { m_core->border_top = value.raw_value(); }
        CSSPixels border_bottom() const { return CSSPixels::from_raw(m_core->border_bottom); }
        void set_border_bottom(CSSPixels value) { m_core->border_bottom = value.raw_value(); }

        CSSPixels padding_left() const { return CSSPixels::from_raw(m_core->padding_left); }
        void set_padding_left(CSSPixels value) { m_core->padding_left = value.raw_value(); }
        CSSPixels padding_right() const { return CSSPixels::from_raw(m_core->padding_right); }
        void set_padding_right(CSSPixels value) { m_core->padding_right = value.raw_value(); }
        CSSPixels padding_top() const { return CSSPixels::from_raw(m_core->padding_top); }
        void set_padding_top(CSSPixels value) { m_core->padding_top = value.raw_value(); }
        CSSPixels padding_bottom() const { return CSSPixels::from_raw(m_core->padding_bottom); }
        void set_padding_bottom(CSSPixels value) { m_core->padding_bottom = value.raw_value(); }

        CSSPixels inset_left() const { return CSSPixels::from_raw(m_core->inset_left); }
        void set_inset_left(CSSPixels value) { m_core->inset_left = value.raw_value(); }
        CSSPixels inset_right() const { return CSSPixels::from_raw(m_core->inset_right); }
        void set_inset_right(CSSPixels value) { m_core->inset_right = value.raw_value(); }
        CSSPixels inset_top() const { return CSSPixels::from_raw(m_core->inset_top); }
        void set_inset_top(CSSPixels value) { m_core->inset_top = value.raw_value(); }
        CSSPixels inset_bottom() const { return CSSPixels::from_raw(m_core->inset_bottom); }
        void set_inset_bottom(CSSPixels value) { m_core->inset_bottom = value.raw_value(); }

        Vector<LineBox>& line_boxes() { return m_cpp_extension.line_boxes; }
        Vector<LineBox> const& line_boxes() const { return m_cpp_extension.line_boxes; }

        Vector<Painting::InlineBoxPiece>& inline_box_pieces() { return m_cpp_extension.inline_box_pieces; }
        Vector<Painting::InlineBoxPiece> const& inline_box_pieces() const { return m_cpp_extension.inline_box_pieces; }

        // Baselines of this box's in-flow content, relative to the box's content-box top edge.
        // Populated eagerly by the formatting context that lays out this box's children.
        // An empty Optional means the box has no baseline set (https://drafts.csswg.org/css-align-3/#baseline-export);
        // consumers synthesize a baseline from the margin box instead.
        Optional<CSSPixels> first_baseline() const
        {
            if (!m_core->has_first_baseline)
                return {};
            return CSSPixels::from_raw(m_core->first_baseline);
        }
        void set_first_baseline(Optional<CSSPixels> baseline)
        {
            m_core->has_first_baseline = baseline.has_value();
            m_core->first_baseline = baseline.value_or(0).raw_value();
        }
        Optional<CSSPixels> last_baseline() const
        {
            if (!m_core->has_last_baseline)
                return {};
            return CSSPixels::from_raw(m_core->last_baseline);
        }
        void set_last_baseline(Optional<CSSPixels> baseline)
        {
            m_core->has_last_baseline = baseline.has_value();
            m_core->last_baseline = baseline.value_or(0).raw_value();
        }

        CSSPixels margin_box_left() const { return margin_left() + border_left_collapsed() + padding_left(); }
        CSSPixels margin_box_right() const { return margin_right() + border_right_collapsed() + padding_right(); }
        CSSPixels margin_box_top() const { return margin_top() + border_top_collapsed() + padding_top(); }
        CSSPixels margin_box_bottom() const { return margin_bottom() + border_bottom_collapsed() + padding_bottom(); }

        CSSPixels margin_box_inline_size() const { return margin_box_left() + content_inline_size() + margin_box_right(); }
        CSSPixels margin_box_block_size() const { return margin_box_top() + content_block_size() + margin_box_bottom(); }

        CSSPixels border_box_left() const { return border_left_collapsed() + padding_left(); }
        CSSPixels border_box_right() const { return border_right_collapsed() + padding_right(); }
        CSSPixels border_box_top() const { return border_top_collapsed() + padding_top(); }
        CSSPixels border_box_bottom() const { return border_bottom_collapsed() + padding_bottom(); }

        CSSPixels border_box_inline_size() const { return border_box_left() + content_inline_size() + border_box_right(); }
        CSSPixels border_box_block_size() const { return border_box_top() + content_block_size() + border_box_bottom(); }

        CSSPixels padding_box_inline_size() const { return padding_left() + content_inline_size() + padding_right(); }
        CSSPixels padding_box_block_size() const { return padding_top() + content_block_size() + padding_bottom(); }

        Optional<LineBoxFragmentCoordinate> containing_line_box_fragment() const
        {
            if (!m_core->has_containing_line_box_fragment)
                return {};
            return LineBoxFragmentCoordinate {
                .line_box_index = m_core->containing_line_box_fragment.line_box_index,
                .fragment_index = m_core->containing_line_box_fragment.fragment_index,
            };
        }
        void set_containing_line_box_fragment(Optional<LineBoxFragmentCoordinate> coordinate)
        {
            m_core->has_containing_line_box_fragment = coordinate.has_value();
            if (coordinate.has_value()) {
                m_core->containing_line_box_fragment = {
                    .line_box_index = coordinate->line_box_index,
                    .fragment_index = coordinate->fragment_index,
                };
            } else {
                m_core->containing_line_box_fragment = {};
            }
        }

        void set_lowest_floating_descendant_bottom_margin_edge(Optional<CSSPixels> bottom_margin_edge) { ensure_rare_data().lowest_floating_descendant_bottom_margin_edge = bottom_margin_edge; }
        Optional<CSSPixels> lowest_floating_descendant_bottom_margin_edge() const
        {
            if (!m_cpp_extension.rare)
                return {};
            return m_cpp_extension.rare->lowest_floating_descendant_bottom_margin_edge;
        }

        void set_override_borders_data(Painting::Paintable::BordersDataWithElementKind const& override_borders_data) { ensure_rare_data().override_borders_data = override_borders_data; }
        Optional<Painting::Paintable::BordersDataWithElementKind> const& override_borders_data() const
        {
            static Optional<Painting::Paintable::BordersDataWithElementKind> const empty;
            return m_cpp_extension.rare ? m_cpp_extension.rare->override_borders_data : empty;
        }

        void set_table_cell_coordinates(Painting::Paintable::TableCellCoordinates const& table_cell_coordinates) { ensure_rare_data().table_cell_coordinates = table_cell_coordinates; }
        Optional<Painting::Paintable::TableCellCoordinates> const& table_cell_coordinates() const
        {
            static Optional<Painting::Paintable::TableCellCoordinates> const empty;
            return m_cpp_extension.rare ? m_cpp_extension.rare->table_cell_coordinates : empty;
        }

        void set_computed_svg_path(Gfx::Path const& svg_path) { ensure_rare_data().computed_svg_path = svg_path; }
        Gfx::Path* computed_svg_path()
        {
            if (!m_cpp_extension.rare || !m_cpp_extension.rare->computed_svg_path.has_value())
                return nullptr;
            return &*m_cpp_extension.rare->computed_svg_path;
        }

        void set_computed_svg_transforms(Painting::SVGGraphicsPaintable::ComputedTransforms const& computed_transforms) { ensure_rare_data().computed_svg_transforms = computed_transforms; }
        Optional<Painting::SVGGraphicsPaintable::ComputedTransforms> const& computed_svg_transforms() const
        {
            static Optional<Painting::SVGGraphicsPaintable::ComputedTransforms> const empty;
            return m_cpp_extension.rare ? m_cpp_extension.rare->computed_svg_transforms : empty;
        }

        void set_grid_layout_data(OwnPtr<GridLayoutData> grid_layout_data) { ensure_rare_data().grid_layout_data = move(grid_layout_data); }
        GridLayoutData const* grid_layout_data() const
        {
            return m_cpp_extension.rare ? m_cpp_extension.rare->grid_layout_data.ptr() : nullptr;
        }
        OwnPtr<GridLayoutData> take_grid_layout_data()
        {
            if (!m_cpp_extension.rare)
                return {};
            return move(m_cpp_extension.rare->grid_layout_data);
        }

        void set_grid_template_columns(RefPtr<CSS::GridTrackSizeListStyleValue const> used_values_for_grid_template_columns) { ensure_rare_data().grid_template_columns = move(used_values_for_grid_template_columns); }
        RefPtr<CSS::GridTrackSizeListStyleValue const> const& grid_template_columns() const
        {
            static auto const& empty = *new RefPtr<CSS::GridTrackSizeListStyleValue const>;
            return m_cpp_extension.rare ? m_cpp_extension.rare->grid_template_columns : empty;
        }

        void set_grid_template_rows(RefPtr<CSS::GridTrackSizeListStyleValue const> used_values_for_grid_template_rows) { ensure_rare_data().grid_template_rows = move(used_values_for_grid_template_rows); }
        RefPtr<CSS::GridTrackSizeListStyleValue const> const& grid_template_rows() const
        {
            static auto const& empty = *new RefPtr<CSS::GridTrackSizeListStyleValue const>;
            return m_cpp_extension.rare ? m_cpp_extension.rare->grid_template_rows : empty;
        }

        void set_flex_layout_data(OwnPtr<FlexLayoutData> flex_layout_data) { ensure_rare_data().flex_layout_data = move(flex_layout_data); }
        FlexLayoutData const* flex_layout_data() const
        {
            return m_cpp_extension.rare ? m_cpp_extension.rare->flex_layout_data.ptr() : nullptr;
        }
        OwnPtr<FlexLayoutData> take_flex_layout_data()
        {
            if (!m_cpp_extension.rare)
                return {};
            return move(m_cpp_extension.rare->flex_layout_data);
        }

        void set_abspos_layout_inputs(AbsposLayoutInputs abspos_layout_inputs) { ensure_rare_data().abspos_layout_inputs = move(abspos_layout_inputs); }
        AbsposLayoutInputs const* abspos_layout_inputs() const
        {
            if (!m_cpp_extension.rare || !m_cpp_extension.rare->abspos_layout_inputs.has_value())
                return nullptr;
            return &*m_cpp_extension.rare->abspos_layout_inputs;
        }

    private:
        friend struct LayoutState;
        friend class FormattingContext;

        void place(CSSPixelPoint content_offset)
        {
            VERIFY(!m_core->has_content_offset);
            m_core->has_content_offset = true;
            m_core->content_offset = {
                .x = content_offset.x().raw_value(),
                .y = content_offset.y().raw_value(),
            };
        }

        AvailableSize available_inline_size_inside() const;
        AvailableSize available_block_size_inside() const;

        bool use_collapsing_borders_model() const { return m_cpp_extension.rare && m_cpp_extension.rare->override_borders_data.has_value(); }
        // Implement the collapsing border model https://www.w3.org/TR/CSS22/tables.html#collapsing-borders.
        CSSPixels border_left_collapsed() const { return use_collapsing_borders_model() ? round(border_left() / 2) : border_left(); }
        CSSPixels border_right_collapsed() const { return use_collapsing_borders_model() ? round(border_right() / 2) : border_right(); }
        CSSPixels border_top_collapsed() const { return use_collapsing_borders_model() ? round(border_top() / 2) : border_top(); }
        CSSPixels border_bottom_collapsed() const { return use_collapsing_borders_model() ? round(border_bottom() / 2) : border_bottom(); }

        struct RareData {
            AK_ALLOC_WITH_KMALLOC_PARTITION(HeapPartition::Layout);

            Optional<CSSPixels> lowest_floating_descendant_bottom_margin_edge;
            Optional<Painting::Paintable::TableCellCoordinates> table_cell_coordinates;
            Optional<Gfx::Path> computed_svg_path;
            OwnPtr<GridLayoutData> grid_layout_data;
            OwnPtr<FlexLayoutData> flex_layout_data;
            RefPtr<CSS::GridTrackSizeListStyleValue const> grid_template_columns;
            RefPtr<CSS::GridTrackSizeListStyleValue const> grid_template_rows;
            Optional<Painting::Paintable::BordersDataWithElementKind> override_borders_data;
            Optional<Painting::SVGGraphicsPaintable::ComputedTransforms> computed_svg_transforms;
            Optional<AbsposLayoutInputs> abspos_layout_inputs;
        };

        struct CppUsedValuesExtension {
            Vector<LineBox> line_boxes;
            Vector<Painting::InlineBoxPiece> inline_box_pieces;
            OwnPtr<RareData> rare;
        };

        RareData& ensure_rare_data()
        {
            if (!m_cpp_extension.rare)
                m_cpp_extension.rare = make<RareData>();
            return *m_cpp_extension.rare;
        }

        RustFFI::UsedValuesCore* m_core { nullptr };
        CppUsedValuesExtension m_cpp_extension;
        // UniformBumpAllocator's chunk walk assumes that each completed chunk
        // contains an integral number of entries. Keep this cache entry at 80
        // bytes so a 4 KiB layout chunk has no trailing partial entry.
        u8 m_allocator_padding[16] {};
    };

    enum class Purpose : u8 {
        Commit,      // Results will be committed (full layout, or a partial relayout of a subtree).
        Measurement, // Throwaway state; only scalar measurements are read back, nothing is committed.
    };

    LayoutState();
    LayoutState(NodeWithStyle const& subtree_root, Purpose);
    ~LayoutState();

    bool is_for_measurement() const { return m_purpose == Purpose::Measurement; }

    // Commits the used values produced by layout and builds a paintable tree. The overload
    // taking a paintable to replace splices the rebuilt paint subtree in at that paintable's
    // position; the plain overload replaces the root's own paintable when there is one.
    void commit(Box& root);
    void commit(Box& root, Painting::Paintable& paintable_to_replace);

    void ensure_capacity(u32 node_count);

    void set_should_collect_devtools_layout_data(bool should_collect) { m_should_collect_devtools_layout_data = should_collect; }
    bool should_collect_devtools_layout_data() const { return m_should_collect_devtools_layout_data; }

    UsedValues& get_mutable(NodeWithStyle const&);
    UsedValues const& get(NodeWithStyle const&) const;

    UsedValues& create(NodeWithStyle const&, Optional<CSSPixels> percentage_basis_inline_size, Optional<CSSPixels> percentage_basis_block_size);

    UsedValues& populate_from_paintable(NodeWithStyle const&, Painting::Paintable const&);

    UsedValues const* try_get(NodeWithStyle const&) const;
    UsedValues* try_get_mutable(NodeWithStyle const&);
    UsedValues const* try_get(Node const&) const;
    UsedValues* try_get_mutable(Node const&);

    bool has_subtree_root() const { return m_subtree_root != nullptr; }
    void* rust_state_handle() const { return m_rust_state; }

    struct ContainedAbsposChild {
        Box const* box { nullptr };
        StaticPositionRect static_position_rect;
    };
    void register_contained_abspos_child(Box const& target, Box const& child, StaticPositionRect const&);
    [[nodiscard]] Optional<ContainedAbsposChild> take_next_contained_abspos_child(Box const& target);

private:
    void commit_used_values_and_build_paint_tree(Box& root, RefPtr<Painting::Paintable> parent_paintable, RefPtr<Painting::Paintable> insert_before_paintable);
    void commit_subtree_and_build_paint_tree(Node&, Painting::Paintable* parent_paintable, Painting::Paintable* insert_before_paintable);
    RefPtr<Painting::Paintable> commit_used_values_to_paintable(UsedValues&);
    void resolve_relative_positions_and_assign_inline_box_geometry();

    RustLayoutState* m_rust_state { nullptr };
    PagedStore<UsedValues> m_used_values_store;
    Layout::NodeWithStyle const* m_subtree_root { nullptr };

    Purpose m_purpose { Purpose::Commit };
    bool m_should_collect_devtools_layout_data { false };
};

inline CSSPixels clamp_to_max_dimension_value(CSSPixels value)
{
    if (value.might_be_saturated())
        return CSSPixels(CSSPixels::max_dimension_value);
    return value;
}

}
