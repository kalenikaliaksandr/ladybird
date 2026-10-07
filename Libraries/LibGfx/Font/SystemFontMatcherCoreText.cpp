/*
 * Copyright 2006 The Android Open Source Project
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/ports/SkFontMgr_mac_ct.cpp, skia/src/ports/SkTypeface_mac_ct.cpp and skia/src/utils/mac/SkCTFont.cpp

#include <AK/Array.h>
#include <AK/ByteString.h>
#include <AK/Noncopyable.h>
#include <AK/StdLibExtras.h>
#include <LibCore/MappedFile.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/SystemFontMatcher.h>

#include <CoreText/CoreText.h>
#include <dlfcn.h>
#include <harfbuzz/hb-ot.h>
#include <harfbuzz/hb.h>
#include <math.h>
#include <sys/param.h>

namespace Gfx::SystemFontMatcher {

namespace {

// Releases a CoreFoundation object at the end of its scope.
template<typename T>
class Retained {
    AK_MAKE_NONCOPYABLE(Retained);

public:
    explicit Retained(T object = nullptr)
        : m_object(object)
    {
    }

    Retained(Retained&& other)
        : m_object(exchange(other.m_object, nullptr))
    {
    }

    Retained& operator=(Retained&& other)
    {
        if (this != &other) {
            if (m_object)
                CFRelease(m_object);
            m_object = exchange(other.m_object, nullptr);
        }
        return *this;
    }

    ~Retained()
    {
        if (m_object)
            CFRelease(m_object);
    }

    T get() const { return m_object; }

private:
    T m_object { nullptr };
};

}

// The weights of CoreText, from -1 to 1, at the CSS weights 0, 100, ..., 1000. The AppKit constants give them for the
// native fonts.
static Array<CGFloat, 11> const& core_text_weights()
{
    static Array<CGFloat, 11> const weights = [] {
        // Use these if the constants are not available.
        Array<CGFloat, 11> default_weights { -1.00, -0.80, -0.60, -0.40, 0.00, 0.23, 0.30, 0.40, 0.56, 0.62, 1.00 };
        static constexpr Array weight_names {
            "NSFontWeightUltraLight",
            "NSFontWeightThin",
            "NSFontWeightLight",
            "NSFontWeightRegular",
            "NSFontWeightMedium",
            "NSFontWeightSemibold",
            "NSFontWeightBold",
            "NSFontWeightHeavy",
            "NSFontWeightBlack",
        };
        Array<CGFloat, 11> weights;
        weights[0] = -1;
        for (size_t index = 0; index < weight_names.size(); ++index) {
            auto* value = dlsym(RTLD_DEFAULT, weight_names[index]);
            if (!value)
                return default_weights;
            weights[index + 1] = *static_cast<CGFloat const*>(value);
        }
        weights[10] = 1;
        return weights;
    }();
    return weights;
}

static CGFloat core_text_weight(int css_weight)
{
    auto const& weights = core_text_weights();
    if (css_weight < 0)
        return weights[0];
    for (int index = 0; index < 10; ++index) {
        int next_css_weight = (index + 1) * 100;
        if (css_weight < next_css_weight)
            return weights[index] + ((css_weight - index * 100) * (weights[index + 1] - weights[index])) / (next_css_weight - index * 100);
    }
    return weights[10];
}

// The widths of CoreText, from -0.5 to 0.5, for the CSS width classes 0 to 10.
static CGFloat core_text_width(int css_width)
{
    if (css_width < 0)
        return -0.5;
    if (css_width >= 10)
        return 0.5;
    return -0.5 + ((css_width - 0) * (0.5 - -0.5)) / (10 - 0);
}

static Retained<CFStringRef> make_string(StringView string)
{
    return Retained<CFStringRef> { CFStringCreateWithBytes(kCFAllocatorDefault, reinterpret_cast<UInt8 const*>(string.characters_without_null_termination()), static_cast<CFIndex>(string.length()), kCFStringEncodingUTF8, false) };
}

static Optional<String> string_from(CFStringRef string)
{
    if (!string)
        return {};
    auto length = CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), kCFStringEncodingUTF8) + 1;
    Vector<char> buffer;
    buffer.resize(length);
    if (!CFStringGetCString(string, buffer.data(), length, kCFStringEncodingUTF8))
        return {};
    auto result = String::from_utf8(StringView { buffer.data(), strlen(buffer.data()) });
    if (result.is_error())
        return {};
    return result.release_value();
}

static void add_number(CFMutableDictionaryRef dictionary, CFStringRef key, CGFloat value)
{
    Retained<CFNumberRef> number { CFNumberCreate(kCFAllocatorDefault, kCFNumberCGFloatType, &value) };
    if (number.get())
        CFDictionaryAddValue(dictionary, key, number.get());
}

static Retained<CTFontDescriptorRef> create_descriptor(CFStringRef family, u16 weight, u16 width, u8 slope)
{
    Retained<CFMutableDictionaryRef> attributes { CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks) };
    Retained<CFMutableDictionaryRef> traits { CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks) };
    if (!attributes.get() || !traits.get())
        return Retained<CTFontDescriptorRef> {};

    // A font style keeps the weight in 0..1000 and the width in 1..9. Setting both the symbolic traits and these
    // traits can give strange results.
    add_number(traits.get(), kCTFontWeightTrait, core_text_weight(min<u16>(weight, 1000)));
    add_number(traits.get(), kCTFontWidthTrait, core_text_width(clamp<u16>(width, 1, 9)));
    // An italic or oblique slant of 0.07 gives better matches.
    add_number(traits.get(), kCTFontSlantTrait, slope == 1 || slope == 2 ? 0.07 : 0);
    CFDictionaryAddValue(attributes.get(), kCTFontTraitsAttribute, traits.get());

    if (family)
        CFDictionaryAddValue(attributes.get(), kCTFontFamilyNameAttribute, family);

    return Retained<CTFontDescriptorRef> { CTFontDescriptorCreateWithAttributes(attributes.get()) };
}

static Retained<CFSetRef> family_name_required()
{
    CFStringRef values[] = { kCTFontFamilyNameAttribute };
    return Retained<CFSetRef> { CFSetCreate(kCFAllocatorDefault, reinterpret_cast<void const**>(values), array_size(values), &kCFTypeSetCallBacks) };
}

static bool has_character(CTFontRef font, u32 code_point)
{
    UniChar utf16[2] = {};
    CGGlyph glyphs[2] = {};
    CFIndex length = 1;
    if (code_point < 0x10000) {
        utf16[0] = static_cast<UniChar>(code_point);
    } else {
        utf16[0] = static_cast<UniChar>(0xD800 + ((code_point - 0x10000) >> 10));
        utf16[1] = static_cast<UniChar>(0xDC00 + ((code_point - 0x10000) & 0x3FF));
        length = 2;
    }
    return CTFontGetGlyphsForCharacters(font, utf16, glyphs, length);
}

static Optional<String> name_of_face(hb_face_t* face, hb_ot_name_id_t name_id)
{
    auto length = hb_ot_name_get_utf8(face, name_id, HB_LANGUAGE_INVALID, nullptr, nullptr);
    if (length == 0)
        return {};
    Vector<char> name;
    name.resize(length + 1);
    auto capacity = length + 1;
    hb_ot_name_get_utf8(face, name_id, HB_LANGUAGE_INVALID, &capacity, name.data());
    auto result = String::from_utf8(StringView { name.data(), capacity });
    if (result.is_error())
        return {};
    return result.release_value();
}

// Whether the coordinates of a named instance of the face are the variation of a CoreText font. CoreText leaves the axes
// at their default values out of the variation.
static bool instance_has_variation(hb_face_t* face, unsigned instance_index, ReadonlySpan<hb_ot_var_axis_info_t> axes, CFDictionaryRef variation)
{
    Vector<float> coordinates;
    coordinates.resize(axes.size());
    auto coordinate_count = static_cast<unsigned>(axes.size());
    if (hb_ot_var_named_instance_get_design_coords(face, instance_index, &coordinate_count, coordinates.data()) != axes.size())
        return false;
    for (size_t index = 0; index < axes.size(); ++index) {
        double expected = axes[index].default_value;
        i64 tag = axes[index].tag;
        Retained<CFNumberRef> key { CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt64Type, &tag) };
        if (auto value = static_cast<CFNumberRef>(CFDictionaryGetValue(variation, key.get())))
            CFNumberGetValue(value, kCFNumberDoubleType, &expected);
        // CoreText rounds some coordinates that the font gives in 16.16 fixed point.
        if (fabs(coordinates[index] - expected) > 0.001)
            return false;
    }
    return true;
}

// The face index in a font file for a font that CoreText gives. This is the face with the same PostScript name, or
// else a face of the same family with a named instance at the variation of the font, or that face by itself if no
// named instance is at the variation. The upper 16 bits of the index select the named instance.
static Optional<u32> face_index_of(ReadonlyBytes bytes, CTFontRef font, String const& postscript_name)
{
    Retained<CFDictionaryRef> variation { CTFontCopyVariation(font) };
    Retained<CFStringRef> core_text_family { CTFontCopyFamilyName(font) };
    auto family = string_from(core_text_family.get());

    auto* blob = hb_blob_create(reinterpret_cast<char const*>(bytes.data()), bytes.size(), HB_MEMORY_MODE_READONLY, nullptr, nullptr);
    auto face_count = hb_face_count(blob);
    Optional<u32> match;
    Optional<u32> instance_match;
    Optional<u32> family_match;
    for (u32 index = 0; index < face_count && !match.has_value(); ++index) {
        auto* face = hb_face_create(blob, index);
        if (name_of_face(face, HB_OT_NAME_ID_POSTSCRIPT_NAME) == postscript_name) {
            match = index;
        } else if (variation.get() && CFDictionaryGetCount(variation.get()) > 0 && family.has_value() && !instance_match.has_value()) {
            auto face_family = name_of_face(face, HB_OT_NAME_ID_TYPOGRAPHIC_FAMILY);
            if (!face_family.has_value())
                face_family = name_of_face(face, HB_OT_NAME_ID_FONT_FAMILY);
            if (face_family == family) {
                if (!family_match.has_value())
                    family_match = index;
                unsigned axis_count = hb_ot_var_get_axis_count(face);
                Vector<hb_ot_var_axis_info_t> axes;
                axes.resize(axis_count);
                hb_ot_var_get_axis_infos(face, 0, &axis_count, axes.data());
                auto instance_count = hb_ot_var_get_named_instance_count(face);
                for (unsigned instance = 0; instance < instance_count; ++instance) {
                    if (instance_has_variation(face, instance, axes, variation.get())) {
                        instance_match = index | ((instance + 1) << 16);
                        break;
                    }
                }
            }
        }
        hb_face_destroy(face);
    }
    hb_blob_destroy(blob);
    if (match.has_value())
        return match;
    if (instance_match.has_value())
        return instance_match;
    return family_match;
}

// The file of a font that CoreText gives, if Typeface loads its face, or else the PostScript name to open it by.
static Optional<SystemFontMatch> match_of(CTFontRef font)
{
    Retained<CFStringRef> core_text_postscript_name { CTFontCopyPostScriptName(font) };
    auto postscript_name = string_from(core_text_postscript_name.get());
    if (!postscript_name.has_value())
        return {};

    Retained<CFURLRef> url { static_cast<CFURLRef>(CTFontCopyAttribute(font, kCTFontURLAttribute)) };
    char path[MAXPATHLEN];
    if (url.get() && CFURLGetFileSystemRepresentation(url.get(), true, reinterpret_cast<UInt8*>(path), sizeof(path))) {
        if (auto mapped_file = Core::MappedFile::map({ path, strlen(path) }); !mapped_file.is_error()) {
            if (auto ttc_index = face_index_of(mapped_file.value()->bytes(), font, *postscript_name); ttc_index.has_value()) {
                if (auto file = loadable_file({ path, strlen(path) }, *ttc_index); file.has_value())
                    return SystemFontMatch { file.release_value() };
            }
        }
    }

    return SystemFontMatch { PlatformFontName { postscript_name.release_value() } };
}

static Retained<CTFontRef> match_font_for_family(StringView family, u16 weight, u16 width, u8 slope)
{
    auto family_name = make_string(family);
    if (!family_name.get())
        return Retained<CTFontRef> {};
    auto requested_descriptor = create_descriptor(family_name.get(), weight, width, slope);
    if (!requested_descriptor.get())
        return Retained<CTFontRef> {};
    auto required_attributes = family_name_required();
    Retained<CTFontDescriptorRef> resolved_descriptor { CTFontDescriptorCreateMatchingFontDescriptor(requested_descriptor.get(), required_attributes.get()) };
    if (!resolved_descriptor.get())
        return Retained<CTFontRef> {};
    return Retained<CTFontRef> { CTFontCreateWithFontDescriptor(resolved_descriptor.get(), 0, nullptr) };
}

Optional<SystemFontMatch> match_family_style(StringView family, u16 weight, u16 width, u8 slope)
{
    auto font = match_font_for_family(family, weight, width, slope);
    if (!font.get())
        return {};
    return match_of(font.get());
}

Optional<SystemFontMatch> match_code_point(u32 code_point, u16 weight, u16 width, u8 slope, bool prefer_color_emoji)
{
    auto descriptor = create_descriptor(nullptr, weight, width, slope);
    if (!descriptor.get())
        return {};
    Retained<CTFontRef> family_font { CTFontCreateWithFontDescriptor(descriptor.get(), 0, nullptr) };
    if (!family_font.get())
        return {};

    // A code point that is a surrogate or that is past the last code point gives no string, and no font has it.
    Retained<CFStringRef> string { CFStringCreateWithBytes(kCFAllocatorDefault, reinterpret_cast<UInt8 const*>(&code_point), sizeof(code_point), kCFStringEncodingUTF32LE, false) };
    if (!string.get())
        return {};
    auto range = CFRangeMake(0, CFStringGetLength(string.get()));

    // The "und-Zsye" language tag steers the match towards a color emoji font.
    auto locale = make_string(prefer_color_emoji ? "und-Zsye"sv : ""sv);
    Retained<CTFontRef> fallback_font { CTFontCreateForStringWithLanguage(family_font.get(), string.get(), range, locale.get()) };
    if (!fallback_font.get() || !has_character(fallback_font.get(), code_point))
        return {};

    // The family of the fallback font can have a face that matches the style better. CoreText has no fallback for a
    // collection of fonts, so look the family up by its name. A name that starts with '.' is a hidden system font,
    // which this cannot find.
    Retained<CFStringRef> fallback_family { CTFontCopyFamilyName(fallback_font.get()) };
    if (fallback_family.get() && CFStringGetLength(fallback_family.get()) > 0 && CFStringGetCharacterAtIndex(fallback_family.get(), 0) != '.') {
        auto styled_descriptor = create_descriptor(fallback_family.get(), weight, width, slope);
        if (styled_descriptor.get()) {
            Retained<CTFontRef> styled_font { CTFontCreateWithFontDescriptor(styled_descriptor.get(), 0, nullptr) };
            if (styled_font.get() && has_character(styled_font.get(), code_point))
                fallback_font = move(styled_font);
        }
    }

    return match_of(fallback_font.get());
}

Optional<FlyString> resolve_generic_family(StringView family, u16 weight, u8 slope)
{
    auto font = match_font_for_family(family, weight, FontWidth::Normal, slope);
    if (!font.get())
        return {};
    Retained<CFStringRef> family_name { CTFontCopyFamilyName(font.get()) };
    auto name = string_from(family_name.get());
    if (!name.has_value())
        return {};
    return FlyString { name.release_value() };
}

}
