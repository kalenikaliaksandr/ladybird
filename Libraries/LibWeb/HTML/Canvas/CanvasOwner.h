/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Variant.h>
#include <LibGC/Ptr.h>
#include <LibWeb/Forward.h>

namespace Web::HTML {

// The canvas a rendering context draws into: either a <canvas> element or an OffscreenCanvas.
using CanvasOwner = Variant<GC::Ref<HTMLCanvasElement>, GC::Ref<OffscreenCanvas>>;

}
