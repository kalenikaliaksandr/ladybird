/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/OwnPtr.h>
#include <AK/Optional.h>
#include <AK/RefPtr.h>
#include <AK/Variant.h>
#include <LibWeb/CSS/Enums.h>
#include <LibWeb/CSS/PercentageOr.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>
#include <LibWeb/Layout/LayoutRustFFI.h>

namespace Web::CSS {

class LengthPercentage;
class LengthPercentageOrAuto;
class Size;

}

namespace Web::Layout {

class LayoutRustBridge {
public:
    explicit LayoutRustBridge(LayoutMode);
    ~LayoutRustBridge();

    void run_root_layout(Box& viewport, CSSPixels viewport_inline_size, CSSPixels viewport_block_size, u32 node_count, bool should_collect_devtools_layout_data);
    void compute_subtree_layout(Box&, Painting::Paintable& paintable_to_replace);
    void replay_saved_abspos_layout(Box&, Painting::Paintable& paintable_to_replace);

private:
    [[nodiscard]] RustFFI::FfiLayoutNavCallbacks navigation_callbacks();
    [[nodiscard]] RustFFI::FfiLayoutFcCallbacks formatting_context_callbacks();
    [[nodiscard]] Node const* parent(Node const&) const;
    [[nodiscard]] Node const* first_child(Node const&) const;
    [[nodiscard]] Node const* next_sibling(Node const&) const;
    [[nodiscard]] Node const* previous_sibling(Node const&) const;
    [[nodiscard]] Box const* containing_block(Node const&) const;
    [[nodiscard]] RustFFI::FfiCommitSink commit_sink();

    struct LineCommitContext;
    LayoutMode m_layout_mode;
    Box const* m_commit_root { nullptr };
    OwnPtr<LineCommitContext> m_line_commit_context;
    RefPtr<Painting::Paintable> m_replaced_paintable;
    RefPtr<Painting::Paintable> m_commit_parent_paintable;
    RefPtr<Painting::Paintable> m_commit_insert_before_paintable;
};

struct StyleVerticalAlignFacts {
    bool is_keyword;
    u8 keyword;
    RustFFI::FfiSizeValue value;
};

[[nodiscard]] RustFFI::FfiSizeValue build_style_size_value(CSS::Size const&);
[[nodiscard]] RustFFI::FfiSizeValue build_style_size_value(CSS::LengthPercentage const&);
[[nodiscard]] RustFFI::FfiSizeValue build_style_size_value(CSS::LengthPercentageOrAuto const&);
[[nodiscard]] StyleVerticalAlignFacts build_style_vertical_align_value(Variant<CSS::VerticalAlign, CSS::LengthPercentage> const&);

[[nodiscard]] RustFFI::FfiLayoutBoxFacts build_layout_box_facts(NodeWithStyle const&);
[[nodiscard]] RustFFI::FfiTableBoxFacts build_table_box_facts(NodeWithStyle const&);
[[nodiscard]] Optional<RustFFI::FfiFormattingContextType> formatting_context_type_created_by_box(Box const&);
[[nodiscard]] StringView formatting_context_type_name(RustFFI::FfiFormattingContextType);
[[nodiscard]] bool box_inset_properties_contain_anchor_functions(Box const&);
[[nodiscard]] bool can_replay_saved_abspos_layout_inputs_after_style_change(Box const&);
void verify_style_calc_handles_balanced();

}

// Non-null calc handles returned in FfiSizeValue keep their corresponding
// CSS::CalculatedStyleValue alive. This is their matching release hook.
extern "C" WEB_API void ladybird_layout_release_calc_handle(void const*);
// Releases one name-table reference transferred by FfiGridStyleFacts.
extern "C" WEB_API void ladybird_layout_release_grid_name_handle(size_t);
// Releases one position-anchor name reference transferred by a lazy style
// field decode.
extern "C" WEB_API void ladybird_layout_release_anchor_name_handle(size_t);

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_size_value(u8 kind, i32 px_raw, double fraction);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_auto_margin_value();
extern "C" WEB_API Web::Layout::StyleVerticalAlignFacts ladybird_layout_test_build_vertical_align(bool is_keyword, u8 keyword, i32 px_raw);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_calc_size_value();
extern "C" WEB_API i32 ladybird_layout_test_resolve_calc_handle_cpp(void const*, i32 percentage_basis_raw);
extern "C" WEB_API void ladybird_layout_test_verify_calc_handles_balanced();
extern "C" WEB_API Web::Layout::RustFFI::FfiGridStyleFacts ladybird_layout_test_build_grid_facts_snapshot();
extern "C" WEB_API void ladybird_layout_test_release_grid_facts_snapshot(Web::Layout::RustFFI::FfiGridStyleFacts const*);
