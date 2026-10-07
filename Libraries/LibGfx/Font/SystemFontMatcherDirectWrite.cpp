/*
 * Copyright 2014 Google Inc.
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/ports/SkFontMgr_win_dw.cpp and skia/src/ports/SkTypeface_win_dw.cpp

#include <AK/Vector.h>
#include <AK/Windows.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/SystemFontMatcher.h>

#include <dwrite.h>
#include <dwrite_2.h>
#include <dwrite_3.h>
// NB: WRL instead of WinRT, because WinRT reports errors with exceptions.
#include <wrl/client.h>

namespace Gfx::SystemFontMatcher {

using Microsoft::WRL::ComPtr;

namespace {

struct DirectWrite {
    ComPtr<IDWriteFactory> factory;
    ComPtr<IDWriteFontCollection> collection;
    // Windows 8.1 and later have a fallback for the fonts of the system.
    ComPtr<IDWriteFontFallback> fallback;
    // The locale of the user, ending with a null character.
    Vector<wchar_t> locale;
};

struct DirectWriteStyle {
    DWRITE_FONT_WEIGHT weight;
    DWRITE_FONT_STRETCH stretch;
    DWRITE_FONT_STYLE style;
};

// The text that the fallback of DirectWrite reads: one code point, with a locale and a number substitution.
class FallbackSource final : public IDWriteTextAnalysisSource {
public:
    FallbackSource(wchar_t const* text, UINT32 length, wchar_t const* locale, IDWriteNumberSubstitution* number_substitution)
        : m_text(text)
        , m_length(length)
        , m_locale(locale)
        , m_number_substitution(number_substitution)
    {
    }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID interface_id, void** object) override
    {
        if (interface_id == __uuidof(IUnknown) || interface_id == __uuidof(IDWriteTextAnalysisSource)) {
            *object = this;
            AddRef();
            return S_OK;
        }
        *object = nullptr;
        return E_NOINTERFACE;
    }

    ULONG STDMETHODCALLTYPE AddRef() override
    {
        return InterlockedIncrement(&m_reference_count);
    }

    ULONG STDMETHODCALLTYPE Release() override
    {
        auto reference_count = InterlockedDecrement(&m_reference_count);
        if (reference_count == 0)
            delete this;
        return reference_count;
    }

    HRESULT STDMETHODCALLTYPE GetTextAtPosition(UINT32 position, WCHAR const** text, UINT32* length) override
    {
        if (position >= m_length) {
            *text = nullptr;
            *length = 0;
            return S_OK;
        }
        *text = m_text + position;
        *length = m_length - position;
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE GetTextBeforePosition(UINT32 position, WCHAR const** text, UINT32* length) override
    {
        if (position < 1 || position >= m_length) {
            *text = nullptr;
            *length = 0;
            return S_OK;
        }
        *text = m_text;
        *length = position;
        return S_OK;
    }

    DWRITE_READING_DIRECTION STDMETHODCALLTYPE GetParagraphReadingDirection() override
    {
        return DWRITE_READING_DIRECTION_LEFT_TO_RIGHT;
    }

    HRESULT STDMETHODCALLTYPE GetLocaleName(UINT32 position, UINT32* length, WCHAR const** locale) override
    {
        // All of the text has one locale.
        *length = position < m_length ? m_length - position : 0;
        *locale = m_locale;
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE GetNumberSubstitution(UINT32 position, UINT32* length, IDWriteNumberSubstitution** number_substitution) override
    {
        *length = position < m_length ? m_length - position : 0;
        *number_substitution = m_number_substitution;
        return S_OK;
    }

private:
    ~FallbackSource() = default;

    ULONG m_reference_count { 1 };
    wchar_t const* m_text { nullptr };
    UINT32 m_length { 0 };
    wchar_t const* m_locale { nullptr };
    IDWriteNumberSubstitution* m_number_substitution { nullptr };
};

}

static DirectWrite const* direct_write()
{
    static DirectWrite const* const instance = []() -> DirectWrite const* {
        auto* direct_write = new DirectWrite;
        if (FAILED(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory), reinterpret_cast<IUnknown**>(direct_write->factory.GetAddressOf())))
            || FAILED(direct_write->factory->GetSystemFontCollection(&direct_write->collection, FALSE))) {
            delete direct_write;
            return nullptr;
        }

        ComPtr<IDWriteFactory2> factory2;
        if (SUCCEEDED(direct_write->factory.As(&factory2)))
            (void)factory2->GetSystemFontFallback(&direct_write->fallback);

        wchar_t locale[LOCALE_NAME_MAX_LENGTH] {};
        int locale_length = GetUserDefaultLocaleName(locale, LOCALE_NAME_MAX_LENGTH);
        for (int index = 0; index < locale_length && locale[index] != 0; ++index)
            direct_write->locale.append(locale[index]);
        direct_write->locale.append(0);
        return direct_write;
    }();
    return instance;
}

static Vector<wchar_t> to_wide_string(StringView string)
{
    Vector<wchar_t> wide_string;
    if (!string.is_empty()) {
        auto length = MultiByteToWideChar(CP_UTF8, 0, string.characters_without_null_termination(), static_cast<int>(string.length()), nullptr, 0);
        if (length > 0) {
            wide_string.resize(length);
            MultiByteToWideChar(CP_UTF8, 0, string.characters_without_null_termination(), static_cast<int>(string.length()), wide_string.data(), length);
        }
    }
    wide_string.append(0);
    return wide_string;
}

static Optional<String> from_wide_string(wchar_t const* wide_string, UINT32 length)
{
    if (length == 0)
        return String {};
    auto size = WideCharToMultiByte(CP_UTF8, 0, wide_string, static_cast<int>(length), nullptr, 0, nullptr, nullptr);
    if (size <= 0)
        return {};
    Vector<char> buffer;
    buffer.resize(size);
    WideCharToMultiByte(CP_UTF8, 0, wide_string, static_cast<int>(length), buffer.data(), size, nullptr, nullptr);
    auto string = String::from_utf8(StringView { buffer.data(), buffer.size() });
    if (string.is_error())
        return {};
    return string.release_value();
}

static DirectWriteStyle direct_write_style(u16 weight, u16 width, u8 slope)
{
    // A font style keeps the weight in 0..1000 and the width in 1..9.
    DirectWriteStyle style {
        .weight = static_cast<DWRITE_FONT_WEIGHT>(min<u16>(weight, 1000)),
        .stretch = static_cast<DWRITE_FONT_STRETCH>(clamp<u16>(width, FontWidth::UltraCondensed, FontWidth::UltraExpanded)),
        .style = DWRITE_FONT_STYLE_NORMAL,
    };
    if (slope == 1)
        style.style = DWRITE_FONT_STYLE_ITALIC;
    else if (slope == 2)
        style.style = DWRITE_FONT_STYLE_OBLIQUE;
    return style;
}

// The face of a font that DirectWrite gives, if it is in a local file that Typeface loads. The face of a variable font
// that DirectWrite gives at the coordinates of a named instance is the face at its default coordinates.
static Optional<SystemFontMatch> match_of(IDWriteFont* font)
{
    ComPtr<IDWriteFontFace> face;
    if (FAILED(font->CreateFontFace(&face)))
        return {};

    // A face that is in several files does not load from one of them.
    UINT32 file_count = 0;
    if (FAILED(face->GetFiles(&file_count, nullptr)) || file_count != 1)
        return {};
    ComPtr<IDWriteFontFile> file;
    if (FAILED(face->GetFiles(&file_count, file.GetAddressOf())))
        return {};

    void const* key = nullptr;
    UINT32 key_size = 0;
    if (FAILED(file->GetReferenceKey(&key, &key_size)))
        return {};
    ComPtr<IDWriteFontFileLoader> loader;
    if (FAILED(file->GetLoader(&loader)))
        return {};
    ComPtr<IDWriteLocalFontFileLoader> local_loader;
    if (FAILED(loader.As(&local_loader)))
        return {};

    UINT32 path_length = 0;
    if (FAILED(local_loader->GetFilePathLengthFromKey(key, key_size, &path_length)))
        return {};
    Vector<wchar_t> path;
    path.resize(path_length + 1);
    if (FAILED(local_loader->GetFilePathFromKey(key, key_size, path.data(), path_length + 1)))
        return {};
    auto utf8_path = from_wide_string(path.data(), path_length);
    if (!utf8_path.has_value())
        return {};

    auto loadable = loadable_file(*utf8_path, face->GetIndex());
    if (!loadable.has_value())
        return {};
    return SystemFontMatch { loadable.release_value() };
}

static ComPtr<IDWriteFont> match_font_for_family(StringView family, u16 weight, u16 width, u8 slope)
{
    auto const* direct_write = SystemFontMatcher::direct_write();
    if (!direct_write)
        return {};

    auto family_name = to_wide_string(family);
    UINT32 family_index = 0;
    BOOL exists = FALSE;
    if (FAILED(direct_write->collection->FindFamilyName(family_name.data(), &family_index, &exists)) || !exists)
        return {};
    ComPtr<IDWriteFontFamily> font_family;
    if (FAILED(direct_write->collection->GetFontFamily(family_index, &font_family)))
        return {};

    auto style = direct_write_style(weight, width, slope);
    ComPtr<IDWriteFont> font;
    if (FAILED(font_family->GetFirstMatchingFont(style.weight, style.stretch, style.style, &font)))
        return {};
    return font;
}

Optional<SystemFontMatch> match_family_style(StringView family, u16 weight, u16 width, u8 slope)
{
    auto font = match_font_for_family(family, weight, width, slope);
    if (!font)
        return {};
    return match_of(font.Get());
}

Optional<SystemFontMatch> match_code_point(u32 code_point, u16 weight, u16 width, u8 slope, bool prefer_color_emoji)
{
    auto const* direct_write = SystemFontMatcher::direct_write();
    if (!direct_write || !direct_write->fallback)
        return {};

    if (code_point > 0x10FFFF)
        return {};
    wchar_t text[2] {};
    UINT32 text_length = 1;
    if (code_point < 0x10000) {
        text[0] = static_cast<wchar_t>(code_point);
    } else {
        text[0] = static_cast<wchar_t>(0xD800 + ((code_point - 0x10000) >> 10));
        text[1] = static_cast<wchar_t>(0xDC00 + ((code_point - 0x10000) & 0x3FF));
        text_length = 2;
    }

    // The "und-Zsye" language tag steers the fallback towards a color emoji font. Otherwise, the locale of the user
    // picks among the fonts of a script.
    wchar_t const* locale = prefer_color_emoji ? L"und-Zsye" : direct_write->locale.data();

    ComPtr<IDWriteNumberSubstitution> number_substitution;
    if (FAILED(direct_write->factory->CreateNumberSubstitution(DWRITE_NUMBER_SUBSTITUTION_METHOD_NONE, locale, TRUE, &number_substitution)))
        return {};
    ComPtr<FallbackSource> source;
    source.Attach(new FallbackSource(text, text_length, locale, number_substitution.Get()));

    auto style = direct_write_style(weight, width, slope);
    UINT32 mapped_length = 0;
    ComPtr<IDWriteFont> font;
    FLOAT scale = 1;
    if (FAILED(direct_write->fallback->MapCharacters(source.Get(), 0, text_length, direct_write->collection.Get(), nullptr, style.weight, style.style, style.stretch, &mapped_length, &font, &scale)))
        return {};
    if (!font)
        return {};
    return match_of(font.Get());
}

Optional<FlyString> resolve_generic_family(StringView family, u16 weight, u8 slope)
{
    auto font = match_font_for_family(family, weight, FontWidth::Normal, slope);
    if (!font)
        return {};

    // The family names of the face, or of its family if the face cannot give them.
    ComPtr<IDWriteLocalizedStrings> family_names;
    ComPtr<IDWriteFontFace> face;
    ComPtr<IDWriteFontFace3> face3;
    if (SUCCEEDED(font->CreateFontFace(&face)) && SUCCEEDED(face.As(&face3))) {
        if (FAILED(face3->GetFamilyNames(&family_names)))
            return {};
    } else {
        ComPtr<IDWriteFontFamily> font_family;
        if (FAILED(font->GetFontFamily(&font_family)) || FAILED(font_family->GetFamilyNames(&family_names)))
            return {};
    }

    UINT32 name_length = 0;
    if (FAILED(family_names->GetStringLength(0, &name_length)))
        return {};
    Vector<wchar_t> name;
    name.resize(name_length + 1);
    if (FAILED(family_names->GetString(0, name.data(), name_length + 1)))
        return {};
    auto resolved_family = from_wide_string(name.data(), name_length);
    if (!resolved_family.has_value())
        return {};
    return FlyString { resolved_family.release_value() };
}

}
