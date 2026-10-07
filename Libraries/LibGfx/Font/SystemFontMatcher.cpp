/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/MappedFile.h>
#include <LibFileSystem/FileSystem.h>
#include <LibGfx/Font/SystemFontMatcher.h>
#include <LibGfx/Font/WOFF/Loader.h>

#ifdef AK_OS_MACOS
#    include <LibGfx/Font/TypefaceCoreText.h>
#endif

namespace Gfx::SystemFontMatcher {

static FontFileFormat format_of(ReadonlyBytes bytes)
{
    // https://www.w3.org/TR/WOFF/#WOFFHeader
    if (bytes.size() >= 4 && bytes[0] == 'w' && bytes[1] == 'O' && bytes[2] == 'F' && bytes[3] == 'F')
        return FontFileFormat::WOFF;
    return FontFileFormat::OpenType;
}

static ErrorOr<NonnullRefPtr<Typeface>> load_mapped_file(NonnullOwnPtr<Core::MappedFile> mapped_file, u32 ttc_index, FontFileFormat format)
{
    if (format == FontFileFormat::WOFF)
        return WOFF::try_load_from_bytes(mapped_file->bytes(), ttc_index);
    return Typeface::try_load_from_mapped_file(move(mapped_file), ttc_index);
}

Optional<SystemFontFile> loadable_file(StringView path, u32 ttc_index)
{
    auto canonical_path = FileSystem::real_path(path);
    if (canonical_path.is_error())
        return {};
    auto mapped_file = Core::MappedFile::map(canonical_path.value());
    if (mapped_file.is_error())
        return {};
    auto format = format_of(mapped_file.value()->bytes());
    if (load_mapped_file(mapped_file.release_value(), ttc_index, format).is_error())
        return {};
    auto string_path = String::from_byte_string(canonical_path.release_value());
    if (string_path.is_error())
        return {};
    return SystemFontFile { .path = string_path.release_value(), .ttc_index = ttc_index, .format = format };
}

ErrorOr<NonnullRefPtr<Typeface>> load(SystemFontMatch const& match)
{
    return match.visit(
        [](SystemFontFile const& file) -> ErrorOr<NonnullRefPtr<Typeface>> {
            auto typeface = TRY(load_mapped_file(TRY(Core::MappedFile::map(file.path)), file.ttc_index, file.format));
            if (file.format == FontFileFormat::OpenType)
                typeface->set_file_path(file.path);
            return typeface;
        },
        [](PlatformFontName const& name) -> ErrorOr<NonnullRefPtr<Typeface>> {
#ifdef AK_OS_MACOS
            return TRY(TypefaceCoreText::try_load_postscript_name(name.postscript_name));
#else
            (void)name;
            return Error::from_string_literal("Only CoreText opens fonts by name");
#endif
        },
        [](NonnullRefPtr<Typeface> const& typeface) -> ErrorOr<NonnullRefPtr<Typeface>> {
            return typeface;
        });
}

}
