/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Tokenizing a source text without parsing it, as syntax highlighters and a REPL's completion do.

use crate::lexer::Lexer;
use crate::token::TokenCategory;
use crate::token::TokenType;

/// A token, with the trivia (whitespace and comments) before it. Offsets and lengths count UTF-16 code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceToken {
    pub token_type: TokenType,
    pub category: TokenCategory,
    pub offset: u32,
    pub length: u32,
    pub trivia_offset: u32,
    pub trivia_length: u32,
}

/// The tokens of `source`, ending with its end-of-file token, whose trivia is whatever follows the last token.
pub fn tokenize(source: &[u16]) -> SourceTokens<'_> {
    SourceTokens {
        lexer: Lexer::new(source, 1, 0),
        reached_end_of_file: false,
    }
}

pub struct SourceTokens<'a> {
    lexer: Lexer<'a>,
    reached_end_of_file: bool,
}

impl Iterator for SourceTokens<'_> {
    type Item = SourceToken;

    fn next(&mut self) -> Option<SourceToken> {
        if self.reached_end_of_file {
            return None;
        }
        let token = self.lexer.next();
        self.reached_end_of_file = token.token_type == TokenType::Eof;
        Some(SourceToken {
            token_type: token.token_type,
            category: token.token_type.category(),
            offset: token.value_start,
            length: token.value_len,
            trivia_offset: token.trivia_start,
            trivia_length: token.trivia_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(string: &str) -> Vec<u16> {
        string.encode_utf16().collect()
    }

    #[test]
    fn tokens_cover_the_source_and_end_with_its_trailing_trivia() {
        let source = utf16("let x = 'é'; // done\n");
        let tokens: Vec<SourceToken> = tokenize(&source).collect();

        let types: Vec<TokenType> = tokens.iter().map(|token| token.token_type).collect();
        assert_eq!(
            types,
            [
                TokenType::Let,
                TokenType::Identifier,
                TokenType::Equals,
                TokenType::StringLiteral,
                TokenType::Semicolon,
                TokenType::Eof,
            ]
        );
        assert_eq!(tokens[0].category, TokenCategory::Keyword);
        assert_eq!(tokens[3].category, TokenCategory::String);

        let mut covered_up_to = 0;
        for token in &tokens {
            assert_eq!(token.trivia_offset, covered_up_to);
            assert_eq!(token.offset, token.trivia_offset + token.trivia_length);
            covered_up_to = token.offset + token.length;
        }
        assert_eq!(covered_up_to as usize, source.len());

        let end_of_file = tokens.last().expect("there is an end-of-file token");
        let trailing_trivia = &source[end_of_file.trivia_offset as usize..][..end_of_file.trivia_length as usize];
        assert_eq!(trailing_trivia, utf16(" // done\n"));
    }

    #[test]
    fn empty_source_has_only_the_end_of_file_token() {
        let tokens: Vec<SourceToken> = tokenize(&[]).collect();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token_type, TokenType::Eof);
        assert_eq!(tokens[0].length, 0);
    }
}
