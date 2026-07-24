/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/OwnPtr.h>
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

struct LayoutInput;

class LayoutRustBridge {
public:
    explicit LayoutRustBridge(FormattingContext&);
    ~LayoutRustBridge();

    [[nodiscard]] RustFFI::FfiLayoutNavCallbacks navigation_callbacks();
    [[nodiscard]] RustFFI::FfiLayoutFcCallbacks formatting_context_callbacks();

    [[nodiscard]] static RustFFI::FfiLayoutInput to_ffi(LayoutInput const&);
    [[nodiscard]] static LayoutInput from_ffi(RustFFI::FfiLayoutInput const&);

private:
    [[nodiscard]] Node const* parent(Node const&) const;
    [[nodiscard]] Node const* first_child(Node const&) const;
    [[nodiscard]] Node const* next_sibling(Node const&) const;
    [[nodiscard]] Node const* previous_sibling(Node const&) const;
    [[nodiscard]] Box const* containing_block(Node const&) const;

    FormattingContext& m_formatting_context;
    HashMap<Box const*, OwnPtr<FormattingContext>> m_child_contexts;
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

[[nodiscard]] RustFFI::FfiStyleFacts build_style_facts(NodeWithStyle const&);
[[nodiscard]] RustFFI::FfiLayoutBoxFacts build_layout_box_facts(NodeWithStyle const&);
[[nodiscard]] RustFFI::FfiTableBoxFacts build_table_box_facts(NodeWithStyle const&);
void release_style_facts(RustFFI::FfiStyleFacts const&);
void verify_style_calc_handles_balanced();

}

// Non-null calc handles returned in FfiSizeValue keep their corresponding
// CSS::CalculatedStyleValue alive. This is their matching release hook.
extern "C" WEB_API void ladybird_layout_release_calc_handle(void const*);
// Releases one name-table reference transferred by FfiGridStyleFacts.
extern "C" WEB_API void ladybird_layout_release_grid_name_handle(size_t);
// Releases one position-anchor name reference transferred by FfiStyleFacts.
extern "C" WEB_API void ladybird_layout_release_anchor_name_handle(size_t);

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_size_value(u8 kind, i32 px_raw, double fraction);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_auto_margin_value();
extern "C" WEB_API Web::Layout::StyleVerticalAlignFacts ladybird_layout_test_build_vertical_align(bool is_keyword, u8 keyword, i32 px_raw);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_calc_size_value();
extern "C" WEB_API i32 ladybird_layout_test_resolve_calc_handle_cpp(void const*, i32 percentage_basis_raw);
extern "C" WEB_API void ladybird_layout_test_verify_calc_handles_balanced();
extern "C" WEB_API Web::Layout::RustFFI::FfiGridStyleFacts ladybird_layout_test_build_grid_facts_snapshot();
extern "C" WEB_API void ladybird_layout_test_release_grid_facts_snapshot(Web::Layout::RustFFI::FfiGridStyleFacts const*);
