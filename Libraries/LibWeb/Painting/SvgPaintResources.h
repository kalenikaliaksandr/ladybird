/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibCompositing/RustFFI.h>
#include <LibWeb/Export.h>
#include <LibWeb/Forward.h>

namespace Web::Painting {

// What the render state asks of the DOM as it syncs the SVG paint resources the rows of a document name: the primitives
// of a filter the row names, and the paint server it fills or strokes with, pushed into the sink it hands over.
bool push_svg_filter_reference_of_row(Layout::BegunRead const&, DOM::Document&, Compositing::RustFFI::NodeSlotId, void const* url_value, void* sink);
void push_svg_paint_server_description_of_row(Layout::BegunRead const&, DOM::Document&, Compositing::RustFFI::NodeSlotId, bool is_stroke, void* sink);

}
