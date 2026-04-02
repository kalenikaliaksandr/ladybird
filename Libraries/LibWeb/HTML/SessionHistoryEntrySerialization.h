/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/HTML/DocumentState.h>
#include <LibWeb/HTML/SessionHistoryEntry.h>
#include <LibWebView/UISessionHistoryEntry.h>

namespace Web::HTML {

WebView::UIDocumentState serialize_document_state(DocumentState const&);
WebView::UISessionHistoryEntry serialize_session_history_entry(SessionHistoryEntry const&);

void apply_ui_document_state(WebView::UIDocumentState const&, DocumentState&, GC::Heap&);
void apply_ui_session_history_entry(WebView::UISessionHistoryEntry const&, SessionHistoryEntry&, GC::Heap&);

}
