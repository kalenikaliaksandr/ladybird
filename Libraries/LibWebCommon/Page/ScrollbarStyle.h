/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Types.h>

namespace Web {

// Classic scrollbars take room between a scroll container's border and padding. Overlay scrollbars take none and paint
// over the padding box.
enum class ScrollbarStyle : u8 {
    Classic,
    Overlay,
};

}
