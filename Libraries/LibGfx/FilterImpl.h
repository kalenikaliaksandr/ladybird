/*
 * Copyright (c) 2024-2025, Lucien Fiorini <lucienfiorini@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/Types.h>
#include <AK/Vector.h>
#include <LibGfx/DecodedImageFrame.h>
#include <core/SkColorFilter.h>
#include <effects/SkImageFilters.h>

namespace Gfx {

struct FilterImpl {
    sk_sp<SkImageFilter> filter;
    Vector<u8> serialized_bytes;
    HashMap<u64, DecodedImageFrame> image_frames;

    static NonnullOwnPtr<FilterImpl> create(sk_sp<SkImageFilter>, Vector<u8>, HashMap<u64, DecodedImageFrame> = {});

    NonnullOwnPtr<FilterImpl> clone() const;
};

}
