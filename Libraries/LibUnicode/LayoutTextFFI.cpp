/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>
#include <LibUnicode/Bidi.h>
#include <LibUnicode/CaseMapping.h>
#include <LibUnicode/FullwidthMapping.h>

struct UnicodeLayoutTextMappingEdit {
    size_t source_start { 0 };
    size_t source_length { 0 };
    size_t destination_start { 0 };
    size_t destination_length { 0 };
};

template<typename Edit>
constexpr bool edit_layout_matches = sizeof(Edit) == sizeof(UnicodeLayoutTextMappingEdit)
    && offsetof(Edit, source_start) == offsetof(UnicodeLayoutTextMappingEdit, source_start)
    && offsetof(Edit, source_length) == offsetof(UnicodeLayoutTextMappingEdit, source_length)
    && offsetof(Edit, destination_start) == offsetof(UnicodeLayoutTextMappingEdit, destination_start)
    && offsetof(Edit, destination_length) == offsetof(UnicodeLayoutTextMappingEdit, destination_length);
static_assert(edit_layout_matches<Unicode::CaseMappingEdit>);
static_assert(edit_layout_matches<Unicode::FullwidthMappingEdit>);

using UnicodeLayoutTextMappingSink = void (*)(void* context, u8 const* ascii_text, u16 const* utf16_text, size_t length_in_code_units, UnicodeLayoutTextMappingEdit const* edits, size_t edit_count);

extern "C" {
void unicode_layout_apply_case_mapping(u16 const* text, size_t length_in_code_units, u8 mapping, u8 const* ascii_locale, size_t locale_length, bool preserve_existing_trailing_code_points, void* context, UnicodeLayoutTextMappingSink sink);
void unicode_layout_apply_fullwidth_mapping(u16 const* text, size_t length_in_code_units, void* context, UnicodeLayoutTextMappingSink sink);
bool unicode_layout_may_require_bidi_processing(u16 const* text, size_t length_in_code_units);
}

static constexpr u8 UNICODE_LAYOUT_CASE_MAPPING_LOWERCASE = 0;
static constexpr u8 UNICODE_LAYOUT_CASE_MAPPING_UPPERCASE = 1;
static constexpr u8 UNICODE_LAYOUT_CASE_MAPPING_TITLECASE = 2;

static Utf16View utf16_view_from_ffi(u16 const* text, size_t length_in_code_units)
{
    if (length_in_code_units == 0)
        return {};
    return Utf16View { reinterpret_cast<char16_t const*>(text), length_in_code_units };
}

template<typename Edit>
static void deliver_mapping_result(Utf16String const& text, Vector<Edit> const& mapping_edits, void* context, UnicodeLayoutTextMappingSink sink)
{
    auto view = text.utf16_view();
    sink(context,
        view.has_ascii_storage() ? reinterpret_cast<u8 const*>(view.ascii_span().data()) : nullptr,
        view.has_ascii_storage() ? nullptr : reinterpret_cast<u16 const*>(view.utf16_span().data()),
        view.length_in_code_units(),
        reinterpret_cast<UnicodeLayoutTextMappingEdit const*>(mapping_edits.data()),
        mapping_edits.size());
}

extern "C" void unicode_layout_apply_case_mapping(u16 const* text, size_t length_in_code_units, u8 mapping, u8 const* ascii_locale, size_t locale_length, bool preserve_existing_trailing_code_points, void* context, UnicodeLayoutTextMappingSink sink)
{
    auto case_mapping = [&] {
        switch (mapping) {
        case UNICODE_LAYOUT_CASE_MAPPING_LOWERCASE:
            return Unicode::CaseMapping::Lowercase;
        case UNICODE_LAYOUT_CASE_MAPPING_UPPERCASE:
            return Unicode::CaseMapping::Uppercase;
        case UNICODE_LAYOUT_CASE_MAPPING_TITLECASE:
            return Unicode::CaseMapping::Titlecase;
        }
        VERIFY_NOT_REACHED();
    }();

    Optional<Utf16View> locale_view;
    if (ascii_locale)
        locale_view = Utf16View { StringView { reinterpret_cast<char const*>(ascii_locale), locale_length } };

    auto result = Unicode::apply_case_mapping(
        Utf16String::from_utf16(utf16_view_from_ffi(text, length_in_code_units)),
        case_mapping,
        locale_view,
        preserve_existing_trailing_code_points ? TrailingCodePointTransformation::PreserveExisting : TrailingCodePointTransformation::Lowercase);
    deliver_mapping_result(result.text, result.edits, context, sink);
}

extern "C" void unicode_layout_apply_fullwidth_mapping(u16 const* text, size_t length_in_code_units, void* context, UnicodeLayoutTextMappingSink sink)
{
    auto source = Utf16String::from_utf16(utf16_view_from_ffi(text, length_in_code_units));
    auto fullwidth = source.to_fullwidth();
    if (fullwidth.length_in_code_units() == source.length_in_code_units()) {
        deliver_mapping_result(fullwidth, Vector<Unicode::FullwidthMappingEdit> {}, context, sink);
        return;
    }

    // Full-width conversion can combine code points within a grapheme, such as a half-width katakana
    // base and voiced sound mark. Transforming complete graphemes preserves that context while
    // identifying each length-changing source span.
    auto result = Unicode::apply_fullwidth_mapping(source);
    deliver_mapping_result(result.text, result.edits, context, sink);
}

extern "C" bool unicode_layout_may_require_bidi_processing(u16 const* text, size_t length_in_code_units)
{
    return Unicode::may_require_bidi_processing(utf16_view_from_ffi(text, length_in_code_units));
}
