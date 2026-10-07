/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/ports/SkFontHost_FreeType.cpp (SkFontScanner_FreeType) and skia/src/core/SkFontDescriptor.cpp.

#include <AK/StringBuilder.h>
#include <LibGfx/Font/FaceDescription.h>
#include <LibGfx/Font/FontTable.h>
#include <LibTextCodec/Decoder.h>

#include <harfbuzz/hb.h>
#include <math.h>

namespace Gfx {

namespace {

// https://learn.microsoft.com/en-us/typography/opentype/spec/name#platform-ids
enum class PlatformID : u16 {
    Unicode = 0,
    Macintosh = 1,
    ISO = 2,
    Windows = 3,
};

constexpr u16 macintosh_roman_encoding = 0;
constexpr u16 macintosh_english_language = 0;
constexpr u16 windows_symbol_encoding = 0;
constexpr u16 windows_unicode_bmp_encoding = 1;
constexpr u16 windows_unicode_full_encoding = 10;

// https://learn.microsoft.com/en-us/typography/opentype/spec/name#name-ids
constexpr u16 family_name_id = 1;
constexpr u16 typographic_family_name_id = 16;
constexpr u16 wws_family_name_id = 21;

constexpr FourCC wght_tag { "wght" };
constexpr FourCC wdth_tag { "wdth" };
constexpr FourCC slnt_tag { "slnt" };
constexpr FourCC ital_tag { "ital" };

constexpr int bold_weight = 700;
constexpr int normal_weight = 400;
constexpr int normal_width = 5;
constexpr u8 upright_slope = 0;
constexpr u8 italic_slope = 1;
constexpr u8 oblique_slope = 2;

struct NameRecord {
    u16 platform_id { 0 };
    u16 encoding_id { 0 };
    u16 language_id { 0 };
    u16 name_id { 0 };
    ReadonlyBytes string;
};

// The records of the name table whose strings are inside the table. A name table that is too small for its own
// record list has no records.
Vector<NameRecord> read_name_records(FontTable const& table)
{
    if (!table.contains(0, 6))
        return {};
    auto format = table.u16_at(0);
    auto record_count = table.u16_at(2);
    auto storage_offset = table.u16_at(4);

    size_t storage_start = 6 + 12 * static_cast<size_t>(record_count);
    if (storage_start > table.bytes().size())
        return {};

    Vector<u16> language_tag_lengths;
    if (format == 1) {
        if (!table.contains(storage_start, 2))
            return {};
        auto language_tag_count = table.u16_at(storage_start);
        auto language_tags_start = storage_start + 2;
        storage_start += 2 + 4 * static_cast<size_t>(language_tag_count);
        if (storage_start > table.bytes().size())
            return {};
        for (size_t index = 0; index < language_tag_count; ++index) {
            auto length = table.u16_at(language_tags_start + 4 * index);
            size_t offset = storage_offset + static_cast<size_t>(table.u16_at(language_tags_start + 4 * index + 2));
            if (offset < storage_start || !table.contains(offset, length))
                length = 0;
            language_tag_lengths.append(length);
        }
    }

    Vector<NameRecord> records;
    for (size_t index = 0; index < record_count; ++index) {
        auto record_start = 6 + 12 * index;
        auto length = table.u16_at(record_start + 8);
        if (length == 0)
            continue;
        size_t offset = storage_offset + static_cast<size_t>(table.u16_at(record_start + 10));
        if (offset < storage_start || !table.contains(offset, length))
            continue;
        auto language_id = table.u16_at(record_start + 4);
        if (format == 1 && language_id >= 0x8000) {
            auto language_tag_index = language_id - 0x8000u;
            if (language_tag_index >= language_tag_lengths.size() || language_tag_lengths[language_tag_index] == 0)
                continue;
        }
        records.append({
            .platform_id = table.u16_at(record_start),
            .encoding_id = table.u16_at(record_start + 2),
            .language_id = language_id,
            .name_id = table.u16_at(record_start + 6),
            .string = table.bytes().slice(offset, length),
        });
    }
    return records;
}

// A name ends at its first U+0000, and control characters become '?'.
void append_name_code_point(StringBuilder& builder, u32 code_point)
{
    builder.append_code_point(code_point < 0x20 ? '?' : code_point);
}

String decode_utf16_name(ReadonlyBytes string)
{
    StringBuilder builder;
    for (size_t index = 0; index + 1 < string.size(); index += 2) {
        u32 code_unit = (string[index] << 8) | string[index + 1];
        if (code_unit == 0)
            break;
        if (code_unit >= 0xD800 && code_unit < 0xDC00 && index + 3 < string.size()) {
            u32 low_surrogate = (string[index + 2] << 8) | string[index + 3];
            if (low_surrogate >= 0xDC00 && low_surrogate < 0xE000) {
                append_name_code_point(builder, 0x10000 + ((code_unit - 0xD800) << 10) + (low_surrogate - 0xDC00));
                index += 2;
                continue;
            }
        }
        if (code_unit >= 0xD800 && code_unit < 0xE000)
            code_unit = 0xFFFD;
        append_name_code_point(builder, code_unit);
    }
    return MUST(builder.to_string());
}

String decode_macintosh_name(ReadonlyBytes string, u16 encoding_id)
{
    for (size_t index = 0; index < string.size(); ++index) {
        if (string[index] == 0) {
            string = string.trim(index);
            break;
        }
    }
    StringBuilder builder;
    if (encoding_id == macintosh_roman_encoding) {
        auto decoder = TextCodec::decoder_for_exact_name("macintosh"sv);
        VERIFY(decoder.has_value());
        MUST(decoder->process_code_points(StringView { string }, [&](u32 code_point) -> ErrorOr<void> {
            append_name_code_point(builder, code_point);
            return {};
        }));
    } else {
        // Without a decoder for this script, keep the ASCII characters only.
        for (auto byte : string)
            append_name_code_point(builder, byte > 0x7F ? '?' : byte);
    }
    return MUST(builder.to_string());
}

// Chooses the record for a name the same way as FreeType's tt_face_get_name(): an English Windows record, then an
// English or Roman Macintosh record, then a Unicode record. A Windows record in another language is used only if the
// font has no Macintosh record.
Optional<String> find_name(ReadonlySpan<NameRecord> records, u16 name_id)
{
    Optional<size_t> found_unicode;
    Optional<size_t> found_macintosh_roman;
    Optional<size_t> found_macintosh_english;
    Optional<size_t> found_windows;
    bool windows_is_english = false;

    for (size_t index = 0; index < records.size(); ++index) {
        auto const& record = records[index];
        if (record.name_id != name_id)
            continue;
        switch (static_cast<PlatformID>(record.platform_id)) {
        case PlatformID::Unicode:
        case PlatformID::ISO:
            found_unicode = index;
            break;
        case PlatformID::Macintosh:
            if (record.language_id == macintosh_english_language)
                found_macintosh_english = index;
            else if (record.encoding_id == macintosh_roman_encoding)
                found_macintosh_roman = index;
            break;
        case PlatformID::Windows: {
            bool is_english = (record.language_id & 0x3FF) == 0x009;
            if (found_windows.has_value() && !is_english)
                break;
            if (record.encoding_id == windows_symbol_encoding || record.encoding_id == windows_unicode_bmp_encoding || record.encoding_id == windows_unicode_full_encoding) {
                windows_is_english = is_english;
                found_windows = index;
            }
            break;
        }
        }
    }

    auto found_macintosh = found_macintosh_english.has_value() ? found_macintosh_english : found_macintosh_roman;
    if (found_windows.has_value() && !(found_macintosh.has_value() && !windows_is_english))
        return decode_utf16_name(records[*found_windows].string);
    if (found_macintosh.has_value())
        return decode_macintosh_name(records[*found_macintosh].string, records[*found_macintosh].encoding_id);
    if (found_unicode.has_value())
        return decode_utf16_name(records[*found_unicode].string);
    return {};
}

// https://learn.microsoft.com/en-us/typography/opentype/spec/os2
struct OS2Fields {
    u16 weight_class { 0 };
    u16 width_class { 0 };
    u16 selection { 0 };
};

// FreeType reads the fields that the version of the table has, and counts the table as missing if the font data ends
// before them.
Optional<OS2Fields> read_os2(FontTable const& os2_table)
{
    if (os2_table.is_empty())
        return {};
    auto const& table = os2_table.with_following_data();
    if (!table.contains(0, 78))
        return {};
    auto version = table.u16_at(0);
    if (version == 0xFFFF)
        return {};
    if (version >= 1 && !table.contains(78, 8))
        return {};
    if (version >= 2 && !table.contains(86, 10))
        return {};
    if (version >= 5 && !table.contains(96, 4))
        return {};
    return OS2Fields {
        .weight_class = table.u16_at(4),
        .width_class = table.u16_at(6),
        .selection = table.u16_at(62),
    };
}

constexpr u16 italic_selection_bit = 1 << 0;
constexpr u16 bold_selection_bit = 1 << 5;
constexpr u16 wws_selection_bit = 1 << 8;
constexpr u16 oblique_selection_bit = 1 << 9;

float fixed_to_float(i32 value)
{
    return static_cast<float>(value) * 1.52587890625e-5f;
}

i32 float_to_fixed(float value)
{
    value *= 65536.0f;
    if (isnan(value))
        return NumericLimits<i32>::max();
    // The largest and the smallest i32 values that a float holds exactly.
    value = min(value, 2147483520.0f);
    value = max(value, -2147483520.0f);
    return static_cast<i32>(value);
}

int fixed_round_to_int(i32 value)
{
    return (value + 0x8000) >> 16;
}

// Maps a value of the wdth axis, in percent of the normal width, to the scale of usWidthClass.
int width_for_width_axis_value(float width)
{
    static constexpr Array<float, 9> axis_values { 50, 62.5f, 75, 87.5f, 100, 112.5f, 125, 150, 200 };
    size_t right = 0;
    while (right < axis_values.size() && axis_values[right] < width)
        ++right;
    float width_class;
    if (right == axis_values.size()) {
        width_class = axis_values.size();
    } else if (right == 0) {
        width_class = 1;
    } else {
        float fraction = (width - axis_values[right - 1]) / (axis_values[right] - axis_values[right - 1]);
        float left_class = right;
        float right_class = right + 1;
        width_class = left_class + (right_class - left_class) * fraction;
    }
    return static_cast<int>(floorf(width_class + 0.5f));
}

FaceStyle pinned_style(int weight, int width, int slope)
{
    return {
        .weight = static_cast<u16>(clamp(weight, 0, 1000)),
        .width = static_cast<u16>(clamp(width, 1, 9)),
        .slope = static_cast<u8>(clamp(slope, 0, 2)),
    };
}

// Keeps a value inside a range. A NaN becomes the minimum.
float pin_to_range(float value, float minimum, float maximum)
{
    auto pinned = maximum < value ? maximum : value;
    return minimum < pinned ? pinned : minimum;
}

bool is_usable_weight_axis(float minimum, float maximum)
{
    auto range = maximum - minimum;
    return range > 5 && range <= 1000 && maximum <= 1000;
}

bool is_usable_width_axis(float minimum, float maximum)
{
    auto range = maximum - minimum;
    return range > 0 && range <= 500 && maximum <= 500;
}

}

FaceDescription FaceDescription::read(hb_face_t* face)
{
    FaceDescription description;

    FontTable os2_table { face, FourCC { "OS/2" } };
    auto os2 = read_os2(os2_table);

    {
        FontTable name_table { face, FourCC { "name" } };
        auto records = read_name_records(name_table);
        Optional<String> family;
        if (os2.has_value() && (os2->selection & wws_selection_bit)) {
            family = find_name(records, typographic_family_name_id);
            if (!family.has_value())
                family = find_name(records, family_name_id);
        } else {
            family = find_name(records, wws_family_name_id);
            if (!family.has_value())
                family = find_name(records, typographic_family_name_id);
            if (!family.has_value())
                family = find_name(records, family_name_id);
        }
        if (family.has_value())
            description.m_family = FlyString { family.release_value() };
    }

    auto has_table = [&](char const* tag) {
        return !FontTable { face, FourCC { tag } }.is_empty();
    };
    bool has_horizontal_header = [&] {
        FontTable table { face, FourCC { "hhea" } };
        return !table.is_empty() && table.with_following_data().contains(0, 36);
    }();
    // Faces with color bitmaps, and faces without horizontal metrics, count as faces without outlines.
    bool has_outlines = (has_table("glyf") || has_table("CFF ") || has_table("CFF2"))
        && !has_table("CBLC") && !has_table("CBDT") && has_horizontal_header;

    bool is_bold = false;
    bool is_italic = false;
    if (has_outlines && os2.has_value()) {
        is_italic = (os2->selection & (oblique_selection_bit | italic_selection_bit)) != 0;
        is_bold = (os2->selection & bold_selection_bit) != 0;
    } else {
        // https://learn.microsoft.com/en-us/typography/opentype/spec/head
        auto read_mac_style = [&](char const* tag) -> Optional<u16> {
            FontTable table { face, FourCC { tag } };
            if (table.is_empty() || !table.with_following_data().contains(0, 54))
                return {};
            return table.with_following_data().u16_at(44);
        };
        auto mac_style = read_mac_style("head");
        if (!mac_style.has_value())
            mac_style = read_mac_style("bhed");
        is_bold = (mac_style.value_or(0) & 1) != 0;
        is_italic = (mac_style.value_or(0) & 2) != 0;
    }

    int weight = is_bold ? bold_weight : normal_weight;
    int width = normal_width;
    int slope = is_italic ? italic_slope : upright_slope;
    if (os2.has_value()) {
        weight = os2->weight_class;
        width = os2->width_class;
        if (os2->selection & oblique_selection_bit)
            slope = oblique_slope;
    }

    // https://learn.microsoft.com/en-us/typography/opentype/spec/fvar
    Vector<i32> face_coordinates;
    {
        FontTable fvar_table { face, FourCC { "fvar" } };
        if (fvar_table.contains(0, 16)) {
            auto version = fvar_table.u32_at(0);
            size_t axes_offset = fvar_table.u16_at(4);
            size_t axis_count = fvar_table.u16_at(8);
            size_t axis_size = fvar_table.u16_at(10);
            size_t instance_count = fvar_table.u16_at(12);
            size_t instance_size = fvar_table.u16_at(14);
            bool is_valid = fvar_table.bytes().size() >= 20
                && version == 0x00010000
                && axis_size == 20
                && axis_count != 0
                && axis_count <= 0x3FFE
                && (instance_size == 4 + 4 * axis_count || instance_size == 6 + 4 * axis_count)
                && instance_count <= 0x7EFF
                && axes_offset + axis_size * axis_count + instance_size * instance_count <= fvar_table.bytes().size();
            if (is_valid) {
                // The upper 16 bits of the face index select a named instance, from 1, and the face starts from its
                // coordinates. A face does not have to list its default instance, and FreeType then adds it after the
                // others, so an index after the last instance selects the default coordinates.
                auto instance_index = hb_face_get_index(face) >> 16;
                Optional<size_t> instance_start;
                if (instance_index > 0 && instance_index <= instance_count)
                    instance_start = axes_offset + axis_size * axis_count + instance_size * (instance_index - 1);

                for (size_t index = 0; index < axis_count; ++index) {
                    auto axis_start = axes_offset + axis_size * index;
                    auto minimum = fvar_table.i32_at(axis_start + 4);
                    auto default_value = fvar_table.i32_at(axis_start + 8);
                    auto maximum = fvar_table.i32_at(axis_start + 12);
                    // An axis whose default is outside its range cannot vary.
                    if (minimum > default_value || default_value > maximum) {
                        minimum = default_value;
                        maximum = default_value;
                    }
                    auto face_value = instance_start.has_value() ? fvar_table.i32_at(*instance_start + 4 + 4 * index) : default_value;
                    description.m_axes.append({
                        .tag = fvar_table.u32_at(axis_start),
                        .minimum = fixed_to_float(minimum),
                        .face_value = pin_to_range(fixed_to_float(face_value), fixed_to_float(minimum), fixed_to_float(maximum)),
                        .maximum = fixed_to_float(maximum),
                    });
                    face_coordinates.append(face_value);
                }
            }
        }
    }

    // The coordinates of a variable face override the style that its OS/2 table declares.
    Optional<size_t> weight_axis;
    Optional<size_t> width_axis;
    Optional<size_t> slant_axis;
    for (size_t index = 0; index < description.m_axes.size(); ++index) {
        auto const& axis = description.m_axes[index];
        if (axis.tag == wght_tag.to_u32() && is_usable_weight_axis(axis.minimum, axis.maximum))
            weight_axis = index;
        if (axis.tag == wdth_tag.to_u32() && is_usable_width_axis(axis.minimum, axis.maximum))
            width_axis = index;
        if (axis.tag == slnt_tag.to_u32())
            slant_axis = index;
    }
    if (weight_axis.has_value())
        weight = fixed_round_to_int(face_coordinates[*weight_axis]);
    if (width_axis.has_value())
        width = width_for_width_axis_value(fixed_to_float(face_coordinates[*width_axis]));
    if (slant_axis.has_value() && fixed_to_float(face_coordinates[*slant_axis]) < 0)
        slope = oblique_slope;

    description.m_style = pinned_style(weight, width, slope);
    // The face gets its style from its axes, the same way as a font with variations.
    description.m_style = description.style_for_variations({});
    return description;
}

FaceStyle FaceDescription::style_for_variations(ReadonlySpan<FontVariationAxis> variations) const
{
    int weight = m_style.weight;
    int width = m_style.width;
    int slope = m_style.slope;
    Optional<i32> slant_value;
    Optional<i32> italic_value;

    for (auto const& axis : m_axes) {
        // The value in the face, replaced by the last variation for this axis, kept inside the range of the axis.
        auto value = axis.face_value;
        for (size_t index = variations.size(); index-- > 0;) {
            if (variations[index].tag.to_u32() == axis.tag) {
                value = pin_to_range(variations[index].value, axis.minimum, axis.maximum);
                break;
            }
        }
        auto fixed_value = float_to_fixed(value);

        if (axis.tag == wght_tag.to_u32() && is_usable_weight_axis(axis.minimum, axis.maximum))
            weight = fixed_round_to_int(fixed_value);
        if (axis.tag == wdth_tag.to_u32() && is_usable_width_axis(axis.minimum, axis.maximum))
            width = width_for_width_axis_value(fixed_to_float(fixed_value));
        if (axis.tag == slnt_tag.to_u32())
            slant_value = fixed_value;
        if (axis.tag == ital_tag.to_u32())
            italic_value = fixed_value;
    }

    if (italic_value.has_value() && *italic_value != 0) {
        slope = italic_slope;
    } else if (slant_value.has_value() && *slant_value != 0) {
        slope = oblique_slope;
    } else if (italic_value.has_value() && slant_value.has_value()) {
        slope = upright_slope;
    } else if (italic_value.has_value() && !slant_value.has_value()) {
        if (slope == italic_slope)
            slope = upright_slope;
    } else if (!italic_value.has_value() && slant_value.has_value()) {
        if (slope == oblique_slope)
            slope = upright_slope;
    }

    return pinned_style(weight, width, slope);
}

}
