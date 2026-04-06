/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/Vector.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/Font/Font.h>
#include <LibGfx/Font/Typeface.h>
#include <LibGfx/ImmutableBitmap.h>

namespace Web::Painting {

struct PendingTypeface {
    u64 typeface_id;
    ReadonlyBytes font_data;
    u32 ttc_index;
};

struct PendingFont {
    u64 font_id;
    u64 typeface_id;
    float point_size;
    Gfx::FontVariationSettings variations;
    Gfx::ShapeFeatures features;
};

struct PendingImage {
    u64 image_id;
    NonnullRefPtr<Gfx::Bitmap const> bitmap;
};

class GPUResourceRegistry {
public:
    u64 ensure_image_id(Gfx::ImmutableBitmap const&);
    u64 ensure_font_id(Gfx::Font const&);

    Vector<PendingTypeface> take_pending_typefaces() { return move(m_pending_typefaces); }
    Vector<PendingFont> take_pending_fonts() { return move(m_pending_fonts); }
    Vector<PendingImage> take_pending_images() { return move(m_pending_images); }

private:
    u64 ensure_typeface_id(Gfx::Typeface const&);

    HashMap<Gfx::ImmutableBitmap const*, u64> m_image_ids;
    HashMap<Gfx::Typeface const*, u64> m_typeface_ids;
    HashMap<Gfx::Font const*, u64> m_font_ids;

    u64 m_next_image_id { 1 };
    u64 m_next_typeface_id { 1 };
    u64 m_next_font_id { 1 };

    Vector<PendingTypeface> m_pending_typefaces;
    Vector<PendingFont> m_pending_fonts;
    Vector<PendingImage> m_pending_images;
};

}
