/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/JsonArray.h>
#include <AK/JsonObject.h>
#include <AK/JsonValue.h>
#include <AK/LexicalPath.h>
#include <LibCore/Directory.h>
#include <LibCore/File.h>
#include <LibCore/StandardPaths.h>
#include <LibURL/Parser.h>
#include <LibWebView/BookmarkStore.h>

namespace WebView {

static constexpr auto url_key = "url"sv;
static constexpr auto title_key = "title"sv;

static ErrorOr<JsonArray> read_bookmarks_file(StringView bookmarks_path)
{
    auto file = Core::File::open(bookmarks_path, Core::File::OpenMode::Read);
    if (file.is_error()) {
        if (file.error().is_errno() && file.error().code() == ENOENT)
            return JsonArray {};
        return file.release_error();
    }

    auto contents = TRY(file.value()->read_until_eof());
    auto json = TRY(JsonValue::from_string(contents));

    if (!json.is_array())
        return JsonArray {};
    return move(json.as_array());
}

static ErrorOr<void> write_bookmarks_file(StringView bookmarks_path, JsonArray const& contents)
{
    auto directory = LexicalPath { bookmarks_path }.parent();
    TRY(Core::Directory::create(directory, Core::Directory::CreateDirectories::Yes));

    auto file = TRY(Core::File::open(bookmarks_path, Core::File::OpenMode::Write));
    TRY(file->write_until_depleted(contents.serialized()));

    return {};
}

BookmarkStore BookmarkStore::create(Badge<Application>)
{
    auto config_directory = ByteString::formatted("{}/Ladybird", Core::StandardPaths::config_directory());
    auto bookmarks_path = ByteString::formatted("{}/Bookmarks.json", config_directory);

    return BookmarkStore { move(bookmarks_path) };
}

BookmarkStore::BookmarkStore(ByteString bookmarks_path)
    : m_bookmarks_path(move(bookmarks_path))
{
    auto bookmarks_json = read_bookmarks_file(m_bookmarks_path);
    if (bookmarks_json.is_error()) {
        warnln("Unable to read Ladybird bookmarks: {}", bookmarks_json.error());
        return;
    }

    bookmarks_json.value().for_each([&](JsonValue const& entry) {
        if (!entry.is_object())
            return;

        auto url_string = entry.as_object().get_string(url_key);
        auto title = entry.as_object().get_string(title_key);
        if (!url_string.has_value())
            return;

        auto url = URL::Parser::basic_parse(*url_string);
        if (!url.has_value())
            return;

        m_bookmarks.append(Bookmark {
            .url = url.release_value(),
            .title = title.value_or(String {}),
        });
    });
}

void BookmarkStore::add_bookmark(URL::URL url, String title)
{
    if (is_bookmarked(url))
        return;

    m_bookmarks.append(Bookmark {
        .url = move(url),
        .title = move(title),
    });

    persist_bookmarks();

    if (on_bookmarks_changed)
        on_bookmarks_changed();
}

bool BookmarkStore::remove_bookmark(URL::URL const& url)
{
    auto serialized = url.serialize();

    bool removed = m_bookmarks.remove_all_matching([&](auto const& bookmark) {
        return bookmark.url.serialize() == serialized;
    });

    if (removed) {
        persist_bookmarks();

        if (on_bookmarks_changed)
            on_bookmarks_changed();
    }

    return removed;
}

bool BookmarkStore::is_bookmarked(URL::URL const& url) const
{
    auto serialized = url.serialize();

    for (auto const& bookmark : m_bookmarks) {
        if (bookmark.url.serialize() == serialized)
            return true;
    }

    return false;
}

void BookmarkStore::persist_bookmarks()
{
    JsonArray bookmarks;
    bookmarks.ensure_capacity(m_bookmarks.size());

    for (auto const& bookmark : m_bookmarks) {
        JsonObject entry;
        entry.set(url_key, bookmark.url.serialize());
        entry.set(title_key, bookmark.title);
        bookmarks.must_append(move(entry));
    }

    if (auto result = write_bookmarks_file(m_bookmarks_path, bookmarks); result.is_error())
        warnln("Unable to persist Ladybird bookmarks: {}", result.error());
}

}
