/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/ImmutableBitmap.h>
#include <LibWeb/Painting/ExternalContentSource.h>

namespace Web::Painting {

NonnullRefPtr<ExternalContentSource> ExternalContentSource::create()
{
    return adopt_ref(*new ExternalContentSource());
}

void ExternalContentSource::update(RefPtr<Gfx::ImmutableBitmap> bitmap)
{
    RefPtr<Gfx::ImmutableBitmap> old;
    {
        Threading::MutexLocker const locker { m_mutex };
        old = move(m_bitmap);
        m_bitmap = move(bitmap);
    }
    // old destroyed here, outside the lock — its destructor may take
    // a Skia context lock, so we must not hold m_mutex during destruction.
}

void ExternalContentSource::clear()
{
    RefPtr<Gfx::ImmutableBitmap> old;
    {
        Threading::MutexLocker const locker { m_mutex };
        old = move(m_bitmap);
    }
    // old destroyed here, outside the lock.
}

RefPtr<Gfx::ImmutableBitmap> ExternalContentSource::current_bitmap() const
{
    Threading::MutexLocker const locker { m_mutex };
    return m_bitmap;
}

}
