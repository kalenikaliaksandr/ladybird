/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Path.h>
#include <LibGfx/SkiaUtils.h>
#include <RustFFI.h>
#include <core/SkString.h>
#include <utils/SkParsePath.h>

// The path query that libgfx_rust does not answer itself yet.
extern "C" {
void ladybird_gfx_skia_path_append_svg_string(void const*, void (*)(void*, u8 const*, size_t), void*);
}

static SkPath skia_path(void const* path)
{
    return Gfx::to_skia_path(Gfx::Path::data_of(static_cast<Gfx::FFI::RustPath const*>(path)));
}

extern "C" void ladybird_gfx_skia_path_append_svg_string(void const* path, void (*append)(void*, u8 const*, size_t), void* context)
{
    auto svg_string = SkParsePath::ToSVGString(skia_path(path));
    append(context, reinterpret_cast<u8 const*>(svg_string.c_str()), svg_string.size());
}
