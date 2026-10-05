/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/String.h>
#include <AK/Vector.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/Palette.h>
#include <LibGfx/SystemTheme.h>
#include <LibJS/SyntaxHighlighter.h>
#include <LibJS/Token.h>
#include <LibSyntax/Highlighter.h>
#include <LibTest/TestCase.h>

// The JavaScript highlighter that LibWeb uses for view-source and script elements. The same expectations hold for the
// C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

static StringView name_of(TokenCategory category)
{
    switch (category) {
    case TokenCategory::Invalid:
        return "Invalid"sv;
    case TokenCategory::Trivia:
        return "Trivia"sv;
    case TokenCategory::Number:
        return "Number"sv;
    case TokenCategory::String:
        return "String"sv;
    case TokenCategory::Punctuation:
        return "Punctuation"sv;
    case TokenCategory::Operator:
        return "Operator"sv;
    case TokenCategory::Keyword:
        return "Keyword"sv;
    case TokenCategory::ControlKeyword:
        return "ControlKeyword"sv;
    case TokenCategory::Identifier:
        return "Identifier"sv;
    }
    VERIFY_NOT_REACHED();
}

// Each span as "<line>:<column>-<line>:<column> <category>", with " (skippable)" for trivia.
static Vector<String> highlighted_spans(StringView source)
{
    auto buffer = MUST(Core::AnonymousBuffer::create_with_size(sizeof(Gfx::SystemTheme)));
    Gfx::Palette palette { Gfx::PaletteImpl::create_with_anonymous_buffer(buffer) };

    Syntax::ProxyHighlighterClient client { { 0, 0 }, 0, source };
    SyntaxHighlighter highlighter;
    EXPECT_EQ(highlighter.language(), Syntax::Language::JavaScript);
    highlighter.attach(client);
    highlighter.rehighlight(palette);
    highlighter.detach();

    Vector<String> spans;
    for (auto const& span : client.corrected_spans()) {
        auto category = token_category_from_packed(span.data);
        EXPECT_EQ(span.attributes.bold, category == TokenCategory::Keyword || category == TokenCategory::ControlKeyword);
        spans.append(MUST(String::formatted("{}:{}-{}:{} {}{}", span.range.start().line(), span.range.start().column(), span.range.end().line(), span.range.end().column(), name_of(category), span.is_skippable ? " (skippable)"sv : ""sv)));
    }
    return spans;
}

TEST_CASE(tokens_become_spans_of_their_category)
{
    Vector<String> expected_spans {
        "0:0-0:3 Keyword"_string,
        "0:3-0:4 Trivia (skippable)"_string,
        "0:4-0:5 Identifier"_string,
        "0:5-0:6 Trivia (skippable)"_string,
        "0:6-0:7 Operator"_string,
        "0:7-0:8 Trivia (skippable)"_string,
        "0:8-0:9 Number"_string,
        "0:9-0:10 Punctuation"_string,
        "0:10-1:0 Trivia (skippable)"_string,
        "1:0-1:2 ControlKeyword"_string,
    };
    auto spans = highlighted_spans("let x = 1; // note\nif"sv);
    EXPECT_EQ(spans, expected_spans);
}

TEST_CASE(columns_count_code_points)
{
    Vector<String> expected_spans {
        "0:0-0:1 Identifier"_string,
        "0:1-0:2 Operator"_string,
        "0:2-0:5 String"_string,
        "0:5-1:0 Trivia (skippable)"_string,
        "1:0-1:1 Invalid"_string,
    };
    auto spans = highlighted_spans("f+\"\xF0\x9F\x98\x80\"\n#"sv);
    EXPECT_EQ(spans, expected_spans);
}

TEST_CASE(invalid_utf8_and_a_byte_order_mark)
{
    Vector<String> expected_spans {
        "0:0-0:1 Identifier"_string,
        "0:1-0:2 Trivia (skippable)"_string,
        "0:2-0:5 String"_string,
    };
    auto spans = highlighted_spans("\xEF\xBB\xBFx '\xFF'"sv);
    EXPECT_EQ(spans, expected_spans);
}
