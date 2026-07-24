/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

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
    [[nodiscard]] RustFFI::FfiLayoutNavCallbacks navigation_callbacks();

private:
    [[nodiscard]] Node const* parent(Node const&) const;
    [[nodiscard]] Node const* first_child(Node const&) const;
    [[nodiscard]] Node const* next_sibling(Node const&) const;
    [[nodiscard]] Node const* previous_sibling(Node const&) const;
    [[nodiscard]] Box const* containing_block(Node const&) const;
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
void release_style_facts(RustFFI::FfiStyleFacts const&);
void verify_style_calc_handles_balanced();

}

// Non-null calc handles returned in FfiSizeValue keep their corresponding
// CSS::CalculatedStyleValue alive. This is their matching release hook.
extern "C" WEB_API void ladybird_layout_release_calc_handle(void const*);

extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_size_value(u8 kind, i32 px_raw, double fraction);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_auto_margin_value();
extern "C" WEB_API Web::Layout::StyleVerticalAlignFacts ladybird_layout_test_build_vertical_align(bool is_keyword, u8 keyword, i32 px_raw);
extern "C" WEB_API Web::Layout::RustFFI::FfiSizeValue ladybird_layout_test_build_calc_size_value();
extern "C" WEB_API i32 ladybird_layout_test_resolve_calc_handle_cpp(void const*, i32 percentage_basis_raw);
extern "C" WEB_API void ladybird_layout_test_verify_calc_handles_balanced();
