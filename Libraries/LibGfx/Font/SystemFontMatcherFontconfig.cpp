/*
 * Copyright 2014 Google Inc.
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/ports/SkFontMgr_fontconfig.cpp

#include <AK/ByteString.h>
#include <AK/Mutex.h>
#include <AK/NeverDestroyed.h>
#include <AK/Vector.h>
#include <LibCore/System.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/GlobalFontConfig.h>
#include <LibGfx/Font/SystemFontMatcher.h>

#include <fontconfig/fontconfig.h>
#include <unistd.h>

namespace Gfx::SystemFontMatcher {

namespace {

template<typename T, T* (*create)(), void (*destroy)(T*)>
class FontconfigObject {
    AK_MAKE_NONCOPYABLE(FontconfigObject);

public:
    FontconfigObject()
        : m_object(create())
    {
        VERIFY(m_object);
    }

    explicit FontconfigObject(T* object)
        : m_object(object)
    {
    }

    FontconfigObject(FontconfigObject&& other)
        : m_object(exchange(other.m_object, nullptr))
    {
    }

    ~FontconfigObject()
    {
        if (m_object)
            destroy(m_object);
    }

    T* get() const { return m_object; }
    T* leak() { return exchange(m_object, nullptr); }

private:
    T* m_object { nullptr };
};

using CharSet = FontconfigObject<FcCharSet, FcCharSetCreate, FcCharSetDestroy>;
using Config = FontconfigObject<FcConfig, FcConfigCreate, FcConfigDestroy>;
using FontSet = FontconfigObject<FcFontSet, FcFontSetCreate, FcFontSetDestroy>;
using LangSet = FontconfigObject<FcLangSet, FcLangSetCreate, FcLangSetDestroy>;
using ObjectSet = FontconfigObject<FcObjectSet, FcObjectSetCreate, FcObjectSetDestroy>;
using Pattern = FontconfigObject<FcPattern, FcPatternCreate, FcPatternDestroy>;

// The file of a matched font. Its face is loaded after the lock is released.
struct FontFileCandidate {
    ByteString path;
    u32 ttc_index { 0 };
};

}

static Mutex& fontconfig_mutex()
{
    static NeverDestroyed<Mutex> mutex;
    return *mutex;
}

static FcChar8 const* fontconfig_string(char const* string)
{
    return reinterpret_cast<FcChar8 const*>(string);
}

static int get_int(FcPattern* pattern, char const* object, int missing)
{
    int value = 0;
    if (FcPatternGetInteger(pattern, object, 0, &value) != FcResultMatch)
        return missing;
    return value;
}

static char const* get_string(FcPattern* pattern, char const* object)
{
    FcChar8* value = nullptr;
    if (FcPatternGetString(pattern, object, 0, &value) != FcResultMatch)
        return nullptr;
    return reinterpret_cast<char const*>(value);
}

enum class Binding {
    Weak,
    Strong,
    NoId,
};

// Fontconfig has no call that gives the binding of a value, so find it from its effect on matching. The binding has an
// effect only on values of FC_FAMILY and FC_POSTSCRIPT_NAME: a weak value is scored after FC_LANG, and a strong value
// before it.
static Binding binding_of(FcPattern* pattern, char const* object, int id)
{
    // Make a copy of the pattern with only the value 'pattern'['object'['id']] in it.
    ObjectSet requested_object_only { FcObjectSetBuild(object, nullptr) };
    Pattern minimal { FcPatternFilter(pattern, requested_object_only.get()) };
    FcBool has_id = true;
    for (int index = 0; has_id && index < id; ++index)
        has_id = FcPatternRemove(minimal.get(), object, 0);
    if (!has_id)
        return Binding::NoId;
    FcValue value;
    if (FcPatternGet(minimal.get(), object, 0, &value) != FcResultMatch)
        return Binding::NoId;
    while (has_id)
        has_id = FcPatternRemove(minimal.get(), object, 1);

    // Make a font set with two patterns:
    // 1. The same 'object' as minimal, and a lang object with only 'nomatchlang'.
    // 2. A different 'object' from minimal, and a lang object with only 'matchlang'.
    FontSet font_set;

    LangSet strong_lang_set;
    FcLangSetAdd(strong_lang_set.get(), fontconfig_string("nomatchlang"));
    Pattern strong { FcPatternDuplicate(minimal.get()) };
    FcPatternAddLangSet(strong.get(), FC_LANG, strong_lang_set.get());

    LangSet weak_lang_set;
    FcLangSetAdd(weak_lang_set.get(), fontconfig_string("matchlang"));
    Pattern weak;
    FcPatternAddString(weak.get(), object, fontconfig_string("nomatchstring"));
    FcPatternAddLangSet(weak.get(), FC_LANG, weak_lang_set.get());

    FcFontSetAdd(font_set.get(), strong.leak());
    FcFontSetAdd(font_set.get(), weak.leak());

    // Add 'matchlang' to the copy of the pattern. If the value is weak, the pattern with 'matchlang' matches. If the
    // value is strong, the pattern with 'nomatchlang' matches.
    FcPatternAddLangSet(minimal.get(), FC_LANG, weak_lang_set.get());

    // The match needs a configuration, but nothing in it is used.
    Config config;
    FcFontSet* font_sets[] = { font_set.get() };
    FcResult result;
    Pattern match { FcFontSetMatch(config.get(), font_sets, array_size(font_sets), minimal.get(), &result) };

    FcLangSet* match_lang_set = nullptr;
    if (!match.get() || FcPatternGetLangSet(match.get(), FC_LANG, 0, &match_lang_set) != FcResultMatch)
        return Binding::Strong;
    return FcLangSetHasLang(match_lang_set, fontconfig_string("matchlang")) == FcLangEqual ? Binding::Weak : Binding::Strong;
}

// Removes the weak values after the last strong value of FC_FAMILY or FC_POSTSCRIPT_NAME. If all values are weak, the
// pattern does not change.
static void remove_weak(FcPattern* pattern, char const* object)
{
    ObjectSet requested_object_only { FcObjectSetBuild(object, nullptr) };
    Pattern minimal { FcPatternFilter(pattern, requested_object_only.get()) };

    int last_strong_id = -1;
    int id_count = 0;
    for (int id = 0;; ++id) {
        auto binding = binding_of(minimal.get(), object, 0);
        if (binding == Binding::NoId) {
            id_count = id;
            break;
        }
        if (binding == Binding::Strong)
            last_strong_id = id;
        FcPatternRemove(minimal.get(), object, 0);
    }

    if (last_strong_id < 0)
        return;

    for (int id = last_strong_id + 1; id < id_count; ++id)
        FcPatternRemove(pattern, object, last_strong_id + 1);
}

struct MapRange {
    float old_value;
    float new_value;
};

static int map_range(float value, float old_min, float old_max, float new_min, float new_max)
{
    return static_cast<int>(new_min + ((value - old_min) * (new_max - new_min) / (old_max - old_min)));
}

template<size_t range_count>
static float map_ranges(float value, MapRange const (&ranges)[range_count])
{
    if (value < ranges[0].old_value)
        return ranges[0].new_value;

    for (size_t index = 0; index < range_count - 1; ++index) {
        if (value < ranges[index + 1].old_value)
            return map_range(value, ranges[index].old_value, ranges[index + 1].old_value, ranges[index].new_value, ranges[index + 1].new_value);
    }

    return ranges[range_count - 1].new_value;
}

static void add_style(FcPattern* pattern, u16 weight, u16 width, u8 slope)
{
    static constexpr MapRange weight_ranges[] = {
        { 100, FC_WEIGHT_THIN },
        { 200, FC_WEIGHT_EXTRALIGHT },
        { 300, FC_WEIGHT_LIGHT },
        { 350, FC_WEIGHT_DEMILIGHT },
        { 380, FC_WEIGHT_BOOK },
        { 400, FC_WEIGHT_REGULAR },
        { 500, FC_WEIGHT_MEDIUM },
        { 600, FC_WEIGHT_DEMIBOLD },
        { 700, FC_WEIGHT_BOLD },
        { 800, FC_WEIGHT_EXTRABOLD },
        { 900, FC_WEIGHT_BLACK },
        { 1000, FC_WEIGHT_EXTRABLACK },
    };
    static constexpr MapRange width_ranges[] = {
        { FontWidth::UltraCondensed, FC_WIDTH_ULTRACONDENSED },
        { FontWidth::ExtraCondensed, FC_WIDTH_EXTRACONDENSED },
        { FontWidth::Condensed, FC_WIDTH_CONDENSED },
        { FontWidth::SemiCondensed, FC_WIDTH_SEMICONDENSED },
        { FontWidth::Normal, FC_WIDTH_NORMAL },
        { FontWidth::SemiExpanded, FC_WIDTH_SEMIEXPANDED },
        { FontWidth::Expanded, FC_WIDTH_EXPANDED },
        { FontWidth::ExtraExpanded, FC_WIDTH_EXTRAEXPANDED },
        { FontWidth::UltraExpanded, FC_WIDTH_ULTRAEXPANDED },
    };

    // A font style keeps the weight in 0..1000 and the width in 1..9.
    auto fontconfig_weight = static_cast<int>(map_ranges(min<u16>(weight, 1000), weight_ranges));
    auto fontconfig_width = static_cast<int>(map_ranges(clamp<u16>(width, FontWidth::UltraCondensed, FontWidth::UltraExpanded), width_ranges));
    int fontconfig_slant = FC_SLANT_ROMAN;
    if (slope == 1)
        fontconfig_slant = FC_SLANT_ITALIC;
    else if (slope == 2)
        fontconfig_slant = FC_SLANT_OBLIQUE;

    FcPatternAddInteger(pattern, FC_WEIGHT, fontconfig_weight);
    FcPatternAddInteger(pattern, FC_WIDTH, fontconfig_width);
    FcPatternAddInteger(pattern, FC_SLANT, fontconfig_slant);
}

// Whether a string value of the object in the font is the same as one in the pattern, ignoring case.
static bool any_string_matching(FcPattern* font, FcPattern* pattern, char const* object)
{
    auto strings_of = [&](FcPattern* strings_pattern) {
        Vector<FcChar8*, 32> strings;
        // Use a limit on the number of values that is arbitrary but high.
        static constexpr int maximum_id = 65536;
        for (int id = 0; id < maximum_id; ++id) {
            FcChar8* string = nullptr;
            auto result = FcPatternGetString(strings_pattern, object, id, &string);
            if (result == FcResultNoId)
                break;
            if (result == FcResultMatch)
                strings.append(string);
        }
        return strings;
    };

    auto font_strings = strings_of(font);
    auto pattern_strings = strings_of(pattern);
    for (auto* font_string : font_strings) {
        for (auto* pattern_string : pattern_strings) {
            if (FcStrCmpIgnoreCase(font_string, pattern_string) == 0)
                return true;
        }
    }
    return false;
}

static bool contains_character(FcPattern* font, u32 code_point)
{
    for (int id = 0;; ++id) {
        FcCharSet* char_set = nullptr;
        auto result = FcPatternGetCharSet(font, FC_CHARSET, id, &char_set);
        if (result == FcResultNoId)
            break;
        if (result != FcResultMatch)
            continue;
        if (FcCharSetHasChar(char_set, code_point))
            return true;
    }
    return false;
}

static Optional<FontFileCandidate> file_candidate(FcPattern* font)
{
    auto const* file_name = get_string(font, FC_FILE);
    if (!file_name)
        return {};

    // The upper 16 bits of the index select a named instance of a variable face.
    auto ttc_index = static_cast<u32>(get_int(font, FC_INDEX, 0));

    // Fontconfig can give a path in its sysroot without the sysroot, and a path outside the sysroot as it is. Prefer
    // the path in the sysroot.
    if (auto const* sysroot = reinterpret_cast<char const*>(FcConfigGetSysRoot(GlobalFontConfig::the().get())); sysroot && *sysroot) {
        auto path = ByteString::formatted("{}{}", sysroot, file_name);
        if (!Core::System::access(path, R_OK).is_error())
            return FontFileCandidate { move(path), ttc_index };
    }
    return FontFileCandidate { file_name, ttc_index };
}

// The font for a family and a style, if it has the family.
static Pattern match_font_for_family(StringView family, u16 weight, u16 width, u8 slope)
{
    auto* config = GlobalFontConfig::the().get();

    Pattern pattern;
    auto family_name = family.to_byte_string();
    FcPatternAddString(pattern.get(), FC_FAMILY, fontconfig_string(family_name.characters()));
    add_style(pattern.get(), weight, width, slope);
    FcConfigSubstitute(config, pattern.get(), FcMatchPattern);
    FcDefaultSubstitute(pattern.get());

    // The font must have one of the families of the pattern up to its last strong family. Weak families before that
    // one, such as the preferred families of an alias, count too. Fontconfig adds weak default families after it, and
    // these do not count.
    Pattern strong_pattern { FcPatternDuplicate(pattern.get()) };
    remove_weak(strong_pattern.get(), FC_FAMILY);

    FcResult result;
    Pattern font { FcFontMatch(config, pattern.get(), &result) };
    if (!font.get() || !any_string_matching(font.get(), strong_pattern.get(), FC_FAMILY))
        return Pattern { nullptr };
    return font;
}

// The font for a code point and a style, if it has the code point.
static Pattern match_font_for_code_point(u32 code_point, u16 weight, u16 width, u8 slope, bool prefer_color_emoji)
{
    auto* config = GlobalFontConfig::the().get();

    Pattern pattern;
    add_style(pattern.get(), weight, width, slope);

    CharSet char_set;
    FcCharSetAddChar(char_set.get(), code_point);
    FcPatternAddCharSet(pattern.get(), FC_CHARSET, char_set.get());

    // The "und-Zsye" language tag steers the match towards a color emoji font. Without it, a text presentation font
    // is preferred for a code point that is also an emoji.
    if (prefer_color_emoji) {
        LangSet lang_set;
        FcLangSetAdd(lang_set.get(), fontconfig_string("und-Zsye"));
        FcPatternAddLangSet(pattern.get(), FC_LANG, lang_set.get());
    }

    FcConfigSubstitute(config, pattern.get(), FcMatchPattern);
    FcDefaultSubstitute(pattern.get());

    FcResult result;
    Pattern font { FcFontMatch(config, pattern.get(), &result) };
    if (!font.get() || !contains_character(font.get(), code_point))
        return Pattern { nullptr };
    return font;
}

static Optional<SystemFontMatch> loadable_match(Optional<FontFileCandidate> candidate)
{
    if (!candidate.has_value())
        return {};
    auto file = loadable_file(candidate->path, candidate->ttc_index);
    if (!file.has_value())
        return {};
    return SystemFontMatch { file.release_value() };
}

Optional<SystemFontMatch> match_family_style(StringView family, u16 weight, u16 width, u8 slope)
{
    Optional<FontFileCandidate> candidate;
    {
        MutexLocker locker(fontconfig_mutex());
        auto font = match_font_for_family(family, weight, width, slope);
        if (font.get())
            candidate = file_candidate(font.get());
    }
    return loadable_match(move(candidate));
}

Optional<SystemFontMatch> match_code_point(u32 code_point, u16 weight, u16 width, u8 slope, bool prefer_color_emoji)
{
    Optional<FontFileCandidate> candidate;
    {
        MutexLocker locker(fontconfig_mutex());
        auto font = match_font_for_code_point(code_point, weight, width, slope, prefer_color_emoji);
        if (font.get())
            candidate = file_candidate(font.get());
    }
    return loadable_match(move(candidate));
}

Optional<FlyString> resolve_generic_family(StringView family, u16 weight, u8 slope)
{
    ByteString family_name;
    {
        MutexLocker locker(fontconfig_mutex());
        auto font = match_font_for_family(family, weight, FontWidth::Normal, slope);
        if (!font.get())
            return {};
        // The family can have faces that load even if this face does not, so the file only has to be readable.
        auto candidate = file_candidate(font.get());
        if (!candidate.has_value() || Core::System::access(candidate->path, R_OK).is_error())
            return {};
        if (auto const* name = get_string(font.get(), FC_FAMILY))
            family_name = name;
    }
    auto resolved_family = FlyString::from_utf8(family_name.view());
    if (resolved_family.is_error())
        return {};
    return resolved_family.release_value();
}

}
