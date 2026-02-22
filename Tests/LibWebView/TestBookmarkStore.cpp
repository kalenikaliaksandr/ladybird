/*
 * Copyright (c) 2026, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <LibCore/File.h>
#include <LibFileSystem/FileSystem.h>
#include <LibTest/TestCase.h>
#include <LibURL/Parser.h>
#include <LibWebView/BookmarkStore.h>

static WebView::BookmarkStore create_test_store(ByteString const& path)
{
    return WebView::BookmarkStore { path };
}

TEST_CASE(add_and_check_bookmark)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-{}.json", getpid());
    auto store = create_test_store(path);

    auto url = URL::Parser::basic_parse("https://example.com"sv).release_value();
    EXPECT(!store.is_bookmarked(url));

    store.add_bookmark(url, "Example"_string);
    EXPECT(store.is_bookmarked(url));
    EXPECT_EQ(store.bookmarks().size(), 1u);
    EXPECT_EQ(store.bookmarks()[0].title, "Example"_string);

    (void)FileSystem::remove(path, FileSystem::RecursionMode::Disallowed);
}

TEST_CASE(duplicate_add_is_noop)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-{}.json", getpid());
    auto store = create_test_store(path);

    auto url = URL::Parser::basic_parse("https://example.com"sv).release_value();
    store.add_bookmark(url, "Example"_string);
    store.add_bookmark(url, "Example Again"_string);
    EXPECT_EQ(store.bookmarks().size(), 1u);
    EXPECT_EQ(store.bookmarks()[0].title, "Example"_string);

    (void)FileSystem::remove(path, FileSystem::RecursionMode::Disallowed);
}

TEST_CASE(remove_bookmark)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-{}.json", getpid());
    auto store = create_test_store(path);

    auto url = URL::Parser::basic_parse("https://example.com"sv).release_value();
    store.add_bookmark(url, "Example"_string);
    EXPECT(store.is_bookmarked(url));

    auto removed = store.remove_bookmark(url);
    EXPECT(removed);
    EXPECT(!store.is_bookmarked(url));
    EXPECT_EQ(store.bookmarks().size(), 0u);

    auto url2 = URL::Parser::basic_parse("https://nonexistent.com"sv).release_value();
    auto removed2 = store.remove_bookmark(url2);
    EXPECT(!removed2);

    (void)FileSystem::remove(path, FileSystem::RecursionMode::Disallowed);
}

TEST_CASE(round_trip_persistence)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-roundtrip-{}.json", getpid());

    {
        auto store = create_test_store(path);
        store.add_bookmark(URL::Parser::basic_parse("https://example.com"sv).release_value(), "Example"_string);
        store.add_bookmark(URL::Parser::basic_parse("https://github.com"sv).release_value(), "GitHub"_string);
        EXPECT_EQ(store.bookmarks().size(), 2u);
    }

    {
        auto store = create_test_store(path);
        EXPECT_EQ(store.bookmarks().size(), 2u);
        EXPECT_EQ(store.bookmarks()[0].title, "Example"_string);
        EXPECT_EQ(store.bookmarks()[1].title, "GitHub"_string);
        EXPECT(store.is_bookmarked(URL::Parser::basic_parse("https://example.com"sv).release_value()));
        EXPECT(store.is_bookmarked(URL::Parser::basic_parse("https://github.com"sv).release_value()));
    }

    (void)FileSystem::remove(path, FileSystem::RecursionMode::Disallowed);
}

TEST_CASE(malformed_json_loads_gracefully)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-malformed-{}.json", getpid());

    {
        auto file = MUST(Core::File::open(path, Core::File::OpenMode::Write));
        MUST(file->write_until_depleted("not valid json"sv.bytes()));
    }

    auto store = create_test_store(path);
    EXPECT_EQ(store.bookmarks().size(), 0u);

    (void)FileSystem::remove(path, FileSystem::RecursionMode::Disallowed);
}

TEST_CASE(nonexistent_file_loads_gracefully)
{
    auto path = ByteString::formatted("/tmp/ladybird-test-bookmarks-nonexistent-{}.json", getpid());
    auto store = create_test_store(path);
    EXPECT_EQ(store.bookmarks().size(), 0u);
}
