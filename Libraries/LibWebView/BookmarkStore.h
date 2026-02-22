/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Badge.h>
#include <AK/Function.h>
#include <AK/String.h>
#include <AK/Vector.h>
#include <LibURL/URL.h>
#include <LibWebView/Forward.h>

namespace WebView {

struct Bookmark {
    URL::URL url;
    String title;
};

class WEBVIEW_API BookmarkStore {
public:
    static BookmarkStore create(Badge<Application>);
    explicit BookmarkStore(ByteString bookmarks_path);

    Vector<Bookmark> const& bookmarks() const { return m_bookmarks; }

    void add_bookmark(URL::URL, String title);
    bool remove_bookmark(URL::URL const&);
    bool is_bookmarked(URL::URL const&) const;

    Function<void()> on_bookmarks_changed;

private:
    void persist_bookmarks();

    ByteString m_bookmarks_path;
    Vector<Bookmark> m_bookmarks;
};

}
