/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Painting/GPUResourceRegistry.h>

namespace Web::Painting {

u64 GPUResourceRegistry::ensure_typeface_id(Gfx::Typeface const& typeface)
{
    auto it = m_typeface_ids.find(&typeface);
    if (it != m_typeface_ids.end())
        return it->value;

    auto id = m_next_typeface_id++;
    m_typeface_ids.set(&typeface, id);
    m_pending_typefaces.append({
        .typeface_id = id,
        .font_data = typeface.font_data(),
        .ttc_index = typeface.font_data_ttc_index(),
    });
    return id;
}

u64 GPUResourceRegistry::ensure_font_id(Gfx::Font const& font)
{
    auto it = m_font_ids.find(&font);
    if (it != m_font_ids.end())
        return it->value;

    auto typeface_id = ensure_typeface_id(font.typeface());
    auto id = m_next_font_id++;
    m_font_ids.set(&font, id);
    m_pending_fonts.append({
        .font_id = id,
        .typeface_id = typeface_id,
        .point_size = font.point_size(),
        .variations = font.font_variation_settings(),
        .features = font.features(),
    });
    return id;
}

u64 GPUResourceRegistry::ensure_image_id(Gfx::ImmutableBitmap const& bitmap)
{
    auto it = m_image_ids.find(&bitmap);
    if (it != m_image_ids.end())
        return it->value;

    auto id = m_next_image_id++;
    m_image_ids.set(&bitmap, id);

    auto cpu_bitmap = bitmap.bitmap();
    if (cpu_bitmap) {
        m_pending_images.append({
            .image_id = id,
            .bitmap = cpu_bitmap.release_nonnull(),
        });
    }
    // FIXME: Handle YUV-backed bitmaps

    return id;
}

}
